use crate::cache::CodeIndex;
use crate::graph::{
    archived_barrel_reachable, archived_find_file, archived_find_symbols_by_name,
};
use crate::model::{language_family, ArchivedGraphCacheData, SymbolKind};
use crate::noncode::{
    find_noncode_mentions, find_noncode_mentions_with_pattern, is_generic_file_stem,
    is_test_or_fixture_path, TextFileHits,
};
use crate::parser::{is_stdlib_receiver, parse_file, ParseOptions};
use crate::walk::is_test_file;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::process::Command;

const CONTEXT_NOISE_NAMES: &[&str] = &[
    "get", "set", "put", "post", "delete", "head", "patch", "start", "stop", "run", "main",
    "init", "setup", "close", "dict", "list", "str", "int", "bool", "float", "type", "key",
    "value", "name", "data", "config", "result", "error", "test", "self", "cls", "app", "log",
    "logger", "enabled", "default", "constructor", "length", "size", "fetch", "send", "table",
    "one", "append", "write", "read", "update", "create", "find", "add", "remove", "index",
    "map", "filter", "sort", "join", "split", "trim", "replace", "push", "pop", "shift",
    "reduce", "keys", "values", "items", "search", "match", "query", "count", "call", "apply",
    "bind",
];

fn is_context_noise(name: &str) -> bool {
    CONTEXT_NOISE_NAMES.contains(&name)
}

fn context_kind_order(kind: u8) -> u8 {
    match SymbolKind::from_u8(kind) {
        SymbolKind::Function | SymbolKind::Method => 0,
        SymbolKind::Class => 1,
        SymbolKind::Type => 2,
        SymbolKind::Property => 3,
        SymbolKind::Variable => 4,
        _ => 9,
    }
}

pub fn find_similar_files(g: &ArchivedGraphCacheData, query: &str, max: usize) -> Vec<String> {
    let base = query.rsplit('/').next().unwrap_or(query).to_ascii_lowercase();
    let q_lower = query.to_ascii_lowercase();
    let mut out = Vec::new();
    for f in g.files.iter() {
        if f.symbol_count.to_native() < 0 {
            continue;
        }
        let path = f.path.as_str();
        let p_lower = path.to_ascii_lowercase();
        let p_base = p_lower.rsplit('/').next().unwrap_or(&p_lower);
        if p_base == base || p_base.contains(&base) || p_lower.contains(&q_lower) {
            out.push(path.to_string());
            if out.len() >= max {
                break;
            }
        }
    }
    out
}

pub fn get_caller_counts_for_file(
    g: &ArchivedGraphCacheData,
    file_path: &str,
) -> FxHashMap<String, usize> {
    let mut counts = FxHashMap::default();
    let Some(f_idx) = archived_find_file(g, file_path) else {
        return counts;
    };
    let fi = f_idx as usize;
    let ds = g.def_offsets[fi].to_native() as usize;
    let de = g.def_offsets[fi + 1].to_native() as usize;
    for &si_r in &g.def_syms[ds..de] {
        let si = si_r.to_native() as usize;
        let rs = g.ref_in_offsets[si].to_native() as usize;
        let re = g.ref_in_offsets[si + 1].to_native() as usize;
        let mut ext_count = 0usize;
        for &src_r in &g.ref_in_sources[rs..re] {
            if src_r.to_native() != f_idx {
                ext_count += 1;
            }
        }
        if ext_count > 0 {
            counts.insert(g.symbols[si].name.as_str().to_string(), ext_count);
        }
    }
    counts
}

pub fn cmd_stats(g: &ArchivedGraphCacheData) {
    let file_count = g
        .files
        .iter()
        .filter(|f| f.symbol_count.to_native() >= 0)
        .count();
    println!("Files:   {file_count}");
    println!("Symbols: {}", g.symbols.len());
    println!("Edges:   {}", g.total_edges.to_native());
}

fn build_focus_distances(g: &ArchivedGraphCacheData, focus_idxs: &[u32]) -> FxHashMap<u32, u8> {
    let mut dist = FxHashMap::default();
    let mut q = VecDeque::new();
    for &fi in focus_idxs {
        dist.insert(fi, 0u8);
        q.push_back(fi);
    }
    while let Some(cur) = q.pop_front() {
        let d = dist[&cur];
        if d >= 2 {
            continue;
        }
        let cu = cur as usize;
        let os = g.imp_out_offsets[cu].to_native() as usize;
        let oe = g.imp_out_offsets[cu + 1].to_native() as usize;
        for &dst_r in &g.imp_out_targets[os..oe] {
            let dst = dst_r.to_native();
            if !dist.contains_key(&dst) {
                dist.insert(dst, d + 1);
                q.push_back(dst);
            }
        }
        let is = g.imp_in_offsets[cu].to_native() as usize;
        let ie = g.imp_in_offsets[cu + 1].to_native() as usize;
        for &src_r in &g.imp_in_sources[is..ie] {
            let src = src_r.to_native();
            if !dist.contains_key(&src) {
                dist.insert(src, d + 1);
                q.push_back(src);
            }
        }
    }
    dist
}

pub fn cmd_map(
    g: &ArchivedGraphCacheData,
    focus_files: &[String],
    include_tests: bool,
    count_only: bool,
    offset: usize,
    limit: usize,
) {
    if g.files.is_empty() {
        if count_only {
            println!("total: 0");
        } else {
            println!("(empty index)");
        }
        return;
    }

    let mut focus_idxs = Vec::new();
    for f in focus_files {
        if let Some(fi) = archived_find_file(g, f) {
            focus_idxs.push(fi);
        } else {
            let sugg = find_similar_files(g, f, 3);
            if sugg.is_empty() {
                eprintln!("⚠ Focus path not found in index: \"{f}\"");
            } else {
                eprintln!(
                    "⚠ Focus path not found in index: \"{f}\" (did you mean: {}?)",
                    sugg.join(", ")
                );
            }
        }
    }

    let mut total_files = 0usize;
    for f in g.files.iter() {
        if f.symbol_count.to_native() < 0 {
            continue;
        }
        if !include_tests && is_test_file(f.path.as_str()) {
            continue;
        }
        total_files += 1;
    }

    // Build ordered symbol indices (fast path: use pre-sorted ranked_syms when unfocused!)
    let ordered: Vec<u32> = if focus_idxs.is_empty() {
        g.ranked_syms
            .iter()
            .map(|r| r.to_native())
            .filter(|&si| {
                if include_tests {
                    return true;
                }
                let fi = g.symbols[si as usize].file_idx.to_native() as usize;
                !is_test_file(g.files[fi].path.as_str())
            })
            .collect()
    } else {
        let dists = build_focus_distances(g, &focus_idxs);
        let mut scored: Vec<(u32, f32)> = (0..g.symbols.len() as u32)
            .filter_map(|si| {
                let sym = &g.symbols[si as usize];
                let fi = sym.file_idx.to_native();
                if !include_tests && is_test_file(g.files[fi as usize].path.as_str()) {
                    return None;
                }
                let mult = match dists.get(&fi) {
                    Some(0) => 250.0f32,
                    Some(1) => 12.0,
                    Some(2) => 2.5,
                    _ => 0.15,
                };
                Some((si, sym.score.to_native() * mult))
            })
            .collect();
        scored.sort_unstable_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        // Cap per-file symbols (max 10 per file) when focusing so a single huge focus file
        // doesn't consume all 50 slots and hide its neighborhood!
        let per_file_cap = 10usize;
        let mut counts: FxHashMap<u32, usize> = FxHashMap::default();
        let mut capped: Vec<u32> = Vec::with_capacity(scored.len());
        let mut overflow: Vec<u32> = Vec::new();
        for (si, _) in scored {
            let fi = g.symbols[si as usize].file_idx.to_native();
            let c = counts.entry(fi).or_insert(0);
            if *c < per_file_cap {
                *c += 1;
                capped.push(si);
            } else {
                overflow.push(si);
            }
        }
        capped.extend(overflow);
        capped
    };

    let total_symbols = ordered.len();
    if count_only {
        println!("total: {total_symbols}");
        return;
    }

    let end = (offset + limit).min(total_symbols);
    let slice = if offset < total_symbols {
        &ordered[offset..end]
    } else {
        &[]
    };

    // Preserve first-appearance file ordering
    let mut file_order_seen: Vec<u32> = Vec::new();
    let mut groups: FxHashMap<u32, Vec<String>> = FxHashMap::default();
    for &si in slice {
        let sym = &g.symbols[si as usize];
        let fi = sym.file_idx.to_native();
        let entry = groups.entry(fi).or_insert_with(|| {
            file_order_seen.push(fi);
            Vec::new()
        });
        entry.push(format!(
            "  {:>4}│ {}",
            sym.line_start.to_native(),
            sym.signature.as_str()
        ));
    }

    let mut out = Vec::new();
    for fi in &file_order_seen {
        out.push(format!("{}:", g.files[*fi as usize].path.as_str()));
        if let Some(lines) = groups.get(fi) {
            out.extend(lines.iter().cloned());
        }
        out.push(String::new());
    }
    println!("{}", out.join("\n").trim_end());

    if slice.len() < total_symbols {
        println!(
            "\nShowing {} of {} files, {} of {} symbols (ranked by PageRank)",
            file_order_seen.len(),
            total_files,
            slice.len(),
            total_symbols
        );
        println!("Use --limit N to show more, or --count for totals");
    }
}

enum TreeNode {
    File(i32),
    Dir(BTreeMap<String, TreeNode>),
}

fn format_tree(map: &BTreeMap<String, TreeNode>, indent: &str, out: &mut Vec<String>) {
    for (k, v) in map {
        match v {
            TreeNode::File(cnt) => out.push(format!("{indent}{k} ({cnt} symbols)")),
            TreeNode::Dir(sub) => {
                out.push(format!("{indent}{k}/"));
                format_tree(sub, &format!("{indent}  "), out);
            }
        }
    }
}

fn insert_tree_path(map: &mut BTreeMap<String, TreeNode>, parts: &[&str], cnt: i32) {
    if parts.len() == 1 {
        map.insert(parts[0].to_string(), TreeNode::File(cnt));
        return;
    }
    let entry = map
        .entry(parts[0].to_string())
        .or_insert_with(|| TreeNode::Dir(BTreeMap::new()));
    if let TreeNode::Dir(sub) = entry {
        insert_tree_path(sub, &parts[1..], cnt);
    }
}

pub fn cmd_structure(g: &ArchivedGraphCacheData, depth: usize, explicit_depth: bool) {
    let mut root_map: BTreeMap<String, TreeNode> = BTreeMap::new();
    for &fi_r in g.file_order.iter() {
        let f = &g.files[fi_r.to_native() as usize];
        let cnt = f.symbol_count.to_native();
        if cnt < 0 {
            continue;
        }
        let parts: Vec<&str> = f.path.as_str().split('/').collect();
        if parts.len() > depth + 1 {
            continue;
        }
        insert_tree_path(&mut root_map, &parts, cnt);
    }
    if root_map.is_empty() {
        println!("(empty index)");
        return;
    }
    let mut out = Vec::new();
    format_tree(&root_map, "", &mut out);
    println!("{}", out.join("\n"));
    if !explicit_depth {
        println!("\n(default depth {depth} — use --depth N to expand)");
    }
}

fn print_noncode_section(hits: &[TextFileHits], total_files: usize) {
    if hits.is_empty() {
        return;
    }
    let more = if total_files > hits.len() {
        format!(", showing top {}", hits.len())
    } else {
        String::new()
    };
    println!(
        "\nNon-code / config mentions ({total_files} file{}{more}):",
        if total_files == 1 { "" } else { "s" }
    );
    for fh in hits {
        for h in &fh.hits {
            println!("  {}:{}  {}", fh.file, h.line, h.content);
        }
        if fh.total_hits_in_file > fh.hits.len() {
            println!(
                "  {}  (... +{} more matches)",
                fh.file,
                fh.total_hits_in_file - fh.hits.len()
            );
        }
    }
}

fn split_query_tokens(query: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in query.split(|c: char| !c.is_alphanumeric()) {
        if raw.is_empty() {
            continue;
        }
        // Also split camelCase tokens like `reviewPage` -> `["review", "page"]`
        let chars: Vec<char> = raw.chars().collect();
        let mut start = 0usize;
        for i in 1..chars.len() {
            let prev = chars[i - 1];
            let cur = chars[i];
            let next_lower = chars.get(i + 1).map_or(false, |c| c.is_lowercase());
            if (prev.is_lowercase() && cur.is_uppercase())
                || (prev.is_uppercase() && cur.is_uppercase() && next_lower)
            {
                let part: String = chars[start..i].iter().collect();
                out.push(part.to_ascii_lowercase());
                start = i;
            }
        }
        let tail: String = chars[start..].iter().collect();
        if !tail.is_empty() {
            out.push(tail.to_ascii_lowercase());
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_search(
    idx: &CodeIndex,
    g: &ArchivedGraphCacheData,
    query: &str,
    kind: Option<&str>,
    path_filters: &[String],
    max_per_file: usize,
    include_tests: bool,
    no_tests_explicit: bool,
    count_only: bool,
    offset: usize,
    limit: usize,
    user_limit: bool,
    include_noncode: bool,
) {
    let eff_include_tests =
        include_tests || (!no_tests_explicit && path_filters_target_tests(path_filters));
    let matches_path = |p: &str| -> bool {
        path_filters.is_empty()
            || path_filters
                .iter()
                .any(|pf| p.contains(pf.trim_start_matches("./")))
    };

    let q_lower = query.to_ascii_lowercase();
    let tokens = split_query_tokens(query);
    let multi_token = tokens.len() > 1;

    // Also match files by path (critical for route files like `account-settings/page.tsx`)
    let mut matching_files: Vec<&str> = Vec::new();
    if kind.is_none() && offset == 0 && !count_only {
        for &fi_r in g.file_order.iter() {
            let f = &g.files[fi_r.to_native() as usize];
            if f.symbol_count.to_native() < 0 {
                continue;
            }
            let p = f.path.as_str();
            if !matches_path(p) {
                continue;
            }
            if !eff_include_tests && is_test_file(p) {
                continue;
            }
            let pl = p.to_ascii_lowercase();
            let hit = if multi_token {
                tokens.iter().all(|t| pl.contains(t.as_str()))
            } else {
                pl.contains(&q_lower)
            };
            if hit {
                matching_files.push(p);
            }
        }
        for p_r in g.noncode_files.iter() {
            let p = p_r.as_str();
            if !matches_path(p) {
                continue;
            }
            if !eff_include_tests && is_test_file(p) {
                continue;
            }
            let pl = p.to_ascii_lowercase();
            let hit = if multi_token {
                tokens.iter().all(|t| pl.contains(t.as_str()))
            } else {
                pl.contains(&q_lower)
            };
            if hit {
                matching_files.push(p);
            }
        }
    }

    let mut tier_exact_name: Vec<u32> = Vec::new();
    let mut tier_name_all: Vec<u32> = Vec::new();
    let mut tier_path_and_name: Vec<u32> = Vec::new();
    let mut tier_sig: Vec<u32> = Vec::new();
    let mut test_sym_count = 0usize;

    for &si_r in g.ranked_syms.iter() {
        let si = si_r.to_native();
        let sym = &g.symbols[si as usize];
        if let Some(k) = kind {
            if !SymbolKind::from_u8(sym.kind).matches_filter(k) {
                continue;
            }
        }
        let fi = sym.file_idx.to_native() as usize;
        let file_path = g.files[fi].path.as_str();
        if !matches_path(file_path) {
            continue;
        }
        let is_test = is_test_file(file_path);

        let name_l = sym.name.as_str().to_ascii_lowercase();
        let sig_l = sym.signature.as_str().to_ascii_lowercase();
        let file_l = file_path.to_ascii_lowercase();

        let matched_tier = if !multi_token {
            if name_l.contains(&q_lower) {
                Some(0u8)
            } else if sig_l.contains(&q_lower) {
                Some(3u8)
            } else {
                None
            }
        } else {
            // Require every token to appear in name, file path, or signature
            let mut all_ok = true;
            let mut name_hits = 0usize;
            let mut path_hits = 0usize;
            for tok in &tokens {
                let in_name = name_l.contains(tok.as_str());
                let in_path = file_l.contains(tok.as_str());
                let in_sig = sig_l.contains(tok.as_str());
                if in_name {
                    name_hits += 1;
                }
                if in_path {
                    path_hits += 1;
                }
                if !in_name && !in_path && !in_sig {
                    all_ok = false;
                    break;
                }
            }
            if !all_ok {
                None
            } else if name_l.contains(&q_lower) {
                Some(0)
            } else if name_hits == tokens.len() {
                Some(1)
            } else if name_hits + path_hits >= tokens.len() && (name_hits > 0 || path_hits == tokens.len()) {
                Some(2)
            } else {
                Some(3)
            }
        };

        if let Some(tier) = matched_tier {
            if is_test && !eff_include_tests {
                test_sym_count += 1;
                continue;
            }
            match tier {
                0 => tier_exact_name.push(si),
                1 => tier_name_all.push(si),
                2 => tier_path_and_name.push(si),
                _ => tier_sig.push(si),
            }
        }
    }

    let mut all_matches: Vec<u32> = Vec::with_capacity(
        tier_exact_name.len() + tier_name_all.len() + tier_path_and_name.len() + tier_sig.len(),
    );
    all_matches.extend(tier_exact_name);
    all_matches.extend(tier_name_all);
    all_matches.extend(tier_path_and_name);
    all_matches.extend(tier_sig);

    // Apply per-file cap (default 5) so one huge script doesn't eat all 30 slots
    let matches: Vec<u32> = if max_per_file > 0 {
        let mut per_file: FxHashMap<u32, usize> = FxHashMap::default();
        all_matches
            .into_iter()
            .filter(|&si| {
                let fi = g.symbols[si as usize].file_idx.to_native();
                let c = per_file.entry(fi).or_insert(0);
                if *c < max_per_file {
                    *c += 1;
                    true
                } else {
                    false
                }
            })
            .collect()
    } else {
        all_matches
    };

    if count_only {
        println!("total: {}", matches.len());
        return;
    }

    if !matching_files.is_empty() {
        let show_n = matching_files.len().min(10);
        println!("Matching files ({}):", matching_files.len());
        for p in &matching_files[..show_n] {
            println!("  {p}");
        }
        if matching_files.len() > show_n {
            println!(
                "  ... (+{} more files — use `betterrank files <path>`)",
                matching_files.len() - show_n
            );
        }
        println!();
    }

    let end = (offset + limit).min(matches.len());
    let slice = if offset < matches.len() {
        &matches[offset..end]
    } else {
        &[]
    };

    for &si in slice {
        let sym = &g.symbols[si as usize];
        let fi = sym.file_idx.to_native() as usize;
        println!(
            "{}:{}  [{}] {}",
            g.files[fi].path.as_str(),
            sym.line_start.to_native(),
            SymbolKind::from_u8(sym.kind).as_str(),
            sym.signature.as_str()
        );
    }

    if slice.is_empty() && matching_files.is_empty() {
        println!("(no matches for \"{query}\")");
    } else if slice.len() == limit && !user_limit && matches.len() > limit {
        println!("\n(showing top {limit} by relevance — use --limit N or --count for total)");
    }
    if test_sym_count > 0 {
        println!("(+ {test_sym_count} test matches hidden — pass --include-tests to expand)");
    }

    if include_noncode && offset == 0 && kind.is_none() {
        let (mut nc_hits, mut nc_total) = find_noncode_mentions(&idx.roots, Some(g), query, 10, 2);
        if !path_filters.is_empty() {
            nc_hits.retain(|fh| matches_path(&fh.file));
            nc_total = nc_hits.len();
        }
        print_noncode_section(&nc_hits, nc_total);
    }
}

fn rollup_directories(
    filtered: &[&str],
    pattern: Option<&str>,
    depth: usize,
) -> Vec<(String, usize)> {
    let base_prefix = pattern
        .map(|p| p.trim_start_matches("./").trim_end_matches('/'))
        .filter(|p| !p.is_empty() && !p.contains('*') && !p.contains('?'))
        .map(|p| format!("{p}/"));

    let d = depth.max(1);
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for &path in filtered {
        // Strip base_prefix per-file whenever matched so we NEVER collapse back up to a parent dir!
        let (active_pfx, rel) = match base_prefix.as_deref() {
            Some(pfx) if path.starts_with(pfx) => (Some(pfx), &path[pfx.len()..]),
            _ => (None, path),
        };
        let parts: Vec<&str> = rel.split('/').collect();
        let key = if parts.len() <= 1 {
            match active_pfx {
                Some(pfx) => format!("{pfx} (direct)"),
                None => "(root)".to_string(),
            }
        } else {
            let take_n = d.min(parts.len() - 1);
            let sub = parts[..take_n].join("/");
            match active_pfx {
                Some(pfx) => format!("{pfx}{sub}/"),
                None => format!("{sub}/"),
            }
        };
        *counts.entry(key).or_insert(0) += 1;
    }
    counts.into_iter().collect()
}

fn compile_file_pattern(raw: &str) -> (String, Option<Regex>) {
    let pat_lower = raw.trim_start_matches("./").trim_end_matches('/').to_ascii_lowercase();
    let has_glob = pat_lower.contains('*') || pat_lower.contains('?');
    let glob_re = if has_glob {
        let mut re_s = String::from("^.*");
        for ch in pat_lower.chars() {
            match ch {
                '*' => re_s.push_str(".*"),
                '?' => re_s.push('.'),
                other => re_s.push_str(&regex::escape(&other.to_string())),
            }
        }
        re_s.push_str(".*$");
        Regex::new(&re_s).ok()
    } else {
        None
    };
    (pat_lower, glob_re)
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_files(
    g: &ArchivedGraphCacheData,
    patterns: &[String],
    filter: Option<&str>,
    code_only: bool,
    noncode_only: bool,
    no_tests: bool,
    dirs_only: bool,
    depth: Option<usize>,
    show_all: bool,
    count_only: bool,
    offset: usize,
    limit: usize,
) {
    let mut paths: Vec<&str> = Vec::new();
    let mut unindexed_set: FxHashSet<&str> = FxHashSet::default();
    if !noncode_only {
        for &fi_r in g.file_order.iter() {
            let f = &g.files[fi_r.to_native() as usize];
            let p = f.path.as_str();
            if f.symbol_count.to_native() < 0 {
                unindexed_set.insert(p);
            }
            paths.push(p);
        }
    }
    if !code_only {
        for p_r in g.noncode_files.iter() {
            paths.push(p_r.as_str());
        }
    }
    paths.sort_unstable();
    paths.dedup();

    // For each non-glob pattern, check whether it matches as a real directory prefix (`apps/qa/`)
    // so `files apps/qa` doesn't accidentally pull in `apps/qa-other/...` or `.github/apps-qa.yml`!
    let compiled_pats: Vec<(String, Option<Regex>, bool)> = patterns
        .iter()
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            let (pl, re_opt) = compile_file_pattern(p);
            let is_dir_prefix = if re_opt.is_none() && !pl.is_empty() {
                let pfx = format!("{pl}/");
                paths.iter().any(|path| {
                    let low = path.to_ascii_lowercase();
                    low.starts_with(&pfx) || low == pl
                })
            } else {
                false
            };
            (pl, re_opt, is_dir_prefix)
        })
        .collect();
    let filter_re = filter.and_then(|pat| {
        regex::RegexBuilder::new(pat)
            .case_insensitive(true)
            .build()
            .ok()
    });
    let filter_sub = filter.map(|p| p.to_ascii_lowercase());

    let filtered: Vec<&str> = paths
        .into_iter()
        .filter(|&p| {
            if no_tests && is_test_file(p) {
                return false;
            }
            let p_low = p.to_ascii_lowercase();
            if !compiled_pats.is_empty() {
                let any_pat = compiled_pats.iter().any(|(pl, re_opt, is_dir_prefix)| {
                    if let Some(ref re) = re_opt {
                        re.is_match(&p_low)
                    } else if *is_dir_prefix {
                        p_low == *pl
                            || (p_low.starts_with(pl.as_str())
                                && p_low.as_bytes().get(pl.len()) == Some(&b'/'))
                    } else {
                        p_low.contains(pl.as_str())
                    }
                });
                if !any_pat {
                    return false;
                }
            }
            if let Some(ref re) = filter_re {
                if !re.is_match(p) {
                    return false;
                }
            } else if let Some(ref sub) = filter_sub {
                if !p_low.contains(sub.as_str()) {
                    return false;
                }
            }
            true
        })
        .collect();

    if count_only {
        println!("total: {}", filtered.len());
        return;
    }

    if filtered.is_empty() {
        println!("(no matching files)");
        return;
    }

    // Directory rollup: triggered explicitly via `--dirs`, or automatically ONLY when
    // filtered results exceed `--limit` (default 200) and neither `--all` nor `--offset` was passed.
    let auto_rollup = !show_all && offset == 0 && filtered.len() > limit;
    if dirs_only || auto_rollup {
        let mut rollup_depth = depth.unwrap_or(1);
        let single_pat = if compiled_pats.len() == 1 {
            Some(compiled_pats[0].0.as_str())
        } else {
            None
        };
        let mut dirs = rollup_directories(&filtered, single_pat, rollup_depth);
        // If auto-rolling up a subtree like `apps/qa` and depth=1 collapsed everything into a single
        // intermediate folder (e.g. `apps/qa/src/`), step 1 level deeper automatically!
        if depth.is_none() && dirs.len() == 1 && !dirs[0].0.ends_with("(direct)") {
            rollup_depth += 1;
            let deeper = rollup_directories(&filtered, single_pat, rollup_depth);
            if deeper.len() > 1 {
                dirs = deeper;
            }
        }
        let show_dirs = dirs.len().min(limit.max(200));
        for (dir, cnt) in &dirs[..show_dirs] {
            println!(
                "{dir}  ({cnt} {})",
                if *cnt == 1 { "file" } else { "files" }
            );
        }
        if dirs.len() > show_dirs {
            println!("... (+{} more directories)", dirs.len() - show_dirs);
        }
        println!(
            "\n{} files across {} {} (use a subdirectory, --filter <regex>, --depth N, --all, or --limit N to list individual files).",
            filtered.len(),
            dirs.len(),
            if dirs.len() == 1 { "directory" } else { "directories" }
        );
        return;
    }

    let eff_limit = if show_all { filtered.len() } else { limit };
    let end = (offset + eff_limit).min(filtered.len());
    let slice = if offset < filtered.len() {
        &filtered[offset..end]
    } else {
        &[]
    };
    for p in slice {
        if unindexed_set.contains(p) {
            println!("{p}  [unindexed]");
        } else {
            println!("{p}");
        }
    }
    if slice.len() < filtered.len() {
        println!(
            "\nShowing {} of {} files. Use --dirs, --filter <regex>, --all, --offset N, or a narrower prefix/glob.",
            slice.len(),
            filtered.len()
        );
    }
}

pub fn cmd_symbols(
    g: &ArchivedGraphCacheData,
    file: Option<&str>,
    kind: Option<&str>,
    count_only: bool,
    offset: usize,
    limit: usize,
    user_limit: bool,
) {
    let file_idx_filter = match file {
        Some(f) => match archived_find_file(g, f) {
            Some(fi) => Some(fi),
            None => {
                if count_only {
                    println!("total: 0");
                } else {
                    println!("(no symbols found)");
                }
                return;
            }
        },
        None => None,
    };

    let mut matches: Vec<u32> = if let Some(fi) = file_idx_filter {
        let ds = g.def_offsets[fi as usize].to_native() as usize;
        let de = g.def_offsets[fi as usize + 1].to_native() as usize;
        let mut syms: Vec<u32> = g.def_syms[ds..de]
            .iter()
            .map(|r| r.to_native())
            .filter(|&si| {
                kind.map_or(true, |k| {
                    SymbolKind::from_u8(g.symbols[si as usize].kind).matches_filter(k)
                })
            })
            .collect();
        syms.sort_unstable_by(|&a, &b| {
            g.symbols[b as usize]
                .score
                .to_native()
                .total_cmp(&g.symbols[a as usize].score.to_native())
        });
        syms
    } else {
        g.ranked_syms
            .iter()
            .map(|r| r.to_native())
            .filter(|&si| {
                kind.map_or(true, |k| {
                    SymbolKind::from_u8(g.symbols[si as usize].kind).matches_filter(k)
                })
            })
            .collect()
    };

    if count_only {
        println!("total: {}", matches.len());
        return;
    }

    let total = matches.len();
    let end = (offset + limit).min(total);
    let slice = if offset < total {
        &matches[offset..end]
    } else {
        &[]
    };

    for &si in slice {
        let sym = &g.symbols[si as usize];
        let fi = sym.file_idx.to_native() as usize;
        println!(
            "{}:{}  [{}] {}",
            g.files[fi].path.as_str(),
            sym.line_start.to_native(),
            SymbolKind::from_u8(sym.kind).as_str(),
            sym.signature.as_str()
        );
    }

    if slice.is_empty() {
        println!("(no symbols found)");
    } else if slice.len() == limit && !user_limit && total > limit {
        println!("\n(showing top {limit} by relevance — use --limit N or --count for total)");
    }
    matches.clear();
}

fn print_compact_tests_footer(
    g: &ArchivedGraphCacheData,
    hidden_tests: &mut [(u32, f32)],
    with_context_hint: bool,
) {
    if hidden_tests.is_empty() {
        return;
    }
    hidden_tests.sort_unstable_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| g.files[a.0 as usize].path.as_str().cmp(g.files[b.0 as usize].path.as_str()))
    });
    let total = hidden_tests.len();
    let max_show = 15usize;
    let shown = total.min(max_show);
    println!("\nTests ({total}):");
    for &(fi, _) in &hidden_tests[..shown] {
        println!("  {}", g.files[fi as usize].path.as_str());
    }
    if total > shown {
        println!(
            "  ... (+{} more — pass --include-tests to expand)",
            total - shown
        );
    } else if with_context_hint {
        println!("  (pass --include-tests to expand with context)");
    }
}

fn path_filters_target_tests(path_filters: &[String]) -> bool {
    path_filters.iter().any(|pf| {
        let low = pf.to_ascii_lowercase();
        is_test_or_fixture_path(pf)
            || low.contains("test")
            || low.contains("spec")
            || low.contains("fixture")
            || low.contains("benchmark")
    })
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_callers(
    idx: &CodeIndex,
    g: &ArchivedGraphCacheData,
    raw_symbol: &str,
    file: Option<&str>,
    path_filters: &[String],
    context_lines: usize,
    include_tests: bool,
    no_tests_explicit: bool,
    count_only: bool,
    offset: usize,
    limit: usize,
    user_limit: bool,
    include_noncode: bool,
) {
    let eff_include_tests =
        include_tests || (!no_tests_explicit && path_filters_target_tests(path_filters));
    // Support file-scoped syntax (`router.ts:getItem`) AND receiver-qualified syntax (`review.getItem` / `api.review.getItem`)
    let (file_hint_from_sym, sym_after_colon) = match raw_symbol.rsplit_once(':') {
        Some((fpart, spart)) if !fpart.is_empty() && !spart.is_empty() => (Some(fpart), spart),
        _ => (None, raw_symbol),
    };
    let (receiver_hint, symbol) = match sym_after_colon.rsplit_once('.') {
        Some((rpart, spart)) if !rpart.is_empty() && !spart.is_empty() => {
            let last_recv = rpart.rsplit('.').next().unwrap_or(rpart);
            (Some(last_recv), spart)
        }
        _ => (None, sym_after_colon),
    };
    let effective_file = file.or(file_hint_from_sym);

    let mut target_syms = archived_find_symbols_by_name(g, symbol);
    if let Some(f) = effective_file {
        let f_norm = f.trim_start_matches("./");
        let suffix = format!("/{f_norm}");
        target_syms.retain(|&si| {
            let fi = g.symbols[si as usize].file_idx.to_native() as usize;
            let p = g.files[fi].path.as_str();
            p == f_norm || p.ends_with(&suffix)
        });
    }
    if let Some(recv) = receiver_hint {
        let recv_low = recv.to_ascii_lowercase();
        let narrowed: Vec<u32> = target_syms
            .iter()
            .copied()
            .filter(|&si| {
                let fi = g.symbols[si as usize].file_idx.to_native() as usize;
                let p = g.files[fi].path.as_str().to_ascii_lowercase();
                if p.contains(&format!("/{recv_low}."))
                    || p.starts_with(&format!("{recv_low}."))
                    || p.contains(&format!("/{recv_low}/"))
                {
                    return true;
                }
                let ds = g.def_offsets[fi].to_native() as usize;
                let de = g.def_offsets[fi + 1].to_native() as usize;
                g.def_syms[ds..de].iter().any(|r| {
                    let sname = g.symbols[r.to_native() as usize]
                        .name
                        .as_str()
                        .to_ascii_lowercase();
                    sname == recv_low || sname == format!("{recv_low}router")
                })
            })
            .collect();
        if !narrowed.is_empty() {
            target_syms = narrowed;
        }
    }

    let recv_call_re = receiver_hint.map(|recv| {
        let r_esc = regex::escape(recv);
        let s_esc = regex::escape(symbol);
        Regex::new(&format!(r"\b{r_esc}\s*\??\.\s*{s_esc}\b")).unwrap()
    });

    let mut caller_set: FxHashSet<u32> = FxHashSet::default();
    let mut callers: Vec<(u32, f32)> = Vec::new();
    let mut hidden_tests: Vec<(u32, f32)> = Vec::new();
    let mut seen_hidden_tests: FxHashSet<u32> = FxHashSet::default();

    for &si in &target_syms {
        let rs = g.ref_in_offsets[si as usize].to_native() as usize;
        let re = g.ref_in_offsets[si as usize + 1].to_native() as usize;
        for &src_r in &g.ref_in_sources[rs..re] {
            let fi = src_r.to_native();
            let cpath = g.files[fi as usize].path.as_str();
            if !path_filters.is_empty()
                && !path_filters
                    .iter()
                    .any(|pf| cpath.contains(pf.trim_start_matches("./")))
            {
                continue;
            }
            if let Some(ref rre) = recv_call_re {
                let abs = idx.resolve_abs_path(cpath);
                let ok = fs::read_to_string(&abs)
                    .map(|src| rre.is_match(&src))
                    .unwrap_or(false);
                if !ok {
                    continue;
                }
            }
            if !eff_include_tests && is_test_or_fixture_path(cpath) {
                if seen_hidden_tests.insert(fi) {
                    hidden_tests.push((fi, g.files[fi as usize].file_score.to_native()));
                }
                continue;
            }
            if caller_set.insert(fi) {
                callers.push((fi, g.files[fi as usize].file_score.to_native()));
            }
        }
    }

    callers.sort_unstable_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| g.files[a.0 as usize].path.as_str().cmp(g.files[b.0 as usize].path.as_str()))
    });

    if count_only {
        println!("total: {}", callers.len());
        return;
    }

    let total = callers.len();
    let end = (offset + limit).min(total);
    let slice = if offset < total {
        &callers[offset..end]
    } else {
        &[]
    };

    if context_lines > 0 {
        let escaped = regex::escape(symbol);
        let call_re = Regex::new(&format!(r"(^|[^A-Za-z0-9_]){escaped}\s*\(")).unwrap();
        let jsx_re = Regex::new(&format!(r"<\s*{escaped}([\s>/.]|$)")).unwrap();
        let member_re = Regex::new(&format!(r"\.\s*{escaped}\b")).unwrap();
        let bracket_re = Regex::new(&format!(r#"\[\s*['"`]{escaped}['"`]\s*\]"#)).unwrap();
        let import_re = Regex::new(&format!(r"(?:import|from)\s.*\b{escaped}\b")).unwrap();
        let type_annot_re =
            Regex::new(&format!(r"(?::|->|\[|,|\|)\s*{escaped}([^A-Za-z0-9_]|$)")).unwrap();
        let ident_re = Regex::new(&format!(r"(^|[^A-Za-z0-9_]){escaped}([^A-Za-z0-9_]|$)")).unwrap();

        let mut def_ranges: Vec<(u32, u32, u32)> = Vec::new();
        let mut is_non_callable = false;
        let mut is_class = false;
        for &si in &target_syms {
            let sym = &g.symbols[si as usize];
            def_ranges.push((
                sym.file_idx.to_native(),
                sym.line_start.to_native(),
                sym.line_end.to_native(),
            ));
            let k = SymbolKind::from_u8(sym.kind);
            if matches!(k, SymbolKind::Variable | SymbolKind::Type) {
                is_non_callable = true;
            }
            if k == SymbolKind::Class {
                is_class = true;
            }
        }

        for &(fi, _) in slice {
            let path = g.files[fi as usize].path.as_str();
            println!("{path}:");
            let abs = idx.resolve_abs_path(path);
            let Ok(source) = fs::read_to_string(&abs) else {
                println!("  (no call sites found in source)\n");
                continue;
            };
            let lines: Vec<&str> = source.split('\n').collect();
            let mut matched_indices: Vec<usize> = Vec::new();
            for (i, line) in lines.iter().enumerate() {
                let lnum = i as u32 + 1;
                // Skip only the declaration line (`lnum == ds`) so recursive self-calls inside the body still show!
                if def_ranges
                    .iter()
                    .any(|&(dfi, ds, _de)| dfi == fi && lnum == ds)
                {
                    continue;
                }
                let matched = if let Some(ref rre) = recv_call_re {
                    rre.is_match(line)
                } else {
                    call_re.is_match(line)
                        || jsx_re.is_match(line)
                        || member_re.is_match(line)
                        || bracket_re.is_match(line)
                        || import_re.is_match(line)
                        || (is_class && type_annot_re.is_match(line))
                        || (is_non_callable && ident_re.is_match(line))
                };
                if matched {
                    matched_indices.push(i);
                }
            }
            if matched_indices.is_empty() {
                println!("  (no call sites found in source)\n");
                continue;
            }
            // Merge overlapping context windows so adjacent call sites (e.g. lines 59, 60, 62)
            // print as one clean contiguous hunk instead of 3 duplicated blocks!
            let matched_set: FxHashSet<usize> = matched_indices.iter().copied().collect();
            let mut hunks: Vec<(usize, usize)> = Vec::new();
            for &i in &matched_indices {
                let s = i.saturating_sub(context_lines);
                let e = (i + context_lines).min(lines.len().saturating_sub(1));
                if let Some(last) = hunks.last_mut() {
                    if s <= last.1 + 1 {
                        last.1 = last.1.max(e);
                        continue;
                    }
                }
                hunks.push((s, e));
            }
            for (s, e) in hunks {
                for j in s..=e {
                    let sep = if matched_set.contains(&j) { "│" } else { "-" };
                    println!("  {:>6}{sep} {}", j + 1, lines[j]);
                }
                println!();
            }
        }
        if slice.is_empty() && (hidden_tests.is_empty() || no_tests_explicit) {
            println!("(no callers found)");
        }
    } else {
        for &(fi, _) in slice {
            println!("{}", g.files[fi as usize].path.as_str());
        }
        if slice.is_empty() && (hidden_tests.is_empty() || no_tests_explicit) {
            println!("(no callers found)");
        } else if slice.len() == limit && !user_limit && total > limit {
            println!("\n(showing top {limit} by relevance — use --limit N or --count for total)");
        }
    }

    if !no_tests_explicit {
        print_compact_tests_footer(g, &mut hidden_tests, context_lines > 0);
    }

    if include_noncode && offset == 0 {
        let (nc_hits, nc_total) = find_noncode_mentions(&idx.roots, Some(g), symbol, 10, 2);
        print_noncode_section(&nc_hits, nc_total);
    }
}

pub fn cmd_deps(
    g: &ArchivedGraphCacheData,
    file: &str,
    count_only: bool,
    offset: usize,
    limit: usize,
    user_limit: bool,
) {
    let Some(fi) = archived_find_file(g, file) else {
        print_file_not_found(g, file);
        return;
    };
    let s = g.imp_out_offsets[fi as usize].to_native() as usize;
    let e = g.imp_out_offsets[fi as usize + 1].to_native() as usize;
    let mut deps: Vec<(u32, f32)> = g.imp_out_targets[s..e]
        .iter()
        .map(|r| {
            let d = r.to_native();
            (d, g.files[d as usize].file_score.to_native())
        })
        .collect();
    deps.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));

    if count_only {
        println!("total: {}", deps.len());
        return;
    }
    let total = deps.len();
    let end = (offset + limit).min(total);
    let slice = if offset < total {
        &deps[offset..end]
    } else {
        &[]
    };
    for &(d, _) in slice {
        println!("{}", g.files[d as usize].path.as_str());
    }
    if slice.is_empty() {
        println!("(no dependencies)");
    } else if slice.len() == limit && !user_limit && total > limit {
        println!("\n(showing top {limit} by relevance — use --limit N or --count for total)");
    }
}

fn find_enclosing_package_name(
    abs_file: &std::path::Path,
    roots: &[crate::model::RootEntry],
) -> Option<String> {
    let stop_root = roots
        .iter()
        .find(|r| abs_file.starts_with(&r.path))
        .map(|r| r.path.as_path());
    let mut cur = abs_file.parent();
    while let Some(dir) = cur {
        if let Some(sr) = stop_root {
            if dir == sr {
                break;
            }
        }
        let pkg = dir.join("package.json");
        if pkg.is_file() {
            if let Ok(txt) = fs::read_to_string(&pkg) {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&txt) {
                    if let Some(name) = val.get("name").and_then(|v| v.as_str()) {
                        let trimmed = name.trim();
                        if !trimmed.is_empty() {
                            return Some(trimmed.to_string());
                        }
                    }
                }
            }
            break;
        }
        cur = dir.parent();
    }
    None
}

fn is_ambiguous_plain_word(stem: &str) -> bool {
    stem.len() <= 10 && stem.chars().all(|c| c.is_ascii_lowercase())
}

fn find_import_line_for_stem(
    abs: &std::path::Path,
    stem: &str,
    pkg_name: Option<&str>,
) -> Option<usize> {
    let source = fs::read_to_string(abs).ok()?;
    for (i, line) in source.lines().enumerate() {
        let t = line.trim_start();
        let is_imp = t.starts_with("import ")
            || t.starts_with("from ")
            || t.starts_with("export ")
            || t.contains(" from ")
            || t.contains("require(");
        if !is_imp {
            continue;
        }
        if line.contains(stem) || pkg_name.map_or(false, |pkg| line.contains(pkg)) {
            return Some(i + 1);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_dependents(
    idx: &CodeIndex,
    g: &ArchivedGraphCacheData,
    file: &str,
    include_tests: bool,
    no_tests_explicit: bool,
    count_only: bool,
    offset: usize,
    limit: usize,
    user_limit: bool,
    include_noncode: bool,
) {
    let Some(fi) = archived_find_file(g, file) else {
        print_file_not_found(g, file);
        return;
    };
    let s = g.imp_in_offsets[fi as usize].to_native() as usize;
    let e = g.imp_in_offsets[fi as usize + 1].to_native() as usize;

    let mut deps: Vec<(u32, f32)> = Vec::new();
    let mut hidden_tests: Vec<(u32, f32)> = Vec::new();
    for r in &g.imp_in_sources[s..e] {
        let d = r.to_native();
        let dpath = g.files[d as usize].path.as_str();
        if !include_tests && is_test_or_fixture_path(dpath) {
            hidden_tests.push((d, g.files[d as usize].file_score.to_native()));
            continue;
        }
        deps.push((d, g.files[d as usize].file_score.to_native()));
    }
    deps.sort_unstable_by(|a, b| b.1.total_cmp(&a.1));

    if count_only {
        println!("total: {}", deps.len());
        return;
    }
    let total = deps.len();
    let end = (offset + limit).min(total);
    let slice = if offset < total {
        &deps[offset..end]
    } else {
        &[]
    };

    // Build set of symbols defined in target file so we can show exact imported/used symbols per dependent!
    let ds = g.def_offsets[fi as usize].to_native() as usize;
    let de = g.def_offsets[fi as usize + 1].to_native() as usize;
    let target_def_set: FxHashSet<u32> = g.def_syms[ds..de].iter().map(|r| r.to_native()).collect();
    let base = file.rsplit('/').next().unwrap_or(file);
    let stem = base.rfind('.').map(|i| &base[..i]).unwrap_or(base);
    let target_abs = idx.resolve_abs_path(file);
    let enclosing_pkg = if stem == "index" {
        find_enclosing_package_name(&target_abs, &idx.roots)
    } else {
        None
    };

    for &(d, _) in slice {
        let dpath = g.files[d as usize].path.as_str();
        let rs = g.ref_out_offsets[d as usize].to_native() as usize;
        let re = g.ref_out_offsets[d as usize + 1].to_native() as usize;
        let mut used_syms: Vec<&str> = Vec::new();
        for &sym_r in &g.ref_out_targets[rs..re] {
            let si = sym_r.to_native();
            if target_def_set.contains(&si) {
                let name = g.symbols[si as usize].name.as_str();
                if !used_syms.contains(&name) {
                    used_syms.push(name);
                }
            }
        }
        let imp_line =
            find_import_line_for_stem(&idx.resolve_abs_path(dpath), stem, enclosing_pkg.as_deref());
        let loc = match imp_line {
            Some(ln) => format!("{dpath}:{ln}"),
            None => dpath.to_string(),
        };
        if used_syms.is_empty() {
            println!("{loc}");
        } else {
            let shown: Vec<&str> = used_syms.iter().take(6).copied().collect();
            let more = if used_syms.len() > 6 {
                format!(" (+{} more)", used_syms.len() - 6)
            } else {
                String::new()
            };
            println!("{loc}  uses: {}{more}", shown.join(", "));
        }
    }
    if slice.is_empty() && (hidden_tests.is_empty() || no_tests_explicit) {
        println!("(no dependents)");
    } else if slice.len() == limit && !user_limit && total > limit {
        println!("\n(showing top {limit} by relevance — use --limit N or --count for total)");
    }
    if !no_tests_explicit {
        print_compact_tests_footer(g, &mut hidden_tests, false);
    }

    if include_noncode && offset == 0 && stem.len() >= 3 && stem != "__init__" {
        // Compute path-qualified prefix (`parent/stem`, or `grandparent/src/stem` when parent is `src`/`lib`).
        let parts: Vec<&str> = file.split('/').collect();
        let qualified_stem = if parts.len() >= 3
            && matches!(parts[parts.len() - 2], "src" | "lib" | "dist")
        {
            Some(format!(
                "{}/{}/{}",
                parts[parts.len() - 3],
                parts[parts.len() - 2],
                stem
            ))
        } else if parts.len() >= 2 {
            Some(format!("{}/{}", parts[parts.len() - 2], stem))
        } else {
            None
        };

        let (nc_hits, nc_total) = if is_generic_file_stem(stem) {
            // For generic stems like `index.ts` / `shared.ts` / `utils.py`, NEVER match bare
            // `index.ts` across unrelated `package.json` files — require enclosing workspace
            // package name (e.g. `@scope/pkg`) or path-qualified `pkg/src/index` / `dir/shared`!
            let mut alts: Vec<String> = Vec::new();
            if let Some(ref pkg) = enclosing_pkg {
                alts.push(regex::escape(pkg));
            }
            if let Some(ref qs) = qualified_stem {
                alts.push(regex::escape(qs));
            }
            if alts.is_empty() {
                (Vec::new(), 0)
            } else {
                let pat = format!(r"(?:{})\b", alts.join("|"));
                find_noncode_mentions_with_pattern(&idx.roots, Some(g), stem, Some(&pat), 8, 2)
            }
        } else if is_ambiguous_plain_word(stem) {
            // For short plain lowercase English words (e.g. `drift.ts`, `cache.py`, `state.ts`),
            // require filename-with-extension (`drift.ts`) or path-qualified (`pkg/drift`) rather
            // than bare English prose `\bdrift\b` in CI workflows / SQL comments!
            let mut alts = vec![regex::escape(base)];
            if let Some(ref qs) = qualified_stem {
                alts.push(regex::escape(qs));
            }
            let pat = format!(r"(?:{})\b", alts.join("|"));
            find_noncode_mentions_with_pattern(&idx.roots, Some(g), stem, Some(&pat), 8, 2)
        } else if stem.len() >= 4 {
            find_noncode_mentions(&idx.roots, Some(g), stem, 8, 2)
        } else {
            (Vec::new(), 0)
        };
        print_noncode_section(&nc_hits, nc_total);
    }
}

fn print_file_not_found(g: &ArchivedGraphCacheData, file: &str) {
    eprintln!("File \"{file}\" not found in index.");
    let sugg = find_similar_files(g, file, 5);
    if !sugg.is_empty() {
        eprintln!("Did you mean:");
        for s in sugg {
            eprintln!("  {s}");
        }
    }
    eprintln!("Tip: File paths are relative to --root. Use `betterrank structure` to see indexed files.");
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_neighborhood(
    g: &ArchivedGraphCacheData,
    file: &str,
    hops: u8,
    max_files: usize,
    max_symbols: usize,
    count_only: bool,
    offset: usize,
    limit: usize,
) {
    let Some(start_fi) = archived_find_file(g, file) else {
        print_file_not_found(g, file);
        return;
    };

    let mut visited: FxHashMap<u32, u8> = FxHashMap::default();
    let mut direct_set: FxHashSet<u32> = FxHashSet::default();
    visited.insert(start_fi, 0);
    direct_set.insert(start_fi);

    let mut q = VecDeque::new();
    q.push_back(start_fi);
    while let Some(cur) = q.pop_front() {
        let d = visited[&cur];
        if d >= hops {
            continue;
        }
        let s = g.imp_out_offsets[cur as usize].to_native() as usize;
        let e = g.imp_out_offsets[cur as usize + 1].to_native() as usize;
        for &dst_r in &g.imp_out_targets[s..e] {
            let dst = dst_r.to_native();
            if d == 0 {
                direct_set.insert(dst);
            }
            if !visited.contains_key(&dst) {
                visited.insert(dst, d + 1);
                q.push_back(dst);
            }
        }
    }

    // Include direct dependents
    let is = g.imp_in_offsets[start_fi as usize].to_native() as usize;
    let ie = g.imp_in_offsets[start_fi as usize + 1].to_native() as usize;
    for &src_r in &g.imp_in_sources[is..ie] {
        let src = src_r.to_native();
        direct_set.insert(src);
        visited.entry(src).or_insert(1);
    }

    let total_visited = visited.len();

    let mut direct_files: Vec<u32> = direct_set.iter().copied().collect();
    let mut other_files: Vec<u32> = visited
        .keys()
        .copied()
        .filter(|fi| !direct_set.contains(fi))
        .collect();

    direct_files.sort_unstable_by(|&a, &b| {
        if a == start_fi {
            return std::cmp::Ordering::Less;
        }
        if b == start_fi {
            return std::cmp::Ordering::Greater;
        }
        g.files[b as usize]
            .file_score
            .to_native()
            .total_cmp(&g.files[a as usize].file_score.to_native())
    });
    other_files.sort_unstable_by(|&a, &b| {
        g.files[b as usize]
            .file_score
            .to_native()
            .total_cmp(&g.files[a as usize].file_score.to_native())
    });

    let mut selected = direct_files;
    if selected.len() < max_files {
        let rem = max_files - selected.len();
        selected.extend(other_files.into_iter().take(rem));
    }

    let selected_set: FxHashSet<u32> = selected.iter().copied().collect();
    let mut edges: Vec<(u32, u32)> = Vec::new();
    let mut sym_list: Vec<u32> = Vec::new();
    for &fi in &selected {
        let os = g.imp_out_offsets[fi as usize].to_native() as usize;
        let oe = g.imp_out_offsets[fi as usize + 1].to_native() as usize;
        for &dst_r in &g.imp_out_targets[os..oe] {
            let dst = dst_r.to_native();
            if selected_set.contains(&dst) {
                edges.push((fi, dst));
            }
        }
        let ds = g.def_offsets[fi as usize].to_native() as usize;
        let de = g.def_offsets[fi as usize + 1].to_native() as usize;
        for &si_r in &g.def_syms[ds..de] {
            sym_list.push(si_r.to_native());
        }
    }
    sym_list.sort_unstable_by(|&a, &b| {
        g.symbols[b as usize]
            .score
            .to_native()
            .total_cmp(&g.symbols[a as usize].score.to_native())
    });

    if count_only {
        println!(
            "files: {} ({} visited, {} after ranking)",
            selected.len(),
            total_visited,
            selected.len()
        );
        println!("symbols: {}", sym_list.len());
        println!("edges: {}", edges.len());
        return;
    }

    let end = (offset + limit).min(selected.len());
    let shown_files = if offset < selected.len() {
        &selected[offset..end]
    } else {
        &[]
    };

    if shown_files.len() < selected.len() {
        println!("Files ({} of {}):", shown_files.len(), selected.len());
    } else {
        println!("Files ({}):", shown_files.len());
    }
    for &fi in shown_files {
        println!("  {}", g.files[fi as usize].path.as_str());
    }

    if !edges.is_empty() {
        println!("\nImports ({}):", edges.len());
        for (s, d) in &edges {
            println!(
                "  {} → {}",
                g.files[*s as usize].path.as_str(),
                g.files[*d as usize].path.as_str()
            );
        }
    }

    if !sym_list.is_empty() {
        let shown = &sym_list[..sym_list.len().min(max_symbols)];
        if shown.len() < sym_list.len() {
            println!(
                "\nSymbols (top {} of {}, ranked by PageRank):",
                shown.len(),
                sym_list.len()
            );
        } else {
            println!("\nSymbols ({}):", sym_list.len());
        }
        let mut by_file: Vec<u32> = Vec::new();
        let mut groups: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        for &si in shown {
            let fi = g.symbols[si as usize].file_idx.to_native();
            groups
                .entry(fi)
                .or_insert_with(|| {
                    by_file.push(fi);
                    Vec::new()
                })
                .push(si);
        }
        for fi in by_file {
            println!("  {}:", g.files[fi as usize].path.as_str());
            if let Some(syms) = groups.get(&fi) {
                for &si in syms {
                    let sym = &g.symbols[si as usize];
                    println!(
                        "    {:>4}│ {}",
                        sym.line_start.to_native(),
                        sym.signature.as_str()
                    );
                }
            }
        }
        if shown.len() < sym_list.len() {
            println!(
                "\n(use --max-symbols N to show more, or `betterrank outline <file>` for a specific file)"
            );
        }
    }
}

pub fn cmd_context(
    idx: &CodeIndex,
    g: &ArchivedGraphCacheData,
    symbol: &str,
    file: Option<&str>,
    no_source: bool,
) {
    let mut candidates = archived_find_symbols_by_name(g, symbol);
    if let Some(f) = file {
        candidates.retain(|&si| {
            let fi = g.symbols[si as usize].file_idx.to_native() as usize;
            g.files[fi].path.as_str() == f
        });
    }
    if candidates.is_empty() {
        println!("(symbol \"{symbol}\" not found)");
        return;
    }
    candidates.sort_unstable_by(|&a, &b| {
        g.symbols[b as usize]
            .score
            .to_native()
            .total_cmp(&g.symbols[a as usize].score.to_native())
    });

    let target_si = candidates[0];
    let target = &g.symbols[target_si as usize];
    let target_fi = target.file_idx.to_native();
    let target_file = g.files[target_fi as usize].path.as_str();
    let target_line_start = target.line_start.to_native();
    let target_line_end = target.line_end.to_native();

    let abs = idx.resolve_abs_path(target_file);
    let Ok(source) = fs::read_to_string(&abs) else {
        println!("(symbol \"{symbol}\" not found)");
        return;
    };
    let lines: Vec<&str> = source.split('\n').collect();
    let target_family = language_family(target_file);
    let reachable = archived_barrel_reachable(g, target_fi, 4);

    // Outgoing verified REFERENCES set for target_fi
    let rs = g.ref_out_offsets[target_fi as usize].to_native() as usize;
    let re = g.ref_out_offsets[target_fi as usize + 1].to_native() as usize;
    let verified_refs: FxHashSet<u32> = g.ref_out_targets[rs..re]
        .iter()
        .map(|r| r.to_native())
        .collect();

    // Parse target file on demand for AST-only localRefs + localBindings
    let mut scoped_local_refs: FxHashSet<String> = FxHashSet::default();
    let mut local_bindings: FxHashSet<String> = FxHashSet::default();
    if let Some(parsed) = parse_file(
        target_file,
        &source,
        ParseOptions {
            include_outline_defs: false,
            include_local_refs: true,
            dedupe: false,
        },
    ) {
        let matched_idx = parsed
            .definitions
            .iter()
            .position(|d| d.name == symbol && d.line_start == target_line_start)
            .or_else(|| parsed.definitions.iter().position(|d| d.name == symbol));
        if let Some(mi) = matched_idx {
            if let Some(Some(info)) = parsed.local_info.get(mi) {
                scoped_local_refs = info.local_refs.clone();
                local_bindings = info.local_bindings.clone();
            }
        }
    }

    // Resolve usedSymbols directly via O(log S) archived_find_symbols_by_name!
    let mut used_syms: Vec<(u32, u8)> = Vec::new();
    for name in &scoped_local_refs {
        if name.len() < 3 || is_context_noise(name) || local_bindings.contains(name) {
            continue;
        }
        let syms = archived_find_symbols_by_name(g, name);
        let mut best: Option<(u32, u8, f32)> = None;
        let mut verified_fallback: Option<(u32, f32)> = None;

        for si in syms {
            let s = &g.symbols[si as usize];
            let sfi = s.file_idx.to_native();
            if sfi == target_fi && s.name.as_str() == symbol {
                continue;
            }
            if language_family(g.files[sfi as usize].path.as_str()) != target_family {
                continue;
            }
            if sfi != target_fi && !s.exported {
                continue;
            }
            let hop = if sfi == target_fi {
                0u8
            } else {
                reachable.get(&sfi).copied().unwrap_or(99)
            };
            let sc = s.score.to_native();
            if hop <= 4 {
                let better = best.map_or(true, |(_, bh, bsc)| {
                    hop < bh || (hop == bh && sc > bsc)
                });
                if better {
                    best = Some((si, hop, sc));
                }
            } else if verified_refs.contains(&si) {
                let better = verified_fallback.map_or(true, |(_, bsc)| sc > bsc);
                if better {
                    verified_fallback = Some((si, sc));
                }
            }
        }

        if let Some((si, _, _)) = best {
            used_syms.push((si, g.symbols[si as usize].kind));
        } else if let Some((si, _)) = verified_fallback {
            used_syms.push((si, g.symbols[si as usize].kind));
        }
    }

    // Resolve signature Type references
    let type_re = Regex::new(r"\b([A-Z][a-zA-Z0-9_]+)\b").unwrap();
    let mut seen_types: FxHashSet<String> = FxHashSet::default();
    let mut type_refs: Vec<u32> = Vec::new();
    for cap in type_re.captures_iter(target.signature.as_str()) {
        let tname = cap[1].to_string();
        if !seen_types.insert(tname.clone()) {
            continue;
        }
        let syms = archived_find_symbols_by_name(g, &tname);
        let mut best: Option<(u32, u8, f32)> = None;
        for si in syms {
            let s = &g.symbols[si as usize];
            let sfi = s.file_idx.to_native();
            if language_family(g.files[sfi as usize].path.as_str()) != target_family {
                continue;
            }
            if sfi != target_fi && !s.exported {
                continue;
            }
            let k = SymbolKind::from_u8(s.kind);
            if !matches!(k, SymbolKind::Class | SymbolKind::Type) {
                continue;
            }
            let hop = if sfi == target_fi {
                0u8
            } else {
                reachable.get(&sfi).copied().unwrap_or(99)
            };
            let sc = s.score.to_native();
            if best.map_or(true, |(_, bh, bsc)| hop < bh || (hop == bh && sc > bsc)) {
                best = Some((si, hop, sc));
            }
        }
        if let Some((si, _, _)) = best {
            type_refs.push(si);
        }
    }

    let type_names: FxHashSet<&str> = type_refs
        .iter()
        .map(|&si| g.symbols[si as usize].name.as_str())
        .collect();
    used_syms.retain(|&(si, _)| !type_names.contains(g.symbols[si as usize].name.as_str()));
    used_syms.sort_unstable_by(|&(a, ak), &(b, bk)| {
        context_kind_order(ak)
            .cmp(&context_kind_order(bk))
            .then_with(|| g.symbols[a as usize].name.as_str().cmp(g.symbols[b as usize].name.as_str()))
    });

    // Callers (including same-file)
    let mut caller_files: Vec<u32> = Vec::new();
    let mut seen_callers: FxHashSet<u32> = FxHashSet::default();
    for &si in &candidates {
        let cs = g.ref_in_offsets[si as usize].to_native() as usize;
        let ce = g.ref_in_offsets[si as usize + 1].to_native() as usize;
        for &src_r in &g.ref_in_sources[cs..ce] {
            let fi = src_r.to_native();
            if seen_callers.insert(fi) {
                caller_files.push(fi);
            }
        }
    }
    caller_files.sort_unstable_by(|&a, &b| {
        g.files[a as usize]
            .path
            .as_str()
            .cmp(g.files[b as usize].path.as_str())
    });

    let pad = target_line_end.to_string().len().max(4);
    println!(
        "── {} ({}:{}-{}) ──\n",
        target.name.as_str(),
        target_file,
        target_line_start,
        target_line_end
    );

    if !no_source {
        let s = target_line_start.saturating_sub(1) as usize;
        let e = (target_line_end as usize).min(lines.len());
        for i in s..e {
            println!("  {:>pad$}│ {}", i + 1, lines[i], pad = pad);
        }
        println!();
    } else if !target.signature.is_empty() {
        println!("  {}\n", target.signature.as_str());
    }

    if !type_refs.is_empty() {
        println!("Types:");
        for si in type_refs {
            let s = &g.symbols[si as usize];
            let tf = g.files[s.file_idx.to_native() as usize].path.as_str();
            let ls = s.line_start.to_native();
            let le = s.line_end.to_native();
            println!("  {} ({}:{})", s.name.as_str(), tf, ls);
            if let Ok(tsrc) = fs::read_to_string(idx.resolve_abs_path(tf)) {
                let tlines: Vec<&str> = tsrc.split('\n').collect();
                let preview_end = le.min(ls + 14) as usize;
                let pstart = ls.saturating_sub(1) as usize;
                for line in &tlines[pstart..preview_end.min(tlines.len())] {
                    println!("    {line}");
                }
                if le as usize > preview_end {
                    println!("    ... ({} more lines)", le as usize - preview_end);
                }
            }
            println!();
        }
    }

    if !used_syms.is_empty() {
        println!("References:");
        for (si, _) in used_syms {
            let s = &g.symbols[si as usize];
            let sfi = s.file_idx.to_native();
            let sf = g.files[sfi as usize].path.as_str();
            let loc = if sfi == target_fi {
                format!("line {}", s.line_start.to_native())
            } else {
                format!("{}:{}", sf, s.line_start.to_native())
            };
            let label = if s.signature.is_empty() {
                s.name.as_str()
            } else {
                s.signature.as_str()
            };
            println!(
                "  [{}] {}  ({})",
                SymbolKind::from_u8(s.kind).as_str(),
                label,
                loc
            );
        }
        println!();
    }

    if !caller_files.is_empty() {
        println!(
            "Callers ({} file{}):",
            caller_files.len(),
            if caller_files.len() == 1 { "" } else { "s" }
        );
        for fi in caller_files {
            let f = g.files[fi as usize].path.as_str();
            if fi == target_fi {
                println!("  {f} (same file)");
            } else {
                println!("  {f}");
            }
        }
    } else {
        println!("Callers: (none detected)");
    }
}

pub fn cmd_trace(
    idx: &CodeIndex,
    g: &ArchivedGraphCacheData,
    symbol: &str,
    file: Option<&str>,
    max_depth: usize,
) {
    let mut targets = archived_find_symbols_by_name(g, symbol);
    if let Some(f) = file {
        targets.retain(|&si| {
            let fi = g.symbols[si as usize].file_idx.to_native() as usize;
            g.files[fi].path.as_str() == f
        });
    }
    if targets.is_empty() {
        println!("(symbol \"{symbol}\" not found)");
        return;
    }
    targets.sort_unstable_by(|&a, &b| {
        g.symbols[b as usize]
            .score
            .to_native()
            .total_cmp(&g.symbols[a as usize].score.to_native())
    });
    let root_si = targets[0];
    let root_sym = &g.symbols[root_si as usize];
    let root_fi = root_sym.file_idx.to_native();

    let mut source_cache: FxHashMap<u32, Option<String>> = FxHashMap::default();
    let mut def_cache: FxHashMap<u32, Vec<(String, u32, u32)>> = FxHashMap::default();
    let mut visited: FxHashSet<(u32, String)> = FxHashSet::default();

    #[allow(clippy::too_many_arguments)]
    fn walk_up(
        idx: &CodeIndex,
        g: &ArchivedGraphCacheData,
        sym_name: &str,
        sym_fi: u32,
        sym_line: u32,
        depth: usize,
        max_depth: usize,
        source_cache: &mut FxHashMap<u32, Option<String>>,
        def_cache: &mut FxHashMap<u32, Vec<(String, u32, u32)>>,
        visited: &mut FxHashSet<(u32, String)>,
    ) {
        let indent = if depth == 0 {
            String::new()
        } else {
            format!("{}← ", "  ".repeat(depth))
        };
        println!(
            "{indent}{} ({}:{})",
            sym_name,
            g.files[sym_fi as usize].path.as_str(),
            sym_line
        );
        if depth >= max_depth || !visited.insert((sym_fi, sym_name.to_string())) {
            return;
        }

        let mut sym_idxs = archived_find_symbols_by_name(g, sym_name);
        sym_idxs.retain(|&si| g.symbols[si as usize].file_idx.to_native() == sym_fi);
        let self_range = sym_idxs
            .iter()
            .find(|&&si| g.symbols[si as usize].line_start.to_native() == sym_line)
            .or_else(|| sym_idxs.first())
            .map(|&si| {
                (
                    g.symbols[si as usize].line_start.to_native(),
                    g.symbols[si as usize].line_end.to_native(),
                )
            });

        let mut caller_fis: FxHashSet<u32> = FxHashSet::default();
        caller_fis.insert(sym_fi); // include intra-file callers
        for &si in &sym_idxs {
            let rs = g.ref_in_offsets[si as usize].to_native() as usize;
            let re = g.ref_in_offsets[si as usize + 1].to_native() as usize;
            for &src_r in &g.ref_in_sources[rs..re] {
                caller_fis.insert(src_r.to_native());
            }
        }

        let escaped = regex::escape(sym_name);
        let call_re = Regex::new(&format!(r"(^|[^A-Za-z0-9_]){escaped}\s*\(")).unwrap();
        let member_re = Regex::new(&format!(r"\.\s*{escaped}\s*\(")).unwrap();

        let mut next_nodes: Vec<(String, u32, u32)> = Vec::new();
        let mut added: FxHashSet<(u32, String)> = FxHashSet::default();

        for cfi in caller_fis {
            let path = g.files[cfi as usize].path.as_str();
            let src_opt = source_cache
                .entry(cfi)
                .or_insert_with(|| fs::read_to_string(idx.resolve_abs_path(path)).ok())
                .clone();
            let Some(src) = src_opt else { continue };

            let defs = def_cache
                .entry(cfi)
                .or_insert_with(|| {
                    parse_file(path, &src, ParseOptions::default())
                        .map(|p| {
                            let mut d: Vec<(String, u32, u32)> = p
                                .definitions
                                .into_iter()
                                .filter(|d| {
                                    !matches!(
                                        SymbolKind::from_u8(d.kind),
                                        SymbolKind::Class | SymbolKind::Type
                                    )
                                })
                                .map(|d| (d.name, d.line_start, d.line_end))
                                .collect();
                            d.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| (b.2 - b.1).cmp(&(a.2 - a.1))));
                            d
                        })
                        .unwrap_or_default()
                })
                .clone();

            let mut found_any = false;
            let mut module_line: Option<u32> = None;
            for (i, line) in src.split('\n').enumerate() {
                let lnum = i as u32 + 1;
                if cfi == sym_fi {
                    if let Some((sr, er)) = self_range {
                        if lnum >= sr && lnum <= er {
                            continue;
                        }
                    }
                }
                if !call_re.is_match(line) && !member_re.is_match(line) {
                    continue;
                }
                let containing = defs
                    .iter()
                    .rev()
                    .find(|d| lnum >= d.1 && lnum <= d.2);
                if let Some((cname, cline, _)) = containing {
                    if cfi == sym_fi && cname == sym_name {
                        continue;
                    }
                    if added.insert((cfi, cname.clone())) {
                        found_any = true;
                        next_nodes.push((cname.clone(), cfi, *cline));
                    }
                } else if module_line.is_none() {
                    module_line = Some(lnum);
                }
            }
            if !found_any && cfi != sym_fi {
                if let Some(ml) = module_line {
                    next_nodes.push(("<module>".to_string(), cfi, ml));
                }
            }
        }

        next_nodes.sort_by(|a, b| {
            g.files[a.1 as usize]
                .path
                .as_str()
                .cmp(g.files[b.1 as usize].path.as_str())
                .then_with(|| a.0.cmp(&b.0))
        });

        for (cname, cfi, cline) in next_nodes {
            if cname == "<module>" {
                println!(
                    "{}← <module> ({}:{})",
                    "  ".repeat(depth + 1),
                    g.files[cfi as usize].path.as_str(),
                    cline
                );
            } else {
                walk_up(
                    idx,
                    g,
                    &cname,
                    cfi,
                    cline,
                    depth + 1,
                    max_depth,
                    source_cache,
                    def_cache,
                    visited,
                );
            }
        }
    }

    walk_up(
        idx,
        g,
        root_sym.name.as_str(),
        root_fi,
        root_sym.line_start.to_native(),
        0,
        max_depth,
        &mut source_cache,
        &mut def_cache,
        &mut visited,
    );
}

pub fn cmd_callees(
    idx: &CodeIndex,
    g: &ArchivedGraphCacheData,
    symbol: &str,
    file: Option<&str>,
    max_depth: usize,
) {
    let mut targets = archived_find_symbols_by_name(g, symbol);
    if let Some(f) = file {
        targets.retain(|&si| {
            let fi = g.symbols[si as usize].file_idx.to_native() as usize;
            g.files[fi].path.as_str() == f
        });
    }
    if targets.is_empty() {
        println!("(symbol \"{symbol}\" not found)");
        return;
    }
    targets.sort_unstable_by(|&a, &b| {
        g.symbols[b as usize]
            .score
            .to_native()
            .total_cmp(&g.symbols[a as usize].score.to_native())
    });

    let root_si = targets[0];
    let root_sym = &g.symbols[root_si as usize];
    let root_fi = root_sym.file_idx.to_native();

    let mut parse_cache: FxHashMap<u32, Option<crate::parser::ParseOutput>> = FxHashMap::default();
    let mut visited: FxHashSet<(u32, String)> = FxHashSet::default();

    #[allow(clippy::too_many_arguments)]
    fn walk_down(
        idx: &CodeIndex,
        g: &ArchivedGraphCacheData,
        sym_name: &str,
        sym_fi: u32,
        sym_line: u32,
        depth: usize,
        max_depth: usize,
        parse_cache: &mut FxHashMap<u32, Option<crate::parser::ParseOutput>>,
        visited: &mut FxHashSet<(u32, String)>,
    ) {
        let indent = if depth == 0 {
            String::new()
        } else {
            format!("{}→ ", "  ".repeat(depth))
        };
        println!(
            "{indent}{} ({}:{})",
            sym_name,
            g.files[sym_fi as usize].path.as_str(),
            sym_line
        );
        if depth >= max_depth || !visited.insert((sym_fi, sym_name.to_string())) {
            return;
        }

        let path = g.files[sym_fi as usize].path.as_str();
        let entry = parse_cache.entry(sym_fi).or_insert_with(|| {
            let src = fs::read_to_string(idx.resolve_abs_path(path)).ok()?;
            parse_file(
                path,
                &src,
                ParseOptions {
                    include_outline_defs: false,
                    include_local_refs: true,
                    dedupe: false,
                },
            )
        });
        let Some(ref parsed) = entry else { return };
        let def_idx = parsed
            .definitions
            .iter()
            .position(|d| d.name == sym_name && d.line_start == sym_line)
            .or_else(|| parsed.definitions.iter().position(|d| d.name == sym_name));
        let Some(di) = def_idx else { return };
        let Some(Some(ref local)) = parsed.local_info.get(di) else {
            return;
        };
        if local.local_refs.is_empty() {
            return;
        }
        let local_refs = local.local_refs.clone();
        let local_bindings = local.local_bindings.clone();

        let os = g.ref_out_offsets[sym_fi as usize].to_native() as usize;
        let oe = g.ref_out_offsets[sym_fi as usize + 1].to_native() as usize;
        let mut callees: Vec<(String, u32, u32)> = Vec::new();
        let mut seen: FxHashSet<(u32, String)> = FxHashSet::default();
        for &tsi_r in &g.ref_out_targets[os..oe] {
            let tsi = tsi_r.to_native();
            let t = &g.symbols[tsi as usize];
            let tfi = t.file_idx.to_native();
            let tname = t.name.as_str();
            if tfi == sym_fi && tname == sym_name {
                continue;
            }
            if !matches!(
                SymbolKind::from_u8(t.kind),
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Class | SymbolKind::Property
            ) {
                continue;
            }
            if !local_refs.contains(tname)
                || local_bindings.contains(tname)
                || is_stdlib_receiver(tname)
            {
                continue;
            }
            if seen.insert((tfi, tname.to_string())) {
                callees.push((tname.to_string(), tfi, t.line_start.to_native()));
            }
        }
        callees.sort_by(|a, b| {
            g.files[a.1 as usize]
                .path
                .as_str()
                .cmp(g.files[b.1 as usize].path.as_str())
                .then_with(|| a.0.cmp(&b.0))
        });

        for (cname, cfi, cline) in callees {
            walk_down(
                idx,
                g,
                &cname,
                cfi,
                cline,
                depth + 1,
                max_depth,
                parse_cache,
                visited,
            );
        }
    }

    walk_down(
        idx,
        g,
        root_sym.name.as_str(),
        root_fi,
        root_sym.line_start.to_native(),
        0,
        max_depth,
        &mut parse_cache,
        &mut visited,
    );
}

pub fn cmd_history(
    idx: &CodeIndex,
    g: &ArchivedGraphCacheData,
    symbol: &str,
    file: Option<&str>,
    offset: usize,
    limit: usize,
    patch: bool,
) {
    let mut cands = archived_find_symbols_by_name(g, symbol);
    if let Some(f) = file {
        cands.retain(|&si| {
            let fi = g.symbols[si as usize].file_idx.to_native() as usize;
            g.files[fi].path.as_str() == f
        });
    }
    if cands.is_empty() {
        println!("(symbol \"{symbol}\" not found)");
        return;
    }
    cands.sort_unstable_by(|&a, &b| {
        g.symbols[b as usize]
            .score
            .to_native()
            .total_cmp(&g.symbols[a as usize].score.to_native())
    });
    let sym = &g.symbols[cands[0] as usize];
    let fi = sym.file_idx.to_native() as usize;
    let gpath = g.files[fi].path.as_str();
    let ls = sym.line_start.to_native();
    let le = sym.line_end.to_native();

    let mut git_cwd = idx.roots[0].path.clone();
    let mut git_rel = gpath.to_string();
    if idx.is_multi_root() {
        for r in &idx.roots {
            let pfx = format!("{}/", r.prefix);
            if let Some(rest) = gpath.strip_prefix(&pfx) {
                git_cwd = r.path.clone();
                git_rel = rest.to_string();
                break;
            }
        }
    }

    let range_arg = format!("{ls},{le}:{git_rel}");
    let mut cmd = Command::new("git");
    cmd.current_dir(&git_cwd).arg("log").arg("-L").arg(&range_arg);
    if !patch {
        cmd.arg("--no-patch")
            .arg("--format=%h  %ad  %s")
            .arg("--date=short");
    }
    cmd.arg(format!("--skip={offset}"))
        .arg("-n")
        .arg(limit.to_string());

    println!("{} ({gpath}:{ls}-{le})\n", sym.name.as_str());
    match cmd.output() {
        Ok(out) if out.status.success() => {
            let txt = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if txt.is_empty() {
                println!("(no commits found)");
            } else if patch {
                println!("{txt}");
            } else {
                for line in txt.lines() {
                    println!("  {line}");
                }
            }
        }
        _ => eprintln!("git log failed — is this a git repo?"),
    }
}

pub fn cmd_diff(idx: &CodeIndex, g: &ArchivedGraphCacheData, git_ref: &str) {
    let cwd = &idx.roots[0].path;
    let diff_out = Command::new("git")
        .current_dir(cwd)
        .args(["diff", "--name-only", git_ref])
        .output();
    let Ok(out) = diff_out else {
        eprintln!("git diff failed — is this a git repo?");
        return;
    };
    if !out.status.success() {
        eprintln!("git diff failed — is this a git repo?");
        return;
    }
    let mut changed: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|s| s.to_string())
        .collect();
    if let Ok(untracked) = Command::new("git")
        .current_dir(cwd)
        .args(["ls-files", "--others", "--exclude-standard"])
        .output()
    {
        for line in String::from_utf8_lossy(&untracked.stdout).lines() {
            if !line.is_empty() && !changed.iter().any(|c| c == line) {
                changed.push(line.to_string());
            }
        }
    }
    if changed.is_empty() {
        println!("(no symbol changes detected)");
        return;
    }

    let mut total_callers = 0usize;
    let mut printed_any = false;

    for rel in changed {
        let mut old_syms: FxHashMap<String, (u8, String)> = FxHashMap::default();
        if let Ok(show) = Command::new("git")
            .current_dir(cwd)
            .args(["show", &format!("{git_ref}:{rel}")])
            .output()
        {
            if show.status.success() {
                let src = String::from_utf8_lossy(&show.stdout);
                if let Some(parsed) = parse_file(&rel, &src, ParseOptions::default()) {
                    for d in parsed.definitions {
                        old_syms.insert(d.name, (d.kind, d.signature));
                    }
                }
            }
        }

        let mut cur_syms: FxHashMap<String, (u8, String)> = FxHashMap::default();
        if let Ok(src) = fs::read_to_string(cwd.join(&rel)) {
            if let Some(parsed) = parse_file(&rel, &src, ParseOptions::default()) {
                for d in parsed.definitions {
                    cur_syms.insert(d.name, (d.kind, d.signature));
                }
            }
        }

        let caller_counts = get_caller_counts_for_file(g, &rel);
        let mut changes: Vec<(&str, u8, String, usize)> = Vec::new();
        for (name, (kind, _)) in &old_syms {
            if !cur_syms.contains_key(name) {
                let cc = caller_counts.get(name).copied().unwrap_or(0);
                total_callers += cc;
                changes.push(("-", *kind, name.clone(), cc));
            }
        }
        for (name, (kind, sig)) in &cur_syms {
            match old_syms.get(name) {
                None => changes.push(("+", *kind, name.clone(), 0)),
                Some((_, old_sig)) if old_sig != sig => {
                    let cc = caller_counts.get(name).copied().unwrap_or(0);
                    total_callers += cc;
                    changes.push(("~", *kind, name.clone(), cc));
                }
                _ => {}
            }
        }

        if !changes.is_empty() {
            printed_any = true;
            changes.sort_by(|a, b| b.3.cmp(&a.3));
            println!("{rel}:");
            for (tag, kind, name, cc) in changes {
                let note = if cc > 0 {
                    format!("  ({cc} caller{})", if cc == 1 { "" } else { "s" })
                } else {
                    String::new()
                };
                println!(
                    "  {tag} [{}] {name}{note}",
                    SymbolKind::from_u8(kind).as_str()
                );
            }
            println!();
        }
    }

    if !printed_any {
        println!("(no symbol changes detected)");
    } else if total_callers > 0 {
        println!(
            "⚠ {total_callers} external caller{} of modified/removed symbols",
            if total_callers == 1 { "" } else { "s" }
        );
    }
}

pub fn cmd_orphans(
    g: &ArchivedGraphCacheData,
    level: &str,
    kind: Option<&str>,
    count_only: bool,
    offset: usize,
    limit: usize,
) {
    let excluded_bases = [
        "index", "main", "app", "server", "cli", "mod", "lib", "manage", "wsgi", "asgi",
        "handler", "lambda", "__init__", "__main__", "config", "settings", "conf", "conftest",
        "setup",
    ];
    let is_fp_file = |path: &str| -> bool {
        let base = path.rsplit('/').next().unwrap_or(path);
        if base.starts_with('.') || path.ends_with(".d.ts") || is_test_file(path) {
            return true;
        }
        let stem = base
            .rfind('.')
            .map(|i| &base[..i])
            .unwrap_or(base)
            .to_ascii_lowercase();
        excluded_bases.contains(&stem.as_str())
            || stem.ends_with(".config")
            || stem.ends_with("rc")
    };

    if level == "file" {
        let mut orphans: Vec<(u32, i32)> = Vec::new();
        for (fi_u, f) in g.files.iter().enumerate() {
            let cnt = f.symbol_count.to_native();
            if cnt < 0 || is_fp_file(f.path.as_str()) {
                continue;
            }
            let in_c = g.imp_in_offsets[fi_u + 1].to_native() - g.imp_in_offsets[fi_u].to_native();
            let out_c =
                g.imp_out_offsets[fi_u + 1].to_native() - g.imp_out_offsets[fi_u].to_native();
            if in_c == 0 && out_c == 0 {
                orphans.push((fi_u as u32, cnt));
            }
        }
        if count_only {
            println!("total: {}", orphans.len());
            return;
        }
        let total = orphans.len();
        let end = (offset + limit).min(total);
        let slice = if offset < total {
            &orphans[offset..end]
        } else {
            &[]
        };
        for &(fi, cnt) in slice {
            println!("{}  ({cnt} symbols)", g.files[fi as usize].path.as_str());
        }
        if slice.is_empty() {
            println!("(no orphan files found)");
        } else if slice.len() < total {
            println!(
                "\nShowing {} of {total} orphan files (use --limit N for more)",
                slice.len()
            );
        }
    } else {
        let mut orphans: Vec<u32> = Vec::new();
        for (si_u, s) in g.symbols.iter().enumerate() {
            let fi = s.file_idx.to_native();
            let path = g.files[fi as usize].path.as_str();
            if is_fp_file(path) {
                continue;
            }
            let k = SymbolKind::from_u8(s.kind);
            if k == SymbolKind::Method || s.name.len() <= 2 {
                continue;
            }
            if let Some(req) = kind {
                if !k.matches_filter(req) {
                    continue;
                }
            }
            let rs = g.ref_in_offsets[si_u].to_native() as usize;
            let re = g.ref_in_offsets[si_u + 1].to_native() as usize;
            let has_ext = g.ref_in_sources[rs..re]
                .iter()
                .any(|r| r.to_native() != fi);
            if !has_ext {
                orphans.push(si_u as u32);
            }
        }
        if count_only {
            println!("total: {}", orphans.len());
            return;
        }
        let total = orphans.len();
        let end = (offset + limit).min(total);
        let slice = if offset < total {
            &orphans[offset..end]
        } else {
            &[]
        };
        let mut by_file: Vec<u32> = Vec::new();
        let mut groups: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        for &si in slice {
            let fi = g.symbols[si as usize].file_idx.to_native();
            groups
                .entry(fi)
                .or_insert_with(|| {
                    by_file.push(fi);
                    Vec::new()
                })
                .push(si);
        }
        for fi in &by_file {
            println!("{}:", g.files[*fi as usize].path.as_str());
            if let Some(syms) = groups.get(fi) {
                for &si in syms {
                    let s = &g.symbols[si as usize];
                    println!(
                        "  {:>4}│ [{}] {}",
                        s.line_start.to_native(),
                        SymbolKind::from_u8(s.kind).as_str(),
                        s.signature.as_str()
                    );
                }
            }
            println!();
        }
        if slice.is_empty() {
            println!("(no orphan symbols found)");
        } else if slice.len() < total {
            println!(
                "Showing {} of {total} orphan symbols across {} files (use --limit N for more)",
                slice.len(),
                by_file.len()
            );
        }
    }
}
