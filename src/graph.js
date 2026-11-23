import graphology from 'graphology';
const { MultiDirectedGraph } = graphology;
import pagerankModule from 'graphology-metrics/centrality/pagerank.js';
const pagerank = pagerankModule.default || pagerankModule;
import { writeFile, readFile, mkdir, rename, unlink } from 'fs/promises';
import { dirname, posix, basename } from 'path';
import { gzipSync, gunzipSync } from 'zlib';
import { getLanguageFamily, METHOD_NOISE_NAMES } from './parser.js';

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

const BARREL_BASENAME_RE = /^(index|__init__)\.[^.]+$/i;
const PARSE_CACHE_VERSION = 1;

function isBarrelFile(filePath) {
  return BARREL_BASENAME_RE.test(basename(filePath));
}

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
    let bucket = defIndex.get(attrs.name);
    if (!bucket) {
      bucket = { items: [], hasProperty: false, hasMethodOrPyCallable: false };
      defIndex.set(attrs.name, bucket);
    }
    const family = getLanguageFamily(attrs.file);
    const kind = attrs.kind;
    if (kind === 'property') bucket.hasProperty = true;
    if (kind === 'method' || (family === 'python' && (kind === 'function' || kind === 'class'))) {
      bucket.hasMethodOrPyCallable = true;
    }
    bucket.items.push({
      key: node,
      file: attrs.file,
      kind,
      exported: attrs.exported !== false,
      family,
    });
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
 * Build import reachability helper with BFS up to `maxHops`.
 *
 * Hop 1 walks all direct `IMPORTS` edges from `file`.
 * Hops 2..maxHops ONLY traverse through barrel / package entry files
 * (`index.*`, `__init__.py`) or explicit re-export edges (`export ... from ...`),
 * preventing unrelated transitive runtime imports (e.g. `ui -> trpc -> root -> pkg`)
 * from falsely treating downstream symbols as imported by the UI file.
 */
function buildImportReachability(graph) {
  const directImports = new Map();
  const reexportImports = new Map();

  graph.forEachNode((node, attrs) => {
    if (attrs.type !== 'file') return;
    const targets = new Set();
    const reexports = new Set();
    const barrel = isBarrelFile(node);
    graph.forEachOutEdge(node, (_edge, eAttrs, _src, target) => {
      if (eAttrs.type !== 'IMPORTS') return;
      targets.add(target);
      if (barrel || eAttrs.reexport) {
        reexports.add(target);
      }
    });
    if (targets.size > 0) directImports.set(node, targets);
    if (reexports.size > 0) reexportImports.set(node, reexports);
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
      // Only expand beyond hop 1 through barrel files or explicit re-exports
      const next = reexportImports.get(cur);
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
 * Supports both raw node keys (strings) and pre-enriched `{ key, file }` descriptors.
 */
function filterClosestReachable(targets, sourceFile, graph, importReach, maxHops = 4) {
  if (!importReach) return [];
  const distMap = importReach.getReachable(sourceFile, maxHops);
  if (!distMap) return [];

  let bestHop = Infinity;
  const matched = [];
  for (const t of targets) {
    let tf;
    if (typeof t === 'string') {
      try { tf = graph.getNodeAttribute(t, 'file'); } catch { continue; }
    } else {
      tf = t.file;
    }
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

function resolveTsConfigPathBases(sourceFile, specifier, tsPathMappings) {
  if (!tsPathMappings || tsPathMappings.length === 0) return [];
  const bases = [];

  // Prefer the deepest tsconfig directory containing sourceFile
  const sorted = [...tsPathMappings].sort((a, b) => b.dir.length - a.dir.length);
  for (const cfg of sorted) {
    if (cfg.dir && !sourceFile.startsWith(cfg.dir + '/')) continue;
    const baseDir = cfg.baseUrl && cfg.baseUrl !== '.'
      ? posix.normalize(posix.join(cfg.dir || '', cfg.baseUrl))
      : (cfg.dir || '');

    for (const [pattern, targets] of Object.entries(cfg.paths)) {
      if (!Array.isArray(targets)) continue;
      if (pattern.includes('*')) {
        const [pre, post] = pattern.split('*');
        if (specifier.startsWith(pre) && specifier.endsWith(post)) {
          const matched = specifier.slice(pre.length, specifier.length - post.length);
          for (const raw of targets) {
            if (typeof raw !== 'string') continue;
            const expanded = raw.includes('*') ? raw.replace('*', matched) : raw;
            const joined = posix.normalize(posix.join(baseDir, expanded));
            if (!joined.startsWith('..')) bases.push(joined);
          }
        }
      } else if (specifier === pattern) {
        for (const raw of targets) {
          if (typeof raw !== 'string') continue;
          const joined = posix.normalize(posix.join(baseDir, raw));
          if (!joined.startsWith('..')) bases.push(joined);
        }
      }
    }
    if (bases.length > 0) return bases;
  }

  return bases;
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

  // Workspace package imports (`@scope/pkg`, `@scope/pkg/subpath`) — check before
  // generic `@/` or tsconfig paths so multi-root workspace packages resolve cleanly.
  const wsBases = resolveWorkspaceImportBases(normalizedSpecifier, resolverCtx.workspacePackages);
  if (wsBases.length > 0) return wsBases;

  // Custom tsconfig.json `compilerOptions.paths` mappings
  const tsBases = resolveTsConfigPathBases(normalizedSource, normalizedSpecifier, resolverCtx.tsPathMappings);
  if (tsBases.length > 0) return tsBases;

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

  // Python module imports (relative or anchored at any discovered pythonRoot)
  const pyBases = resolvePythonImportBases(normalizedSource, normalizedSpecifier, resolverCtx.pythonRoots);
  if (pyBases.length > 0) return pyBases;

  return [];
}

const JS_RESOLVE_EXTENSIONS = ['.ts', '.tsx', '.js', '.jsx', '.mjs', '.cjs'];
const PY_RESOLVE_EXTENSIONS = ['.py'];

function hasExplicitImportExtension(filePath) {
  return IMPORT_RESOLVE_EXTENSIONS.some(ext => filePath.endsWith(ext));
}

function resolveImportTargetFile(sourceFile, specifier, fileLookup, resolverCtx = {}) {
  const importBases = resolveImportBases(sourceFile, specifier, resolverCtx);
  if (importBases.length === 0) return null;

  const isPy = isPythonFile(sourceFile);
  const isJs = !isPy && isJsFamilyFile(sourceFile);
  const exts = isPy ? PY_RESOLVE_EXTENSIONS : (isJs ? JS_RESOLVE_EXTENSIONS : IMPORT_RESOLVE_EXTENSIONS);
  const entryName = isPy ? '__init__' : 'index';

  for (const importBase of importBases) {
    const direct = fileLookup.get(importBase);
    if (direct) return direct;

    if (!hasExplicitImportExtension(importBase)) {
      for (let i = 0; i < exts.length; i++) {
        const resolved = fileLookup.get(`${importBase}${exts[i]}`);
        if (resolved) return resolved;
      }
      for (let i = 0; i < exts.length; i++) {
        const resolved = fileLookup.get(`${importBase}/${entryName}${exts[i]}`);
        if (resolved) return resolved;
      }
    } else if (isJs && (importBase.endsWith('.js') || importBase.endsWith('.mjs'))) {
      // TS ESM imports often write `import './foo.js'` pointing to `foo.ts` / `foo.tsx`
      const stem = importBase.replace(/\.(m?js)$/, '');
      const tsHit = fileLookup.get(`${stem}.ts`) || fileLookup.get(`${stem}.tsx`);
      if (tsHit) return tsHit;
    }
  }

  return null;
}

function isSourceIndependentSpecifier(specifier, isPy) {
  if (!specifier) return false;
  if (specifier.startsWith('.')) return false;
  if (isPy) return true;
  if (specifier.startsWith('~/') || specifier.startsWith('@/') || specifier.startsWith('/') || specifier.startsWith('src/')) {
    return false;
  }
  return true;
}

function wireExplicitImportEdges(records, graph, fileLookup, addedImports, resolverCtx = {}) {
  const specCache = new Map();
  const hasTsPaths = !!(resolverCtx.tsPathMappings && resolverCtx.tsPathMappings.length > 0);

  for (const { file, imports = [] } of records) {
    const isPy = isPythonFile(file);
    const cachePrefix = isPy ? 'py\0' : (!hasTsPaths && isJsFamilyFile(file) ? 'js\0' : null);

    for (const entry of imports) {
      let targetFile;
      if (cachePrefix && isSourceIndependentSpecifier(entry.path, isPy)) {
        const ck = cachePrefix + entry.path;
        if (specCache.has(ck)) {
          targetFile = specCache.get(ck);
        } else {
          targetFile = resolveImportTargetFile(file, entry.path, fileLookup, resolverCtx);
          specCache.set(ck, targetFile);
        }
      } else {
        targetFile = resolveImportTargetFile(file, entry.path, fileLookup, resolverCtx);
      }

      if (!targetFile || targetFile === file) continue;

      const impKey = `${file}\0${targetFile}`;
      if (addedImports.has(impKey)) {
        if (entry.isReexport) {
          const edgeKey = addedImports.get(impKey);
          if (edgeKey && graph.hasEdge(edgeKey)) {
            graph.setEdgeAttribute(edgeKey, 'reexport', true);
          }
        }
        continue;
      }

      const edgeKey = graph.addEdge(file, targetFile, {
        type: 'IMPORTS',
        reexport: !!entry.isReexport,
      });
      addedImports.set(impKey, edgeKey);
    }
  }
}

/**
 * Match a receiver name (e.g. `review` from `api.review.getItem.useQuery()`)
 * against target files (`routers/review.ts`, `routers/reviewRouter.ts`).
 */
function matchPropertyTargetsByReceiver(propTargets, receiver) {
  if (!receiver) return [];
  const recvNorm = receiver.toLowerCase().replace(/[^a-z0-9]/g, '');
  if (recvNorm.length < 2) return [];

  const matches = [];
  for (const t of propTargets) {
    const stem = basename(t.file).replace(/\.[^.]+$/, '').toLowerCase().replace(/[^a-z0-9]/g, '');
    if (
      stem === recvNorm ||
      stem === `${recvNorm}router` ||
      stem === `${recvNorm}procedures` ||
      `${stem}s` === recvNorm ||
      `${recvNorm}s` === stem
    ) {
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
  const modResolveCache = new Map();
  const resolveMod = (file, spec) => {
    const k = `${file}\0${spec}`;
    if (modResolveCache.has(k)) return modResolveCache.get(k);
    const res = resolveImportTargetFile(file, spec, fileLookup, resolverCtx);
    modResolveCache.set(k, res);
    return res;
  };

  for (const { file, references } of records) {
    const srcFamily = getLanguageFamily(file);
    const isPy = srcFamily === 'python';
    const isJs = srcFamily === 'js';

    for (const ref of references) {
      if (ref.kind === 'method' && (METHOD_NOISE_NAMES.has(ref.name) || ref.name.length < 3)) {
        continue;
      }

      const bucket = defIndex.get(ref.name);
      if (!bucket) continue;

      // Fast O(1) skips before scanning target lists
      if (ref.kind === 'property' && !ref.receiverModule && !bucket.hasProperty) continue;
      if (ref.kind === 'method' && !bucket.hasMethodOrPyCallable) continue;

      const rawTargets = bucket.items;

      // Enforce language-family matching and exported-only cross-file wiring
      const targets = [];
      for (const t of rawTargets) {
        if (t.file === file) {
          targets.push(t);
          continue;
        }
        if (t.family !== srcFamily) continue;
        if (!t.exported) continue;
        targets.push(t);
      }
      if (targets.length === 0) continue;

      let resolvedTargets = [];

      if (ref.kind === 'method') {
        const eligible = targets.filter(t => {
          if (t.kind === 'method') return true;
          // Python `mod.fn()` calls module-level functions
          if (isPy && (t.kind === 'function' || t.kind === 'class')) return true;
          return false;
        });
        if (eligible.length === 0) continue;

        // If `ref.receiverModule` resolves directly (e.g. `pool.fn()` or `lib.fn()`),
        // match targets in that module (or its barrel re-exports) first.
        let receiverModuleMatchedFile = false;
        if (ref.receiverModule) {
          const modFile = resolveMod(file, ref.receiverModule);
          if (modFile) {
            receiverModuleMatchedFile = true;
            const directMod = eligible.filter(t => t.file === modFile);
            if (directMod.length > 0) {
              resolvedTargets = directMod;
            } else {
              const modReach = importReach.getReachable(modFile, 2);
              if (modReach) {
                resolvedTargets = eligible.filter(t => modReach.has(t.file));
              }
            }
          }
        }

        if (resolvedTargets.length === 0 && !receiverModuleMatchedFile) {
          const sameFile = eligible.filter(t => t.file === file);
          if (sameFile.length > 0) {
            resolvedTargets = sameFile;
          } else {
            resolvedTargets = filterClosestReachable(eligible, file, graph, importReach, 4);
          }
        }
      } else if (ref.kind === 'property') {
        // Direct module-scoped receiver:
        // - Python `mod.CONST` attribute access on an imported module alias
        // - JS/TS `caller.getEpisode()` bound via `createCallerFactory(router)` or `import * as ns`
        let receiverModuleMatchedFile = false;
        if (ref.receiverModule) {
          const modFile = resolveMod(file, ref.receiverModule);
          if (modFile) {
            receiverModuleMatchedFile = true;
            const modPool = isPy
              ? targets
              : targets.filter(t => (
                t.kind === 'property' || t.kind === 'function' || t.kind === 'variable' || t.kind === 'class'
              ));
            const directMod = modPool.filter(t => t.file === modFile);
            if (directMod.length > 0) {
              resolvedTargets = directMod;
            } else {
              const modReach = importReach.getReachable(modFile, 2);
              if (modReach) {
                resolvedTargets = modPool.filter(t => modReach.has(t.file));
              }
            }
          }
        }

        if (resolvedTargets.length === 0 && !receiverModuleMatchedFile) {
          const propTargets = targets.filter(t => t.kind === 'property');
          if (propTargets.length === 0) continue;

          const sameFile = propTargets.filter(t => t.file === file);
          if (sameFile.length > 0) {
            resolvedTargets = sameFile;
          } else {
            // Receiver-aware matching for RPC proxies like `api.review.getItem.useQuery()`
            const recvMatches = matchPropertyTargetsByReceiver(propTargets, ref.receiver);
            if (recvMatches.length > 0) {
              resolvedTargets = recvMatches;
            } else if (ref.isRpcChain || !METHOD_NOISE_NAMES.has(ref.name)) {
              // Imported router files (direct or via barrels)
              const reachable = filterClosestReachable(propTargets, file, graph, importReach, 4);
              if (reachable.length > 0) {
                resolvedTargets = reachable;
              } else if (ref.isRpcChain && propTargets.length <= 2) {
                // Unambiguous chained RPC proxy call where router alias differs from filename
                resolvedTargets = propTargets;
              }
            }
          }
        }
      } else {
        // Lane C: bare symbol refs (`foo()`, `<Comp />`, `Type`, `import { FOO }`)
        const nonMethodTargets = targets.filter(t => t.kind !== 'method');
        if (nonMethodTargets.length === 0) continue;

        const sameFile = nonMethodTargets.filter(t => t.file === file);
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
            const sameDir = nonMethodTargets.filter(t => posix.dirname(t.file) === srcDir);
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
        const targetKey = target.key;
        const refKey = `${file}\0${targetKey}`;
        if (!addedRefs.has(refKey)) {
          addedRefs.add(refKey);
          graph.addEdge(file, targetKey, { type: 'REFERENCES' });
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
  const addedImports = new Map();
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
  const addedImports = new Map();
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
async function writeFileAtomic(filePath, data) {
  await mkdir(dirname(filePath), { recursive: true });
  const tmpPath = `${filePath}.${process.pid}.${Date.now()}.tmp`;
  await writeFile(tmpPath, data);
  try {
    await rename(tmpPath, filePath);
  } catch (err) {
    await unlink(tmpPath).catch(() => {});
    throw err;
  }
}

/**
 * Serialize graph + mtimes + precomputed unfocused PageRank into a compact
 * integer-indexed, gzip-compressed v5 cache file.
 * Exported flag is encoded in kindCode case (uppercase = exported, lowercase = local).
 * Re-export IMPORTS edge indices are stored in `re` so barrel-only reachability
 * works identically on warm cache loads.
 */
async function saveGraph(graph, mtimeMap, cachePath, { precomputedRanked = null } = {}) {
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
  const reexports = [];
  graph.forEachEdge((_edge, attrs, source, target) => {
    if (attrs.type === 'REFERENCES') {
      const fIdx = fileIndex.get(source);
      const sIdx = symIndex.get(target);
      if (fIdx !== undefined && sIdx !== undefined) refs.push(fIdx, sIdx);
    } else if (attrs.type === 'IMPORTS') {
      const srcIdx = fileIndex.get(source);
      const dstIdx = fileIndex.get(target);
      if (srcIdx !== undefined && dstIdx !== undefined) {
        if (attrs.reexport) reexports.push(imports.length >> 1);
        imports.push(srcIdx, dstIdx);
      }
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

  const payload = JSON.stringify({ v: 5, f: files, s: symbols, r: refs, i: imports, re: reexports, pr });
  const compressed = gzipSync(Buffer.from(payload), { level: 1 });
  await writeFileAtomic(cachePath, compressed);
}

/**
 * Load compact v5 (gzipped) graph cache from disk.
 */
async function loadGraph(cachePath) {
  try {
    const buf = await readFile(cachePath);
    const jsonStr = (buf.length >= 2 && buf[0] === 0x1f && buf[1] === 0x8b)
      ? gunzipSync(buf).toString('utf-8')
      : buf.toString('utf-8');
    const raw = JSON.parse(jsonStr);

    if (raw.v !== 5) return null;

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

    const reSet = new Set(raw.re || []);
    const imp = raw.i || [];
    for (let k = 0; k < imp.length; k += 2) {
      const src = filePaths[imp[k]];
      const dst = filePaths[imp[k + 1]];
      if (src !== undefined && dst !== undefined) {
        graph.addEdge(src, dst, { type: 'IMPORTS', reexport: reSet.has(k >> 1) });
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

const REF_KIND_TO_CODE = { symbol: 's', method: 'm', property: 'p' };
const CODE_TO_REF_KIND = { s: 'symbol', m: 'method', p: 'property' };

/**
 * Compactly serialize per-root parsed file records (definitions, deduplicated
 * references, deduplicated imports, and skipped-file markers) so multi-root
 * combinations reuse warm single-root parses without touching disk or tree-sitter.
 */
async function saveParseCache(parseCachePath, fileRecordMap) {
  const entries = {};
  for (const [relPath, rec] of fileRecordMap) {
    if (rec.skipped) {
      entries[relPath] = [rec.mtimeMs, 1];
      continue;
    }

    const defs = (rec.definitions || []).map(d => [
      d.name,
      encodeKind(d.kind, d.exported !== false),
      d.lineStart,
      d.lineEnd,
      d.signature || '',
    ]);

    // Deduplicate references per file (graph linking only needs unique tuples)
    const seenRefs = new Set();
    const refs = [];
    for (const r of rec.references || []) {
      const rk = REF_KIND_TO_CODE[r.kind] || 's';
      const recv = r.receiver || '';
      const recvMod = r.receiverModule || '';
      const flags = r.isRpcChain ? 1 : 0;
      const key = `${r.name}\0${rk}\0${recv}\0${recvMod}\0${flags}`;
      if (seenRefs.has(key)) continue;
      seenRefs.add(key);
      if (flags) refs.push([r.name, rk, recv, recvMod, flags]);
      else if (recvMod) refs.push([r.name, rk, recv, recvMod]);
      else if (recv) refs.push([r.name, rk, recv]);
      else if (rk !== 's') refs.push([r.name, rk]);
      else refs.push([r.name]);
    }

    const seenImps = new Set();
    const imps = [];
    for (const imp of rec.imports || []) {
      const reBit = imp.isReexport ? 1 : 0;
      const key = `${imp.path}\0${reBit}`;
      if (seenImps.has(key)) continue;
      seenImps.add(key);
      imps.push(reBit ? [imp.path, 1] : [imp.path]);
    }

    entries[relPath] = [rec.mtimeMs, 0, defs, refs, imps];
  }

  const payload = JSON.stringify({ v: PARSE_CACHE_VERSION, f: entries });
  const compressed = gzipSync(Buffer.from(payload), { level: 1 });
  await writeFileAtomic(parseCachePath, compressed);
}

/**
 * Load per-root parsed file records from disk. Returns `Map<relPath, record>`.
 */
async function loadParseCache(parseCachePath) {
  const result = new Map();
  try {
    const buf = await readFile(parseCachePath);
    const jsonStr = (buf.length >= 2 && buf[0] === 0x1f && buf[1] === 0x8b)
      ? gunzipSync(buf).toString('utf-8')
      : buf.toString('utf-8');
    const raw = JSON.parse(jsonStr);
    if (raw.v !== PARSE_CACHE_VERSION || !raw.f || typeof raw.f !== 'object') {
      return result;
    }

    for (const [relPath, entry] of Object.entries(raw.f)) {
      if (!Array.isArray(entry) || entry.length < 2) continue;
      const mtimeMs = entry[0];
      const skipped = entry[1] === 1;
      if (skipped) {
        result.set(relPath, { mtimeMs, skipped: true });
        continue;
      }

      const defs = (entry[2] || []).map(([name, kindCode, lineStart, lineEnd, signature]) => {
        const { kind, exported } = decodeKind(kindCode);
        return { name, kind, exported, lineStart, lineEnd, signature };
      });

      const refs = (entry[3] || []).map(tup => ({
        name: tup[0],
        kind: CODE_TO_REF_KIND[tup[1]] || 'symbol',
        receiver: tup[2] || null,
        receiverModule: tup[3] || null,
        isRpcChain: !!(tup[4] && (tup[4] & 1)),
      }));

      const imps = (entry[4] || []).map(tup => ({
        path: tup[0],
        isReexport: tup[1] === 1,
      }));

      result.set(relPath, { mtimeMs, skipped: false, definitions: defs, references: refs, imports: imps });
    }
  } catch {
    // Missing or unreadable parse cache — return empty map
  }
  return result;
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
  saveParseCache,
  loadParseCache,
  buildImportReachability,
};
