use crate::model::{is_supported_extension, ParsedDef, SymbolKind};
use crate::parser::{parse_file, ParseOptions};
use rustc_hash::{FxHashMap, FxHashSet};

const MIN_COLLAPSE_LINES: u32 = 2;

pub fn parse_line_range(spec: &str) -> Option<(u32, u32)> {
    let s = spec.trim().trim_start_matches(['L', 'l']);
    let sep = s.find('-').or_else(|| s.find(':'))?;
    let start: u32 = s[..sep].trim().trim_start_matches(['L', 'l']).parse().ok()?;
    let end: u32 = s[sep + 1..]
        .trim()
        .trim_start_matches(['L', 'l'])
        .parse()
        .ok()?;
    if start < 1 || end < start {
        return None;
    }
    Some((start, end))
}

#[derive(Debug, Default)]
pub struct OutlineOpts<'a> {
    pub caller_counts: Option<&'a FxHashMap<String, usize>>,
    pub line_range: Option<(u32, u32)>,
    pub no_comments: bool,
    pub depth: u32,
    pub explicit_depth: bool,
    pub collapse_imports: bool,
    pub show_imports: bool,
    pub full_skeleton: bool,
    pub filter: Option<&'a str>,
    pub around: Option<&'a str>,
    pub around_before: usize,
    pub around_after: usize,
    pub full_under: Option<usize>,
    pub max_chars: Option<usize>,
    /// When true (default in budgeted non-TTY output), cap individual ultra-long lines (>500 chars)
    /// so a single 10 KB comment line cannot consume the entire output budget.
    pub cap_long_lines: bool,
}

fn shell_quote_path_spec(spec: &str) -> String {
    if spec
        .chars()
        .any(|c| matches!(c, '(' | ')' | '[' | ']' | ' ' | '$' | '&' | ';' | '\''))
    {
        format!("'{}'", spec.replace('\'', "'\\''"))
    } else {
        spec.to_string()
    }
}

fn format_source_line(line: &str, cap_long_lines: bool) -> std::borrow::Cow<'_, str> {
    const MAX_LINE_CHARS: usize = 500;
    if !cap_long_lines || line.len() <= MAX_LINE_CHARS {
        return std::borrow::Cow::Borrowed(line);
    }
    let char_count = line.chars().count();
    if char_count <= MAX_LINE_CHARS {
        return std::borrow::Cow::Borrowed(line);
    }
    let prefix: String = line.chars().take(MAX_LINE_CHARS).collect();
    std::borrow::Cow::Owned(format!(
        "{prefix}... (+{} chars truncated)",
        char_count - MAX_LINE_CHARS
    ))
}

fn parse_header_range(line: &str) -> Option<(String, u32, u32)> {
    // Matches `── file.ts:10-900 ──` or `── sym (file.ts:10-900) ──`
    // Careful: Next.js route-group paths like `app/(dashboard)/page.tsx:10-900` contain `(`!
    // Only unwrap `sym (...)` when the header actually ends with `)`.
    let inner = line.strip_prefix("── ")?.strip_suffix(" ──")?;
    let loc = if let Some(no_trail) = inner.strip_suffix(')') {
        if let Some(paren_start) = no_trail.rfind(" (") {
            &no_trail[paren_start + 2..]
        } else {
            inner
        }
    } else {
        inner
    };
    let (file, range) = loc.rsplit_once(':')?;
    let (start_s, end_s) = range.split_once('-')?;
    let start_num: u32 = start_s.parse().ok()?;
    let end_num: u32 = end_s.parse().ok()?;
    Some((file.to_string(), start_num, end_num))
}

fn parse_numbered_line(line: &str) -> Option<u32> {
    let prefix = line.split('│').next()?;
    prefix.trim().parse::<u32>().ok()
}

pub fn apply_max_chars(s: String, file_path: &str, max_chars: Option<usize>) -> String {
    let Some(limit) = max_chars else {
        return s;
    };
    if limit == 0 || s.len() <= limit {
        return s;
    }
    let all_lines: Vec<&str> = s.lines().collect();
    let total_lines = all_lines.len();
    let mut kept = String::with_capacity(limit + 180);
    let mut active_header: Option<(String, u32, u32)> = None;
    let mut last_line_num: Option<u32> = None;

    for (idx, &line) in all_lines.iter().enumerate() {
        let add_len = line.len() + if kept.is_empty() { 0 } else { 1 };
        if !kept.is_empty() && kept.len() + add_len > limit {
            let omitted = total_lines.saturating_sub(idx);
            // Collect remaining range of the interrupted slice PLUS any trailing slices from the same file!
            let mut next_ranges: Vec<String> = Vec::new();
            let mut target_file = file_path.to_string();
            if let Some((ref f, start_ln, end_ln)) = active_header {
                target_file = f.clone();
                let next_start = match last_line_num {
                    Some(cur) if cur < end_ln => Some(cur + 1),
                    None => Some(start_ln),
                    _ => None,
                };
                if let Some(ns) = next_start {
                    next_ranges.push(format!("L{ns}-{end_ln}"));
                }
            } else if let Some(cur) = last_line_num {
                next_ranges.push(format!("L{}-{}", cur + 1, cur + 300));
            }
            for &rem_line in &all_lines[idx..] {
                if let Some((rf, rs, re)) = parse_header_range(rem_line) {
                    if rf == target_file {
                        let tag = format!("L{rs}-{re}");
                        if !next_ranges.contains(&tag) {
                            next_ranges.push(tag);
                        }
                    }
                }
            }
            let resume = if !next_ranges.is_empty() {
                let shown_ranges: Vec<&str> =
                    next_ranges.iter().take(6).map(|r| r.as_str()).collect();
                let extra = if next_ranges.len() > 6 {
                    format!(" (+{} more hunks)", next_ranges.len() - 6)
                } else {
                    String::new()
                };
                let arg = shell_quote_path_spec(&format!(
                    "{target_file}:{}",
                    shown_ranges.join(",")
                ));
                format!(" — next: betterrank outline {arg}{extra}")
            } else {
                " — narrow with --filter <regex> or --around <regex>".to_string()
            };
            kept.push_str(&format!(
                "\n... (truncated at --max-chars {limit}; {omitted} more lines omitted{resume})"
            ));
            return kept;
        }
        if let Some(hdr) = parse_header_range(line) {
            active_header = Some(hdr);
            last_line_num = None;
        } else if let Some(ln) = parse_numbered_line(line) {
            last_line_num = Some(ln);
        }
        if !kept.is_empty() {
            kept.push('\n');
        }
        kept.push_str(line);
    }
    kept
}

#[allow(clippy::too_many_arguments)]
fn around_mode(
    lines: &[&str],
    file_path: &str,
    pat: &str,
    before_ctx: usize,
    after_ctx: usize,
    bounds: Option<(u32, u32)>,
    pad: usize,
    no_comments: bool,
    ext: &str,
) -> String {
    let re = regex::RegexBuilder::new(pat)
        .case_insensitive(true)
        .build()
        .ok();
    let sub = pat.to_ascii_lowercase();
    let comment_lines = if no_comments {
        detect_comment_lines(lines, ext)
    } else {
        FxHashSet::default()
    };

    let lo = bounds.map_or(1, |(s, _)| s.max(1)) as usize;
    let hi = bounds
        .map_or(lines.len(), |(_, e)| (e as usize).min(lines.len()))
        .min(lines.len());
    if lo > hi || lines.is_empty() {
        return format!("(no matches for --around \"{pat}\" in {file_path})");
    }

    let mut matched: Vec<usize> = Vec::new();
    for i in lo..=hi {
        if no_comments && comment_lines.contains(&(i as u32)) {
            continue;
        }
        let line = lines[i - 1];
        let hit = match &re {
            Some(r) => r.is_match(line),
            None => line.to_ascii_lowercase().contains(&sub),
        };
        if hit {
            matched.push(i);
        }
    }
    if matched.is_empty() {
        return format!("(no matches for --around \"{pat}\" in {file_path})");
    }

    let mut hunks: Vec<(usize, usize)> = Vec::new();
    for &m in &matched {
        let s = m.saturating_sub(before_ctx).max(lo);
        let e = (m + after_ctx).min(hi);
        if let Some(last) = hunks.last_mut() {
            if s <= last.1 + 1 {
                last.1 = last.1.max(e);
                continue;
            }
        }
        hunks.push((s, e));
    }

    let mut out = Vec::new();
    for (idx, (s, e)) in hunks.iter().enumerate() {
        if idx > 0 {
            out.push(String::new());
        }
        out.push(format!("── {file_path}:{s}-{e} ──"));
        for i in *s..=*e {
            if no_comments && comment_lines.contains(&(i as u32)) {
                continue;
            }
            out.push(format!("{:>pad$}│ {}", i, lines[i - 1], pad = pad));
        }
    }
    out.join("\n")
}

/// Lightweight block parser for `.prisma` schemas (`model`, `enum`, `type`, `view`) so
/// `outline schema.prisma` produces a clean skeleton and `outline schema.prisma:ModelName`
/// extracts the exact block!
fn parse_prisma_defs(lines: &[&str]) -> Vec<ParsedDef> {
    let mut defs = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        let kw_opt = trimmed
            .strip_prefix("model ")
            .map(|r| (SymbolKind::Class, "model", r))
            .or_else(|| trimmed.strip_prefix("enum ").map(|r| (SymbolKind::Type, "enum", r)))
            .or_else(|| trimmed.strip_prefix("type ").map(|r| (SymbolKind::Type, "type", r)))
            .or_else(|| trimmed.strip_prefix("view ").map(|r| (SymbolKind::Class, "view", r)));
        if let Some((kind, kw, rest)) = kw_opt {
            let name = rest
                .split(|c: char| c.is_whitespace() || c == '{')
                .next()
                .unwrap_or("")
                .trim();
            if !name.is_empty() && trimmed.contains('{') {
                let start = i as u32 + 1;
                let mut depth = 0i32;
                let mut end = start;
                let mut j = i;
                while j < lines.len() {
                    for b in lines[j].bytes() {
                        if b == b'{' {
                            depth += 1;
                        } else if b == b'}' {
                            depth -= 1;
                        }
                    }
                    end = j as u32 + 1;
                    if depth <= 0 {
                        break;
                    }
                    j += 1;
                }
                defs.push(ParsedDef {
                    name: name.to_string(),
                    kind: kind as u8,
                    signature: format!("{kw} {name}"),
                    line_start: start,
                    line_end: end,
                    body_start_line: start + 1,
                    exported: true,
                });
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    defs
}

fn structured_config_outline(
    lines: &[&str],
    file_path: &str,
    ext: &str,
    filter: Option<&str>,
    depth_opt: Option<u32>,
) -> Option<String> {
    let lower_ext = ext.to_ascii_lowercase();
    if !matches!(lower_ext.as_str(), ".json" | ".yml" | ".yaml" | ".toml") {
        return None;
    }

    let filter_re = filter.and_then(|pat| {
        regex::RegexBuilder::new(pat)
            .case_insensitive(true)
            .build()
            .ok()
    });
    let filter_sub = filter.map(|p| p.to_ascii_lowercase());
    let matches_filter = |label: &str| -> bool {
        if let Some(ref re) = filter_re {
            re.is_match(label)
        } else if let Some(ref sub) = filter_sub {
            label.to_ascii_lowercase().contains(sub)
        } else {
            true
        }
    };

    let mut entries: Vec<(u32, usize, String)> = Vec::new();
    if lower_ext == ".toml" {
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if trimmed.starts_with('[') && trimmed.ends_with(']') {
                entries.push((i as u32 + 1, 0, trimmed.to_string()));
            } else if !line.starts_with(' ') && !line.starts_with('\t') {
                if let Some((k, _)) = trimmed.split_once('=') {
                    let key = k.trim();
                    if !key.is_empty() {
                        entries.push((i as u32 + 1, 1, key.to_string()));
                    }
                }
            }
        }
    } else if lower_ext == ".yml" || lower_ext == ".yaml" {
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "---" {
                continue;
            }
            let indent = line.bytes().take_while(|&b| b == b' ').count();
            if indent > 2 {
                continue;
            }
            let body = trimmed.strip_prefix("- ").unwrap_or(trimmed);
            if let Some((k, _)) = body.split_once(':') {
                let key = k.trim().trim_matches(|c| c == '"' || c == '\'');
                if !key.is_empty() && !key.contains(' ') {
                    let level = if indent == 0 { 0 } else { 1 };
                    entries.push((i as u32 + 1, level, key.to_string()));
                }
            }
        }
    } else {
        // JSON: track brace depth / shallow indentation (level 0 = depth 1, level 1 = depth 2)
        let mut depth: i32 = 0;
        for (i, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if depth >= 1 && depth <= 2 && trimmed.starts_with('"') {
                if let Some(end_q) = trimmed[1..].find('"') {
                    let key = &trimmed[1..1 + end_q];
                    let after = trimmed[2 + end_q..].trim_start();
                    if after.starts_with(':') {
                        let level = (depth - 1) as usize;
                        entries.push((i as u32 + 1, level, key.to_string()));
                    }
                }
            }
            let mut in_str = false;
            let mut esc = false;
            for b in trimmed.bytes() {
                if esc {
                    esc = false;
                    continue;
                }
                if b == b'\\' && in_str {
                    esc = true;
                    continue;
                }
                if b == b'"' {
                    in_str = !in_str;
                    continue;
                }
                if !in_str {
                    if b == b'{' {
                        depth += 1;
                    } else if b == b'}' {
                        depth = (depth - 1).max(0);
                    }
                }
            }
        }
    }

    if entries.is_empty() {
        return None;
    }

    let total_lines = lines.len() as u32;
    let build_rows = |max_level: usize| -> Vec<String> {
        let mut rows: Vec<String> = Vec::new();
        for idx in 0..entries.len() {
            let (start, level, ref label) = entries[idx];
            if level > max_level || !matches_filter(label) {
                continue;
            }
            let mut end = total_lines;
            for next in &entries[idx + 1..] {
                if next.1 <= level {
                    end = next.0.saturating_sub(1).max(start);
                    break;
                }
            }
            let indent = if level == 0 { "  " } else { "    " };
            let range = if end > start {
                format!("L{start}-{end}")
            } else {
                format!("L{start}")
            };
            rows.push(format!("{indent}{range:<12} {label}"));
        }
        rows
    };

    let req_max_level = match depth_opt {
        Some(1) => 0usize,
        _ => 1usize,
    };
    let mut rows = build_rows(req_max_level);
    let mut collapsed_to_top = false;
    let est_bytes: usize = rows.iter().map(|r| r.len() + 1).sum();
    if req_max_level > 0 && filter.is_none() && est_bytes > 12_000 {
        let top_rows = build_rows(0);
        if !top_rows.is_empty() && top_rows.len() < rows.len() {
            rows = top_rows;
            collapsed_to_top = true;
        }
    }

    let mut out = Vec::with_capacity(rows.len() + 3);
    out.push(format!(
        "{file_path} ({} lines, {} keys — pass :ALL or Lstart-end to expand):",
        lines.len(),
        entries.len()
    ));
    if rows.is_empty() {
        out.push("  (no keys matched --filter)".to_string());
    } else {
        out.extend(rows);
        if collapsed_to_top {
            out.push(
                "  (large config: showing top-level keys only — use --filter <regex> or --depth 2)"
                    .to_string(),
            );
        }
    }
    Some(out.join("\n"))
}

pub fn build_outline(
    source: &str,
    file_path: &str,
    expand_symbols: &[String],
    opts: OutlineOpts<'_>,
) -> String {
    let max_chars = opts.max_chars;
    let res = build_outline_inner(source, file_path, expand_symbols, opts);
    apply_max_chars(res, file_path, max_chars)
}

fn build_outline_inner(
    source: &str,
    file_path: &str,
    expand_symbols: &[String],
    opts: OutlineOpts<'_>,
) -> String {
    let lines: Vec<&str> = source.split('\n').collect();
    let pad = lines.len().to_string().len().max(4);
    let depth = if opts.depth == 0 { 1 } else { opts.depth };
    let ext = file_path.rfind('.').map(|i| &file_path[i..]).unwrap_or("");

    // Support `file:ALL` to explicitly expand the entire file cleanly on any file type.
    // If `--full-under N` was ALSO passed and the file exceeds N lines, fall through to the
    // compact/structured skeleton instead of dumping thousands of lines!
    let mut eff_expand: Vec<String> = expand_symbols.to_vec();
    if eff_expand.len() == 1 && eff_expand[0].eq_ignore_ascii_case("ALL") {
        let within_full_under = opts.full_under.map_or(true, |n| lines.len() <= n);
        if within_full_under {
            if let Some(pat) = opts.around {
                return around_mode(
                    &lines,
                    file_path,
                    pat,
                    opts.around_before,
                    opts.around_after,
                    None,
                    pad,
                    opts.no_comments,
                    ext,
                );
            }
            return raw_view(
                &lines,
                file_path,
                pad,
                opts.line_range,
                opts.no_comments,
                opts.show_imports,
                opts.cap_long_lines,
                ext,
            );
        } else {
            eff_expand.clear();
        }
    }
    let expand_symbols = &eff_expand;

    // Fast path: if all expand targets are explicit line ranges (e.g. `outline schema.prisma 100-140,800-830`),
    // work cleanly on ANY file (including Prisma, YAML, JSON, Dockerfiles) without AST parsing!
    // If `--around <regex>` is ALSO passed, run `around_mode` inside each requested line range!
    if !expand_symbols.is_empty()
        && expand_symbols
            .iter()
            .all(|s| parse_line_range(s).is_some() || s.eq_ignore_ascii_case("ALL"))
    {
        if let Some(pat) = opts.around {
            let mut chunks = Vec::new();
            for target in expand_symbols {
                let bounds = if target.eq_ignore_ascii_case("ALL") {
                    None
                } else {
                    parse_line_range(target)
                };
                chunks.push(around_mode(
                    &lines,
                    file_path,
                    pat,
                    opts.around_before,
                    opts.around_after,
                    bounds,
                    pad,
                    opts.no_comments,
                    ext,
                ));
            }
            return chunks.join("\n\n");
        }
        return expand_mode(
            &lines,
            &[],
            file_path,
            expand_symbols,
            pad,
            opts.line_range,
            opts.no_comments,
            opts.show_imports,
            opts.cap_long_lines,
            ext,
        );
    }

    // `--around <regex>` with no symbol targets searches the whole file (or `--lines` bounds) directly!
    if let Some(pat) = opts.around {
        if expand_symbols.is_empty() {
            return around_mode(
                &lines,
                file_path,
                pat,
                opts.around_before,
                opts.around_after,
                opts.line_range,
                pad,
                opts.no_comments,
                ext,
            );
        }
    }

    // `--full-under N`: return full file when line count <= N and no symbols/ranges requested
    if let Some(n) = opts.full_under {
        if expand_symbols.is_empty() && opts.filter.is_none() && lines.len() <= n {
            return raw_view(
                &lines,
                file_path,
                pad,
                opts.line_range,
                opts.no_comments,
                opts.show_imports,
                opts.cap_long_lines,
                ext,
            );
        }
    }

    // Plain `--lines A-B` without folding flags returns a fast raw slice.
    // Combining `--lines` with `--depth`, `--no-comments`, `--annotate`, or `--collapse-imports`
    // folds first and slices the folded output cleanly.
    let wants_folded = opts.explicit_depth
        || depth != 1
        || opts.no_comments
        || opts.caller_counts.is_some()
        || opts.collapse_imports
        || opts.full_skeleton;
    if opts.line_range.is_some() && expand_symbols.is_empty() && !wants_folded {
        return raw_view(
            &lines,
            file_path,
            pad,
            opts.line_range,
            opts.no_comments,
            opts.show_imports,
            opts.cap_long_lines,
            ext,
        );
    }

    // Support `.prisma` schema models/enums/types/views as first-class outline blocks!
    let is_prisma = ext.eq_ignore_ascii_case(".prisma");
    if !is_supported_extension(file_path) && !is_prisma {
        if expand_symbols.is_empty()
            && !opts.full_skeleton
            && opts.line_range.is_none()
            && lines.len() > 40
        {
            let depth_opt = if opts.explicit_depth {
                Some(depth)
            } else {
                None
            };
            if let Some(cfg_out) =
                structured_config_outline(&lines, file_path, ext, opts.filter, depth_opt)
            {
                return cfg_out;
            }
        }
        return raw_view(
            &lines,
            file_path,
            pad,
            opts.line_range,
            opts.no_comments,
            opts.show_imports,
            opts.cap_long_lines,
            ext,
        );
    }

    let raw_defs: Vec<ParsedDef> = if is_prisma {
        let pdefs = parse_prisma_defs(&lines);
        if pdefs.is_empty() {
            return raw_view(
                &lines,
                file_path,
                pad,
                opts.line_range,
                opts.no_comments,
                opts.show_imports,
                opts.cap_long_lines,
                ext,
            );
        }
        pdefs
    } else {
        match parse_file(
            file_path,
            source,
            ParseOptions {
                include_outline_defs: true,
                include_local_refs: false,
                dedupe: false,
            },
        ) {
            Some(p) if !p.definitions.is_empty() => p.definitions,
            _ => {
                return raw_view(
                    &lines,
                    file_path,
                    pad,
                    opts.line_range,
                    opts.no_comments,
                    opts.show_imports,
                    opts.cap_long_lines,
                    ext,
                )
            }
        }
    };

    let kind_priority = |k: u8| -> u8 {
        match SymbolKind::from_u8(k) {
            SymbolKind::Class => 5,
            SymbolKind::Namespace => 4,
            SymbolKind::Method | SymbolKind::Function => 3,
            SymbolKind::Type | SymbolKind::Property => 2,
            SymbolKind::Variable => 1,
            SymbolKind::Other => 0,
        }
    };

    let mut by_line: FxHashMap<u32, ParsedDef> = FxHashMap::default();
    for d in raw_defs {
        let replace = by_line
            .get(&d.line_start)
            .map_or(true, |ex| kind_priority(d.kind) > kind_priority(ex.kind));
        if replace {
            by_line.insert(d.line_start, d);
        }
    }
    let mut defs: Vec<ParsedDef> = by_line.into_values().collect();
    defs.sort_by_key(|d| d.line_start);

    // Symbol-scoped `--around <regex>` (e.g. `outline router.ts:processOrder --around "status" -C 15`)
    if let Some(pat) = opts.around {
        if !expand_symbols.is_empty() {
            let mut chunks = Vec::new();
            for target in expand_symbols {
                if let Some((rs, re)) = parse_line_range(target) {
                    chunks.push(around_mode(
                        &lines,
                        file_path,
                        pat,
                        opts.around_before,
                        opts.around_after,
                        Some((rs, re)),
                        pad,
                        opts.no_comments,
                        ext,
                    ));
                    continue;
                }
                let matches: Vec<&ParsedDef> = defs.iter().filter(|d| d.name == *target).collect();
                if matches.is_empty() {
                    chunks.push(format!("Symbol \"{target}\" not found in {file_path}"));
                } else {
                    for d in matches {
                        chunks.push(around_mode(
                            &lines,
                            file_path,
                            pat,
                            opts.around_before,
                            opts.around_after,
                            Some((d.line_start, d.line_end)),
                            pad,
                            opts.no_comments,
                            ext,
                        ));
                    }
                }
            }
            return chunks.join("\n\n");
        }
    }

    if !expand_symbols.is_empty() {
        return expand_mode(
            &lines,
            &defs,
            file_path,
            expand_symbols,
            pad,
            opts.line_range,
            opts.no_comments,
            opts.show_imports,
            opts.cap_long_lines,
            ext,
        );
    }

    // Default to 1-line-per-symbol compact skeleton (collapsing imports) so large files
    // never blow past the output budget; `--depth 1` also stays in compact mode
    // restricted to top-level symbols (`d == 0`). Use folded `outline_mode` when
    // `--depth >= 2`, `--lines`, or `--full` is explicitly requested.
    let use_compact = !opts.full_skeleton
        && opts.line_range.is_none()
        && (!opts.explicit_depth || depth == 1);
    if use_compact {
        return compact_outline_mode(&lines, &defs, file_path, &opts);
    }

    let mut effective_opts = opts;
    if !effective_opts.show_imports {
        effective_opts.collapse_imports = true;
    }
    outline_mode(&lines, &defs, pad, depth, ext, effective_opts)
}

fn compact_outline_mode(
    lines: &[&str],
    defs: &[ParsedDef],
    file_path: &str,
    opts: &OutlineOpts<'_>,
) -> String {
    let filter_re = opts.filter.and_then(|pat| {
        regex::RegexBuilder::new(pat)
            .case_insensitive(true)
            .build()
            .ok()
    });
    let filter_sub = opts.filter.map(|p| p.to_ascii_lowercase());
    let top_level_only = opts.explicit_depth && opts.depth <= 1;

    // Compute parent nesting depth & immediate parent kind
    // (0 = top-level, 1 = class method or router/registry Variable property, >=2 = deep inner noise)
    let n = defs.len();
    let mut depth_of = vec![0usize; n];
    let mut parent_of: Vec<Option<usize>> = vec![None; n];
    for i in 0..n {
        let mut d = 0usize;
        let mut best_parent: Option<usize> = None;
        for j in 0..n {
            if i == j {
                continue;
            }
            if defs[i].line_start > defs[j].line_start && defs[i].line_end <= defs[j].line_end {
                d += 1;
                if best_parent.map_or(true, |p| {
                    defs[j].line_start >= defs[p].line_start && defs[j].line_end <= defs[p].line_end
                }) {
                    best_parent = Some(j);
                }
            }
        }
        depth_of[i] = d;
        parent_of[i] = best_parent;
    }

    let render_rows = |only_top: bool| -> Vec<(String, bool)> {
        let mut rows = Vec::new();
        for (i, def) in defs.iter().enumerate() {
            let d = depth_of[i];
            if d >= 2 {
                continue;
            }
            if only_top && d >= 1 {
                continue;
            }
            if d == 1 {
                let k = SymbolKind::from_u8(def.kind);
                let pk = parent_of[i].map(|p| SymbolKind::from_u8(defs[p].kind));
                let keep = match k {
                    SymbolKind::Method => true,
                    SymbolKind::Property => matches!(pk, Some(SymbolKind::Variable)),
                    SymbolKind::Class => true,
                    _ => false,
                };
                if !keep {
                    continue;
                }
            }
            if let Some(ref re) = filter_re {
                if !re.is_match(&def.name) && !re.is_match(&def.signature) {
                    continue;
                }
            } else if let Some(ref sub) = filter_sub {
                if !def.name.to_ascii_lowercase().contains(sub)
                    && !def.signature.to_ascii_lowercase().contains(sub)
                {
                    continue;
                }
            }

            let indent = if d == 0 { "  " } else { "    " };
            let range = if def.line_end > def.line_start {
                format!("L{}-{}", def.line_start, def.line_end)
            } else {
                format!("L{}", def.line_start)
            };
            let kind_s = SymbolKind::from_u8(def.kind).as_str();
            let sig = if def.signature.chars().count() > 96 {
                let mut s: String = def.signature.chars().take(93).collect();
                s.push_str("...");
                s
            } else {
                def.signature.clone()
            };
            let mut row = format!("{indent}{range:<12} [{kind_s:<8}] {sig}");
            if let Some(counts) = opts.caller_counts {
                if let Some(&cnt) = counts.get(&def.name) {
                    if cnt > 0 {
                        row.push_str(&format!(
                            "  ({} {})",
                            cnt,
                            if cnt == 1 { "caller" } else { "callers" }
                        ));
                    }
                }
            }
            let is_exp = def.exported
                || parent_of[i].map_or(false, |p| defs[p].exported)
                || def.signature.trim_start().starts_with("export ");
            rows.push((row, is_exp));
        }
        rows
    };

    let budget_bytes = match opts.max_chars {
        Some(0) => usize::MAX,
        Some(n) => n.saturating_sub(300).max(2_000),
        None => 16_000usize,
    };

    let mut rows = render_rows(top_level_only);
    let mut exported_first = false;
    let est_bytes: usize = rows.iter().map(|(r, _)| r.len() + 1).sum();

    // On huge files that would exceed the output budget, prioritize exported symbols (and their
    // router/class members) first so private helpers at the top never bury the exported API!
    if opts.filter.is_none() && est_bytes > budget_bytes {
        let has_exp = rows.iter().any(|(_, e)| *e);
        let has_priv = rows.iter().any(|(_, e)| !*e);
        if has_exp && has_priv {
            let mut exp_rows: Vec<(String, bool)> = Vec::new();
            let mut priv_rows: Vec<(String, bool)> = Vec::new();
            for item in rows {
                if item.1 {
                    exp_rows.push(item);
                } else {
                    priv_rows.push(item);
                }
            }
            exp_rows.extend(priv_rows);
            rows = exp_rows;
            exported_first = true;
        }
    }

    let mut out = Vec::with_capacity(rows.len() + 6);
    if let Some(pat) = opts.filter {
        out.push(format!(
            "{file_path} ({} lines, {} of {} symbols matching \"{pat}\"):",
            lines.len(),
            rows.len(),
            defs.len()
        ));
    } else if exported_first {
        out.push(format!(
            "{file_path} ({} lines, {} symbols — exported symbols first):",
            lines.len(),
            defs.len()
        ));
    } else {
        out.push(format!(
            "{file_path} ({} lines, {} symbols):",
            lines.len(),
            defs.len()
        ));
    }

    if !opts.show_imports && opts.filter.is_none() {
        if let Some(imp) = detect_import_block(lines) {
            out.push(format!(
                "  L{}-{}  [imports] ({} lines)",
                imp.start, imp.end, imp.line_count
            ));
        }
    }

    if rows.is_empty() && opts.filter.is_some() {
        out.push("  (no symbols matched --filter)".to_string());
    } else {
        let cap_bytes = if opts.filter.is_none() {
            budget_bytes
        } else {
            usize::MAX
        };
        let mut used_bytes = 0usize;
        let total_rows = rows.len();
        let mut shown_rows = 0usize;
        for (row, _) in rows {
            if used_bytes + row.len() + 1 > cap_bytes && shown_rows > 0 {
                break;
            }
            used_bytes += row.len() + 1;
            out.push(row);
            shown_rows += 1;
        }
        if shown_rows < total_rows {
            out.push(format!(
                "  ... (+{} more symbols — use --filter <regex> or --depth 1)",
                total_rows - shown_rows
            ));
        }
    }

    out.join("\n")
}

fn emit_line_range(
    out: &mut Vec<String>,
    lines: &[&str],
    start: usize,
    end: usize,
    pad: usize,
    no_comments: bool,
    comment_lines: &FxHashSet<u32>,
    show_imports: bool,
    cap_long_lines: bool,
) {
    // Collapse large top-of-file import blocks (>= 6 lines) by default in `:ALL` / `L1-...` slices
    // unless `--show-imports` is passed OR the requested range ends inside the import block!
    let imp_collapse = if !show_imports && start <= 10 {
        detect_import_block(lines).filter(|imp| {
            imp.line_count >= 6
                && (imp.start as usize) >= start
                && (imp.end as usize) < end
        })
    } else {
        None
    };

    let mut i = start;
    while i <= end {
        if let Some(ref imp) = imp_collapse {
            if i == imp.start as usize {
                out.push(format!(
                    "{}│ ... [imports collapsed: L{}-{} — pass --show-imports to expand]",
                    " ".repeat(pad),
                    imp.start,
                    imp.end
                ));
                i = (imp.end as usize) + 1;
                continue;
            }
        }
        if no_comments && comment_lines.contains(&(i as u32)) {
            i += 1;
            continue;
        }
        let formatted = format_source_line(lines[i - 1], cap_long_lines);
        out.push(format!("{:>pad$}│ {formatted}", i, pad = pad));
        i += 1;
    }
}

#[allow(clippy::too_many_arguments)]
fn raw_view(
    lines: &[&str],
    file_path: &str,
    pad: usize,
    line_range: Option<(u32, u32)>,
    no_comments: bool,
    show_imports: bool,
    cap_long_lines: bool,
    ext: &str,
) -> String {
    let start = line_range.map_or(1, |(s, _)| s.max(1)) as usize;
    let end = line_range
        .map_or(lines.len(), |(_, e)| (e as usize).min(lines.len()))
        .min(lines.len());
    let mut out = Vec::with_capacity(end.saturating_sub(start) + 2);
    out.push(format!("── {file_path}:{start}-{end} ──"));
    let comment_lines = if no_comments {
        detect_comment_lines(lines, ext)
    } else {
        FxHashSet::default()
    };
    emit_line_range(
        &mut out,
        lines,
        start,
        end,
        pad,
        no_comments,
        &comment_lines,
        show_imports,
        cap_long_lines,
    );
    out.join("\n")
}

fn find_import_redirect_hint(lines: &[&str], sym: &str) -> Option<(usize, String)> {
    let re = regex::Regex::new(&format!(r"\b{}\b", regex::escape(sym))).ok()?;
    let mut in_import = false;
    let mut imp_start = 0usize;
    let mut imp_buf = String::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if !in_import {
            if trimmed.starts_with("import ")
                || trimmed.starts_with("from ")
                || (trimmed.starts_with("export ") && trimmed.contains(" from "))
            {
                imp_start = i + 1;
                imp_buf.clear();
                imp_buf.push_str(trimmed);
                if trimmed.contains(" from ")
                    || trimmed.starts_with("from ")
                    || trimmed.ends_with(';')
                {
                    if re.is_match(&imp_buf) {
                        return Some((imp_start, imp_buf.clone()));
                    }
                } else {
                    in_import = true;
                }
            }
        } else {
            imp_buf.push(' ');
            imp_buf.push_str(trimmed);
            if trimmed.contains(" from ") || trimmed.ends_with(';') || trimmed.ends_with(')') {
                in_import = false;
                if re.is_match(&imp_buf) {
                    return Some((imp_start, imp_buf.clone()));
                }
            }
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn expand_mode(
    lines: &[&str],
    defs: &[ParsedDef],
    file_path: &str,
    expand_symbols: &[String],
    pad: usize,
    line_range: Option<(u32, u32)>,
    no_comments: bool,
    show_imports: bool,
    cap_long_lines: bool,
    ext: &str,
) -> String {
    let comment_lines = if no_comments {
        detect_comment_lines(lines, ext)
    } else {
        FxHashSet::default()
    };
    let batch_mode = expand_symbols.len() > 1;
    // Only cap multi-symbol name batches; never silently chop an explicit Lstart-end line range!
    let max_lines_per_symbol: usize = if batch_mode { 140 } else { usize::MAX };

    let mut out = Vec::new();
    for target in expand_symbols {
        if target.eq_ignore_ascii_case("ALL") {
            let start = 1usize;
            let end = lines.len();
            out.push(format!("── {file_path}:{start}-{end} ──"));
            emit_line_range(
                &mut out,
                lines,
                start,
                end,
                pad,
                no_comments,
                &comment_lines,
                show_imports,
                cap_long_lines,
            );
            if batch_mode {
                out.push(String::new());
            }
            continue;
        }
        // Support direct line-range slices like `3150-3220` or `L3150-3220`
        if let Some((rs, re)) = parse_line_range(target) {
            let start = rs.max(1) as usize;
            let end = (re as usize).min(lines.len());
            out.push(format!("── {file_path}:{start}-{end} ──"));
            emit_line_range(
                &mut out,
                lines,
                start,
                end,
                pad,
                no_comments,
                &comment_lines,
                show_imports,
                cap_long_lines,
            );
            if batch_mode {
                out.push(String::new());
            }
            continue;
        }

        let matches: Vec<&ParsedDef> = defs.iter().filter(|d| d.name == *target).collect();
        if matches.is_empty() {
            if let Some((ln, imp_text)) = find_import_redirect_hint(lines, target) {
                let preview = if imp_text.len() > 140 {
                    format!("{}...", &imp_text[..140])
                } else {
                    imp_text
                };
                out.push(format!(
                    "Symbol \"{target}\" is not defined in {file_path} — imported at L{ln}: {preview}"
                ));
                continue;
            }
            out.push(format!("Symbol \"{target}\" not found in {file_path}"));
            let lower = target.to_ascii_lowercase();
            let mut similar: Vec<&str> = Vec::new();
            for d in defs {
                if d.name.to_ascii_lowercase().contains(&lower)
                    && !similar.contains(&d.name.as_str())
                {
                    similar.push(&d.name);
                    if similar.len() >= 5 {
                        break;
                    }
                }
            }
            if !similar.is_empty() {
                out.push(format!("Did you mean: {}", similar.join(", ")));
            } else {
                let avail: Vec<&str> = defs.iter().take(40).map(|d| d.name.as_str()).collect();
                out.push(format!("Available: {}", avail.join(", ")));
            }
            continue;
        }
        for def in &matches {
            out.push(format!(
                "── {} ({}:{}-{}) ──",
                def.name, file_path, def.line_start, def.line_end
            ));
            let start = line_range
                .map_or(def.line_start, |(s, _)| def.line_start.max(s))
                .max(1) as usize;
            let end = line_range
                .map_or(def.line_end, |(_, e)| def.line_end.min(e))
                .min(lines.len() as u32) as usize;
            let mut emitted = 0usize;
            for i in start..=end {
                if no_comments && comment_lines.contains(&(i as u32)) {
                    continue;
                }
                if emitted >= max_lines_per_symbol {
                    let rem = end.saturating_sub(i) + 1;
                    out.push(format!(
                        "  ... (+{rem} more lines in {} — run: betterrank outline {file_path} {i}-{end})",
                        def.name
                    ));
                    break;
                }
                let formatted = format_source_line(lines[i - 1], cap_long_lines);
                out.push(format!("{:>pad$}│ {formatted}", i, pad = pad));
                emitted += 1;
            }
            if matches.len() > 1 || batch_mode {
                out.push(String::new());
            }
        }
    }
    out.join("\n").trim_end().to_string()
}

struct CollapseRange {
    start: u32,
    end: u32,
    line_count: u32,
    name: Option<String>,
}

fn detect_import_block(lines: &[&str]) -> Option<CollapseRange> {
    let mut depth = 0i32;
    let mut in_import = false;
    let mut first: Option<u32> = None;
    let mut last: Option<u32> = None;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with('#')
            || trimmed.starts_with("\"use ")
            || trimmed.starts_with("'use ")
        {
            continue;
        }
        if !in_import {
            if trimmed.starts_with("import ")
                || trimmed == "import"
                || (trimmed.starts_with("from ") && trimmed.contains(" import"))
            {
                if first.is_none() {
                    first = Some(i as u32 + 1);
                }
                depth = 0;
                for b in line.bytes() {
                    if b == b'{' || b == b'(' {
                        depth += 1;
                    } else if b == b'}' || b == b')' {
                        depth -= 1;
                    }
                }
                if depth > 0 {
                    in_import = true;
                } else {
                    last = Some(i as u32 + 1);
                }
            } else if first.is_some() {
                break;
            } else if i > 40 {
                break;
            }
        } else {
            for b in line.bytes() {
                if b == b'{' || b == b'(' {
                    depth += 1;
                } else if b == b'}' || b == b')' {
                    depth -= 1;
                }
            }
            if depth <= 0 {
                in_import = false;
                last = Some(i as u32 + 1);
            }
        }
    }

    let (s, e) = (first?, last?);
    let cnt = e - s + 1;
    if cnt < 3 {
        return None;
    }
    Some(CollapseRange {
        start: s,
        end: e,
        line_count: cnt,
        name: None,
    })
}

fn detect_comment_lines(lines: &[&str], ext: &str) -> FxHashSet<u32> {
    let mut skipped = FxHashSet::default();
    if matches!(
        ext.to_ascii_lowercase().as_str(),
        ".py" | ".yml" | ".yaml" | ".toml" | ".sh" | ".bash" | ".dockerfile" | ".env" | ".cfg" | ".ini" | ""
    ) {
        let mut in_quote: Option<&str> = None;
        for (i, line) in lines.iter().enumerate() {
            let ln = i as u32 + 1;
            let trimmed = line.trim();
            if let Some(q) = in_quote {
                skipped.insert(ln);
                if trimmed.contains(q) {
                    in_quote = None;
                }
            } else if trimmed.starts_with('#') {
                skipped.insert(ln);
            } else {
                for q in &["\"\"\"", "'''"] {
                    if let Some(rest) = trimmed.strip_prefix(q) {
                        skipped.insert(ln);
                        if !rest.contains(q) {
                            in_quote = Some(q);
                        }
                        break;
                    }
                }
            }
        }
        return skipped;
    }

    let mut in_block = false;
    for (i, line) in lines.iter().enumerate() {
        let ln = i as u32 + 1;
        let trimmed = line.trim();
        if !in_block {
            if trimmed.starts_with("//") {
                skipped.insert(ln);
            } else if trimmed.starts_with("/*") {
                skipped.insert(ln);
                if !trimmed.contains("*/") {
                    in_block = true;
                }
            }
        } else {
            skipped.insert(ln);
            if trimmed.contains("*/") {
                in_block = false;
            }
        }
    }
    skipped
}

fn append_annotation(line: &str, count: usize) -> String {
    let ann = if count == 1 {
        "← 1 caller".to_string()
    } else {
        format!("← {count} callers")
    };
    let min_w = 60usize;
    let pad = if line.len() < min_w {
        " ".repeat(min_w - line.len())
    } else {
        "  ".to_string()
    };
    format!("{line}{pad}{ann}")
}

fn outline_mode(
    lines: &[&str],
    defs: &[ParsedDef],
    pad: usize,
    depth: u32,
    ext: &str,
    opts: OutlineOpts<'_>,
) -> String {
    let n = defs.len();
    let mut parents: Vec<Option<usize>> = vec![None; n];
    for i in 0..n {
        let mut parent: Option<usize> = None;
        for j in 0..n {
            if i == j {
                continue;
            }
            if defs[i].line_start <= defs[j].line_start || defs[i].line_end > defs[j].line_end {
                continue;
            }
            match parent {
                None => parent = Some(j),
                Some(p) => {
                    if defs[j].line_start >= defs[p].line_start
                        && defs[j].line_end <= defs[p].line_end
                    {
                        parent = Some(j);
                    }
                }
            }
        }
        parents[i] = parent;
    }

    let mut structural = FxHashSet::default();
    let mut deep_fn = FxHashSet::default();
    for i in 0..n {
        let Some(p) = parents[i] else { continue };
        let pk = SymbolKind::from_u8(defs[p].kind);
        let ck = SymbolKind::from_u8(defs[i].kind);
        let is_obj_router = pk == SymbolKind::Variable && ck == SymbolKind::Property;
        let is_class_member = matches!(pk, SymbolKind::Class | SymbolKind::Namespace)
            && matches!(
                ck,
                SymbolKind::Method
                    | SymbolKind::Function
                    | SymbolKind::Property
                    | SymbolKind::Class
            );
        let is_deep_fn = depth >= 2
            && matches!(
                pk,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Variable
            )
            && parents[p].is_none()
            && matches!(ck, SymbolKind::Function | SymbolKind::Variable);

        if is_obj_router || is_class_member {
            structural.insert(p);
        } else if is_deep_fn {
            deep_fn.insert(p);
        }
    }
    for &s in &structural {
        deep_fn.remove(&s);
    }

    let mut delegating = structural.clone();
    delegating.extend(deep_fn.iter().copied());

    let mut nested = vec![false; n];
    for i in 0..n {
        let mut anc = parents[i];
        while let Some(a) = anc {
            if !delegating.contains(&a) {
                nested[i] = true;
                break;
            }
            anc = parents[a];
        }
    }

    let mut collapse_ranges: Vec<CollapseRange> = Vec::new();
    let mut inline_ann: FxHashMap<u32, usize> = FxHashMap::default();

    if opts.collapse_imports {
        if let Some(imp) = detect_import_block(lines) {
            collapse_ranges.push(imp);
        }
    }

    for i in 0..n {
        if nested[i] {
            continue;
        }
        let d = &defs[i];
        if delegating.contains(&i) {
            if let Some(cc) = opts.caller_counts {
                if let Some(&cnt) = cc.get(&d.name) {
                    inline_ann.insert(d.line_start, cnt);
                }
            }
            continue;
        }
        let can_collapse = d.body_start_line > 0
            && d.body_start_line <= d.line_end
            && (d.line_end - d.body_start_line + 1) >= MIN_COLLAPSE_LINES;
        if !can_collapse {
            if let Some(cc) = opts.caller_counts {
                if let Some(&cnt) = cc.get(&d.name) {
                    inline_ann.insert(d.line_start, cnt);
                }
            }
            continue;
        }
        collapse_ranges.push(CollapseRange {
            start: d.body_start_line,
            end: d.line_end,
            line_count: d.line_end - d.body_start_line + 1,
            name: Some(d.name.clone()),
        });
    }

    // Collapse non-declaration gaps inside `--depth 2` expanded functions/components
    for &ci in &deep_fn {
        let c = &defs[ci];
        if c.body_start_line == 0 || c.body_start_line >= c.line_end {
            continue;
        }
        let mut children: Vec<&ParsedDef> = (0..n)
            .filter(|&i| !nested[i] && parents[i] == Some(ci))
            .map(|i| &defs[i])
            .collect();
        children.sort_by_key(|d| d.line_start);

        let mut cursor = c.body_start_line;
        let end_limit = c.line_end.saturating_sub(1);
        for ch in children {
            let gap_end = ch.line_start.saturating_sub(1);
            if gap_end >= cursor && (gap_end - cursor + 1) >= MIN_COLLAPSE_LINES {
                collapse_ranges.push(CollapseRange {
                    start: cursor,
                    end: gap_end,
                    line_count: gap_end - cursor + 1,
                    name: None,
                });
            }
            if ch.line_end + 1 > cursor {
                cursor = ch.line_end + 1;
            }
        }
        if end_limit >= cursor && (end_limit - cursor + 1) >= MIN_COLLAPSE_LINES {
            collapse_ranges.push(CollapseRange {
                start: cursor,
                end: end_limit,
                line_count: end_limit - cursor + 1,
                name: None,
            });
        }
    }

    collapse_ranges.sort_by_key(|r| r.start);
    let comment_lines = if opts.no_comments {
        Some(detect_comment_lines(lines, ext))
    } else {
        None
    };

    let slice_start = opts.line_range.map_or(1, |(s, _)| s.max(1));
    let slice_end = opts
        .line_range
        .map_or(lines.len() as u32, |(_, e)| e.min(lines.len() as u32));

    let mut out = Vec::new();
    let mut line_num = 1u32;
    let mut range_idx = 0usize;

    while line_num <= slice_end {
        while range_idx < collapse_ranges.len() && collapse_ranges[range_idx].start < line_num {
            range_idx += 1;
        }
        if range_idx < collapse_ranges.len() && collapse_ranges[range_idx].start == line_num {
            let r = &collapse_ranges[range_idx];
            if r.end >= slice_start && r.start <= slice_end {
                let body_line = lines.get((line_num - 1) as usize).copied().unwrap_or("");
                let indent_len = body_line
                    .bytes()
                    .take_while(|b| *b == b' ' || *b == b'\t')
                    .count();
                let indent = &body_line[..indent_len];
                let mut marker = format!(
                    "{}│ {}... ({} lines)",
                    " ".repeat(pad),
                    indent,
                    r.line_count
                );
                if let (Some(cc), Some(ref name)) = (opts.caller_counts, &r.name) {
                    if let Some(&cnt) = cc.get(name) {
                        marker = append_annotation(&marker, cnt);
                    }
                }
                out.push(marker);
            }
            line_num = r.end + 1;
            range_idx += 1;
            continue;
        }

        if line_num < slice_start {
            line_num += 1;
            continue;
        }
        if comment_lines
            .as_ref()
            .map_or(false, |cl| cl.contains(&line_num))
        {
            line_num += 1;
            continue;
        }

        let mut rendered = format!(
            "{:>pad$}│ {}",
            line_num,
            lines[(line_num - 1) as usize],
            pad = pad
        );
        if let Some(&cnt) = inline_ann.get(&line_num) {
            rendered = append_annotation(&rendered, cnt);
        }
        out.push(rendered);
        line_num += 1;
    }

    out.join("\n")
}
