use crate::graph::{
    build_graph_cache, load_parse_cache, save_graph_cache, save_parse_cache, MmapGraphCache,
};
use crate::model::{ParseCacheData, ParsedFileRecord, ResolverCtx, RootEntry, CACHE_FORMAT_VERSION};
use crate::parser::{parse_file, ParseOptions};
use crate::walk::{
    discover_root_metadata, is_minified_source, load_root_extra_ignores, normalize_roots,
    walk_source_files, CacheLock,
};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub fn platform_cache_dir() -> PathBuf {
    if let Ok(custom) = std::env::var("CODE_INDEX_CACHE_DIR") {
        if !custom.is_empty() {
            return PathBuf::from(custom);
        }
    }
    if let Some(base) = dirs::cache_dir() {
        return base.join("code-index");
    }
    PathBuf::from("/tmp/code-index")
}

pub fn graph_cache_path_for_roots(roots: &[RootEntry]) -> PathBuf {
    let mut sorted: Vec<String> = roots
        .iter()
        .map(|r| r.path.to_string_lossy().to_string())
        .collect();
    sorted.sort();
    let joined = sorted.join("\0");
    let hash = blake3::hash(joined.as_bytes()).to_hex();
    platform_cache_dir().join(format!("{}.brbin", &hash[..12]))
}

pub fn parse_cache_path_for_root(root_path: &Path) -> PathBuf {
    let s = root_path.to_string_lossy();
    let hash = blake3::hash(s.as_bytes()).to_hex();
    platform_cache_dir().join(format!("{}.parse.brbin", &hash[..12]))
}

pub struct CodeIndex {
    pub roots: Vec<RootEntry>,
    pub cache_path: PathBuf,
}

impl CodeIndex {
    pub fn new(raw_roots: &[PathBuf]) -> Self {
        let roots = normalize_roots(raw_roots);
        let cache_path = graph_cache_path_for_roots(&roots);
        Self { roots, cache_path }
    }

    pub fn is_multi_root(&self) -> bool {
        self.roots.len() > 1
    }

    pub fn resolve_abs_path(&self, graph_rel_path: &str) -> PathBuf {
        if !self.is_multi_root() {
            return self.roots[0].path.join(graph_rel_path);
        }
        for r in &self.roots {
            let pfx = format!("{}/", r.prefix);
            if let Some(rest) = graph_rel_path.strip_prefix(&pfx) {
                return r.path.join(rest);
            }
        }
        self.roots[0].path.join(graph_rel_path)
    }

    pub fn reindex(&self) -> Result<MmapGraphCache, String> {
        let _ = fs::remove_file(&self.cache_path);
        for r in &self.roots {
            let _ = fs::remove_file(parse_cache_path_for_root(&r.path));
        }
        self.ensure()
    }

    /// Ensure the zero-copy `mmap` graph cache is up-to-date and return it.
    ///
    /// Fast path (warm & clean): walks/stats in parallel, compares mtimes directly
    /// against the `mmap`'d archive, and returns in ~5–25 ms with zero deserialization!
    pub fn ensure(&self) -> Result<MmapGraphCache, String> {
        // Walk all roots in parallel
        let walked_per_root: Vec<_> = self
            .roots
            .par_iter()
            .map(|r| {
                let extra = load_root_extra_ignores(&r.path);
                let files = walk_source_files(&r.path, &extra);
                (r, files)
            })
            .collect();

        // Fast-path check against existing mmap cache (zero lock, zero deserialization!)
        if let Some(existing) = MmapGraphCache::open(&self.cache_path) {
            if self.is_cache_fresh(&existing, &walked_per_root) {
                return Ok(existing);
            }
        }

        // Acquire exclusive cache lock so concurrent agents don't run duplicate cold builds
        let lock = CacheLock::acquire(&self.cache_path, 120_000);
        if lock.waited {
            if let Some(fresh) = MmapGraphCache::open(&self.cache_path) {
                if self.is_cache_fresh(&fresh, &walked_per_root) {
                    return Ok(fresh);
                }
            }
        }

        let is_cold = !self.cache_path.exists();
        let total_files: usize = walked_per_root.iter().map(|(_, f)| f.len()).sum();
        if is_cold {
            let label = if self.is_multi_root() {
                self.roots
                    .iter()
                    .map(|r| r.prefix.as_str())
                    .collect::<Vec<_>>()
                    .join(" + ")
            } else {
                self.roots[0].path.to_string_lossy().to_string()
            };
            eprintln!("Indexing {label}... {total_files} files found, parsing...");
        }

        let t0 = Instant::now();

        // Discover per-root metadata (package.json, tsconfig.json, pyproject.toml)
        let mut resolver_ctx = ResolverCtx::default();
        for r in &self.roots {
            let meta = discover_root_metadata(&r.path, &r.prefix);
            resolver_ctx.workspace_packages.extend(meta.workspace_packages);
            resolver_ctx.ts_path_mappings.extend(meta.ts_path_mappings);
            for pr in meta.python_roots {
                if !resolver_ctx.python_roots.contains(&pr) {
                    resolver_ctx.python_roots.push(pr);
                }
            }
        }

        // Parse changed/uncached files per root in parallel via rayon, reusing per-root .parse.brbin caches!
        let mut all_records: Vec<ParsedFileRecord> = Vec::with_capacity(total_files);
        let mut pending_parse_caches: Vec<(PathBuf, ParseCacheData)> = Vec::new();
        let mut total_reused = 0usize;
        let mut total_skipped = 0usize;

        for (r, walked) in &walked_per_root {
            let pcache_path = parse_cache_path_for_root(&r.path);
            let root_cache = load_parse_cache(&pcache_path);

            let mut hits: Vec<ParsedFileRecord> = Vec::new();
            let mut misses = Vec::new();

            for wf in walked {
                if let Some(cached) = root_cache.get(&wf.rel_path) {
                    if cached.mtime_ms == wf.mtime_ms {
                        total_reused += 1;
                        if cached.skipped {
                            total_skipped += 1;
                        }
                        hits.push(cached.clone());
                        continue;
                    }
                }
                misses.push(wf);
            }

            // Schedule largest files first (LPT) so huge files never become rayon tail stragglers
            misses.sort_unstable_by(|a, b| b.size.cmp(&a.size));

            let newly_parsed: Vec<ParsedFileRecord> = misses
                .par_iter()
                .filter_map(|wf| {
                    let abs = r.path.join(&wf.rel_path);
                    let source = fs::read_to_string(&abs).ok()?;
                    if is_minified_source(&source, &wf.rel_path) {
                        return Some(ParsedFileRecord {
                            rel_path: wf.rel_path.clone(),
                            mtime_ms: wf.mtime_ms,
                            skipped: true,
                            definitions: Vec::new(),
                            references: Vec::new(),
                            imports: Vec::new(),
                        });
                    }
                    let parsed = parse_file(
                        &wf.rel_path,
                        &source,
                        ParseOptions {
                            include_outline_defs: false,
                            include_local_refs: false,
                            dedupe: true,
                        },
                    )?;
                    Some(ParsedFileRecord {
                        rel_path: wf.rel_path.clone(),
                        mtime_ms: wf.mtime_ms,
                        skipped: false,
                        definitions: parsed.definitions,
                        references: parsed.references,
                        imports: parsed.imports,
                    })
                })
                .collect();

            for rec in &newly_parsed {
                if rec.skipped {
                    total_skipped += 1;
                }
            }

            let cache_dirty = !newly_parsed.is_empty() || root_cache.len() != walked.len();
            if cache_dirty {
                let mut next_cache_files: Vec<ParsedFileRecord> =
                    Vec::with_capacity(hits.len() + newly_parsed.len());
                next_cache_files.extend(hits.iter().cloned());
                next_cache_files.extend(newly_parsed.iter().cloned());
                pending_parse_caches.push((
                    pcache_path,
                    ParseCacheData {
                        version: CACHE_FORMAT_VERSION,
                        files: next_cache_files,
                    },
                ));
            }
            drop(root_cache);

            // Prefix rel_path with root.prefix for multi-root graph linking
            for mut rec in hits.into_iter().chain(newly_parsed.into_iter()) {
                if !r.prefix.is_empty() {
                    rec.rel_path = format!("{}/{}", r.prefix, rec.rel_path);
                }
                all_records.push(rec);
            }
        }

        // Overlap `.parse.brbin` disk serialization concurrently with `build_graph_cache`!
        let (graph_data, ()) = rayon::join(
            || build_graph_cache(&all_records, &resolver_ctx),
            || {
                pending_parse_caches.par_iter().for_each(|(path, pdata)| {
                    let _ = save_parse_cache(pdata, path);
                });
            },
        );
        let sym_count = graph_data.symbols.len();
        let edge_count = graph_data.total_edges;
        let indexed_files = all_records.iter().filter(|r| !r.skipped).count();

        save_graph_cache(&graph_data, &self.cache_path)
            .map_err(|e| format!("Failed to write graph cache: {e}"))?;

        if is_cold {
            let elapsed = t0.elapsed().as_secs_f64();
            let cache_note = if total_reused > 0 {
                format!(", {total_reused} cached")
            } else {
                String::new()
            };
            let skip_note = if total_skipped > 0 {
                format!(", skipped {total_skipped} minified/generated")
            } else {
                String::new()
            };
            eprintln!(
                "Indexed {indexed_files} files ({sym_count} symbols, {edge_count} edges{cache_note}{skip_note}) in {elapsed:.2}s"
            );
        }

        MmapGraphCache::open(&self.cache_path)
            .ok_or_else(|| "Failed to mmap newly written graph cache".to_string())
    }

    fn is_cache_fresh(
        &self,
        cache: &MmapGraphCache,
        walked_per_root: &[(&RootEntry, Vec<crate::walk::WalkedFile>)],
    ) -> bool {
        let g = cache.archived();
        let total_walked: usize = walked_per_root.iter().map(|(_, f)| f.len()).sum();
        if g.files.len() != total_walked {
            return false;
        }
        // Build O(1) lookup of current walked mtimes or binary-search archived file_order
        let mut stored: FxHashMap<&str, u64> = FxHashMap::default();
        stored.reserve(g.files.len());
        for f in g.files.iter() {
            stored.insert(f.path.as_str(), f.mtime_ms.to_native());
        }
        for (r, files) in walked_per_root {
            for wf in files {
                let matched = if r.prefix.is_empty() {
                    stored.get(wf.rel_path.as_str()).copied()
                } else {
                    let gp = format!("{}/{}", r.prefix, wf.rel_path);
                    stored.get(gp.as_str()).copied()
                };
                if matched != Some(wf.mtime_ms) {
                    return false;
                }
            }
        }
        true
    }
}
