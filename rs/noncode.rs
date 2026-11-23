use crate::model::{is_supported_extension, ArchivedGraphCacheData, RootEntry};
use crate::walk::{is_test_file, normalize_slashes};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::UTF8;
use grep_searcher::{Searcher, SearcherBuilder, Sink, SinkContext, SinkContextKind, SinkMatch};
use ignore::WalkBuilder;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

const NONCODE_SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    ".next",
    ".nuxt",
    "dist",
    "build",
    "out",
    "coverage",
    "target",
    "__pycache__",
    ".venv",
    "venv",
    ".turbo",
    ".cache",
    ".parcel-cache",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    "Pods",
    ".code-index",
];

const NONCODE_SKIP_FILES: &[&str] = &[
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "Cargo.lock",
    "poetry.lock",
    "uv.lock",
    "composer.lock",
    "Gemfile.lock",
    "eslint-suppressions.json",
];

/// High-signal non-code / infra / schema / contract surfaces that AST-only tools miss.
pub fn is_noncode_config_file(rel_path: &str) -> bool {
    let base = rel_path.rsplit('/').next().unwrap_or(rel_path);
    let lower_base = base.to_ascii_lowercase();
    if NONCODE_SKIP_FILES
        .iter()
        .any(|&s| lower_base == s.to_ascii_lowercase())
    {
        return false;
    }
    if lower_base.ends_with(".min.json") || lower_base.ends_with(".tsbuildinfo") {
        return false;
    }
    if lower_base == "dockerfile"
        || lower_base.starts_with("dockerfile.")
        || lower_base.ends_with(".dockerfile")
        || lower_base == "makefile"
        || lower_base == "procfile"
        || lower_base == "docker-compose.yml"
        || lower_base == "docker-compose.yaml"
    {
        return true;
    }
    let ext = match lower_base.rfind('.') {
        Some(idx) => &lower_base[idx..],
        None => return false,
    };
    matches!(
        ext,
        ".prisma"
            | ".sql"
            | ".yml"
            | ".yaml"
            | ".json"
            | ".toml"
            | ".sh"
            | ".bash"
            | ".graphql"
            | ".gql"
            | ".proto"
            | ".tf"
            | ".hcl"
            | ".env"
            | ".ini"
            | ".cfg"
    )
}

pub fn is_test_or_fixture_path(path: &str) -> bool {
    if is_test_file(path) {
        return true;
    }
    let lower = format!("/{}", path.to_ascii_lowercase());
    for seg in &[
        "/__fixtures__/",
        "/fixtures/",
        "/model-benchmarks/",
        "/benchmarks/",
        "/contract_corpus/",
        "/__snapshots__/",
    ] {
        if lower.contains(seg) {
            return true;
        }
    }
    let base = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    base == "eslint-suppressions.json" || base.ends_with(".snap")
}

fn is_minified_asset_fast(rel_path: &str, abs_path: &Path, size: u64) -> bool {
    let lower = rel_path.to_ascii_lowercase();
    if lower.ends_with(".min.js")
        || lower.ends_with(".min.mjs")
        || lower.ends_with(".min.cjs")
        || lower.ends_with(".min.css")
        || lower.ends_with(".bundle.js")
        || lower.ends_with(".bundle.mjs")
        || lower.ends_with(".chunk.js")
        || lower.ends_with(".map")
    {
        return true;
    }
    // Check first 4 KB for bundled/minified JS/CSS/JSON lines (> 300 avg chars/line)
    if size > 64 * 1024
        && (lower.ends_with(".js")
            || lower.ends_with(".mjs")
            || lower.ends_with(".cjs")
            || lower.ends_with(".css"))
    {
        if let Ok(mut f) = File::open(abs_path) {
            let mut buf = [0u8; 4096];
            if let Ok(n) = f.read(&mut buf) {
                if n > 512 {
                    let newlines = memchr::memchr_iter(b'\n', &buf[..n]).count();
                    if newlines == 0 || (n / (newlines + 1)) > 300 {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn matches_type_filter(rel_path: &str, types: &[String]) -> bool {
    if types.is_empty() {
        return true;
    }
    let lower = rel_path.to_ascii_lowercase();
    let base = lower.rsplit('/').next().unwrap_or(&lower);
    for t in types {
        let matched = match t.as_str() {
            "dockerfile" | "docker" => {
                base == "dockerfile"
                    || base.starts_with("dockerfile.")
                    || base.ends_with(".dockerfile")
                    || base.starts_with("docker-compose.")
            }
            "ci" | "workflow" | "workflows" => {
                lower.contains(".github/workflows/")
                    || base == ".gitlab-ci.yml"
                    || base == "jenkinsfile"
            }
            "packaging" | "pkg" | "deps" => {
                matches!(
                    base,
                    "pyproject.toml"
                        | "setup.py"
                        | "setup.cfg"
                        | "package.json"
                        | "cargo.toml"
                        | "go.mod"
                        | "pipfile"
                ) || (base.starts_with("requirements") && base.ends_with(".txt"))
            }
            "prisma" => base.ends_with(".prisma"),
            "sql" => base.ends_with(".sql"),
            "config" | "noncode" | "non-code" => is_noncode_config_file(rel_path),
            "code" => is_supported_extension(rel_path),
            _ => lower.ends_with(&format!(".{t}")) || base == t,
        };
        if matched {
            return true;
        }
    }
    false
}

fn matches_ext_filter(rel_path: &str, exts: &[String]) -> bool {
    if exts.is_empty() {
        return true;
    }
    let lower = rel_path.to_ascii_lowercase();
    let file_ext = match lower.rfind('.') {
        Some(dot) => &lower[dot + 1..],
        None => return false,
    };
    exts.iter()
        .any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(file_ext))
}

#[derive(Debug, Clone)]
pub struct TextMatchLine {
    pub line: u64,
    pub content: String,
    pub is_context: bool,
    pub enclosing_sym: Option<String>,
}

fn is_bulk_data_or_locale_file(rel_path: &str, size: u64) -> bool {
    let lower = rel_path.to_ascii_lowercase();
    if lower.ends_with(".dic")
        || lower.ends_with(".aff")
        || lower.ends_with(".jsonl")
        || lower.ends_with(".ndjson")
        || lower.ends_with(".csv")
        || lower.ends_with(".tsv")
        || lower.ends_with(".svg")
    {
        return true;
    }
    let slash = format!("/{lower}");
    if slash.contains("/locales/") || slash.contains("/locale/") || slash.contains("/i18n/") {
        return true;
    }
    if lower.ends_with(".json") && size > 128 * 1024 {
        return true;
    }
    false
}

fn is_import_only_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("import ")
        || t.starts_with("import{")
        || (t.starts_with("from ") && t.contains(" import "))
        || (t.starts_with("export ") && t.contains(" from "))
        || t.contains("= require(")
}

fn is_comment_only_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//")
        || t.starts_with('#')
        || t.starts_with("/*")
        || t.starts_with("* ")
        || t == "*"
        || t.starts_with("*/")
        || t.starts_with("-- ")
        || t.starts_with("<!--")
}

pub fn is_schema_or_idl_extension(rel_path: &str) -> bool {
    let lower = rel_path.to_ascii_lowercase();
    lower.ends_with(".prisma")
        || lower.ends_with(".sql")
        || lower.ends_with(".graphql")
        || lower.ends_with(".gql")
        || lower.ends_with(".proto")
}

/// Non-config documentation and structured data/fixture files surfaced by `betterrank files`
/// (kept separate from `is_noncode_config_file` so `.gz` archives and `.csv` dumps never pollute
/// automatic non-code mention scans).
pub fn is_doc_or_data_file(rel_path: &str) -> bool {
    let lower = rel_path.to_ascii_lowercase();
    lower.ends_with(".md")
        || lower.ends_with(".mdx")
        || lower.ends_with(".txt")
        || lower.ends_with(".rst")
        || lower.ends_with(".jsonl")
        || lower.ends_with(".ndjson")
        || lower.ends_with(".jsonl.gz")
        || lower.ends_with(".json.gz")
        || lower.ends_with(".csv")
        || lower.ends_with(".tsv")
}

fn compile_glob_regex(pat: &str) -> Option<regex::Regex> {
    let p = pat.trim();
    if p.is_empty() {
        return None;
    }
    let has_slash = p.contains('/');
    let mut re_s = if has_slash {
        String::from("^")
    } else {
        String::from("(^|/)")
    };
    let chars: Vec<char> = p.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        match chars[i] {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    re_s.push_str(".*");
                    i += 2;
                    if i < chars.len() && chars[i] == '/' {
                        i += 1;
                    }
                    continue;
                }
                re_s.push_str("[^/]*");
            }
            '?' => re_s.push_str("[^/]"),
            other => re_s.push_str(&regex::escape(&other.to_string())),
        }
        i += 1;
    }
    re_s.push('$');
    regex::RegexBuilder::new(&re_s)
        .case_insensitive(true)
        .build()
        .ok()
}

fn matches_path_filter(graph_path: &str, raw_pf: &str) -> bool {
    let pf = raw_pf.trim_start_matches("./").trim_end_matches('/');
    if pf.is_empty() {
        return true;
    }
    // Check directory/path segment boundaries so `--path apps/qa` matches `apps/qa/...`
    // and `repo/apps/qa/...` without leaking sibling dirs like `apps/qa-triage/...`!
    let mut start = 0usize;
    while let Some(rel_idx) = graph_path[start..].find(pf) {
        let idx = start + rel_idx;
        let left_ok = idx == 0 || graph_path.as_bytes()[idx - 1] == b'/';
        let end = idx + pf.len();
        let right_ok = end == graph_path.len() || graph_path.as_bytes()[end] == b'/';
        if left_ok && right_ok {
            return true;
        }
        start = idx + 1;
        if start >= graph_path.len() {
            break;
        }
    }
    false
}

fn matches_glob_filters(
    rel_path: &str,
    pos_globs: &[regex::Regex],
    neg_globs: &[regex::Regex],
) -> bool {
    if neg_globs.iter().any(|re| re.is_match(rel_path)) {
        return false;
    }
    if pos_globs.is_empty() {
        return true;
    }
    pos_globs.iter().any(|re| re.is_match(rel_path))
}

fn detect_python_docstring_lines(abs_path: &Path) -> FxHashSet<u64> {
    let mut skipped = FxHashSet::default();
    let Ok(source) = std::fs::read_to_string(abs_path) else {
        return skipped;
    };
    let mut in_quote: Option<&str> = None;
    for (i, line) in source.split('\n').enumerate() {
        let ln = i as u64 + 1;
        let trimmed = line.trim();
        if let Some(q) = in_quote {
            skipped.insert(ln);
            if trimmed.contains(q) {
                in_quote = None;
            }
        } else {
            for q in &["\"\"\"", "'''"] {
                if let Some(pos) = trimmed.find(q) {
                    // Only treat standalone docstring/triple-quoted comment lines (not inline string assignments like `x = """..."""`)
                    let before = trimmed[..pos].trim();
                    if before.is_empty() || before == "r" || before == "f" || before == "rf" || before == "fr" {
                        skipped.insert(ln);
                        let rest = &trimmed[pos + 3..];
                        if !rest.contains(q) {
                            in_quote = Some(q);
                        }
                        break;
                    }
                }
            }
        }
    }
    skipped
}

#[derive(Debug, Clone)]
pub struct TextFileHits {
    pub file: String,
    pub hits: Vec<TextMatchLine>,
    pub total_hits_in_file: usize,
    pub rank_score: f32,
    pub matched_branches: Vec<String>,
    pub enclosing_syms: Vec<String>,
}

fn truncate_line(s: &str, max_chars: usize) -> String {
    let trimmed = s.trim_end_matches(&['\r', '\n'][..]);
    if trimmed.chars().count() <= max_chars {
        trimmed.to_string()
    } else {
        let mut out: String = trimmed.chars().take(max_chars).collect();
        out.push_str("...");
        out
    }
}

fn walk_root_parallel<F>(
    r: &RootEntry,
    skip_set: &std::sync::Arc<FxHashSet<&'static str>>,
    max_bytes: u64,
    accept: F,
) -> Vec<(String, PathBuf)>
where
    F: Fn(&str, &str) -> bool + Send + Sync + 'static,
{
    use ignore::WalkState;
    use std::sync::{Arc, Mutex};

    let root_path = Arc::new(r.path.clone());
    let prefix = Arc::new(r.prefix.clone());
    let accept = Arc::new(accept);
    let out: Arc<Mutex<Vec<(String, PathBuf)>>> = Arc::new(Mutex::new(Vec::with_capacity(512)));
    let threads = std::thread::available_parallelism()
        .map(|n| n.get().clamp(2, 12))
        .unwrap_or(4);

    let skip_for_filter = Arc::clone(skip_set);
    let walker = WalkBuilder::new(&r.path)
        .standard_filters(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .hidden(false) // IMPORTANT: include `.github/workflows/*.yml`!
        .threads(threads)
        .filter_entry(move |e| {
            if e.depth() == 0 {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            if e.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                return !skip_for_filter.contains(name.as_ref());
            }
            true
        })
        .build_parallel();

    walker.run(|| {
        let root_path = Arc::clone(&root_path);
        let prefix = Arc::clone(&prefix);
        let accept = Arc::clone(&accept);
        let out = Arc::clone(&out);

        struct Flusher {
            local: Vec<(String, PathBuf)>,
            out: Arc<Mutex<Vec<(String, PathBuf)>>>,
        }
        impl Drop for Flusher {
            fn drop(&mut self) {
                if !self.local.is_empty() {
                    if let Ok(mut guard) = self.out.lock() {
                        guard.append(&mut self.local);
                    }
                }
            }
        }
        let mut flusher = Flusher {
            local: Vec::with_capacity(128),
            out,
        };

        Box::new(move |res| {
            if let Ok(entry) = res {
                if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                    let abs = entry.path();
                    if let Ok(rel) = abs.strip_prefix(&*root_path) {
                        let rel_str = normalize_slashes(&rel.to_string_lossy());
                        let graph_path = if prefix.is_empty() {
                            rel_str.clone()
                        } else {
                            format!("{}/{rel_str}", *prefix)
                        };
                        if accept(&rel_str, &graph_path) {
                            if let Ok(meta) = entry.metadata() {
                                let sz = meta.len();
                                if sz <= max_bytes && !is_minified_asset_fast(&rel_str, abs, sz) {
                                    flusher.local.push((graph_path, abs.to_path_buf()));
                                }
                            }
                        }
                    }
                }
            }
            WalkState::Continue
        })
    });

    match Arc::try_unwrap(out) {
        Ok(m) => m.into_inner().unwrap_or_default(),
        Err(arc) => arc.lock().map(|g| g.clone()).unwrap_or_default(),
    }
}

fn resolve_graph_abs_path(roots: &[RootEntry], graph_path: &str) -> PathBuf {
    if roots.len() == 1 {
        return roots[0].path.join(graph_path);
    }
    for r in roots {
        if let Some(rest) = graph_path
            .strip_prefix(r.prefix.as_str())
            .and_then(|s| s.strip_prefix('/'))
        {
            return r.path.join(rest);
        }
    }
    roots[0].path.join(graph_path)
}

fn enclosing_symbol_for_line(
    g: &ArchivedGraphCacheData,
    file_idx: u32,
    line: u32,
) -> Option<String> {
    let fi = file_idx as usize;
    if fi + 1 >= g.def_offsets.len() {
        return None;
    }
    let ds = g.def_offsets[fi].to_native() as usize;
    let de = g.def_offsets[fi + 1].to_native() as usize;
    let mut best: Option<(&str, u32, u32, u32)> = None;
    for &si_r in &g.def_syms[ds..de] {
        let sym = &g.symbols[si_r.to_native() as usize];
        let ls = sym.line_start.to_native();
        let le = sym.line_end.to_native();
        if line >= ls && line <= le {
            let span = le.saturating_sub(ls);
            if best.map_or(true, |(_, _, _, bs)| span < bs) {
                best = Some((sym.name.as_str(), ls, le, span));
            }
        }
    }
    best.map(|(n, ls, le, _)| format!("in {n} {ls}-{le}"))
}

const GENERIC_FILE_STEMS: &[&str] = &[
    "shared", "index", "utils", "util", "types", "type", "config", "page", "route", "main",
    "app", "client", "server", "helpers", "helper", "constants", "errors", "models", "model",
    "schema", "test", "tests", "init", "common", "base", "core", "lib", "api", "hooks",
    "styles", "layout", "loading", "error",
];

pub fn is_generic_file_stem(stem: &str) -> bool {
    GENERIC_FILE_STEMS.contains(&stem.to_ascii_lowercase().as_str())
}

/// Fast parallel ripgrep-crate scan over non-code/config files for a symbol or file stem.
/// Uses cached `noncode_files` from `.brbin` when available (zero filesystem walk!).
pub fn find_noncode_mentions(
    roots: &[RootEntry],
    graph: Option<&ArchivedGraphCacheData>,
    term: &str,
    max_files: usize,
    max_hits_per_file: usize,
) -> (Vec<TextFileHits>, usize) {
    find_noncode_mentions_with_pattern(roots, graph, term, None, max_files, max_hits_per_file)
}

/// Same as `find_noncode_mentions`, with an optional custom regex (e.g. path-qualified regex for
/// generic filenames like `shared.ts` so we don't match prose `"shared"` across 75 unrelated files).
pub fn find_noncode_mentions_with_pattern(
    roots: &[RootEntry],
    graph: Option<&ArchivedGraphCacheData>,
    term: &str,
    custom_regex: Option<&str>,
    max_files: usize,
    max_hits_per_file: usize,
) -> (Vec<TextFileHits>, usize) {
    if term.len() < 3 && custom_regex.is_none() {
        return (Vec::new(), 0);
    }
    let pat = match custom_regex {
        Some(r) => r.to_string(),
        None => {
            let escaped = regex::escape(term);
            format!(r"\b{escaped}\b")
        }
    };
    let matcher = match RegexMatcherBuilder::new().build(&pat) {
        Ok(m) => m,
        Err(_) => return (Vec::new(), 0),
    };

    let candidates: Vec<(String, PathBuf)> = if let Some(g) = graph {
        g.noncode_files
            .iter()
            .filter_map(|p| {
                let gp = p.as_str();
                if !is_noncode_config_file(gp) {
                    return None;
                }
                let abs = resolve_graph_abs_path(roots, gp);
                Some((gp.to_string(), abs))
            })
            .collect()
    } else {
        let skip_set = std::sync::Arc::new(
            NONCODE_SKIP_DIRS
                .iter()
                .copied()
                .collect::<FxHashSet<&'static str>>(),
        );
        roots
            .par_iter()
            .flat_map(|r| {
                walk_root_parallel(r, &skip_set, 4 * 1024 * 1024, |rel_str, _| {
                    is_noncode_config_file(rel_str)
                })
            })
            .collect()
    };

    let mut matched: Vec<TextFileHits> = candidates
        .into_par_iter()
        .filter_map(|(graph_path, abs)| {
            let mut searcher = SearcherBuilder::new().line_number(true).build();
            let mut hits = Vec::new();
            let mut total_in_file = 0usize;
            let _ = searcher.search_path(
                &matcher,
                &abs,
                UTF8(|lnum, line| {
                    total_in_file += 1;
                    if hits.len() < max_hits_per_file {
                        hits.push(TextMatchLine {
                            line: lnum,
                            content: truncate_line(line.trim(), 140),
                            is_context: false,
                            enclosing_sym: None,
                        });
                    }
                    Ok(true)
                }),
            );
            if total_in_file == 0 {
                return None;
            }
            let priority = noncode_priority(&graph_path);
            Some(TextFileHits {
                file: graph_path,
                hits,
                total_hits_in_file: total_in_file,
                rank_score: priority,
                matched_branches: Vec::new(),
                enclosing_syms: Vec::new(),
            })
        })
        .collect();

    matched.sort_by(|a, b| {
        b.rank_score
            .total_cmp(&a.rank_score)
            .then_with(|| a.file.cmp(&b.file))
    });
    let total_files = matched.len();
    matched.truncate(max_files);
    (matched, total_files)
}

fn noncode_priority(path: &str) -> f32 {
    let lower = path.to_ascii_lowercase();
    if lower.contains("dockerfile") || lower.contains(".github/workflows/") {
        return 10.0;
    }
    if lower.ends_with(".prisma") || lower.ends_with(".sql") {
        return 9.0;
    }
    if lower.ends_with("pyproject.toml") || lower.ends_with("package.json") {
        return 7.0;
    }
    if lower.ends_with(".json") || lower.ends_with(".yaml") || lower.ends_with(".yml") {
        return 5.0;
    }
    3.0
}

/// Split top-level `|` alternation branches (respecting escapes, `(...)`, `[...]`).
fn split_top_level_alternation(pattern: &str) -> Vec<String> {
    let mut branches = Vec::new();
    let mut cur = String::new();
    let mut paren_depth = 0usize;
    let mut in_bracket = false;
    let mut escaped = false;

    for ch in pattern.chars() {
        if escaped {
            cur.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            cur.push(ch);
            escaped = true;
            continue;
        }
        if in_bracket {
            cur.push(ch);
            if ch == ']' {
                in_bracket = false;
            }
            continue;
        }
        match ch {
            '[' => {
                in_bracket = true;
                cur.push(ch);
            }
            '(' => {
                paren_depth += 1;
                cur.push(ch);
            }
            ')' => {
                paren_depth = paren_depth.saturating_sub(1);
                cur.push(ch);
            }
            '|' if paren_depth == 0 => {
                if !cur.is_empty() {
                    branches.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        branches.push(cur);
    }
    branches
}

#[derive(Debug, Default)]
pub struct GrepOpts {
    pub case_insensitive: bool,
    pub fixed_strings: bool,
    pub word_regexp: bool,
    pub noncode_only: bool,
    pub code_only: bool,
    pub include_tests: bool,
    pub include_data: bool,
    pub no_tests_explicit: bool,
    pub no_imports: bool,
    pub no_comments: bool,
    pub files_only: bool,
    pub compact: bool,
    pub limit_files: usize,
    pub max_per_file: usize,
    pub user_max_per_file: bool,
    pub before_context: usize,
    pub after_context: usize,
    pub path_filters: Vec<String>,
    pub exclude_paths: Vec<String>,
    pub exts: Vec<String>,
    pub types: Vec<String>,
    pub globs: Vec<String>,
    pub and_patterns: Vec<String>,
    pub extra_patterns: Vec<String>,
}

#[derive(Debug, Default)]
pub struct GrepResult {
    pub hits: Vec<TextFileHits>,
    pub total_files: usize,
    pub total_hits: usize,
    pub hidden_test_files: usize,
    pub hidden_test_hits: usize,
    pub hidden_test_file_paths: Vec<String>,
    pub auto_included_tests: bool,
    pub path_matched_zero: bool,
    pub branch_counts: Vec<(String, usize)>,
    /// Per-branch counts split across `(branch_label, code_files, noncode_files, test_files)`
    pub branch_scope_counts: Vec<(String, usize, usize, usize)>,
    pub repo_counts: Vec<(String, usize)>,
}

#[derive(Debug, Clone)]
struct MatchRecord {
    line: u64,
    is_comment: bool,
    branches: Vec<usize>,
}

struct GrepSink<'a> {
    max_matches: usize,
    buffer_cap: usize,
    total_matches: usize,
    total_noncomment: usize,
    captured_matches: usize,
    captured_noncomment: usize,
    no_imports: bool,
    no_comments: bool,
    skip_lines: Option<&'a FxHashSet<u64>>,
    collect_inventory_syms: bool,
    branch_regexes: &'a [(String, regex::Regex)],
    seen_branches: Vec<bool>,
    captured_branches: Vec<bool>,
    records: Vec<MatchRecord>,
    hits: Vec<TextMatchLine>,
    inventory_syms: Vec<String>,
    graph: Option<&'a ArchivedGraphCacheData>,
    file_idx: Option<u32>,
    last_matched_captured: bool,
}

impl<'a> Sink for GrepSink<'a> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        let lnum = mat.line_number().unwrap_or(0);
        if let Some(skip) = self.skip_lines {
            if skip.contains(&lnum) {
                self.last_matched_captured = false;
                return Ok(true);
            }
        }
        let text = String::from_utf8_lossy(mat.bytes());
        if self.no_imports && is_import_only_line(&text) {
            self.last_matched_captured = false;
            return Ok(true);
        }
        let is_comment = is_comment_only_line(&text);
        if self.no_comments && is_comment {
            self.last_matched_captured = false;
            return Ok(true);
        }
        self.total_matches += 1;
        if !is_comment {
            self.total_noncomment += 1;
        }

        let mut line_branches = Vec::new();
        let mut introduces_uncaptured_branch = false;
        if !self.branch_regexes.is_empty() {
            for (bi, (_, re)) in self.branch_regexes.iter().enumerate() {
                if re.is_match(&text) {
                    self.seen_branches[bi] = true;
                    line_branches.push(bi);
                    if !self.captured_branches[bi] {
                        introduces_uncaptured_branch = true;
                    }
                }
            }
        }

        let want_capture = self.max_matches > 0
            && (self.captured_matches < self.buffer_cap
                || (!is_comment && self.captured_noncomment < self.max_matches)
                || introduces_uncaptured_branch);

        // Collect up to 5 unique enclosing symbol names for `--files-only` / `--inventory`
        let mut resolved_enclosing: Option<String> = None;
        if self.collect_inventory_syms && self.inventory_syms.len() < 5 {
            if let (Some(g), Some(fi)) = (self.graph, self.file_idx) {
                if let Some(enc) = enclosing_symbol_for_line(g, fi, lnum as u32) {
                    // Strip `in <name> <start>-<end>` -> `<name>` for compact inventory listing
                    let short = enc
                        .strip_prefix("in ")
                        .and_then(|s| s.split_whitespace().next())
                        .unwrap_or(&enc)
                        .to_string();
                    if !short.is_empty() && !self.inventory_syms.contains(&short) {
                        self.inventory_syms.push(short);
                    }
                    resolved_enclosing = Some(enc);
                }
            }
        }

        if want_capture {
            self.captured_matches += 1;
            if !is_comment {
                self.captured_noncomment += 1;
            }
            for &bi in &line_branches {
                self.captured_branches[bi] = true;
            }
            let enclosing = resolved_enclosing.or_else(|| match (self.graph, self.file_idx) {
                (Some(g), Some(fi)) => enclosing_symbol_for_line(g, fi, lnum as u32),
                _ => None,
            });
            self.records.push(MatchRecord {
                line: lnum,
                is_comment,
                branches: line_branches,
            });
            self.hits.push(TextMatchLine {
                line: lnum,
                content: truncate_line(&text, 180),
                is_context: false,
                enclosing_sym: enclosing,
            });
            self.last_matched_captured = true;
        } else {
            self.last_matched_captured = false;
        }
        Ok(true)
    }

    fn context(&mut self, _searcher: &Searcher, ctx: &SinkContext<'_>) -> Result<bool, Self::Error> {
        let keep = self.max_matches > 0
            && match ctx.kind() {
                SinkContextKind::Before => {
                    self.captured_matches < self.buffer_cap
                        || self.captured_noncomment < self.max_matches
                        || self.captured_branches.iter().any(|&c| !c)
                }
                SinkContextKind::After => self.last_matched_captured,
                _ => false,
            };
        if keep {
            let lnum = ctx.line_number().unwrap_or(0);
            if self.hits.last().map_or(true, |prev| prev.line != lnum) {
                let text = String::from_utf8_lossy(ctx.bytes());
                self.hits.push(TextMatchLine {
                    line: lnum,
                    content: truncate_line(&text, 180),
                    is_context: true,
                    enclosing_sym: None,
                });
            }
        }
        Ok(true)
    }
}

/// Select up to `max_matches` match lines from a file's buffered records:
/// 1) Demote comment-only lines below real code lines so `# comments` never hide actual code.
/// 2) When multi-branch alternation is active, ensure rarest branches in the file get slots first
///    (round-robin across branches) so a noisy branch can't starve a rare branch in the same file!
fn finalize_file_hits(
    hits: Vec<TextMatchLine>,
    records: &[MatchRecord],
    max_matches: usize,
    num_branches: usize,
    before_ctx: usize,
    after_ctx: usize,
) -> Vec<TextMatchLine> {
    if max_matches == 0 {
        return Vec::new();
    }
    if records.len() <= max_matches {
        return hits;
    }

    let mut chosen_lines: FxHashSet<u64> = FxHashSet::default();
    let pick_pool = |pool: &[&MatchRecord], chosen: &mut FxHashSet<u64>| {
        if pool.is_empty() || chosen.len() >= max_matches {
            return;
        }
        if num_branches >= 2 {
            // Count occurrences per branch inside this pool and pick rarest branches first!
            let mut per_b: Vec<Vec<u64>> = vec![Vec::new(); num_branches];
            for r in pool {
                for &bi in &r.branches {
                    if bi < num_branches {
                        per_b[bi].push(r.line);
                    }
                }
            }
            let mut b_order: Vec<usize> = (0..num_branches)
                .filter(|&bi| !per_b[bi].is_empty())
                .collect();
            b_order.sort_by_key(|&bi| per_b[bi].len());
            let mut cursors = vec![0usize; num_branches];
            loop {
                let mut progressed = false;
                for &bi in &b_order {
                    if chosen.len() >= max_matches {
                        break;
                    }
                    while cursors[bi] < per_b[bi].len() {
                        let ln = per_b[bi][cursors[bi]];
                        cursors[bi] += 1;
                        if chosen.insert(ln) {
                            progressed = true;
                            break;
                        }
                    }
                }
                if !progressed || chosen.len() >= max_matches {
                    break;
                }
            }
        }
        for r in pool {
            if chosen.len() >= max_matches {
                break;
            }
            chosen.insert(r.line);
        }
    };

    let code_recs: Vec<&MatchRecord> = records.iter().filter(|r| !r.is_comment).collect();
    let comment_recs: Vec<&MatchRecord> = records.iter().filter(|r| r.is_comment).collect();
    pick_pool(&code_recs, &mut chosen_lines);
    pick_pool(&comment_recs, &mut chosen_lines);

    let mut chosen_sorted: Vec<u64> = chosen_lines.iter().copied().collect();
    chosen_sorted.sort_unstable();

    hits.into_iter()
        .filter(|h| {
            if !h.is_context {
                return chosen_lines.contains(&h.line);
            }
            chosen_sorted.iter().any(|&m| {
                h.line >= m.saturating_sub(before_ctx as u64) && h.line <= m + after_ctx as u64
            })
        })
        .collect()
}

fn path_explicitly_targets_tests(path_filters: &[String]) -> bool {
    path_filters.iter().any(|pf| {
        let low = pf.to_ascii_lowercase();
        is_test_or_fixture_path(pf)
            || low.contains("test")
            || low.contains("spec")
            || low.contains("fixture")
            || low.contains("benchmark")
    })
}

/// First-class `betterrank grep <pattern>`: ripgrep-speed search across all files
/// (code + configs + Dockerfiles + Prisma + YAML + JSON), ranked code-first by PageRank,
/// annotated with enclosing symbols + ranges (when warm), demoting tests/fixtures
/// by default, and balancing multi-term alternations.
pub fn ranked_grep(
    roots: &[RootEntry],
    pattern: &str,
    graph: Option<&ArchivedGraphCacheData>,
    opts: &GrepOpts,
) -> Result<GrepResult, String> {
    // Combine primary pattern with any `-e` patterns
    let (combined_pattern, branches): (String, Vec<String>) = if !opts.extra_patterns.is_empty() {
        let mut all_p = Vec::with_capacity(1 + opts.extra_patterns.len());
        if !pattern.is_empty() {
            all_p.push(pattern.to_string());
        }
        all_p.extend(opts.extra_patterns.iter().cloned());
        let joined = if opts.fixed_strings {
            all_p
                .iter()
                .map(|p| regex::escape(p))
                .collect::<Vec<_>>()
                .join("|")
        } else {
            all_p
                .iter()
                .map(|p| format!("(?:{p})"))
                .collect::<Vec<_>>()
                .join("|")
        };
        (joined, all_p)
    } else if !opts.fixed_strings {
        let split = split_top_level_alternation(pattern);
        if split.len() >= 2 && split.len() <= 20 {
            (pattern.to_string(), split)
        } else {
            (pattern.to_string(), Vec::new())
        }
    } else {
        (pattern.to_string(), Vec::new())
    };

    let use_fixed = opts.fixed_strings && opts.extra_patterns.is_empty();
    let mut builder = RegexMatcherBuilder::new();
    builder.case_insensitive(opts.case_insensitive);
    builder.fixed_strings(use_fixed);
    builder.word(opts.word_regexp);
    let matcher = builder
        .build(&combined_pattern)
        .map_err(|e| format!("Invalid grep pattern: {e}"))?;

    // Compile per-branch line regexes for single-pass branch tagging & rarest-branch-first quotas
    let branch_regexes: Vec<(String, regex::Regex)> = branches
        .iter()
        .filter_map(|b| {
            let core = if opts.fixed_strings {
                regex::escape(b)
            } else {
                b.clone()
            };
            let pat = if opts.word_regexp {
                format!(r"\b(?:{core})\b")
            } else {
                core
            };
            regex::RegexBuilder::new(&pat)
                .case_insensitive(opts.case_insensitive)
                .build()
                .ok()
                .map(|re| (b.clone(), re))
        })
        .collect();

    // Compile optional `--and <regex>` co-occurrence matchers (every matched file must match all)
    let mut and_matchers: Vec<grep_regex::RegexMatcher> = Vec::with_capacity(opts.and_patterns.len());
    for ap in &opts.and_patterns {
        let mut ab = RegexMatcherBuilder::new();
        ab.case_insensitive(opts.case_insensitive);
        ab.fixed_strings(opts.fixed_strings);
        ab.word(opts.word_regexp);
        let m = ab
            .build(ap)
            .map_err(|e| format!("Invalid --and pattern \"{ap}\": {e}"))?;
        and_matchers.push(m);
    }

    // Compile positive & negative `--glob` patterns (e.g. `*Dockerfile*`, `*.yml`, `!*.test.ts`)
    let mut pos_globs: Vec<regex::Regex> = Vec::new();
    let mut neg_globs: Vec<regex::Regex> = Vec::new();
    for g in &opts.globs {
        let trimmed = g.trim();
        if let Some(neg) = trimmed.strip_prefix('!') {
            if let Some(re) = compile_glob_regex(neg) {
                neg_globs.push(re);
            }
        } else if let Some(re) = compile_glob_regex(trimmed) {
            pos_globs.push(re);
        }
    }

    // Map file -> (file_idx, PageRank score) from zero-copy mmap graph (if available)
    let mut file_meta: FxHashMap<&str, (u32, f32)> = FxHashMap::default();
    if let Some(g) = graph {
        file_meta.reserve(g.files.len());
        for (i, f) in g.files.iter().enumerate() {
            file_meta.insert(f.path.as_str(), (i as u32, f.file_score.to_native()));
        }
    }

    let noncode_only = opts.noncode_only;
    let code_only = opts.code_only;
    let include_data = opts.include_data
        || noncode_only
        || !opts.types.is_empty()
        || !opts.exts.is_empty()
        || !pos_globs.is_empty();
    let effective_include_tests =
        opts.include_tests || (!opts.no_tests_explicit && path_explicitly_targets_tests(&opts.path_filters));
    let path_filters = opts.path_filters.clone();
    let exclude_paths = opts.exclude_paths.clone();
    let exts = opts.exts.clone();
    let types = opts.types.clone();

    let candidates: Vec<(String, PathBuf)> = if noncode_only && graph.is_some() {
        let g = graph.unwrap();
        g.noncode_files
            .iter()
            .filter_map(|p| {
                let gp = p.as_str();
                if !is_noncode_config_file(gp) {
                    return None;
                }
                if !matches_ext_filter(gp, &exts)
                    || !matches_type_filter(gp, &types)
                    || !matches_glob_filters(gp, &pos_globs, &neg_globs)
                {
                    return None;
                }
                if !path_filters.is_empty()
                    && !path_filters.iter().any(|pf| matches_path_filter(gp, pf))
                {
                    return None;
                }
                if exclude_paths.iter().any(|ep| matches_path_filter(gp, ep)) {
                    return None;
                }
                let abs = resolve_graph_abs_path(roots, gp);
                Some((gp.to_string(), abs))
            })
            .collect()
    } else {
        let skip_set = std::sync::Arc::new(
            NONCODE_SKIP_DIRS
                .iter()
                .copied()
                .collect::<FxHashSet<&'static str>>(),
        );
        let pos_globs_arc = std::sync::Arc::new(pos_globs);
        let neg_globs_arc = std::sync::Arc::new(neg_globs);
        roots
            .par_iter()
            .flat_map(|r| {
                let pfs = path_filters.clone();
                let eps = exclude_paths.clone();
                let exts_c = exts.clone();
                let types_c = types.clone();
                let pg = std::sync::Arc::clone(&pos_globs_arc);
                let ng = std::sync::Arc::clone(&neg_globs_arc);
                walk_root_parallel(r, &skip_set, 4 * 1024 * 1024, move |rel_str, graph_path| {
                    let base = rel_str.rsplit('/').next().unwrap_or(rel_str);
                    if NONCODE_SKIP_FILES
                        .iter()
                        .any(|&s| base.eq_ignore_ascii_case(s))
                    {
                        return false;
                    }
                    if noncode_only && !is_noncode_config_file(rel_str) {
                        return false;
                    }
                    if code_only
                        && !is_supported_extension(rel_str)
                        && !is_schema_or_idl_extension(rel_str)
                    {
                        return false;
                    }
                    if !matches_ext_filter(rel_str, &exts_c)
                        || !matches_type_filter(rel_str, &types_c)
                        || !matches_glob_filters(rel_str, &pg, &ng)
                    {
                        return false;
                    }
                    if !pfs.is_empty()
                        && !pfs.iter().any(|pf| matches_path_filter(graph_path, pf))
                    {
                        return false;
                    }
                    if eps.iter().any(|ep| matches_path_filter(graph_path, ep)) {
                        return false;
                    }
                    true
                })
            })
            .filter(|(gp, abs)| {
                if include_data {
                    return true;
                }
                let sz = std::fs::metadata(abs).map(|m| m.len()).unwrap_or(0);
                !is_bulk_data_or_locale_file(gp, sz)
            })
            .collect()
    };

    if candidates.is_empty() && !path_filters.is_empty() {
        return Ok(GrepResult {
            path_matched_zero: true,
            ..GrepResult::default()
        });
    }

    // Auto-raise `--max-per-file` to `--limit` when `--path` targets a single file!
    let effective_max_per_file = if opts.files_only {
        0
    } else if !opts.user_max_per_file && candidates.len() == 1 && !path_filters.is_empty() {
        opts.limit_files.max(opts.max_per_file).max(30)
    } else {
        opts.max_per_file.max(1)
    };
    let before_ctx = if opts.files_only || opts.compact {
        0
    } else {
        opts.before_context.min(15)
    };
    let after_ctx = if opts.files_only || opts.compact {
        0
    } else {
        opts.after_context.min(15)
    };

    struct ScoredMatch {
        file_hits: TextFileHits,
        is_test_or_fix: bool,
        is_code_file: bool,
        repo_idx: usize,
        matched_branches: Vec<usize>,
    }

    let prefer_code = !noncode_only && types.is_empty();
    let no_imports = opts.no_imports;
    let no_comments = opts.no_comments;
    let collect_inventory_syms = opts.files_only;
    let num_branches = branch_regexes.len();
    let buffer_cap = (effective_max_per_file * 8).clamp(32, 64);

    let mut all_scored: Vec<ScoredMatch> = candidates
        .into_par_iter()
        .filter_map(|(graph_path, abs)| {
            let mut sb = SearcherBuilder::new();
            sb.line_number(true)
                .binary_detection(grep_searcher::BinaryDetection::quit(b'\x00'));
            if before_ctx > 0 {
                sb.before_context(before_ctx);
            }
            if after_ctx > 0 {
                sb.after_context(after_ctx);
            }
            let mut searcher = sb.build();

            let (file_idx, base_score) = match file_meta.get(graph_path.as_str()) {
                Some(&(fi, sc)) => (Some(fi), sc),
                None => {
                    let sc = if is_noncode_config_file(&graph_path) {
                        0.0005 * noncode_priority(&graph_path)
                    } else {
                        0.0001
                    };
                    (None, sc)
                }
            };

            // When `--no-comments` is active on a `.py` file, strip Python triple-quoted docstrings too
            let py_skip = if no_comments && graph_path.to_ascii_lowercase().ends_with(".py") {
                Some(detect_python_docstring_lines(&abs))
            } else {
                None
            };

            let mut sink = GrepSink {
                max_matches: effective_max_per_file,
                buffer_cap,
                total_matches: 0,
                total_noncomment: 0,
                captured_matches: 0,
                captured_noncomment: 0,
                no_imports,
                no_comments,
                skip_lines: py_skip.as_ref(),
                collect_inventory_syms,
                branch_regexes: &branch_regexes,
                seen_branches: vec![false; num_branches],
                captured_branches: vec![false; num_branches],
                records: Vec::new(),
                hits: Vec::new(),
                inventory_syms: Vec::new(),
                graph,
                file_idx,
                last_matched_captured: false,
            };
            let _ = searcher.search_path(&matcher, &abs, &mut sink);
            if sink.total_matches == 0 {
                return None;
            }

            // Enforce `--and <regex>` same-file co-occurrence (runs ONLY on files that matched primary pattern!)
            if !and_matchers.is_empty() {
                let mut and_searcher = SearcherBuilder::new()
                    .binary_detection(grep_searcher::BinaryDetection::quit(b'\x00'))
                    .build();
                for am in &and_matchers {
                    let mut found = false;
                    let _ = and_searcher.search_path(
                        am,
                        &abs,
                        UTF8(|lnum, line| {
                            if let Some(ref skip) = py_skip {
                                if skip.contains(&lnum) {
                                    return Ok(true);
                                }
                            }
                            if no_comments && is_comment_only_line(line) {
                                return Ok(true);
                            }
                            found = true;
                            Ok(false)
                        }),
                    );
                    if !found {
                        return None;
                    }
                }
            }

            let mut matched_branches = Vec::new();
            let mut matched_branch_names = Vec::new();
            if num_branches >= 2 {
                for (bi, &seen) in sink.seen_branches.iter().enumerate() {
                    if seen {
                        matched_branches.push(bi);
                        matched_branch_names.push(branch_regexes[bi].0.clone());
                    }
                }
            }

            let final_hits = finalize_file_hits(
                sink.hits,
                &sink.records,
                effective_max_per_file,
                num_branches,
                before_ctx,
                after_ctx,
            );

            let is_tf = is_test_or_fixture_path(&graph_path);
            let is_code_file =
                is_supported_extension(&graph_path) || is_schema_or_idl_extension(&graph_path);
            let is_dts = graph_path.to_ascii_lowercase().ends_with(".d.ts");
            let comment_only_file = sink.total_noncomment == 0;

            // Rank runtime source code ahead of `.json`/`.md`/`.yaml` unless `--non-code` or `--type` is requested
            let mut score = if prefer_code && is_code_file {
                base_score.max(0.0002) + 0.05
            } else {
                base_score
            };
            if is_dts {
                score *= 0.2;
            }
            if comment_only_file {
                score *= 0.25;
            }
            if is_tf {
                score *= 0.15;
            }

            let repo_idx = if roots.len() > 1 {
                roots
                    .iter()
                    .position(|r| graph_path.starts_with(&format!("{}/", r.prefix)))
                    .unwrap_or(0)
            } else {
                0
            };

            Some(ScoredMatch {
                file_hits: TextFileHits {
                    file: graph_path,
                    hits: final_hits,
                    total_hits_in_file: sink.total_matches,
                    rank_score: score,
                    matched_branches: matched_branch_names,
                    enclosing_syms: sink.inventory_syms,
                },
                is_test_or_fix: is_tf,
                is_code_file,
                repo_idx,
                matched_branches,
            })
        })
        .collect();

    all_scored.sort_by(|a, b| {
        b.file_hits
            .rank_score
            .total_cmp(&a.file_hits.rank_score)
            .then_with(|| a.file_hits.file.cmp(&b.file_hits.file))
    });

    // Compute per-branch scope counts (code / non-code / tests) across ALL matched files
    // BEFORE hiding tests, so agents immediately distinguish 0/0/0 from test/doc-only terms!
    let mut branch_scope_counts: Vec<(String, usize, usize, usize)> = Vec::new();
    if num_branches >= 2 {
        let mut sc = vec![(0usize, 0usize, 0usize); num_branches];
        for item in &all_scored {
            for &bi in &item.matched_branches {
                if item.is_test_or_fix {
                    sc[bi].2 += 1;
                } else if item.is_code_file {
                    sc[bi].0 += 1;
                } else {
                    sc[bi].1 += 1;
                }
            }
        }
        for (i, (label, _)) in branch_regexes.iter().enumerate() {
            branch_scope_counts.push((label.clone(), sc[i].0, sc[i].1, sc[i].2));
        }
    }

    // Separate prod vs test/fixture unless `--include-tests` (or `--path` explicitly targets tests).
    // If prod has 0 matches and tests DO match (and `--no-tests` was not passed), automatically
    // promote test matches into the main output so test-only searches never return an empty body!
    let mut prod: Vec<ScoredMatch> = Vec::new();
    let mut hidden_tests: Vec<ScoredMatch> = Vec::new();

    for item in all_scored {
        if !effective_include_tests && item.is_test_or_fix {
            hidden_tests.push(item);
        } else {
            prod.push(item);
        }
    }

    let mut auto_included_tests = false;
    if prod.is_empty() && !hidden_tests.is_empty() && !opts.no_tests_explicit {
        prod = std::mem::take(&mut hidden_tests);
        auto_included_tests = true;
    }

    let hidden_test_files = hidden_tests.len();
    let hidden_test_hits: usize = hidden_tests
        .iter()
        .map(|m| m.file_hits.total_hits_in_file)
        .sum();
    let hidden_test_file_paths: Vec<String> = hidden_tests
        .iter()
        .take(15)
        .map(|m| m.file_hits.file.clone())
        .collect();

    // Compute per-branch file counts over the displayed pool (prod, or all if --include-tests)
    let mut branch_counts: Vec<(String, usize)> = Vec::new();
    if num_branches >= 2 {
        let mut counts = vec![0usize; num_branches];
        for item in &prod {
            for &bi in &item.matched_branches {
                counts[bi] += 1;
            }
        }
        for (i, (label, _)) in branch_regexes.iter().enumerate() {
            branch_counts.push((label.clone(), counts[i]));
        }
    }

    // Compute per-repo file counts when multiple `--root`s are active
    let mut repo_counts: Vec<(String, usize)> = Vec::new();
    if roots.len() > 1 {
        let mut rcounts = vec![0usize; roots.len()];
        for item in &prod {
            if item.repo_idx < rcounts.len() {
                rcounts[item.repo_idx] += 1;
            }
        }
        for (ri, r) in roots.iter().enumerate() {
            repo_counts.push((r.prefix.clone(), rcounts[ri]));
        }
    }

    let total_files = prod.len();
    let total_hits: usize = prod.iter().map(|m| m.file_hits.total_hits_in_file).sum();

    // Fair round-robin selection across BOTH rarest alternation branches AND multiple `--root` repos
    // so neither a 164-hit noisy term nor a 20K-file monorepo can starve rare terms or sibling repos!
    let cap = if opts.limit_files > 0 {
        opts.limit_files.min(prod.len())
    } else {
        prod.len()
    };
    let multi_repo = roots.len() > 1;
    let multi_branch = num_branches >= 2;

    let selected: Vec<TextFileHits> = if (multi_branch || multi_repo) && !prod.is_empty() && cap > 0 {
        let mut chosen_set = vec![false; prod.len()];
        let mut chosen_indices: Vec<usize> = Vec::with_capacity(cap);

        // Build per-branch buckets (rarest non-empty branch first)
        let mut per_branch: Vec<Vec<usize>> = vec![Vec::new(); num_branches];
        if multi_branch {
            for (idx, item) in prod.iter().enumerate() {
                for &bi in &item.matched_branches {
                    per_branch[bi].push(idx);
                }
            }
        }
        let mut branch_order: Vec<usize> = (0..num_branches)
            .filter(|&bi| !per_branch[bi].is_empty())
            .collect();
        branch_order.sort_by_key(|&bi| per_branch[bi].len());

        // Build per-repo buckets (rarest non-empty repo first)
        let mut per_repo: Vec<Vec<usize>> = vec![Vec::new(); roots.len()];
        if multi_repo {
            for (idx, item) in prod.iter().enumerate() {
                if item.repo_idx < per_repo.len() {
                    per_repo[item.repo_idx].push(idx);
                }
            }
        }
        let mut repo_order: Vec<usize> = (0..roots.len())
            .filter(|&ri| !per_repo[ri].is_empty())
            .collect();
        repo_order.sort_by_key(|&ri| per_repo[ri].len());

        let mut b_cursors = vec![0usize; num_branches];
        let mut r_cursors = vec![0usize; roots.len()];

        // Phase 1: if multi-repo, guarantee every non-empty repo gets its top matches first
        // (up to floor(cap / active_repos), min 2, max 5 per repo in initial reservation)
        if multi_repo && repo_order.len() > 1 {
            let reserve_rounds = (cap / repo_order.len()).clamp(1, 4);
            for _ in 0..reserve_rounds {
                for &ri in &repo_order {
                    if chosen_indices.len() >= cap {
                        break;
                    }
                    while r_cursors[ri] < per_repo[ri].len() {
                        let cand = per_repo[ri][r_cursors[ri]];
                        r_cursors[ri] += 1;
                        if !chosen_set[cand] {
                            chosen_set[cand] = true;
                            chosen_indices.push(cand);
                            break;
                        }
                    }
                }
            }
        }

        // Phase 2: rarest-branch-first round-robin (or repo round-robin if single branch)
        if multi_branch {
            loop {
                let mut progressed = false;
                for &bi in &branch_order {
                    if chosen_indices.len() >= cap {
                        break;
                    }
                    while b_cursors[bi] < per_branch[bi].len() {
                        let cand = per_branch[bi][b_cursors[bi]];
                        b_cursors[bi] += 1;
                        if !chosen_set[cand] {
                            chosen_set[cand] = true;
                            chosen_indices.push(cand);
                            progressed = true;
                            break;
                        }
                    }
                }
                if !progressed || chosen_indices.len() >= cap {
                    break;
                }
            }
        } else if multi_repo {
            loop {
                let mut progressed = false;
                for &ri in &repo_order {
                    if chosen_indices.len() >= cap {
                        break;
                    }
                    while r_cursors[ri] < per_repo[ri].len() {
                        let cand = per_repo[ri][r_cursors[ri]];
                        r_cursors[ri] += 1;
                        if !chosen_set[cand] {
                            chosen_set[cand] = true;
                            chosen_indices.push(cand);
                            progressed = true;
                            break;
                        }
                    }
                }
                if !progressed || chosen_indices.len() >= cap {
                    break;
                }
            }
        }

        // Fill any remaining slots in PageRank order
        if chosen_indices.len() < cap {
            for idx in 0..prod.len() {
                if chosen_indices.len() >= cap {
                    break;
                }
                if !chosen_set[idx] {
                    chosen_set[idx] = true;
                    chosen_indices.push(idx);
                }
            }
        }
        let mut prod_opt: Vec<Option<ScoredMatch>> = prod.into_iter().map(Some).collect();
        chosen_indices
            .into_iter()
            .filter_map(|i| prod_opt[i].take().map(|s| s.file_hits))
            .collect()
    } else {
        let mut v: Vec<TextFileHits> = prod.into_iter().map(|s| s.file_hits).collect();
        if cap < v.len() {
            v.truncate(cap);
        }
        v
    };

    Ok(GrepResult {
        hits: selected,
        total_files,
        total_hits,
        hidden_test_files,
        hidden_test_hits,
        hidden_test_file_paths,
        auto_included_tests,
        path_matched_zero: false,
        branch_counts,
        branch_scope_counts,
        repo_counts,
    })
}
