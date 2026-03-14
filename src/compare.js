import { readFile, stat as fsStat } from 'fs/promises';
import { glob } from 'glob';
import { join, relative, basename } from 'path';
import { parseFile, SUPPORTED_EXTENSIONS } from './parser.js';

const IGNORE_PATTERNS = [
  '**/node_modules/**', '**/.npm/**', '**/.yarn/**', '**/.pnp.*',
  '**/bower_components/**', '**/*.min.js', '**/*.bundle.js', '**/*.map',
  '**/__pycache__/**', '**/.venv/**', '**/venv/**', '**/env/**',
  '**/.env/**', '**/.virtualenvs/**', '**/site-packages/**',
  '**/*.egg-info/**', '**/.eggs/**', '**/dist/**', '**/build/**',
  '**/.git/**', '**/.svn/**', '**/.hg/**',
  '**/vendor/**', '**/tmp/**', '**/temp/**',
  '**/.idea/**', '**/.vscode/**', '**/.DS_Store',
  '**/Pods/**', '**/DerivedData/**',
];

// Names too generic to be meaningful matches across codebases.
// These are filtered on EXACT name match only — `processEvent` survives,
// only bare `process` is dropped.
const NOISE_NAMES = new Set([
  // Ultra-common function names
  'get', 'set', 'run', 'main', 'init', 'setup', 'start', 'stop',
  'open', 'close', 'read', 'write', 'delete', 'update', 'create',
  'add', 'remove', 'clear', 'reset', 'test', 'check', 'load',
  'toString', 'toJSON', 'valueOf', 'hash', 'eq', 'repr', 'str',
  'copy', 'keys', 'values', 'items', 'pop', 'push', 'append',
  'default', 'setdefault', 'apply', 'call', 'bind',
  'map', 'filter', 'reduce', 'format', 'parse', 'validate',
  'serialize', 'deserialize', 'configure', 'connect',
  // Python dunders
  '__init__', '__repr__', '__str__', '__eq__', '__hash__',
  '__enter__', '__exit__', '__iter__', '__next__', '__len__',
  '__getitem__', '__setitem__', '__delitem__', '__contains__',
  '__call__', '__bool__', '__getattr__', '__setattr__', '__delattr__',
  '__get__', '__set__', '__delete__',
  // JS common
  'constructor', 'render', 'process', 'handle', 'execute',
  // Single-char and trivially short names
  'a', 'b', 'c', 'd', 'e', 'f', 'x', 'y', 'n', 'i', 'j', 'k',
  // Common test fixture names
  'foo', 'bar', 'baz', 'wrapper', 'decorator', 'callback',
  'index', 'app', 'client', 'response', 'request',
]);

async function scanAndParse(dirPath) {
  const pattern = `**/*{${SUPPORTED_EXTENSIONS.join(',')}}`;
  const files = await glob(pattern, {
    cwd: dirPath,
    ignore: IGNORE_PATTERNS,
    absolute: true,
    nodir: true,
  });

  const results = [];
  for (const absPath of files) {
    const relPath = relative(dirPath, absPath);
    try {
      const source = await readFile(absPath, 'utf-8');
      const result = parseFile(relPath, source);
      if (result) results.push(result);
    } catch {
      // skip unparseable files
    }
  }
  return results;
}

async function parseSingleFile(filePath) {
  const source = await readFile(filePath, 'utf-8');
  const result = parseFile(basename(filePath), source);
  return result ? [result] : [];
}

function extractSymbols(parseResults) {
  const symbols = [];
  for (const fileResult of parseResults) {
    for (const def of fileResult.definitions) {
      symbols.push({
        name: def.name,
        kind: def.kind,
        file: fileResult.file,
        lineStart: def.lineStart,
        lineEnd: def.lineEnd,
        signature: def.signature,
        paramCount: (def.paramNames || []).length,
        paramNames: def.paramNames || [],
        localRefs: def.localRefs || [],
        bodyLines: (def.lineEnd || 0) - (def.lineStart || 0),
      });
    }
  }
  return symbols;
}

// Test file detection
const TEST_SEGMENTS = ['test/', 'tests/', '__tests__/', 'spec/', 'specs/', 'conftest'];
function isTestFile(file) {
  const lower = file.toLowerCase();
  return TEST_SEGMENTS.some(s => lower.includes(s)) || basename(file).startsWith('test_');
}

/**
 * Compare two codebases (files or directories).
 *
 * Returns deterministic structural facts grouped by symbol name.
 * Filters out noise (test_ prefixes, dunders, trivially generic names).
 *
 * Shared symbols are ranked by sharedRefs count (how many internal
 * function calls they have in common — the strongest signal for
 * "these are likely doing the same thing").
 */
async function compare(pathA, pathB, { kind, includeTests = false } = {}) {
  // Validate paths exist
  let statA, statB;
  try {
    statA = await fsStat(pathA);
  } catch {
    throw new Error(`Path A does not exist: ${pathA}`);
  }
  try {
    statB = await fsStat(pathB);
  } catch {
    throw new Error(`Path B does not exist: ${pathB}`);
  }

  const parseResultsA = statA.isDirectory()
    ? await scanAndParse(pathA)
    : await parseSingleFile(pathA);
  const parseResultsB = statB.isDirectory()
    ? await scanAndParse(pathB)
    : await parseSingleFile(pathB);

  let symbolsA = extractSymbols(parseResultsA);
  let symbolsB = extractSymbols(parseResultsB);

  // Apply kind filter
  if (kind) {
    symbolsA = symbolsA.filter(s => s.kind === kind);
    symbolsB = symbolsB.filter(s => s.kind === kind);
  }

  // Filter out test functions, test files, and noise unless explicitly included
  const isSignificant = (s) => {
    if (!includeTests && s.name.startsWith('test_')) return false;
    if (!includeTests && s.name.startsWith('Test')) return false;
    if (!includeTests && isTestFile(s.file)) return false;
    if (NOISE_NAMES.has(s.name)) return false;
    return true;
  };

  symbolsA = symbolsA.filter(isSignificant);
  symbolsB = symbolsB.filter(isSignificant);

  // Deduplicate (same name+file+line can appear from overlapping tree-sitter captures)
  const dedup = (syms) => {
    const seen = new Set();
    return syms.filter(s => {
      const key = `${s.name}::${s.file}::${s.lineStart}`;
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    });
  };
  symbolsA = dedup(symbolsA);
  symbolsB = dedup(symbolsB);

  // Group by name
  const byNameA = new Map();
  for (const s of symbolsA) {
    if (!byNameA.has(s.name)) byNameA.set(s.name, []);
    byNameA.get(s.name).push(s);
  }
  const byNameB = new Map();
  for (const s of symbolsB) {
    if (!byNameB.has(s.name)) byNameB.set(s.name, []);
    byNameB.get(s.name).push(s);
  }

  // Shared: names that exist in both. Group all definitions under one entry.
  const shared = [];
  for (const [name, symsA] of byNameA) {
    if (!byNameB.has(name)) continue;
    const symsB = byNameB.get(name);

    // Collect all local refs across all definitions of this name
    const allRefsA = new Set();
    const allRefsB = new Set();
    for (const s of symsA) for (const r of s.localRefs) allRefsA.add(r);
    for (const s of symsB) for (const r of s.localRefs) allRefsB.add(r);
    const sharedRefs = [...allRefsA].filter(r => allRefsB.has(r));

    // Check if any pair has matching kind and similar param count
    const sameKind = symsA.some(a => symsB.some(b => a.kind === b.kind));
    const sameParamCount = symsA.some(a => symsB.some(b => a.paramCount === b.paramCount));

    shared.push({
      name,
      inA: symsA.map(s => ({ kind: s.kind, file: s.file, line: s.lineStart, signature: s.signature, paramCount: s.paramCount, bodyLines: s.bodyLines })),
      inB: symsB.map(s => ({ kind: s.kind, file: s.file, line: s.lineStart, signature: s.signature, paramCount: s.paramCount, bodyLines: s.bodyLines })),
      sharedRefs,
      sameKind,
      sameParamCount,
    });
  }

  // Sort shared by sharedRefs count (strongest consolidation signal),
  // then by whether kind/params match, then alphabetically
  shared.sort((a, b) => {
    const refDiff = b.sharedRefs.length - a.sharedRefs.length;
    if (refDiff !== 0) return refDiff;
    // Prefer same-kind matches
    if (a.sameKind !== b.sameKind) return a.sameKind ? -1 : 1;
    // Prefer same-param-count matches
    if (a.sameParamCount !== b.sameParamCount) return a.sameParamCount ? -1 : 1;
    // Alphabetical tiebreak
    return a.name.localeCompare(b.name);
  });

  // Only in A / Only in B — sorted alphabetically
  const onlyA = [];
  for (const [name, syms] of byNameA) {
    if (byNameB.has(name)) continue;
    for (const s of syms) {
      onlyA.push({ name, kind: s.kind, file: s.file, line: s.lineStart, signature: s.signature });
    }
  }
  // Sort: public names first, then _private, alphabetical within each group
  const privateLast = (a, b) => {
    const aPrivate = a.name.startsWith('_');
    const bPrivate = b.name.startsWith('_');
    if (aPrivate !== bPrivate) return aPrivate ? 1 : -1;
    return a.name.localeCompare(b.name);
  };
  onlyA.sort(privateLast);

  const onlyB = [];
  for (const [name, syms] of byNameB) {
    if (byNameA.has(name)) continue;
    for (const s of syms) {
      onlyB.push({ name, kind: s.kind, file: s.file, line: s.lineStart, signature: s.signature });
    }
  }
  onlyB.sort(privateLast);

  // File-level comparison (basename matching for directory mode)
  // Filter out basenames that are too generic to be meaningful matches
  const NOISE_BASENAMES = new Set([
    '__init__.py', 'conftest.py', 'conf.py', 'setup.py', 'setup.cfg',
    'index.js', 'index.ts', 'index.tsx', 'main.py', 'main.go', 'main.rs',
    'app.py', 'app.js', 'app.ts', 'mod.rs', 'lib.rs',
    'utils.py', 'utils.js', 'utils.ts', 'helpers.py', 'helpers.js',
    'types.ts', 'types.py', 'config.py', 'config.js', 'config.ts',
    'constants.py', 'constants.js', 'constants.ts',
  ]);
  const filesA = parseResultsA.map(r => r.file);
  const filesB = parseResultsB.map(r => r.file);
  const basenamesA = new Set(filesA.map(f => basename(f)));
  const basenamesB = new Set(filesB.map(f => basename(f)));
  const sharedFiles = [...basenamesA].filter(f => basenamesB.has(f) && !NOISE_BASENAMES.has(f));
  const onlyFilesA = [...basenamesA].filter(f => !basenamesB.has(f));
  const onlyFilesB = [...basenamesB].filter(f => !basenamesA.has(f));

  return {
    labelA: statA.isDirectory() ? pathA : basename(pathA),
    labelB: statB.isDirectory() ? pathB : basename(pathB),
    isDirectoryMode: statA.isDirectory() || statB.isDirectory(),
    shared,
    onlyA,
    onlyB,
    files: {
      shared: sharedFiles,
      onlyA: onlyFilesA,
      onlyB: onlyFilesB,
      totalA: filesA.length,
      totalB: filesB.length,
    },
    summary: {
      totalA: symbolsA.length,
      totalB: symbolsB.length,
      sharedNames: shared.length,
      onlyACount: onlyA.length,
      onlyBCount: onlyB.length,
    },
  };
}

export { compare };
