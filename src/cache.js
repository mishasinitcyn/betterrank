import { createHash } from 'crypto';
import { readFile, unlink } from 'fs/promises';
import { homedir, platform } from 'os';
import { basename, join, resolve } from 'path';
import { parseFile, SUPPORTED_EXTENSIONS } from './parser.js';
import {
  walkFiles,
  statFilesBatched,
  isMinifiedSource,
  discoverRootMetadata,
  acquireCacheLock,
} from './walk.js';
import {
  buildGraph,
  updateGraphFiles,
  rankedSymbols,
  saveGraph,
  loadGraph,
} from './graph.js';

function getPlatformCacheDir() {
  if (process.env.CODE_INDEX_CACHE_DIR) return process.env.CODE_INDEX_CACHE_DIR;

  const home = homedir();
  if (platform() === 'darwin') return join(home, 'Library', 'Caches', 'code-index');
  if (platform() === 'win32') return join(process.env.LOCALAPPDATA || join(home, 'AppData', 'Local'), 'code-index', 'Cache');
  return join(process.env.XDG_CACHE_HOME || join(home, '.cache'), 'code-index');
}

const CACHE_DIR = getPlatformCacheDir();

const IGNORE_PATTERNS = [
  // ── JS / Node ──────────────────────────────────────────
  '**/node_modules/**',
  '**/.npm/**',
  '**/.yarn/**',
  '**/.pnp.*',
  '**/bower_components/**',
  '**/*.min.js',
  '**/*.min.mjs',
  '**/*.min.cjs',
  '**/*.bundle.js',
  '**/*.bundle.mjs',
  '**/*.chunk.js',
  '**/*.map',

  // ── Python ─────────────────────────────────────────────
  '**/__pycache__/**',
  '**/.venv/**',
  '**/venv/**',
  '**/env/**',
  '**/.env/**',
  '**/.virtualenvs/**',
  '**/site-packages/**',
  '**/*.egg-info/**',
  '**/.eggs/**',
  '**/.tox/**',
  '**/.mypy_cache/**',
  '**/.pytest_cache/**',
  '**/.ruff_cache/**',

  // ── Rust ───────────────────────────────────────────────
  '**/target/debug/**',
  '**/target/release/**',

  // ── Java / JVM ─────────────────────────────────────────
  '**/.gradle/**',
  '**/.m2/**',

  // ── Ruby ───────────────────────────────────────────────
  '**/.bundle/**',

  // ── C / C++ ────────────────────────────────────────────
  '**/cmake-build-*/**',
  '**/CMakeFiles/**',

  // ── Go / PHP (root-level vendor only — never hide src/**/vendor/) ──
  'vendor/**',

  // ── Build output & generated ───────────────────────────
  '**/dist/**',
  '**/build/**',
  '**/out/**',
  '**/coverage/**',

  // ── Frameworks ─────────────────────────────────────────
  '**/.next/**',
  '**/.nuxt/**',
  '**/.output/**',
  '**/.svelte-kit/**',
  '**/.angular/**',
  '**/.turbo/**',
  '**/.parcel-cache/**',
  '**/.cache/**',

  // ── iOS / mobile ───────────────────────────────────────
  '**/Pods/**',
  '**/*.xcframework/**',

  // ── Infrastructure / deploy ────────────────────────────
  '**/.terraform/**',
  '**/.serverless/**',
  '**/cdk.out/**',
  '**/.vercel/**',
  '**/.netlify/**',

  // ── VCS & tool caches ──────────────────────────────────
  '**/.git/**',
  '**/.code-index/**',
  '**/.claude/**',
  '**/.cursor/**',
  '**/.nx/**',

  // ── UI component libraries ─────────────────────────────
  '**/components/ui/**',

  // ── Scratch / temp ─────────────────────────────────────
  'tmp/**',
];

const CONFIG_PATH = '.code-index/config.json';

/**
 * Normalize single-root or multi-root input into `{ path, prefix }[]`.
 * Single root uses `prefix = ''`; multiple roots prefix paths by unique directory basename.
 */
function normalizeRoots(projectRootOrRoots) {
  const list = Array.isArray(projectRootOrRoots) ? projectRootOrRoots : [projectRootOrRoots];
  const resolved = list.map(r => resolve(r));

  if (resolved.length === 1) {
    return [{ path: resolved[0], prefix: '' }];
  }

  // Build unique prefixes for multi-root mode (disambiguate duplicate basenames)
  const baseCounts = new Map();
  for (const r of resolved) {
    const b = basename(r);
    baseCounts.set(b, (baseCounts.get(b) || 0) + 1);
  }
  const seen = new Map();
  return resolved.map(r => {
    const b = basename(r);
    if (baseCounts.get(b) === 1) return { path: r, prefix: b };
    const idx = (seen.get(b) || 0) + 1;
    seen.set(b, idx);
    return { path: r, prefix: `${b}-${idx}` };
  });
}

function cachePathForRoot(projectRootOrRoots) {
  const list = Array.isArray(projectRootOrRoots)
    ? [...projectRootOrRoots].map(r => resolve(r)).sort()
    : [resolve(projectRootOrRoots)];
  const hash = createHash('sha256').update(list.join('\0')).digest('hex').slice(0, 12);
  return join(CACHE_DIR, `${hash}.br.gz`);
}

class CodeIndexCache {
  constructor(projectRootOrRoots, opts = {}) {
    this.roots = normalizeRoots(projectRootOrRoots);
    this.projectRoot = this.roots[0].path;
    this.isMultiRoot = this.roots.length > 1;
    this.cachePath = opts.cachePath
      ? join(this.projectRoot, opts.cachePath)
      : cachePathForRoot(this.roots.map(r => r.path));
    this.configPath = join(this.projectRoot, CONFIG_PATH);
    this.graph = null;
    this.mtimes = new Map();
    this.cachedRanked = null;
    this.initialized = false;
    this.skipCacheLoadOnce = false;
    this.extensions = opts.extensions || SUPPORTED_EXTENSIONS;
    this.ignorePatterns = [...IGNORE_PATTERNS, ...(opts.ignore || [])];
  }

  /**
   * Resolve a graph-relative file path back to its absolute path on disk
   * (works in both single-root and multi-root modes).
   */
  resolveAbsPath(graphRelPath) {
    if (!this.isMultiRoot) {
      return join(this.projectRoot, graphRelPath);
    }
    for (const r of this.roots) {
      const pfx = `${r.prefix}/`;
      if (graphRelPath.startsWith(pfx)) {
        return join(r.path, graphRelPath.slice(pfx.length));
      }
    }
    return join(this.projectRoot, graphRelPath);
  }

  async ensure() {
    if (!this.initialized) {
      await this._loadConfig();

      if (!this.skipCacheLoadOnce) {
        const cached = await loadGraph(this.cachePath);
        if (cached) {
          this.graph = cached.graph;
          this.mtimes = cached.mtimes;
          this.cachedRanked = cached.cachedRanked || null;
        }
      }
      this.skipCacheLoadOnce = false;
      this.initialized = true;
    }

    let { changed, deleted, totalScanned } = await this._getChangedFiles();

    if (changed.length === 0 && deleted.length === 0) {
      if (!this.graph) {
        const graphology = await import('graphology');
        const MDG = graphology.default?.MultiDirectedGraph || graphology.MultiDirectedGraph;
        this.graph = new MDG({ allowSelfLoops: false });
      }
      return { changed: 0, deleted: 0, totalScanned };
    }

    // Acquire per-cache lock so concurrent processes targeting the same uncached
    // root wait and reuse the cache instead of running duplicate cold builds.
    const lock = await acquireCacheLock(this.cachePath);
    try {
      if (lock.waited) {
        const fresh = await loadGraph(this.cachePath);
        if (fresh) {
          this.graph = fresh.graph;
          this.mtimes = fresh.mtimes;
          this.cachedRanked = fresh.cachedRanked || null;
          ({ changed, deleted, totalScanned } = await this._getChangedFiles());
          if (changed.length === 0 && deleted.length === 0) {
            return { changed: 0, deleted: 0, totalScanned };
          }
        }
      }

      const isColdStart = !this.graph;
      const rootLabel = this.isMultiRoot
        ? this.roots.map(r => r.prefix).join(' + ')
        : this.projectRoot;
      if (isColdStart) {
        process.stderr.write(`Indexing ${rootLabel}... ${changed.length} files found, parsing...\n`);
      }

      const t0 = Date.now();
      const [resolverCtx, parseOut] = await Promise.all([
        this._discoverResolverContext(),
        this._parseFiles(changed),
      ]);
      const { parsed: newSymbols, skippedCount } = parseOut;

      if (!this.graph) {
        this.graph = buildGraph(newSymbols, resolverCtx);
      } else {
        const allRemoved = [...deleted, ...changed];
        updateGraphFiles(this.graph, allRemoved, newSymbols, resolverCtx);
      }

      this.cachedRanked = rankedSymbols(this.graph);

      try {
        await saveGraph(this.graph, this.mtimes, this.cachePath, {
          precomputedRanked: this.cachedRanked,
        });
        if (this.cachePath.endsWith('.br.gz')) {
          const legacyPath = this.cachePath.slice(0, -'.br.gz'.length) + '.json';
          await unlink(legacyPath).catch(() => {});
        }
      } catch {
        // Cache persistence is best-effort.
      }

      if (isColdStart) {
        const elapsed = ((Date.now() - t0) / 1000).toFixed(1);
        let symbols = 0;
        this.graph.forEachNode((_n, attrs) => { if (attrs.type === 'symbol') symbols++; });
        const skippedNote = skippedCount > 0 ? `, skipped ${skippedCount} minified/generated` : '';
        process.stderr.write(`Indexed ${newSymbols.length} files (${symbols} symbols, ${this.graph.size} edges${skippedNote}) in ${elapsed}s\n`);
      }

      return { changed: changed.length, deleted: deleted.length, totalScanned };
    } finally {
      await lock.release();
    }
  }

  async _discoverResolverContext() {
    const metas = await Promise.all(
      this.roots.map(r => discoverRootMetadata(r.path, r.prefix)),
    );
    const workspacePackages = [];
    const pythonRoots = [];
    for (const m of metas) {
      workspacePackages.push(...m.workspacePackages);
      for (const pr of m.pythonRoots) {
        if (!pythonRoots.includes(pr)) pythonRoots.push(pr);
      }
    }
    return { workspacePackages, pythonRoots };
  }

  async _loadConfig() {
    for (const r of this.roots) {
      try {
        const raw = JSON.parse(await readFile(join(r.path, CONFIG_PATH), 'utf-8'));
        if (Array.isArray(raw.ignore)) {
          this.ignorePatterns = [...this.ignorePatterns, ...raw.ignore];
        }
      } catch {
        // No config file or invalid JSON
      }
    }
  }

  async reindex() {
    this.graph = null;
    this.mtimes = new Map();
    this.cachedRanked = null;
    this.initialized = false;
    this.skipCacheLoadOnce = true;

    try {
      await unlink(this.cachePath);
    } catch {
      // doesn't exist
    }

    return this.ensure();
  }

  async _getChangedFiles() {
    const changed = [];
    const currentFiles = new Set();
    let totalScanned = 0;

    const perRoot = await Promise.all(
      this.roots.map(async r => {
        const relPaths = await walkFiles(r.path, {
          extensions: this.extensions,
          ignorePatterns: this.ignorePatterns,
        });
        const stats = await statFilesBatched(r.path, relPaths);
        return { prefix: r.prefix, stats, count: relPaths.length };
      }),
    );

    for (const { prefix, stats, count } of perRoot) {
      totalScanned += count;
      for (const entry of stats) {
        const graphPath = prefix ? `${prefix}/${entry.relPath}` : entry.relPath;
        currentFiles.add(graphPath);

        const storedMtime = this.mtimes.get(graphPath);
        if (storedMtime === undefined || storedMtime < entry.mtimeMs) {
          changed.push(graphPath);
          this.mtimes.set(graphPath, entry.mtimeMs);
        }
      }
    }

    const deleted = [];
    for (const [f] of this.mtimes) {
      if (!currentFiles.has(f)) {
        deleted.push(f);
        this.mtimes.delete(f);
      }
    }

    return { changed, deleted, totalScanned };
  }

  async _parseFiles(graphPaths) {
    const parsed = [];
    let skippedCount = 0;

    const BATCH_SIZE = 32;
    for (let i = 0; i < graphPaths.length; i += BATCH_SIZE) {
      const batch = graphPaths.slice(i, i + BATCH_SIZE);
      const sources = await Promise.all(
        batch.map(async graphPath => {
          try {
            const source = await readFile(this.resolveAbsPath(graphPath), 'utf-8');
            return { graphPath, source };
          } catch {
            return null;
          }
        }),
      );

      for (const item of sources) {
        if (!item) continue;
        const { graphPath, source } = item;
        if (isMinifiedSource(source, graphPath)) {
          skippedCount++;
          continue;
        }
        try {
          const result = parseFile(graphPath, source);
          if (result) parsed.push(result);
        } catch {
          // skip unparseable files
        }
      }
    }

    return { parsed, skippedCount };
  }

  getGraph() {
    return this.graph;
  }

  getMtimes() {
    return this.mtimes;
  }

  getCachedRanked() {
    return this.cachedRanked;
  }
}

export { CodeIndexCache, CACHE_DIR, cachePathForRoot };
