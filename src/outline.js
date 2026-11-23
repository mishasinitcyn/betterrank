import { parseFile, SUPPORTED_EXTENSIONS } from './parser.js';
import { extname } from 'path';

const MIN_COLLAPSE_LINES = 2;

/**
 * Build an outline view of a source file.
 *
 * Without expandSymbols: returns a skeleton with function/method bodies collapsed
 * (classes and structural object containers stay expanded to reveal member signatures).
 * With expandSymbols: returns the full source of the specified symbols.
 *
 * @param {string} source - File contents
 * @param {string} filePath - File path (for language detection)
 * @param {string[]} expandSymbols - Symbol names to expand (empty = outline mode)
 * @param {object} [opts]
 * @param {Map<string,number>} [opts.callerCounts] - Map of symbol name -> caller file count (for --annotate)
 * @returns {string} Formatted output with line numbers
 */
export function buildOutline(source, filePath, expandSymbols = [], { callerCounts } = {}) {
  const lines = source.split('\n');
  const pad = Math.max(String(lines.length).length, 4);

  const ext = extname(filePath);
  if (!SUPPORTED_EXTENSIONS.includes(ext)) {
    return rawView(lines, pad);
  }

  const parsed = parseFile(filePath, source, { includeOutlineDefinitions: true });
  if (!parsed || parsed.definitions.length === 0) {
    return rawView(lines, pad);
  }

  // Deduplicate definitions by lineStart (decorated_definition / export_statement can cause dupes).
  // Prefer richer kinds (class/method/function) over generic variable duplicates on the same line.
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
    return expandMode(lines, defs, filePath, expandSymbols, pad);
  }
  return outlineMode(lines, defs, pad, callerCounts);
}

function rawView(lines, pad) {
  return lines.map((l, i) => `${String(i + 1).padStart(pad)}│ ${l}`).join('\n');
}

function expandMode(lines, defs, filePath, expandSymbols, pad) {
  const output = [];

  for (const symName of expandSymbols) {
    const matches = defs.filter(d => d.name === symName);

    if (matches.length === 0) {
      output.push(`Symbol "${symName}" not found in ${filePath}`);
      const similar = [...new Set(
        defs.filter(d => d.name.toLowerCase().includes(symName.toLowerCase()))
          .map(d => d.name)
      )].slice(0, 5);
      if (similar.length > 0) {
        output.push(`Did you mean: ${similar.join(', ')}`);
      } else {
        output.push(`Available: ${defs.map(d => d.name).join(', ')}`);
      }
      continue;
    }

    for (const def of matches) {
      if (matches.length > 1 || expandSymbols.length > 1) {
        output.push(`── ${def.name} (${filePath}:${def.lineStart}-${def.lineEnd}) ──`);
      }
      for (let i = def.lineStart; i <= def.lineEnd; i++) {
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

/**
 * Find container definitions whose bodies should stay open so their direct
 * children (class methods or router object properties) are shown individually
 * instead of collapsing the entire container into one line.
 */
function findDelegatingContainers(defs, parentMap) {
  const childCounts = new Map();

  for (const def of defs) {
    const parent = parentMap.get(def);
    if (!parent) continue;

    const isObjectRouterChild = parent.kind === 'variable' && def.kind === 'property';
    const isClassMemberChild =
      (parent.kind === 'class' || parent.kind === 'namespace') &&
      (def.kind === 'method' || def.kind === 'function' || def.kind === 'property' || def.kind === 'class');

    if (isObjectRouterChild || isClassMemberChild) {
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

function outlineMode(lines, defs, pad, callerCounts) {
  const parentMap = findImmediateParents(defs);
  const delegatingContainers = findDelegatingContainers(defs, parentMap);

  // A definition is swallowed if ANY ancestor along its parent chain is NOT a delegating container.
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
  const inlineAnnotations = new Map(); // lineNum -> callerCount

  const importCollapse = detectImportBlock(lines);
  if (importCollapse) collapseRanges.push(importCollapse);

  for (const def of defs) {
    if (nested.has(def)) continue;

    if (delegatingContainers.has(def)) {
      // Annotate the class / container header line itself since its body stays open
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
      // Short/one-line function or method — annotate its declaration line directly
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

  const output = [];
  let lineNum = 1;
  let rangeIdx = 0;

  while (lineNum <= lines.length) {
    // Advance past any overlapping range already skipped
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

    let rendered = `${String(lineNum).padStart(pad)}│ ${lines[lineNum - 1]}`;
    if (inlineAnnotations.has(lineNum)) {
      rendered = appendAnnotation(rendered, inlineAnnotations.get(lineNum));
    }
    output.push(rendered);
    lineNum++;
  }

  return output.join('\n');
}
