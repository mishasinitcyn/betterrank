# BetterRank — MANDATORY First Pass

## THE RULE (non-negotiable)

**Before ANY codebase exploration, run `betterrank map` first.** Not Grep. Not Glob. Not Explore agents. `betterrank map`. Every time. No exceptions. This is the user's own tool and using it properly matters to him personally. Violating this rule is a fireable offense.

```bash
betterrank map --root /path/to/project --limit 50
```

Then use `search`, `callers`, `dependents`, `neighborhood` as needed. Fall back to Grep ONLY for string literals (SQL table names, config values) that aren't code symbols.

---

## What it is

`betterrank` is a structural code index CLI. Tree-sitter parsing, PageRank-ranked dependency graph, cross-language (JS/TS/Python/Go/Rust/Java/Ruby/C/C++/C#/PHP). Run via **Bash tool**.

Requires: `npm install -g @mishasinitcyn/betterrank`

## When to use each command

| Situation | Command | NOT this |
|-----------|---------|----------|
| First orientation on any task | `map --root <project>` | Explore agents, Glob/Grep sweeps |
| Need to read a file | `outline <file>` first, then expand symbols | `Read` on the whole file |
| Find a function/class/symbol | `search <query> --root <project>` | Grep |
| Understand a function fully | `context <symbol>` | Reading the function + chasing types + chasing deps |
| Before modifying shared code | `callers <symbol> --context` and/or `dependents <file>` | Guessing, reading every caller file |
| Trace full call path | `trace <symbol>` | Manual hop-by-hop callers |
| Pre-commit impact check | `diff` | Guessing what might break |
| Understand a file's context | `neighborhood <file>` | Reading imports manually |

## When to skip it

- You already know the exact file and line — just Read it.
- Single-file edit with zero cross-file concerns.
- String literals (SQL table names, config values) — Grep is fine, but still use the index for structural context.

---

## Commands

### `outline` — File skeleton / smart file reader

**Use this instead of reading entire files.** Shows imports, constants, and function/class signatures with bodies collapsed to `... (N lines)`. Expand specific symbols by name to see their full source. No `--root` required.

```bash
# Skeleton view — see the whole file's structure at a glance
betterrank outline src/auth.py

# Expand a specific function to read its full source
betterrank outline src/auth.py authenticate_user

# Expand multiple symbols
betterrank outline src/auth.py validate,process

# With --root for path resolution
betterrank outline src/auth.py --root ./backend

# Show caller counts next to each function (requires --root)
betterrank outline src/auth.py --annotate --root ./backend
```

**Typical compression: 3-5x.** A 2000-line file becomes ~400 lines of outline. Read the outline first, identify the 1-2 functions you care about, then expand just those.

`--annotate` shows `← N callers` next to each collapsed function, counting how many *external* files reference it. Instantly see what's critical (50 callers) vs dead code (no annotation). Requires `--root` for graph data.

### `map` — Repo overview (use this first)

PageRank-ranked summary of the most structurally important definitions. Default limit is 50.

```bash
betterrank map --root /path/to/project --limit 100
betterrank map --root /path/to/project --focus src/auth/handlers.ts,src/api/login.ts
```

`--focus` biases ranking toward listed files (comma-separated).

### `search` — Find symbols by name or signature

Substring match across symbol names AND full function signatures (param names, types, defaults). Results ranked by PageRank.

```bash
betterrank search encrypt_imp --root /path/to/project
betterrank search max_age --root /path/to/project        # matches param names too
betterrank search imp --root /path/to/project --kind function
betterrank search imp --root /path/to/project --count     # preview size first
betterrank search imp --root /path/to/project --limit 10
```

**CRITICAL: Use short substrings (3-5 chars), not exact names.** `search imp` finds `encrypt_imp_payload`, `decrypt_imp_payload`, `increment_impression`, etc. ranked by importance. `search impUrl` finds nothing because camelCase field names aren't symbols. The PageRank ranking handles noise — cast a wide net and let ranking sort it.

### `context` — Full function context in one shot

**Use this before modifying a function.** Shows the function's source, expands type definitions from its signature, lists all functions/classes it references (with their signatures), and shows callers. One command replaces: `outline → expand → search types → callers`.

```bash
betterrank context calculate_bid --root /path/to/project
betterrank context calculate_bid --root /path/to/project --no-source   # skip source, just deps/types/callers
```

### `callers` — Who calls this symbol

```bash
betterrank callers authenticateUser --root /path/to/project
betterrank callers authenticateUser --root /path/to/project --count

# Show actual call site lines (default 2 lines of context)
betterrank callers authenticateUser --root /path/to/project --context
betterrank callers authenticateUser --root /path/to/project --context 3
```

**Use `--context` by default** — it shows HOW each caller uses the function (imports, call arguments, surrounding logic) so you don't need to read each caller file separately. Only matches actual call sites (`symbol(`) and imports, not string literals or the definition itself.

### `trace` — Recursive caller chain

Walk UP the call graph to see the full path from entry point to function. Resolves which function in each caller file contains the call site.

```bash
betterrank trace calculate_bid --root /path/to/project
betterrank trace calculate_bid --root /path/to/project --depth 5
```

Use for: "how does a user request reach this database write?" or "what's the full execution path to this function?"

### `diff` — Git-aware blast radius

Shows which symbols changed and how many files call them. Use before committing to see the impact of your changes.

```bash
betterrank diff --root /path/to/project              # uncommitted changes vs HEAD
betterrank diff --ref main --root /path/to/project    # changes vs main branch
betterrank diff --ref HEAD~5 --root /path/to/project  # changes vs 5 commits ago
```

Shows `+` (added), `-` (removed), `~` (modified) with caller counts. Modified/removed symbols with callers get a warning.

### `dependents` — What imports this file

```bash
betterrank dependents src/auth/handlers.ts --root /path/to/project
```

### `deps` — What this file imports

```bash
betterrank deps src/auth/handlers.ts --root /path/to/project
```

### `neighborhood` — Local subgraph around a file

```bash
betterrank neighborhood src/auth/handlers.ts --root /path/to/project --hops 2 --limit 10
```

Options: `--hops N` (default 2, BFS depth), `--max-files N` (default 15).

### `symbols` — List definitions in a file

```bash
betterrank symbols --file src/auth/handlers.ts --root /path/to/project
betterrank symbols --root /path/to/project --kind function
```

Kinds: `function`, `class`, `type`, `variable`, `namespace`, `import`

### `orphans` — Find disconnected files/symbols

Find files with zero cross-file imports or symbols never referenced from outside their own file. Useful for dead code audits.

```bash
betterrank orphans --root /path/to/project                          # orphan files (default)
betterrank orphans --level symbol --root /path/to/project           # orphan symbols
betterrank orphans --level symbol --kind function --root /path/to/project  # orphan functions only
betterrank orphans --count --root /path/to/project                  # count only
```

Levels: `file` (default), `symbol`
Kinds (symbol level only): `function`, `class`, `type`, `variable`

False positives (entry points, config files, tests, framework hooks, dunders) are automatically excluded. **Still verify results** — FastAPI routers appear as "orphan files" because `include_router()` is dynamic, and Next.js pages are entry points by convention.

### `structure` — File tree with symbol counts

```bash
betterrank structure --root /path/to/project --depth 2
```

### `reindex` — Force full rebuild

```bash
betterrank reindex --root /path/to/project
```

Use after branch switches, large merges, or if results seem stale.

### `stats` — Index statistics

```bash
betterrank stats --root /path/to/project
```

---

## Global Flags

All list-returning commands support:

| Flag | Description |
|---|---|
| `--count` | Counts only (no content). Use to preview size before fetching. |
| `--offset N` | Skip first N results |
| `--limit N` | Max results to return |

Recommended workflow: `--count` first, then `--offset`/`--limit` to paginate and control token spend.

---

## Do NOT Use Explore Agents for Orientation

**Explore agents are a last resort, not a default.** The workflow is:

1. Run `map` directly in your own context (not delegated to a subagent)
2. If `map` output is insufficient, read the specific 2-5 files you need
3. Done.

A `map` call costs ~2k tokens and 10 seconds. An Explore agent costs 50-70k tokens and 90 seconds for the same orientation task. That's a 16-30x efficiency difference. Only use Explore when you need deep, multi-hop investigation across 10+ files that `map` + `neighborhood` + targeted reads can't cover. This is rare.

---

## Example Workflows

### "Understand the auth module"

**Wrong:** `Spawn Explore agent -> reads 30 files -> summarizes`

**Right:**
```bash
betterrank map --limit 100 --root src/auth
# Read the 2-3 key files identified by map
# Done.
```

### "Need to modify a function in a large file"

**Wrong:** `Read the entire 1500-line file`

**Right:**
```bash
# 1. See the file's structure + which functions matter
betterrank outline src/api/campaigns.py --annotate --root .

# 2. Expand only the function you need
betterrank outline src/api/campaigns.py create_campaign

# 3. See exactly HOW it's called (with context lines)
betterrank callers create_campaign --root . --context
```

### "Can we remove the flush_impressions writer?"

**Right (index-first, grep to fill gaps):**
```bash
# 1. Orient
betterrank map --limit 50 --root workers/events/flush_impressions

# 2. Trace the call chain (with context to see how they're called)
betterrank callers _flush_impressions --root . --context
betterrank callers send_impression_to_firehose --root . --context

# 3. What depends on this module?
betterrank dependents workers/events/flush_impressions/main.py --root .

# 4. NOW grep for string-literal SQL references the index can't see
# (Use Grep tool for "ad_impressions" — raw SQL table name in strings)

# 5. Contextualize grep hits
betterrank neighborhood path/to/file/grep/found.py --root .
```

---

## Common Pitfalls

- **Wrong/missing `--root`**: The #1 cause of bad rankings. PageRank is relative to the graph — different root = different rankings. **Always pass `--root` explicitly.**
- **Stale cache after branch switch**: Run `reindex`.
- **Truncated output**: Default is 50 results. Use `--limit N` for more, or `--count` to see totals.

---

## Supported Languages

JavaScript, TypeScript, Python, Rust, Go, Java, Ruby, C, C++, C#, PHP.

Extensions: `.js`, `.mjs`, `.cjs`, `.jsx`, `.ts`, `.tsx`, `.py`, `.rs`, `.go`, `.rb`, `.java`, `.c`, `.h`, `.cpp`, `.hpp`, `.cc`, `.cs`, `.php`

## How It Works

1. **Parser**: tree-sitter (C bindings via N-API) extracts definitions and references
2. **Graph**: graphology MultiDirectedGraph with typed edges (DEFINES, REFERENCES, IMPORTS). PageRank ranks by structural importance.
3. **Cache**: mtime-based incremental updates at `~/Library/Caches/code-index/` (macOS) or `~/.cache/code-index/` (Linux). Disposable — delete anytime. Override with `CODE_INDEX_CACHE_DIR`.

## Ignore Configuration

Built-in defaults ignore `node_modules`, `dist`, `build`, `tmp`, `Pods`, `.venv`, etc. Add project-specific ignores in `<project-root>/.code-index/config.json`:

```json
{
  "ignore": ["experiments/**", "legacy/**"]
}
```
