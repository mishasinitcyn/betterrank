use crate::model::{is_supported_extension, ResolverCtx, RootEntry, TsPathMapping, WorkspacePackage};
use ignore::WalkBuilder;
use rustc_hash::FxHashSet;
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const FAST_PRUNE_DIRS: &[&str] = &[
    "node_modules",
    ".npm",
    ".yarn",
    "bower_components",
    "__pycache__",
    ".venv",
    "venv",
    "env",
    ".env",
    ".virtualenvs",
    "site-packages",
    ".eggs",
    ".tox",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".gradle",
    ".m2",
    ".bundle",
    "CMakeFiles",
    "dist",
    "build",
    "out",
    "coverage",
    ".next",
    ".nuxt",
    ".output",
    ".svelte-kit",
    ".angular",
    ".turbo",
    ".parcel-cache",
    ".cache",
    "Pods",
    ".terraform",
    ".serverless",
    "cdk.out",
    ".vercel",
    ".netlify",
    ".git",
    ".code-index",
    ".claude",
    ".cursor",
    ".nx",
    "target",
];

pub fn normalize_slashes(p: &str) -> String {
    p.replace('\\', "/")
}

/// Walk up from `start_dir` to find the enclosing `.git` directory/file.
/// Falls back cleanly to `start_dir`.
pub fn find_default_root(start_dir: &Path) -> PathBuf {
    let start = fs::canonicalize(start_dir).unwrap_or_else(|_| start_dir.to_path_buf());
    let mut dir = start.as_path();
    loop {
        if dir.join(".git").exists() {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent,
            _ => return start,
        }
    }
}

pub fn normalize_roots(raw_roots: &[PathBuf]) -> Vec<RootEntry> {
    let resolved: Vec<PathBuf> = raw_roots
        .iter()
        .map(|r| fs::canonicalize(r).unwrap_or_else(|_| r.clone()))
        .collect();

    if resolved.len() == 1 {
        return vec![RootEntry {
            path: resolved[0].clone(),
            prefix: String::new(),
        }];
    }

    let mut base_counts = rustc_hash::FxHashMap::<String, usize>::default();
    for r in &resolved {
        let b = r
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "root".to_string());
        *base_counts.entry(b).or_insert(0) += 1;
    }

    let mut seen = rustc_hash::FxHashMap::<String, usize>::default();
    resolved
        .into_iter()
        .map(|r| {
            let b = r
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "root".to_string());
            let prefix = if base_counts.get(&b).copied().unwrap_or(0) == 1 {
                b
            } else {
                let idx = seen.entry(b.clone()).or_insert(0);
                *idx += 1;
                format!("{b}-{idx}")
            };
            RootEntry { path: r, prefix }
        })
        .collect()
}

pub fn is_test_file(path: &str) -> bool {
    let lower = format!("/{}", path.to_ascii_lowercase());
    for seg in &[
        "/test/",
        "/tests/",
        "/__tests__/",
        "/spec/",
        "/specs/",
        "/testing/",
        "/fixtures/",
        "/mocks/",
        "/e2e/",
        "/cypress/",
    ] {
        if lower.contains(seg) {
            return true;
        }
    }
    let file_name = path.rsplit('/').next().unwrap_or(path);
    let stem = match file_name.rfind('.') {
        Some(dot) => &file_name[..dot],
        None => file_name,
    }
    .to_ascii_lowercase();

    stem.starts_with("test_")
        || stem.starts_with("test.")
        || stem.ends_with(".test")
        || stem.ends_with(".spec")
        || stem.ends_with("_test")
        || stem.ends_with("_spec")
}

pub fn should_ignore_rel_path(rel_path: &str, extra_ignores: &[String]) -> bool {
    // Root-level vendor/ and tmp/ only (never hide src/**/vendor/)
    if rel_path.starts_with("vendor/") || rel_path == "vendor" {
        return true;
    }
    if rel_path.starts_with("tmp/") || rel_path == "tmp" {
        return true;
    }
    if rel_path.contains("/components/ui/") || rel_path.starts_with("components/ui/") {
        return true;
    }
    // File suffix filters
    let lower = rel_path.to_ascii_lowercase();
    if lower.ends_with(".min.js")
        || lower.ends_with(".min.mjs")
        || lower.ends_with(".min.cjs")
        || lower.ends_with(".bundle.js")
        || lower.ends_with(".bundle.mjs")
        || lower.ends_with(".chunk.js")
        || lower.ends_with(".map")
    {
        return true;
    }
    for pat in extra_ignores {
        let p = pat.trim_matches('*').trim_matches('/');
        if !p.is_empty() && (rel_path.starts_with(p) || rel_path.contains(&format!("/{p}/"))) {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone)]
pub struct WalkedFile {
    pub rel_path: String,
    pub mtime_ms: u64,
    pub size: u64,
}

/// Load extra ignore patterns from `<root>/.code-index/config.json`.
pub fn load_root_extra_ignores(root: &Path) -> Vec<String> {
    let cfg_path = root.join(".code-index/config.json");
    let raw = match fs::read_to_string(cfg_path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    parsed
        .get("ignore")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// Fast parallel file walker using ripgrep's `ignore` crate (respects `.gitignore`,
/// skips hidden/build/vendor directories before descending, filters to supported extensions).
pub fn walk_source_files(root: &Path, extra_ignores: &[String]) -> Vec<WalkedFile> {
    use ignore::WalkState;
    use std::sync::{Arc, Mutex};

    let prune_set: Arc<FxHashSet<&'static str>> = Arc::new(FAST_PRUNE_DIRS.iter().copied().collect());
    let root_buf = Arc::new(root.to_path_buf());
    let extra_owned = Arc::new(extra_ignores.to_vec());
    let out: Arc<Mutex<Vec<WalkedFile>>> = Arc::new(Mutex::new(Vec::with_capacity(2048)));

    let threads = std::thread::available_parallelism()
        .map(|n| n.get().clamp(2, 12))
        .unwrap_or(4);

    let prune_for_filter = Arc::clone(&prune_set);
    let walker = WalkBuilder::new(root)
        .standard_filters(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(false)
        .hidden(false)
        .threads(threads)
        .filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            let name = entry.file_name().to_string_lossy();
            if entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                if prune_for_filter.contains(name.as_ref()) {
                    return false;
                }
                if name.starts_with("cmake-build-")
                    || name.ends_with(".egg-info")
                    || name.ends_with(".xcframework")
                {
                    return false;
                }
                // Root-only vendor/ and tmp/
                if entry.depth() == 1 && (name == "vendor" || name == "tmp") {
                    return false;
                }
            }
            true
        })
        .build_parallel();

    walker.run(|| {
        let root_buf = Arc::clone(&root_buf);
        let extra_owned = Arc::clone(&extra_owned);
        let out = Arc::clone(&out);
        let mut local: Vec<WalkedFile> = Vec::with_capacity(256);

        struct Flusher {
            local: Vec<WalkedFile>,
            out: Arc<Mutex<Vec<WalkedFile>>>,
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
            local: std::mem::take(&mut local),
            out,
        };

        Box::new(move |res| {
            if let Ok(entry) = res {
                if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                    let name = entry.file_name().to_string_lossy();
                    if is_supported_extension(&name) {
                        if let Ok(rel) = entry.path().strip_prefix(&*root_buf) {
                            let rel_str = normalize_slashes(&rel.to_string_lossy());
                            if !should_ignore_rel_path(&rel_str, &extra_owned) {
                                if let Ok(meta) = entry.metadata() {
                                    let mtime_ms = meta
                                        .modified()
                                        .ok()
                                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                                        .map(|d| d.as_millis() as u64)
                                        .unwrap_or(0);
                                    flusher.local.push(WalkedFile {
                                        rel_path: rel_str,
                                        mtime_ms,
                                        size: meta.len(),
                                    });
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

/// Detect minified / bundled / generated source files that waste parser time.
pub fn is_minified_source(source: &str, file_path: &str) -> bool {
    let bytes = source.len();
    if bytes == 0 {
        return false;
    }
    let ext = match file_path.rfind('.') {
        Some(idx) => &file_path[idx..],
        None => "",
    };
    if matches!(ext, ".js" | ".mjs" | ".cjs" | ".jsx") && bytes > 500_000 {
        let mut lines = 0usize;
        for b in source.bytes() {
            if b == b'\n' {
                lines += 1;
            }
        }
        if bytes / (lines + 1) > 200 {
            return true;
        }
    }

    let sample_len = bytes.min(8192);
    // Avoid slicing mid UTF-8 char
    let mut end = sample_len;
    while end > 0 && !source.is_char_boundary(end) {
        end -= 1;
    }
    let sample = &source[..end];

    let mut line_count = 0usize;
    let mut max_line = 0usize;
    let mut cur = 0usize;
    for b in sample.bytes() {
        if b == b'\n' {
            if cur > max_line {
                max_line = cur;
            }
            cur = 0;
            line_count += 1;
        } else {
            cur += 1;
        }
    }
    if cur > max_line {
        max_line = cur;
    }
    line_count += 1;

    if max_line > 2000 && bytes > 20_000 {
        return true;
    }
    if bytes > 50_000 && (sample.len() / line_count) > 500 {
        return true;
    }
    false
}

fn strip_json_comments_and_commas(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;
    let mut in_str = false;
    let mut escape = false;

    while i < len {
        let ch = bytes[i];
        let next = if i + 1 < len { bytes[i + 1] } else { 0 };
        if in_str {
            out.push(ch as char);
            if escape {
                escape = false;
            } else if ch == b'\\' {
                escape = true;
            } else if ch == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if ch == b'"' {
            in_str = true;
            out.push('"');
            i += 1;
            continue;
        }
        if ch == b'/' && next == b'/' {
            i += 2;
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if ch == b'/' && next == b'*' {
            i += 2;
            while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(len);
            continue;
        }
        out.push(ch as char);
        i += 1;
    }

    // Remove trailing commas before } or ]
    let re = regex::Regex::new(r",\s*([}\]])").unwrap();
    re.replace_all(&out, "$1").into_owned()
}

fn resolve_export_value(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    if let Some(obj) = v.as_object() {
        for key in &["import", "default", "types", "require"] {
            if let Some(child) = obj.get(*key) {
                if let Some(res) = resolve_export_value(child) {
                    return Some(res);
                }
            }
        }
    }
    None
}

/// Discover workspace JS/TS packages (`package.json`), `tsconfig.json` `paths`,
/// and Python package roots (`pyproject.toml`, `setup.py`, `setup.cfg`) up to depth 3.
pub fn discover_root_metadata(root_dir: &Path, prefix: &str) -> ResolverCtx {
    let mut ctx = ResolverCtx::default();
    let prefix_norm = normalize_slashes(prefix).trim_end_matches('/').to_string();
    ctx.python_roots.push(prefix_norm.clone());

    let skip: FxHashSet<&'static str> = FAST_PRUNE_DIRS.iter().copied().collect();

    fn scan(
        abs_dir: &Path,
        rel_dir: &str,
        depth: usize,
        prefix_norm: &str,
        skip: &FxHashSet<&'static str>,
        ctx: &mut ResolverCtx,
    ) {
        let entries = match fs::read_dir(abs_dir) {
            Ok(rd) => rd,
            Err(_) => return,
        };
        let mut has_pkg = false;
        let mut has_tsconfig = false;
        let mut has_py = false;
        let mut subdirs: Vec<(PathBuf, String)> = Vec::new();

        for entry in entries.flatten() {
            let ft = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            let name = entry.file_name().to_string_lossy().to_string();
            if ft.is_file() {
                if name == "package.json" {
                    has_pkg = true;
                } else if name == "tsconfig.json" {
                    has_tsconfig = true;
                } else if name == "pyproject.toml" || name == "setup.py" || name == "setup.cfg" {
                    has_py = true;
                }
            } else if ft.is_dir()
                && depth < 3
                && !skip.contains(name.as_str())
                && !name.starts_with('.')
            {
                let next_rel = if rel_dir.is_empty() {
                    name
                } else {
                    format!("{rel_dir}/{name}")
                };
                subdirs.push((entry.path(), next_rel));
            }
        }

        let full_rel_dir = if prefix_norm.is_empty() {
            rel_dir.to_string()
        } else if rel_dir.is_empty() {
            prefix_norm.to_string()
        } else {
            format!("{prefix_norm}/{rel_dir}")
        };

        if has_py && !ctx.python_roots.contains(&full_rel_dir) {
            ctx.python_roots.push(full_rel_dir.clone());
        }

        if has_pkg {
            if let Ok(raw) = fs::read_to_string(abs_dir.join("package.json")) {
                if let Ok(pkg) = serde_json::from_str::<Value>(&raw) {
                    if let Some(name) = pkg.get("name").and_then(|v| v.as_str()) {
                        let main = pkg
                            .get("main")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let mut exports = Vec::new();
                        if let Some(exp_obj) = pkg.get("exports").and_then(|v| v.as_object()) {
                            for (k, v) in exp_obj {
                                if let Some(target) = resolve_export_value(v) {
                                    exports.push((k.clone(), target));
                                }
                            }
                        } else if let Some(exp_str) = pkg.get("exports").and_then(|v| v.as_str()) {
                            exports.push((".".to_string(), exp_str.to_string()));
                        }
                        ctx.workspace_packages.push(WorkspacePackage {
                            name: name.to_string(),
                            dir: full_rel_dir.clone(),
                            main,
                            exports,
                        });
                    }
                }
            }
        }

        if has_tsconfig {
            if let Ok(raw) = fs::read_to_string(abs_dir.join("tsconfig.json")) {
                let cleaned = strip_json_comments_and_commas(&raw);
                if let Ok(ts) = serde_json::from_str::<Value>(&cleaned) {
                    if let Some(co) = ts.get("compilerOptions") {
                        if let Some(paths_obj) = co.get("paths").and_then(|v| v.as_object()) {
                            let base_url = co
                                .get("baseUrl")
                                .and_then(|v| v.as_str())
                                .unwrap_or(".")
                                .to_string();
                            let mut paths = Vec::new();
                            for (pat, arr) in paths_obj {
                                if let Some(list) = arr.as_array() {
                                    let targets: Vec<String> = list
                                        .iter()
                                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                        .collect();
                                    if !targets.is_empty() {
                                        paths.push((pat.clone(), targets));
                                    }
                                }
                            }
                            if !paths.is_empty() {
                                ctx.ts_path_mappings.push(TsPathMapping {
                                    dir: full_rel_dir.clone(),
                                    base_url,
                                    paths,
                                });
                            }
                        }
                    }
                }
            }
        }

        for (sub_abs, sub_rel) in subdirs {
            scan(&sub_abs, &sub_rel, depth + 1, prefix_norm, skip, ctx);
        }
    }

    scan(root_dir, "", 0, &prefix_norm, &skip, &mut ctx);
    ctx
}

/// Exclusive per-cache lockfile so concurrent CLI processes don't duplicate cold builds.
pub struct CacheLock {
    lock_path: PathBuf,
    acquired: bool,
    pub waited: bool,
}

impl CacheLock {
    pub fn acquire(cache_path: &Path, timeout_ms: u64) -> Self {
        let mut lock_os = cache_path.as_os_str().to_os_string();
        lock_os.push(".lock");
        let lock_path = PathBuf::from(lock_os);
        if let Some(parent) = lock_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let mut waited = false;

        loop {
            match OpenOptions::new().write(true).create_new(true).open(&lock_path) {
                Ok(mut file) => {
                    let ts = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_millis())
                        .unwrap_or(0);
                    let _ = write!(file, "{{\"pid\":{},\"ts\":{}}}", std::process::id(), ts);
                    return Self {
                        lock_path,
                        acquired: true,
                        waited,
                    };
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Check if stale (> 120s old)
                    let stale = fs::metadata(&lock_path)
                        .and_then(|m| m.modified())
                        .and_then(|t| {
                            t.elapsed()
                                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
                        })
                        .map(|d| d > Duration::from_secs(120))
                        .unwrap_or(false);
                    if stale {
                        let _ = fs::remove_file(&lock_path);
                        continue;
                    }
                    if Instant::now() >= deadline {
                        return Self {
                            lock_path,
                            acquired: false,
                            waited,
                        };
                    }
                    waited = true;
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(_) => {
                    return Self {
                        lock_path,
                        acquired: false,
                        waited,
                    };
                }
            }
        }
    }
}

impl Drop for CacheLock {
    fn drop(&mut self) {
        if self.acquired {
            let _ = fs::remove_file(&self.lock_path);
        }
    }
}
