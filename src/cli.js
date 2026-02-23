#!/usr/bin/env node

import { CodeIndex } from './index.js';
import { resolve, relative, isAbsolute } from 'path';

const DEFAULT_LIMIT = 50;
const DEFAULT_DEPTH = 3;

const USAGE = `
betterrank <command> [options]

Commands:
  ui          [--port N]                            Launch web UI (default port: 3333)
  map         [--focus file1,file2]                 Repo map (ranked by PageRank)
  search      <query> [--kind type]                 Substring search on symbol names + signatures (ranked by PageRank)
  structure   [--depth N]                           File tree with symbol counts (default depth: ${DEFAULT_DEPTH})
  symbols     [--file path] [--kind type]           List definitions (ranked by PageRank)
  callers     <symbol> [--file path]                All call sites (ranked by importance)
  deps        <file>                                What this file imports (ranked)
  dependents  <file>                                What imports this file (ranked)
  neighborhood <file> [--hops N] [--max-files N]    Local subgraph (ranked by PageRank)
  reindex                                           Force full rebuild
  stats                                             Index statistics

Global flags:
  --root <path>     Project root (default: cwd). Always pass this explicitly.
  --count           Return counts only (no content)
  --offset N        Skip first N results
  --limit N         Max results to return (default: ${DEFAULT_LIMIT} for list commands)
`.trim();

async function main() {
  const args = process.argv.slice(2);
  if (args.length === 0 || args[0] === '--help' || args[0] === '-h') {
    console.log(USAGE);
    process.exit(0);
  }

  const command = args[0];
  const flags = parseFlags(args.slice(1));

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
          console.log('(no matches)');
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
      const result = await idx.callers({ symbol, file: normalizeFilePath(flags.file), count: countMode, offset, limit: effectiveLimit });
      if (countMode) {
        console.log(`total: ${result.total}`);
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
      if (countMode) {
        console.log(`total: ${result.total}`);
      } else {
        for (const d of result) console.log(d);
        if (result.length === 0) {
          console.log('(no dependencies)');
        } else if (result.length === effectiveLimit && userLimit === undefined) {
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
      if (countMode) {
        console.log(`total: ${result.total}`);
      } else {
        for (const d of result) console.log(d);
        if (result.length === 0) {
          console.log('(no dependents)');
        } else if (result.length === effectiveLimit && userLimit === undefined) {
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
    if (args[i].startsWith('--')) {
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
