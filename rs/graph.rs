use crate::model::{
    language_family, ArchivedGraphCacheData, FileEntry, GraphCacheData, LangFamily, ParseCacheData,
    ParsedFileRecord, RefKind, ResolverCtx, SymbolEntry, SymbolKind, TsPathMapping,
    WorkspacePackage, CACHE_FORMAT_VERSION, GRAPH_CACHE_MAGIC, PARSE_CACHE_MAGIC,
};
use crate::parser::is_method_noise;
use memmap2::Mmap;
use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

const AMBIGUITY_CAP: usize = 5;
const HEADER_LEN: usize = 16;

const JS_EXTS: &[&str] = &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"];
const PY_EXTS: &[&str] = &[".py"];
const ALL_EXTS: &[&str] = &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".py", ".rs", ".go"];

pub fn is_barrel_file(path: &str) -> bool {
    let base = path.rsplit('/').next().unwrap_or(path);
    let stem = match base.rfind('.') {
        Some(dot) => &base[..dot],
        None => base,
    };
    stem.eq_ignore_ascii_case("index") || stem == "__init__"
}

fn posix_normalize(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.last().map_or(true, |&last| last == "..") {
                    parts.push("..");
                } else {
                    parts.pop();
                }
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn posix_join(a: &str, b: &str) -> String {
    if a.is_empty() || a == "." {
        posix_normalize(b)
    } else if b.is_empty() || b == "." {
        posix_normalize(a)
    } else {
        posix_normalize(&format!("{a}/{b}"))
    }
}

fn posix_dirname(p: &str) -> &str {
    match p.rfind('/') {
        Some(idx) => &p[..idx],
        None => "",
    }
}

fn resolve_workspace_import_bases(spec: &str, packages: &[WorkspacePackage]) -> Vec<String> {
    let mut bases = Vec::new();
    for pkg in packages {
        if spec == pkg.name {
            for (k, v) in &pkg.exports {
                if k == "." {
                    bases.push(posix_join(&pkg.dir, v));
                }
            }
            if let Some(ref main) = pkg.main {
                bases.push(posix_join(&pkg.dir, main));
            }
            bases.push(posix_join(&pkg.dir, "src/index"));
            bases.push(posix_join(&pkg.dir, "index"));
            return bases;
        }
        if let Some(subpath) = spec.strip_prefix(&pkg.name).and_then(|s| s.strip_prefix('/')) {
            let dot_sub = format!("./{subpath}");
            for (pat, target) in &pkg.exports {
                if pat == &dot_sub {
                    bases.push(posix_join(&pkg.dir, target));
                } else if let Some(star) = pat.find('*') {
                    let pre = &pat[..star];
                    let post = &pat[star + 1..];
                    if dot_sub.starts_with(pre)
                        && dot_sub.ends_with(post)
                        && dot_sub.len() >= pre.len() + post.len()
                    {
                        let matched = &dot_sub[pre.len()..dot_sub.len() - post.len()];
                        if let Some(tstar) = target.find('*') {
                            let expanded =
                                format!("{}{}{}", &target[..tstar], matched, &target[tstar + 1..]);
                            bases.push(posix_join(&pkg.dir, &expanded));
                        }
                    }
                }
            }
            bases.push(posix_join(&pkg.dir, &format!("src/{subpath}")));
            bases.push(posix_join(&pkg.dir, subpath));
            return bases;
        }
    }
    bases
}

fn resolve_ts_config_path_bases(
    source_file: &str,
    spec: &str,
    mappings: &[TsPathMapping],
) -> Vec<String> {
    if mappings.is_empty() {
        return Vec::new();
    }
    let mut idxs: Vec<usize> = (0..mappings.len()).collect();
    idxs.sort_by(|&a, &b| mappings[b].dir.len().cmp(&mappings[a].dir.len()));

    let mut bases = Vec::new();
    for idx in idxs {
        let cfg = &mappings[idx];
        if !cfg.dir.is_empty() && !source_file.starts_with(&format!("{}/", cfg.dir)) {
            continue;
        }
        let base_dir = if cfg.base_url.is_empty() || cfg.base_url == "." {
            cfg.dir.clone()
        } else {
            posix_join(&cfg.dir, &cfg.base_url)
        };

        for (pat, targets) in &cfg.paths {
            if let Some(star) = pat.find('*') {
                let pre = &pat[..star];
                let post = &pat[star + 1..];
                if spec.starts_with(pre)
                    && spec.ends_with(post)
                    && spec.len() >= pre.len() + post.len()
                {
                    let matched = &spec[pre.len()..spec.len() - post.len()];
                    for raw in targets {
                        let expanded = if let Some(ts) = raw.find('*') {
                            format!("{}{}{}", &raw[..ts], matched, &raw[ts + 1..])
                        } else {
                            raw.clone()
                        };
                        let joined = posix_join(&base_dir, &expanded);
                        if !joined.starts_with("..") {
                            bases.push(joined);
                        }
                    }
                }
            } else if spec == pat {
                for raw in targets {
                    let joined = posix_join(&base_dir, raw);
                    if !joined.starts_with("..") {
                        bases.push(joined);
                    }
                }
            }
        }
        if !bases.is_empty() {
            return bases;
        }
    }
    bases
}

fn is_valid_py_dotted(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    for seg in s.split('.') {
        let bytes = seg.as_bytes();
        if bytes.is_empty() || !(bytes[0].is_ascii_alphabetic() || bytes[0] == b'_') {
            return false;
        }
        if !bytes
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return false;
        }
    }
    true
}

fn resolve_python_import_bases(source_file: &str, spec: &str, python_roots: &[String]) -> Vec<String> {
    if !source_file.ends_with(".py") || spec.is_empty() {
        return Vec::new();
    }
    if spec.starts_with('.') {
        let dots = spec.bytes().take_while(|&b| b == b'.').count();
        let mut base_dir = posix_dirname(source_file).to_string();
        for _ in 1..dots {
            base_dir = posix_dirname(&base_dir).to_string();
        }
        let rem = spec[dots..].trim_start_matches('.');
        if rem.is_empty() {
            return vec![base_dir];
        }
        return vec![posix_join(&base_dir, &rem.replace('.', "/"))];
    }

    if is_valid_py_dotted(spec) {
        let mod_path = spec.replace('.', "/");
        let mut bases = Vec::with_capacity(1 + python_roots.len() * 2);
        bases.push(mod_path.clone());
        for root in python_roots {
            if !root.is_empty() {
                bases.push(posix_join(root, &mod_path));
                bases.push(posix_join(root, &format!("src/{mod_path}")));
            } else {
                bases.push(posix_join("src", &mod_path));
            }
        }
        return bases;
    }
    Vec::new()
}

fn resolve_import_bases(source_file: &str, spec: &str, ctx: &ResolverCtx) -> Vec<String> {
    if spec.is_empty() || spec.starts_with("node:") {
        return Vec::new();
    }
    if source_file.ends_with(".py") {
        return resolve_python_import_bases(source_file, spec, &ctx.python_roots);
    }
    if spec.starts_with('.') {
        return vec![posix_join(posix_dirname(source_file), spec)];
    }

    let ws = resolve_workspace_import_bases(spec, &ctx.workspace_packages);
    if !ws.is_empty() {
        return ws;
    }

    let ts = resolve_ts_config_path_bases(source_file, spec, &ctx.ts_path_mappings);
    if !ts.is_empty() {
        return ts;
    }

    if let Some(tail) = spec.strip_prefix("~/").or_else(|| spec.strip_prefix("@/")) {
        let mut bases = Vec::with_capacity(2);
        if let Some(src_idx) = source_file.find("/src/") {
            let app_src = &source_file[..src_idx + 4];
            bases.push(posix_join(app_src, tail));
        }
        bases.push(posix_join("src", tail));
        return bases;
    }

    if let Some(stripped) = spec.strip_prefix('/') {
        return vec![posix_normalize(stripped)];
    }

    if spec.starts_with("src/") {
        let mut bases = Vec::with_capacity(2);
        if let Some(src_idx) = source_file.find("/src/") {
            let app_root = &source_file[..src_idx];
            bases.push(posix_join(app_root, spec));
        }
        bases.push(posix_normalize(spec));
        return bases;
    }

    Vec::new()
}

fn has_explicit_ext(p: &str) -> bool {
    ALL_EXTS.iter().any(|ext| p.ends_with(ext))
}

fn resolve_import_target_idx(
    source_file: &str,
    spec: &str,
    file_lookup: &FxHashMap<String, u32>,
    ctx: &ResolverCtx,
) -> Option<u32> {
    let bases = resolve_import_bases(source_file, spec, ctx);
    if bases.is_empty() {
        return None;
    }
    let is_py = source_file.ends_with(".py");
    let is_js = !is_py && language_family(source_file) == LangFamily::Js;
    let exts = if is_py {
        PY_EXTS
    } else if is_js {
        JS_EXTS
    } else {
        ALL_EXTS
    };
    let entry_name = if is_py { "__init__" } else { "index" };

    for base in &bases {
        if let Some(&idx) = file_lookup.get(base.as_str()) {
            return Some(idx);
        }
        if !has_explicit_ext(base) {
            for ext in exts {
                let cand = format!("{base}{ext}");
                if let Some(&idx) = file_lookup.get(&cand) {
                    return Some(idx);
                }
            }
            for ext in exts {
                let cand = format!("{base}/{entry_name}{ext}");
                if let Some(&idx) = file_lookup.get(&cand) {
                    return Some(idx);
                }
            }
        } else if is_js && (base.ends_with(".js") || base.ends_with(".mjs")) {
            let stem = base
                .strip_suffix(".mjs")
                .or_else(|| base.strip_suffix(".js"))
                .unwrap_or(base);
            if let Some(&idx) = file_lookup.get(&format!("{stem}.ts")) {
                return Some(idx);
            }
            if let Some(&idx) = file_lookup.get(&format!("{stem}.tsx")) {
                return Some(idx);
            }
        }
    }
    None
}

struct SymDesc {
    sym_idx: u32,
    file_idx: u32,
    kind: u8,
    exported: bool,
    family: LangFamily,
    stem_norm: String,
}

struct DefBucket {
    items: Vec<SymDesc>,
    has_property: bool,
    has_method_or_py_callable: bool,
}

fn norm_alnum_lower(s: &str) -> String {
    s.bytes()
        .filter(|b| b.is_ascii_alphanumeric())
        .map(|b| b.to_ascii_lowercase() as char)
        .collect()
}

fn file_stem_norm(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    let stem = match base.rfind('.') {
        Some(dot) => &base[..dot],
        None => base,
    };
    norm_alnum_lower(stem)
}

fn match_property_targets_by_receiver<'a>(
    prop_targets: &[&'a SymDesc],
    receiver: Option<&str>,
) -> Vec<&'a SymDesc> {
    let Some(recv) = receiver else {
        return Vec::new();
    };
    let recv_norm = norm_alnum_lower(recv);
    if recv_norm.len() < 2 {
        return Vec::new();
    }
    let recv_router = format!("{recv_norm}router");
    let recv_procs = format!("{recv_norm}procedures");
    let recv_s = format!("{recv_norm}s");

    let mut matches = Vec::new();
    for &t in prop_targets {
        let stem = &t.stem_norm;
        if stem == &recv_norm
            || stem == &recv_router
            || stem == &recv_procs
            || stem == &recv_s
            || format!("{stem}s") == recv_norm
        {
            matches.push(t);
        }
    }
    matches
}

pub fn path_weight(path: &str) -> f32 {
    let base = path.rsplit('/').next().unwrap_or(path);
    let lower = base.to_ascii_lowercase();
    let stem = match lower.rfind('.') {
        Some(dot) => &lower[..dot],
        None => &lower,
    };
    if stem.starts_with("test_")
        || stem.ends_with(".test")
        || stem.ends_with(".spec")
        || stem.ends_with("_test")
        || stem.ends_with("_spec")
    {
        return 0.15;
    }
    for (pat, w) in &[
        ("temp_qa/", 0.10f32),
        ("qa/temp_", 0.10),
        ("qa/", 0.20),
        ("tests/", 0.20),
        ("test/", 0.20),
        ("__tests__/", 0.20),
        ("scripts/", 0.30),
        ("deploy/", 0.30),
    ] {
        if path.starts_with(pat) || path.contains(&format!("/{pat}")) {
            return *w;
        }
    }
    1.0
}

/// Build the complete CSR graph + sparse PageRank from parsed file records.
pub fn build_graph_cache(
    records: &[ParsedFileRecord],
    ctx: &ResolverCtx,
) -> GraphCacheData {
    let num_files = records.len();
    let mut files: Vec<FileEntry> = Vec::with_capacity(num_files);
    let mut file_lookup: FxHashMap<String, u32> = FxHashMap::default();
    file_lookup.reserve(num_files);

    let mut symbols: Vec<SymbolEntry> = Vec::with_capacity(num_files * 5);
    let mut def_offsets: Vec<u32> = Vec::with_capacity(num_files + 1);
    let mut def_syms: Vec<u32> = Vec::with_capacity(num_files * 5);

    let mut file_families: Vec<LangFamily> = Vec::with_capacity(num_files);
    let mut file_barrels: Vec<bool> = Vec::with_capacity(num_files);
    let mut file_stems: Vec<String> = Vec::with_capacity(num_files);

    for (f_idx_usize, rec) in records.iter().enumerate() {
        let f_idx = f_idx_usize as u32;
        def_offsets.push(def_syms.len() as u32);
        let sym_count = if rec.skipped {
            -1
        } else {
            rec.definitions.len() as i32
        };
        files.push(FileEntry {
            path: rec.rel_path.clone(),
            mtime_ms: rec.mtime_ms,
            symbol_count: sym_count,
            file_score: 0.0,
        });
        file_families.push(language_family(&rec.rel_path));
        file_barrels.push(is_barrel_file(&rec.rel_path));
        file_stems.push(file_stem_norm(&rec.rel_path));

        if !rec.skipped {
            file_lookup.insert(rec.rel_path.clone(), f_idx);
            for d in &rec.definitions {
                let s_idx = symbols.len() as u32;
                symbols.push(SymbolEntry {
                    file_idx: f_idx,
                    name: d.name.clone(),
                    kind: d.kind,
                    exported: d.exported,
                    line_start: d.line_start,
                    line_end: d.line_end,
                    signature: d.signature.clone(),
                    score: 0.0,
                });
                def_syms.push(s_idx);
            }
        }
    }
    def_offsets.push(def_syms.len() as u32);

    // Build pre-enriched definition index
    let mut def_index: FxHashMap<&str, DefBucket> = FxHashMap::default();
    def_index.reserve(symbols.len() / 2 + 16);
    for (s_idx_u, sym) in symbols.iter().enumerate() {
        let s_idx = s_idx_u as u32;
        let f_idx = sym.file_idx;
        let fam = file_families[f_idx as usize];
        let kind = sym.kind;
        let bucket = def_index.entry(sym.name.as_str()).or_insert_with(|| DefBucket {
            items: Vec::new(),
            has_property: false,
            has_method_or_py_callable: false,
        });
        if kind == SymbolKind::Property as u8 {
            bucket.has_property = true;
        }
        if kind == SymbolKind::Method as u8
            || (fam == LangFamily::Python
                && (kind == SymbolKind::Function as u8 || kind == SymbolKind::Class as u8))
        {
            bucket.has_method_or_py_callable = true;
        }
        bucket.items.push(SymDesc {
            sym_idx: s_idx,
            file_idx: f_idx,
            kind,
            exported: sym.exported,
            family: fam,
            stem_norm: file_stems[f_idx as usize].clone(),
        });
    }

    // Pass 1: Explicit AST IMPORTS edges only (parallelized per file)
    let imp_out: Vec<Vec<(u32, bool)>> = records
        .par_iter()
        .enumerate()
        .map(|(f_idx_u, rec)| {
            if rec.skipped || rec.imports.is_empty() {
                return Vec::new();
            }
            let f_idx = f_idx_u as u32;
            let mut out: Vec<(u32, bool)> = Vec::with_capacity(rec.imports.len());
            let mut seen_targets: FxHashMap<u32, usize> = FxHashMap::default();
            for imp in &rec.imports {
                let Some(dst) =
                    resolve_import_target_idx(&rec.rel_path, &imp.path, &file_lookup, ctx)
                else {
                    continue;
                };
                if dst == f_idx {
                    continue;
                }
                if let Some(&pos) = seen_targets.get(&dst) {
                    if imp.is_reexport {
                        out[pos].1 = true;
                    }
                } else {
                    let pos = out.len();
                    seen_targets.insert(dst, pos);
                    out.push((dst, imp.is_reexport));
                }
            }
            out
        })
        .collect();

    let mut imp_in: Vec<Vec<u32>> = vec![Vec::new(); num_files];
    for (f_idx_u, adj) in imp_out.iter().enumerate() {
        let f_idx = f_idx_u as u32;
        for &(dst, _) in adj {
            imp_in[dst as usize].push(f_idx);
        }
    }

    // Build barrel-only re-export reachability helper
    let mut reexport_adj: Vec<Vec<u32>> = vec![Vec::new(); num_files];
    for f_idx_u in 0..num_files {
        let barrel = file_barrels[f_idx_u];
        for &(dst, is_reexp) in &imp_out[f_idx_u] {
            if barrel || is_reexp {
                reexport_adj[f_idx_u].push(dst);
            }
        }
    }

    fn compute_reachable(
        file_idx: u32,
        max_hops: u8,
        imp_out: &[Vec<(u32, bool)>],
        reexport_adj: &[Vec<u32>],
    ) -> Option<FxHashMap<u32, u8>> {
        if imp_out[file_idx as usize].is_empty() {
            return None;
        }
        let mut dist = FxHashMap::default();
        let mut queue = VecDeque::new();
        for &(dst, _) in &imp_out[file_idx as usize] {
            dist.insert(dst, 1u8);
            queue.push_back(dst);
        }
        while let Some(cur) = queue.pop_front() {
            let d = dist[&cur];
            if d >= max_hops {
                continue;
            }
            for &nxt in &reexport_adj[cur as usize] {
                if nxt == file_idx || dist.contains_key(&nxt) {
                    continue;
                }
                dist.insert(nxt, d + 1);
                queue.push_back(nxt);
            }
        }
        Some(dist)
    }

    // Pass 2: Import-scoped symbol REFERENCES edges (parallelized per file)
    let ref_out: Vec<Vec<u32>> = records
        .par_iter()
        .enumerate()
        .map(|(f_idx_u, rec)| {
            if rec.skipped || rec.references.is_empty() {
                return Vec::new();
            }
            let f_idx = f_idx_u as u32;
            let src_family = file_families[f_idx_u];
            let is_py = src_family == LangFamily::Python;
            let is_js = src_family == LangFamily::Js;

            let mut out: Vec<u32> = Vec::new();
            let mut added_for_file: FxHashSet<u32> = FxHashSet::default();
            let mut self_reach_computed = false;
            let mut self_reach: Option<FxHashMap<u32, u8>> = None;
            let mut mod_resolve_cache: FxHashMap<&str, Option<u32>> = FxHashMap::default();
            let mut mod_reach_cache: FxHashMap<u32, Option<FxHashMap<u32, u8>>> =
                FxHashMap::default();

            for r in &rec.references {
                if r.kind == RefKind::Method as u8 && (is_method_noise(&r.name) || r.name.len() < 3)
                {
                    continue;
                }
                let Some(bucket) = def_index.get(r.name.as_str()) else {
                    continue;
                };
                if r.kind == RefKind::Property as u8
                    && r.receiver_module.is_none()
                    && !bucket.has_property
                {
                    continue;
                }
                if r.kind == RefKind::Method as u8 && !bucket.has_method_or_py_callable {
                    continue;
                }

                let mut targets: Vec<&SymDesc> = Vec::new();
                for desc in &bucket.items {
                    if desc.file_idx == f_idx {
                        targets.push(desc);
                    } else if desc.family == src_family && desc.exported {
                        targets.push(desc);
                    }
                }
                if targets.is_empty() {
                    continue;
                }

                let mut resolved: Vec<&SymDesc> = Vec::new();

                if r.kind == RefKind::Method as u8 {
                    let eligible: Vec<&SymDesc> = targets
                        .into_iter()
                        .filter(|t| {
                            t.kind == SymbolKind::Method as u8
                                || (is_py
                                    && (t.kind == SymbolKind::Function as u8
                                        || t.kind == SymbolKind::Class as u8))
                        })
                        .collect();
                    if eligible.is_empty() {
                        continue;
                    }

                    let mut recv_mod_matched_file = false;
                    if let Some(ref mod_spec) = r.receiver_module {
                        let mod_file = *mod_resolve_cache
                            .entry(mod_spec.as_str())
                            .or_insert_with(|| {
                                resolve_import_target_idx(
                                    &rec.rel_path,
                                    mod_spec,
                                    &file_lookup,
                                    ctx,
                                )
                            });
                        if let Some(mf) = mod_file {
                            recv_mod_matched_file = true;
                            let direct: Vec<&SymDesc> = eligible
                                .iter()
                                .copied()
                                .filter(|t| t.file_idx == mf)
                                .collect();
                            if !direct.is_empty() {
                                resolved = direct;
                            } else {
                                let mr = mod_reach_cache.entry(mf).or_insert_with(|| {
                                    compute_reachable(mf, 2, &imp_out, &reexport_adj)
                                });
                                if let Some(mod_reach) = mr.as_ref() {
                                    resolved = eligible
                                        .iter()
                                        .copied()
                                        .filter(|t| mod_reach.contains_key(&t.file_idx))
                                        .collect();
                                }
                            }
                        }
                    }

                    if resolved.is_empty() && !recv_mod_matched_file {
                        let same: Vec<&SymDesc> = eligible
                            .iter()
                            .copied()
                            .filter(|t| t.file_idx == f_idx)
                            .collect();
                        if !same.is_empty() {
                            resolved = same;
                        } else {
                            if !self_reach_computed {
                                self_reach =
                                    compute_reachable(f_idx, 4, &imp_out, &reexport_adj);
                                self_reach_computed = true;
                            }
                            if let Some(dist_map) = self_reach.as_ref() {
                                resolved = filter_closest_reachable(&eligible, dist_map);
                            }
                        }
                    }
                } else if r.kind == RefKind::Property as u8 {
                    let mut recv_mod_matched_file = false;
                    if let Some(ref mod_spec) = r.receiver_module {
                        let mod_file = *mod_resolve_cache
                            .entry(mod_spec.as_str())
                            .or_insert_with(|| {
                                resolve_import_target_idx(
                                    &rec.rel_path,
                                    mod_spec,
                                    &file_lookup,
                                    ctx,
                                )
                            });
                        if let Some(mf) = mod_file {
                            recv_mod_matched_file = true;
                            let pool: Vec<&SymDesc> = if is_py {
                                targets.clone()
                            } else {
                                targets
                                    .iter()
                                    .copied()
                                    .filter(|t| {
                                        t.kind == SymbolKind::Property as u8
                                            || t.kind == SymbolKind::Function as u8
                                            || t.kind == SymbolKind::Variable as u8
                                            || t.kind == SymbolKind::Class as u8
                                    })
                                    .collect()
                            };
                            let direct: Vec<&SymDesc> = pool
                                .iter()
                                .copied()
                                .filter(|t| t.file_idx == mf)
                                .collect();
                            if !direct.is_empty() {
                                resolved = direct;
                            } else {
                                let mr = mod_reach_cache.entry(mf).or_insert_with(|| {
                                    compute_reachable(mf, 2, &imp_out, &reexport_adj)
                                });
                                if let Some(mod_reach) = mr.as_ref() {
                                    resolved = pool
                                        .iter()
                                        .copied()
                                        .filter(|t| mod_reach.contains_key(&t.file_idx))
                                        .collect();
                                }
                            }
                        }
                    }

                    if resolved.is_empty() && !recv_mod_matched_file {
                        let prop_targets: Vec<&SymDesc> = targets
                            .into_iter()
                            .filter(|t| t.kind == SymbolKind::Property as u8)
                            .collect();
                        if prop_targets.is_empty() {
                            continue;
                        }
                        let same: Vec<&SymDesc> = prop_targets
                            .iter()
                            .copied()
                            .filter(|t| t.file_idx == f_idx)
                            .collect();
                        if !same.is_empty() {
                            resolved = same;
                        } else {
                            let recv_matches = match_property_targets_by_receiver(
                                &prop_targets,
                                r.receiver.as_deref(),
                            );
                            if !recv_matches.is_empty() {
                                resolved = recv_matches;
                            } else if r.is_rpc_chain || !is_method_noise(&r.name) {
                                if !self_reach_computed {
                                    self_reach =
                                        compute_reachable(f_idx, 4, &imp_out, &reexport_adj);
                                    self_reach_computed = true;
                                }
                                if let Some(dist_map) = self_reach.as_ref() {
                                    resolved = filter_closest_reachable(&prop_targets, dist_map);
                                }
                                if resolved.is_empty() && r.is_rpc_chain && prop_targets.len() <= 2
                                {
                                    resolved = prop_targets;
                                }
                            }
                        }
                    }
                } else {
                    // Lane C: bare symbol references
                    let non_method: Vec<&SymDesc> = targets
                        .into_iter()
                        .filter(|t| t.kind != SymbolKind::Method as u8)
                        .collect();
                    if non_method.is_empty() {
                        continue;
                    }
                    let same: Vec<&SymDesc> = non_method
                        .iter()
                        .copied()
                        .filter(|t| t.file_idx == f_idx)
                        .collect();
                    if !same.is_empty() {
                        resolved = same;
                    } else {
                        if !self_reach_computed {
                            self_reach = compute_reachable(f_idx, 4, &imp_out, &reexport_adj);
                            self_reach_computed = true;
                        }
                        if let Some(dist_map) = self_reach.as_ref() {
                            resolved = filter_closest_reachable(&non_method, dist_map);
                        }
                        if resolved.is_empty() && !is_js && !is_py {
                            let src_dir = posix_dirname(&rec.rel_path);
                            let same_dir: Vec<&SymDesc> = non_method
                                .iter()
                                .copied()
                                .filter(|t| {
                                    posix_dirname(&files[t.file_idx as usize].path) == src_dir
                                })
                                .collect();
                            if !same_dir.is_empty() {
                                resolved = same_dir;
                            } else if non_method.len() <= AMBIGUITY_CAP {
                                resolved = non_method;
                            }
                        }
                    }
                }

                for t in resolved {
                    if added_for_file.insert(t.sym_idx) {
                        out.push(t.sym_idx);
                    }
                }
            }
            out
        })
        .collect();

    let mut ref_in: Vec<Vec<u32>> = vec![Vec::new(); symbols.len()];
    for (f_idx_u, syms) in ref_out.iter().enumerate() {
        let f_idx = f_idx_u as u32;
        for &si in syms {
            ref_in[si as usize].push(f_idx);
        }
    }

    // Flatten CSR arrays
    let mut imp_out_offsets = Vec::with_capacity(num_files + 1);
    let mut imp_out_targets = Vec::new();
    let mut imp_out_reexport = Vec::new();
    for adj in &imp_out {
        imp_out_offsets.push(imp_out_targets.len() as u32);
        for &(dst, reexp) in adj {
            imp_out_targets.push(dst);
            imp_out_reexport.push(if reexp { 1 } else { 0 });
        }
    }
    imp_out_offsets.push(imp_out_targets.len() as u32);

    let mut imp_in_offsets = Vec::with_capacity(num_files + 1);
    let mut imp_in_sources = Vec::new();
    for adj in &imp_in {
        imp_in_offsets.push(imp_in_sources.len() as u32);
        imp_in_sources.extend_from_slice(adj);
    }
    imp_in_offsets.push(imp_in_sources.len() as u32);

    let mut ref_out_offsets = Vec::with_capacity(num_files + 1);
    let mut ref_out_targets = Vec::new();
    for adj in &ref_out {
        ref_out_offsets.push(ref_out_targets.len() as u32);
        ref_out_targets.extend_from_slice(adj);
    }
    ref_out_offsets.push(ref_out_targets.len() as u32);

    let mut ref_in_offsets = Vec::with_capacity(symbols.len() + 1);
    let mut ref_in_sources = Vec::new();
    for adj in &ref_in {
        ref_in_offsets.push(ref_in_sources.len() as u32);
        ref_in_sources.extend_from_slice(adj);
    }
    ref_in_offsets.push(ref_in_sources.len() as u32);

    let total_edges = (def_syms.len() + imp_out_targets.len() + ref_out_targets.len()) as u32;

    // Compute sparse PageRank over files + symbols (DEFINES + IMPORTS + REFERENCES)
    compute_unfocused_pagerank(
        &mut files,
        &mut symbols,
        &def_offsets,
        &def_syms,
        &imp_out_offsets,
        &imp_out_targets,
        &ref_out_offsets,
        &ref_out_targets,
    );

    // Pre-sort file_order, sym_name_order, and ranked_syms for O(log N) / O(1) warm queries
    let mut file_order: Vec<u32> = (0..files.len() as u32).collect();
    file_order.sort_unstable_by(|&a, &b| files[a as usize].path.cmp(&files[b as usize].path));

    let mut sym_name_order: Vec<u32> = (0..symbols.len() as u32).collect();
    sym_name_order.sort_unstable_by(|&a, &b| {
        symbols[a as usize]
            .name
            .cmp(&symbols[b as usize].name)
            .then_with(|| symbols[a as usize].file_idx.cmp(&symbols[b as usize].file_idx))
            .then_with(|| {
                symbols[a as usize]
                    .line_start
                    .cmp(&symbols[b as usize].line_start)
            })
    });

    let mut ranked_syms: Vec<u32> = (0..symbols.len() as u32).collect();
    ranked_syms.sort_unstable_by(|&a, &b| {
        symbols[b as usize]
            .score
            .total_cmp(&symbols[a as usize].score)
            .then_with(|| a.cmp(&b))
    });

    GraphCacheData {
        version: CACHE_FORMAT_VERSION,
        files,
        symbols,
        file_order,
        sym_name_order,
        ranked_syms,
        def_offsets,
        def_syms,
        imp_out_offsets,
        imp_out_targets,
        imp_out_reexport,
        imp_in_offsets,
        imp_in_sources,
        ref_in_offsets,
        ref_in_sources,
        ref_out_offsets,
        ref_out_targets,
        total_edges,
        noncode_files: Vec::new(),
    }
}

fn filter_closest_reachable<'a>(
    targets: &[&'a SymDesc],
    dist_map: &FxHashMap<u32, u8>,
) -> Vec<&'a SymDesc> {
    let mut best_hop = u8::MAX;
    let mut matched = Vec::new();
    for &t in targets {
        let Some(&hop) = dist_map.get(&t.file_idx) else {
            continue;
        };
        if hop < best_hop {
            best_hop = hop;
            matched.clear();
            matched.push(t);
        } else if hop == best_hop {
            matched.push(t);
        }
    }
    matched
}

#[allow(clippy::too_many_arguments)]
fn compute_unfocused_pagerank(
    files: &mut [FileEntry],
    symbols: &mut [SymbolEntry],
    def_offsets: &[u32],
    def_syms: &[u32],
    imp_out_offsets: &[u32],
    imp_out_targets: &[u32],
    ref_out_offsets: &[u32],
    ref_out_targets: &[u32],
) {
    let nf = files.len();
    let ns = symbols.len();
    let n = nf + ns;
    if n == 0 {
        return;
    }

    let alpha = 0.85f64;
    let init = 1.0 / (n as f64);
    let mut rank = vec![init; n];
    let mut next = vec![0.0f64; n];

    // Precompute out-degree for every file node (symbol nodes have out-degree 0)
    let mut out_deg = vec![0u32; nf];
    for f in 0..nf {
        if files[f].symbol_count < 0 {
            continue;
        }
        let d_defs = def_offsets[f + 1] - def_offsets[f];
        let d_imps = imp_out_offsets[f + 1] - imp_out_offsets[f];
        let d_refs = ref_out_offsets[f + 1] - ref_out_offsets[f];
        out_deg[f] = d_defs + d_imps + d_refs;
    }

    for _ in 0..60 {
        let mut dangling = 0.0f64;
        for f in 0..nf {
            if out_deg[f] == 0 {
                dangling += rank[f];
            }
        }
        // All symbol nodes are sinks (dangling)
        for s in 0..ns {
            dangling += rank[nf + s];
        }

        let base = (1.0 - alpha) / (n as f64) + alpha * dangling / (n as f64);
        next.fill(base);

        for f in 0..nf {
            let deg = out_deg[f];
            if deg == 0 {
                continue;
            }
            let contrib = alpha * rank[f] / (deg as f64);
            for &s in &def_syms[def_offsets[f] as usize..def_offsets[f + 1] as usize] {
                next[nf + s as usize] += contrib;
            }
            for &dst in &imp_out_targets[imp_out_offsets[f] as usize..imp_out_offsets[f + 1] as usize]
            {
                next[dst as usize] += contrib;
            }
            for &s in &ref_out_targets[ref_out_offsets[f] as usize..ref_out_offsets[f + 1] as usize]
            {
                next[nf + s as usize] += contrib;
            }
        }

        let mut diff = 0.0f64;
        for i in 0..n {
            diff += (next[i] - rank[i]).abs();
        }
        std::mem::swap(&mut rank, &mut next);
        if diff < 1e-6 {
            break;
        }
    }

    // Apply path-tier weights to symbol scores and sum file scores
    for f in files.iter_mut() {
        f.file_score = 0.0;
    }
    let f_weights: Vec<f32> = files.iter().map(|f| path_weight(&f.path)).collect();
    for (s_idx, sym) in symbols.iter_mut().enumerate() {
        let fi = sym.file_idx as usize;
        let score = (rank[nf + s_idx] as f32) * f_weights[fi];
        sym.score = score;
        files[fi].file_score += score;
    }
}

fn write_atomic_with_header(path: &Path, magic: [u8; 8], payload: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}.{}", std::process::id(), payload.len()));
    {
        let mut f = File::create(&tmp)?;
        let mut hdr = [0u8; HEADER_LEN];
        hdr[..8].copy_from_slice(&magic);
        hdr[8..12].copy_from_slice(&CACHE_FORMAT_VERSION.to_le_bytes());
        f.write_all(&hdr)?;
        f.write_all(payload)?;
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

pub fn save_graph_cache(data: &GraphCacheData, path: &Path) -> std::io::Result<()> {
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(data)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    write_atomic_with_header(path, GRAPH_CACHE_MAGIC, &bytes)
}

/// Zero-copy `mmap`-backed view of `ArchivedGraphCacheData`.
/// Loading a 20k-file graph cache takes ~1–5 ms with zero heap deserialization.
pub struct MmapGraphCache {
    mmap: Mmap,
}

impl MmapGraphCache {
    pub fn open(path: &Path) -> Option<Self> {
        let file = File::open(path).ok()?;
        let mmap = unsafe { Mmap::map(&file).ok()? };
        if mmap.len() < HEADER_LEN {
            return None;
        }
        if mmap[..8] != GRAPH_CACHE_MAGIC {
            return None;
        }
        let ver = u32::from_le_bytes(mmap[8..12].try_into().ok()?);
        if ver != CACHE_FORMAT_VERSION {
            return None;
        }
        // Validate archive bytes once safely
        rkyv::access::<ArchivedGraphCacheData, rkyv::rancor::Error>(&mmap[HEADER_LEN..]).ok()?;
        Some(Self { mmap })
    }

    #[inline]
    pub fn archived(&self) -> &ArchivedGraphCacheData {
        // Already validated in `open()`
        unsafe { rkyv::access_unchecked::<ArchivedGraphCacheData>(&self.mmap[HEADER_LEN..]) }
    }
}

pub fn save_parse_cache(data: &ParseCacheData, path: &Path) -> std::io::Result<()> {
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(data)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    write_atomic_with_header(path, PARSE_CACHE_MAGIC, &bytes)
}

pub fn load_parse_cache(path: &Path) -> FxHashMap<String, ParsedFileRecord> {
    let mut map = FxHashMap::default();
    let Ok(file) = File::open(path) else {
        return map;
    };
    let Ok(mmap) = (unsafe { Mmap::map(&file) }) else {
        return map;
    };
    if mmap.len() < HEADER_LEN || mmap[..8] != PARSE_CACHE_MAGIC {
        return map;
    }
    let Ok(ver_bytes): Result<[u8; 4], _> = mmap[8..12].try_into() else {
        return map;
    };
    if u32::from_le_bytes(ver_bytes) != CACHE_FORMAT_VERSION {
        return map;
    }
    let Ok(archived) =
        rkyv::access::<crate::model::ArchivedParseCacheData, rkyv::rancor::Error>(&mmap[HEADER_LEN..])
    else {
        return map;
    };
    let Ok(data): Result<ParseCacheData, _> =
        rkyv::deserialize::<ParseCacheData, rkyv::rancor::Error>(archived)
    else {
        return map;
    };
    map.reserve(data.files.len());
    for rec in data.files {
        map.insert(rec.rel_path.clone(), rec);
    }
    map
}

// Re-export helpers used by query.rs
pub fn archived_find_file(g: &ArchivedGraphCacheData, target: &str) -> Option<u32> {
    let order = g.file_order.as_slice();
    let idx = order
        .binary_search_by(|&fi| {
            let fi_u = fi.to_native() as usize;
            g.files[fi_u].path.as_str().cmp(target)
        })
        .ok()?;
    let fi = order[idx].to_native();
    if g.files[fi as usize].symbol_count.to_native() < 0 {
        return None;
    }
    Some(fi)
}

pub fn archived_find_symbols_by_name(g: &ArchivedGraphCacheData, name: &str) -> Vec<u32> {
    let order = g.sym_name_order.as_slice();
    let start = order.partition_point(|&si| {
        let si_u = si.to_native() as usize;
        g.symbols[si_u].name.as_str() < name
    });
    let mut out = Vec::new();
    for &si in &order[start..] {
        let si_u = si.to_native();
        if g.symbols[si_u as usize].name.as_str() != name {
            break;
        }
        out.push(si_u);
    }
    out
}

/// Barrel-only BFS reachability directly over the zero-copy `mmap` graph (used by `context()`).
pub fn archived_barrel_reachable(
    g: &ArchivedGraphCacheData,
    file_idx: u32,
    max_hops: u8,
) -> FxHashMap<u32, u8> {
    let mut dist = FxHashMap::default();
    let mut queue = VecDeque::new();
    let f = file_idx as usize;
    let s0 = g.imp_out_offsets[f].to_native() as usize;
    let e0 = g.imp_out_offsets[f + 1].to_native() as usize;
    for &dst_r in &g.imp_out_targets[s0..e0] {
        let dst = dst_r.to_native();
        dist.insert(dst, 1u8);
        queue.push_back(dst);
    }
    while let Some(cur) = queue.pop_front() {
        let d = dist[&cur];
        if d >= max_hops {
            continue;
        }
        let cu = cur as usize;
        let barrel = is_barrel_file(g.files[cu].path.as_str());
        let s = g.imp_out_offsets[cu].to_native() as usize;
        let e = g.imp_out_offsets[cu + 1].to_native() as usize;
        for pos in s..e {
            if !barrel && g.imp_out_reexport[pos] == 0 {
                continue;
            }
            let nxt = g.imp_out_targets[pos].to_native();
            if nxt == file_idx || dist.contains_key(&nxt) {
                continue;
            }
            dist.insert(nxt, d + 1);
            queue.push_back(nxt);
        }
    }
    dist
}
