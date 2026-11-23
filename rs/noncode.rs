use crate::model::{ArchivedGraphCacheData, RootEntry};
use crate::walk::{is_test_file, normalize_slashes};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::UTF8;
use grep_searcher::SearcherBuilder;
use ignore::WalkBuilder;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::PathBuf;

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
    ".code-index",
    ".playwright-mcp",
    ".memsearch",
    ".cursor",
    "Pods",
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
];

/// High-signal non-code / infra / schema / contract surfaces that AST-only tools miss.
pub fn is_noncode_config_file(rel_path: &str) -> bool {
    let base = rel_path.rsplit('/').next().unwrap_or(rel_path);
    let lower_base = base.to_ascii_lowercase();
    if NONCODE_SKIP_FILES.iter().any(|&s| lower_base == s.to_ascii_lowercase()) {
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

#[derive(Debug, Clone)]
pub struct TextMatchLine {
    pub line: u64,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct TextFileHits {
    pub file: String,
    pub hits: Vec<TextMatchLine>,
    pub total_hits_in_file: usize,
    pub rank_score: f32,
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
                            if entry.metadata().map_or(false, |m| m.len() <= max_bytes) {
                                flusher.local.push((graph_path, abs.to_path_buf()));
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

/// Fast parallel ripgrep-crate scan over non-code/config files for a symbol or file stem.
/// Used automatically by `callers`, `dependents`, and `search` so agents never miss
/// Dockerfiles, CI workflows, Prisma schemas, SQL, or JSON contracts.
pub fn find_noncode_mentions(
    roots: &[RootEntry],
    term: &str,
    max_files: usize,
    max_hits_per_file: usize,
) -> (Vec<TextFileHits>, usize) {
    if term.len() < 3 {
        return (Vec::new(), 0);
    }
    let escaped = regex::escape(term);
    let pat = format!(r"\b{escaped}\b");
    let matcher = match RegexMatcherBuilder::new().build(&pat) {
        Ok(m) => m,
        Err(_) => return (Vec::new(), 0),
    };

    let skip_set = std::sync::Arc::new(
        NONCODE_SKIP_DIRS
            .iter()
            .copied()
            .collect::<FxHashSet<&'static str>>(),
    );
    let candidates: Vec<(String, PathBuf)> = roots
        .par_iter()
        .flat_map(|r| {
            walk_root_parallel(r, &skip_set, 512 * 1024, |rel_str, _| {
                is_noncode_config_file(rel_str)
            })
        })
        .collect();

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

#[derive(Debug, Default)]
pub struct GrepOpts {
    pub case_insensitive: bool,
    pub fixed_strings: bool,
    pub noncode_only: bool,
    pub no_tests: bool,
    pub files_only: bool,
    pub limit_files: usize,
    pub max_per_file: usize,
    pub path_filter: Option<String>,
}

/// First-class `betterrank grep <pattern>`: ripgrep-speed search across all files
/// (code + configs + Dockerfiles + Prisma + YAML + JSON), ranked by PageRank and
/// hard-bounded by default so agents never flood context with 450 KB raw dumps.
pub fn ranked_grep(
    roots: &[RootEntry],
    pattern: &str,
    graph: Option<&ArchivedGraphCacheData>,
    opts: &GrepOpts,
) -> Result<(Vec<TextFileHits>, usize, usize), String> {
    let mut builder = RegexMatcherBuilder::new();
    builder.case_insensitive(opts.case_insensitive);
    builder.fixed_strings(opts.fixed_strings);
    let matcher = builder
        .build(pattern)
        .map_err(|e| format!("Invalid grep pattern: {e}"))?;

    // Map file -> PageRank score from zero-copy mmap graph (if available)
    let mut file_scores: FxHashMap<&str, f32> = FxHashMap::default();
    if let Some(g) = graph {
        file_scores.reserve(g.files.len());
        for f in g.files.iter() {
            file_scores.insert(f.path.as_str(), f.file_score.to_native());
        }
    }

    let skip_set = std::sync::Arc::new(
        NONCODE_SKIP_DIRS
            .iter()
            .copied()
            .collect::<FxHashSet<&'static str>>(),
    );
    let noncode_only = opts.noncode_only;
    let no_tests = opts.no_tests;
    let path_filter = opts.path_filter.clone();

    let candidates: Vec<(String, PathBuf)> = roots
        .par_iter()
        .flat_map(|r| {
            let pf_opt = path_filter.clone();
            walk_root_parallel(r, &skip_set, 2 * 1024 * 1024, move |rel_str, graph_path| {
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
                if no_tests && is_test_file(graph_path) {
                    return false;
                }
                if let Some(ref pf) = pf_opt {
                    if !graph_path.contains(pf.as_str()) {
                        return false;
                    }
                }
                true
            })
        })
        .collect();

    let max_per_file = if opts.files_only {
        0
    } else {
        opts.max_per_file.max(1)
    };

    let mut matched: Vec<TextFileHits> = candidates
        .into_par_iter()
        .filter_map(|(graph_path, abs)| {
            let mut searcher = SearcherBuilder::new()
                .line_number(true)
                .binary_detection(grep_searcher::BinaryDetection::quit(b'\x00'))
                .build();
            let mut hits = Vec::new();
            let mut total_in_file = 0usize;
            let _ = searcher.search_path(
                &matcher,
                &abs,
                UTF8(|lnum, line| {
                    total_in_file += 1;
                    if hits.len() < max_per_file {
                        hits.push(TextMatchLine {
                            line: lnum,
                            content: truncate_line(line, 180),
                        });
                    }
                    Ok(true)
                }),
            );
            if total_in_file == 0 {
                return None;
            }
            let base_score = file_scores.get(graph_path.as_str()).copied().unwrap_or_else(|| {
                if is_noncode_config_file(&graph_path) {
                    0.0005 * noncode_priority(&graph_path)
                } else {
                    0.0001
                }
            });
            let score = if is_test_file(&graph_path) {
                base_score * 0.15
            } else {
                base_score
            };
            Some(TextFileHits {
                file: graph_path,
                hits,
                total_hits_in_file: total_in_file,
                rank_score: score,
            })
        })
        .collect();

    matched.sort_by(|a, b| {
        b.rank_score
            .total_cmp(&a.rank_score)
            .then_with(|| a.file.cmp(&b.file))
    });
    let total_files = matched.len();
    let total_hits: usize = matched.iter().map(|m| m.total_hits_in_file).sum();
    if opts.limit_files > 0 && matched.len() > opts.limit_files {
        matched.truncate(opts.limit_files);
    }
    Ok((matched, total_files, total_hits))
}
