use betterrank::cache::CodeIndex;
use betterrank::noncode::{ranked_grep, GrepOpts};
use betterrank::outline::{build_outline, parse_line_range, OutlineOpts};
use betterrank::query::{
    cmd_callees, cmd_callers, cmd_context, cmd_deps, cmd_dependents, cmd_diff, cmd_history,
    cmd_map, cmd_neighborhood, cmd_orphans, cmd_search, cmd_stats, cmd_structure, cmd_symbols,
    cmd_trace, get_caller_counts_for_file,
};
use betterrank::walk::{find_default_root, normalize_slashes};
use rustc_hash::FxHashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

const DEFAULT_LIMIT: usize = 50;
const DEFAULT_DEPTH: usize = 3;

const USAGE: &str = "\
betterrank <command> [options]

Commands:
  outline     <file> [symbol1,symbol2] [--annotate]  File skeleton (--annotate, --lines, --no-comments, --depth)
  map         [--focus file1,file2] [--include-tests] Repo map (ranked by PageRank, excludes tests by default)
  search      <query> [--kind type]                 Substring search on symbols + non-code config mentions
  grep        <pattern> [--non-code] [--no-tests]   PageRank-ranked, bounded ripgrep across ALL files (code + configs)
  structure   [--depth N]                           File tree with symbol counts (default depth: 3)
  symbols     [--file path] [--kind type]           List definitions (ranked by PageRank)
  callers     <symbol> [--file path] [--context]     All call sites + non-code config mentions (ranked)
  context     <symbol> [--file path]                 Full context: source, deps, types, callers
  history     <symbol> [--file path]                Git history of a specific function
  trace       <symbol> [--depth N]                  Recursive caller chain (upward)
  callees     <symbol> [--depth N]                  Recursive callee chain (downward)
  diff        [--ref <commit>]                      Git-aware blast radius (changed symbols + callers)
  deps        <file>                                What this file imports (ranked)
  dependents  <file>                                What imports this file + non-code config mentions (ranked)
  neighborhood <file> [--hops N] [--max-files N]    Local subgraph (ranked by PageRank)
  orphans     [--level file|symbol] [--kind type]   Find disconnected files/symbols
  reindex                                           Force full rebuild
  stats                                             Index statistics

Global flags:
  --root <path>     Project root (repeatable for multi-root; defaults to enclosing .git root)
  --no-noncode      Skip automatic non-code/config mention scan on callers/dependents/search
  --count           Return counts only (no content)
  --offset N        Skip first N results
  --limit N         Max results to return (default: 50 for list commands)
  --version, -v     Print version and exit
  --help, -h        Show help for a command";

#[derive(Default)]
struct Flags {
    positional: Vec<String>,
    roots: Vec<String>,
    values: FxHashMap<String, String>,
    bools: rustc_hash::FxHashSet<String>,
}

impl Flags {
    fn has(&self, k: &str) -> bool {
        self.bools.contains(k) || self.values.contains_key(k)
    }
    fn get(&self, k: &str) -> Option<&str> {
        self.values.get(k).map(|s| s.as_str())
    }
    fn get_usize(&self, k: &str) -> Option<usize> {
        self.values.get(k).and_then(|s| s.parse().ok())
    }
}

fn parse_flags(args: &[String]) -> Flags {
    let mut f = Flags::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "-h" | "--help" => {
                f.bools.insert("help".to_string());
                i += 1;
            }
            "-v" | "--version" => {
                f.bools.insert("version".to_string());
                i += 1;
            }
            "-p" => {
                f.bools.insert("patch".to_string());
                i += 1;
            }
            "-i" => {
                f.bools.insert("case-insensitive".to_string());
                i += 1;
            }
            "-F" => {
                f.bools.insert("fixed-strings".to_string());
                i += 1;
            }
            "-l" => {
                f.bools.insert("files-only".to_string());
                i += 1;
            }
            s if s.starts_with("--") => {
                let key = &s[2..];
                // Known boolean-only flags never consume the next positional argument
                let is_bool_flag = matches!(
                    key,
                    "count"
                        | "annotate"
                        | "no-comments"
                        | "collapse-imports"
                        | "include-tests"
                        | "no-tests"
                        | "no-source"
                        | "no-noncode"
                        | "non-code"
                        | "files-only"
                        | "case-insensitive"
                        | "fixed-strings"
                        | "patch"
                );
                if !is_bool_flag && i + 1 < args.len() && !args[i + 1].starts_with('-') {
                    // Special case: `--context` only consumes next arg if it parses as an integer
                    if key == "context" && args[i + 1].parse::<usize>().is_err() {
                        f.bools.insert(key.to_string());
                        i += 1;
                        continue;
                    }
                    let val = args[i + 1].clone();
                    if key == "root" {
                        f.roots.push(val.clone());
                    }
                    f.values.insert(key.to_string(), val);
                    i += 2;
                } else {
                    f.bools.insert(key.to_string());
                    i += 1;
                }
            }
            _ => {
                f.positional.push(arg.clone());
                i += 1;
            }
        }
    }
    f
}

fn resolve_roots(flags: &Flags) -> Vec<PathBuf> {
    if !flags.roots.is_empty() {
        flags.roots.iter().map(PathBuf::from).collect()
    } else {
        let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        vec![find_default_root(&cwd)]
    }
}

fn normalize_graph_file_arg(idx: &CodeIndex, raw: &str) -> String {
    let norm = normalize_slashes(raw);
    if idx.is_multi_root() {
        for r in &idx.roots {
            if norm.starts_with(&format!("{}/", r.prefix)) {
                return norm;
            }
        }
        if let Ok(abs) = fs::canonicalize(raw) {
            for r in &idx.roots {
                if let Ok(rel) = abs.strip_prefix(&r.path) {
                    return format!("{}/{}", r.prefix, normalize_slashes(&rel.to_string_lossy()));
                }
            }
        }
        return norm;
    }

    if let Ok(abs) = fs::canonicalize(raw) {
        if let Ok(rel) = abs.strip_prefix(&idx.roots[0].path) {
            return normalize_slashes(&rel.to_string_lossy());
        }
    }
    norm
}

fn print_command_help(cmd: &str) {
    match cmd {
        "map" => println!("betterrank map [--focus file1,file2] [--include-tests] [--limit N] [--root <path>]\n\nOptions:\n  --focus <files>   Comma-separated files to bias ranking\n  --include-tests   Include test/spec files (excluded by default)\n  --count           Return total symbol count only\n  --offset N        Skip first N symbols\n  --limit N         Max symbols to return (default: 50)"),
        "callers" => println!("betterrank callers <symbol> [--file path] [--context [N]] [--no-tests] [--no-noncode] [--root <path>]\n\nOptions:\n  --file <path>    Disambiguate duplicate symbol names\n  --context [N]    Show N lines of context around call sites (default: 2)\n  --no-tests       Exclude test/spec files\n  --no-noncode     Skip automatic non-code/config mention scan\n  --count          Return count only"),
        "grep" => println!("betterrank grep <pattern> [--non-code] [--no-tests] [-l] [-i] [-F] [--path sub] [--limit N] [--max-per-file N] [--root <path>]\n\nPageRank-ranked, hard-bounded ripgrep across ALL files (code + Dockerfiles + CI YAML + Prisma + SQL + JSON).\n\nOptions:\n  --non-code         Search only non-code/config/infra/schema files\n  --no-tests         Exclude test/spec files\n  -l, --files-only   List matching file paths only\n  -i                 Case-insensitive search\n  -F                 Fixed-string literal search (no regex)\n  --path <substr>    Restrict to files whose path contains <substr>\n  --limit N          Max matching files (default: 30)\n  --max-per-file N   Max matching lines per file (default: 5)"),
        "outline" => println!("betterrank outline <file> [symbol1,symbol2] [--annotate --lines A-B --no-comments --depth N --collapse-imports --root <path>]"),
        _ => println!("{USAGE}"),
    }
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        println!("{USAGE}");
        return;
    }
    if args[0] == "--version" || args[0] == "-v" {
        println!("{}", betterrank::version());
        return;
    }

    let command = args[0].as_str();
    let flags = parse_flags(&args[1..]);

    if flags.has("help") {
        print_command_help(command);
        return;
    }

    if command == "outline" {
        let Some(file_arg) = flags.positional.first() else {
            eprintln!("Usage: betterrank outline <file> [symbol1,symbol2] [--annotate --lines A-B --no-comments --depth N --root <path>]");
            process::exit(1);
        };
        let expand_syms: Vec<String> = flags
            .positional
            .get(1)
            .map(|s| s.split(',').map(|x| x.to_string()).collect())
            .unwrap_or_default();
        let annotate = flags.has("annotate");
        let no_comments = flags.has("no-comments");
        let collapse_imports = flags.has("collapse-imports");
        let explicit_depth = flags.get("depth").is_some();
        let depth = flags.get_usize("depth").unwrap_or(1) as u32;
        let line_range = match flags.get("lines") {
            Some(spec) => match parse_line_range(spec) {
                Some(r) => Some(r),
                None => {
                    eprintln!("Invalid --lines range \"{spec}\". Expected START-END or START:END");
                    process::exit(1);
                }
            },
            None => None,
        };

        let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let raw_roots: Vec<PathBuf> = if !flags.roots.is_empty() {
            flags.roots.iter().map(PathBuf::from).collect()
        } else if annotate {
            vec![find_default_root(&cwd)]
        } else {
            vec![cwd.clone()]
        };
        let roots: Vec<PathBuf> = raw_roots
            .iter()
            .map(|r| fs::canonicalize(r).unwrap_or_else(|_| r.clone()))
            .collect();

        let p = Path::new(file_arg);
        let candidates: Vec<PathBuf> = if p.is_absolute() {
            vec![p.to_path_buf()]
        } else {
            let mut c = vec![roots[0].join(p), cwd.join(p)];
            for r in roots.iter().skip(1) {
                c.push(r.join(p));
            }
            c
        };

        let mut source_opt = None;
        let mut abs_path = candidates[0].clone();
        for cand in candidates {
            if let Ok(s) = fs::read_to_string(&cand) {
                abs_path = fs::canonicalize(&cand).unwrap_or(cand);
                source_opt = Some(s);
                break;
            }
        }
        let Some(source) = source_opt else {
            eprintln!("Cannot read file: {}", abs_path.display());
            process::exit(1);
        };

        let matched_root = roots
            .iter()
            .find(|r| abs_path.strip_prefix(r).is_ok())
            .unwrap_or(&roots[0]);
        let rel_path = normalize_slashes(
            &abs_path
                .strip_prefix(matched_root)
                .unwrap_or(&abs_path)
                .to_string_lossy(),
        );

        let caller_counts = if annotate {
            let idx = CodeIndex::new(&roots);
            match idx.ensure() {
                Ok(mmap) => {
                    let gpath = if idx.is_multi_root() {
                        let pfx = idx
                            .roots
                            .iter()
                            .find(|r| &r.path == matched_root)
                            .map(|r| r.prefix.as_str())
                            .unwrap_or("");
                        format!("{pfx}/{rel_path}")
                    } else {
                        rel_path.clone()
                    };
                    Some(get_caller_counts_for_file(mmap.archived(), &gpath))
                }
                Err(_) => None,
            }
        } else {
            None
        };

        let out = build_outline(
            &source,
            &rel_path,
            &expand_syms,
            OutlineOpts {
                caller_counts: caller_counts.as_ref(),
                line_range,
                no_comments,
                depth,
                explicit_depth,
                collapse_imports,
            },
        );
        println!("{out}");
        return;
    }

    let raw_roots = resolve_roots(&flags);
    let idx = CodeIndex::new(&raw_roots);
    let count_only = flags.has("count");
    let offset = flags.get_usize("offset").unwrap_or(0);
    let user_limit = flags.get_usize("limit");
    let limit = user_limit.unwrap_or(DEFAULT_LIMIT);
    let include_noncode = !flags.has("no-noncode");

    // `grep` works even without triggering a cold index (uses warm mmap cache if present for PageRank!)
    if command == "grep" {
        let Some(pattern) = flags.positional.first() else {
            eprintln!("Usage: betterrank grep <pattern> [--non-code] [--no-tests] [-l] [-i] [-F] [--limit N]");
            process::exit(1);
        };
        let warm = betterrank::graph::MmapGraphCache::open(&idx.cache_path);
        let opts = GrepOpts {
            case_insensitive: flags.has("case-insensitive"),
            fixed_strings: flags.has("fixed-strings"),
            noncode_only: flags.has("non-code"),
            no_tests: flags.has("no-tests"),
            files_only: flags.has("files-only"),
            limit_files: user_limit.unwrap_or(30),
            max_per_file: flags.get_usize("max-per-file").unwrap_or(5),
            path_filter: flags.get("path").map(|s| s.to_string()),
        };
        match ranked_grep(&idx.roots, pattern, warm.as_ref().map(|m| m.archived()), &opts) {
            Ok((hits, total_files, total_hits)) => {
                if count_only {
                    println!("files: {total_files}");
                    println!("matches: {total_hits}");
                    return;
                }
                if hits.is_empty() {
                    println!("(no matches for \"{pattern}\")");
                    return;
                }
                for fh in &hits {
                    if opts.files_only {
                        println!("{}  ({} matches)", fh.file, fh.total_hits_in_file);
                    } else {
                        println!("{}:", fh.file);
                        for h in &fh.hits {
                            println!("  {:>5}│ {}", h.line, h.content);
                        }
                        if fh.total_hits_in_file > fh.hits.len() {
                            println!(
                                "        ... (+{} more in file)",
                                fh.total_hits_in_file - fh.hits.len()
                            );
                        }
                    }
                }
                if total_files > hits.len() {
                    println!(
                        "\nShowing {} of {total_files} files ({total_hits} total matches; ranked by PageRank). Use --limit N or --path <subpath> to narrow.",
                        hits.len()
                    );
                }
            }
            Err(e) => {
                eprintln!("{e}");
                process::exit(1);
            }
        }
        return;
    }

    if command == "reindex" {
        let t0 = std::time::Instant::now();
        match idx.reindex() {
            Ok(mmap) => {
                let g = mmap.archived();
                let files = g
                    .files
                    .iter()
                    .filter(|f| f.symbol_count.to_native() >= 0)
                    .count();
                println!(
                    "Reindexed in {}ms: {} files, {} symbols, {} edges",
                    t0.elapsed().as_millis(),
                    files,
                    g.symbols.len(),
                    g.total_edges.to_native()
                );
            }
            Err(e) => {
                eprintln!("{e}");
                process::exit(1);
            }
        }
        return;
    }

    let mmap = match idx.ensure() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Index error: {e}");
            process::exit(1);
        }
    };
    let g = mmap.archived();

    match command {
        "stats" => cmd_stats(g),
        "map" => {
            let focus: Vec<String> = flags
                .get("focus")
                .map(|s| {
                    s.split(',')
                        .map(|x| x.trim())
                        .filter(|x| !x.is_empty())
                        .map(|x| normalize_graph_file_arg(&idx, x))
                        .collect()
                })
                .unwrap_or_default();
            cmd_map(
                g,
                &focus,
                flags.has("include-tests"),
                count_only,
                offset,
                limit,
            );
        }
        "structure" => {
            let explicit = flags.get("depth").is_some();
            let depth = flags.get_usize("depth").unwrap_or(DEFAULT_DEPTH);
            cmd_structure(g, depth, explicit);
        }
        "search" => {
            let Some(q) = flags.positional.first() else {
                eprintln!("Usage: betterrank search <query> [--kind type]");
                process::exit(1);
            };
            cmd_search(
                &idx,
                g,
                q,
                flags.get("kind"),
                count_only,
                offset,
                limit,
                user_limit.is_some(),
                include_noncode,
            );
        }
        "symbols" => {
            let f_arg = flags
                .get("file")
                .map(|f| normalize_graph_file_arg(&idx, f));
            cmd_symbols(
                g,
                f_arg.as_deref(),
                flags.get("kind"),
                count_only,
                offset,
                limit,
                user_limit.is_some(),
            );
        }
        "callers" => {
            let Some(sym) = flags.positional.first() else {
                eprintln!("Usage: betterrank callers <symbol> [--file path]");
                process::exit(1);
            };
            let f_arg = flags
                .get("file")
                .map(|f| normalize_graph_file_arg(&idx, f));
            let ctx_lines = if let Some(n) = flags.get_usize("context") {
                n
            } else if flags.bools.contains("context") {
                2
            } else {
                0
            };
            cmd_callers(
                &idx,
                g,
                sym,
                f_arg.as_deref(),
                ctx_lines,
                flags.has("no-tests"),
                count_only,
                offset,
                limit,
                user_limit.is_some(),
                include_noncode,
            );
        }
        "context" => {
            let Some(sym) = flags.positional.first() else {
                eprintln!("Usage: betterrank context <symbol> [--file path]");
                process::exit(1);
            };
            let f_arg = flags
                .get("file")
                .map(|f| normalize_graph_file_arg(&idx, f));
            cmd_context(&idx, g, sym, f_arg.as_deref(), flags.has("no-source"));
        }
        "trace" => {
            let Some(sym) = flags.positional.first() else {
                eprintln!("Usage: betterrank trace <symbol> [--depth N]");
                process::exit(1);
            };
            let f_arg = flags
                .get("file")
                .map(|f| normalize_graph_file_arg(&idx, f));
            let depth = flags.get_usize("depth").unwrap_or(3);
            cmd_trace(&idx, g, sym, f_arg.as_deref(), depth);
        }
        "callees" => {
            let Some(sym) = flags.positional.first() else {
                eprintln!("Usage: betterrank callees <symbol> [--depth N]");
                process::exit(1);
            };
            let f_arg = flags
                .get("file")
                .map(|f| normalize_graph_file_arg(&idx, f));
            let depth = flags.get_usize("depth").unwrap_or(3);
            cmd_callees(&idx, g, sym, f_arg.as_deref(), depth);
        }
        "history" => {
            let Some(sym) = flags.positional.first() else {
                eprintln!("Usage: betterrank history <symbol> [--file path] [--patch]");
                process::exit(1);
            };
            let f_arg = flags
                .get("file")
                .map(|f| normalize_graph_file_arg(&idx, f));
            let hist_limit = user_limit.unwrap_or(20);
            cmd_history(
                &idx,
                g,
                sym,
                f_arg.as_deref(),
                offset,
                hist_limit,
                flags.has("patch"),
            );
        }
        "diff" => {
            let git_ref = flags.get("ref").unwrap_or("HEAD");
            cmd_diff(&idx, g, git_ref);
        }
        "deps" => {
            let Some(raw_f) = flags.positional.first() else {
                eprintln!("Usage: betterrank deps <file>");
                process::exit(1);
            };
            let f = normalize_graph_file_arg(&idx, raw_f);
            cmd_deps(g, &f, count_only, offset, limit, user_limit.is_some());
        }
        "dependents" => {
            let Some(raw_f) = flags.positional.first() else {
                eprintln!("Usage: betterrank dependents <file>");
                process::exit(1);
            };
            let f = normalize_graph_file_arg(&idx, raw_f);
            cmd_dependents(
                &idx,
                g,
                &f,
                count_only,
                offset,
                limit,
                user_limit.is_some(),
                include_noncode,
            );
        }
        "neighborhood" => {
            let Some(raw_f) = flags.positional.first() else {
                eprintln!("Usage: betterrank neighborhood <file> [--hops N] [--max-files N]");
                process::exit(1);
            };
            let f = normalize_graph_file_arg(&idx, raw_f);
            let hops = flags.get_usize("hops").unwrap_or(2) as u8;
            let max_files = flags.get_usize("max-files").unwrap_or(15);
            let max_symbols = flags.get_usize("max-symbols").unwrap_or(60);
            let eff_limit = user_limit.unwrap_or(max_files);
            cmd_neighborhood(
                g,
                &f,
                hops,
                max_files,
                max_symbols,
                count_only,
                offset,
                eff_limit,
            );
        }
        "orphans" => {
            let level = flags.get("level").unwrap_or("file");
            if level != "file" && level != "symbol" {
                eprintln!("Unknown level: \"{level}\". Use \"file\" or \"symbol\".");
                process::exit(1);
            }
            cmd_orphans(g, level, flags.get("kind"), count_only, offset, limit);
        }
        _ => {
            eprintln!("Unknown command: {command}");
            println!("{USAGE}");
            process::exit(1);
        }
    }
}
