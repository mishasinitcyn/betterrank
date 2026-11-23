//! Single-pass iterative `TreeCursor` Python bulk indexer.
//!
//! Eliminates 3 separate `tree-sitter` `QueryCursor` VM passes, `strip_comments_and_strings`
//! byte-copying, O(vars * lines) scanning, and duplicate `String` allocations.

use crate::model::{ParsedDef, ParsedImport, ParsedRef, RefKind, SymbolKind};
use crate::parser::{
    create_definition_pub, extract_callee_leaf_name_pub, is_method_noise, is_stdlib_receiver, Lang,
    ParseOutput,
};
use rustc_hash::{FxHashMap, FxHashSet};
use tree_sitter::Node;

#[inline]
fn node_text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

#[inline]
fn is_upper_const(name: &str) -> bool {
    let b = name.as_bytes();
    if b.len() < 2 || !b[0].is_ascii_uppercase() {
        return false;
    }
    b[1..].iter().all(|&c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
}

fn process_py_import<'a>(
    node: Node,
    src: &'a [u8],
    imports: &mut Vec<ParsedImport>,
    seen_imports: &mut FxHashSet<(String, bool)>,
    symbol_aliases: &mut FxHashMap<&'a str, &'a str>,
    module_aliases: &mut FxHashMap<&'a str, String>,
    import_refs: &mut Vec<(&'a str, u32)>,
) {
    let kind = node.kind();
    if kind == "import_statement" {
        for i in 0..node.named_child_count() {
            let Some(ch) = node.named_child(i) else { continue };
            match ch.kind() {
                "dotted_name" => {
                    let txt = node_text(ch, src);
                    if !txt.is_empty() {
                        if seen_imports.insert((txt.to_string(), false)) {
                            imports.push(ParsedImport {
                                path: txt.to_string(),
                                is_reexport: false,
                            });
                        }
                        if let Some(leaf) = txt.rsplit('.').next() {
                            module_aliases.insert(leaf, txt.to_string());
                        }
                    }
                }
                "aliased_import" => {
                    let mod_node = ch.child_by_field_name("name").or_else(|| ch.named_child(0));
                    let alias_node = ch.child_by_field_name("alias").or_else(|| ch.named_child(1));
                    if let Some(mn) = mod_node {
                        if mn.kind() == "dotted_name" {
                            let txt = node_text(mn, src);
                            if !txt.is_empty() && seen_imports.insert((txt.to_string(), false)) {
                                imports.push(ParsedImport {
                                    path: txt.to_string(),
                                    is_reexport: false,
                                });
                            }
                            if let Some(an) = alias_node {
                                if an.kind() == "identifier" {
                                    module_aliases.insert(node_text(an, src), txt.to_string());
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    } else if kind == "import_from_statement" {
        let mod_node = node.child_by_field_name("module_name");
        let mod_path = mod_node.map(|n| node_text(n, src));
        if let Some(mp) = mod_path {
            if !mp.is_empty() && seen_imports.insert((mp.to_string(), false)) {
                imports.push(ParsedImport {
                    path: mp.to_string(),
                    is_reexport: false,
                });
            }
        }

        for i in 0..node.named_child_count() {
            let Some(ch) = node.named_child(i) else { continue };
            if mod_node.map(|m| m.id()) == Some(ch.id()) {
                continue;
            }
            let (orig_name, local_name) = match ch.kind() {
                "dotted_name" | "identifier" => {
                    let t = node_text(ch, src);
                    (Some(t), Some(t))
                }
                "aliased_import" => {
                    let o = ch
                        .child_by_field_name("name")
                        .or_else(|| ch.named_child(0))
                        .map(|n| node_text(n, src));
                    let a = ch
                        .child_by_field_name("alias")
                        .or_else(|| ch.named_child(1))
                        .map(|n| node_text(n, src));
                    (o, a)
                }
                _ => (None, None),
            };

            let Some(orig) = orig_name else { continue };
            if orig.is_empty() {
                continue;
            }

            if let Some(mp) = mod_path {
                let sub_path = if mp.bytes().all(|b| b == b'.') {
                    format!("{mp}{orig}")
                } else {
                    format!("{mp}.{orig}")
                };
                if seen_imports.insert((sub_path.clone(), false)) {
                    imports.push(ParsedImport {
                        path: sub_path.clone(),
                        is_reexport: false,
                    });
                }
                if let Some(loc) = local_name {
                    module_aliases.insert(loc, sub_path);
                }
            }

            import_refs.push((orig, ch.start_position().row as u32 + 1));

            if let Some(loc) = local_name {
                if loc != orig {
                    symbol_aliases.insert(loc, orig);
                }
            }
        }
    }
}

fn collect_class_body_defs<'a>(
    cls_node: Node,
    src: &'a [u8],
    seen_defs: &mut FxHashSet<(&'a str, u32)>,
    defs: &mut Vec<ParsedDef>,
) {
    let Some(body) = cls_node.child_by_field_name("body") else {
        return;
    };
    for i in 0..body.named_child_count() {
        let Some(stmt) = body.named_child(i) else { continue };
        match stmt.kind() {
            "function_definition" => {
                if let Some(nn) = stmt.child_by_field_name("name") {
                    let name = node_text(nn, src);
                    let row = stmt.start_position().row as u32;
                    if seen_defs.insert((name, row)) {
                        defs.push(create_definition_pub(
                            name.to_string(),
                            stmt,
                            Lang::Python,
                            src,
                            Some(SymbolKind::Method),
                            Some(!name.starts_with('_')),
                            None,
                        ));
                    }
                }
            }
            "class_definition" => {
                if let Some(nn) = stmt.child_by_field_name("name") {
                    let name = node_text(nn, src);
                    let row = stmt.start_position().row as u32;
                    if seen_defs.insert((name, row)) {
                        defs.push(create_definition_pub(
                            name.to_string(),
                            stmt,
                            Lang::Python,
                            src,
                            Some(SymbolKind::Class),
                            Some(!name.starts_with('_')),
                            None,
                        ));
                    }
                }
                collect_class_body_defs(stmt, src, seen_defs, defs);
            }
            "decorated_definition" => {
                if let Some(inner) = stmt.child_by_field_name("definition") {
                    if inner.kind() == "function_definition" {
                        if let Some(nn) = inner.child_by_field_name("name") {
                            let name = node_text(nn, src);
                            let row = inner.start_position().row as u32;
                            if seen_defs.insert((name, row)) {
                                defs.push(create_definition_pub(
                                    name.to_string(),
                                    inner,
                                    Lang::Python,
                                    src,
                                    Some(SymbolKind::Method),
                                    Some(!name.starts_with('_')),
                                    None,
                                ));
                            }
                        }
                    } else if inner.kind() == "class_definition" {
                        if let Some(nn) = inner.child_by_field_name("name") {
                            let name = node_text(nn, src);
                            let row = inner.start_position().row as u32;
                            if seen_defs.insert((name, row)) {
                                defs.push(create_definition_pub(
                                    name.to_string(),
                                    inner,
                                    Lang::Python,
                                    src,
                                    Some(SymbolKind::Class),
                                    Some(!name.starts_with('_')),
                                    None,
                                ));
                            }
                        }
                        collect_class_body_defs(inner, src, seen_defs, defs);
                    }
                }
            }
            _ => {}
        }
    }
}

pub fn parse_python_index(root: Node, src: &[u8]) -> ParseOutput {
    let mut definitions: Vec<ParsedDef> = Vec::with_capacity(16);
    let mut seen_defs: FxHashSet<(&str, u32)> = FxHashSet::default();
    let mut all_exports: FxHashSet<&str> = FxHashSet::default();
    let mut top_assigns: Vec<(&str, Node)> = Vec::new();

    let mut imports: Vec<ParsedImport> = Vec::new();
    let mut seen_imports: FxHashSet<(String, bool)> = FxHashSet::default();
    let mut symbol_aliases: FxHashMap<&str, &str> = FxHashMap::default();
    let mut module_aliases: FxHashMap<&str, String> = FxHashMap::default();
    let mut import_refs: Vec<(&str, u32)> = Vec::new();

    // Pass 1: fast O(top-level statements) scan for defs, __all__, constants, top imports
    for i in 0..root.named_child_count() {
        let Some(stmt) = root.named_child(i) else { continue };
        match stmt.kind() {
            "function_definition" => {
                if let Some(nn) = stmt.child_by_field_name("name") {
                    let name = node_text(nn, src);
                    let row = stmt.start_position().row as u32;
                    if seen_defs.insert((name, row)) {
                        definitions.push(create_definition_pub(
                            name.to_string(),
                            stmt,
                            Lang::Python,
                            src,
                            Some(SymbolKind::Function),
                            Some(!name.starts_with('_')),
                            None,
                        ));
                    }
                }
            }
            "class_definition" => {
                if let Some(nn) = stmt.child_by_field_name("name") {
                    let name = node_text(nn, src);
                    let row = stmt.start_position().row as u32;
                    if seen_defs.insert((name, row)) {
                        definitions.push(create_definition_pub(
                            name.to_string(),
                            stmt,
                            Lang::Python,
                            src,
                            Some(SymbolKind::Class),
                            Some(!name.starts_with('_')),
                            None,
                        ));
                    }
                }
                collect_class_body_defs(stmt, src, &mut seen_defs, &mut definitions);
            }
            "decorated_definition" => {
                if let Some(inner) = stmt.child_by_field_name("definition") {
                    if inner.kind() == "function_definition" {
                        if let Some(nn) = inner.child_by_field_name("name") {
                            let name = node_text(nn, src);
                            let row = inner.start_position().row as u32;
                            if seen_defs.insert((name, row)) {
                                definitions.push(create_definition_pub(
                                    name.to_string(),
                                    inner,
                                    Lang::Python,
                                    src,
                                    Some(SymbolKind::Function),
                                    Some(!name.starts_with('_')),
                                    None,
                                ));
                            }
                        }
                    } else if inner.kind() == "class_definition" {
                        if let Some(nn) = inner.child_by_field_name("name") {
                            let name = node_text(nn, src);
                            let row = inner.start_position().row as u32;
                            if seen_defs.insert((name, row)) {
                                definitions.push(create_definition_pub(
                                    name.to_string(),
                                    inner,
                                    Lang::Python,
                                    src,
                                    Some(SymbolKind::Class),
                                    Some(!name.starts_with('_')),
                                    None,
                                ));
                            }
                        }
                        collect_class_body_defs(inner, src, &mut seen_defs, &mut definitions);
                    }
                }
            }
            "expression_statement" => {
                if let Some(assign) = stmt.named_child(0) {
                    if assign.kind() == "assignment" {
                        if let Some(left) = assign.child_by_field_name("left") {
                            if left.kind() == "identifier" {
                                let name = node_text(left, src);
                                if name == "__all__" {
                                    if let Some(right) = assign.child_by_field_name("right") {
                                        if matches!(right.kind(), "list" | "tuple") {
                                            for k in 0..right.named_child_count() {
                                                if let Some(el) = right.named_child(k) {
                                                    if el.kind() == "string" {
                                                        let cleaned = node_text(el, src)
                                                            .trim_matches(|c| {
                                                                c == '\'' || c == '"'
                                                            });
                                                        if !cleaned.is_empty() {
                                                            all_exports.insert(cleaned);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                } else {
                                    top_assigns.push((name, assign));
                                }
                            }
                        }
                    }
                }
            }
            "import_statement" | "import_from_statement" => {
                process_py_import(
                    stmt,
                    src,
                    &mut imports,
                    &mut seen_imports,
                    &mut symbol_aliases,
                    &mut module_aliases,
                    &mut import_refs,
                );
            }
            _ => {}
        }
    }

    let mut var_defs: FxHashMap<&str, (u32, u32)> = FxHashMap::default();
    for (name, assign) in top_assigns {
        if !is_upper_const(name) && !all_exports.contains(name) {
            continue;
        }
        let d = create_definition_pub(
            name.to_string(),
            assign,
            Lang::Python,
            src,
            Some(SymbolKind::Variable),
            Some(true),
            None,
        );
        if name.len() >= 2 {
            var_defs.insert(name, (d.line_start, d.line_end));
        }
        definitions.push(d);
    }

    // Pass 2: single iterative TreeCursor walk over AST for all refs + nested imports
    let mut references: Vec<ParsedRef> = Vec::with_capacity(32);
    let mut seen_refs: FxHashSet<(String, u8, Option<String>, Option<String>)> =
        FxHashSet::default();
    let mut seen_var_refs: FxHashSet<&str> = FxHashSet::default();

    let mut push_ref = |name: &str,
                        kind: RefKind,
                        recv: Option<&str>,
                        recv_mod: Option<&str>,
                        line: u32| {
        if name.is_empty() {
            return;
        }
        if kind == RefKind::Method && (name.len() < 3 || is_method_noise(name)) {
            return;
        }
        if kind == RefKind::Property
            && recv_mod.is_none()
            && (name.len() < 2 || is_method_noise(name))
        {
            return;
        }
        let k = kind as u8;
        let key = (
            name.to_string(),
            k,
            recv.map(|s| s.to_string()),
            recv_mod.map(|s| s.to_string()),
        );
        if seen_refs.insert(key.clone()) {
            references.push(ParsedRef {
                name: key.0,
                kind: k,
                receiver: key.2,
                receiver_module: key.3,
                is_rpc_chain: false,
                line,
            });
        }
    };

    for (rname, line) in import_refs {
        push_ref(rname, RefKind::Symbol, None, None, line);
    }

    let mut cursor = root.walk();
    let mut reached_root = false;
    while !reached_root {
        let node = cursor.node();
        let kind = node.kind();
        let mut skip_children = false;

        match kind {
            "comment" | "string" | "string_content" => {
                skip_children = true;
            }
            "import_statement" | "import_from_statement" => {
                // Only process nested imports (top-level already processed)
                if node.parent().map(|p| p.id()) != Some(root.id()) {
                    let mut nested_refs = Vec::new();
                    process_py_import(
                        node,
                        src,
                        &mut imports,
                        &mut seen_imports,
                        &mut symbol_aliases,
                        &mut module_aliases,
                        &mut nested_refs,
                    );
                    for (rname, line) in nested_refs {
                        push_ref(rname, RefKind::Symbol, None, None, line);
                    }
                }
                skip_children = true;
            }
            "call" => {
                if let Some(func) = node.child_by_field_name("function") {
                    match func.kind() {
                        "identifier" => {
                            let raw = node_text(func, src);
                            let resolved = symbol_aliases.get(raw).copied().unwrap_or(raw);
                            push_ref(
                                resolved,
                                RefKind::Symbol,
                                None,
                                None,
                                func.start_position().row as u32 + 1,
                            );
                        }
                        "attribute" => {
                            if let Some(attr) = func.child_by_field_name("attribute") {
                                let mname = node_text(attr, src);
                                let recv = func
                                    .child_by_field_name("object")
                                    .and_then(|o| extract_callee_leaf_name_pub(o, src));
                                if !recv.map(is_stdlib_receiver).unwrap_or(false) {
                                    let recv_mod =
                                        recv.and_then(|r| module_aliases.get(r).map(|s| s.as_str()));
                                    push_ref(
                                        mname,
                                        RefKind::Method,
                                        recv,
                                        recv_mod,
                                        attr.start_position().row as u32 + 1,
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "attribute" => {
                if let Some(attr) = node.child_by_field_name("attribute") {
                    let pname = node_text(attr, src);
                    let recv = node
                        .child_by_field_name("object")
                        .and_then(|o| extract_callee_leaf_name_pub(o, src));
                    if !recv.map(is_stdlib_receiver).unwrap_or(false) {
                        let recv_mod =
                            recv.and_then(|r| module_aliases.get(r).map(|s| s.as_str()));
                        push_ref(
                            pname,
                            RefKind::Property,
                            recv,
                            recv_mod,
                            attr.start_position().row as u32 + 1,
                        );
                    }
                }
            }
            "decorator" => {
                if let Some(ch) = node.named_child(0) {
                    if ch.kind() == "identifier" {
                        let raw = node_text(ch, src);
                        let resolved = symbol_aliases.get(raw).copied().unwrap_or(raw);
                        push_ref(
                            resolved,
                            RefKind::Symbol,
                            None,
                            None,
                            ch.start_position().row as u32 + 1,
                        );
                    }
                }
            }
            "identifier" => {
                if !var_defs.is_empty() {
                    let name = node_text(node, src);
                    if let Some(&(ls, le)) = var_defs.get(name) {
                        if !seen_var_refs.contains(name) {
                            let line = node.start_position().row as u32 + 1;
                            if line < ls || line > le {
                                seen_var_refs.insert(name);
                                push_ref(name, RefKind::Symbol, None, None, line);
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        if !skip_children && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                reached_root = true;
                break;
            }
        }
    }

    ParseOutput {
        definitions,
        references,
        imports,
        local_info: Vec::new(),
    }
}
