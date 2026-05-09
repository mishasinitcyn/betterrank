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
 * @param {object} [opts]
 * @param {Map<string,number>} [opts.callerCounts] - Map of symbol name → caller file count (for --annotate)
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

// Detect a contiguous block of import statements at the top of the file.
// Returns { start, end, lineCount } (1-indexed) or null if block is < 3 lines.
// Blank lines and leading non-import lines (e.g. "use client") are skipped over.
function detectImportBlock(lines) {
  let braceDepth = 0;
  let inImport = false;
  let firstImportLine = -1; // 0-indexed
  let lastImportLine = -1;  // 0-indexed

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
        // First non-blank, non-import line after imports began
        break;
      }
      // else: pre-import line ("use client", comments) — keep scanning
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

function outlineMode(lines, defs, pad, callerCounts) {
  // Mark definitions that are nested inside another definition.
  // Only top-level defs get their own collapse range — nested ones are swallowed
  // by the parent's collapse, preventing dozens of inline-callback markers.
  const nested = new Set();
  for (const def of defs) {
    for (const other of defs) {
      if (other === def) continue;
      if (def.lineStart > other.lineStart && def.lineEnd <= other.lineEnd) {
        nested.add(def);
        break;
      }
    }
  }

  const collapseRanges = [];

  // Collapse the import block first (covers the lucide/shadcn import wall in TSX)
  const importCollapse = detectImportBlock(lines);
  if (importCollapse) collapseRanges.push(importCollapse);

  // Collapse all top-level definitions (functions, classes, interfaces, components)
  for (const def of defs) {
    if (nested.has(def)) continue;
    if (!def.bodyStartLine) continue;
    if (def.bodyStartLine > def.lineEnd) continue;

    const bodyLineCount = def.lineEnd - def.bodyStartLine + 1;
    if (bodyLineCount < MIN_COLLAPSE_LINES) continue;

    collapseRanges.push({
      start: def.bodyStartLine,
      end: def.lineEnd,
      lineCount: bodyLineCount,
      name: def.name,
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
      const bodyLine = lines[lineNum - 1];
      const indent = bodyLine ? bodyLine.match(/^\s*/)[0] : '';
      let marker = `${' '.repeat(pad)}│ ${indent}... (${range.lineCount} lines)`;

      // Append caller annotation if available
      if (callerCounts && range.name && callerCounts.has(range.name)) {
        const count = callerCounts.get(range.name);
        const annotation = count === 1 ? '← 1 caller' : `← ${count} callers`;
        // Right-pad to align annotations
        const minWidth = 60;
        if (marker.length < minWidth) {
          marker += ' '.repeat(minWidth - marker.length);
        } else {
          marker += '  ';
        }
        marker += annotation;
      }

      output.push(marker);
      lineNum = range.end + 1;
      rangeIdx++;
      continue;
    }

    output.push(`${String(lineNum).padStart(pad)}│ ${lines[lineNum - 1]}`);
    lineNum++;
  }

  return output.join('\n');
}
