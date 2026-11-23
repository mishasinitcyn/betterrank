import { createHash } from 'crypto';
import { readFile, unlink } from 'fs/promises';
import { homedir, platform } from 'os';
import { join } from 'path';
import { parseFile, SUPPORTED_EXTENSIONS } from './parser.js';
import { walkFiles, statFilesBatched, isMinifiedSource } from './walk.js';
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

  // ── Go ─────────────────────────────────────────────────
  '**/vendor/**',

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
  // shadcn, etc. — high fan-in but rarely investigation targets.
  // To re-include, add "!**/components/ui/**" in .code-index/config.json ignore list.
  '**/components/ui/**',

  // ── Scratch / temp ─────────────────────────────────────
  'tmp/**',
];

const CONFIG_PATH = '.code-index/config.json';

/**
 * Derive a deterministic cache filename from the project root path.
 * Uses compact gzipped v3 format (.br.gz).
 */
function cachePathForRoot(projectRoot) {
  const hash = createHash('sha256').update(projectRoot).digest('hex').slice(0, 12);
  return join(CACHE_DIR, `${hash}.br.gz`);
}

class CodeIndexCache {
  constructor(projectRoot, opts = {}) {
    this.projectRoot = projectRoot;
    this.cachePath = opts.cachePath ? join(projectRoot, opts.cachePath) : cachePathForRoot(projectRoot);
    this.configPath = join(projectRoot, CONFIG_PATH);
    this.graph = null;
    this.mtimes = new Map();
    this.cachedRanked = null;
    this.initialized = false;
    this.skipCacheLoadOnce = false;
    this.extensions = opts.extensions || SUPPORTED_EXTENSIONS;
    this.ignorePatterns = [...IGNORE_PATTERNS, ...(opts.ignore || [])];
  }

  /**
   * Ensure the index is loaded and up-to-date.
   * Lazy-initializes on first call; incremental updates on subsequent calls.
   */
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

    const { changed, deleted, totalScanned } = await this._getChangedFiles();

    if (changed.length === 0 && deleted.length === 0) {
      if (!this.graph) {
        const graphology = await import('graphology');
        const MDG = graphology.default?.MultiDirectedGraph || graphology.MultiDirectedGraph;
        this.graph = new MDG({ allowSelfLoops: false });
      }
      return { changed: 0, deleted: 0, totalScanned };
    }

    const isColdStart = !this.graph;
    if (isColdStart) {
      process.stderr.write(`Indexing ${this.projectRoot}... ${changed.length} files found, parsing...\n`);
    }

    const t0 = Date.now();
    const { parsed: newSymbols, skippedCount } = await this._parseFiles(changed);

    if (!this.graph) {
      this.graph = buildGraph(newSymbols);
    } else {
      const allRemoved = [...deleted, ...changed];
      updateGraphFiles(this.graph, allRemoved, newSymbols);
    }

    // Recompute unfocused PageRank once and persist with the cache
    this.cachedRanked = rankedSymbols(this.graph);

    try {
      await saveGraph(this.graph, this.mtimes, this.cachePath, {
        precomputedRanked: this.cachedRanked,
      });
      // Clean up legacy v1/v2 .json cache if present for this project root
      if (this.cachePath.endsWith('.br.gz')) {
        const legacyPath = this.cachePath.slice(0, -'.br.gz'.length) + '.json';
        await unlink(legacyPath).catch(() => {});
      }
    } catch {
      // Cache persistence is best-effort. Keep the in-memory graph usable.
    }

    if (isColdStart) {
      const elapsed = ((Date.now() - t0) / 1000).toFixed(1);
      let symbols = 0;
      this.graph.forEachNode((_n, attrs) => { if (attrs.type === 'symbol') symbols++; });
      const skippedNote = skippedCount > 0 ? `, skipped ${skippedCount} minified/generated` : '';
      process.stderr.write(`Indexed ${newSymbols.length} files (${symbols} symbols, ${this.graph.size} edges${skippedNote}) in ${elapsed}s\n`);
    }

    return { changed: changed.length, deleted: deleted.length, totalScanned };
  }

  async _loadConfig() {
    try {
      const raw = JSON.parse(await readFile(this.configPath, 'utf-8'));
      if (Array.isArray(raw.ignore)) {
        this.ignorePatterns = [...this.ignorePatterns, ...raw.ignore];
      }
    } catch {
      // No config file or invalid JSON — use defaults only
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
      // doesn't exist, fine
    }

    return this.ensure();
  }

  /**
   * Walk the project tree in parallel and find files that have changed since last parse.
   */
  async _getChangedFiles() {
    const relPaths = await walkFiles(this.projectRoot, {
      extensions: this.extensions,
      ignorePatterns: this.ignorePatterns,
    });

    const stats = await statFilesBatched(this.projectRoot, relPaths);
    const changed = [];
    const currentFiles = new Set();

    for (const entry of stats) {
      const { relPath, mtimeMs } = entry;
      currentFiles.add(relPath);

      const storedMtime = this.mtimes.get(relPath);
      if (storedMtime === undefined || storedMtime < mtimeMs) {
        changed.push(relPath);
        this.mtimes.set(relPath, mtimeMs);
      }
    }

    const deleted = [];
    for (const [f] of this.mtimes) {
      if (!currentFiles.has(f)) {
        deleted.push(f);
        this.mtimes.delete(f);
      }
    }

    return { changed, deleted, totalScanned: relPaths.length };
  }

  /**
   * Read and parse a batch of files, skipping minified/generated files.
   */
  async _parseFiles(relPaths) {
    const parsed = [];
    let skippedCount = 0;

    // Read file contents in bounded parallel batches, then parse synchronously
    const BATCH_SIZE = 32;
    for (let i = 0; i < relPaths.length; i += BATCH_SIZE) {
      const batch = relPaths.slice(i, i + BATCH_SIZE);
      const sources = await Promise.all(
        batch.map(async relPath => {
          try {
            const source = await readFile(join(this.projectRoot, relPath), 'utf-8');
            return { relPath, source };
          } catch {
            return null;
          }
        }),
      );

      for (const item of sources) {
        if (!item) continue;
        const { relPath, source } = item;
        if (isMinifiedSource(source, relPath)) {
          skippedCount++;
          continue;
        }
        try {
          const result = parseFile(relPath, source);
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
