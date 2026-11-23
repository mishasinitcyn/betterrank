import { readdir, stat, readFile, open, unlink, mkdir } from 'fs/promises';
import { dirname, join, posix } from 'path';

const MINIFIED_FILENAME_RE = /\.(min|bundle|chunk)\.(js|mjs|cjs|ts|css)$/i;
const MAX_SOURCE_BYTES = 1024 * 1024; // 1 MB
const MINIFIED_MIN_BYTES = 4096;
const MINIFIED_SAMPLE_BYTES = 65536;
const MINIFIED_AVG_LINE_LEN = 200;
const MINIFIED_LONG_LINE_LEN = 500;
const MINIFIED_LONG_LINE_RATIO = 0.5;

/**
 * Detect minified, bundled, or generated mega-line source files so they
 * don't waste parse time or pollute PageRank scores.
 */
export function isMinifiedSource(source, filePath = '') {
  if (filePath && MINIFIED_FILENAME_RE.test(filePath)) return true;
  if (!source) return false;
  if (source.length > MAX_SOURCE_BYTES) return true;
  if (source.length < MINIFIED_MIN_BYTES) return false;

  const sample = source.length > MINIFIED_SAMPLE_BYTES
    ? source.slice(0, MINIFIED_SAMPLE_BYTES)
    : source;

  let lines = 1;
  let lineStart = 0;
  let longLineBytes = 0;

  for (let i = 0; i < sample.length; i++) {
    if (sample.charCodeAt(i) === 10) {
      const len = i - lineStart;
      if (len > MINIFIED_LONG_LINE_LEN) longLineBytes += len;
      lines++;
      lineStart = i + 1;
    }
  }
  const lastLen = sample.length - lineStart;
  if (lastLen > MINIFIED_LONG_LINE_LEN) longLineBytes += lastLen;

  const avgLineLen = sample.length / lines;
  if (avgLineLen > MINIFIED_AVG_LINE_LEN) return true;
  if (longLineBytes / sample.length > MINIFIED_LONG_LINE_RATIO) return true;

  return false;
}

/**
 * Convert a glob pattern (supporting **, *, ?, and leading !) into a RegExp
 * matching normalized POSIX relative paths.
 */
function globToRegExp(pattern) {
  let normalized = pattern.replace(/\\/g, '/').replace(/^\.\//, '');
  const isDirOnly = normalized.endsWith('/**');
  if (isDirOnly) normalized = normalized.slice(0, -3);

  let re = '^';
  let i = 0;
  while (i < normalized.length) {
    const ch = normalized[i];
    if (ch === '*') {
      if (normalized[i + 1] === '*') {
        if (normalized[i + 2] === '/') {
          re += '(?:.*/)?';
          i += 3;
        } else {
          re += '.*';
          i += 2;
        }
      } else {
        re += '[^/]*';
        i++;
      }
    } else if (ch === '?') {
      re += '[^/]';
      i++;
    } else if ('^$.|+()[]{}\\'.includes(ch)) {
      re += '\\' + ch;
      i++;
    } else {
      re += ch;
      i++;
    }
  }

  if (isDirOnly) {
    re += '(?:/.*)?$';
  } else {
    re += '$';
  }
  return new RegExp(re);
}

/**
 * Compile ignore patterns (including !negations) into a fast matcher with
 * directory pruning support.
 */
export function compileIgnoreMatcher(patterns) {
  const rules = [];
  let hasNegations = false;
  const fastPruneDirNames = new Set();

  for (const raw of patterns) {
    if (!raw || typeof raw !== 'string') continue;
    const negated = raw.startsWith('!');
    const body = negated ? raw.slice(1) : raw;
    if (negated) hasNegations = true;
    rules.push({ negated, regex: globToRegExp(body), raw: body });
  }

  if (!hasNegations) {
    for (const rule of rules) {
      const m = rule.raw.match(/^\*\*\/([^/*?]+)\/\*\*$/);
      if (m) fastPruneDirNames.add(m[1]);
    }
  }

  function isIgnored(relPath) {
    let ignored = false;
    for (const rule of rules) {
      if (rule.regex.test(relPath)) {
        ignored = !rule.negated;
      }
    }
    return ignored;
  }

  function shouldPruneDir(dirName, relPath) {
    if (fastPruneDirNames.has(dirName)) return true;
    if (hasNegations) return false;
    return isIgnored(relPath);
  }

  return { isIgnored, shouldPruneDir };
}

/**
 * Walk `rootDir` in parallel using native fs.readdir({ withFileTypes: true }),
 * pruning ignored directories before descending and filtering by extension.
 *
 * Returns POSIX-normalized relative paths.
 */
export async function walkFiles(rootDir, { extensions, ignorePatterns = [] }) {
  const extSet = new Set(extensions);
  const matcher = compileIgnoreMatcher(ignorePatterns);
  const results = [];

  async function visitDir(absDir, relPrefix) {
    let entries;
    try {
      entries = await readdir(absDir, { withFileTypes: true });
    } catch {
      return;
    }

    const subdirs = [];
    for (const entry of entries) {
      const name = entry.name;
      const relPath = relPrefix ? `${relPrefix}/${name}` : name;

      if (entry.isDirectory()) {
        if (matcher.shouldPruneDir(name, relPath)) continue;
        subdirs.push([join(absDir, name), relPath]);
      } else if (entry.isFile()) {
        const dotIdx = name.lastIndexOf('.');
        if (dotIdx === -1) continue;
        const ext = name.slice(dotIdx);
        if (!extSet.has(ext)) continue;
        if (matcher.isIgnored(relPath)) continue;
        results.push(relPath);
      }
    }

    if (subdirs.length > 0) {
      await Promise.all(subdirs.map(([abs, rel]) => visitDir(abs, rel)));
    }
  }

  await visitDir(rootDir, '');
  return results;
}

/**
 * Stat files with bounded concurrency. Returns Array<{ relPath, mtimeMs, size }>.
 */
export async function statFilesBatched(rootDir, relPaths, concurrency = 64) {
  const out = new Array(relPaths.length);
  let idx = 0;

  async function worker() {
    while (idx < relPaths.length) {
      const i = idx++;
      const relPath = relPaths[i];
      try {
        const st = await stat(join(rootDir, relPath));
        out[i] = { relPath, mtimeMs: st.mtimeMs, size: st.size };
      } catch {
        out[i] = null;
      }
    }
  }

  const workers = [];
  const count = Math.min(concurrency, relPaths.length);
  for (let w = 0; w < count; w++) workers.push(worker());
  await Promise.all(workers);

  return out.filter(Boolean);
}

const SKIP_META_DIRS = new Set([
  'node_modules', '.git', '.next', '.nuxt', '.turbo', 'dist', 'build', 'out',
  'coverage', '__pycache__', '.venv', 'venv', 'Pods', 'target', '.cache',
]);

function stripJsonCommentsAndTrailingCommas(raw) {
  let out = '';
  let inStr = false;
  let escape = false;
  let i = 0;
  while (i < raw.length) {
    const ch = raw[i];
    const next = raw[i + 1];
    if (inStr) {
      out += ch;
      if (escape) {
        escape = false;
      } else if (ch === '\\') {
        escape = true;
      } else if (ch === '"') {
        inStr = false;
      }
      i++;
      continue;
    }
    if (ch === '"') {
      inStr = true;
      out += ch;
      i++;
      continue;
    }
    if (ch === '/' && next === '/') {
      i += 2;
      while (i < raw.length && raw[i] !== '\n') i++;
      continue;
    }
    if (ch === '/' && next === '*') {
      i += 2;
      while (i < raw.length && !(raw[i] === '*' && raw[i + 1] === '/')) i++;
      i += 2;
      continue;
    }
    out += ch;
    i++;
  }
  return out.replace(/,\s*([}\]])/g, '$1');
}

/**
 * Discover workspace JS/TS packages (`package.json` with a `name`), `tsconfig.json`
 * `paths` aliases, and Python package roots (`pyproject.toml`, `setup.py`, `setup.cfg`)
 * up to depth 3 inside a root directory.
 *
 * Used by graph.js to resolve workspace aliases like `@scope/pkg`, custom tsconfig
 * paths, and Python package imports under single-repo or multi-repo roots.
 */
export async function discoverRootMetadata(rootDir, prefix = '') {
  const workspacePackages = [];
  const pythonRoots = [];
  const tsPathMappings = [];
  const prefixNorm = prefix ? prefix.replace(/\\/g, '/').replace(/\/+$/, '') : '';

  if (prefixNorm) {
    pythonRoots.push(prefixNorm);
  } else {
    pythonRoots.push('');
  }

  async function scan(absDir, relDir, depth) {
    let entries;
    try {
      entries = await readdir(absDir, { withFileTypes: true });
    } catch {
      return;
    }

    let hasPackageJson = false;
    let hasTsConfig = false;
    let hasPyProject = false;
    const subdirs = [];

    for (const entry of entries) {
      if (entry.isFile()) {
        if (entry.name === 'package.json') hasPackageJson = true;
        else if (entry.name === 'tsconfig.json') hasTsConfig = true;
        else if (entry.name === 'pyproject.toml' || entry.name === 'setup.py' || entry.name === 'setup.cfg') {
          hasPyProject = true;
        }
      } else if (entry.isDirectory() && depth < 3 && !SKIP_META_DIRS.has(entry.name) && !entry.name.startsWith('.')) {
        subdirs.push([join(absDir, entry.name), relDir ? `${relDir}/${entry.name}` : entry.name]);
      }
    }

    const fullRelDir = prefixNorm
      ? (relDir ? `${prefixNorm}/${relDir}` : prefixNorm)
      : relDir;

    if (hasPyProject && !pythonRoots.includes(fullRelDir)) {
      pythonRoots.push(fullRelDir);
    }

    // Register root-level package.json too (critical for repeatable --root packages/a --root apps/b)
    if (hasPackageJson) {
      try {
        const pkg = JSON.parse(await readFile(join(absDir, 'package.json'), 'utf-8'));
        if (pkg.name && typeof pkg.name === 'string') {
          workspacePackages.push({
            name: pkg.name,
            dir: fullRelDir,
            main: typeof pkg.main === 'string' ? pkg.main : null,
            exports: pkg.exports && typeof pkg.exports === 'object' ? pkg.exports : null,
          });
        }
      } catch {
        // ignore invalid package.json
      }
    }

    if (hasTsConfig) {
      try {
        const raw = await readFile(join(absDir, 'tsconfig.json'), 'utf-8');
        const tsconfig = JSON.parse(stripJsonCommentsAndTrailingCommas(raw));
        const paths = tsconfig?.compilerOptions?.paths;
        if (paths && typeof paths === 'object' && Object.keys(paths).length > 0) {
          tsPathMappings.push({
            dir: fullRelDir,
            baseUrl: typeof tsconfig.compilerOptions.baseUrl === 'string'
              ? tsconfig.compilerOptions.baseUrl
              : '.',
            paths,
          });
        }
      } catch {
        // ignore invalid tsconfig.json
      }
    }

    if (subdirs.length > 0) {
      await Promise.all(subdirs.map(([abs, rel]) => scan(abs, rel, depth + 1)));
    }
  }

  await scan(rootDir, '', 0);
  return { workspacePackages, pythonRoots, tsPathMappings };
}

function isPidAlive(pid) {
  if (!pid || !Number.isInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (err) {
    return err.code === 'EPERM';
  }
}

/**
 * Acquire an exclusive per-cache lockfile so concurrent CLI processes targeting
 * the same uncached root don't run duplicate cold builds simultaneously.
 *
 * Returns `{ acquired, waited, release }`.
 * - If another live process held the lock and released it while we waited,
 *   `waited` is `true` so the caller can immediately reload the fresh cache.
 */
export async function acquireCacheLock(cachePath, { timeoutMs = 120000, pollMs = 100 } = {}) {
  const lockPath = `${cachePath}.lock`;
  await mkdir(dirname(lockPath), { recursive: true }).catch(() => {});

  const deadline = Date.now() + timeoutMs;
  let waited = false;

  while (true) {
    try {
      const handle = await open(lockPath, 'wx');
      await handle.writeFile(JSON.stringify({ pid: process.pid, ts: Date.now() }));
      await handle.close();

      let released = false;
      const release = async () => {
        if (released) return;
        released = true;
        await unlink(lockPath).catch(() => {});
      };
      return { acquired: true, waited, release };
    } catch (err) {
      if (err.code !== 'EEXIST') {
        return { acquired: false, waited, release: async () => {} };
      }

      // Check for stale lock (dead PID or older than 2 minutes)
      let stale = false;
      try {
        const raw = await readFile(lockPath, 'utf-8');
        const info = JSON.parse(raw);
        if (!isPidAlive(info.pid) || (Date.now() - (info.ts || 0)) > 120000) {
          stale = true;
        }
      } catch {
        // Lock file may be mid-write or vanished; check mtime
        try {
          const st = await stat(lockPath);
          if (Date.now() - st.mtimeMs > 120000) stale = true;
        } catch {
          continue;
        }
      }

      if (stale) {
        await unlink(lockPath).catch(() => {});
        continue;
      }

      if (Date.now() >= deadline) {
        return { acquired: false, waited, release: async () => {} };
      }

      waited = true;
      await new Promise(resolve => setTimeout(resolve, pollMs));
    }
  }
}
