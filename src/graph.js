import graphology from 'graphology';
const { MultiDirectedGraph } = graphology;
import pagerankModule from 'graphology-metrics/centrality/pagerank.js';
const pagerank = pagerankModule.default || pagerankModule;
import { writeFile, readFile, mkdir } from 'fs/promises';
import { dirname, posix } from 'path';

/**
 * Build a multi-directed graph from parsed symbol data.
 *
 * Uses MultiDirectedGraph so that DEFINES and REFERENCES edges can coexist
 * between the same (file, symbol) pair — fixing same-file caller tracking.
 * Dedup Sets prevent duplicate REFERENCES/IMPORTS edges from repeated calls
 * to the same symbol within a single file.
 *
 * @param {Array<{file: string, definitions: Array, references: Array}>} allSymbols
 * @returns {MultiDirectedGraph}
 */
// Max definitions for a name before cross-file wiring is skipped entirely.
// Names with more definitions than this (main, run, get, close, etc.) are
// too ambiguous to provide useful structural signal.
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

/**
 * Disambiguate which targets a reference should wire to.
 *
 * 1. If the name is unambiguous (1 definition), wire to it.
 * 2. If a same-file definition exists, wire to it only (skip cross-file).
 * 3. If no same-file match and targets exceed AMBIGUITY_CAP, skip all (too noisy).
 * 4. Otherwise wire to all cross-file targets (low-ambiguity, probably real).
 */
function disambiguateTargets(targets, sourceFile, graph) {
  if (targets.length === 1) return targets;

  // Check for same-file definition
  const sameFile = targets.filter(t => {
    try { return graph.getNodeAttribute(t, 'file') === sourceFile; } catch { return false; }
  });

  if (sameFile.length > 0) return sameFile;

  // No same-file match — apply ambiguity cap
  if (targets.length > AMBIGUITY_CAP) return [];

  return targets;
}

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

function wireSymbolReferences(records, graph, defIndex, addedRefs, addedImports) {
  for (const { file, references } of records) {
    for (const ref of references) {
      const targets = defIndex.get(ref.name);
      if (!targets) continue;

      const filteredTargets = ref.kind === 'property'
        ? targets.filter(target => {
          try { return graph.getNodeAttribute(target, 'kind') === 'property'; } catch { return false; }
        })
        : targets;
      if (filteredTargets.length === 0) continue;

      const resolvedTargets = disambiguateTargets(filteredTargets, file, graph);

      for (const target of resolvedTargets) {
        const targetFile = graph.getNodeAttribute(target, 'file');

        const refKey = `${file}\0${target}`;
        if (!addedRefs.has(refKey)) {
          addedRefs.add(refKey);
          graph.addEdge(file, target, { type: 'REFERENCES' });
        }

        if (targetFile !== file) {
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

function mergeDefinitions(graph, records) {
  for (const { file, definitions } of records) {
    graph.mergeNode(file, { type: 'file', symbolCount: definitions.length });

    for (const def of definitions) {
      const symbolKey = `${file}::${def.name}`;
      graph.mergeNode(symbolKey, {
        type: 'symbol',
        kind: def.kind,
        name: def.name,
        file,
        lineStart: def.lineStart,
        lineEnd: def.lineEnd,
        signature: def.signature,
        astProfile: def.astProfile || null,
        paramNames: def.paramNames || null,
        localRefs: def.localRefs || null,
      });
      graph.addEdge(file, symbolKey, { type: 'DEFINES' });
    }
  }
}

function buildGraph(allSymbols) {
  const graph = new MultiDirectedGraph({ allowSelfLoops: false });
  mergeDefinitions(graph, allSymbols);
  const defIndex = buildDefinitionIndex(graph);

  // Dedup: one REFERENCES edge and one IMPORTS edge per unique (source, target) pair
  const addedRefs = new Set();
  const addedImports = new Set();
  wireSymbolReferences(allSymbols, graph, defIndex, addedRefs, addedImports);
  wireExplicitImportEdges(allSymbols, graph, buildFileLookup(graph), addedImports);

  return graph;
}

/**
 * Incrementally update the graph: remove all nodes for the given files,
 * then re-add from fresh parse results.
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

    const nextNames = new Set(nextRecord.definitions.map(def => def.name));
    const staleNodes = [];
    graph.forEachNode((node, attrs) => {
      if (attrs.type !== 'symbol' || attrs.file !== filePath) return;
      if (!nextNames.has(attrs.name)) staleNodes.push(node);
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
  wireSymbolReferences(newSymbols, graph, defIndex, addedRefs, addedImports);
  wireExplicitImportEdges(newSymbols, graph, fileLookup, addedImports);
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

// Path-tier dampening: files outside core source directories get their
// PageRank scores multiplied by a fraction. This prevents scripts, tests,
// and temp files from dominating the map output over actual source code.
//
// Configurable via .code-index/config.json:
//   { "pathTiers": { "scripts/": 0.3, "tests/": 0.2 } }
const DEFAULT_PATH_TIERS = [
  // Order matters: first match wins. More specific prefixes should come first.
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

  const g = graph.copy();
  const focusDistances = focusFiles.length > 0 ? buildFocusDistanceMap(graph, focusFiles) : null;

  if (focusFiles.length > 0) {
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

  delete scores['__focus__'];

  // Apply path-tier dampening to symbol scores
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
 * Serialize graph + mtime map to disk.
 */
async function saveGraph(graph, mtimeMap, cachePath) {
  await mkdir(dirname(cachePath), { recursive: true });
  const data = {
    version: 2,
    graph: graph.export(),
    mtimes: Object.fromEntries(mtimeMap),
  };
  await writeFile(cachePath, JSON.stringify(data));
}

/**
 * Load graph + mtime map from disk. Returns null if cache doesn't exist.
 */
async function loadGraph(cachePath) {
  try {
    const raw = JSON.parse(await readFile(cachePath, 'utf-8'));
    if (raw.version !== 1 && raw.version !== 2) return null;
    const graph = new MultiDirectedGraph({ allowSelfLoops: false });
    graph.import(raw.graph);
    const mtimes = new Map(Object.entries(raw.mtimes));
    return { graph, mtimes };
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
