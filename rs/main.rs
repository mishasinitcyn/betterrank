use betterrank::cache::CodeIndex;
use betterrank::noncode::{ranked_grep, GrepOpts};
use betterrank::outline::{build_outline, parse_line_range, OutlineOpts};
use betterrank::query::{
    cmd_callees, cmd_callers, cmd_context, cmd_dependents, cmd_deps, cmd_diff, cmd_files,
    cmd_history, cmd_map, cmd_neighborhood, cmd_orphans, cmd_search, cmd_stats, cmd_structure,
    cmd_symbols, cmd_trace, get_caller_counts_for_file,
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
  outline     <file> [sym1,L10-50,...] [--annotate]  Compact symbol skeleton (collapses imports); pass symbols or line ranges to expand
  map         [--focus file1,file2] [--include-tests] Repo map (ranked by PageRank, excludes tests by default)
  files       [dir-or-glob] [--code|--non-code]     List all indexed code + config files by path/glob
  search      <query> [--kind type]                 Path- & token-aware search on symbols, files, + non-code mentions
  grep        <pattern> [-C N] [--code|--non-code]  PageRank-ranked ripgrep with enclosing symbols & balanced alternations
  structure   [--depth N]                           File tree with symbol counts (default depth: 3)
  symbols     [--file path] [--kind type]           List definitions (ranked by PageRank)
  callers     <sym1[,sym2]> [--file path] [--context] Ranked callers (prod first, merged context, supports file.ts:sym)
  context     <symbol> [--file path]                 Full context: source, deps, types, callers
  history     <symbol> [--file path]                Git history of a specific function
  trace       <symbol> [--depth N]                  Recursive caller chain (upward)
  callees     <symbol> [--depth N]                  Recursive callee chain (downward)
  diff        [--ref <commit>]                      Git-aware blast radius (changed symbols + callers)
  deps        <file>                                What this file imports (ranked)
  dependents  <file>                                What imports this file (with import lines & symbols) + non-code mentions
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
    extra_patterns: Vec<String>,
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
            "-C" => {
                if i + 1 < args.len() && args[i + 1].parse::<usize>().is_ok() {
                    f.values.insert("context".to_string(), args[i + 1].clone());
                    i += 2;
                } else {
                    f.values.insert("context".to_string(), "2".to_string());
                    i += 1;
                }
            }
            s if s.starts_with("-C") && s[2..].parse::<usize>().is_ok() => {
                f.values.insert("context".to_string(), s[2..].to_string());
                i += 1;
            }
            "-A" => {
                if i + 1 < args.len() && args[i + 1].parse::<usize>().is_ok() {
                    f.values.insert("after-context".to_string(), args[i + 1].clone());
                    i += 2;
                } else {
                    i += 1;
                }
            }
            s if s.starts_with("-A") && s[2..].parse::<usize>().is_ok() => {
                f.values.insert("after-context".to_string(), s[2..].to_string());
                i += 1;
            }
            "-B" => {
                if i + 1 < args.len() && args[i + 1].parse::<usize>().is_ok() {
                    f.values.insert("before-context".to_string(), args[i + 1].clone());
                    i += 2;
                } else {
                    i += 1;
                }
            }
            s if s.starts_with("-B") && s[2..].parse::<usize>().is_ok() => {
                f.values.insert("before-context".to_string(), s[2..].to_string());
                i += 1;
            }
            "-e" => {
                if i + 1 < args.len() {
                    f.extra_patterns.push(args[i + 1].clone());
                    i += 2;
                } else {
                    i += 1;
                }
            }
            s if s.starts_with("--") => {
                let raw = &s[2..];
                if let Some((k, v)) = raw.split_once('=') {
                    if k == "root" {
                        f.roots.push(v.to_string());
                    }
                    f.values.insert(k.to_string(), v.to_string());
                    i += 1;
                    continue;
                }
                let key = raw;
                // Known boolean-only flags never consume the next positional argument
                let is_bool_flag = matches!(
                    key,
                    "count"
                        | "annotate"
                        | "no-comments"
                        | "collapse-imports"
                        | "show-imports"
                        | "full"
                        | "include-tests"
                        | "no-tests"
                        | "no-source"
                        | "no-noncode"
                        | "non-code"
                        | "code"
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
        "callers" => println!("betterrank callers <sym1[,sym2,...]> [--file path] [--context [N]] [--no-tests] [--no-noncode] [--root <path>]\n\nOptions:\n  --file <path>    Disambiguate duplicate symbol names\n  --context [N]    Show N lines of context around call sites (default: 2)\n  --no-tests       Exclude test/spec files\n  --no-noncode     Skip automatic non-code/config mention scan\n  --count          Return count only"),
        "grep" => println!("betterrank grep <pattern> [--non-code] [--no-tests] [-C N] [-l] [-i] [-F] [--path sub] [--limit N] [--max-per-file N] [--root <path>]\n\nPageRank-ranked, hard-bounded ripgrep across ALL files (code + Dockerfiles + CI YAML + Prisma + SQL + JSON), annotated with enclosing symbol names when warm.\n\nOptions:\n  --non-code         Search only non-code/config/infra/schema files\n  --no-tests         Exclude test/spec files\n  -C N, --context N  Show N lines of surrounding context (max: 5)\n  -l, --files-only   List matching file paths only\n  -i                 Case-insensitive search\n  -F                 Fixed-string literal search (no regex)\n  --path <substr>    Restrict to files whose path contains <substr>\n  --limit N          Max matching files (default: 30)\n  --max-per-file N   Max matching lines per file (default: 5)"),
        "outline" => println!("betterrank outline <file> [symbol1,symbol2] [--annotate --lines A-B --no-comments --depth N --collapse-imports --root <path>]"),
        _ => println!("{USAGE}"),
    }
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        println!("{USAGE}");
        return;
    }

    // Parse flags across all arguments so global flags (e.g. `--root <path>`)
    // work both before and after the subcommand.
    let mut flags = parse_flags(&args);
    if flags.has("version") && flags.positional.is_empty() {
        println!("{}", betterrank::version());
        return;
    }
    if flags.positional.is_empty() {
        println!("{USAGE}");
        return;
    }

    let command_owned = flags.positional.remove(0);
    let command = command_owned.as_str();

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
                show_imports: flags.has("show-imports"),
                full_skeleton: flags.has("full"),
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
        if flags.positional.is_empty() && flags.extra_patterns.is_empty() {
            eprintln!("Usage: betterrank grep <pattern> [-C N] [--code|--non-code] [--ext ts,py] [--type dockerfile,ci] [--include-tests] [-l] [-i] [-F] [--limit N]");
            process::exit(1);
        }
        let pattern = flags
            .positional
            .first()
            .map(|s| s.as_str())
            .unwrap_or("");
        let warm = betterrank::graph::MmapGraphCache::open(&idx.cache_path);
        let ctx_lines = if let Some(n) = flags.get_usize("context") {
            n
        } else if flags.bools.contains("context") {
            2
        } else {
            0
        };
        let before_ctx = flags.get_usize("before-context").unwrap_or(ctx_lines);
        let after_ctx = flags.get_usize("after-context").unwrap_or(ctx_lines);
        let exts: Vec<String> = flags
            .get("ext")
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let types: Vec<String> = flags
            .get("type")
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_ascii_lowercase())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let include_tests = flags.has("include-tests") && !flags.has("no-tests");
        let opts = GrepOpts {
            case_insensitive: flags.has("case-insensitive"),
            fixed_strings: flags.has("fixed-strings"),
            noncode_only: flags.has("non-code"),
            code_only: flags.has("code"),
            include_tests,
            files_only: flags.has("files-only"),
            limit_files: user_limit.unwrap_or(30),
            max_per_file: flags.get_usize("max-per-file").unwrap_or(5),
            user_max_per_file: flags.get("max-per-file").is_some(),
            before_context: before_ctx,
            after_context: after_ctx,
            path_filter: flags.get("path").map(|s| s.to_string()),
            exts,
            types,
            extra_patterns: flags.extra_patterns.clone(),
        };
        match ranked_grep(&idx.roots, pattern, warm.as_ref().map(|m| m.archived()), &opts) {
            Ok(res) => {
                if res.path_matched_zero {
                    let pf = opts.path_filter.as_deref().unwrap_or("");
                    eprintln!("⚠ --path \"{pf}\" matched 0 files under the active --root(s). Use a repo-relative path or pass --root <repo>.");
                    return;
                }
                if count_only {
                    println!("files: {}", res.total_files);
                    println!("matches: {}", res.total_hits);
                    if res.hidden_test_files > 0 {
                        println!(
                            "hidden_test_files: {} (matches: {})",
                            res.hidden_test_files, res.hidden_test_hits
                        );
                    }
                    return;
                }
                if !res.branch_counts.is_empty() {
                    let summary: Vec<String> = res
                        .branch_counts
                        .iter()
                        .map(|(b, c)| format!("{b}: {c}"))
                        .collect();
                    println!("Pattern matches (files): {}", summary.join(" | "));
                }
                if res.hits.is_empty() && res.hidden_test_files == 0 {
                    println!("(no matches for \"{pattern}\")");
                    return;
                }
                let has_ctx = opts.before_context > 0 || opts.after_context > 0;
                for fh in &res.hits {
                    if opts.files_only {
                        println!("{}  ({} matches)", fh.file, fh.total_hits_in_file);
                    } else {
                        println!("{}:", fh.file);
                        let mut prev_line: Option<u64> = None;
                        let mut shown_matches = 0usize;
                        for h in &fh.hits {
                            if let Some(pl) = prev_line {
                                if has_ctx && h.line > pl + 1 {
                                    println!("  --");
                                }
                            }
                            if h.is_context {
                                println!("  {:>5}- {}", h.line, h.content);
                            } else {
                                shown_matches += 1;
                                if let Some(ref sym) = h.enclosing_sym {
                                    println!("  {:>5} [{sym}]│ {}", h.line, h.content);
                                } else {
                                    println!("  {:>5}│ {}", h.line, h.content);
                                }
                            }
                            prev_line = Some(h.line);
                        }
                        if fh.total_hits_in_file > shown_matches {
                            println!(
                                "        ... (+{} more in file)",
                                fh.total_hits_in_file - shown_matches
                            );
                        }
                    }
                }
                if res.total_files > res.hits.len() {
                    println!(
                        "\nShowing {} of {} files ({} total matches; ranked by PageRank). Use --limit N or --path <subpath> to narrow.",
                        res.hits.len(),
                        res.total_files,
                        res.total_hits
                    );
                }
                if res.hidden_test_files > 0 {
                    println!(
                        "(+ {} matches in {} test/fixture {} hidden — pass --include-tests to expand)",
                        res.hidden_test_hits,
                        res.hidden_test_files,
                        if res.hidden_test_files == 1 { "file" } else { "files" }
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
        "files" => {
            let pat = flags.positional.first().map(|s| s.as_str());
            let files_limit = user_limit.unwrap_or(200);
            cmd_files(
                g,
                pat,
                flags.has("code"),
                flags.has("non-code"),
                flags.has("no-tests"),
                count_only,
                offset,
                files_limit,
            );
        }
        "search" => {
            if flags.positional.is_empty() {
                eprintln!("Usage: betterrank search <query> [--kind type] [--max-per-file N] [--include-tests]");
                process::exit(1);
            }
            let q = flags.positional.join(" ");
            let max_per_file = flags.get_usize("max-per-file").unwrap_or(5);
            let include_tests = flags.has("include-tests") && !flags.has("no-tests");
            cmd_search(
                &idx,
                g,
                &q,
                flags.get("kind"),
                max_per_file,
                include_tests,
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
            let Some(sym_arg) = flags.positional.first() else {
                eprintln!("Usage: betterrank callers <sym1[,sym2,...]> [--file|--defined-in path] [--path sub] [--include-tests]");
                process::exit(1);
            };
            let f_arg = flags
                .get("file")
                .or_else(|| flags.get("defined-in"))
                .map(|f| normalize_graph_file_arg(&idx, f));
            let ctx_lines = if let Some(n) = flags.get_usize("context") {
                n
            } else if flags.bools.contains("context") {
                2
            } else {
                0
            };
            let include_tests = flags.has("include-tests") && !flags.has("no-tests");
            let syms: Vec<&str> = sym_arg
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect();
            for (i, sym) in syms.iter().enumerate() {
                if syms.len() > 1 {
                    if i > 0 {
                        println!();
                    }
                    println!("=== callers: {sym} ===");
                }
                cmd_callers(
                    &idx,
                    g,
                    sym,
                    f_arg.as_deref(),
                    flags.get("path"),
                    ctx_lines,
                    include_tests,
                    count_only,
                    offset,
                    limit,
                    user_limit.is_some(),
                    include_noncode,
                );
            }
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
                eprintln!("Usage: betterrank dependents <file> [--include-tests]");
                process::exit(1);
            };
            let f = normalize_graph_file_arg(&idx, raw_f);
            let include_tests = flags.has("include-tests") && !flags.has("no-tests");
            cmd_dependents(
                &idx,
                g,
                &f,
                include_tests,
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
