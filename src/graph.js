import graphology from 'graphology';
const { MultiDirectedGraph } = graphology;
import pagerankModule from 'graphology-metrics/centrality/pagerank.js';
const pagerank = pagerankModule.default || pagerankModule;
import { writeFile, readFile, mkdir } from 'fs/promises';
import { dirname } from 'path';

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

function buildGraph(allSymbols) {
  const graph = new MultiDirectedGraph({ allowSelfLoops: false });

  for (const { file, definitions } of allSymbols) {
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
      });
      graph.addEdge(file, symbolKey, { type: 'DEFINES' });
    }
  }

  // Build a name→symbolKey index for wiring references
  const defIndex = new Map();
  for (const { file, definitions } of allSymbols) {
    for (const def of definitions) {
      const key = `${file}::${def.name}`;
      if (!defIndex.has(def.name)) defIndex.set(def.name, []);
      defIndex.get(def.name).push(key);
    }
  }

  // Dedup: one REFERENCES edge and one IMPORTS edge per unique (source, target) pair
  const addedRefs = new Set();
  const addedImports = new Set();

  for (const { file, references } of allSymbols) {
    for (const ref of references) {
      const targets = defIndex.get(ref.name);
      if (!targets) continue;

      // Disambiguate: resolve which targets to actually wire
      const resolvedTargets = disambiguateTargets(targets, file, graph);

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

  return graph;
}

/**
 * Incrementally update the graph: remove all nodes for the given files,
 * then re-add from fresh parse results.
 */
function updateGraphFiles(graph, removedFiles, newSymbols) {
  for (const filePath of removedFiles) {
    removeFileNodes(graph, filePath);
  }

  // Re-add from newSymbols using the same logic as buildGraph,
  // but operating on the existing graph.
  const defIndex = new Map();

  // Rebuild defIndex from the entire graph (existing + new)
  graph.forEachNode((node, attrs) => {
    if (attrs.type === 'symbol') {
      if (!defIndex.has(attrs.name)) defIndex.set(attrs.name, []);
      defIndex.get(attrs.name).push(node);
    }
  });

  const addedRefs = new Set();
  const addedImports = new Set();

  for (const { file, definitions, references } of newSymbols) {
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
      });
      graph.addEdge(file, symbolKey, { type: 'DEFINES' });

      if (!defIndex.has(def.name)) defIndex.set(def.name, []);
      defIndex.get(def.name).push(symbolKey);
    }

    for (const ref of references) {
      const targets = defIndex.get(ref.name);
      if (!targets) continue;

      const resolvedTargets = disambiguateTargets(targets, file, graph);

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
        return [key, score * getPathWeight(file, pathTiers)];
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
