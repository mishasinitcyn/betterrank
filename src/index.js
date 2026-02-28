import { readFile } from 'fs/promises';
import { join, dirname, relative, sep, basename } from 'path';
import { CodeIndexCache } from './cache.js';
import { rankedSymbols } from './graph.js';
import { parseFile } from './parser.js';

// ── Orphan false-positive filters ──────────────────────────────────────────
//
// Orphan detection finds files/symbols with no cross-file connections.
// Many of these are false positives: entry points, config, tests, framework
// hooks, etc. that are invoked by runtimes, not by other source files.
// These filters aggressively exclude them (at the cost of some true positives).

// File basenames (without extension) that are runtime entry points, config,
// or package markers — they have no incoming IMPORTS because the runtime
// loads them directly, not because they're dead.
const ORPHAN_EXCLUDED_BASENAMES = new Set([
  'index', 'main', 'app', 'server', 'cli', 'mod', 'lib',
  'manage', 'wsgi', 'asgi', 'handler', 'lambda',
  '__init__', '__main__',
  'config', 'settings', 'conf', 'conftest', 'setup',
  'gulpfile', 'gruntfile', 'makefile', 'rakefile', 'taskfile',
]);

// Path segments indicating test/spec directories
const TEST_PATH_SEGMENTS = [
  '/test/', '/tests/', '/__tests__/', '/spec/', '/specs/',
  '/testing/', '/fixtures/', '/mocks/', '/e2e/', '/cypress/',
];

function isTestFile(filePath) {
  const lower = '/' + filePath.toLowerCase();
  for (const seg of TEST_PATH_SEGMENTS) {
    if (lower.includes(seg)) return true;
  }
  const stem = basename(filePath).replace(/\.[^.]+$/, '').toLowerCase();
  return (
    stem.startsWith('test_') || stem.startsWith('test.') ||
    stem.endsWith('.test') || stem.endsWith('.spec') ||
    stem.endsWith('_test') || stem.endsWith('_spec')
  );
}

function isOrphanFalsePositiveFile(filePath) {
  const base = basename(filePath);
  const stem = base.replace(/\.[^.]+$/, '').toLowerCase();

  if (ORPHAN_EXCLUDED_BASENAMES.has(stem)) return true;

  // Dotfiles are always config (.eslintrc, .prettierrc, etc.)
  if (base.startsWith('.')) return true;

  // Type definition files (.d.ts) — consumed by the compiler, not by imports
  if (filePath.endsWith('.d.ts')) return true;

  // Config files with compound names (vite.config.ts, jest.config.js, etc.)
  if (/[./]config$/i.test(stem) || /\.rc$/i.test(stem)) return true;

  // Test/spec files — invoked by test runners
  if (isTestFile(filePath)) return true;

  return false;
}

// Symbol names that are entry points, lifecycle hooks, or framework-called.
const FRAMEWORK_INVOKED_SYMBOLS = new Set([
  'main', 'run', 'start', 'serve', 'handler', 'execute', 'app',
  'setup', 'teardown', 'setUp', 'tearDown',
  'beforeAll', 'afterAll', 'beforeEach', 'afterEach', 'before', 'after',
  'constructor', 'init', 'initialize', 'configure', 'register',
  'middleware', 'plugin', 'default', 'module', 'exports',
]);

/**
 * Detect if a function signature is likely a class/instance method rather
 * than a standalone function. Method calls (obj.method()) are intentionally
 * not tracked as references (too noisy without type info), so all methods
 * appear orphaned. We exclude them to avoid flooding the results.
 */
function isLikelyMethod(signature, filePath) {
  if (!signature) return false;
  const s = signature.trimStart();

  const ext = filePath.substring(filePath.lastIndexOf('.'));

  // JS/TS: standalone functions always use the `function` keyword.
  // Class methods don't: `async ensure()`, `getGraph()`, `constructor()`.
  // Arrow functions assigned to vars are kind='variable', not 'function',
  // so they don't reach this check.
  if (['.js', '.mjs', '.cjs', '.jsx', '.ts', '.tsx'].includes(ext)) {
    return !/^(export\s+)?(default\s+)?(async\s+)?function[\s(]/.test(s);
  }

  // Python: methods have self or cls as first parameter
  if (ext === '.py') {
    return /\(\s*(self|cls)\s*[,)]/.test(s);
  }

  // Java/C#/Go: harder to detect without parent context — don't filter
  return false;
}

function isOrphanFalsePositiveSymbol(name, kind, filePath, signature) {
  if (FRAMEWORK_INVOKED_SYMBOLS.has(name)) return true;

  // Python dunders — called implicitly by the runtime
  if (name.startsWith('__') && name.endsWith('__')) return true;

  // Test functions — called by test runners
  if (name.startsWith('test_') || name.startsWith('Test') ||
      name.startsWith('spec_') || name.startsWith('Spec')) return true;

  // Very short names — too generic, ambiguity cap probably suppressed real refs
  if (name.length <= 2) return true;

  // Class/instance methods — obj.method() calls aren't tracked as references,
  // so every method appears orphaned. Filter them out.
  if (kind === 'function' && isLikelyMethod(signature, filePath)) return true;

  // Symbols in test files — all invoked by the test runner
  if (isTestFile(filePath)) return true;

  // Symbols in entry point / config files — reachable via runtime
  if (isOrphanFalsePositiveFile(filePath)) return true;

  // Symbol name matches file basename — likely the primary export
  const fileBase = basename(filePath).replace(/\.[^.]+$/, '');
  if (name === fileBase || name.toLowerCase() === fileBase.toLowerCase()) return true;

  return false;
}

/**
 * Find file nodes in the graph that look similar to the given path.
 * Uses basename matching and substring matching on the full path.
 */
function findSimilarFiles(graph, filePath, maxSuggestions = 5) {
  if (!graph) return [];
  const base = basename(filePath);
  const baseLower = base.toLowerCase();
  const pathLower = filePath.toLowerCase();
  const suggestions = [];

  graph.forEachNode((node, attrs) => {
    if (attrs.type !== 'file') return;
    const nodeLower = node.toLowerCase();
    const nodeBase = basename(node).toLowerCase();
    // Exact basename match or basename contains query
    if (nodeBase === baseLower || nodeBase.includes(baseLower) || nodeLower.includes(pathLower)) {
      suggestions.push(node);
    }
  });

  return suggestions.slice(0, maxSuggestions);
}

/**
 * Apply offset/limit pagination to an array.
 * Returns { items, total } where total is the unpaginated count.
 */
function paginate(arr, { offset = 0, limit } = {}) {
  const total = arr.length;
  const start = Math.max(0, offset);
  const items = limit !== undefined ? arr.slice(start, start + limit) : arr.slice(start);
  return { items, total };
}

class CodeIndex {
  constructor(projectRoot, opts = {}) {
    this.projectRoot = projectRoot;
    this.cache = new CodeIndexCache(projectRoot, opts);
    this._rankedCache = null;
    this._fileScoresCache = null;
  }

  async _ensureReady() {
    const result = await this.cache.ensure();
    if (result.changed > 0 || result.deleted > 0) {
      this._rankedCache = null;
      this._fileScoresCache = null;
    }
    return result;
  }

  /**
   * Lazy-cached PageRank scores for all symbols.
   * Unfocused (no bias) results are cached for the session.
   */
  _getRanked(focusFiles = []) {
    if (focusFiles.length === 0 && this._rankedCache) {
      return this._rankedCache;
    }
    const graph = this.cache.getGraph();
    if (!graph || graph.order === 0) return [];
    const ranked = rankedSymbols(graph, focusFiles);
    if (focusFiles.length === 0) {
      this._rankedCache = ranked;
    }
    return ranked;
  }

  /**
   * Lazy-cached file-level PageRank scores (sum of symbol scores per file).
   */
  _getFileScores() {
    if (this._fileScoresCache) return this._fileScoresCache;
    const ranked = this._getRanked();
    const graph = this.cache.getGraph();
    const scores = new Map();
    for (const [symbolKey, score] of ranked) {
      try {
        const attrs = graph.getNodeAttributes(symbolKey);
        if (attrs.type !== 'symbol') continue;
        scores.set(attrs.file, (scores.get(attrs.file) || 0) + score);
      } catch { continue; }
    }
    this._fileScoresCache = scores;
    return scores;
  }

  /**
   * Aider-style repo map: a compact summary of the most structurally
   * important definitions and their signatures, ranked by PageRank.
   *
   * @param {object} opts
   * @param {string[]} [opts.focusFiles] - Bias ranking toward these files
   * @param {number} [opts.offset] - Skip first N symbols
   * @param {number} [opts.limit] - Max symbols to return (default: 50)
   * @param {boolean} [opts.count] - If true, return only { total }
   * @returns {{content, shownFiles, shownSymbols, totalFiles, totalSymbols}|{total: number}}
   */
  async map({ focusFiles = [], offset, limit, count = false, structured = false } = {}) {
    const ensureResult = await this._ensureReady();
    const graph = this.cache.getGraph();
    if (!graph || graph.order === 0) {
      const diagnostics = {
        root: this.projectRoot,
        filesScanned: ensureResult.totalScanned || 0,
        extensions: this.cache.extensions.join(', '),
      };
      if (count) return { total: 0, diagnostics };
      if (structured) return { files: [], shownFiles: 0, shownSymbols: 0, totalFiles: 0, totalSymbols: 0, diagnostics };
      return { content: '(empty index)', shownFiles: 0, shownSymbols: 0, totalFiles: 0, totalSymbols: 0, diagnostics };
    }

    // Count totals from the graph
    let totalFiles = 0;
    let totalSymbols = 0;
    graph.forEachNode((_node, attrs) => {
      if (attrs.type === 'file') totalFiles++;
      else if (attrs.type === 'symbol') totalSymbols++;
    });

    const ranked = this._getRanked(focusFiles);

    // Collect all symbol entries ranked by PageRank (rich data for both formats)
    const allEntries = [];
    for (const [symbolKey, _score] of ranked) {
      let attrs;
      try {
        attrs = graph.getNodeAttributes(symbolKey);
      } catch {
        continue;
      }
      if (attrs.type !== 'symbol') continue;

      allEntries.push({
        file: attrs.file,
        name: attrs.name,
        kind: attrs.kind,
        lineStart: attrs.lineStart,
        lineEnd: attrs.lineEnd,
        signature: attrs.signature,
      });
    }

    if (count) return { total: allEntries.length };

    const { items } = paginate(allEntries, { offset, limit });

    // Structured format: return file objects with nested symbol arrays
    if (structured) {
      const fileGroups = new Map();
      for (const entry of items) {
        if (!fileGroups.has(entry.file)) fileGroups.set(entry.file, []);
        fileGroups.get(entry.file).push({
          name: entry.name,
          kind: entry.kind,
          lineStart: entry.lineStart,
          lineEnd: entry.lineEnd,
          signature: entry.signature,
        });
      }
      return {
        files: [...fileGroups.entries()].map(([path, symbols]) => ({ path, symbols })),
        shownFiles: fileGroups.size,
        shownSymbols: items.length,
        totalFiles,
        totalSymbols,
      };
    }

    // Text format (CLI output)
    const fileGroups = new Map();
    for (const entry of items) {
      if (!fileGroups.has(entry.file)) fileGroups.set(entry.file, []);
      const line = `  ${String(entry.lineStart).padStart(4)}│ ${entry.signature}`;
      fileGroups.get(entry.file).push(line);
    }

    const lines = [];
    for (const [file, entries] of fileGroups) {
      lines.push(`${file}:`);
      lines.push(...entries);
      lines.push('');
    }

    return {
      content: lines.join('\n').trimEnd(),
      shownFiles: fileGroups.size,
      shownSymbols: items.length,
      totalFiles,
      totalSymbols,
    };
  }

  /**
   * File tree with symbol counts per file.
   *
   * @param {object} [opts]
   * @param {number} [opts.depth] - Max directory depth (default unlimited)
   * @returns {string} Formatted tree
   */
  async structure({ depth } = {}) {
    await this._ensureReady();
    const graph = this.cache.getGraph();
    if (!graph || graph.order === 0) return '(empty index)';

    const fileNodes = [];
    graph.forEachNode((node, attrs) => {
      if (attrs.type === 'file') {
        fileNodes.push({ path: node, symbolCount: attrs.symbolCount || 0 });
      }
    });

    fileNodes.sort((a, b) => a.path.localeCompare(b.path));

    const tree = {};
    for (const { path: filePath, symbolCount } of fileNodes) {
      const parts = filePath.split(/[/\\]/);
      if (depth !== undefined && parts.length > depth + 1) continue;

      let current = tree;
      for (let i = 0; i < parts.length - 1; i++) {
        if (!current[parts[i]]) current[parts[i]] = {};
        current = current[parts[i]];
      }
      current[parts[parts.length - 1]] = symbolCount;
    }

    return formatTree(tree, '');
  }

  /**
   * Fuzzy-search symbols by substring matching against name and signature
   * (which includes parameter names and types). Results ranked by PageRank.
   *
   * @param {object} opts
   * @param {string} opts.query - Substring to match (case-insensitive)
   * @param {string} [opts.kind] - Filter to this kind (function, class, type, variable)
   * @param {number} [opts.offset] - Skip first N results
   * @param {number} [opts.limit] - Max results to return
   * @param {boolean} [opts.count] - If true, return only { total }
   * @returns {Array|{total: number}}
   */
  async search({ query, kind, offset, limit, count = false }) {
    await this._ensureReady();
    const graph = this.cache.getGraph();
    if (!graph || graph.order === 0) return count ? { total: 0 } : [];

    const ranked = this._getRanked();
    const scoreMap = new Map(ranked);
    const q = query.toLowerCase();

    const results = [];
    graph.forEachNode((node, attrs) => {
      if (attrs.type !== 'symbol') return;
      if (kind && attrs.kind !== kind) return;

      const nameMatch = attrs.name.toLowerCase().includes(q);
      const sigMatch = attrs.signature && attrs.signature.toLowerCase().includes(q);
      if (!nameMatch && !sigMatch) return;

      results.push({
        name: attrs.name,
        kind: attrs.kind,
        file: attrs.file,
        lineStart: attrs.lineStart,
        lineEnd: attrs.lineEnd,
        signature: attrs.signature,
        _score: scoreMap.get(node) || 0,
      });
    });

    results.sort((a, b) => b._score - a._score);
    for (const r of results) delete r._score;

    if (count) return { total: results.length };
    return paginate(results, { offset, limit }).items;
  }

  /**
   * List symbol definitions, optionally filtered.
   * Results are ranked by PageRank (most structurally important first).
   * Supports offset/limit pagination and count-only mode.
   *
   * @param {object} [opts]
   * @param {string} [opts.file] - Filter to this file
   * @param {string} [opts.kind] - Filter to this kind (function, class, type, variable)
   * @param {number} [opts.offset] - Skip first N results
   * @param {number} [opts.limit] - Max results to return
   * @param {boolean} [opts.count] - If true, return only { total }
   * @returns {Array|{total: number}}
   */
  async symbols({ file, kind, offset, limit, count = false } = {}) {
    await this._ensureReady();
    const graph = this.cache.getGraph();
    if (!graph || graph.order === 0) return count ? { total: 0 } : [];

    const ranked = this._getRanked();
    const scoreMap = new Map(ranked);

    const results = [];
    graph.forEachNode((node, attrs) => {
      if (attrs.type !== 'symbol') return;
      if (file && attrs.file !== file) return;
      if (kind && attrs.kind !== kind) return;
      results.push({
        name: attrs.name,
        kind: attrs.kind,
        file: attrs.file,
        lineStart: attrs.lineStart,
        lineEnd: attrs.lineEnd,
        signature: attrs.signature,
        _score: scoreMap.get(node) || 0,
      });
    });

    results.sort((a, b) => b._score - a._score);
    for (const r of results) delete r._score;

    if (count) return { total: results.length };
    return paginate(results, { offset, limit }).items;
  }

  /**
   * All call sites of a symbol across the codebase.
   * Results are ranked by file-level PageRank (most important callers first).
   * Supports offset/limit pagination and count-only mode.
   *
   * When `context` > 0, reads each caller file from disk and extracts
   * the actual call-site lines with surrounding context.
   *
   * @param {object} opts
   * @param {string} opts.symbol - Symbol name
   * @param {string} [opts.file] - Disambiguate by file
   * @param {number} [opts.context=0] - Lines of context around each call site (0 = off)
   * @param {number} [opts.offset] - Skip first N results
   * @param {number} [opts.limit] - Max results to return
   * @param {boolean} [opts.count] - If true, return only { total }
   * @returns {Array<{file, sites?}>|{total: number}}
   */
  async callers({ symbol, file, offset, limit, count = false, context = 0 }) {
    await this._ensureReady();
    const graph = this.cache.getGraph();
    if (!graph) return count ? { total: 0 } : [];

    const fileScores = this._getFileScores();

    const targetKeys = [];
    graph.forEachNode((node, attrs) => {
      if (attrs.type !== 'symbol') return;
      if (attrs.name !== symbol) return;
      if (file && attrs.file !== file) return;
      targetKeys.push(node);
    });

    const callerSet = new Set();
    const results = [];

    for (const targetKey of targetKeys) {
      graph.forEachInEdge(targetKey, (edge, attrs, source) => {
        if (attrs.type !== 'REFERENCES') return;
        const sourceAttrs = graph.getNodeAttributes(source);
        const callerFile = sourceAttrs.file || source;
        if (!callerSet.has(callerFile)) {
          callerSet.add(callerFile);
          results.push({ file: callerFile, _score: fileScores.get(callerFile) || 0 });
        }
      });
    }

    results.sort((a, b) => b._score - a._score);
    for (const r of results) delete r._score;

    if (count) return { total: results.length };

    const paged = paginate(results, { offset, limit }).items;

    if (context > 0) {
      // Match call sites: symbol followed by ( — avoids string literals and definitions
      const escaped = symbol.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
      const callPattern = new RegExp(`(?<![a-zA-Z0-9_])${escaped}\\s*\\(`);
      // Fallback: import/from lines that reference the symbol
      const importPattern = new RegExp(`(?:import|from)\\s.*\\b${escaped}\\b`);

      // Collect the target symbol's own definition ranges to exclude
      const defRanges = [];
      for (const tk of targetKeys) {
        try {
          const attrs = graph.getNodeAttributes(tk);
          if (attrs.type === 'symbol') defRanges.push({ file: attrs.file, start: attrs.lineStart, end: attrs.lineEnd });
        } catch { /* skip */ }
      }

      for (const entry of paged) {
        entry.sites = [];
        try {
          const absPath = join(this.projectRoot, entry.file);
          const source = await readFile(absPath, 'utf-8');
          const lines = source.split('\n');

          for (let i = 0; i < lines.length; i++) {
            const lineNum = i + 1;
            // Skip lines inside the symbol's own definition
            const inDef = defRanges.some(r => r.file === entry.file && lineNum >= r.start && lineNum <= r.end);
            if (inDef) continue;

            const line = lines[i];
            if (!callPattern.test(line) && !importPattern.test(line)) continue;

            const start = Math.max(0, i - context);
            const end = Math.min(lines.length - 1, i + context);
            const text = [];
            for (let j = start; j <= end; j++) {
              text.push({ line: j + 1, content: lines[j] });
            }
            entry.sites.push({ line: lineNum, text });
          }
        } catch {
          // File unreadable — skip context for this caller
        }
      }
    }

    return paged;
  }

  /**
   * What this file imports / depends on.
   * Results are ranked by file-level PageRank (most important dependencies first).
   * Supports offset/limit pagination and count-only mode.
   *
   * @param {object} opts
   * @param {string} opts.file - File path (relative to project root)
   * @param {number} [opts.offset] - Skip first N results
   * @param {number} [opts.limit] - Max results to return
   * @param {boolean} [opts.count] - If true, return only { total }
   * @returns {string[]|{total: number}}
   */
  async dependencies({ file, offset, limit, count = false }) {
    await this._ensureReady();
    const graph = this.cache.getGraph();
    if (!graph || !graph.hasNode(file)) {
      const suggestions = findSimilarFiles(graph, file);
      if (count) return { total: 0, fileNotFound: true, suggestions };
      return { items: [], fileNotFound: true, suggestions };
    }

    const fileScores = this._getFileScores();

    const deps = new Set();
    graph.forEachOutEdge(file, (edge, attrs, _source, target) => {
      if (attrs.type === 'IMPORTS') {
        const targetAttrs = graph.getNodeAttributes(target);
        if (targetAttrs.type === 'file') {
          deps.add(target);
        }
      }
    });

    const sorted = [...deps].sort((a, b) => (fileScores.get(b) || 0) - (fileScores.get(a) || 0));
    if (count) return { total: sorted.length };
    return paginate(sorted, { offset, limit }).items;
  }

  /**
   * What depends on this file.
   * Results are ranked by file-level PageRank (most important dependents first).
   * Supports offset/limit pagination and count-only mode.
   *
   * @param {object} opts
   * @param {string} opts.file - File path (relative to project root)
   * @param {number} [opts.offset] - Skip first N results
   * @param {number} [opts.limit] - Max results to return
   * @param {boolean} [opts.count] - If true, return only { total }
   * @returns {string[]|{total: number}}
   */
  async dependents({ file, offset, limit, count = false }) {
    await this._ensureReady();
    const graph = this.cache.getGraph();
    if (!graph || !graph.hasNode(file)) {
      const suggestions = findSimilarFiles(graph, file);
      if (count) return { total: 0, fileNotFound: true, suggestions };
      return { items: [], fileNotFound: true, suggestions };
    }

    const fileScores = this._getFileScores();

    const deps = new Set();
    graph.forEachInEdge(file, (edge, attrs, source) => {
      if (attrs.type === 'IMPORTS') {
        const sourceAttrs = graph.getNodeAttributes(source);
        if (sourceAttrs.type === 'file') {
          deps.add(source);
        }
      }
    });

    const sorted = [...deps].sort((a, b) => (fileScores.get(b) || 0) - (fileScores.get(a) || 0));
    if (count) return { total: sorted.length };
    return paginate(sorted, { offset, limit }).items;
  }

  /**
   * File-level dependency neighborhood with PageRank-ranked symbol signatures.
   *
   * BFS walks outgoing IMPORTS edges (dependencies). Optionally includes
   * direct dependents (files that import the starting file).
   *
   * Files are aggressively ranked by PageRank. Only the starting file's
   * direct neighbors are guaranteed; further-hop files must earn their
   * place via PageRank score. Default cap is 15 files.
   *
   * Supports count-only mode (returns sizes without content) and
   * offset/limit pagination on files (symbols follow the file list).
   *
   * @param {object} opts
   * @param {string} opts.file - Starting file
   * @param {number} [opts.hops=2] - Max hops along outgoing IMPORTS edges
   * @param {boolean} [opts.includeDependents=true] - Include files that directly import the starting file
   * @param {number} [opts.maxFiles=15] - Max files to include (direct neighbors always included)
   * @param {boolean} [opts.count=false] - If true, return only counts
   * @param {number} [opts.offset] - Skip first N files in output
   * @param {number} [opts.limit] - Max files to return (after ranking/capping)
   * @returns {{files, symbols, edges, total?}}
   */
  async neighborhood({
    file,
    hops = 2,
    includeDependents = true,
    maxFiles = 15,
    count = false,
    offset,
    limit,
  }) {
    await this._ensureReady();
    const graph = this.cache.getGraph();
    if (!graph || !graph.hasNode(file)) {
      const suggestions = findSimilarFiles(graph, file);
      return count
        ? { totalFiles: 0, totalSymbols: 0, totalEdges: 0, fileNotFound: true, suggestions }
        : { files: [], symbols: [], edges: [], fileNotFound: true, suggestions };
    }

    // BFS over file nodes, following outgoing IMPORTS edges (dependencies)
    // Track hop depth per file for ranking
    const fileHops = new Map(); // file -> hop distance
    const queue = [{ node: file, depth: 0 }];
    fileHops.set(file, 0);

    while (queue.length > 0) {
      const { node, depth } = queue.shift();
      if (depth >= hops) continue;

      graph.forEachOutEdge(node, (edge, attrs, _source, target) => {
        if (attrs.type !== 'IMPORTS') return;
        const targetAttrs = graph.getNodeAttributes(target);
        if (targetAttrs.type !== 'file') return;
        if (!fileHops.has(target)) {
          fileHops.set(target, depth + 1);
          queue.push({ node: target, depth: depth + 1 });
        }
      });
    }

    // Direct dependents (hop "0.5" — they import the starting file)
    const directDependents = new Set();
    if (includeDependents) {
      graph.forEachInEdge(file, (edge, attrs, source) => {
        if (attrs.type !== 'IMPORTS') return;
        const sourceAttrs = graph.getNodeAttributes(source);
        if (sourceAttrs.type !== 'file') return;
        directDependents.add(source);
        if (!fileHops.has(source)) {
          fileHops.set(source, 1);
        }
      });
    }

    // Identify direct neighbors (hop 1 deps + direct dependents)
    const directFiles = new Set([file]);
    graph.forEachOutEdge(file, (edge, attrs, _source, target) => {
      if (attrs.type === 'IMPORTS') directFiles.add(target);
    });
    for (const d of directDependents) directFiles.add(d);

    // Rank all visited files by PageRank, biased toward starting file
    const ranked = this._getRanked([file]);
    const prMap = new Map(ranked);

    // Score files: direct neighbors get a large bonus, then PageRank
    const allVisited = [...fileHops.keys()];
    const fileScored = allVisited.map(f => {
      const isDirect = directFiles.has(f);
      const hopDist = fileHops.get(f) || 99;
      // Sum PageRank of symbols in this file
      let filePR = 0;
      graph.forEachOutEdge(f, (edge, attrs, _source, target) => {
        if (attrs.type === 'DEFINES') {
          filePR += prMap.get(target) || 0;
        }
      });
      // Direct neighbors always sort first; within each tier, sort by PageRank
      const score = (isDirect ? 1e6 : 0) + filePR * 1e4 - hopDist;
      return { file: f, score, isDirect };
    });
    fileScored.sort((a, b) => b.score - a.score);

    // Cap: always include direct neighbors, then fill up to maxFiles with ranked hop-2+ files
    const cappedFiles = [];
    const cappedSet = new Set();
    for (const entry of fileScored) {
      if (entry.isDirect || cappedFiles.length < maxFiles) {
        cappedFiles.push(entry.file);
        cappedSet.add(entry.file);
      }
    }

    // Collect IMPORTS edges involving the starting file only
    const edges = [];
    const edgeSet = new Set();
    graph.forEachOutEdge(file, (edge, attrs, source, target) => {
      if (attrs.type !== 'IMPORTS') return;
      if (!cappedSet.has(target)) return;
      const key = `${source}->${target}`;
      if (!edgeSet.has(key)) {
        edgeSet.add(key);
        edges.push({ source, target, type: 'IMPORTS' });
      }
    });
    graph.forEachInEdge(file, (edge, attrs, source) => {
      if (attrs.type !== 'IMPORTS') return;
      if (!cappedSet.has(source)) return;
      const key = `${source}->${file}`;
      if (!edgeSet.has(key)) {
        edgeSet.add(key);
        edges.push({ source, target: file, type: 'IMPORTS' });
      }
    });

    // Collect all symbols from capped files, ranked by PageRank
    const symbols = [];
    for (const [symbolKey, _score] of ranked) {
      let attrs;
      try {
        attrs = graph.getNodeAttributes(symbolKey);
      } catch {
        continue;
      }
      if (attrs.type !== 'symbol') continue;
      if (!cappedSet.has(attrs.file)) continue;

      symbols.push({
        name: attrs.name,
        kind: attrs.kind,
        file: attrs.file,
        lineStart: attrs.lineStart,
        signature: attrs.signature,
      });
    }

    if (count) {
      return {
        totalFiles: cappedFiles.length,
        totalSymbols: symbols.length,
        totalEdges: edges.length,
        totalVisited: allVisited.length,
      };
    }

    // Apply pagination to files (symbols follow the file list)
    const { items: paginatedFiles } = paginate(cappedFiles, { offset, limit });
    const paginatedFileSet = new Set(paginatedFiles);
    const paginatedSymbols = symbols.filter(s => paginatedFileSet.has(s.file));
    const paginatedEdges = edges.filter(
      e => paginatedFileSet.has(e.source) || paginatedFileSet.has(e.target)
    );

    return {
      files: paginatedFiles,
      symbols: paginatedSymbols,
      edges: paginatedEdges,
      total: cappedFiles.length,
    };
  }

  /**
   * Find orphaned files or symbols — nodes with no cross-file connections.
   *
   * level='file': files with zero IMPORTS edges (neither importing nor imported).
   *   These are the "satellites" in the graph UI.
   *
   * level='symbol': symbols with no incoming REFERENCES from outside their own file.
   *   Dead code candidates — defined but never used cross-file.
   *
   * False positives (entry points, config files, test files, framework hooks,
   * dunders, etc.) are excluded by default.
   *
   * @param {object} [opts]
   * @param {'file'|'symbol'} [opts.level='file'] - Granularity
   * @param {string} [opts.kind] - Filter symbols by kind (only for level='symbol')
   * @param {number} [opts.offset] - Skip first N results
   * @param {number} [opts.limit] - Max results to return
   * @param {boolean} [opts.count=false] - If true, return only { total }
   * @returns {Array|{total: number}}
   */
  async orphans({ level = 'file', kind, offset, limit, count = false } = {}) {
    await this._ensureReady();
    const graph = this.cache.getGraph();
    if (!graph || graph.order === 0) return count ? { total: 0 } : [];

    if (level === 'file') {
      const results = [];
      graph.forEachNode((node, attrs) => {
        if (attrs.type !== 'file') return;

        // Skip false positives: entry points, config, tests
        if (isOrphanFalsePositiveFile(node)) return;

        // Check for any IMPORTS edge (in or out)
        let hasImport = false;
        graph.forEachEdge(node, (_edge, edgeAttrs) => {
          if (!hasImport && edgeAttrs.type === 'IMPORTS') hasImport = true;
        });

        if (!hasImport) {
          results.push({ file: node, symbolCount: attrs.symbolCount || 0 });
        }
      });

      // Meatier files first — more likely to be real orphans worth investigating
      results.sort((a, b) => b.symbolCount - a.symbolCount);
      if (count) return { total: results.length };
      return paginate(results, { offset, limit }).items;
    }

    if (level === 'symbol') {
      const results = [];
      graph.forEachNode((node, attrs) => {
        if (attrs.type !== 'symbol') return;
        if (kind && attrs.kind !== kind) return;

        // Skip false positives: framework hooks, dunders, test funcs, methods, etc.
        if (isOrphanFalsePositiveSymbol(attrs.name, attrs.kind, attrs.file, attrs.signature)) return;

        // Check for any incoming REFERENCES from a different file
        let hasExternalRef = false;
        graph.forEachInEdge(node, (_edge, edgeAttrs, source) => {
          if (hasExternalRef) return;
          if (edgeAttrs.type !== 'REFERENCES') return;
          try {
            const sourceFile = graph.getNodeAttribute(source, 'file') || source;
            if (sourceFile !== attrs.file) hasExternalRef = true;
          } catch {
            if (source !== attrs.file) hasExternalRef = true;
          }
        });

        if (!hasExternalRef) {
          results.push({
            name: attrs.name,
            kind: attrs.kind,
            file: attrs.file,
            lineStart: attrs.lineStart,
            signature: attrs.signature,
          });
        }
      });

      // Group by file, then by line within file
      results.sort((a, b) => a.file.localeCompare(b.file) || a.lineStart - b.lineStart);
      if (count) return { total: results.length };
      return paginate(results, { offset, limit }).items;
    }

    throw new Error(`Unknown level: "${level}". Use "file" or "symbol".`);
  }

  /**
   * File-level dependency graph for visualization.
   * Returns nodes (files) ranked by PageRank and IMPORTS edges between them.
   *
   * @param {object} opts
   * @param {number} [opts.limit] - Max nodes to return (default: 500)
   * @returns {{ nodes: Array<{id, label, category, score}>, edges: Array<{source, target}> }}
   */
  async graph({ limit = 500 } = {}) {
    await this._ensureReady();
    const graph = this.cache.getGraph();
    if (!graph || graph.order === 0) {
      return { nodes: [], edges: [] };
    }

    const fileScores = this._getFileScores();

    // Collect file nodes with scores, sorted by PageRank
    const fileEntries = [];
    graph.forEachNode((node, attrs) => {
      if (attrs.type !== 'file') return;
      fileEntries.push({ id: node, score: fileScores.get(node) || 0 });
    });
    fileEntries.sort((a, b) => b.score - a.score);

    // Cap to limit
    const capped = fileEntries.slice(0, limit);
    const cappedSet = new Set(capped.map(f => f.id));

    // Build nodes with category (first path segment) and label (filename)
    const nodes = capped.map(f => {
      const parts = f.id.split('/');
      const category = parts.length > 1 ? parts[0] : 'root';
      const label = parts[parts.length - 1].replace(/\.[^.]+$/, '');
      return { id: f.id, label, category, score: f.score };
    });

    // Collect IMPORTS edges between capped files
    const edges = [];
    const edgeSet = new Set();
    for (const f of capped) {
      graph.forEachOutEdge(f.id, (_edge, attrs, source, target) => {
        if (attrs.type !== 'IMPORTS') return;
        if (!cappedSet.has(target)) return;
        const key = `${source}->${target}`;
        if (!edgeSet.has(key)) {
          edgeSet.add(key);
          edges.push({ source, target });
        }
      });
    }

    return { nodes, edges };
  }

  /**
   * Get caller counts for all symbols defined in a file.
   * Returns a Map<symbolName, number> where the count is unique
   * files that reference each symbol (excluding self-references).
   *
   * @param {string} file - Relative file path
   * @returns {Map<string, number>}
   */
  async getCallerCounts(file) {
    await this._ensureReady();
    const graph = this.cache.getGraph();
    if (!graph) return new Map();

    const counts = new Map();
    graph.forEachNode((node, attrs) => {
      if (attrs.type !== 'symbol') return;
      if (attrs.file !== file) return;

      const callerFiles = new Set();
      graph.forEachInEdge(node, (_edge, edgeAttrs, source) => {
        if (edgeAttrs.type !== 'REFERENCES') return;
        const sourceAttrs = graph.getNodeAttributes(source);
        const sourceFile = sourceAttrs.file || source;
        if (sourceFile !== file) callerFiles.add(sourceFile);
      });

      if (callerFiles.size > 0) {
        counts.set(attrs.name, callerFiles.size);
      }
    });

    return counts;
  }

  /**
   * One-shot context: everything needed to understand a function.
   *
   * Returns the function's source, the signatures of functions/types it
   * references, and a callers summary — all from a single command.
   *
   * @param {object} opts
   * @param {string} opts.symbol - Symbol name
   * @param {string} [opts.file] - Disambiguate by file
   * @returns {object|null} { definition, usedSymbols, typeRefs, callers }
   */
  async context({ symbol, file }) {
    await this._ensureReady();
    const graph = this.cache.getGraph();
    if (!graph) return null;

    // Find the target symbol (highest PageRank if multiple)
    const candidates = [];
    graph.forEachNode((node, attrs) => {
      if (attrs.type !== 'symbol') return;
      if (attrs.name !== symbol) return;
      if (file && attrs.file !== file) return;
      candidates.push({ key: node, ...attrs });
    });
    if (candidates.length === 0) return null;

    const ranked = this._getRanked();
    const scoreMap = new Map(ranked);
    candidates.sort((a, b) => (scoreMap.get(b.key) || 0) - (scoreMap.get(a.key) || 0));
    const target = candidates[0];

    // Read the source file
    let source, lines;
    try {
      const absPath = join(this.projectRoot, target.file);
      source = await readFile(absPath, 'utf-8');
      lines = source.split('\n');
    } catch {
      return null;
    }

    // Extract the function body text
    const bodyLines = lines.slice(target.lineStart - 1, target.lineEnd);
    const bodyText = bodyLines.join('\n');

    // Build a set of all symbol names in the graph (for matching)
    const allSymbols = new Map(); // name -> [{ file, kind, signature, lineStart }]
    graph.forEachNode((node, attrs) => {
      if (attrs.type !== 'symbol') return;
      if (attrs.file === target.file && attrs.name === target.name) return; // skip self
      if (!allSymbols.has(attrs.name)) allSymbols.set(attrs.name, []);
      allSymbols.get(attrs.name).push({
        file: attrs.file,
        kind: attrs.kind,
        signature: attrs.signature,
        lineStart: attrs.lineStart,
        lineEnd: attrs.lineEnd,
      });
    });

    // Find symbols referenced in the body
    // Use word-boundary matching for each known symbol name
    // Skip very common names that cause false positives
    const NOISE_NAMES = new Set([
      'get', 'set', 'put', 'post', 'delete', 'head', 'patch',
      'start', 'stop', 'run', 'main', 'init', 'setup', 'close',
      'dict', 'list', 'str', 'int', 'bool', 'float', 'type',
      'key', 'value', 'name', 'data', 'config', 'result', 'error',
      'test', 'self', 'cls', 'app', 'log', 'logger',
      'enabled', 'default', 'constructor', 'length', 'size',
      'fetch', 'send', 'table', 'one', 'append', 'write', 'read',
      'update', 'create', 'find', 'add', 'remove', 'index', 'map',
      'filter', 'sort', 'join', 'split', 'trim', 'replace',
      'push', 'pop', 'shift', 'reduce', 'keys', 'values', 'items',
      'search', 'match', 'query', 'count', 'call', 'apply', 'bind',
    ]);
    const usedSymbols = [];
    const seen = new Set();
    for (const [name, defs] of allSymbols) {
      if (name.length < 3) continue; // skip very short names
      if (NOISE_NAMES.has(name)) continue;
      if (seen.has(name)) continue;
      const pattern = new RegExp(`(?<![a-zA-Z0-9_])${name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}(?![a-zA-Z0-9_])`);
      if (pattern.test(bodyText)) {
        seen.add(name);
        // Pick the best definition (same-file first, then highest PageRank)
        const sameFile = defs.find(d => d.file === target.file);
        const best = sameFile || defs[0];
        usedSymbols.push({ name, ...best });
      }
    }
    // Sort: functions first, then types, then by name
    const kindOrder = { function: 0, class: 1, type: 2, variable: 3 };
    usedSymbols.sort((a, b) => (kindOrder[a.kind] ?? 9) - (kindOrder[b.kind] ?? 9) || a.name.localeCompare(b.name));

    // Resolve type annotations in the signature
    // Extract type-like tokens from the signature (capitalized words, common patterns)
    const typeRefs = [];
    const sigText = target.signature || '';
    const typePattern = /(?<![a-zA-Z0-9_])([A-Z][a-zA-Z0-9_]+)(?![a-zA-Z0-9_])/g;
    const seenTypes = new Set();
    let match;
    while ((match = typePattern.exec(sigText)) !== null) {
      const typeName = match[1];
      if (seenTypes.has(typeName)) continue;
      seenTypes.add(typeName);
      const typeDefs = allSymbols.get(typeName);
      if (!typeDefs) continue;
      // Find the type definition and get its fields (expand its body)
      const best = typeDefs.find(d => d.file === target.file) || typeDefs[0];
      if (best.kind === 'class' || best.kind === 'type') {
        let fields = null;
        try {
          const typeAbsPath = join(this.projectRoot, best.file);
          const typeSource = await readFile(typeAbsPath, 'utf-8');
          const typeLines = typeSource.split('\n');
          // Extract the body lines (up to 15 lines to keep it compact)
          const maxPreview = 15;
          const bodyStart = best.lineStart; // 1-indexed
          const bodyEnd = Math.min(best.lineEnd, best.lineStart + maxPreview - 1);
          fields = typeLines.slice(bodyStart - 1, bodyEnd).map(l => l);
          if (best.lineEnd > bodyEnd) {
            fields.push(`    ... (${best.lineEnd - bodyEnd} more lines)`);
          }
        } catch { /* skip */ }
        typeRefs.push({ name: typeName, file: best.file, lineStart: best.lineStart, kind: best.kind, fields });
      }
    }

    // Get callers (external files only) — union across ALL same-name nodes
    // to handle ambiguous graph resolution (same name in different files)
    const callerFiles = new Set();
    for (const c of candidates) {
      graph.forEachInEdge(c.key, (_edge, attrs, src) => {
        if (attrs.type !== 'REFERENCES') return;
        const srcAttrs = graph.getNodeAttributes(src);
        const sf = srcAttrs.file || src;
        if (sf !== target.file) callerFiles.add(sf);
      });
    }

    // Remove symbols already shown in typeRefs to avoid redundancy
    const typeRefNames = new Set(typeRefs.map(t => t.name));
    const filteredSymbols = usedSymbols.filter(s => !typeRefNames.has(s.name));

    return {
      definition: {
        name: target.name,
        kind: target.kind,
        file: target.file,
        lineStart: target.lineStart,
        lineEnd: target.lineEnd,
        signature: target.signature,
        source: bodyLines,
      },
      usedSymbols: filteredSymbols,
      typeRefs,
      callers: [...callerFiles].sort(),
    };
  }

  /**
   * Git history of a specific function, using its tree-sitter line range.
   *
   * @param {object} opts
   * @param {string} opts.symbol - Symbol name
   * @param {string} [opts.file] - Disambiguate by file
   * @param {number} [opts.offset=0] - Skip first N commits
   * @param {number} [opts.limit=20] - Max commits to show
   * @param {boolean} [opts.patch=false] - Include function-scoped diffs
   * @returns {object|null} { definition, commits, raw? }
   */
  async history({ symbol, file, offset = 0, limit = 20, patch = false }) {
    await this._ensureReady();
    const graph = this.cache.getGraph();
    if (!graph) return null;

    const candidates = [];
    graph.forEachNode((node, attrs) => {
      if (attrs.type !== 'symbol') return;
      if (attrs.name !== symbol) return;
      if (file && attrs.file !== file) return;
      candidates.push(attrs);
    });
    if (candidates.length === 0) return null;

    const ranked = this._getRanked();
    const scoreMap = new Map(ranked);
    candidates.sort((a, b) => {
      const aKey = `${a.file}::${a.name}`;
      const bKey = `${b.file}::${b.name}`;
      return (scoreMap.get(bKey) || 0) - (scoreMap.get(aKey) || 0);
    });
    const target = candidates[0];

    const { execSync } = await import('child_process');
    try {
      if (patch) {
        // Full output with diffs — return raw text
        const raw = execSync(
          `git log -L ${target.lineStart},${target.lineEnd}:${target.file} --skip=${offset} -n ${limit}`,
          { cwd: this.projectRoot, encoding: 'utf-8', stdio: ['pipe', 'pipe', 'pipe'], timeout: 30000 }
        ).trim();
        return { definition: target, commits: [], raw };
      }
      // Summary only
      const output = execSync(
        `git log -L ${target.lineStart},${target.lineEnd}:${target.file} --no-patch --format="%h  %ad  %s" --date=short --skip=${offset} -n ${limit}`,
        { cwd: this.projectRoot, encoding: 'utf-8', stdio: ['pipe', 'pipe', 'pipe'], timeout: 30000 }
      ).trim();
      const commits = output ? output.split('\n').filter(Boolean) : [];
      return { definition: target, commits };
    } catch {
      return { definition: target, commits: [], error: 'git log failed — is this a git repo?' };
    }
  }

  /**
   * Recursive caller chain — walk UP the call graph from a symbol.
   * At each hop, resolves which function in the caller file contains
   * the call site by cross-referencing line numbers with definitions.
   *
   * Returns a tree: { name, file, line, callers: [...] }
   *
   * @param {object} opts
   * @param {string} opts.symbol - Starting symbol name
   * @param {string} [opts.file] - Disambiguate by file
   * @param {number} [opts.depth=3] - Max hops upward
   * @returns {object} Tree root node
   */
  async trace({ symbol, file, depth = 3 }) {
    await this._ensureReady();
    const graph = this.cache.getGraph();
    if (!graph) return null;

    // Find the target symbol node(s)
    const targetKeys = [];
    graph.forEachNode((node, attrs) => {
      if (attrs.type !== 'symbol') return;
      if (attrs.name !== symbol) return;
      if (file && attrs.file !== file) return;
      targetKeys.push(node);
    });

    if (targetKeys.length === 0) return null;

    // Use the first match (highest PageRank if multiple)
    const ranked = this._getRanked();
    const scoreMap = new Map(ranked);
    targetKeys.sort((a, b) => (scoreMap.get(b) || 0) - (scoreMap.get(a) || 0));
    const rootKey = targetKeys[0];
    const rootAttrs = graph.getNodeAttributes(rootKey);

    // Cache of file -> definitions (avoid re-parsing the same file)
    const defCache = new Map();

    const getFileDefs = async (filePath) => {
      if (defCache.has(filePath)) return defCache.get(filePath);
      try {
        const absPath = join(this.projectRoot, filePath);
        const source = await readFile(absPath, 'utf-8');
        const parsed = parseFile(filePath, source);
        const defs = parsed ? parsed.definitions.sort((a, b) => a.lineStart - b.lineStart) : [];
        defCache.set(filePath, defs);
        return defs;
      } catch {
        defCache.set(filePath, []);
        return [];
      }
    };

    // Find which definition in a file contains a given line
    const findContainingDef = (defs, line) => {
      for (let i = defs.length - 1; i >= 0; i--) {
        if (line >= defs[i].lineStart && line <= defs[i].lineEnd) return defs[i];
      }
      return null;
    };

    // BFS upward through callers
    const visited = new Set(); // "file::symbol" keys to prevent cycles

    const buildNode = async (symbolName, symbolFile, symbolLine, currentDepth) => {
      const nodeKey = `${symbolFile}::${symbolName}`;
      const node = { name: symbolName, file: symbolFile, line: symbolLine, callers: [] };

      if (currentDepth >= depth) return node;
      if (visited.has(nodeKey)) return node;
      visited.add(nodeKey);

      // Find this symbol in the graph
      const symKeys = [];
      graph.forEachNode((gNode, attrs) => {
        if (attrs.type !== 'symbol') return;
        if (attrs.name !== symbolName) return;
        if (attrs.file !== symbolFile) return;
        symKeys.push(gNode);
      });

      // Collect caller files
      const callerFiles = new Set();
      for (const sk of symKeys) {
        graph.forEachInEdge(sk, (_edge, attrs, source) => {
          if (attrs.type !== 'REFERENCES') return;
          const sourceAttrs = graph.getNodeAttributes(source);
          const sf = sourceAttrs.file || source;
          if (sf !== symbolFile) callerFiles.add(sf);
        });
      }

      // For each caller file, find which function contains the call
      const callPattern = new RegExp(
        `(?<![a-zA-Z0-9_])${symbolName.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\s*\\(`
      );

      for (const callerFile of callerFiles) {
        try {
          const absPath = join(this.projectRoot, callerFile);
          const source = await readFile(absPath, 'utf-8');
          const lines = source.split('\n');
          const defs = await getFileDefs(callerFile);

          // Find the first call site line
          let callLine = null;
          for (let i = 0; i < lines.length; i++) {
            if (callPattern.test(lines[i])) { callLine = i + 1; break; }
          }

          // Resolve to containing function
          const containingDef = callLine ? findContainingDef(defs, callLine) : null;

          if (containingDef) {
            const callerNode = await buildNode(containingDef.name, callerFile, containingDef.lineStart, currentDepth + 1);
            node.callers.push(callerNode);
          } else {
            // Top-level call (not inside any function) — show as file-level
            node.callers.push({ name: `<module>`, file: callerFile, line: callLine, callers: [] });
          }
        } catch {
          // Skip unreadable files
        }
      }

      // Sort callers by file for deterministic output
      node.callers.sort((a, b) => a.file.localeCompare(b.file));
      return node;
    };

    return buildNode(rootAttrs.name, rootAttrs.file, rootAttrs.lineStart, 0);
  }

  /**
   * Git-aware blast radius — what symbols changed and who calls them.
   *
   * Compares the working tree (or a git ref) against the index to find
   * added, removed, and modified symbols, then looks up their callers.
   *
   * @param {object} [opts]
   * @param {string} [opts.ref] - Git ref to diff against (default: HEAD)
   * @returns {{ changed: Array<{file, symbols: Array<{name, kind, change, callerCount}>}>, totalCallers: number }}
   */
  async diff({ ref = 'HEAD' } = {}) {
    await this._ensureReady();
    const graph = this.cache.getGraph();
    if (!graph) return { changed: [], totalCallers: 0 };

    // Get changed files from git
    const { execSync } = await import('child_process');
    let changedFiles;
    try {
      const output = execSync(`git diff --name-only ${ref}`, {
        cwd: this.projectRoot,
        encoding: 'utf-8',
        timeout: 10000,
      }).trim();
      if (!output) return { changed: [], totalCallers: 0 };
      changedFiles = output.split('\n').filter(Boolean);
    } catch {
      return { changed: [], totalCallers: 0, error: 'git diff failed — is this a git repo?' };
    }

    // Also include untracked new files
    try {
      const untracked = execSync('git ls-files --others --exclude-standard', {
        cwd: this.projectRoot,
        encoding: 'utf-8',
        timeout: 10000,
      }).trim();
      if (untracked) {
        for (const f of untracked.split('\n').filter(Boolean)) {
          if (!changedFiles.includes(f)) changedFiles.push(f);
        }
      }
    } catch { /* ignore */ }

    const results = [];
    let totalCallers = 0;

    for (const filePath of changedFiles) {
      // Get OLD symbols from git ref
      const oldSymbols = new Map();
      try {
        const oldSource = execSync(`git show ${ref}:${filePath}`, {
          cwd: this.projectRoot,
          encoding: 'utf-8',
          stdio: ['pipe', 'pipe', 'pipe'],
          timeout: 10000,
        });
        const oldParsed = parseFile(filePath, oldSource);
        if (oldParsed) {
          for (const def of oldParsed.definitions) {
            oldSymbols.set(def.name, { kind: def.kind, signature: def.signature });
          }
        }
      } catch {
        // File is new (doesn't exist in ref) — all symbols are "added"
      }

      // Get graph keys for caller lookups on current symbols
      const graphKeys = new Map();
      graph.forEachNode((node, attrs) => {
        if (attrs.type !== 'symbol' || attrs.file !== filePath) return;
        graphKeys.set(attrs.name, node);
      });

      // Parse the CURRENT file from disk
      let currentSymbols = new Map();
      try {
        const absPath = join(this.projectRoot, filePath);
        const source = await readFile(absPath, 'utf-8');
        const parsed = parseFile(filePath, source);
        if (parsed) {
          for (const def of parsed.definitions) {
            currentSymbols.set(def.name, {
              kind: def.kind,
              signature: def.signature,
            });
          }
        }
      } catch {
        // File might be deleted — all old symbols are "removed"
      }

      const symbolChanges = [];

      // Detect removed symbols (in old ref, not on disk)
      for (const [name, old] of oldSymbols) {
        if (!currentSymbols.has(name)) {
          const gk = graphKeys.get(name);
          const callerCount = gk ? this._countExternalCallers(graph, gk, filePath) : 0;
          symbolChanges.push({ name, kind: old.kind, change: 'removed', signature: old.signature, callerCount });
          totalCallers += callerCount;
        }
      }

      // Detect added and modified symbols
      for (const [name, current] of currentSymbols) {
        const old = oldSymbols.get(name);
        if (!old) {
          symbolChanges.push({ name, kind: current.kind, change: 'added', signature: current.signature, callerCount: 0 });
        } else if (old.signature !== current.signature) {
          const gk = graphKeys.get(name);
          const callerCount = gk ? this._countExternalCallers(graph, gk, filePath) : 0;
          symbolChanges.push({ name, kind: current.kind, change: 'modified', signature: current.signature, callerCount });
          totalCallers += callerCount;
        }
      }

      if (symbolChanges.length > 0) {
        symbolChanges.sort((a, b) => b.callerCount - a.callerCount);
        results.push({ file: filePath, symbols: symbolChanges });
      }
    }

    results.sort((a, b) => {
      const aMax = Math.max(0, ...a.symbols.map(s => s.callerCount));
      const bMax = Math.max(0, ...b.symbols.map(s => s.callerCount));
      return bMax - aMax;
    });

    return { changed: results, totalCallers };
  }

  /** Count unique external files that reference a symbol node. */
  _countExternalCallers(graph, symbolKey, symbolFile) {
    const files = new Set();
    graph.forEachInEdge(symbolKey, (_edge, attrs, source) => {
      if (attrs.type !== 'REFERENCES') return;
      const sourceAttrs = graph.getNodeAttributes(source);
      const sf = sourceAttrs.file || source;
      if (sf !== symbolFile) files.add(sf);
    });
    return files.size;
  }

  /**
   * Force a full rebuild.
   */
  async reindex() {
    this._rankedCache = null;
    this._fileScoresCache = null;
    return this.cache.reindex();
  }

  /**
   * Get index stats.
   */
  async stats() {
    await this._ensureReady();
    const graph = this.cache.getGraph();
    if (!graph) return { files: 0, symbols: 0, edges: 0 };

    let files = 0;
    let symbols = 0;
    graph.forEachNode((_node, attrs) => {
      if (attrs.type === 'file') files++;
      else if (attrs.type === 'symbol') symbols++;
    });

    return { files, symbols, edges: graph.size };
  }
}

function formatTree(obj, indent) {
  const lines = [];
  const entries = Object.entries(obj).sort(([a], [b]) => a.localeCompare(b));

  for (const [key, value] of entries) {
    if (typeof value === 'number') {
      lines.push(`${indent}${key} (${value} symbols)`);
    } else {
      lines.push(`${indent}${key}/`);
      lines.push(formatTree(value, indent + '  '));
    }
  }

  return lines.join('\n');
}

export { CodeIndex };
export default CodeIndex;
