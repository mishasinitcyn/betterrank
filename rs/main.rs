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
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process;

const DEFAULT_LIMIT: usize = 50;
const DEFAULT_DEPTH: usize = 3;
const DEFAULT_NON_TTY_MAX_CHARS: usize = 16_000;

fn effective_max_chars(flags: &Flags) -> Option<usize> {
    if let Some(n) = flags.get_usize("max-chars") {
        if n == 0 {
            None
        } else {
            Some(n)
        }
    } else if std::io::stdout().is_terminal() {
        None
    } else {
        Some(DEFAULT_NON_TTY_MAX_CHARS)
    }
}

const USAGE: &str = "\
betterrank <command> [options]

Commands:
  outline     <file[:syms|L1-50]> [...] [--filter]  Compact skeleton or batched multi-file slices (always-on headers)
  map         [--focus file1,file2] [--include-tests] Repo map (ranked by PageRank, excludes tests by default)
  files       [dir-or-glob] [--dirs] [--depth N]    List indexed code + config files (auto directory rollup when > 200)
  search      <query> [--kind type]                 Path- & token-aware search on symbols, files, + non-code mentions
  grep        <pattern> [-C N] [--compact] [--path] Code-first PageRank ripgrep (repeatable --path, --exclude-path, --no-imports)
  structure   [--depth N]                           File tree with symbol counts (default depth: 3)
  symbols     [--file path] [--kind type]           List definitions (ranked by PageRank)
  callers     <sym1[,sym2]> [--file path] [--context] Ranked callers + compact Tests footer (supports file.ts:sym)
  context     <symbol> [--file path]                 Full context: source, deps, types, callers
  history     <symbol> [--file path]                Git history of a specific function
  trace       <symbol> [--depth N]                  Recursive caller chain (upward)
  callees     <symbol> [--depth N]                  Recursive callee chain (downward)
  diff        [--ref <commit>]                      Git-aware blast radius (changed symbols + callers)
  deps        <file>                                What this file imports (ranked)
  dependents  <file>                                What imports this file (lines + symbols + Tests footer) + non-code mentions
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
    paths: Vec<String>,
    exclude_paths: Vec<String>,
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
            "-w" => {
                f.bools.insert("word-regexp".to_string());
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
                    } else if k == "path" {
                        f.paths.push(v.to_string());
                    } else if k == "exclude-path" {
                        f.exclude_paths.push(v.to_string());
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
                        | "include-data"
                        | "no-tests"
                        | "no-imports"
                        | "no-source"
                        | "no-noncode"
                        | "non-code"
                        | "code"
                        | "files-only"
                        | "compact"
                        | "dirs"
                        | "all"
                        | "case-insensitive"
                        | "fixed-strings"
                        | "word-regexp"
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
                    } else if key == "path" {
                        f.paths.push(val.clone());
                    } else if key == "exclude-path" {
                        f.exclude_paths.push(val.clone());
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

fn find_root_for_existing_path(p: &Path) -> Option<PathBuf> {
    if !p.exists() {
        return None;
    }
    let abs = fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let start_dir = if abs.is_dir() {
        abs.as_path()
    } else {
        abs.parent().unwrap_or(abs.as_path())
    };
    Some(find_default_root(start_dir))
}

fn resolve_roots(flags: &Flags, target_file_hint: Option<&str>) -> Vec<PathBuf> {
    if !flags.roots.is_empty() {
        return flags.roots.iter().map(PathBuf::from).collect();
    }
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if let Some(raw_f) = target_file_hint {
        let candidate = Path::new(raw_f);
        let probe = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            cwd.join(candidate)
        };
        if let Some(root) = find_root_for_existing_path(&probe) {
            return vec![root];
        }
    }
    vec![find_default_root(&cwd)]
}

fn strip_outline_spec(arg: &str) -> (&str, Vec<String>) {
    // Only split `file:spec` when the left side looks like a path (length > 1 so Windows `C:\...` isn't split)
    // and the right side does not contain a path separator.
    if let Some((fpart, spec)) = arg.rsplit_once(':') {
        if fpart.len() > 1 && !spec.is_empty() && !spec.contains('/') && !spec.contains('\\') {
            let specs: Vec<String> = spec
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !specs.is_empty() {
                return (fpart, specs);
            }
        }
    }
    (arg, Vec::new())
}

fn try_resolve_outline_file(file_arg: &str, roots: &[PathBuf], cwd: &Path) -> Option<PathBuf> {
    let p = Path::new(file_arg);
    if p.is_absolute() {
        return p.is_file().then(|| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()));
    }
    let mut cands = Vec::with_capacity(roots.len() + 1);
    if let Some(r0) = roots.first() {
        cands.push(r0.join(p));
    }
    cands.push(cwd.join(p));
    for r in roots.iter().skip(1) {
        cands.push(r.join(p));
    }
    for cand in cands {
        if cand.is_file() {
            return Some(fs::canonicalize(&cand).unwrap_or(cand));
        }
    }
    None
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
        "callers" => println!("betterrank callers <sym1[,sym2,...]> [--file path] [--path sub] [--context [N]] [--include-tests|--no-tests] [--no-noncode] [--root <path>]\n\nOptions:\n  --file <path>    Disambiguate duplicate symbol names (or pass file.ts:sym)\n  --path <substr>  Filter caller paths (repeatable OR-union)\n  --context [N]    Show N lines of merged context around call sites (default: 2)\n  --include-tests  Include test/fixture callers with full context\n  --no-tests       Suppress even the compact Tests (N) footer\n  --no-noncode     Skip automatic non-code/config mention scan\n  --count          Return count only"),
        "grep" => println!("betterrank grep <pattern> [-C N] [-w] [--compact] [--code|--non-code] [--path sub ...] [--exclude-path sub ...] [--no-imports] [--no-comments] [--include-tests] [--include-data] [--max-chars N] [-l] [-i] [-F] [--limit N] [--root <path>]\n\nCode-first, PageRank-ranked ripgrep across all files with rarest-branch-first alternation quotas and enclosing symbol labels.\n\nOptions:\n  --code             Search only supported source code files\n  --non-code         Search only non-code/config/infra/schema files\n  --path <substr>    Restrict to matching paths (repeatable OR-union; auto-includes tests if path targets tests)\n  --exclude-path <s> Exclude files whose path contains <s> (repeatable)\n  --no-imports       Filter out import/export-from/require lines\n  --no-comments      Filter out comment-only lines (comments are already demoted below code by default)\n  --include-tests    Include test/spec/fixture files in main output\n  --no-tests         Force-exclude tests even when --path targets tests\n  --include-data     Include bulk data/locale/dictionary files (*.dic, locales/*, >128KB JSON)\n  --compact          Dense 1-line-per-file matrix: path [matched_terms] L12, L45\n  --max-chars N      Output char budget (default: 16000 when non-TTY, 0 = unlimited)\n  -C N, --context N  Show N lines of surrounding context (max: 15)\n  -w, --word-regexp  Match whole words only\n  -l, --files-only   List matching file paths only\n  -i                 Case-insensitive search\n  -F                 Fixed-string literal search (no regex)\n  --limit N          Max matching files (default: 30)\n  --max-per-file N   Max matching lines per file (default: 5; uncapped on single-file --path)"),
        "files" => println!("betterrank files [dir-or-glob ...] [--filter <regex>] [--dirs] [--depth N] [--all] [--code|--non-code] [--no-tests] [--limit N] [--root <path>]\n\nLists indexed files across one or more dir/glob patterns. Automatically rolls up by directory when matches exceed --limit (default: 200).\n\nOptions:\n  --filter <regex>  Case-insensitive regex filter on file paths (applied before rollup)\n  --dirs            Force directory rollup view (`dir/ (N files)`)\n  --depth N         Directory rollup depth below prefix when rolling up (default: 1)\n  --all             Disable auto-rollup and list all matching files\n  --code            Code files only\n  --non-code        Non-code/config files only\n  --no-tests        Exclude test/spec files"),
        "outline" => println!("betterrank outline <file[:sym1,L10-50|ALL]> [...] [--filter <regex>] [--around <regex> [-C N]] [--full-under N] [--depth N] [--max-chars N] [--annotate] [--no-comments] [--full] [--root <path>]\n\nExamples:\n  betterrank outline src/api.ts --filter \"review|score\"\n  betterrank outline src/a.ts:10-40 src/b.ts:handler,90-120\n  betterrank outline src/api.ts:handler --around \"status\" -C 15\n  betterrank outline package.json:ALL --full-under 200"),
        _ => println!("{USAGE}"),
    }
}

fn main() {
    // Reset SIGPIPE to default on Unix so piping `betterrank ... | head -n 50`
    // exits cleanly instead of panicking with `Broken pipe (os error 32)`.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }

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
        if flags.positional.is_empty() {
            eprintln!("Usage: betterrank outline <file[:sym1,L10-50|ALL]> [...] [--filter <regex>] [--around <regex> [-C N]] [--full-under N] [--annotate] [--no-comments] [--depth N] [--max-chars N] [--root <path>]");
            process::exit(1);
        }
        let annotate = flags.has("annotate");
        let no_comments = flags.has("no-comments");
        let collapse_imports = flags.has("collapse-imports");
        let explicit_depth = flags.get("depth").is_some();
        let depth = flags.get_usize("depth").unwrap_or(1) as u32;
        let filter_pat = flags.get("filter");
        let around_pat = flags.get("around");
        let around_ctx = flags.get_usize("context").unwrap_or(15);
        let full_under = flags.get_usize("full-under");
        let max_chars = effective_max_chars(&flags);
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
        let initial_roots: Vec<PathBuf> = if !flags.roots.is_empty() {
            flags
                .roots
                .iter()
                .map(|r| fs::canonicalize(r).unwrap_or_else(|_| PathBuf::from(r)))
                .collect()
        } else {
            vec![fs::canonicalize(&cwd).unwrap_or_else(|_| cwd.clone())]
        };

        // Parse either classic single-file `outline <file> [sym1,10-40]` OR multi-file batched
        // `outline fileA.ts:10-40 fileB.ts:sym1,sym2 fileC.ts`.
        let targets: Vec<(String, Vec<String>)> = if flags.positional.len() == 2 {
            let (f0, s0) = strip_outline_spec(&flags.positional[0]);
            let (f1, s1) = strip_outline_spec(&flags.positional[1]);
            let second_is_existing_file = s1.is_empty()
                && try_resolve_outline_file(f1, &initial_roots, &cwd).is_some();
            if s0.is_empty() && s1.is_empty() && !second_is_existing_file {
                let syms: Vec<String> = flags.positional[1]
                    .split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect();
                vec![(f0.to_string(), syms)]
            } else {
                vec![(f0.to_string(), s0), (f1.to_string(), s1)]
            }
        } else {
            flags
                .positional
                .iter()
                .map(|arg| {
                    // If the raw arg itself exists on disk, treat it as a bare file path
                    if try_resolve_outline_file(arg, &initial_roots, &cwd).is_some() {
                        (arg.clone(), Vec::new())
                    } else {
                        let (fp, sp) = strip_outline_spec(arg);
                        (fp.to_string(), sp)
                    }
                })
                .collect()
        };

        // Resolve roots from the target file's own enclosing `.git` when `--root` was omitted
        // (avoids walking up from `cwd` and cold-indexing a parent multi-repo workspace!).
        let first_resolved = try_resolve_outline_file(&targets[0].0, &initial_roots, &cwd);
        let raw_roots: Vec<PathBuf> = if !flags.roots.is_empty() {
            flags.roots.iter().map(PathBuf::from).collect()
        } else if let Some(ref abs0) = first_resolved {
            vec![find_default_root(abs0.parent().unwrap_or(&cwd))]
        } else if annotate {
            vec![find_default_root(&cwd)]
        } else {
            vec![cwd.clone()]
        };
        let roots: Vec<PathBuf> = raw_roots
            .iter()
            .map(|r| fs::canonicalize(r).unwrap_or_else(|_| r.clone()))
            .collect();

        // Open index once across all targets if `--annotate` is requested
        let annotate_idx_and_mmap = if annotate {
            let idx = CodeIndex::new(&roots);
            idx.ensure().ok().map(|mmap| (idx, mmap))
        } else {
            None
        };

        let mut remaining_chars = max_chars;
        let mut sections: Vec<String> = Vec::with_capacity(targets.len());
        let mut skipped_targets: Vec<String> = Vec::new();

        for (file_arg, expand_syms) in &targets {
            if let Some(rem) = remaining_chars {
                if rem < 120 && !sections.is_empty() {
                    let spec = if expand_syms.is_empty() {
                        file_arg.clone()
                    } else {
                        format!("{file_arg}:{}", expand_syms.join(","))
                    };
                    skipped_targets.push(spec);
                    continue;
                }
            }

            let Some(abs_path) = try_resolve_outline_file(file_arg, &roots, &cwd) else {
                eprintln!("Cannot read file: {file_arg}");
                if targets.len() == 1 {
                    process::exit(1);
                }
                continue;
            };
            let Ok(source) = fs::read_to_string(&abs_path) else {
                eprintln!("Cannot read file: {}", abs_path.display());
                if targets.len() == 1 {
                    process::exit(1);
                }
                continue;
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

            let caller_counts = annotate_idx_and_mmap.as_ref().map(|(idx, mmap)| {
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
                get_caller_counts_for_file(mmap.archived(), &gpath)
            });

            let out = build_outline(
                &source,
                &rel_path,
                expand_syms,
                OutlineOpts {
                    caller_counts: caller_counts.as_ref(),
                    line_range,
                    no_comments,
                    depth,
                    explicit_depth,
                    collapse_imports,
                    show_imports: flags.has("show-imports"),
                    full_skeleton: flags.has("full"),
                    filter: filter_pat,
                    around: around_pat,
                    around_context: around_ctx,
                    full_under,
                    max_chars: remaining_chars,
                },
            );
            if let Some(rem) = remaining_chars {
                remaining_chars = Some(rem.saturating_sub(out.len() + 2));
            }
            sections.push(out);
        }

        if !skipped_targets.is_empty() {
            let cap_label = max_chars.unwrap_or(DEFAULT_NON_TTY_MAX_CHARS);
            sections.push(format!(
                "... (stopped at --max-chars {cap_label} — next: betterrank outline {})",
                skipped_targets.join(" ")
            ));
        }

        if !sections.is_empty() {
            println!("{}", sections.join("\n\n"));
        }
        return;
    }

    // For file-targeted commands, auto-detect `.git` root from the target file when `--root` is omitted.
    let target_file_hint: Option<&str> = match command {
        "deps" | "dependents" | "neighborhood" => flags.positional.first().map(|s| s.as_str()),
        "symbols" | "context" | "history" | "trace" | "callees" => flags.get("file"),
        "grep" if flags.paths.len() == 1 => {
            let p = Path::new(&flags.paths[0]);
            if p.is_absolute() && p.exists() {
                Some(flags.paths[0].as_str())
            } else {
                None
            }
        }
        _ => None,
    };

    let raw_roots = resolve_roots(&flags, target_file_hint);
    let idx = CodeIndex::new(&raw_roots);
    let count_only = flags.has("count");
    let offset = flags.get_usize("offset").unwrap_or(0);
    let user_limit = flags.get_usize("limit");
    let limit = user_limit.unwrap_or(DEFAULT_LIMIT);
    let include_noncode = !flags.has("no-noncode");

    // `grep` works even without triggering a cold index (uses warm mmap cache if present for PageRank!)
    if command == "grep" {
        if flags.positional.is_empty() && flags.extra_patterns.is_empty() {
            eprintln!("Usage: betterrank grep <pattern> [-C N] [--compact] [--code|--non-code] [--path sub ...] [--exclude-path sub ...] [--no-imports] [--include-tests] [-l] [-i] [-F] [--limit N]");
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
        let norm_path_filters: Vec<String> = flags
            .paths
            .iter()
            .map(|p| {
                let raw = Path::new(p);
                if raw.is_absolute() && raw.exists() {
                    normalize_graph_file_arg(&idx, p)
                } else {
                    normalize_slashes(p)
                }
            })
            .collect();
        let norm_exclude_paths: Vec<String> = flags
            .exclude_paths
            .iter()
            .map(|p| normalize_slashes(p))
            .collect();
        let no_tests_explicit = flags.has("no-tests");
        let include_tests = flags.has("include-tests") && !no_tests_explicit;
        let max_chars = effective_max_chars(&flags);
        let opts = GrepOpts {
            case_insensitive: flags.has("case-insensitive"),
            fixed_strings: flags.has("fixed-strings"),
            word_regexp: flags.has("word-regexp"),
            noncode_only: flags.has("non-code"),
            code_only: flags.has("code"),
            include_tests,
            include_data: flags.has("include-data"),
            no_tests_explicit,
            no_imports: flags.has("no-imports"),
            no_comments: flags.has("no-comments"),
            files_only: flags.has("files-only"),
            compact: flags.has("compact"),
            limit_files: user_limit.unwrap_or(30),
            max_per_file: flags.get_usize("max-per-file").unwrap_or(5),
            user_max_per_file: flags.get("max-per-file").is_some(),
            before_context: before_ctx,
            after_context: after_ctx,
            path_filters: norm_path_filters,
            exclude_paths: norm_exclude_paths,
            exts,
            types,
            extra_patterns: flags.extra_patterns.clone(),
        };
        match ranked_grep(&idx.roots, pattern, warm.as_ref().map(|m| m.archived()), &opts) {
            Ok(res) => {
                if res.path_matched_zero {
                    let pf = opts.path_filters.join(", ");
                    eprintln!("⚠ --path \"{pf}\" matched 0 files under the active --root(s). Use a repo-relative path or pass --root <repo>.");
                    return;
                }
                if count_only {
                    println!("files: {}", res.total_files);
                    println!("matches: {}", res.total_hits);
                    if !res.repo_counts.is_empty() {
                        let summary: Vec<String> = res
                            .repo_counts
                            .iter()
                            .map(|(r, c)| format!("{r}: {c}"))
                            .collect();
                        println!("repos: {}", summary.join(" | "));
                    }
                    if res.hidden_test_files > 0 {
                        println!(
                            "hidden_test_files: {} (matches: {})",
                            res.hidden_test_files, res.hidden_test_hits
                        );
                    }
                    return;
                }
                let mut emitted_chars = 0usize;
                if !res.branch_counts.is_empty() {
                    let summary: Vec<String> = res
                        .branch_counts
                        .iter()
                        .map(|(b, c)| format!("{b}: {c}"))
                        .collect();
                    let line = format!("Pattern matches (files): {}", summary.join(" | "));
                    emitted_chars += line.len() + 1;
                    println!("{line}");
                }
                if !res.repo_counts.is_empty() {
                    let summary: Vec<String> = res
                        .repo_counts
                        .iter()
                        .map(|(r, c)| format!("{r}: {c}"))
                        .collect();
                    let line = format!("Repos (files): {}", summary.join(" | "));
                    emitted_chars += line.len() + 1;
                    println!("{line}");
                }
                if res.hits.is_empty() && res.hidden_test_files == 0 {
                    println!("(no matches for \"{pattern}\")");
                    return;
                }
                let has_ctx = opts.before_context > 0 || opts.after_context > 0;
                let mut shown_files = 0usize;
                let mut stopped_at_cap = false;

                for fh in &res.hits {
                    let mut block = String::new();
                    if opts.files_only {
                        block.push_str(&format!("{}  ({} matches)\n", fh.file, fh.total_hits_in_file));
                    } else if opts.compact {
                        let line_nums: Vec<String> = fh
                            .hits
                            .iter()
                            .filter(|h| !h.is_context)
                            .map(|h| format!("L{}", h.line))
                            .collect();
                        let shown = line_nums.len();
                        let more = if fh.total_hits_in_file > shown {
                            format!(" (+{})", fh.total_hits_in_file - shown)
                        } else {
                            String::new()
                        };
                        if !fh.matched_branches.is_empty() {
                            block.push_str(&format!(
                                "{}  [{}]  {}{}\n",
                                fh.file,
                                fh.matched_branches.join(", "),
                                line_nums.join(", "),
                                more
                            ));
                        } else {
                            block.push_str(&format!("{}  {}{}\n", fh.file, line_nums.join(", "), more));
                        }
                    } else {
                        block.push_str(&format!("{}:\n", fh.file));
                        let mut prev_line: Option<u64> = None;
                        let mut shown_matches = 0usize;
                        for h in &fh.hits {
                            if let Some(pl) = prev_line {
                                if has_ctx && h.line > pl + 1 {
                                    block.push_str("  --\n");
                                }
                            }
                            if h.is_context {
                                block.push_str(&format!("  {:>5}- {}\n", h.line, h.content));
                            } else {
                                shown_matches += 1;
                                if let Some(ref sym) = h.enclosing_sym {
                                    block.push_str(&format!(
                                        "  {:>5} [{sym}]│ {}\n",
                                        h.line, h.content
                                    ));
                                } else {
                                    block.push_str(&format!("  {:>5}│ {}\n", h.line, h.content));
                                }
                            }
                            prev_line = Some(h.line);
                        }
                        if fh.total_hits_in_file > shown_matches {
                            block.push_str(&format!(
                                "        ... (+{} more in file)\n",
                                fh.total_hits_in_file - shown_matches
                            ));
                        }
                    }

                    if let Some(cap) = max_chars {
                        if !opts.files_only && shown_files > 0 && emitted_chars + block.len() > cap {
                            stopped_at_cap = true;
                            break;
                        }
                    }
                    print!("{block}");
                    emitted_chars += block.len();
                    shown_files += 1;
                }

                if stopped_at_cap {
                    let cap_val = max_chars.unwrap_or(DEFAULT_NON_TTY_MAX_CHARS);
                    println!(
                        "\n... (stopped at --max-chars {cap_val} after {shown_files} of {} files; {} total matches — narrow with --path / --compact or pass --max-chars 0)",
                        res.total_files, res.total_hits
                    );
                } else if res.total_files > res.hits.len() {
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
            let files_limit = user_limit.unwrap_or(200);
            cmd_files(
                g,
                &flags.positional,
                flags.get("filter"),
                flags.has("code"),
                flags.has("non-code"),
                flags.has("no-tests"),
                flags.has("dirs"),
                flags.get_usize("depth"),
                flags.has("all"),
                count_only,
                offset,
                files_limit,
            );
        }
        "search" => {
            if flags.positional.is_empty() {
                eprintln!("Usage: betterrank search <query> [--kind type] [--path sub ...] [--max-per-file N] [--include-tests]");
                process::exit(1);
            }
            let q = flags.positional.join(" ");
            let max_per_file = flags.get_usize("max-per-file").unwrap_or(5);
            let no_tests_explicit = flags.has("no-tests");
            let include_tests = flags.has("include-tests") && !no_tests_explicit;
            cmd_search(
                &idx,
                g,
                &q,
                flags.get("kind"),
                &flags.paths,
                max_per_file,
                include_tests,
                no_tests_explicit,
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
                eprintln!("Usage: betterrank callers <sym1[,sym2,...]> [--file|--defined-in path] [--path sub ...] [--include-tests|--no-tests]");
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
            let no_tests_explicit = flags.has("no-tests");
            let include_tests = flags.has("include-tests") && !no_tests_explicit;
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
                    &flags.paths,
                    ctx_lines,
                    include_tests,
                    no_tests_explicit,
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
                eprintln!("Usage: betterrank dependents <file> [--include-tests|--no-tests]");
                process::exit(1);
            };
            let f = normalize_graph_file_arg(&idx, raw_f);
            let no_tests_explicit = flags.has("no-tests");
            let include_tests = flags.has("include-tests") && !no_tests_explicit;
            cmd_dependents(
                &idx,
                g,
                &f,
                include_tests,
                no_tests_explicit,
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
