# BetterRank

Structural code index with PageRank-ranked repo maps, symbol search, call-graph queries, and dependency analysis. Built on [tree-sitter](https://tree-sitter.github.io/) and [graphology](https://graphology.github.io/).

BetterRank parses your source files, builds a dependency graph, and ranks every symbol by structural importance using PageRank. Instead of grepping through thousands of files, ask questions like "what are the most important functions in this project?" or "who calls this function?" and get ranked, structured answers in seconds.

## Install

```bash
npm install -g @mishasinitcyn/betterrank
```

## Quick Start

```bash
# Get a ranked overview of any project
betterrank map --root /path/to/project

# View a file's skeleton without reading the whole thing
betterrank outline src/auth.py

# Expand a specific function to see its full source
betterrank outline src/auth.py authenticate_user

# Search for symbols by name or parameter
betterrank search auth --root /path/to/project

# Who calls this function? (with call site context)
betterrank callers authenticateUser --root /path/to/project --context

# What depends on this file?
betterrank dependents src/auth/handlers.ts --root /path/to/project

# What does this file import?
betterrank deps src/auth/handlers.ts --root /path/to/project

# Local subgraph around a file
betterrank neighborhood src/auth/handlers.ts --root /path/to/project --limit 10
```

## How It Works

1. **Parse**: Native tree-sitter extracts definitions (functions, classes, types) and references (calls, imports) from source files
2. **Graph**: graphology MultiDirectedGraph with typed edges (DEFINES, REFERENCES, IMPORTS)
3. **Rank**: PageRank scores every symbol by structural importance — heavily-imported utilities rank higher than leaf files
4. **Cache**: mtime-based incremental updates. Only re-parses changed files. Cache lives at the platform cache directory (`~/Library/Caches/code-index/` on macOS, `~/.cache/code-index/` on Linux)

## Supported Languages

JavaScript, TypeScript, Python, Rust, Go, Java, Ruby, C, C++, C#, PHP

## Commands

### `outline` — File skeleton with collapsed bodies

View a file's structure without reading the entire thing. Function and class bodies are collapsed to `... (N lines)`. Expand specific symbols by name. **No `--root` required** — works on any file standalone.

```bash
# Skeleton view: imports, signatures, constants — bodies collapsed
betterrank outline src/auth.py

# Expand a specific function to see its full source
betterrank outline src/auth.py authenticate_user

# Expand multiple symbols
betterrank outline src/auth.py validate,process

# Show caller counts next to each function (requires --root)
betterrank outline src/auth.py --annotate --root ./backend

# Resolve path relative to a root
betterrank outline src/auth.py --root ./backend
```

**Example output (with `--annotate`):**
```
   1│ from fastapi import APIRouter, Depends
   2│ from core.auth import verify_auth
   3│
   4│ router = APIRouter(prefix="/api")
   5│
   6│ @router.get("/users")
   7│ async def list_users(db = Depends(get_db)):
    │   ... (25 lines)                                          ← 2 callers
  33│
  34│ @router.post("/users")
  35│ async def create_user(data: UserCreate, db = Depends(get_db)):
    │   ... (40 lines)                                          ← 5 callers
```

Typical compression: **3-5x** (a 2000-line file becomes ~400 lines of outline). Annotations show how many *external* files reference each function — instantly see what's critical vs dead code.

### `map` — Repo map

Summary of the most structurally important definitions, ranked by PageRank. Default limit is 50 symbols.

```bash
betterrank map --root /path/to/project
betterrank map --root /path/to/project --limit 100
betterrank map --root /path/to/project --focus src/api/auth.ts,src/api/login.ts
betterrank map --root /path/to/project --count
```

### `search` — Find symbols by name or signature

Substring search across symbol names **and** full function signatures (including parameter names and types). Results ranked by PageRank.

```bash
betterrank search encrypt --root /path/to/project
betterrank search max_age --root /path/to/project          # matches param names too
betterrank search imp --root /path/to/project --kind function
```

### `symbols` — List definitions

```bash
betterrank symbols --root /path/to/project
betterrank symbols --root /path/to/project --file src/auth.ts
betterrank symbols --root /path/to/project --kind function
```

### `callers` — Who calls this symbol

```bash
# File names only
betterrank callers authenticateUser --root /path/to/project

# With call site context lines (default 2 lines around each site)
betterrank callers authenticateUser --root /path/to/project --context

# Custom context window
betterrank callers authenticateUser --root /path/to/project --context 3
```

**Example output (with `--context`):**
```
src/engine/pipeline.py:
      16│ from app.engine.bidding import run_auction
      17│

     153│     return await run_auction(
     154│         searched=campaigns,
     155│         publisher=config.publisher,

src/api/serve.py:
     145│     bid = await run_auction(searched, publisher=pub_id)
```

### `deps` — What does this file import

```bash
betterrank deps src/auth.ts --root /path/to/project
```

### `dependents` — What imports this file

```bash
betterrank dependents src/auth.ts --root /path/to/project
```

### `neighborhood` — Local subgraph around a file

```bash
betterrank neighborhood src/auth.ts --root /path/to/project --count
betterrank neighborhood src/auth.ts --root /path/to/project --hops 2 --limit 10
```

### `orphans` — Find disconnected files/symbols

```bash
betterrank orphans --root /path/to/project                          # orphan files
betterrank orphans --level symbol --root /path/to/project           # orphan symbols
betterrank orphans --level symbol --kind function --root /path/to/project
```

### `structure` — File tree with symbol counts

```bash
betterrank structure --root /path/to/project --depth 3
```

### `reindex` — Force full rebuild

```bash
betterrank reindex --root /path/to/project
```

### `stats` — Index statistics

```bash
betterrank stats --root /path/to/project
```

## Global Flags

| Flag | Description |
|---|---|
| `--root <path>` | Project root. **Always pass this explicitly.** |
| `--count` | Return counts only (no content) |
| `--offset N` | Skip first N results |
| `--limit N` | Max results to return (default: 50) |

## Programmatic API

```javascript
import { CodeIndex } from '@mishasinitcyn/betterrank';

const idx = new CodeIndex('/path/to/project');

const map = await idx.map({ limit: 100, focusFiles: ['src/main.ts'] });
const results = await idx.search({ query: 'auth', kind: 'function', limit: 10 });
const callers = await idx.callers({ symbol: 'authenticate', context: 2 });
const counts = await idx.getCallerCounts('src/auth.ts');
const deps = await idx.dependencies({ file: 'src/auth.ts' });
const dependents = await idx.dependents({ file: 'src/auth.ts' });
const hood = await idx.neighborhood({ file: 'src/auth.ts', hops: 2, maxFiles: 10 });

const stats = await idx.stats();
await idx.reindex();
```

## Ignore Configuration

Built-in defaults ignore `node_modules`, `dist`, `build`, `.venv`, etc. To add project-specific ignores, create `<project-root>/.code-index/config.json`:

```json
{
  "ignore": ["experiments/**", "legacy/**"]
}
```

## Cache

Cache lives at the platform cache directory:
- **macOS**: `~/Library/Caches/code-index/`
- **Linux**: `~/.cache/code-index/`
- **Windows**: `%LOCALAPPDATA%/code-index/Cache/`

Override with `CODE_INDEX_CACHE_DIR` env var. Cache files are disposable — delete anytime, they rebuild automatically.

## License

MIT
