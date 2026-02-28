import { parseFile, SUPPORTED_EXTENSIONS } from './parser.js';
import { extname } from 'path';

const MIN_COLLAPSE_LINES = 2;

/**
 * Build an outline view of a source file.
 *
 * Without expandSymbols: returns a skeleton with function/class bodies collapsed.
 * With expandSymbols: returns the full source of the specified symbols.
 *
 * @param {string} source - File contents
 * @param {string} filePath - File path (for language detection)
 * @param {string[]} expandSymbols - Symbol names to expand (empty = outline mode)
 * @returns {string} Formatted output with line numbers
 */
export function buildOutline(source, filePath, expandSymbols = []) {
  const lines = source.split('\n');
  const pad = Math.max(String(lines.length).length, 4);

  const ext = extname(filePath);
  if (!SUPPORTED_EXTENSIONS.includes(ext)) {
    return rawView(lines, pad);
  }

  const parsed = parseFile(filePath, source);
  if (!parsed || parsed.definitions.length === 0) {
    return rawView(lines, pad);
  }

  // Deduplicate definitions by lineStart (decorated_definition can cause dupes)
  const seenLines = new Set();
  const defs = parsed.definitions
    .filter(d => {
      if (seenLines.has(d.lineStart)) return false;
      seenLines.add(d.lineStart);
      return true;
    })
    .sort((a, b) => a.lineStart - b.lineStart);

  if (expandSymbols.length > 0) {
    return expandMode(lines, defs, filePath, expandSymbols, pad);
  }
  return outlineMode(lines, defs, pad);
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

function outlineMode(lines, defs, pad) {
  // Detect containers: definitions that have child definitions inside them
  const containers = new Set();
  for (const def of defs) {
    for (const other of defs) {
      if (other === def) continue;
      if (other.lineStart > def.lineStart && other.lineEnd <= def.lineEnd) {
        containers.add(def);
        break;
      }
    }
  }

  // Build collapse ranges for leaf definitions with sufficient body size
  const collapseRanges = [];
  for (const def of defs) {
    if (containers.has(def)) continue;
    if (!def.bodyStartLine) continue;
    if (def.bodyStartLine > def.lineEnd) continue;

    const bodyLineCount = def.lineEnd - def.bodyStartLine + 1;
    if (bodyLineCount < MIN_COLLAPSE_LINES) continue;

    collapseRanges.push({
      start: def.bodyStartLine,
      end: def.lineEnd,
      lineCount: bodyLineCount,
    });
  }

  collapseRanges.sort((a, b) => a.start - b.start);

  // Walk lines, skipping collapsed ranges
  const output = [];
  let lineNum = 1;
  let rangeIdx = 0;

  while (lineNum <= lines.length) {
    if (rangeIdx < collapseRanges.length && collapseRanges[rangeIdx].start === lineNum) {
      const range = collapseRanges[rangeIdx];
      // Use the indent of the first body line for natural alignment
      const bodyLine = lines[lineNum - 1];
      const indent = bodyLine ? bodyLine.match(/^\s*/)[0] : '';
      output.push(`${' '.repeat(pad)}│ ${indent}... (${range.lineCount} lines)`);
      lineNum = range.end + 1;
      rangeIdx++;
      continue;
    }

    output.push(`${String(lineNum).padStart(pad)}│ ${lines[lineNum - 1]}`);
    lineNum++;
  }

  return output.join('\n');
}
