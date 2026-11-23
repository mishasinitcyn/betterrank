import graphology from 'graphology';
const { MultiDirectedGraph } = graphology;
import pagerankModule from 'graphology-metrics/centrality/pagerank.js';
const pagerank = pagerankModule.default || pagerankModule;
import { writeFile, readFile, mkdir } from 'fs/promises';
import { dirname, posix } from 'path';
import { gzipSync, gunzipSync } from 'zlib';

// Max definitions for a name before cross-file wiring is skipped entirely
// (when no explicit import connects source -> target).
const AMBIGUITY_CAP = 5;

const IMPORT_RESOLVE_EXTENSIONS = [
  '.ts', '.tsx', '.js', '.jsx', '.mjs', '.cjs',
  '.py', '.rs', '.go', '.rb', '.java',
  '.c', '.h', '.cpp', '.hpp', '.cc', '.cs', '.php',
];

const FOCUS_MAX_HOPS = 2;
const FOCUS_DISTANCE_WEIGHTS = new Map([
  [0, 250],
  [1, 12],
  [2, 2.5],
]);
const FOCUS_DEFAULT_WEIGHT = 0.15;

// Ubiquitous built-in / runtime method names that should not be cross-wired
// via untyped `obj.method()` member access.
const METHOD_NOISE_NAMES = new Set([
  'constructor', 'toString', 'valueOf', 'toJSON', 'hasOwnProperty',
  'map', 'filter', 'reduce', 'forEach', 'flat', 'flatMap', 'some', 'every',
  'find', 'findIndex', 'includes', 'indexOf', 'lastIndexOf', 'push', 'pop',
  'shift', 'unshift', 'slice', 'splice', 'concat', 'join', 'split', 'trim',
  'trimStart', 'trimEnd', 'replace', 'replaceAll', 'match', 'matchAll',
  'startsWith', 'endsWith', 'toLowerCase', 'toUpperCase', 'substring', 'substr',
  'keys', 'values', 'entries', 'from', 'of', 'isArray', 'assign', 'freeze',
  'then', 'catch', 'finally', 'bind', 'call', 'apply',
  'get', 'set', 'has', 'add', 'delete', 'clear', 'size', 'length', 'prototype',
  'log', 'warn', 'error', 'info', 'debug', 'trace',
  'json', 'text', 'blob', 'arrayBuffer', 'clone', 'headers', 'status',
  'all', 'first', 'run', 'exec', 'test', 'sort', 'reverse', 'fill',
  'emit', 'on', 'off', 'once', 'addEventListener', 'removeEventListener',
  'preventDefault', 'stopPropagation', 'stringify', 'parse', 'append', 'close',
  'open', 'start', 'stop', 'init', 'copy',
  '__init__', '__repr__', '__str__', '__eq__', '__hash__', '__len__',
  '__iter__', '__next__', '__enter__', '__exit__', '__getitem__', '__setitem__',
]);

const KIND_TO_CODE = {
  function: 'f',
  method: 'm',
  class: 'c',
  type: 't',
  variable: 'v',
  property: 'p',
  namespace: 'n',
  other: 'o',
};

const CODE_TO_KIND = Object.fromEntries(
  Object.entries(KIND_TO_CODE).map(([k, v]) => [v, k]),
);

function normalizeFilePath(filePath) {
  return filePath.replace(/\\/g, '/');
}

function buildDefinitionIndex(graph) {
  const defIndex = new Map();

  graph.forEachNode((node, attrs) => {
    if (attrs.type !== 'symbol') return;
    if (!defIndex.has(attrs.name)) defIndex.set(attrs.name, []);
    defIndex.get(attrs.name).push(node);
  });

  return defIndex;
}

function buildFileLookup(graph) {
  const fileLookup = new Map();

  graph.forEachNode((node, attrs) => {
    if (attrs.type !== 'file') return;
    fileLookup.set(normalizeFilePath(node), node);
  });

  return fileLookup;
}

/**
 * Build a lookup of direct (1-hop) and barrel-forwarded (2-hop) imported files
 * for each file node so reference wiring can prefer explicitly imported targets.
 */
function buildImportReachability(graph) {
  const directImports = new Map();
  graph.forEachNode((node, attrs) => {
    if (attrs.type !== 'file') return;
    const targets = new Set();
    graph.forEachOutEdge(node, (_edge, eAttrs, _src, target) => {
      if (eAttrs.type === 'IMPORTS') targets.add(target);
    });
    if (targets.size > 0) directImports.set(node, targets);
  });

  return {
    getDirect(file) {
      return directImports.get(file) || null;
    },
    getTwoHop(file) {
      const direct = directImports.get(file);
      if (!direct) return null;
      const twoHop = new Set();
      for (const mid of direct) {
        const next = directImports.get(mid);
        if (!next) continue;
        for (const t of next) {
          if (t !== file && !direct.has(t)) twoHop.add(t);
        }
      }
      return twoHop.size > 0 ? twoHop : null;
    },
  };
}

/**
 * Disambiguate which targets a reference should wire to.
 *
 * 1. If a same-file definition exists, wire to it only (skip cross-file).
 * 2. If the source file explicitly imports target file(s) (1-hop, then 2-hop),
 *    wire only to those imported targets — even if the name exceeds AMBIGUITY_CAP globally.
 * 3. If the name is unambiguous (1 definition), wire to it.
 * 4. If targets exceed AMBIGUITY_CAP, skip all (too noisy).
 * 5. Otherwise wire to all cross-file targets (low-ambiguity).
 */
function disambiguateTargets(targets, sourceFile, graph, importReach = null) {
  if (targets.length === 0) return [];
  if (targets.length === 1) return targets;

  const sameFile = targets.filter(t => {
    try { return graph.getNodeAttribute(t, 'file') === sourceFile; } catch { return false; }
  });
  if (sameFile.length > 0) return sameFile;

  if (importReach) {
    const direct = importReach.getDirect(sourceFile);
    if (direct) {
      const directMatches = targets.filter(t => {
        try { return direct.has(graph.getNodeAttribute(t, 'file')); } catch { return false; }
      });
      if (directMatches.length > 0) return directMatches;

      const twoHop = importReach.getTwoHop(sourceFile);
      if (twoHop) {
        const twoHopMatches = targets.filter(t => {
          try { return twoHop.has(graph.getNodeAttribute(t, 'file')); } catch { return false; }
        });
        if (twoHopMatches.length > 0) return twoHopMatches;
      }
    }
  }

  if (targets.length > AMBIGUITY_CAP) return [];
  return targets;
}

function isPythonFile(filePath) {
  return normalizeFilePath(filePath).endsWith('.py');
}

function resolvePythonImportBase(sourceFile, specifier) {
  if (!isPythonFile(sourceFile)) return null;
  if (!specifier) return null;

  if (specifier.startsWith('.')) {
    const match = specifier.match(/^(\.+)(.*)$/);
    if (!match) return null;

    const dots = match[1].length;
    let baseDir = posix.dirname(sourceFile);
    for (let i = 1; i < dots; i++) {
      baseDir = posix.dirname(baseDir);
    }

    const remainder = match[2].replace(/^\./, '');
    if (!remainder) return baseDir;

    return posix.normalize(posix.join(baseDir, remainder.replace(/\./g, '/')));
  }

  if (/^[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z_][A-Za-z0-9_]*)*$/.test(specifier)) {
    return specifier.replace(/\./g, '/');
  }

  return null;
}

function resolveImportBase(sourceFile, specifier) {
  const normalizedSource = normalizeFilePath(sourceFile);
  const normalizedSpecifier = normalizeFilePath(specifier);
  if (!normalizedSpecifier || normalizedSpecifier.startsWith('node:')) return null;

  if (normalizedSpecifier.startsWith('.')) {
    return posix.normalize(posix.join(posix.dirname(normalizedSource), normalizedSpecifier));
  }

  if (normalizedSpecifier.startsWith('~/') || normalizedSpecifier.startsWith('@/')) {
    return posix.normalize(posix.join('src', normalizedSpecifier.slice(2)));
  }

  if (normalizedSpecifier.startsWith('/')) {
    return posix.normalize(normalizedSpecifier.slice(1));
  }

  if (normalizedSpecifier.startsWith('src/')) {
    return posix.normalize(normalizedSpecifier);
  }

  const pythonBase = resolvePythonImportBase(normalizedSource, normalizedSpecifier);
  if (pythonBase) return pythonBase;

  return null;
}

function hasExplicitImportExtension(filePath) {
  return IMPORT_RESOLVE_EXTENSIONS.some(ext => filePath.endsWith(ext));
}

function resolveImportTargetFile(sourceFile, specifier, fileLookup) {
  const importBase = resolveImportBase(sourceFile, specifier);
  if (!importBase) return null;

  const candidates = [importBase];
  if (!hasExplicitImportExtension(importBase)) {
    for (const ext of IMPORT_RESOLVE_EXTENSIONS) {
      candidates.push(`${importBase}${ext}`);
    }
    const packageEntryNames = isPythonFile(sourceFile) ? ['__init__'] : ['index'];
    for (const ext of IMPORT_RESOLVE_EXTENSIONS) {
      for (const entryName of packageEntryNames) {
        candidates.push(posix.join(importBase, `${entryName}${ext}`));
      }
    }
  }

  for (const candidate of candidates) {
    const resolved = fileLookup.get(normalizeFilePath(candidate));
    if (resolved) return resolved;
  }

  return null;
}

function wireExplicitImportEdges(records, graph, fileLookup, addedImports) {
  for (const { file, imports = [] } of records) {
    for (const entry of imports) {
      const targetFile = resolveImportTargetFile(file, entry.path, fileLookup);
      if (!targetFile || targetFile === file) continue;

      const impKey = `${file}\0${targetFile}`;
      if (addedImports.has(impKey)) continue;

      addedImports.add(impKey);
      graph.addEdge(file, targetFile, { type: 'IMPORTS' });
    }
  }
}

function wireSymbolReferences(records, graph, defIndex, addedRefs, addedImports, importReach) {
  for (const { file, references } of records) {
    for (const ref of references) {
      const targets = defIndex.get(ref.name);
      if (!targets) continue;

      let resolvedTargets;
      let allowSyntheticImport = true;

      if (ref.kind === 'method') {
        // Only wire `obj.method()` calls to class methods defined in the same
        // file or in explicitly imported files (1-hop or 2-hop). This avoids
        // cross-app false positives like `url.search` or `Object.create`.
        if (METHOD_NOISE_NAMES.has(ref.name) || ref.name.length < 3) continue;
        const methodTargets = targets.filter(target => {
          try { return graph.getNodeAttribute(target, 'kind') === 'method'; } catch { return false; }
        });
        if (methodTargets.length === 0) continue;

        const sameFile = methodTargets.filter(t => {
          try { return graph.getNodeAttribute(t, 'file') === file; } catch { return false; }
        });
        if (sameFile.length > 0) {
          resolvedTargets = sameFile;
        } else if (importReach) {
          const direct = importReach.getDirect(file);
          const directMatches = direct
            ? methodTargets.filter(t => {
              try { return direct.has(graph.getNodeAttribute(t, 'file')); } catch { return false; }
            })
            : [];
          if (directMatches.length > 0) {
            resolvedTargets = directMatches;
          } else {
            const twoHop = importReach.getTwoHop(file);
            resolvedTargets = twoHop
              ? methodTargets.filter(t => {
                try { return twoHop.has(graph.getNodeAttribute(t, 'file')); } catch { return false; }
              })
              : [];
          }
        } else {
          resolvedTargets = [];
        }
        allowSyntheticImport = false;
      } else if (ref.kind === 'property') {
        const propTargets = targets.filter(target => {
          try { return graph.getNodeAttribute(target, 'kind') === 'property'; } catch { return false; }
        });
        if (propTargets.length === 0) continue;
        resolvedTargets = disambiguateTargets(propTargets, file, graph, importReach);
      } else {
        // Bare symbol refs (`foo()`, `import { Foo }`, type refs) cannot invoke
        // instance class methods (`obj.foo()`), which are handled via `ref.kind === 'method'`.
        const nonMethodTargets = targets.filter(target => {
          try { return graph.getNodeAttribute(target, 'kind') !== 'method'; } catch { return false; }
        });
        if (nonMethodTargets.length === 0) continue;
        resolvedTargets = disambiguateTargets(nonMethodTargets, file, graph, importReach);
      }

      if (resolvedTargets.length === 0) continue;

      for (const target of resolvedTargets) {
        const targetFile = graph.getNodeAttribute(target, 'file');

        const refKey = `${file}\0${target}`;
        if (!addedRefs.has(refKey)) {
          addedRefs.add(refKey);
          graph.addEdge(file, target, { type: 'REFERENCES' });
        }

        if (allowSyntheticImport && targetFile !== file) {
          const impKey = `${file}\0${targetFile}`;
          if (!addedImports.has(impKey)) {
            addedImports.add(impKey);
            graph.addEdge(file, targetFile, { type: 'IMPORTS' });
          }
        }
      }
    }
  }
}

function mergeDefinitions(graph, records) {
  for (const { file, definitions } of records) {
    graph.mergeNode(file, { type: 'file', symbolCount: definitions.length });

    const usedKeys = new Set();
    for (const def of definitions) {
      let symbolKey = `${file}::${def.name}`;
      if (usedKeys.has(symbolKey)) {
        symbolKey = `${file}::${def.name}@${def.lineStart}`;
      }
      usedKeys.add(symbolKey);

      graph.mergeNode(symbolKey, {
        type: 'symbol',
        kind: def.kind,
        name: def.name,
        file,
        lineStart: def.lineStart,
        lineEnd: def.lineEnd,
        signature: def.signature,
      });
      graph.addEdge(file, symbolKey, { type: 'DEFINES' });
    }
  }
}

function buildGraph(allSymbols) {
  const graph = new MultiDirectedGraph({ allowSelfLoops: false });
  mergeDefinitions(graph, allSymbols);
  const defIndex = buildDefinitionIndex(graph);
  const fileLookup = buildFileLookup(graph);

  const addedRefs = new Set();
  const addedImports = new Set();
  // Wire explicit imports FIRST so reference/method disambiguation can use them
  wireExplicitImportEdges(allSymbols, graph, fileLookup, addedImports);
  const importReach = buildImportReachability(graph);
  wireSymbolReferences(allSymbols, graph, defIndex, addedRefs, addedImports, importReach);

  return graph;
}

/**
 * Incrementally update the graph: remove stale nodes/edges for changed or
 * deleted files, then re-add from fresh parse results.
 */
function updateGraphFiles(graph, removedFiles, newSymbols) {
  const changedFiles = new Map(newSymbols.map(record => [record.file, record]));

  for (const filePath of removedFiles) {
    const nextRecord = changedFiles.get(filePath);
    if (!nextRecord) {
      removeFileNodes(graph, filePath);
      continue;
    }

    if (!graph.hasNode(filePath)) {
      continue;
    }

    const outgoingEdges = [];
    graph.forEachOutEdge(filePath, edge => {
      outgoingEdges.push(edge);
    });
    for (const edge of outgoingEdges) {
      graph.dropEdge(edge);
    }

    const nextKeys = new Set();
    for (const def of nextRecord.definitions) {
      let key = `${filePath}::${def.name}`;
      if (nextKeys.has(key)) key = `${filePath}::${def.name}@${def.lineStart}`;
      nextKeys.add(key);
    }

    const staleNodes = [];
    graph.forEachNode((node, attrs) => {
      if (attrs.type !== 'symbol' || attrs.file !== filePath) return;
      if (!nextKeys.has(node)) staleNodes.push(node);
    });
    for (const node of staleNodes) {
      graph.dropNode(node);
    }
  }

  mergeDefinitions(graph, newSymbols);

  const defIndex = buildDefinitionIndex(graph);
  const fileLookup = buildFileLookup(graph);
  const addedRefs = new Set();
  const addedImports = new Set();
  wireExplicitImportEdges(newSymbols, graph, fileLookup, addedImports);
  const importReach = buildImportReachability(graph);
  wireSymbolReferences(newSymbols, graph, defIndex, addedRefs, addedImports, importReach);
}

function removeFileNodes(graph, filePath) {
  const toRemove = [];
  graph.forEachNode((node, attrs) => {
    if (node === filePath || attrs.file === filePath) {
      toRemove.push(node);
    }
  });
  for (const n of toRemove) {
    graph.dropNode(n);
  }
}

function buildFocusDistanceMap(graph, focusFiles, maxHops = FOCUS_MAX_HOPS) {
  const distances = new Map();
  const queue = [];

  for (const file of focusFiles) {
    if (!graph.hasNode(file)) continue;
    const attrs = graph.getNodeAttributes(file);
    if (attrs.type !== 'file') continue;
    distances.set(file, 0);
    queue.push(file);
  }

  for (let i = 0; i < queue.length; i++) {
    const current = queue[i];
    const currentDistance = distances.get(current);
    if (currentDistance >= maxHops) continue;

    const visitNeighbor = neighbor => {
      if (!graph.hasNode(neighbor)) return;
      const neighborAttrs = graph.getNodeAttributes(neighbor);
      if (neighborAttrs.type !== 'file') return;

      const nextDistance = currentDistance + 1;
      const existing = distances.get(neighbor);
      if (existing !== undefined && existing <= nextDistance) return;
      distances.set(neighbor, nextDistance);
      queue.push(neighbor);
    };

    graph.forEachOutEdge(current, (_edge, attrs, _source, target) => {
      if (attrs.type !== 'IMPORTS') return;
      visitNeighbor(target);
    });

    graph.forEachInEdge(current, (_edge, attrs, source) => {
      if (attrs.type !== 'IMPORTS') return;
      visitNeighbor(source);
    });
  }

  return distances;
}

function getFocusWeight(filePath, focusDistances) {
  if (!focusDistances || focusDistances.size === 0) return 1.0;
  const distance = focusDistances.get(filePath);
  if (distance === undefined) return FOCUS_DEFAULT_WEIGHT;
  return FOCUS_DISTANCE_WEIGHTS.get(distance) || FOCUS_DEFAULT_WEIGHT;
}

const DEFAULT_PATH_TIERS = [
  { pattern: 'temp_qa/', weight: 0.1 },
  { pattern: 'qa/temp_', weight: 0.1 },
  { pattern: 'qa/', weight: 0.2 },
  { pattern: 'tests/', weight: 0.2 },
  { pattern: 'test/', weight: 0.2 },
  { pattern: 'scripts/', weight: 0.3 },
  { pattern: 'deploy/', weight: 0.3 },
];

function getPathWeight(filePath, pathTiers = DEFAULT_PATH_TIERS) {
  for (const { pattern, weight } of pathTiers) {
    if (filePath.startsWith(pattern) || filePath.includes('/' + pattern)) {
      return weight;
    }
  }
  return 1.0;
}

/**
 * Compute PageRank scores, optionally biased toward focusFiles.
 * Applies path-tier dampening so source code ranks above scripts/tests.
 * Returns an array of [symbolKey, score] sorted descending.
 */
function rankedSymbols(graph, focusFiles = [], pathTiers = DEFAULT_PATH_TIERS) {
  if (graph.order === 0) return [];

  const focusDistances = focusFiles.length > 0 ? buildFocusDistanceMap(graph, focusFiles) : null;

  let g = graph;
  if (focusFiles.length > 0) {
    g = graph.copy();
    g.mergeNode('__focus__', { type: 'virtual' });
    for (const f of focusFiles) {
      if (g.hasNode(f)) {
        g.addEdge('__focus__', f, { weight: 10.0 });
      }
    }
  }

  const scores = pagerank(g, {
    alpha: 0.85,
    maxIterations: 100,
    tolerance: 1e-6,
    getEdgeWeight: 'weight',
  });

  if (focusFiles.length > 0) {
    delete scores['__focus__'];
  }

  return Object.entries(scores)
    .filter(([key]) => {
      try {
        return graph.hasNode(key) && graph.getNodeAttribute(key, 'type') === 'symbol';
      } catch {
        return false;
      }
    })
    .map(([key, score]) => {
      try {
        const file = graph.getNodeAttribute(key, 'file');
        return [key, score * getPathWeight(file, pathTiers) * getFocusWeight(file, focusDistances)];
      } catch {
        return [key, score];
      }
    })
    .sort((a, b) => b[1] - a[1]);
}

/**
 * Serialize graph + mtimes + precomputed unfocused PageRank into a compact
 * integer-indexed, gzip-compressed v3 cache file (~25-30x smaller than raw
 * graphology JSON).
 */
async function saveGraph(graph, mtimeMap, cachePath, { precomputedRanked = null } = {}) {
  await mkdir(dirname(cachePath), { recursive: true });

  const files = [];
  const fileIndex = new Map();

  // Indexed file nodes first
  graph.forEachNode((node, attrs) => {
    if (attrs.type !== 'file') return;
    fileIndex.set(node, files.length);
    files.push([node, mtimeMap.get(node) || 0, attrs.symbolCount || 0]);
  });

  // Also persist mtimes for skipped (e.g. minified) files with symbolCount = -1
  // so warm runs don't re-read and re-check them on every query.
  for (const [path, mtimeMs] of mtimeMap) {
    if (fileIndex.has(path)) continue;
    fileIndex.set(path, files.length);
    files.push([path, mtimeMs, -1]);
  }

  const symbols = [];
  const symIndex = new Map();
  graph.forEachNode((node, attrs) => {
    if (attrs.type !== 'symbol') return;
    const fIdx = fileIndex.get(attrs.file);
    if (fIdx === undefined) return;
    symIndex.set(node, symbols.length);
    symbols.push([
      fIdx,
      attrs.name,
      KIND_TO_CODE[attrs.kind] || 'o',
      attrs.lineStart,
      attrs.lineEnd,
      attrs.signature || '',
    ]);
  });

  const refs = [];
  const imports = [];
  graph.forEachEdge((_edge, attrs, source, target) => {
    if (attrs.type === 'REFERENCES') {
      const fIdx = fileIndex.get(source);
      const sIdx = symIndex.get(target);
      if (fIdx !== undefined && sIdx !== undefined) refs.push(fIdx, sIdx);
    } else if (attrs.type === 'IMPORTS') {
      const srcIdx = fileIndex.get(source);
      const dstIdx = fileIndex.get(target);
      if (srcIdx !== undefined && dstIdx !== undefined) imports.push(srcIdx, dstIdx);
    }
  });

  // Precompute & store unfocused PageRank aligned with symbols[]
  const ranked = precomputedRanked || rankedSymbols(graph);
  const pr = new Array(symbols.length).fill(0);
  for (const [symKey, score] of ranked) {
    const sIdx = symIndex.get(symKey);
    if (sIdx !== undefined) {
      pr[sIdx] = Number(score.toPrecision(6));
    }
  }

  const payload = JSON.stringify({ v: 3, f: files, s: symbols, r: refs, i: imports, pr });
  const compressed = gzipSync(Buffer.from(payload), { level: 1 });
  await writeFile(cachePath, compressed);
}

/**
 * Load compact v3 (gzipped) graph cache from disk.
 * Returns { graph, mtimes, cachedRanked } or null if missing/incompatible.
 */
async function loadGraph(cachePath) {
  try {
    const buf = await readFile(cachePath);
    const jsonStr = (buf.length >= 2 && buf[0] === 0x1f && buf[1] === 0x8b)
      ? gunzipSync(buf).toString('utf-8')
      : buf.toString('utf-8');
    const raw = JSON.parse(jsonStr);

    if (raw.v !== 3) return null;

    const graph = new MultiDirectedGraph({ allowSelfLoops: false });
    const mtimes = new Map();
    const filePaths = new Array(raw.f.length);

    for (let idx = 0; idx < raw.f.length; idx++) {
      const [filePath, mtimeMs, symbolCount] = raw.f[idx];
      filePaths[idx] = filePath;
      mtimes.set(filePath, mtimeMs);
      if (symbolCount >= 0) {
        graph.addNode(filePath, { type: 'file', symbolCount });
      }
    }

    const symKeys = new Array(raw.s.length);
    const usedKeysByFile = new Map();
    for (let idx = 0; idx < raw.s.length; idx++) {
      const [fIdx, name, kindCode, lineStart, lineEnd, signature] = raw.s[idx];
      const file = filePaths[fIdx];
      let used = usedKeysByFile.get(fIdx);
      if (!used) {
        used = new Set();
        usedKeysByFile.set(fIdx, used);
      }
      let key = `${file}::${name}`;
      if (used.has(key)) key = `${file}::${name}@${lineStart}`;
      used.add(key);
      symKeys[idx] = key;

      graph.addNode(key, {
        type: 'symbol',
        kind: CODE_TO_KIND[kindCode] || 'other',
        name,
        file,
        lineStart,
        lineEnd,
        signature,
      });
      graph.addEdge(file, key, { type: 'DEFINES' });
    }

    const r = raw.r || [];
    for (let k = 0; k < r.length; k += 2) {
      const src = filePaths[r[k]];
      const dst = symKeys[r[k + 1]];
      if (src !== undefined && dst !== undefined) {
        graph.addEdge(src, dst, { type: 'REFERENCES' });
      }
    }

    const imp = raw.i || [];
    for (let k = 0; k < imp.length; k += 2) {
      const src = filePaths[imp[k]];
      const dst = filePaths[imp[k + 1]];
      if (src !== undefined && dst !== undefined) {
        graph.addEdge(src, dst, { type: 'IMPORTS' });
      }
    }

    let cachedRanked = null;
    if (Array.isArray(raw.pr) && raw.pr.length === symKeys.length) {
      cachedRanked = symKeys.map((key, idx) => [key, raw.pr[idx] || 0]);
      cachedRanked.sort((a, b) => b[1] - a[1]);
    }

    return { graph, mtimes, cachedRanked };
  } catch {
    return null;
  }
}

export {
  AMBIGUITY_CAP,
  DEFAULT_PATH_TIERS,
  disambiguateTargets,
  getPathWeight,
  buildGraph,
  updateGraphFiles,
  removeFileNodes,
  rankedSymbols,
  saveGraph,
  loadGraph,
};
