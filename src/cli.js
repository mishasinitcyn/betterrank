#!/usr/bin/env node

import { CodeIndex } from './index.js';
import { resolve, relative, isAbsolute } from 'path';
import { readFile } from 'fs/promises';

const DEFAULT_LIMIT = 50;
const DEFAULT_DEPTH = 3;

const USAGE = `
betterrank <command> [options]

Commands:
  ui          [--port N]                            Launch web UI (default port: 3333)
  outline     <file> [symbol1,symbol2] [--annotate]  File skeleton (--annotate for caller counts)
  map         [--focus file1,file2]                 Repo map (ranked by PageRank)
  search      <query> [--kind type]                 Substring search on symbol names + signatures (ranked by PageRank)
  structure   [--depth N]                           File tree with symbol counts (default depth: ${DEFAULT_DEPTH})
  symbols     [--file path] [--kind type]           List definitions (ranked by PageRank)
  callers     <symbol> [--file path] [--context]     All call sites (ranked, with context lines)
  deps        <file>                                What this file imports (ranked)
  dependents  <file>                                What imports this file (ranked)
  neighborhood <file> [--hops N] [--max-files N]    Local subgraph (ranked by PageRank)
  orphans     [--level file|symbol] [--kind type]   Find disconnected files/symbols
  reindex                                           Force full rebuild
  stats                                             Index statistics

Global flags:
  --root <path>     Project root (default: cwd). Always pass this explicitly.
  --count           Return counts only (no content)
  --offset N        Skip first N results
  --limit N         Max results to return (default: ${DEFAULT_LIMIT} for list commands)
  --help            Show help for a command (e.g. betterrank search --help)
`.trim();

const COMMAND_HELP = {
  outline: `betterrank outline <file> [symbol1,symbol2,...] [--annotate --root <path>]

View a file's structure with function/class bodies collapsed, or expand
specific symbols to see their full source.

Without symbol names: shows the file skeleton — imports, constants, and
function/class signatures with bodies replaced by "... (N lines)".

With symbol names (comma-separated): shows the full source of those
specific functions/classes with line numbers.

Options:
  --root <path>     Resolve file path relative to this directory
  --annotate        Show caller counts next to each function (requires --root)

Examples:
  betterrank outline src/auth.py
  betterrank outline src/auth.py authenticate_user
  betterrank outline src/auth.py validate,process
  betterrank outline src/handlers.ts --root ./backend
  betterrank outline src/auth.py --annotate --root ./backend`,

  map: `betterrank map [--focus file1,file2] [--root <path>]

Aider-style repo map: the most structurally important definitions ranked by PageRank.

Options:
  --focus <files>   Comma-separated files to bias ranking toward
  --count           Return total symbol count only
  --offset N        Skip first N symbols
  --limit N         Max symbols to return (default: ${DEFAULT_LIMIT})

Examples:
  betterrank map --root ./backend
  betterrank map --root ./frontend --limit 100
  betterrank map --root ./backend --focus src/auth/handlers.ts,src/api/login.ts`,

  search: `betterrank search <query> [--kind type] [--root <path>]

Substring search on symbol names + full signatures (param names, types, defaults).
Results ranked by PageRank (most structurally important first).

Options:
  --kind <type>    Filter: function, class, type, variable, namespace, import
  --count          Return match count only
  --offset N       Skip first N results
  --limit N        Max results (default: ${DEFAULT_LIMIT})

Tips:
  Use short substrings (3-5 chars) — PageRank ranking handles noise.
  "imp" finds encrypt_imp_payload, increment_impression, etc.
  Searches match against both symbol names AND full signatures (param names, types).

Examples:
  betterrank search resolve --root ./backend
  betterrank search auth --kind function --limit 10
  betterrank search max_age --root . --count`,

  structure: `betterrank structure [--depth N] [--root <path>]

File tree with symbol counts per file.

Options:
  --depth N     Max directory depth (default: ${DEFAULT_DEPTH})

Examples:
  betterrank structure --root ./backend --depth 2`,

  symbols: `betterrank symbols [--file path] [--kind type] [--root <path>]

List symbol definitions, optionally filtered by file or kind.
Results ranked by PageRank (most structurally important first).

Options:
  --file <path>    Filter to a specific file (relative to --root)
  --kind <type>    Filter: function, class, type, variable, namespace, import
  --count          Return count only
  --offset N       Skip first N results
  --limit N        Max results (default: ${DEFAULT_LIMIT})

Examples:
  betterrank symbols --file src/auth/handlers.ts --root ./backend
  betterrank symbols --kind class --root . --limit 20`,

  callers: `betterrank callers <symbol> [--file path] [--context [N]] [--root <path>]

Find all files that reference a symbol. Ranked by file-level PageRank.

Options:
  --file <path>    Disambiguate when multiple symbols share a name
  --context [N]    Show N lines of context around each call site (default: 2)
  --count          Return count only
  --offset N       Skip first N results
  --limit N        Max results (default: ${DEFAULT_LIMIT})

Examples:
  betterrank callers authenticateUser --root ./backend
  betterrank callers authenticateUser --root ./backend --context
  betterrank callers resolve --file src/utils.ts --root . --context 3`,

  deps: `betterrank deps <file> [--root <path>]

What this file imports / depends on. Ranked by PageRank.

Options:
  --count          Return count only
  --offset N       Skip first N results
  --limit N        Max results (default: ${DEFAULT_LIMIT})

Examples:
  betterrank deps src/auth/handlers.ts --root ./backend`,

  dependents: `betterrank dependents <file> [--root <path>]

What files import this file. Ranked by PageRank.

Options:
  --count          Return count only
  --offset N       Skip first N results
  --limit N        Max results (default: ${DEFAULT_LIMIT})

Examples:
  betterrank dependents src/auth/handlers.ts --root ./backend`,

  neighborhood: `betterrank neighborhood <file> [--hops N] [--max-files N] [--root <path>]

Local subgraph around a file: its imports, importers, and their symbols.

Options:
  --hops N          BFS depth for outgoing imports (default: 2)
  --max-files N     Max files to include (default: 15, direct neighbors always included)
  --count           Return counts only
  --offset N        Skip first N files
  --limit N         Max files to return

Examples:
  betterrank neighborhood src/auth/handlers.ts --root ./backend
  betterrank neighborhood src/api/bid.js --hops 3 --max-files 20 --root .`,

  orphans: `betterrank orphans [--level file|symbol] [--kind type] [--root <path>]

Find disconnected files or symbols — the "satellites" in the graph UI.

Levels:
  file     Files with zero cross-file imports (default)
  symbol   Symbols never referenced from outside their own file (dead code candidates)

Options:
  --level <type>   "file" or "symbol" (default: file)
  --kind <type>    Filter symbols: function, class, type, variable (only with --level symbol)
  --count          Return count only
  --offset N       Skip first N results
  --limit N        Max results (default: ${DEFAULT_LIMIT})

False positives (entry points, config files, tests, framework hooks, dunders,
etc.) are automatically excluded.

Examples:
  betterrank orphans --root ./backend
  betterrank orphans --level symbol --root .
  betterrank orphans --level symbol --kind function --root .
  betterrank orphans --count --root .`,

  reindex: `betterrank reindex [--root <path>]

Force a full rebuild of the index. Use after branch switches, large merges,
or if results seem stale.

Examples:
  betterrank reindex --root ./backend`,

  stats: `betterrank stats [--root <path>]

Show index statistics: file count, symbol count, edge count.

Examples:
  betterrank stats --root .`,

  ui: `betterrank ui [--port N]

Launch the interactive web UI for exploring the index.

Options:
  --port N    Port to listen on (default: 3333)

Examples:
  betterrank ui
  betterrank ui --port 8080`,
};

async function main() {
  const args = process.argv.slice(2);
  if (args.length === 0 || args[0] === '--help' || args[0] === '-h') {
    console.log(USAGE);
    process.exit(0);
  }

  const command = args[0];
  const flags = parseFlags(args.slice(1));

  // Per-command --help
  if (flags.help === true || flags.h === true) {
    if (COMMAND_HELP[command]) {
      console.log(COMMAND_HELP[command]);
    } else {
      console.log(USAGE);
    }
    process.exit(0);
  }

  // UI command doesn't need --root or a CodeIndex instance
  if (command === 'ui') {
    const { startServer } = await import('./server.js');
    const port = parseInt(flags.port || '3333', 10);
    startServer(port);
    // Open browser
    const opener = process.platform === 'darwin' ? 'open' : process.platform === 'win32' ? 'start' : 'xdg-open';
    const { exec } = await import('child_process');
    exec(`${opener} http://localhost:${port}`);
    return; // Keep process alive (server is listening)
  }

  // Outline command — standalone by default, needs CodeIndex for --annotate
  if (command === 'outline') {
    const filePath = flags._positional[0];
    if (!filePath) {
      console.error('Usage: betterrank outline <file> [symbol1,symbol2] [--annotate --root <path>]');
      process.exit(1);
    }
    const expandSymbols = flags._positional[1] ? flags._positional[1].split(',') : [];
    const annotate = flags.annotate === true;

    if (annotate && !flags.root) {
      console.error('outline --annotate requires --root for graph data');
      process.exit(1);
    }

    const root = flags.root ? resolve(flags.root) : process.cwd();
    const absPath = isAbsolute(filePath) ? filePath : resolve(root, filePath);

    let source;
    try {
      source = await readFile(absPath, 'utf-8');
    } catch (err) {
      console.error(`Cannot read file: ${absPath}`);
      console.error(err.message);
      process.exit(1);
    }

    const relPath = relative(root, absPath);
    const { buildOutline } = await import('./outline.js');

    let callerCounts;
    if (annotate) {
      const idx = new CodeIndex(resolve(flags.root));
      callerCounts = await idx.getCallerCounts(relPath);
    }

    const result = buildOutline(source, relPath, expandSymbols, { callerCounts });
    console.log(result);
    return;
  }

  const projectRoot = resolve(flags.root || process.cwd());
  if (!flags.root) {
    process.stderr.write(`⚠ No --root specified, using cwd: ${projectRoot}\n`);
  }
  const idx = new CodeIndex(projectRoot);

  const countMode = flags.count === true;
  const offset = flags.offset !== undefined ? parseInt(flags.offset, 10) : undefined;
  const userLimit = flags.limit !== undefined ? parseInt(flags.limit, 10) : undefined;

  // Normalize a file path argument relative to projectRoot.
  // Handles cases like `neighborhood gravity-engine/src/foo.py --root gravity-engine`
  // where the graph stores `src/foo.py` but the user passes the full path.
  function normalizeFilePath(filePath) {
    if (!filePath) return filePath;
    const abs = resolve(filePath);
    const rel = relative(projectRoot, abs);
    // If the result starts with '..' the file is outside projectRoot — return as-is
    if (rel.startsWith('..')) return filePath;
    return rel;
  }

  /** Print file-not-found diagnostics and exit. Returns true if handled. */
  function handleFileNotFound(result, filePath) {
    if (!result || !result.fileNotFound) return false;
    console.error(`File "${filePath}" not found in index.`);
    if (result.suggestions && result.suggestions.length > 0) {
      console.error(`Did you mean:`);
      for (const s of result.suggestions) console.error(`  ${s}`);
    }
    console.error(`Tip: File paths are relative to --root. Use \`betterrank structure\` to see indexed files.`);
    return true;
  }

  /** Suggest shorter query alternatives by splitting on _ and camelCase boundaries. */
  function suggestShorterQueries(query) {
    const parts = new Set();
    // Split on underscores
    for (const p of query.split('_')) {
      if (p.length >= 3) parts.add(p.toLowerCase());
    }
    // Split on camelCase boundaries
    const camelParts = query.replace(/([a-z])([A-Z])/g, '$1_$2').split('_');
    for (const p of camelParts) {
      if (p.length >= 3) parts.add(p.toLowerCase());
    }
    // Remove the original query itself
    parts.delete(query.toLowerCase());
    return [...parts].slice(0, 4);
  }

  switch (command) {
    case 'map': {
      const focusFiles = flags.focus ? flags.focus.split(',') : [];
      const effectiveLimit = countMode ? undefined : (userLimit !== undefined ? userLimit : DEFAULT_LIMIT);
      const result = await idx.map({ focusFiles, count: countMode, offset, limit: effectiveLimit });
      if (countMode) {
        console.log(`total: ${result.total}`);
      } else {
        console.log(result.content);
        if (result.shownSymbols < result.totalSymbols) {
          console.log(`\nShowing ${result.shownFiles} of ${result.totalFiles} files, ${result.shownSymbols} of ${result.totalSymbols} symbols (ranked by PageRank)`);
          console.log(`Use --limit N to show more, or --count for totals`);
        }
      }
      // Diagnostics on empty index
      if (result.diagnostics) {
        const d = result.diagnostics;
        console.log(`\nDiagnostics:`);
        console.log(`  Root:        ${d.root}`);
        console.log(`  Files found: ${d.filesScanned}`);
        console.log(`  Extensions:  ${d.extensions}`);
        if (d.filesScanned === 0) {
          console.log(`  Tip:         No supported files found. Try a broader --root.`);
        }
      }
      break;
    }

    case 'search': {
      const query = flags._positional[0];
      if (!query) { console.error('Usage: betterrank search <query> [--kind type]'); process.exit(1); }
      const effectiveLimit = countMode ? undefined : (userLimit !== undefined ? userLimit : DEFAULT_LIMIT);
      const result = await idx.search({ query, kind: flags.kind, count: countMode, offset, limit: effectiveLimit });
      if (countMode) {
        console.log(`total: ${result.total}`);
      } else {
        for (const s of result) {
          console.log(`${s.file}:${s.lineStart}  [${s.kind}] ${s.signature}`);
        }
        if (result.length === 0) {
          const st = await idx.stats();
          console.log(`(no matches for "${query}")`);
          if (st.symbols > 0) {
            console.log(`Index has ${st.symbols.toLocaleString()} symbols across ${st.files.toLocaleString()} files.`);
            const suggestions = suggestShorterQueries(query);
            if (suggestions.length > 0) {
              console.log(`Tip: Use shorter substrings. Try: ${suggestions.map(s => `"${s}"`).join(', ')}`);
            }
          } else {
            console.log(`Index is empty. Check --root or run: betterrank map --root <path>`);
          }
        } else if (result.length === effectiveLimit && userLimit === undefined) {
          console.log(`\n(showing top ${effectiveLimit} by relevance — use --limit N or --count for total)`);
        }
      }
      break;
    }

    case 'structure': {
      const depth = flags.depth ? parseInt(flags.depth, 10) : DEFAULT_DEPTH;
      const result = await idx.structure({ depth });
      console.log(result);
      if (!flags.depth) {
        console.log(`\n(default depth ${DEFAULT_DEPTH} — use --depth N to expand)`);
      }
      break;
    }

    case 'symbols': {
      const effectiveLimit = countMode ? undefined : (userLimit !== undefined ? userLimit : DEFAULT_LIMIT);
      const result = await idx.symbols({ file: normalizeFilePath(flags.file), kind: flags.kind, count: countMode, offset, limit: effectiveLimit });
      if (countMode) {
        console.log(`total: ${result.total}`);
      } else {
        for (const s of result) {
          console.log(`${s.file}:${s.lineStart}  [${s.kind}] ${s.signature}`);
        }
        if (result.length === 0) {
          console.log('(no symbols found)');
        } else if (result.length === effectiveLimit && userLimit === undefined) {
          console.log(`\n(showing top ${effectiveLimit} by relevance — use --limit N or --count for total)`);
        }
      }
      break;
    }

    case 'callers': {
      const symbol = flags._positional[0];
      if (!symbol) { console.error('Usage: betterrank callers <symbol> [--file path]'); process.exit(1); }
      const effectiveLimit = countMode ? undefined : (userLimit !== undefined ? userLimit : DEFAULT_LIMIT);
      const contextLines = flags.context === true ? 2 : (flags.context ? parseInt(flags.context, 10) : 0);
      const result = await idx.callers({ symbol, file: normalizeFilePath(flags.file), count: countMode, offset, limit: effectiveLimit, context: contextLines });
      if (countMode) {
        console.log(`total: ${result.total}`);
      } else if (contextLines > 0) {
        // Rich output with call-site context
        const pad = 6;
        for (const c of result) {
          console.log(`${c.file}:`);
          if (c.sites && c.sites.length > 0) {
            for (const site of c.sites) {
              for (const t of site.text) {
                console.log(`  ${String(t.line).padStart(pad)}│ ${t.content}`);
              }
              console.log('');
            }
          } else {
            console.log('  (no call sites found in source)\n');
          }
        }
        if (result.length === 0) {
          console.log('(no callers found)');
        }
      } else {
        for (const c of result) {
          console.log(c.file);
        }
        if (result.length === 0) {
          console.log('(no callers found)');
        } else if (result.length === effectiveLimit && userLimit === undefined) {
          console.log(`\n(showing top ${effectiveLimit} by relevance — use --limit N or --count for total)`);
        }
      }
      break;
    }

    case 'deps': {
      const file = normalizeFilePath(flags._positional[0]);
      if (!file) { console.error('Usage: betterrank deps <file>'); process.exit(1); }
      const effectiveLimit = countMode ? undefined : (userLimit !== undefined ? userLimit : DEFAULT_LIMIT);
      const result = await idx.dependencies({ file, count: countMode, offset, limit: effectiveLimit });
      if (handleFileNotFound(result, file)) break;
      if (countMode) {
        console.log(`total: ${result.total}`);
      } else {
        const items = result.items || result;
        for (const d of items) console.log(d);
        if (items.length === 0) {
          console.log('(no dependencies)');
        } else if (items.length === effectiveLimit && userLimit === undefined) {
          console.log(`\n(showing top ${effectiveLimit} by relevance — use --limit N or --count for total)`);
        }
      }
      break;
    }

    case 'dependents': {
      const file = normalizeFilePath(flags._positional[0]);
      if (!file) { console.error('Usage: betterrank dependents <file>'); process.exit(1); }
      const effectiveLimit = countMode ? undefined : (userLimit !== undefined ? userLimit : DEFAULT_LIMIT);
      const result = await idx.dependents({ file, count: countMode, offset, limit: effectiveLimit });
      if (handleFileNotFound(result, file)) break;
      if (countMode) {
        console.log(`total: ${result.total}`);
      } else {
        const items = result.items || result;
        for (const d of items) console.log(d);
        if (items.length === 0) {
          console.log('(no dependents)');
        } else if (items.length === effectiveLimit && userLimit === undefined) {
          console.log(`\n(showing top ${effectiveLimit} by relevance — use --limit N or --count for total)`);
        }
      }
      break;
    }

    case 'neighborhood': {
      const file = normalizeFilePath(flags._positional[0]);
      if (!file) { console.error('Usage: betterrank neighborhood <file> [--hops N] [--max-files N]'); process.exit(1); }
      const hops = parseInt(flags.hops || '2', 10);
      const maxFilesFlag = flags['max-files'] ? parseInt(flags['max-files'], 10) : 15;

      // Safety: if neither --count nor --limit/--offset was provided, force a
      // count-first response so we never accidentally dump hundreds of files.
      const needsSafetyCount = !countMode && offset === undefined && userLimit === undefined;

      if (needsSafetyCount) {
        const preview = await idx.neighborhood({
          file, hops, maxFiles: maxFilesFlag, count: true,
        });
        if (handleFileNotFound(preview, file)) break;
        console.log(`files: ${preview.totalFiles} (${preview.totalVisited} visited, ${preview.totalFiles} after ranking)`);
        console.log(`symbols: ${preview.totalSymbols}`);
        console.log(`edges: ${preview.totalEdges}`);
        console.log(`\nUse --limit N (and --offset N) to paginate, or --count to get counts only.`);
        break;
      }

      const result = await idx.neighborhood({
        file, hops, maxFiles: maxFilesFlag,
        count: countMode, offset, limit: userLimit,
      });
      if (handleFileNotFound(result, file)) break;

      if (countMode) {
        console.log(`files: ${result.totalFiles} (${result.totalVisited} visited, ${result.totalFiles} after ranking)`);
        console.log(`symbols: ${result.totalSymbols}`);
        console.log(`edges: ${result.totalEdges}`);
      } else {
        if (result.total !== undefined && result.total > result.files.length) {
          console.log(`Files (${result.files.length} of ${result.total}):`);
        } else {
          console.log(`Files (${result.files.length}):`);
        }
        for (const f of result.files) console.log(`  ${f}`);

        if (result.edges.length > 0) {
          console.log(`\nImports (${result.edges.length}):`);
          for (const e of result.edges) console.log(`  ${e.source} → ${e.target}`);
        }

        if (result.symbols.length > 0) {
          console.log(`\nSymbols (${result.symbols.length}):`);
          const byFile = new Map();
          for (const s of result.symbols) {
            if (!byFile.has(s.file)) byFile.set(s.file, []);
            byFile.get(s.file).push(s);
          }
          for (const [f, syms] of byFile) {
            console.log(`  ${f}:`);
            for (const s of syms) {
              console.log(`    ${String(s.lineStart).padStart(4)}│ ${s.signature}`);
            }
          }
        }
      }
      break;
    }

    case 'orphans': {
      const level = flags.level || 'file';
      if (level !== 'file' && level !== 'symbol') {
        console.error(`Unknown level: "${level}". Use "file" or "symbol".`);
        process.exit(1);
      }
      const effectiveLimit = countMode ? undefined : (userLimit !== undefined ? userLimit : DEFAULT_LIMIT);
      const result = await idx.orphans({ level, kind: flags.kind, count: countMode, offset, limit: effectiveLimit });

      if (countMode) {
        console.log(`total: ${result.total}`);
      } else if (level === 'file') {
        for (const f of result) {
          console.log(`${f.file}  (${f.symbolCount} symbols)`);
        }
        if (result.length === 0) {
          console.log('(no orphan files found)');
        } else {
          const total = await idx.orphans({ level, count: true });
          if (result.length < total.total) {
            console.log(`\nShowing ${result.length} of ${total.total} orphan files (use --limit N for more)`);
          }
        }
      } else {
        // symbol level — group by file like map output
        const byFile = new Map();
        for (const s of result) {
          if (!byFile.has(s.file)) byFile.set(s.file, []);
          byFile.get(s.file).push(s);
        }
        for (const [file, syms] of byFile) {
          console.log(`${file}:`);
          for (const s of syms) {
            console.log(`  ${String(s.lineStart).padStart(4)}│ [${s.kind}] ${s.signature}`);
          }
          console.log('');
        }
        if (result.length === 0) {
          console.log('(no orphan symbols found)');
        } else {
          const total = await idx.orphans({ level, kind: flags.kind, count: true });
          if (result.length < total.total) {
            console.log(`Showing ${result.length} of ${total.total} orphan symbols across ${byFile.size} files (use --limit N for more)`);
          }
        }
      }
      break;
    }

    case 'reindex': {
      const t0 = Date.now();
      const result = await idx.reindex();
      const elapsed = Date.now() - t0;
      const st = await idx.stats();
      console.log(`Reindexed in ${elapsed}ms: ${st.files} files, ${st.symbols} symbols, ${st.edges} edges`);
      break;
    }

    case 'stats': {
      await idx._ensureReady();
      const st = await idx.stats();
      console.log(`Files:   ${st.files}`);
      console.log(`Symbols: ${st.symbols}`);
      console.log(`Edges:   ${st.edges}`);
      break;
    }

    default:
      console.error(`Unknown command: ${command}`);
      console.log(USAGE);
      process.exit(1);
  }
}

function parseFlags(args) {
  const flags = { _positional: [] };
  let i = 0;
  while (i < args.length) {
    if (args[i] === '-h') {
      flags.help = true;
      i++;
    } else if (args[i].startsWith('--')) {
      const key = args[i].substring(2);
      if (i + 1 < args.length && !args[i + 1].startsWith('--')) {
        flags[key] = args[i + 1];
        i += 2;
      } else {
        flags[key] = true;
        i++;
      }
    } else {
      flags._positional.push(args[i]);
      i++;
    }
  }
  return flags;
}

main().catch(err => {
  console.error(err);
  process.exit(1);
});
