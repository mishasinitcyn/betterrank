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
}

pub fn build_outline(
    source: &str,
    file_path: &str,
    expand_symbols: &[String],
    opts: OutlineOpts<'_>,
) -> String {
    let lines: Vec<&str> = source.split('\n').collect();
    let pad = lines.len().to_string().len().max(4);
    let depth = if opts.depth == 0 { 1 } else { opts.depth };
    let ext = file_path.rfind('.').map(|i| &file_path[i..]).unwrap_or("");

    // Fast path: if all expand targets are explicit line ranges (e.g. `outline schema.prisma 100-140,800-830`),
    // work cleanly on ANY file (including Prisma, YAML, JSON, Dockerfiles) without AST parsing!
    if !expand_symbols.is_empty() && expand_symbols.iter().all(|s| parse_line_range(s).is_some()) {
        return expand_mode(
            &lines,
            &[],
            file_path,
            expand_symbols,
            pad,
            opts.line_range,
            opts.no_comments,
            ext,
        );
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
        return raw_view(&lines, pad, opts.line_range);
    }

    if !is_supported_extension(file_path) {
        return raw_view(&lines, pad, opts.line_range);
    }

    let parsed = match parse_file(
        file_path,
        source,
        ParseOptions {
            include_outline_defs: true,
            include_local_refs: false,
            dedupe: false,
        },
    ) {
        Some(p) if !p.definitions.is_empty() => p,
        _ => return raw_view(&lines, pad, opts.line_range),
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
    for d in parsed.definitions {
        let replace = by_line
            .get(&d.line_start)
            .map_or(true, |ex| kind_priority(d.kind) > kind_priority(ex.kind));
        if replace {
            by_line.insert(d.line_start, d);
        }
    }
    let mut defs: Vec<ParsedDef> = by_line.into_values().collect();
    defs.sort_by_key(|d| d.line_start);

    if !expand_symbols.is_empty() {
        return expand_mode(
            &lines,
            &defs,
            file_path,
            expand_symbols,
            pad,
            opts.line_range,
            opts.no_comments,
            ext,
        );
    }

    // Default to 1-line-per-symbol compact skeleton (collapsing imports) so large files
    // never blow past the 20 KB shell buffer; use folded `outline_mode` when `--depth`,
    // `--lines`, or `--full` is explicitly requested.
    if !opts.explicit_depth && !opts.full_skeleton && opts.line_range.is_none() {
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
    let mut out = Vec::with_capacity(defs.len() + 6);
    out.push(format!("{file_path} ({} lines, {} symbols):", lines.len(), defs.len()));

    if !opts.show_imports {
        if let Some(imp) = detect_import_block(lines) {
            out.push(format!(
                "  L{}-{}  [imports] ({} lines)",
                imp.start, imp.end, imp.line_count
            ));
        }
    }

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

    for (i, def) in defs.iter().enumerate() {
        let d = depth_of[i];
        // Skip deeply nested inner object-literal keys (e.g. Mongo $match/$project stages inside functions)
        if d >= 2 {
            continue;
        }
        // Keep top-level symbols, class methods, and properties of top-level Variable routers/registries
        // (skip arbitrary object properties inside functions like `return { summary: ... }`)
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
        out.push(row);
    }

    out.join("\n")
}

fn raw_view(lines: &[&str], pad: usize, line_range: Option<(u32, u32)>) -> String {
    let start = line_range.map_or(1, |(s, _)| s.max(1)) as usize;
    let end = line_range
        .map_or(lines.len(), |(_, e)| (e as usize).min(lines.len()))
        .min(lines.len());
    let mut out = Vec::with_capacity(end.saturating_sub(start) + 1);
    for i in start..=end {
        out.push(format!("{:>pad$}│ {}", i, lines[i - 1], pad = pad));
    }
    out.join("\n")
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
    ext: &str,
) -> String {
    let comment_lines = if no_comments {
        detect_comment_lines(lines, ext)
    } else {
        FxHashSet::default()
    };
    let batch_mode = expand_symbols.len() > 1;
    let max_lines_per_target: usize = if batch_mode { 140 } else { usize::MAX };

    let mut out = Vec::new();
    for target in expand_symbols {
        // Support direct line-range slices like `3150-3220` or `L3150-3220`
        if let Some((rs, re)) = parse_line_range(target) {
            let start = rs.max(1) as usize;
            let end = (re as usize).min(lines.len());
            if batch_mode {
                out.push(format!("── {file_path}:{start}-{end} ──"));
            }
            let mut emitted = 0usize;
            for i in start..=end {
                if no_comments && comment_lines.contains(&(i as u32)) {
                    continue;
                }
                if emitted >= max_lines_per_target {
                    let rem = end.saturating_sub(i) + 1;
                    out.push(format!(
                        "  ... (+{rem} more lines — run: betterrank outline {file_path} {i}-{end})"
                    ));
                    break;
                }
                out.push(format!("{:>pad$}│ {}", i, lines[i - 1], pad = pad));
                emitted += 1;
            }
            if batch_mode {
                out.push(String::new());
            }
            continue;
        }

        let matches: Vec<&ParsedDef> = defs.iter().filter(|d| d.name == *target).collect();
        if matches.is_empty() {
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
            if matches.len() > 1 || batch_mode {
                out.push(format!(
                    "── {} ({}:{}-{}) ──",
                    def.name, file_path, def.line_start, def.line_end
                ));
            }
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
                if emitted >= max_lines_per_target {
                    let rem = end.saturating_sub(i) + 1;
                    out.push(format!(
                        "  ... (+{rem} more lines in {} — run: betterrank outline {file_path} {i}-{end})",
                        def.name
                    ));
                    break;
                }
                out.push(format!("{:>pad$}│ {}", i, lines[i - 1], pad = pad));
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
    if ext == ".py" {
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
