import { createHash } from 'crypto';
import { stat, readFile } from 'fs/promises';
import { glob } from 'glob';
import { homedir, platform } from 'os';
import { join, relative } from 'path';
import { parseFile, SUPPORTED_EXTENSIONS } from './parser.js';
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
  '**/*.bundle.js',
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
  // To re-include, add "!**/components/ui/**" in .code-index/config.json ignore list,
  // or pass --ignore '!**/components/ui/**' on the CLI.
  '**/components/ui/**',

  // ── Scratch / temp ─────────────────────────────────────
  'tmp/**',
];

const CONFIG_PATH = '.code-index/config.json';

/**
 * Derive a deterministic cache filename from the project root path.
 * Uses a short hash so cache files are grouped under one central directory.
 */
function cachePathForRoot(projectRoot) {
  const hash = createHash('sha256').update(projectRoot).digest('hex').slice(0, 12);
  return join(CACHE_DIR, `${hash}.json`);
}

class CodeIndexCache {
  constructor(projectRoot, opts = {}) {
    this.projectRoot = projectRoot;
    this.cachePath = opts.cachePath ? join(projectRoot, opts.cachePath) : cachePathForRoot(projectRoot);
    this.configPath = join(projectRoot, CONFIG_PATH);
    this.graph = null;
    this.mtimes = new Map();
    this.initialized = false;
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

      const cached = await loadGraph(this.cachePath);
      if (cached) {
        this.graph = cached.graph;
        this.mtimes = cached.mtimes;
      }
      this.initialized = true;
    }

    const { changed, deleted, totalScanned } = await this._getChangedFiles();

    if (changed.length === 0 && deleted.length === 0) {
      if (!this.graph) {
        // First run, no cache, no files — empty graph
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
    const newSymbols = await this._parseFiles(changed);

    if (!this.graph) {
      // Full build from scratch
      this.graph = buildGraph(newSymbols);
    } else {
      // Incremental update
      const allRemoved = [...deleted, ...changed];
      updateGraphFiles(this.graph, allRemoved, newSymbols);
    }

    await saveGraph(this.graph, this.mtimes, this.cachePath);

    if (isColdStart) {
      const elapsed = ((Date.now() - t0) / 1000).toFixed(1);
      let symbols = 0;
      this.graph.forEachNode((_n, attrs) => { if (attrs.type === 'symbol') symbols++; });
      process.stderr.write(`Indexed ${changed.length} files (${symbols} symbols, ${this.graph.size} edges) in ${elapsed}s\n`);
    }

    return { changed: changed.length, deleted: deleted.length, totalScanned };
  }

  /**
   * Load project-level config from .code-index/config.json.
   * Merges extra ignore patterns with built-in defaults.
   *
   * Config format:
   * {
   *   "ignore": ["extra/pattern/**", "another/**"]
   * }
   */
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

  /**
   * Force a full reindex from scratch.
   */
  async reindex() {
    this.graph = null;
    this.mtimes = new Map();
    this.initialized = false;

    // Delete the cache file
    try {
      const { unlink } = await import('fs/promises');
      await unlink(this.cachePath);
    } catch {
      // doesn't exist, fine
    }

    return this.ensure();
  }

  /**
   * Walk the project tree and find files that have changed since last parse.
   */
  async _getChangedFiles() {
    const pattern = `**/*{${this.extensions.join(',')}}`;
    const files = await glob(pattern, {
      cwd: this.projectRoot,
      ignore: this.ignorePatterns,
      absolute: true,
      nodir: true,
    });

    const changed = [];
    const currentFiles = new Set();

    for (const absPath of files) {
      const relPath = relative(this.projectRoot, absPath);
      currentFiles.add(relPath);

      try {
        const { mtimeMs } = await stat(absPath);
        const storedMtime = this.mtimes.get(relPath);
        if (storedMtime === undefined || storedMtime < mtimeMs) {
          changed.push(relPath);
          this.mtimes.set(relPath, mtimeMs);
        }
      } catch {
        // file disappeared between glob and stat
      }
    }

    const deleted = [];
    for (const [f] of this.mtimes) {
      if (!currentFiles.has(f)) {
        deleted.push(f);
        this.mtimes.delete(f);
      }
    }

    return { changed, deleted, totalScanned: files.length };
  }

  /**
   * Parse a batch of files and return their symbol data.
   */
  async _parseFiles(relPaths) {
    const results = [];

    for (const relPath of relPaths) {
      const absPath = join(this.projectRoot, relPath);
      try {
        const source = await readFile(absPath, 'utf-8');
        const result = await parseFile(relPath, source);
        if (result) results.push(result);
      } catch {
        // skip unparseable files
      }
    }

    return results;
  }

  getGraph() {
    return this.graph;
  }

  getMtimes() {
    return this.mtimes;
  }
}

export { CodeIndexCache, CACHE_DIR, cachePathForRoot };
