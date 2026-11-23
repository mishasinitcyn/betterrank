import { readdir, stat } from 'fs/promises';
import { join, relative } from 'path';

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

  // Exact single-segment directory patterns like "**/node_modules/**" can be
  // pruned before regex testing as long as no negation rule could re-include them.
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
 * Returns POSIX-normalized relative paths sorted deterministically.
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
