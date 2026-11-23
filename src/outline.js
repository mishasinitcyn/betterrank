import { parseFile, SUPPORTED_EXTENSIONS } from './parser.js';
import { extname } from 'path';

const MIN_COLLAPSE_LINES = 2;

/**
 * Parse a `--lines` range string such as `"120-260"` or `"120:260"`.
 * Returns `[start, end]` (1-indexed inclusive) or null if invalid.
 */
export function parseLineRange(spec) {
  if (!spec || typeof spec !== 'string') return null;
  const m = spec.trim().match(/^(\d+)\s*[-:]\s*(\d+)$/);
  if (!m) return null;
  const start = parseInt(m[1], 10);
  const end = parseInt(m[2], 10);
  if (!Number.isFinite(start) || !Number.isFinite(end) || start < 1 || end < start) return null;
  return [start, end];
}

/**
 * Build an outline view of a source file.
 *
 * @param {string} source - File contents
 * @param {string} filePath - File path (for language detection)
 * @param {string[]} expandSymbols - Symbol names to expand (empty = outline mode)
 * @param {object} [opts]
 * @param {Map<string,number>} [opts.callerCounts] - Symbol name -> caller file count (--annotate)
 * @param {[number,number]|null} [opts.lineRange] - Optional [start, end] 1-indexed line slice
 * @param {boolean} [opts.noComments=false] - Strip multi-line doc comments / docstrings
 * @param {number} [opts.depth=1] - Container expansion depth (1 = classes/routers, 2 = also inner helpers inside functions/components)
 * @param {boolean} [opts.collapseImports=false] - Collapse top-of-file import block (default false: keep imports visible)
 * @returns {string} Formatted output with line numbers
 */
export function buildOutline(
  source,
  filePath,
  expandSymbols = [],
  { callerCounts, lineRange = null, noComments = false, depth = 1, collapseImports = false } = {},
) {
  const lines = source.split('\n');
  const pad = Math.max(String(lines.length).length, 4);

  if (lineRange && expandSymbols.length === 0) {
    const [start, end] = lineRange;
    const clampedStart = Math.max(1, start);
    const clampedEnd = Math.min(lines.length, end);
    const out = [];
    for (let i = clampedStart; i <= clampedEnd; i++) {
      out.push(`${String(i).padStart(pad)}│ ${lines[i - 1]}`);
    }
    return out.join('\n');
  }

  const ext = extname(filePath);
  if (!SUPPORTED_EXTENSIONS.includes(ext)) {
    return rawView(lines, pad, lineRange);
  }

  const parsed = parseFile(filePath, source, { includeOutlineDefinitions: true });
  if (!parsed || parsed.definitions.length === 0) {
    return rawView(lines, pad, lineRange);
  }

  const kindPriority = { class: 5, namespace: 4, method: 3, function: 3, type: 2, property: 2, variable: 1, other: 0 };
  const byLine = new Map();
  for (const d of parsed.definitions) {
    const existing = byLine.get(d.lineStart);
    if (!existing || (kindPriority[d.kind] || 0) > (kindPriority[existing.kind] || 0)) {
      byLine.set(d.lineStart, d);
    }
  }
  const defs = [...byLine.values()].sort((a, b) => a.lineStart - b.lineStart);

  if (expandSymbols.length > 0) {
    return expandMode(lines, defs, filePath, expandSymbols, pad, lineRange);
  }
  return outlineMode(lines, defs, pad, { callerCounts, noComments, depth, collapseImports, ext });
}

function rawView(lines, pad, lineRange = null) {
  const start = lineRange ? Math.max(1, lineRange[0]) : 1;
  const end = lineRange ? Math.min(lines.length, lineRange[1]) : lines.length;
  const out = [];
  for (let i = start; i <= end; i++) {
    out.push(`${String(i).padStart(pad)}│ ${lines[i - 1]}`);
  }
  return out.join('\n');
}

function expandMode(lines, defs, filePath, expandSymbols, pad, lineRange = null) {
  const output = [];

  for (const symName of expandSymbols) {
    const matches = defs.filter(d => d.name === symName);

    if (matches.length === 0) {
      output.push(`Symbol "${symName}" not found in ${filePath}`);
      const similar = [...new Set(
        defs.filter(d => d.name.toLowerCase().includes(symName.toLowerCase()))
          .map(d => d.name),
      )].slice(0, 5);
      if (similar.length > 0) {
        output.push(`Did you mean: ${similar.join(', ')}`);
      } else {
        output.push(`Available: ${defs.map(d => d.name).slice(0, 40).join(', ')}`);
      }
      continue;
    }

    for (const def of matches) {
      if (matches.length > 1 || expandSymbols.length > 1) {
        output.push(`── ${def.name} (${filePath}:${def.lineStart}-${def.lineEnd}) ──`);
      }
      const start = lineRange ? Math.max(def.lineStart, lineRange[0]) : def.lineStart;
      const end = lineRange ? Math.min(def.lineEnd, lineRange[1]) : def.lineEnd;
      for (let i = start; i <= end; i++) {
        output.push(`${String(i).padStart(pad)}│ ${lines[i - 1]}`);
      }
      if (matches.length > 1) output.push('');
    }
  }

  return output.join('\n').trimEnd();
}

function detectImportBlock(lines) {
  let braceDepth = 0;
  let inImport = false;
  let firstImportLine = -1;
  let lastImportLine = -1;

  for (let i = 0; i < lines.length; i++) {
    const trimmed = lines[i].trim();
    if (trimmed === '') continue;

    if (!inImport) {
      if (trimmed.startsWith('import ') || trimmed === 'import') {
        if (firstImportLine === -1) firstImportLine = i;
        braceDepth = 0;
        for (const ch of lines[i]) {
          if (ch === '{') braceDepth++;
          else if (ch === '}') braceDepth--;
        }
        if (braceDepth > 0) {
          inImport = true;
        } else {
          lastImportLine = i;
        }
      } else if (firstImportLine !== -1) {
        break;
      }
    } else {
      for (const ch of lines[i]) {
        if (ch === '{') braceDepth++;
        else if (ch === '}') braceDepth--;
      }
      if (braceDepth <= 0) {
        inImport = false;
        lastImportLine = i;
      }
    }
  }

  if (firstImportLine === -1 || lastImportLine === -1) return null;
  const lineCount = lastImportLine - firstImportLine + 1;
  if (lineCount < 3) return null;
  return { start: firstImportLine + 1, end: lastImportLine + 1, lineCount, name: null };
}

/**
 * Identify multi-line block comment lines (`/* ... * /` in JS/TS/C/Rust/Java,
 * or triple-quoted docstrings in Python) for `--no-comments`.
 */
function detectCommentLines(lines, ext) {
  const skipped = new Set();
  const isPython = ext === '.py';

  if (isPython) {
    let inQuote = null;
    for (let i = 0; i < lines.length; i++) {
      const trimmed = lines[i].trim();
      if (!inQuote) {
        if (trimmed.startsWith('#')) {
          skipped.add(i + 1);
          continue;
        }
        for (const q of ['"""', "'''"]) {
          if (trimmed.startsWith(q)) {
            skipped.add(i + 1);
            const rest = trimmed.slice(3);
            if (!rest.includes(q)) inQuote = q;
            break;
          }
        }
      } else {
        skipped.add(i + 1);
        if (trimmed.includes(inQuote)) inQuote = null;
      }
    }
    return skipped;
  }

  let inBlock = false;
  for (let i = 0; i < lines.length; i++) {
    const trimmed = lines[i].trim();
    if (!inBlock) {
      if (trimmed.startsWith('//')) {
        skipped.add(i + 1);
      } else if (trimmed.startsWith('/*')) {
        skipped.add(i + 1);
        if (!trimmed.includes('*/')) inBlock = true;
      }
    } else {
      skipped.add(i + 1);
      if (trimmed.includes('*/')) inBlock = false;
    }
  }
  return skipped;
}

function findImmediateParents(defs) {
  const parents = new Map();

  for (const def of defs) {
    let parent = null;

    for (const other of defs) {
      if (other === def) continue;
      if (def.lineStart <= other.lineStart || def.lineEnd > other.lineEnd) continue;

      if (!parent) {
        parent = other;
        continue;
      }

      const isCloserParent =
        other.lineStart >= parent.lineStart && other.lineEnd <= parent.lineEnd;
      if (isCloserParent) parent = other;
    }

    parents.set(def, parent);
  }

  return parents;
}

function findDelegatingContainers(defs, parentMap, depth = 1) {
  const childCounts = new Map();

  for (const def of defs) {
    const parent = parentMap.get(def);
    if (!parent) continue;

    const isObjectRouterChild = parent.kind === 'variable' && def.kind === 'property';
    const isClassMemberChild =
      (parent.kind === 'class' || parent.kind === 'namespace') &&
      (def.kind === 'method' || def.kind === 'function' || def.kind === 'property' || def.kind === 'class');
    const isDeepFunctionChild =
      depth >= 2 &&
      (parent.kind === 'function' || parent.kind === 'method' || parent.kind === 'variable') &&
      !parentMap.get(parent) &&
      (def.kind === 'function' || def.kind === 'variable');

    if (isObjectRouterChild || isClassMemberChild || isDeepFunctionChild) {
      childCounts.set(parent, (childCounts.get(parent) || 0) + 1);
    }
  }

  return new Set(
    [...childCounts.entries()]
      .filter(([, count]) => count > 0)
      .map(([def]) => def),
  );
}

function appendAnnotation(line, count) {
  const annotation = count === 1 ? '← 1 caller' : `← ${count} callers`;
  const minWidth = 60;
  const padding = line.length < minWidth ? ' '.repeat(minWidth - line.length) : '  ';
  return line + padding + annotation;
}

function outlineMode(lines, defs, pad, { callerCounts, noComments = false, depth = 1, collapseImports = false, ext = '' } = {}) {
  const parentMap = findImmediateParents(defs);
  const delegatingContainers = findDelegatingContainers(defs, parentMap, depth);

  const nested = new Set();
  for (const def of defs) {
    let ancestor = parentMap.get(def);
    while (ancestor) {
      if (!delegatingContainers.has(ancestor)) {
        nested.add(def);
        break;
      }
      ancestor = parentMap.get(ancestor);
    }
  }

  const collapseRanges = [];
  const inlineAnnotations = new Map();

  if (collapseImports) {
    const importCollapse = detectImportBlock(lines);
    if (importCollapse) collapseRanges.push(importCollapse);
  }

  for (const def of defs) {
    if (nested.has(def)) continue;

    if (delegatingContainers.has(def)) {
      if (callerCounts && callerCounts.has(def.name)) {
        inlineAnnotations.set(def.lineStart, callerCounts.get(def.name));
      }
      continue;
    }

    const canCollapse =
      def.bodyStartLine &&
      def.bodyStartLine <= def.lineEnd &&
      (def.lineEnd - def.bodyStartLine + 1) >= MIN_COLLAPSE_LINES;

    if (!canCollapse) {
      if (callerCounts && callerCounts.has(def.name)) {
        inlineAnnotations.set(def.lineStart, callerCounts.get(def.name));
      }
      continue;
    }

    collapseRanges.push({
      start: def.bodyStartLine,
      end: def.lineEnd,
      lineCount: def.lineEnd - def.bodyStartLine + 1,
      name: def.name,
    });
  }

  collapseRanges.sort((a, b) => a.start - b.start);
  const commentLines = noComments ? detectCommentLines(lines, ext) : null;

  const output = [];
  let lineNum = 1;
  let rangeIdx = 0;

  while (lineNum <= lines.length) {
    while (rangeIdx < collapseRanges.length && collapseRanges[rangeIdx].start < lineNum) {
      rangeIdx++;
    }

    if (rangeIdx < collapseRanges.length && collapseRanges[rangeIdx].start === lineNum) {
      const range = collapseRanges[rangeIdx];
      const bodyLine = lines[lineNum - 1];
      const indent = bodyLine ? bodyLine.match(/^\s*/)[0] : '';
      let marker = `${' '.repeat(pad)}│ ${indent}... (${range.lineCount} lines)`;

      if (callerCounts && range.name && callerCounts.has(range.name)) {
        marker = appendAnnotation(marker, callerCounts.get(range.name));
      }

      output.push(marker);
      lineNum = range.end + 1;
      rangeIdx++;
      continue;
    }

    if (commentLines && commentLines.has(lineNum)) {
      lineNum++;
      continue;
    }

    let rendered = `${String(lineNum).padStart(pad)}│ ${lines[lineNum - 1]}`;
    if (inlineAnnotations.has(lineNum)) {
      rendered = appendAnnotation(rendered, inlineAnnotations.get(lineNum));
    }
    output.push(rendered);
    lineNum++;
  }

  return output.join('\n');
}
