import graphology from 'graphology';
const { MultiDirectedGraph } = graphology;
import pagerankModule from 'graphology-metrics/centrality/pagerank.js';
const pagerank = pagerankModule.default || pagerankModule;
import { writeFile, readFile, mkdir } from 'fs/promises';
import { dirname, posix, basename } from 'path';
import { gzipSync, gunzipSync } from 'zlib';
import { getLanguageFamily } from './parser.js';

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
  'then', 'catch', 'finally', 'bind', 'call', 'apply', 'resolve', 'reject',
  'get', 'set', 'has', 'add', 'delete', 'clear', 'size', 'length', 'prototype',
  'log', 'warn', 'error', 'info', 'debug', 'trace',
  'json', 'text', 'blob', 'arrayBuffer', 'clone', 'headers', 'status',
  'all', 'first', 'run', 'exec', 'test', 'sort', 'reverse', 'fill',
  'emit', 'on', 'off', 'once', 'addEventListener', 'removeEventListener',
  'preventDefault', 'stopPropagation', 'stringify', 'parse', 'append', 'close',
  'open', 'start', 'stop', 'init', 'copy', 'setdefault', 'update', 'items',
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

function encodeKind(kind, exported) {
  const code = KIND_TO_CODE[kind] || 'o';
  return exported ? code.toUpperCase() : code;
}

function decodeKind(code) {
  if (!code) return { kind: 'other', exported: true };
  const lower = code.toLowerCase();
  return {
    kind: CODE_TO_KIND[lower] || 'other',
    exported: code !== lower,
  };
}

function normalizeFilePath(filePath) {
  return filePath.replace(/\\/g, '/');
}

function isPythonFile(filePath) {
  return normalizeFilePath(filePath).endsWith('.py');
}

function isJsFamilyFile(filePath) {
  return getLanguageFamily(filePath) === 'js';
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
 * Build import reachability helper with BFS up to `maxHops` along explicit
 * `IMPORTS` edges (captures direct imports + barrel/workspace re-export chains).
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

  const reachCache = new Map();

  function getReachable(file, maxHops = 4) {
    const cacheKey = `${file}\0${maxHops}`;
    if (reachCache.has(cacheKey)) return reachCache.get(cacheKey);

    const direct = directImports.get(file);
    if (!direct || direct.size === 0) {
      reachCache.set(cacheKey, null);
      return null;
    }

    const dist = new Map();
    const queue = [];
    for (const t of direct) {
      dist.set(t, 1);
      queue.push(t);
    }

    for (let i = 0; i < queue.length; i++) {
      const cur = queue[i];
      const d = dist.get(cur);
      if (d >= maxHops) continue;
      const next = directImports.get(cur);
      if (!next) continue;
      for (const n of next) {
        if (n === file || dist.has(n)) continue;
        dist.set(n, d + 1);
        queue.push(n);
      }
    }

    reachCache.set(cacheKey, dist);
    return dist;
  }

  return {
    getDirect(file) {
      return directImports.get(file) || null;
    },
    getReachable,
  };
}

/**
 * Pick targets in the closest import-reachable hop tier (1-hop > 2-hop > 3-hop > 4-hop).
 */
function filterClosestReachable(targets, sourceFile, graph, importReach, maxHops = 4) {
  if (!importReach) return [];
  const distMap = importReach.getReachable(sourceFile, maxHops);
  if (!distMap) return [];

  let bestHop = Infinity;
  const matched = [];
  for (const t of targets) {
    let tf;
    try { tf = graph.getNodeAttribute(t, 'file'); } catch { continue; }
    const hop = distMap.get(tf);
    if (hop === undefined) continue;
    if (hop < bestHop) {
      bestHop = hop;
      matched.length = 0;
      matched.push(t);
    } else if (hop === bestHop) {
      matched.push(t);
    }
  }
  return matched;
}

/**
 * Disambiguate targets for bare symbol references (kept exported for unit tests).
 */
function disambiguateTargets(targets, sourceFile, graph, importReach = null) {
  if (targets.length === 0) return [];

  const sameFile = targets.filter(t => {
    try { return graph.getNodeAttribute(t, 'file') === sourceFile; } catch { return false; }
  });
  if (sameFile.length > 0) return sameFile;

  const reachable = filterClosestReachable(targets, sourceFile, graph, importReach, 4);
  if (reachable.length > 0) return reachable;

  if (targets.length === 1) return targets;
  if (targets.length > AMBIGUITY_CAP) return [];
  return targets;
}

function resolveExportTarget(entry) {
  if (!entry) return null;
  if (typeof entry === 'string') return entry;
  if (typeof entry === 'object') {
    return resolveExportTarget(entry.import || entry.default || entry.types || entry.require);
  }
  return null;
}

function resolveWorkspaceImportBases(specifier, workspacePackages) {
  if (!workspacePackages || workspacePackages.length === 0) return [];
  const bases = [];

  for (const pkg of workspacePackages) {
    if (specifier === pkg.name) {
      const expRoot = pkg.exports ? resolveExportTarget(pkg.exports['.']) : null;
      if (expRoot) bases.push(posix.normalize(posix.join(pkg.dir, expRoot)));
      if (pkg.main) bases.push(posix.normalize(posix.join(pkg.dir, pkg.main)));
      bases.push(posix.normalize(posix.join(pkg.dir, 'src/index')));
      bases.push(posix.normalize(posix.join(pkg.dir, 'index')));
      return bases;
    }

    if (specifier.startsWith(pkg.name + '/')) {
      const subpath = specifier.slice(pkg.name.length + 1);
      const dotSub = `./${subpath}`;

      if (pkg.exports) {
        if (pkg.exports[dotSub]) {
          const t = resolveExportTarget(pkg.exports[dotSub]);
          if (t) bases.push(posix.normalize(posix.join(pkg.dir, t)));
        }
        // Handle wildcard exports like `"./*": "./src/*.ts"`
        for (const [pattern, targetSpec] of Object.entries(pkg.exports)) {
          if (!pattern.includes('*')) continue;
          const [pre, post] = pattern.split('*');
          if (dotSub.startsWith(pre) && dotSub.endsWith(post)) {
            const matched = dotSub.slice(pre.length, dotSub.length - post.length);
            const rawTarget = resolveExportTarget(targetSpec);
            if (rawTarget && rawTarget.includes('*')) {
              const expanded = rawTarget.replace('*', matched);
              bases.push(posix.normalize(posix.join(pkg.dir, expanded)));
            }
          }
        }
      }

      bases.push(posix.normalize(posix.join(pkg.dir, 'src', subpath)));
      bases.push(posix.normalize(posix.join(pkg.dir, subpath)));
      return bases;
    }
  }

  return bases;
}

function resolvePythonImportBases(sourceFile, specifier, pythonRoots = ['']) {
  if (!isPythonFile(sourceFile) || !specifier) return [];

  if (specifier.startsWith('.')) {
    const match = specifier.match(/^(\.+)(.*)$/);
    if (!match) return [];

    const dots = match[1].length;
    let baseDir = posix.dirname(sourceFile);
    for (let i = 1; i < dots; i++) {
      baseDir = posix.dirname(baseDir);
    }

    const remainder = match[2].replace(/^\./, '');
    if (!remainder) return [baseDir];

    return [posix.normalize(posix.join(baseDir, remainder.replace(/\./g, '/')))];
  }

  if (/^[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z_][A-Za-z0-9_]*)*$/.test(specifier)) {
    const modPath = specifier.replace(/\./g, '/');
    const bases = [modPath];
    for (const root of pythonRoots) {
      if (root) {
        bases.push(posix.normalize(posix.join(root, modPath)));
        bases.push(posix.normalize(posix.join(root, 'src', modPath)));
      } else {
        bases.push(posix.normalize(posix.join('src', modPath)));
      }
    }
    return bases;
  }

  return [];
}

function resolveImportBases(sourceFile, specifier, resolverCtx = {}) {
  const normalizedSource = normalizeFilePath(sourceFile);
  const normalizedSpecifier = normalizeFilePath(specifier);
  if (!normalizedSpecifier || normalizedSpecifier.startsWith('node:')) return [];

  // Python imports use leading dots without slashes (`.helpers`, `..lib.tools`),
  // so handle Python before JS/TS relative `./` paths.
  if (isPythonFile(normalizedSource)) {
    return resolvePythonImportBases(normalizedSource, normalizedSpecifier, resolverCtx.pythonRoots);
  }

  if (normalizedSpecifier.startsWith('.')) {
    return [posix.normalize(posix.join(posix.dirname(normalizedSource), normalizedSpecifier))];
  }

  if (normalizedSpecifier.startsWith('~/') || normalizedSpecifier.startsWith('@/')) {
    const tail = normalizedSpecifier.slice(2);
    const bases = [];
    // If inside a monorepo app (e.g. `apps/qa/src/...`), try that app's `src/` first!
    const srcIdx = normalizedSource.indexOf('/src/');
    if (srcIdx !== -1) {
      const appSrc = normalizedSource.slice(0, srcIdx + 4);
      bases.push(posix.normalize(posix.join(appSrc, tail)));
    }
    bases.push(posix.normalize(posix.join('src', tail)));
    return bases;
  }

  if (normalizedSpecifier.startsWith('/')) {
    return [posix.normalize(normalizedSpecifier.slice(1))];
  }

  if (normalizedSpecifier.startsWith('src/')) {
    const bases = [posix.normalize(normalizedSpecifier)];
    const srcIdx = normalizedSource.indexOf('/src/');
    if (srcIdx !== -1) {
      const appRoot = normalizedSource.slice(0, srcIdx);
      bases.unshift(posix.normalize(posix.join(appRoot, normalizedSpecifier)));
    }
    return bases;
  }

  // Workspace package imports (`@scope/pkg`, `@scope/pkg/subpath`)
  const wsBases = resolveWorkspaceImportBases(normalizedSpecifier, resolverCtx.workspacePackages);
  if (wsBases.length > 0) return wsBases;

  // Python module imports (relative or anchored at any discovered pythonRoot)
  const pyBases = resolvePythonImportBases(normalizedSource, normalizedSpecifier, resolverCtx.pythonRoots);
  if (pyBases.length > 0) return pyBases;

  return [];
}

function hasExplicitImportExtension(filePath) {
  return IMPORT_RESOLVE_EXTENSIONS.some(ext => filePath.endsWith(ext));
}

function resolveImportTargetFile(sourceFile, specifier, fileLookup, resolverCtx = {}) {
  const importBases = resolveImportBases(sourceFile, specifier, resolverCtx);
  if (importBases.length === 0) return null;

  const isPy = isPythonFile(sourceFile);
  const packageEntryNames = isPy ? ['__init__'] : ['index'];

  for (const importBase of importBases) {
    const candidates = [importBase];
    if (!hasExplicitImportExtension(importBase)) {
      for (const ext of IMPORT_RESOLVE_EXTENSIONS) {
        candidates.push(`${importBase}${ext}`);
      }
      for (const ext of IMPORT_RESOLVE_EXTENSIONS) {
        for (const entryName of packageEntryNames) {
          candidates.push(posix.join(importBase, `${entryName}${ext}`));
        }
      }
    } else if (importBase.endsWith('.js') || importBase.endsWith('.mjs')) {
      // TS ESM imports often write `import './foo.js'` pointing to `foo.ts` / `foo.tsx`
      const stem = importBase.replace(/\.(m?js)$/, '');
      candidates.push(`${stem}.ts`, `${stem}.tsx`);
    }

    for (const candidate of candidates) {
      const resolved = fileLookup.get(normalizeFilePath(candidate));
      if (resolved) return resolved;
    }
  }

  return null;
}

function wireExplicitImportEdges(records, graph, fileLookup, addedImports, resolverCtx = {}) {
  for (const { file, imports = [] } of records) {
    for (const entry of imports) {
      const targetFile = resolveImportTargetFile(file, entry.path, fileLookup, resolverCtx);
      if (!targetFile || targetFile === file) continue;

      const impKey = `${file}\0${targetFile}`;
      if (addedImports.has(impKey)) continue;

      addedImports.add(impKey);
      graph.addEdge(file, targetFile, { type: 'IMPORTS' });
    }
  }
}

/**
 * Match a receiver name (e.g. `review` from `api.review.getItem.useQuery()`)
 * against target files (`routers/review.ts`, `routers/reviewRouter.ts`).
 */
function matchPropertyTargetsByReceiver(propTargets, receiver, graph) {
  if (!receiver) return [];
  const recvNorm = receiver.toLowerCase().replace(/[^a-z0-9]/g, '');
  if (recvNorm.length < 2) return [];

  const matches = [];
  for (const t of propTargets) {
    let tf;
    try { tf = graph.getNodeAttribute(t, 'file'); } catch { continue; }
    const stem = basename(tf).replace(/\.[^.]+$/, '').toLowerCase().replace(/[^a-z0-9]/g, '');
    if (stem === recvNorm || stem === `${recvNorm}router` || stem === `${recvNorm}procedures`) {
      matches.push(t);
    }
  }
  return matches;
}

/**
 * Wire symbol `REFERENCES` edges using strict import-scoped disambiguation.
 *
 * IMPORTANT: Never synthesizes `IMPORTS` edges here — `IMPORTS` edges come
 * strictly from explicit AST import statements so `deps`, `dependents`, and
 * `neighborhood` have zero bare-name false positives.
 */
function wireSymbolReferences(records, graph, defIndex, addedRefs, importReach, fileLookup, resolverCtx = {}) {
  for (const { file, references } of records) {
    const srcFamily = getLanguageFamily(file);
    const isPy = srcFamily === 'python';
    const isJs = srcFamily === 'js';

    for (const ref of references) {
      const rawTargets = defIndex.get(ref.name);
      if (!rawTargets) continue;

      // Enforce language-family matching and exported-only cross-file wiring
      const targets = [];
      for (const t of rawTargets) {
        let attrs;
        try { attrs = graph.getNodeAttributes(t); } catch { continue; }
        if (attrs.file === file) {
          targets.push(t);
          continue;
        }
        if (getLanguageFamily(attrs.file) !== srcFamily) continue;
        if (attrs.exported === false) continue;
        targets.push(t);
      }
      if (targets.length === 0) continue;

      let resolvedTargets = [];

      if (ref.kind === 'method') {
        if (METHOD_NOISE_NAMES.has(ref.name) || ref.name.length < 3) continue;

        const eligible = targets.filter(t => {
          const k = graph.getNodeAttribute(t, 'kind');
          if (k === 'method') return true;
          // Python `mod.fn()` calls module-level functions
          if (isPy && (k === 'function' || k === 'class')) return true;
          return false;
        });
        if (eligible.length === 0) continue;

        // If `ref.receiverModule` resolves directly (e.g. `pool.fn()` or `lib.fn()`),
        // match targets in that module (or its 1-hop re-exports) first.
        if (ref.receiverModule) {
          const modFile = resolveImportTargetFile(file, ref.receiverModule, fileLookup, resolverCtx);
          if (modFile) {
            const directMod = eligible.filter(t => graph.getNodeAttribute(t, 'file') === modFile);
            if (directMod.length > 0) {
              resolvedTargets = directMod;
            } else {
              const modReach = importReach.getReachable(modFile, 2);
              if (modReach) {
                resolvedTargets = eligible.filter(t => modReach.has(graph.getNodeAttribute(t, 'file')));
              }
            }
          }
        }

        if (resolvedTargets.length === 0) {
          const sameFile = eligible.filter(t => graph.getNodeAttribute(t, 'file') === file);
          if (sameFile.length > 0) {
            resolvedTargets = sameFile;
          } else {
            resolvedTargets = filterClosestReachable(eligible, file, graph, importReach, 4);
          }
        }
      } else if (ref.kind === 'property') {
        // Python `mod.CONST` attribute access on an imported module alias
        if (isPy && ref.receiverModule) {
          const modFile = resolveImportTargetFile(file, ref.receiverModule, fileLookup, resolverCtx);
          if (modFile) {
            const directMod = targets.filter(t => graph.getNodeAttribute(t, 'file') === modFile);
            if (directMod.length > 0) {
              resolvedTargets = directMod;
            } else {
              const modReach = importReach.getReachable(modFile, 2);
              if (modReach) {
                resolvedTargets = targets.filter(t => modReach.has(graph.getNodeAttribute(t, 'file')));
              }
            }
          }
        }

        if (resolvedTargets.length === 0) {
          const propTargets = targets.filter(t => graph.getNodeAttribute(t, 'kind') === 'property');
          if (propTargets.length === 0) continue;

          const sameFile = propTargets.filter(t => graph.getNodeAttribute(t, 'file') === file);
          if (sameFile.length > 0) {
            resolvedTargets = sameFile;
          } else {
            // Receiver-aware matching for RPC proxies like `api.review.getItem.useQuery()`
            const recvMatches = matchPropertyTargetsByReceiver(propTargets, ref.receiver, graph);
            if (recvMatches.length > 0) {
              resolvedTargets = recvMatches;
            } else {
              const reachable = filterClosestReachable(propTargets, file, graph, importReach, 4);
              if (reachable.length > 0) {
                resolvedTargets = reachable;
              } else if (propTargets.length <= AMBIGUITY_CAP) {
                resolvedTargets = propTargets;
              }
            }
          }
        }
      } else {
        // Lane C: bare symbol refs (`foo()`, `<Comp />`, `Type`, `import { FOO }`)
        const nonMethodTargets = targets.filter(t => graph.getNodeAttribute(t, 'kind') !== 'method');
        if (nonMethodTargets.length === 0) continue;

        const sameFile = nonMethodTargets.filter(t => graph.getNodeAttribute(t, 'file') === file);
        if (sameFile.length > 0) {
          resolvedTargets = sameFile;
        } else {
          // Require explicit import-chain reachability (up to 4 barrel hops) for JS/TS/Python
          const reachable = filterClosestReachable(nonMethodTargets, file, graph, importReach, 4);
          if (reachable.length > 0) {
            resolvedTargets = reachable;
          } else if (!isJs && !isPy) {
            // Non-JS/Python (Go/Java/C#/C/C++): allow same-directory package fallback
            const srcDir = posix.dirname(file);
            const sameDir = nonMethodTargets.filter(t => posix.dirname(graph.getNodeAttribute(t, 'file')) === srcDir);
            if (sameDir.length > 0) {
              resolvedTargets = sameDir;
            } else if (nonMethodTargets.length <= AMBIGUITY_CAP) {
              resolvedTargets = nonMethodTargets;
            }
          }
        }
      }

      if (resolvedTargets.length === 0) continue;

      for (const target of resolvedTargets) {
        const refKey = `${file}\0${target}`;
        if (!addedRefs.has(refKey)) {
          addedRefs.add(refKey);
          graph.addEdge(file, target, { type: 'REFERENCES' });
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
        exported: def.exported !== false,
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

function buildGraph(allSymbols, resolverCtx = {}) {
  const graph = new MultiDirectedGraph({ allowSelfLoops: false });
  mergeDefinitions(graph, allSymbols);
  const defIndex = buildDefinitionIndex(graph);
  const fileLookup = buildFileLookup(graph);

  const addedRefs = new Set();
  const addedImports = new Set();
  // Pass 1: explicit AST imports only
  wireExplicitImportEdges(allSymbols, graph, fileLookup, addedImports, resolverCtx);
  // Pass 2: import-scoped symbol & method references (no synthetic IMPORTS)
  const importReach = buildImportReachability(graph);
  wireSymbolReferences(allSymbols, graph, defIndex, addedRefs, importReach, fileLookup, resolverCtx);

  return graph;
}

function updateGraphFiles(graph, removedFiles, newSymbols, resolverCtx = {}) {
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
  wireExplicitImportEdges(newSymbols, graph, fileLookup, addedImports, resolverCtx);
  const importReach = buildImportReachability(graph);
  wireSymbolReferences(newSymbols, graph, defIndex, addedRefs, importReach, fileLookup, resolverCtx);
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
  { pattern: '__tests__/', weight: 0.2 },
  { pattern: 'scripts/', weight: 0.3 },
  { pattern: 'deploy/', weight: 0.3 },
];

const TEST_FILE_SUFFIX_RE = /(\.(test|spec)|(_test|_spec))\.[^.]+$/i;
const TEST_FILE_PREFIX_RE = /^test_/;

function getPathWeight(filePath, pathTiers = DEFAULT_PATH_TIERS) {
  const base = basename(filePath);
  if (TEST_FILE_SUFFIX_RE.test(base) || TEST_FILE_PREFIX_RE.test(base)) {
    return 0.15;
  }
  for (const { pattern, weight } of pathTiers) {
    if (filePath.startsWith(pattern) || filePath.includes('/' + pattern)) {
      return weight;
    }
  }
  return 1.0;
}

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
 * integer-indexed, gzip-compressed v4 cache file.
 * Exported flag is encoded in kindCode case (uppercase = exported, lowercase = local).
 */
async function saveGraph(graph, mtimeMap, cachePath, { precomputedRanked = null } = {}) {
  await mkdir(dirname(cachePath), { recursive: true });

  const files = [];
  const fileIndex = new Map();

  graph.forEachNode((node, attrs) => {
    if (attrs.type !== 'file') return;
    fileIndex.set(node, files.length);
    files.push([node, mtimeMap.get(node) || 0, attrs.symbolCount || 0]);
  });

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
      encodeKind(attrs.kind, attrs.exported !== false),
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

  const ranked = precomputedRanked || rankedSymbols(graph);
  const pr = new Array(symbols.length).fill(0);
  for (const [symKey, score] of ranked) {
    const sIdx = symIndex.get(symKey);
    if (sIdx !== undefined) {
      pr[sIdx] = Number(score.toPrecision(6));
    }
  }

  const payload = JSON.stringify({ v: 4, f: files, s: symbols, r: refs, i: imports, pr });
  const compressed = gzipSync(Buffer.from(payload), { level: 1 });
  await writeFile(cachePath, compressed);
}

/**
 * Load compact v4 (gzipped) graph cache from disk.
 */
async function loadGraph(cachePath) {
  try {
    const buf = await readFile(cachePath);
    const jsonStr = (buf.length >= 2 && buf[0] === 0x1f && buf[1] === 0x8b)
      ? gunzipSync(buf).toString('utf-8')
      : buf.toString('utf-8');
    const raw = JSON.parse(jsonStr);

    if (raw.v !== 4) return null;

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

      const { kind, exported } = decodeKind(kindCode);
      graph.addNode(key, {
        type: 'symbol',
        kind,
        exported,
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
  buildImportReachability,
};
