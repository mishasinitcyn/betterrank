# BetterRank

Fast native Rust structural code indexer, call-graph analyzer, and PageRank-ranked bounded `ripgrep` CLI for coding agents.

Built on [`oxc_parser`](https://oxc.rs/) (JS/TS/TSX), [`tree-sitter`](https://tree-sitter.github.io/) (Python/Rust/Go & file outlines), [`ripgrep`](https://github.com/BurntSushi/ripgrep) library crates (`ignore` + `grep-searcher` + `grep-regex`), [`rayon`](https://github.com/rayon-rs/rayon), and zero-copy [`rkyv`](https://rkyv.org/) + `mmap` binary caches.

## Install

```bash
npm install -g @mishasinitcyn/betterrank
```

Prebuilt native binaries (~9 MB, zero npm runtime dependencies) are installed automatically for `linux-x64` (static musl), `darwin-arm64`, and `darwin-x64`.

## Quick Start

`--root` defaults automatically to the enclosing `.git` root (or current directory), and is repeatable for cross-repo analysis (`--root repo-a --root repo-b`). Global flags work before or after any subcommand.

```bash
# Ranked overview of the repo (excludes tests by default)
betterrank map
betterrank map --focus src/api/auth.ts --limit 50

# Compact 1-line-per-symbol file outline (imports collapsed by default)
betterrank outline src/auth.ts --annotate

# Expand specific symbols or line ranges (works on code & non-code files)
betterrank outline src/auth.ts authenticateUser,refreshToken --no-comments
betterrank outline prisma/schema.prisma 120-165,410-445

# PageRank-ranked, hard-bounded ripgrep with enclosing symbol labels & context
betterrank grep "sessionToken" -C 2
betterrank grep "DATABASE_URL" --non-code
betterrank grep "foo|bar|baz" --code --ext ts,py

# Path- and token-aware symbol + file search
betterrank search "team settings"

# List all indexed code + config files by prefix or glob
betterrank files src/api

# Ranked callers (production first, tests collapsed, merged context hunks)
betterrank callers authenticateUser --context
betterrank callers auth.ts:authenticateUser --context
betterrank callers fnA,fnB --path src/server

# Enriched file dependents (shows import line, used symbols, + Dockerfile/CI mentions)
betterrank dependents src/auth/session.ts

# Full symbol deep dive, upward/downward call trees, and git blast radius
betterrank context authenticateUser
betterrank trace authenticateUser --depth 3
betterrank callees authenticateUser --depth 3
betterrank diff
```

## How It Works

1. **Parse**: Parallel `oxc_parser` arena parsing for JS/TS/TSX and single-pass `tree-sitter` `TreeCursor` parsing for Python/Rust/Go, with LPT largest-first scheduling and per-root `.parse.brbin` caches locked via per-root lockfiles.
2. **Graph**: Parallel CSR-encoded directed graph (`DEFINES`, `IMPORTS`, `REFERENCES`) with workspace `package.json`/`tsconfig.json` & Python module resolution, barrel-only re-export reachability, and receiver-aware method/RPC disambiguation.
3. **Rank**: Sparse PageRank scores every file and symbol by structural importance.
4. **Zero-copy cache**: Uncompressed `rkyv` binary index (`.brbin`) `mmap`'d directly into memory on warm queries with zero deserialization, including cached non-code/config file paths (`Dockerfile*`, `.github/workflows/*.yml`, `*.prisma`, `*.sql`, `*.json`, `*.toml`, `*.sh`).
5. **Non-code awareness**: `callers`, `dependents`, and `search` automatically scan non-code/config files (opt out with `--no-noncode`), and `betterrank grep` provides PageRank-ordered, hard-bounded text search across all code and config surfaces.

## Supported Languages

- **First-class:** JavaScript, TypeScript, TSX, Python
- **Also supported:** Rust, Go
- **Non-code / config surfaces:** Dockerfiles, GitHub Actions YAML, Prisma schemas, SQL, JSON, TOML, YAML, shell scripts, GraphQL, Protobuf, Terraform

## Commands

| Command | Description |
| --- | --- |
| `outline <file> [sym1,L10-50,...]` | Compact 1-line-per-symbol skeleton (imports collapsed); pass symbol names or line ranges to expand (`--annotate`, `--no-comments`, `--depth N`, `--full`, `--show-imports`) |
| `map [--focus f1,f2]` | PageRank-ranked repo map (excludes tests by default; `--include-tests`, `--limit N`) |
| `grep <pattern>` | PageRank-ranked ripgrep with enclosing symbol ranges, `-C`/`-A`/`-B` context, test/fixture collapsing (`--include-tests`), `--code`, `--non-code`, `--ext`, `--type`, `-e`, and balanced multi-branch alternation counts |
| `files [dir-or-glob]` | List all indexed code + config files by prefix or glob (`--code`, `--non-code`, `--no-tests`) |
| `search <query>` | Path- and token-aware search across symbols, file paths, and non-code mentions (`--kind`, `--max-per-file`, `--include-tests`) |
| `callers <sym1[,sym2]>` | Ranked callers (prod first, merged `--context` hunks, supports `file.ts:sym`, `--defined-in`, `--path`, `--include-tests`) |
| `dependents <file>` | Code importers (with import line + used symbols) plus non-code/config mentions (`--include-tests`) |
| `deps <file>` | Files imported by `<file>` |
| `context <symbol>` | Full symbol source, incoming/outgoing refs, types, and callers (`--file`, `--no-source`) |
| `trace <symbol>` / `callees <symbol>` | Recursive upward caller tree / downward callee tree (`--depth N`) |
| `neighborhood <file>` | Local import/reference subgraph around a file (`--hops N`, `--max-files N`) |
| `diff [--ref <commit>]` | Git-aware blast radius of changed symbols with external caller counts |
| `history <symbol>` | Git log history of a specific symbol (`--patch`) |
| `structure [--depth N]` | File tree with per-file symbol counts |
| `symbols [--file path]` | Ranked definitions list (`--kind`) |
| `orphans [--level file\|symbol]` | Disconnected files or symbols |
| `reindex` / `stats` | Rebuild index or inspect index statistics |

## Global Flags

- `--root <path>` — Project root (repeatable for multi-root cross-repo queries; defaults to enclosing `.git` root)
- `--no-noncode` — Skip automatic non-code/config mention scan on `callers`, `dependents`, and `search`
- `--count` — Return counts only
- `--offset N` / `--limit N` — Pagination
- `--version`, `-v` / `--help`, `-h`

## Agent Rule Snippet

Drop this into your agent rules (`.cursor/rules/`, `CLAUDE.md`, etc.):

```markdown
# BetterRank — Codebase Navigation

Use `betterrank` as your default tool for codebase orientation, reading large files, impact analysis, and text/config searches.

1. Omit `--root` inside a single repo (auto-detects `.git`), or pass repeatable `--root <repo1> --root <repo2>` for cross-repo impact.
2. Start with `betterrank map`, `betterrank search <query>`, or `betterrank files <dir>`.
3. For large files, run `betterrank outline <file> --annotate` first, then expand only what you need (`betterrank outline <file> sym1,sym2` or `betterrank outline <file> 120-180`).
4. Check code + config blast radius with `betterrank callers <file.ts:symbol> --context`, `betterrank dependents <file>`, and `betterrank grep <pattern> -C 2` (`--non-code` / `--type dockerfile,ci,packaging` for infra).
5. Run `betterrank diff` before finishing edits.
```

## Building from Source

```bash
cargo build --release
cargo test --release
```

## License

MIT
