---
name: code-index
description: Structural code index for codebase awareness. Produces Aider-style repo maps, symbol lookups, call-graph queries, and dependency analysis using tree-sitter and graphology. Use when orienting in a codebase, finding callers/dependents of a symbol, generating a repo map, or needing structural code context without reading every file.
---

# BetterRank (`@mishasinitcyn/betterrank`)

A CLI tool that parses source files with tree-sitter, builds a dependency graph with graphology + PageRank, and exposes structural queries. Use it instead of brute-force glob/grep/read-everything.

## Setup

```bash
npm install -g @mishasinitcyn/betterrank
```

## CLI Usage

All commands are run via:

```bash
betterrank <command> [--root <project-path>] [options]
```

**Always pass `--root` explicitly.** If omitted, defaults to `cwd` — running from the wrong directory (e.g. Desktop with nested repos) builds a completely different graph and produces wrong PageRank rankings. The CLI will print a stderr warning if `--root` is missing.

The index auto-initializes on first query and incrementally updates on subsequent queries. Cache lives at the platform cache directory (`~/Library/Caches/code-index/` on macOS, `~/.cache/code-index/` on Linux). One file per `--root`, keyed by path hash. Disposable, delete anytime. Override with `CODE_INDEX_CACHE_DIR` env var.

### Global Flags

All list-returning commands support these flags:

| Flag | Description |
|---|---|
| `--count` | Return counts only (no content). Use to preview size before fetching. |
| `--offset N` | Skip first N results |
| `--limit N` | Max results to return |

The `--count` → `--offset`/`--limit` pattern is the recommended workflow: check the size first, then paginate to control token spend.

## Commands

### `map` — Repo map (use this first)

Summary of the most structurally important definitions, ranked by PageRank. This is the primary orientation tool. Default limit is 50 symbols.

```bash
betterrank map --root /path/to/project
betterrank map --root /path/to/project --limit 100
betterrank map --root /path/to/project --focus src/auth/handlers.ts,src/api/login.ts
betterrank map --root /path/to/project --count
```

`--focus` biases ranking toward the listed files (comma-separated).

### `search` — Find symbols by name or signature

Substring search across symbol names **and** full function signatures (including parameter names and types). Results ranked by PageRank. This is the go-to when you know *what you're looking for* but not *where it lives*.

```bash
# Search symbol names
betterrank search encrypt_imp --root /path/to/project

# Search parameter names (matches inside function signatures)
betterrank search max_age --root /path/to/project

# Filter by kind
betterrank search imp --root /path/to/project --kind function

# Count matches first, then paginate
betterrank search imp --root /path/to/project --count
betterrank search imp --root /path/to/project --offset 0 --limit 10
```

Search is case-insensitive. It matches against both `name` and the full `signature` string (which includes param names, types, defaults, and return types for Python).

**Search strategy**: Use short, distinctive substrings. `imp` is better than `impUrl` (which might be a field name, not a symbol) or `impression` (too broad). The PageRank ranking handles noise — short queries cast a wider net, and the ranking puts the important hits first. Start with 3-4 character substrings and only get more specific if you get too many irrelevant results.

**Use `search` instead of Grep** when looking for functions/classes/types. It returns only definitions (not every mention in comments, tests, string literals), ranked by structural importance. A Grep for "imp" might return 120 lines across 16 files; `search imp` returns the 15 most important function definitions, each with its full signature.

### `structure` — File tree with symbol counts

```bash
betterrank structure --root /path/to/project
betterrank structure --root /path/to/project --depth 2
```

### `symbols` — List definitions

```bash
betterrank symbols --root /path/to/project
betterrank symbols --root /path/to/project --file src/auth/handlers.ts
betterrank symbols --root /path/to/project --kind function
betterrank symbols --root /path/to/project --count
betterrank symbols --root /path/to/project --offset 0 --limit 20
```

Kinds: `function`, `class`, `type`, `variable`, `namespace`, `import`

### `callers` — Who calls this symbol

```bash
betterrank callers authenticateUser --root /path/to/project
betterrank callers authenticateUser --root /path/to/project --count
```

### `deps` — What does this file import

```bash
betterrank deps src/auth/handlers.ts --root /path/to/project
betterrank deps src/auth/handlers.ts --root /path/to/project --count
```

### `dependents` — What imports this file

```bash
betterrank dependents src/auth/handlers.ts --root /path/to/project
betterrank dependents src/auth/handlers.ts --root /path/to/project --count
```

### `neighborhood` — Local subgraph around a file

The recommended workflow for exploring a file's context:

```bash
# 1. Check the size first
betterrank neighborhood src/auth/handlers.ts --root /path/to/project --count

# 2. Fetch the full neighborhood (default: 15 files max)
betterrank neighborhood src/auth/handlers.ts --root /path/to/project --hops 2 --limit 10

# 3. Or paginate through it
betterrank neighborhood src/auth/handlers.ts --root /path/to/project --offset 0 --limit 5
betterrank neighborhood src/auth/handlers.ts --root /path/to/project --offset 5 --limit 5
```

Options:
- `--hops N` (default 2): BFS depth along outgoing IMPORTS edges
- `--max-files N` (default 15): Max files to include. Direct neighbors (hop 1) are always included; further-hop files must rank high enough via PageRank.

Files are aggressively ranked by PageRank biased toward the starting file. The `--count` output distinguishes between total files visited by BFS and files that survived ranking, so you can see how much was filtered.

### `reindex` — Force full rebuild

```bash
betterrank reindex --root /path/to/project
```

Use after branch switches, large merges, or if results seem stale.

### `stats` — Index statistics

```bash
betterrank stats --root /path/to/project
```

## Supported Languages

JavaScript, TypeScript, Python, Rust, Go, Java, Ruby, C, C++, C#, PHP.

Extensions: `.js`, `.mjs`, `.cjs`, `.jsx`, `.ts`, `.tsx`, `.py`, `.rs`, `.go`, `.rb`, `.java`, `.c`, `.h`, `.cpp`, `.hpp`, `.cc`, `.cs`, `.php`

## When to Use Which Command

| Situation | Command |
|---|---|
| First time in a repo, need orientation | `map --limit 100` |
| Know what you're looking for but not where | `search <query>` |
| Looking for functions that take a specific param | `search <param_name>` |
| About to modify a file, need local context | `neighborhood <file> --count`, then `neighborhood <file> --hops 1 --limit 10` |
| Need to know what calls a function before changing it | `callers <symbol>` |
| Need to know what breaks if I change a file | `dependents <file>` |
| Need to understand a file's dependencies | `deps <file>` |
| Large result set, want to control token spend | `<command> --count`, then `--offset 0 --limit 10` |
| Results seem stale after git operations | `reindex` |

## Ignore Configuration

Built-in defaults ignore `node_modules`, `dist`, `build`, `tmp`, `Pods`, `.venv`, etc. To add project-specific ignores, create `<project-root>/.code-index/config.json`:

```json
{
  "ignore": ["experiments/**", "legacy/**"]
}
```

These are merged with the built-in defaults.

## Common Pitfalls

- **Wrong `--root` / no `--root`**: The #1 cause of unexpected rankings. PageRank is relative to the graph — different files indexed = different rankings. Always pass `--root` explicitly. The CLI warns on stderr when it falls back to `cwd`.
- **Stale cache after branch switch**: Run `reindex` after switching branches or pulling large changes.
- **Truncated output**: All commands default to 50 results. The `map` command shows a summary line (`Showing X of Y files`) when output is truncated. Use `--limit N` to show more, or `--count` to see totals.

## How It Works (brief)

1. **Parser**: Native tree-sitter (C bindings via N-API) extracts definitions (functions, classes, types) and references (calls, imports) from source files
2. **Graph**: graphology MultiDirectedGraph with typed edges (DEFINES, REFERENCES, IMPORTS). PageRank ranks symbols by structural importance.
3. **Cache**: mtime-based incremental updates. Only re-parses changed files. Serialized to platform cache dir (`~/Library/Caches/code-index/<hash>.json` on macOS).

## Programmatic API

The tool can also be used as a Node.js module:

```javascript
import { CodeIndex } from '@mishasinitcyn/betterrank';

const idx = new CodeIndex('/path/to/project');

// Orientation
const map = await idx.map({ limit: 100, focusFiles: ['src/main.ts'] });
const results = await idx.search({ query: 'auth', kind: 'function', limit: 10 });
const syms = await idx.symbols({ file: 'src/main.ts', kind: 'function' });

// Graph queries
const callers = await idx.callers({ symbol: 'authenticate' });
const deps = await idx.dependencies({ file: 'src/auth.ts' });
const dependents = await idx.dependents({ file: 'src/auth.ts' });
const hood = await idx.neighborhood({ file: 'src/auth.ts', hops: 2, maxFiles: 10 });

// Count-only (no content, just sizes)
const hoodSize = await idx.neighborhood({ file: 'src/auth.ts', count: true });
const callerCount = await idx.callers({ symbol: 'authenticate', count: true });

// Pagination
const page1 = await idx.symbols({ offset: 0, limit: 20 });
const page2 = await idx.symbols({ offset: 20, limit: 20 });

// Maintenance
const stats = await idx.stats();
await idx.reindex();
```
