use crate::model::{ParsedDef, ParsedImport, ParsedRef, RefKind, SymbolKind};
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::RefCell;
use std::sync::OnceLock;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Language, Node, Parser, Query, QueryCursor};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    JavaScript,
    TypeScript,
    Tsx,
    Python,
    Rust,
    Go,
}

impl Lang {
    pub fn from_path(path: &str) -> Option<Self> {
        let dot = path.rfind('.')?;
        match &path[dot..] {
            ".js" | ".mjs" | ".cjs" | ".jsx" => Some(Lang::JavaScript),
            ".ts" => Some(Lang::TypeScript),
            ".tsx" => Some(Lang::Tsx),
            ".py" => Some(Lang::Python),
            ".rs" => Some(Lang::Rust),
            ".go" => Some(Lang::Go),
            _ => None,
        }
    }

    pub fn is_js(self) -> bool {
        matches!(self, Lang::JavaScript | Lang::TypeScript | Lang::Tsx)
    }

    fn ts_language(self) -> Language {
        match self {
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Go => tree_sitter_go::LANGUAGE.into(),
        }
    }
}

pub const STDLIB_RECEIVERS: &[&str] = &[
    "Promise", "Object", "Array", "Math", "JSON", "Number", "String", "Boolean",
    "Date", "Map", "Set", "WeakMap", "WeakSet", "RegExp", "Symbol", "Reflect",
    "Proxy", "Error", "TypeError", "RangeError", "Buffer", "Uint8Array", "BigInt",
    "console", "process", "window", "document", "globalThis", "URL", "URLSearchParams",
    "Intl", "Response", "Request", "Headers", "FormData", "AbortController", "crypto",
    "vi", "jest", "expect", "assert", "describe", "it", "test", "pytest", "unittest",
    "dict", "list", "set", "tuple", "str", "int", "float", "bool", "bytes",
    "os", "sys", "re", "json", "math", "time", "datetime", "pathlib", "Path",
    "logging", "logger", "typing", "collections", "itertools", "functools",
    "asyncio", "subprocess", "shutil", "tempfile", "copy", "dataclasses", "enum",
    "hashlib", "hmac", "base64", "uuid", "urllib", "io", "csv", "struct", "random",
];

pub const METHOD_NOISE_NAMES: &[&str] = &[
    "constructor", "toString", "valueOf", "toJSON", "hasOwnProperty",
    "map", "filter", "reduce", "forEach", "flat", "flatMap", "some", "every",
    "find", "findIndex", "includes", "indexOf", "lastIndexOf", "push", "pop",
    "shift", "unshift", "slice", "splice", "concat", "join", "split", "trim",
    "trimStart", "trimEnd", "replace", "replaceAll", "match", "matchAll",
    "startsWith", "endsWith", "toLowerCase", "toUpperCase", "substring", "substr",
    "keys", "values", "entries", "from", "of", "isArray", "assign", "freeze",
    "then", "catch", "finally", "bind", "call", "apply", "resolve", "reject",
    "get", "set", "has", "add", "delete", "clear", "size", "length", "prototype",
    "log", "warn", "error", "info", "debug", "trace",
    "json", "text", "blob", "arrayBuffer", "clone", "headers", "status",
    "all", "first", "run", "exec", "test", "sort", "reverse", "fill",
    "emit", "on", "off", "once", "addEventListener", "removeEventListener",
    "preventDefault", "stopPropagation", "stringify", "parse", "append", "close",
    "open", "start", "stop", "init", "copy", "setdefault", "update", "items",
    "__init__", "__repr__", "__str__", "__eq__", "__hash__", "__len__",
    "__iter__", "__next__", "__enter__", "__exit__", "__getitem__", "__setitem__",
];

const RPC_TERMINAL_METHODS: &[&str] = &[
    "useQuery", "useMutation", "useInfiniteQuery", "useSuspenseQuery",
    "useSuspenseInfiniteQuery", "useQueries", "useUtils", "useSubscription",
    "invalidate", "fetch", "prefetch", "cancel", "getData", "setData",
    "getInfiniteData", "setInfiniteData", "ensureData", "query", "mutate",
    "subscribe",
];

const RPC_ROOT_RECEIVERS: &[&str] = &["api", "trpc", "rpc", "utils"];

pub fn is_stdlib_receiver(name: &str) -> bool {
    static SET: OnceLock<FxHashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| STDLIB_RECEIVERS.iter().copied().collect())
        .contains(name)
}

pub fn is_method_noise(name: &str) -> bool {
    static SET: OnceLock<FxHashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| METHOD_NOISE_NAMES.iter().copied().collect())
        .contains(name)
}

fn is_rpc_terminal(name: &str) -> bool {
    RPC_TERMINAL_METHODS.contains(&name)
}

struct CompiledLangQueries {
    def_query: Query,
    def_name_idx: u32,
    def_node_idx: u32,
    ref_query: Query,
    import_query: Option<Query>,
}

fn compile_lang_queries(lang: Lang) -> CompiledLangQueries {
    let ts_lang = lang.ts_language();
    let def_src = match lang {
        Lang::JavaScript => r#"
            (function_declaration name: (identifier) @name) @definition
            (lexical_declaration (variable_declarator name: (identifier) @name value: (arrow_function) @_val)) @definition
            (class_declaration name: (identifier) @name) @definition
            (method_definition name: (property_identifier) @name) @definition
            (export_statement declaration: (function_declaration name: (identifier) @name) @definition)
            (export_statement declaration: (class_declaration name: (identifier) @name) @definition)
            (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name)) @definition)
        "#,
        Lang::TypeScript | Lang::Tsx => r#"
            (function_declaration name: (identifier) @name) @definition
            (lexical_declaration (variable_declarator name: (identifier) @name value: (arrow_function) @_val)) @definition
            (class_declaration name: (type_identifier) @name) @definition
            (method_definition name: (property_identifier) @name) @definition
            (interface_declaration name: (type_identifier) @name) @definition
            (type_alias_declaration name: (type_identifier) @name) @definition
            (enum_declaration name: (identifier) @name) @definition
            (export_statement declaration: (function_declaration name: (identifier) @name) @definition)
            (export_statement declaration: (class_declaration name: (type_identifier) @name) @definition)
            (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name)) @definition)
            (export_statement declaration: (interface_declaration name: (type_identifier) @name) @definition)
            (export_statement declaration: (type_alias_declaration name: (type_identifier) @name) @definition)
            (export_statement declaration: (enum_declaration name: (identifier) @name) @definition)
        "#,
        Lang::Python => r#"
            (function_definition name: (identifier) @name) @definition
            (class_definition name: (identifier) @name) @definition
            (decorated_definition definition: (function_definition name: (identifier) @name) @definition)
            (decorated_definition definition: (class_definition name: (identifier) @name) @definition)
        "#,
        Lang::Rust => r#"
            (function_item name: (identifier) @name) @definition
            (struct_item name: (type_identifier) @name) @definition
            (enum_item name: (type_identifier) @name) @definition
            (trait_item name: (type_identifier) @name) @definition
            (impl_item trait: (type_identifier) @name) @definition
            (type_item name: (type_identifier) @name) @definition
        "#,
        Lang::Go => r#"
            (function_declaration name: (identifier) @name) @definition
            (method_declaration name: (field_identifier) @name) @definition
            (type_declaration (type_spec name: (type_identifier) @name)) @definition
        "#,
    };

    let ref_src = match lang {
        Lang::JavaScript => r#"
            (call_expression function: (identifier) @ref)
            (call_expression function: (member_expression object: (_) @method_recv property: (property_identifier) @method_ref))
            (import_specifier name: (identifier) @import_ref)
            (import_clause (identifier) @import_ref)
            (export_specifier name: (identifier) @import_ref)
            (member_expression object: (_) @prop_recv property: (property_identifier) @prop_ref)
            (jsx_opening_element (identifier) @jsx_ref)
            (jsx_self_closing_element (identifier) @jsx_ref)
            (jsx_opening_element (member_expression (identifier) @jsx_ref))
            (jsx_self_closing_element (member_expression (identifier) @jsx_ref))
        "#,
        Lang::TypeScript => r#"
            (call_expression function: (identifier) @ref)
            (call_expression function: (member_expression object: (_) @method_recv property: (property_identifier) @method_ref))
            (import_specifier name: (identifier) @import_ref)
            (import_clause (identifier) @import_ref)
            (export_specifier name: (identifier) @import_ref)
            (member_expression object: (_) @prop_recv property: (property_identifier) @prop_ref)
            (type_identifier) @ref
        "#,
        Lang::Tsx => r#"
            (call_expression function: (identifier) @ref)
            (call_expression function: (member_expression object: (_) @method_recv property: (property_identifier) @method_ref))
            (import_specifier name: (identifier) @import_ref)
            (import_clause (identifier) @import_ref)
            (export_specifier name: (identifier) @import_ref)
            (member_expression object: (_) @prop_recv property: (property_identifier) @prop_ref)
            (type_identifier) @ref
            (jsx_opening_element (identifier) @jsx_ref)
            (jsx_self_closing_element (identifier) @jsx_ref)
            (jsx_opening_element (member_expression (identifier) @jsx_ref))
            (jsx_self_closing_element (member_expression (identifier) @jsx_ref))
        "#,
        Lang::Python => r#"
            (call function: (identifier) @ref)
            (call function: (attribute object: (_) @method_recv attribute: (identifier) @method_ref))
            (attribute object: (_) @prop_recv attribute: (identifier) @prop_ref)
            (decorator (identifier) @ref)
        "#,
        Lang::Rust => r#"
            (call_expression function: (identifier) @ref)
            (call_expression function: (scoped_identifier name: (identifier) @ref))
            (type_identifier) @ref
        "#,
        Lang::Go => r#"
            (call_expression function: (identifier) @ref)
            (type_identifier) @ref
        "#,
    };

    let import_src = match lang {
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => Some(
            r#"
            (import_statement source: (string) @import_path)
            (export_statement source: (string) @reexport_path)
            (call_expression function: (identifier) @require_fn arguments: (arguments (string) @import_path))
        "#,
        ),
        Lang::Python => Some(
            r#"
            (import_statement) @py_import
            (import_from_statement) @py_import_from
        "#,
        ),
        _ => None,
    };

    let def_query = Query::new(&ts_lang, def_src).expect("valid def query");
    let def_name_idx = def_query.capture_index_for_name("name").unwrap();
    let def_node_idx = def_query.capture_index_for_name("definition").unwrap();
    let ref_query = Query::new(&ts_lang, ref_src).expect("valid ref query");
    let import_query = import_src.map(|s| Query::new(&ts_lang, s).expect("valid import query"));

    CompiledLangQueries {
        def_query,
        def_name_idx,
        def_node_idx,
        ref_query,
        import_query,
    }
}

fn queries_for(lang: Lang) -> &'static CompiledLangQueries {
    static JS: OnceLock<CompiledLangQueries> = OnceLock::new();
    static TS: OnceLock<CompiledLangQueries> = OnceLock::new();
    static TSX: OnceLock<CompiledLangQueries> = OnceLock::new();
    static PY: OnceLock<CompiledLangQueries> = OnceLock::new();
    static RS: OnceLock<CompiledLangQueries> = OnceLock::new();
    static GO: OnceLock<CompiledLangQueries> = OnceLock::new();

    match lang {
        Lang::JavaScript => JS.get_or_init(|| compile_lang_queries(Lang::JavaScript)),
        Lang::TypeScript => TS.get_or_init(|| compile_lang_queries(Lang::TypeScript)),
        Lang::Tsx => TSX.get_or_init(|| compile_lang_queries(Lang::Tsx)),
        Lang::Python => PY.get_or_init(|| compile_lang_queries(Lang::Python)),
        Lang::Rust => RS.get_or_init(|| compile_lang_queries(Lang::Rust)),
        Lang::Go => GO.get_or_init(|| compile_lang_queries(Lang::Go)),
    }
}

thread_local! {
    static TL_PARSERS: RefCell<FxHashMap<Lang, Parser>> = RefCell::new(FxHashMap::default());
    static TL_CURSOR: RefCell<QueryCursor> = RefCell::new(QueryCursor::new());
}

fn with_parser<T>(lang: Lang, f: impl FnOnce(&mut Parser) -> T) -> T {
    TL_PARSERS.with(|cell| {
        let mut map = cell.borrow_mut();
        let parser = map.entry(lang).or_insert_with(|| {
            let mut p = Parser::new();
            p.set_language(&lang.ts_language()).expect("set_language");
            p
        });
        f(parser)
    })
}

fn node_text<'a>(node: Node, src: &'a [u8]) -> &'a str {
    node.utf8_text(src).unwrap_or("")
}

fn is_function_ancestor_type(kind: &str) -> bool {
    matches!(
        kind,
        "function_declaration"
            | "function_expression"
            | "arrow_function"
            | "method_definition"
            | "method_declaration"
            | "function_definition"
            | "function_item"
            | "singleton_method"
    )
}

fn is_class_ancestor_type(kind: &str) -> bool {
    matches!(
        kind,
        "class_declaration" | "class_definition" | "class_specifier" | "impl_item" | "class"
    )
}

fn is_nested_inside_function(node: Node) -> bool {
    let mut cur = node.parent();
    while let Some(p) = cur {
        if is_function_ancestor_type(p.kind()) {
            return true;
        }
        cur = p.parent();
    }
    false
}

fn is_direct_class_method(node: Node) -> bool {
    let mut cur = node.parent();
    while let Some(p) = cur {
        if is_function_ancestor_type(p.kind()) {
            return false;
        }
        if is_class_ancestor_type(p.kind()) {
            return true;
        }
        cur = p.parent();
    }
    false
}

fn is_node_exported(node: Node, lang: Lang) -> bool {
    if !lang.is_js() {
        return true;
    }
    let mut cur = Some(node);
    while let Some(n) = cur {
        if n.kind() == "export_statement" {
            return true;
        }
        cur = n.parent();
    }
    false
}

fn is_literal_value_type(kind: &str) -> bool {
    matches!(
        kind,
        "string" | "template_string" | "number" | "true" | "false" | "null" | "undefined" | "regex"
    )
}

fn is_outline_body_type(kind: &str) -> bool {
    matches!(kind, "statement_block" | "class_body" | "object" | "array")
}

fn find_body_node<'a>(node: Node<'a>, depth: usize) -> Option<Node<'a>> {
    if depth > 8 {
        return None;
    }
    if is_outline_body_type(node.kind()) {
        return Some(node);
    }
    if let Some(body) = node.child_by_field_name("body") {
        return find_body_node(body, depth + 1).or(Some(body));
    }
    if matches!(node.kind(), "call_expression" | "new_expression") {
        if let Some(args) = node.child_by_field_name("arguments") {
            let mut fallback = None;
            for i in 0..args.named_child_count() {
                if let Some(ch) = args.named_child(i) {
                    if let Some(b) = find_body_node(ch, depth + 1) {
                        if matches!(b.kind(), "statement_block" | "class_body") {
                            return Some(b);
                        }
                        if fallback.is_none() {
                            fallback = Some(b);
                        }
                    }
                }
            }
            if fallback.is_some() {
                return fallback;
            }
        }
    }
    for field in &["value", "expression", "argument"] {
        if let Some(ch) = node.child_by_field_name(field) {
            if let Some(b) = find_body_node(ch, depth + 1) {
                return Some(b);
            }
        }
    }
    for i in 0..node.named_child_count() {
        if let Some(ch) = node.named_child(i) {
            if let Some(b) = find_body_node(ch, depth + 1) {
                return Some(b);
            }
        }
    }
    None
}

fn compute_body_start_line(node: Node, body_node: Option<Node>) -> u32 {
    let Some(b) = body_node else {
        return 0;
    };
    let body_row = b.start_position().row as u32;
    let def_row = node.start_position().row as u32;
    if body_row == def_row {
        body_row + 2
    } else {
        body_row + 1
    }
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(320));
    let mut prev_ws = false;
    for ch in s.trim().chars() {
        if ch == '\n' || ch == '\r' || ch == '\t' || ch == ' ' {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            out.push(ch);
            prev_ws = false;
        }
    }
    out
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars).collect();
        out.push_str("...");
        out
    }
}

fn extract_signature(node: Node, lang: Lang, src: &[u8], body_node: Option<Node>) -> String {
    let text = node_text(node, src);
    if lang == Lang::Python {
        if node.kind() == "assignment" {
            let first = text.lines().next().unwrap_or("").trim();
            return truncate_str(first, 200);
        }
        let colon_idx = if let Some(paren_close) = text.find(')') {
            text[paren_close + 1..].find(':').map(|i| paren_close + 1 + i)
        } else {
            text.find(':')
        };
        if let Some(idx) = colon_idx {
            let sig = collapse_ws(&text[..=idx]);
            return truncate_str(&sig, 300);
        }
        return text.lines().next().unwrap_or("").trim().to_string();
    }

    if let Some(b) = body_node {
        if matches!(b.kind(), "statement_block" | "class_body" | "block")
            && b.start_byte() > node.start_byte()
        {
            let rel_end = b.start_byte() - node.start_byte();
            if rel_end <= text.len() && text.is_char_boundary(rel_end) {
                let sig = collapse_ws(&text[..rel_end]);
                return truncate_str(&sig, 200);
            }
        }
    }

    let first_line = text.lines().next().unwrap_or("").trim();
    let cleaned = first_line.strip_suffix('{').unwrap_or(first_line).trim_end();
    truncate_str(cleaned, 200)
}

fn resolve_kind(node: Node, explicit: Option<SymbolKind>) -> SymbolKind {
    if let Some(k) = explicit {
        return k;
    }
    let kind = match node.kind() {
        "function_declaration" | "function_definition" | "arrow_function" | "function_item" => {
            SymbolKind::Function
        }
        "method_definition" | "method_declaration" | "singleton_method" => SymbolKind::Method,
        "class_declaration" | "class_definition" | "struct_item" | "struct_declaration" => {
            SymbolKind::Class
        }
        "interface_declaration"
        | "type_alias_declaration"
        | "type_item"
        | "enum_declaration"
        | "enum_item"
        | "trait_item"
        | "impl_item"
        | "type_declaration"
        | "type_spec" => SymbolKind::Type,
        "lexical_declaration" | "variable_declaration" | "variable_declarator" => {
            SymbolKind::Variable
        }
        "namespace_definition" => SymbolKind::Namespace,
        "decorated_definition" => {
            if let Some(inner) = node.child_by_field_name("definition") {
                if inner.kind() == "class_definition" {
                    return SymbolKind::Class;
                }
                if inner.kind() == "function_definition" && is_direct_class_method(node) {
                    return SymbolKind::Method;
                }
            }
            SymbolKind::Function
        }
        "pair" => SymbolKind::Property,
        _ => SymbolKind::Other,
    };
    if kind == SymbolKind::Function && is_direct_class_method(node) {
        return SymbolKind::Method;
    }
    kind
}

fn create_definition(
    name: String,
    node: Node,
    lang: Lang,
    src: &[u8],
    explicit_kind: Option<SymbolKind>,
    explicit_exported: Option<bool>,
    explicit_body_start: Option<u32>,
) -> ParsedDef {
    let body_node = find_body_node(node, 0);
    let body_start_line =
        explicit_body_start.unwrap_or_else(|| compute_body_start_line(node, body_node));
    let exported = explicit_exported.unwrap_or_else(|| is_node_exported(node, lang));
    ParsedDef {
        name,
        kind: resolve_kind(node, explicit_kind) as u8,
        exported,
        line_start: node.start_position().row as u32 + 1,
        line_end: node.end_position().row as u32 + 1,
        signature: extract_signature(node, lang, src, body_node),
        body_start_line,
    }
}

fn extract_callee_leaf_name<'a>(node: Node<'a>, src: &'a [u8]) -> Option<&'a str> {
    match node.kind() {
        "identifier"
        | "property_identifier"
        | "private_property_identifier"
        | "type_identifier" => Some(node_text(node, src)),
        "member_expression" | "attribute" => {
            let prop = node
                .child_by_field_name("property")
                .or_else(|| node.child_by_field_name("attribute"))
                .or_else(|| {
                    let c = node.named_child_count();
                    if c > 0 { node.named_child(c - 1) } else { None }
                })?;
            extract_callee_leaf_name(prop, src)
        }
        _ => None,
    }
}

fn extract_pair_name(node: Node, src: &[u8]) -> Option<String> {
    let key = node
        .child_by_field_name("key")
        .or_else(|| node.named_child(0))?;
    match key.kind() {
        "property_identifier" | "identifier" | "private_property_identifier" | "number" => {
            Some(node_text(key, src).to_string())
        }
        "string" => {
            let cleaned = node_text(key, src)
                .trim_matches(|c| c == '\'' || c == '"' || c == '`');
            if cleaned.is_empty() || cleaned.starts_with('$') {
                None
            } else {
                Some(cleaned.to_string())
            }
        }
        _ => None,
    }
}

fn should_index_semantic_object_members(decl: Node, is_exported: bool, src: &[u8]) -> bool {
    static SEMANTIC_RE: OnceLock<Regex> = OnceLock::new();
    static NON_SEMANTIC_VAR_RE: OnceLock<Regex> = OnceLock::new();
    static NON_SEMANTIC_CALLEE_RE: OnceLock<Regex> = OnceLock::new();

    let semantic_re = SEMANTIC_RE.get_or_init(|| {
        Regex::new(r"(?i)(router|routes|handlers|actions|reducers|selectors|queries|mutations|registry|registries|map|maps|config|configs|options|endpoints|procedures)$").unwrap()
    });
    let non_semantic_var_re = NON_SEMANTIC_VAR_RE.get_or_init(|| {
        Regex::new(r"(?i)(schema|shape|validator|payload|params?|props?|input|output|response|request|dto|mock|fixture|stub|fake|test)$").unwrap()
    });
    let non_semantic_callee_re = NON_SEMANTIC_CALLEE_RE.get_or_init(|| {
        Regex::new(r"(?i)^(object|array|enum|union|literal|record|tuple|pick|omit|extend|merge|intersection|partial|strictObject|looseObject|createMock.*|mock.*|vi|jest)$").unwrap()
    });

    let var_name = decl
        .child_by_field_name("name")
        .filter(|n| n.kind() == "identifier")
        .map(|n| node_text(n, src));
    if let Some(vn) = var_name {
        if non_semantic_var_re.is_match(vn) {
            return false;
        }
    }

    let value_node = match decl.child_by_field_name("value") {
        Some(v) => v,
        None => return false,
    };
    let callee_name = if matches!(value_node.kind(), "call_expression" | "new_expression") {
        value_node
            .child_by_field_name("function")
            .or_else(|| value_node.child_by_field_name("constructor"))
            .or_else(|| value_node.named_child(0))
            .and_then(|c| extract_callee_leaf_name(c, src))
    } else {
        None
    };

    let has_semantic_var = var_name.map(|v| semantic_re.is_match(v)).unwrap_or(false);
    let has_semantic_callee = callee_name
        .map(|c| !non_semantic_callee_re.is_match(c))
        .unwrap_or(false);

    if !is_exported {
        return value_node.kind() == "object" && has_semantic_var;
    }
    if value_node.kind() == "object" {
        return has_semantic_var;
    }
    if matches!(value_node.kind(), "call_expression" | "new_expression") {
        return has_semantic_var || has_semantic_callee;
    }
    false
}

fn collect_index_definitions(root: Node, lang: Lang, src: &[u8]) -> Vec<ParsedDef> {
    if !lang.is_js() {
        return Vec::new();
    }
    let mut defs = Vec::new();
    for i in 0..root.named_child_count() {
        let Some(mut stmt) = root.named_child(i) else {
            continue;
        };
        let mut is_exported = false;
        if stmt.kind() == "export_statement" {
            is_exported = true;
            let Some(decl) = stmt.child_by_field_name("declaration") else {
                continue;
            };
            stmt = decl;
        }
        if !matches!(stmt.kind(), "lexical_declaration" | "variable_declaration") {
            continue;
        }
        for j in 0..stmt.named_child_count() {
            let Some(decl) = stmt.named_child(j) else {
                continue;
            };
            if decl.kind() != "variable_declarator" {
                continue;
            }
            if !should_index_semantic_object_members(decl, is_exported, src) {
                continue;
            }
            if let Some(body) = find_body_node(decl, 0) {
                if body.kind() == "object" {
                    for k in 0..body.named_child_count() {
                        let Some(pair) = body.named_child(k) else {
                            continue;
                        };
                        if pair.kind() != "pair" || pair.end_position().row <= pair.start_position().row
                        {
                            continue;
                        }
                        if let Some(val) = pair.child_by_field_name("value") {
                            if is_literal_value_type(val.kind()) {
                                continue;
                            }
                        }
                        if let Some(name) = extract_pair_name(pair, src) {
                            defs.push(create_definition(
                                name,
                                pair,
                                lang,
                                src,
                                Some(SymbolKind::Property),
                                Some(is_exported),
                                Some(pair.start_position().row as u32 + 2),
                            ));
                        }
                    }
                }
            }
        }
    }
    defs
}

fn collect_python_top_level_constants(root: Node, src: &[u8]) -> Vec<ParsedDef> {
    static PY_CONST_RE: OnceLock<Regex> = OnceLock::new();
    let py_const_re = PY_CONST_RE.get_or_init(|| Regex::new(r"^[A-Z][A-Z0-9_]+$").unwrap());

    let mut all_exports: FxHashSet<String> = FxHashSet::default();
    let mut top_assigns: Vec<(String, Node)> = Vec::new();

    for i in 0..root.named_child_count() {
        let Some(stmt) = root.named_child(i) else {
            continue;
        };
        if stmt.kind() != "expression_statement" {
            continue;
        }
        let Some(assign) = stmt.named_child(0) else {
            continue;
        };
        if assign.kind() != "assignment" {
            continue;
        }
        let Some(left) = assign.child_by_field_name("left") else {
            continue;
        };
        if left.kind() != "identifier" {
            continue;
        }
        let name = node_text(left, src);
        if name == "__all__" {
            if let Some(right) = assign.child_by_field_name("right") {
                if matches!(right.kind(), "list" | "tuple") {
                    for k in 0..right.named_child_count() {
                        if let Some(el) = right.named_child(k) {
                            if el.kind() == "string" {
                                let cleaned = node_text(el, src)
                                    .trim_matches(|c| c == '\'' || c == '"');
                                if !cleaned.is_empty() {
                                    all_exports.insert(cleaned.to_string());
                                }
                            }
                        }
                    }
                }
            }
            continue;
        }
        top_assigns.push((name.to_string(), assign));
    }

    let mut defs = Vec::new();
    for (name, node) in top_assigns {
        if !py_const_re.is_match(&name) && !all_exports.contains(&name) {
            continue;
        }
        defs.push(create_definition(
            name,
            node,
            Lang::Python,
            src,
            Some(SymbolKind::Variable),
            Some(true),
            None,
        ));
    }
    defs
}

fn collect_js_bottom_exports(root: Node, src: &[u8]) -> FxHashSet<String> {
    let mut exported = FxHashSet::default();
    for i in 0..root.named_child_count() {
        let Some(stmt) = root.named_child(i) else {
            continue;
        };
        if stmt.kind() == "export_statement" {
            if stmt.child_by_field_name("source").is_some() {
                continue;
            }
            for j in 0..stmt.named_child_count() {
                let Some(ch) = stmt.named_child(j) else {
                    continue;
                };
                if ch.kind() == "export_clause" {
                    for k in 0..ch.named_child_count() {
                        if let Some(spec) = ch.named_child(k) {
                            if spec.kind() == "export_specifier" {
                                if let Some(n) =
                                    spec.child_by_field_name("name").or_else(|| spec.named_child(0))
                                {
                                    exported.insert(node_text(n, src).to_string());
                                }
                            }
                        }
                    }
                } else if ch.kind() == "identifier" {
                    exported.insert(node_text(ch, src).to_string());
                }
            }
        } else if stmt.kind() == "expression_statement" {
            if let Some(expr) = stmt.named_child(0) {
                if expr.kind() == "assignment_expression" {
                    let left = expr.child_by_field_name("left").map(|n| node_text(n, src));
                    if let Some(l) = left {
                        if l == "module.exports" || l.starts_with("exports.") {
                            if let Some(right) = expr.child_by_field_name("right") {
                                if right.kind() == "identifier" {
                                    exported.insert(node_text(right, src).to_string());
                                } else if right.kind() == "object" {
                                    for k in 0..right.named_child_count() {
                                        if let Some(p) = right.named_child(k) {
                                            if p.kind() == "shorthand_property_identifier" {
                                                exported.insert(node_text(p, src).to_string());
                                            } else if p.kind() == "pair" {
                                                if let Some(v) = p.child_by_field_name("value") {
                                                    if v.kind() == "identifier" {
                                                        exported
                                                            .insert(node_text(v, src).to_string());
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    exported
}

fn collect_js_import_info(
    root: Node,
    source: &str,
    src: &[u8],
) -> (FxHashMap<String, String>, FxHashMap<String, String>) {
    let mut symbol_aliases = FxHashMap::default();
    let mut module_aliases = FxHashMap::default();
    let mut imported_sym_sources: FxHashMap<String, String> = FxHashMap::default();

    for i in 0..root.named_child_count() {
        let Some(stmt) = root.named_child(i) else {
            continue;
        };
        if stmt.kind() != "import_statement" {
            continue;
        }
        let spec_path = stmt.child_by_field_name("source").map(|n| {
            node_text(n, src)
                .trim_matches(|c| c == '\'' || c == '"' || c == '`')
                .to_string()
        });
        for j in 0..stmt.named_child_count() {
            let Some(clause) = stmt.named_child(j) else {
                continue;
            };
            if clause.kind() != "import_clause" {
                continue;
            }
            for k in 0..clause.named_child_count() {
                let Some(ch) = clause.named_child(k) else {
                    continue;
                };
                if ch.kind() == "identifier" {
                    if let Some(ref sp) = spec_path {
                        imported_sym_sources.insert(node_text(ch, src).to_string(), sp.clone());
                    }
                } else if ch.kind() == "namespace_import" {
                    if let (Some(id), Some(ref sp)) = (ch.named_child(0), &spec_path) {
                        if id.kind() == "identifier" {
                            module_aliases.insert(node_text(id, src).to_string(), sp.clone());
                        }
                    }
                } else if ch.kind() == "named_imports" {
                    for m in 0..ch.named_child_count() {
                        let Some(spec) = ch.named_child(m) else {
                            continue;
                        };
                        if spec.kind() != "import_specifier" {
                            continue;
                        }
                        let orig = spec.child_by_field_name("name").map(|n| node_text(n, src));
                        let alias = spec.child_by_field_name("alias").map(|n| node_text(n, src));
                        match (orig, alias) {
                            (Some(o), Some(a)) if o != a => {
                                symbol_aliases.insert(a.to_string(), o.to_string());
                                if let Some(ref sp) = spec_path {
                                    imported_sym_sources.insert(a.to_string(), sp.clone());
                                }
                            }
                            (Some(o), _) => {
                                if let Some(ref sp) = spec_path {
                                    imported_sym_sources.insert(o.to_string(), sp.clone());
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    // Fast pre-checked tRPC caller binding tracker (only runs when `createCaller` appears)
    if !imported_sym_sources.is_empty() && source.contains("createCaller") {
        static DIRECT_RE: OnceLock<Regex> = OnceLock::new();
        static CALL_RE: OnceLock<Regex> = OnceLock::new();
        let direct_re = DIRECT_RE.get_or_init(|| {
            Regex::new(r"\b(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*=\s*(?:createCallerFactory\s*\(\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*\)|([A-Za-z_$][A-Za-z0-9_$]*)\s*\.\s*createCaller\s*\()").unwrap()
        });
        let call_re = CALL_RE.get_or_init(|| {
            Regex::new(r"\b(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*=\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*\(").unwrap()
        });

        let mut factory_to_mod: FxHashMap<String, String> = FxHashMap::default();
        for caps in direct_re.captures_iter(source) {
            let var_name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let is_factory_expr = caps.get(2).is_some();
            let router_name = caps
                .get(2)
                .or_else(|| caps.get(3))
                .map(|m| m.as_str())
                .unwrap_or("");
            if let Some(mod_spec) = imported_sym_sources.get(router_name) {
                let end_idx = caps.get(0).map(|m| m.end()).unwrap_or(0);
                let after = source[end_idx..source.len().min(end_idx + 40)].trim_start();
                if is_factory_expr && !after.starts_with('(') {
                    factory_to_mod.insert(var_name.to_string(), mod_spec.clone());
                } else {
                    module_aliases.insert(var_name.to_string(), mod_spec.clone());
                }
            }
        }
        if !factory_to_mod.is_empty() {
            for caps in call_re.captures_iter(source) {
                let var_name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                let fn_name = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                if let Some(mod_spec) = factory_to_mod.get(fn_name) {
                    module_aliases.insert(var_name.to_string(), mod_spec.clone());
                }
            }
        }
    }

    (symbol_aliases, module_aliases)
}

fn is_rpc_chain_prop(prop_node: Node, src: &[u8]) -> bool {
    let Some(member) = prop_node.parent() else {
        return false;
    };
    if member.kind() != "member_expression" {
        return false;
    }
    if let Some(grand) = member.parent() {
        if grand.kind() == "member_expression"
            && grand.child_by_field_name("object").map(|n| n.id()) == Some(member.id())
        {
            if let Some(next_prop) = grand.child_by_field_name("property") {
                if is_rpc_terminal(node_text(next_prop, src)) {
                    return true;
                }
            }
        }
    }
    if let Some(obj) = member.child_by_field_name("object") {
        if obj.kind() == "member_expression" {
            let mut cur = obj;
            let mut hops = 0;
            while cur.kind() == "member_expression" && hops < 6 {
                match cur.child_by_field_name("object") {
                    Some(o) => cur = o,
                    None => break,
                }
                hops += 1;
            }
            if cur.kind() == "identifier" && RPC_ROOT_RECEIVERS.contains(&node_text(cur, src)) {
                return true;
            }
        }
    }
    false
}

fn collect_python_imports_and_aliases(
    root: Node,
    src: &[u8],
    import_query: &Query,
) -> (
    Vec<ParsedImport>,
    Vec<ParsedRef>,
    FxHashMap<String, String>,
    FxHashMap<String, String>,
) {
    let mut imports = Vec::new();
    let mut import_refs = Vec::new();
    let mut symbol_aliases = FxHashMap::default();
    let mut module_aliases = FxHashMap::default();

    let py_import_idx = import_query.capture_index_for_name("py_import");
    let py_import_from_idx = import_query.capture_index_for_name("py_import_from");

    TL_CURSOR.with(|cell| {
        let mut cursor = cell.borrow_mut();
        let mut matches = cursor.matches(import_query, root, src);
        while let Some(m) = matches.next() {
            let Some(cap) = m.captures.first() else {
                continue;
            };
            let node = cap.node;
            if Some(cap.index) == py_import_idx {
                for i in 0..node.named_child_count() {
                    let Some(ch) = node.named_child(i) else {
                        continue;
                    };
                    if ch.kind() == "dotted_name" {
                        let txt = node_text(ch, src);
                        if !txt.is_empty() {
                            imports.push(ParsedImport {
                                path: txt.to_string(),
                                is_reexport: false,
                            });
                            if let Some(leaf) = txt.rsplit('.').next() {
                                module_aliases.insert(leaf.to_string(), txt.to_string());
                            }
                        }
                    } else if ch.kind() == "aliased_import" {
                        let mod_node = ch.child_by_field_name("name").or_else(|| ch.named_child(0));
                        let alias_node =
                            ch.child_by_field_name("alias").or_else(|| ch.named_child(1));
                        if let Some(mn) = mod_node {
                            if mn.kind() == "dotted_name" {
                                let txt = node_text(mn, src);
                                imports.push(ParsedImport {
                                    path: txt.to_string(),
                                    is_reexport: false,
                                });
                                if let Some(an) = alias_node {
                                    if an.kind() == "identifier" {
                                        module_aliases.insert(
                                            node_text(an, src).to_string(),
                                            txt.to_string(),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            } else if Some(cap.index) == py_import_from_idx {
                let mod_node = node.child_by_field_name("module_name");
                let mod_path = mod_node.map(|n| node_text(n, src).to_string());
                if let Some(ref mp) = mod_path {
                    if !mp.is_empty() {
                        imports.push(ParsedImport {
                            path: mp.clone(),
                            is_reexport: false,
                        });
                    }
                }

                for i in 0..node.named_child_count() {
                    let Some(ch) = node.named_child(i) else {
                        continue;
                    };
                    if mod_node.map(|m| m.id()) == Some(ch.id()) {
                        continue;
                    }
                    let (orig_name, local_name) =
                        if matches!(ch.kind(), "dotted_name" | "identifier") {
                            let t = node_text(ch, src).to_string();
                            (Some(t.clone()), Some(t))
                        } else if ch.kind() == "aliased_import" {
                            let o = ch
                                .child_by_field_name("name")
                                .or_else(|| ch.named_child(0))
                                .map(|n| node_text(n, src).to_string());
                            let a = ch
                                .child_by_field_name("alias")
                                .or_else(|| ch.named_child(1))
                                .map(|n| node_text(n, src).to_string());
                            (o, a)
                        } else {
                            (None, None)
                        };

                    let Some(orig) = orig_name else { continue };
                    if orig.is_empty() {
                        continue;
                    }

                    if let Some(ref mp) = mod_path {
                        let sub_path = if mp.bytes().all(|b| b == b'.') {
                            format!("{mp}{orig}")
                        } else {
                            format!("{mp}.{orig}")
                        };
                        imports.push(ParsedImport {
                            path: sub_path.clone(),
                            is_reexport: false,
                        });
                        if let Some(ref loc) = local_name {
                            module_aliases.insert(loc.clone(), sub_path);
                        }
                    }

                    import_refs.push(ParsedRef {
                        name: orig.clone(),
                        kind: RefKind::Symbol as u8,
                        receiver: None,
                        receiver_module: None,
                        is_rpc_chain: false,
                        line: ch.start_position().row as u32 + 1,
                    });

                    if let Some(loc) = local_name {
                        if loc != orig {
                            symbol_aliases.insert(loc, orig);
                        }
                    }
                }
            }
        }
    });

    (imports, import_refs, symbol_aliases, module_aliases)
}

fn strip_comments_and_strings(source: &str, is_python: bool) -> String {
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;

    while i < len {
        let ch = bytes[i];
        let next = if i + 1 < len { bytes[i + 1] } else { 0 };

        if is_python {
            if ch == b'#' {
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if (ch == b'"' || ch == b'\'')
                && i + 2 < len
                && bytes[i + 1] == ch
                && bytes[i + 2] == ch
            {
                let q = ch;
                i += 3;
                while i < len {
                    if bytes[i] == b'\\' {
                        i = (i + 2).min(len);
                        continue;
                    }
                    if bytes[i] == b'\n' {
                        out.push('\n');
                    }
                    if i + 2 < len && bytes[i] == q && bytes[i + 1] == q && bytes[i + 2] == q {
                        i += 3;
                        break;
                    }
                    i += 1;
                }
                out.push(' ');
                continue;
            }
            if ch == b'"' || ch == b'\'' {
                let q = ch;
                i += 1;
                while i < len && bytes[i] != b'\n' {
                    if bytes[i] == b'\\' {
                        i = (i + 2).min(len);
                        continue;
                    }
                    if bytes[i] == q {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                out.push(' ');
                continue;
            }
            out.push(if ch.is_ascii() { ch as char } else { ' ' });
            i += 1;
            continue;
        }

        // JS/TS
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
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i = (i + 2).min(len);
            out.push(' ');
            continue;
        }
        if ch == b'"' || ch == b'\'' || ch == b'`' {
            let q = ch;
            i += 1;
            while i < len {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(len);
                    continue;
                }
                if bytes[i] == b'\n' {
                    out.push('\n');
                }
                if bytes[i] == q {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push(' ');
            continue;
        }
        out.push(if ch.is_ascii() { ch as char } else { ' ' });
        i += 1;
    }
    out
}

fn contains_ident(line: &str, name: &str) -> bool {
    let bytes = line.as_bytes();
    let nlen = name.len();
    let mut start = 0;
    while let Some(pos) = line[start..].find(name) {
        let abs = start + pos;
        let before_ok = abs == 0 || !is_ident_byte(bytes[abs - 1]);
        let after_ok = abs + nlen >= bytes.len() || !is_ident_byte(bytes[abs + nlen]);
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

fn collect_same_file_variable_refs(
    source: &str,
    defs: &[ParsedDef],
    is_python: bool,
) -> Vec<ParsedRef> {
    let var_defs: Vec<&ParsedDef> = defs
        .iter()
        .filter(|d| d.kind == SymbolKind::Variable as u8 && d.name.len() >= 2)
        .collect();
    if var_defs.is_empty() {
        return Vec::new();
    }

    let stripped = strip_comments_and_strings(source, is_python);
    let lines: Vec<&str> = stripped.split('\n').collect();
    let mut refs = Vec::new();

    for def in var_defs {
        for (i, line) in lines.iter().enumerate() {
            let line_num = i as u32 + 1;
            if line_num >= def.line_start && line_num <= def.line_end {
                continue;
            }
            if !line.contains(&def.name) {
                continue;
            }
            if !is_python && line.trim_start().starts_with("export") && line.contains('{') {
                continue;
            }
            if contains_ident(line, &def.name) {
                refs.push(ParsedRef {
                    name: def.name.clone(),
                    kind: RefKind::Symbol as u8,
                    receiver: None,
                    receiver_module: None,
                    is_rpc_chain: false,
                    line: line_num,
                });
                break;
            }
        }
    }
    refs
}

fn has_multiline_pair_children(node: Node, src: &[u8]) -> bool {
    if node.kind() != "object" {
        return false;
    }
    for i in 0..node.named_child_count() {
        if let Some(ch) = node.named_child(i) {
            if ch.kind() == "pair"
                && extract_pair_name(ch, src).is_some()
                && ch.end_position().row > ch.start_position().row
            {
                return true;
            }
        }
    }
    false
}

fn collect_outline_definitions(root: Node, lang: Lang, src: &[u8]) -> Vec<ParsedDef> {
    if !lang.is_js() {
        return Vec::new();
    }
    let mut defs = Vec::new();
    fn visit(node: Node, lang: Lang, src: &[u8], defs: &mut Vec<ParsedDef>) {
        if node.kind() == "pair" && node.end_position().row > node.start_position().row {
            if let Some(name) = extract_pair_name(node, src) {
                defs.push(create_definition(
                    name,
                    node,
                    lang,
                    src,
                    Some(SymbolKind::Property),
                    None,
                    Some(node.start_position().row as u32 + 2),
                ));
            }
        } else if node.kind() == "variable_declarator"
            && node.end_position().row > node.start_position().row
        {
            if let Some(name_node) = node.child_by_field_name("name") {
                if name_node.kind() == "identifier" {
                    let body = find_body_node(node, 0);
                    let skip = body
                        .map(|b| b.kind() == "object" && has_multiline_pair_children(b, src))
                        .unwrap_or(false);
                    if !skip {
                        defs.push(create_definition(
                            node_text(name_node, src).to_string(),
                            node,
                            lang,
                            src,
                            Some(SymbolKind::Variable),
                            None,
                            Some(node.start_position().row as u32 + 2),
                        ));
                    }
                }
            }
        }
        for i in 0..node.named_child_count() {
            if let Some(ch) = node.named_child(i) {
                visit(ch, lang, src, defs);
            }
        }
    }
    visit(root, lang, src, &mut defs);
    defs
}

#[derive(Debug, Default)]
pub struct LocalAstInfo {
    pub local_refs: FxHashSet<String>,
    pub local_bindings: FxHashSet<String>,
}

fn collect_local_ast_info(def_node: Node, src: &[u8]) -> LocalAstInfo {
    let mut info = LocalAstInfo::default();

    fn walk(node: Node, is_root: bool, src: &[u8], info: &mut LocalAstInfo) {
        if matches!(
            node.kind(),
            "string"
                | "string_content"
                | "string_fragment"
                | "template_string"
                | "comment"
                | "regex"
                | "number"
        ) {
            return;
        }

        if !is_root {
            match node.kind() {
                "variable_declarator" => {
                    if let Some(n) = node.child_by_field_name("name") {
                        if n.kind() == "identifier" {
                            info.local_bindings.insert(node_text(n, src).to_string());
                        }
                    }
                }
                "assignment" => {
                    if let Some(l) = node.child_by_field_name("left") {
                        if l.kind() == "identifier" {
                            info.local_bindings.insert(node_text(l, src).to_string());
                        }
                    }
                }
                "required_parameter" | "optional_parameter" => {
                    if let Some(p) = node.child_by_field_name("pattern") {
                        if p.kind() == "identifier" {
                            info.local_bindings.insert(node_text(p, src).to_string());
                        }
                    }
                }
                "parameters" | "formal_parameters" => {
                    for i in 0..node.named_child_count() {
                        if let Some(ch) = node.named_child(i) {
                            if ch.kind() == "identifier" {
                                info.local_bindings.insert(node_text(ch, src).to_string());
                            }
                        }
                    }
                }
                "identifier" | "type_identifier" => {
                    let is_decl_key = node.parent().map_or(false, |p| {
                        (p.kind() == "pair"
                            && p.child_by_field_name("key").map(|k| k.id()) == Some(node.id()))
                            || (p.kind() == "property_signature"
                                && p.child_by_field_name("name").map(|k| k.id()) == Some(node.id()))
                    });
                    if !is_decl_key {
                        info.local_refs.insert(node_text(node, src).to_string());
                    }
                }
                "property_identifier" => {
                    let name = node_text(node, src);
                    if !is_method_noise(name) {
                        if let Some(p) = node.parent() {
                            if p.kind() == "member_expression" {
                                let recv = p
                                    .child_by_field_name("object")
                                    .and_then(|o| extract_callee_leaf_name(o, src));
                                if !recv.map(is_stdlib_receiver).unwrap_or(false) {
                                    info.local_refs.insert(name.to_string());
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        for i in 0..node.named_child_count() {
            if let Some(ch) = node.named_child(i) {
                walk(ch, false, src, info);
            }
        }
    }

    walk(def_node, true, src, &mut info);
    info
}

#[derive(Debug)]
pub struct ParseOutput {
    pub definitions: Vec<ParsedDef>,
    pub references: Vec<ParsedRef>,
    pub imports: Vec<ParsedImport>,
    pub local_info: Vec<Option<LocalAstInfo>>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ParseOptions {
    pub include_outline_defs: bool,
    pub include_local_refs: bool,
    pub dedupe: bool,
}

pub(crate) fn create_definition_pub(
    name: String,
    node: Node,
    lang: Lang,
    src: &[u8],
    explicit_kind: Option<SymbolKind>,
    explicit_exported: Option<bool>,
    explicit_body_start: Option<u32>,
) -> ParsedDef {
    create_definition(
        name,
        node,
        lang,
        src,
        explicit_kind,
        explicit_exported,
        explicit_body_start,
    )
}

pub(crate) fn extract_callee_leaf_name_pub<'a>(node: Node<'a>, src: &'a [u8]) -> Option<&'a str> {
    extract_callee_leaf_name(node, src)
}

pub fn parse_file(file_path: &str, source: &str, opts: ParseOptions) -> Option<ParseOutput> {
    let lang = Lang::from_path(file_path)?;
    if lang.is_js() && !opts.include_outline_defs && !opts.include_local_refs && opts.dedupe {
        return crate::oxc_index::parse_js_index_oxc(file_path, source, lang);
    }
    let src = source.as_bytes();
    let tree = with_parser(lang, |p| p.parse(src, None))?;
    let root = tree.root_node();
    if lang == Lang::Python && !opts.include_outline_defs && !opts.include_local_refs && opts.dedupe {
        return Some(crate::py_index::parse_python_index(root, src));
    }
    let q = queries_for(lang);

    let js_bottom_exports = if lang.is_js() {
        Some(collect_js_bottom_exports(root, src))
    } else {
        None
    };

    let mut definitions = Vec::new();
    let mut def_node_ranges: Vec<Option<(usize, usize)>> = Vec::new();
    let mut seen_def_keys: FxHashSet<(String, u32)> = FxHashSet::default();

    TL_CURSOR.with(|cell| {
        let mut cursor = cell.borrow_mut();
        let mut matches = cursor.matches(&q.def_query, root, src);
        while let Some(m) = matches.next() {
            let mut name_node = None;
            let mut def_node = None;
            for cap in m.captures {
                if cap.index == q.def_name_idx {
                    name_node = Some(cap.node);
                } else if cap.index == q.def_node_idx {
                    def_node = Some(cap.node);
                }
            }
            let Some(nn) = name_node else { continue };
            let dn = def_node.unwrap_or(nn);
            if !opts.include_outline_defs && is_nested_inside_function(dn) {
                continue;
            }
            let name = node_text(nn, src).to_string();
            let row = dn.start_position().row as u32;
            if !seen_def_keys.insert((name.clone(), row)) {
                continue;
            }
            let exported = is_node_exported(dn, lang)
                || js_bottom_exports
                    .as_ref()
                    .map_or(false, |set| set.contains(&name));
            definitions.push(create_definition(
                name,
                dn,
                lang,
                src,
                None,
                Some(exported),
                None,
            ));
            if opts.include_local_refs {
                def_node_ranges.push(Some((dn.start_byte(), dn.end_byte())));
            }
        }
    });

    // Mark methods of bottom-exported JS/TS classes as exported
    if lang.is_js() {
        let exported_classes: Vec<(u32, u32)> = definitions
            .iter()
            .filter(|d| d.kind == SymbolKind::Class as u8 && d.exported)
            .map(|d| (d.line_start, d.line_end))
            .collect();
        if !exported_classes.is_empty() {
            for d in &mut definitions {
                if d.kind == SymbolKind::Method as u8 && !d.exported {
                    if exported_classes
                        .iter()
                        .any(|&(s, e)| d.line_start >= s && d.line_end <= e)
                    {
                        d.exported = true;
                    }
                }
            }
        }
    }

    let extra_defs = collect_index_definitions(root, lang, src);
    for d in extra_defs {
        definitions.push(d);
        if opts.include_local_refs {
            def_node_ranges.push(None);
        }
    }

    if lang == Lang::Python {
        let py_consts = collect_python_top_level_constants(root, src);
        for d in py_consts {
            definitions.push(d);
            if opts.include_local_refs {
                def_node_ranges.push(None);
            }
        }
    }

    if opts.include_outline_defs {
        definitions.extend(collect_outline_definitions(root, lang, src));
    }

    let mut references = Vec::new();
    let mut imports = Vec::new();

    if !opts.include_outline_defs {
        let (symbol_aliases, module_aliases) = if lang.is_js() {
            collect_js_import_info(root, source, src)
        } else if lang == Lang::Python {
            if let Some(ref iq) = q.import_query {
                let (py_imps, py_refs, sym_a, mod_a) =
                    collect_python_imports_and_aliases(root, src, iq);
                imports.extend(py_imps);
                references.extend(py_refs);
                (sym_a, mod_a)
            } else {
                (FxHashMap::default(), FxHashMap::default())
            }
        } else {
            (FxHashMap::default(), FxHashMap::default())
        };

        let ref_idx = q.ref_query.capture_index_for_name("ref");
        let import_ref_idx = q.ref_query.capture_index_for_name("import_ref");
        let jsx_ref_idx = q.ref_query.capture_index_for_name("jsx_ref");
        let prop_ref_idx = q.ref_query.capture_index_for_name("prop_ref");
        let prop_recv_idx = q.ref_query.capture_index_for_name("prop_recv");
        let method_ref_idx = q.ref_query.capture_index_for_name("method_ref");
        let method_recv_idx = q.ref_query.capture_index_for_name("method_recv");

        TL_CURSOR.with(|cell| {
            let mut cursor = cell.borrow_mut();
            let mut matches = cursor.matches(&q.ref_query, root, src);
            while let Some(m) = matches.next() {
                let mut method_recv: Option<&str> = None;
                let mut prop_recv: Option<&str> = None;
                for cap in m.captures {
                    if Some(cap.index) == method_recv_idx {
                        method_recv = extract_callee_leaf_name(cap.node, src);
                    } else if Some(cap.index) == prop_recv_idx {
                        prop_recv = extract_callee_leaf_name(cap.node, src);
                    }
                }

                for cap in m.captures {
                    let is_method = Some(cap.index) == method_ref_idx;
                    let is_prop = Some(cap.index) == prop_ref_idx;
                    let is_jsx = Some(cap.index) == jsx_ref_idx;
                    let is_ref = Some(cap.index) == ref_idx;
                    let is_imp_ref = Some(cap.index) == import_ref_idx;
                    if !(is_method || is_prop || is_jsx || is_ref || is_imp_ref) {
                        continue;
                    }
                    let text = node_text(cap.node, src);
                    if text.is_empty() {
                        continue;
                    }
                    if is_jsx && !text.as_bytes()[0].is_ascii_uppercase() {
                        continue;
                    }

                    let line = cap.node.start_position().row as u32 + 1;

                    if is_method {
                        if let Some(mr) = method_recv {
                            if is_stdlib_receiver(mr) {
                                continue;
                            }
                        }
                        let recv_mod = method_recv.and_then(|r| module_aliases.get(r).cloned());
                        references.push(ParsedRef {
                            name: text.to_string(),
                            kind: RefKind::Method as u8,
                            receiver: method_recv.map(|s| s.to_string()),
                            receiver_module: recv_mod,
                            is_rpc_chain: false,
                            line,
                        });
                        continue;
                    }

                    if is_prop {
                        if let Some(pr) = prop_recv {
                            if is_stdlib_receiver(pr) {
                                continue;
                            }
                        }
                        let recv_mod = prop_recv.and_then(|r| module_aliases.get(r).cloned());
                        let is_rpc = if lang.is_js() {
                            is_rpc_chain_prop(cap.node, src)
                        } else {
                            false
                        };
                        references.push(ParsedRef {
                            name: text.to_string(),
                            kind: RefKind::Property as u8,
                            receiver: prop_recv.map(|s| s.to_string()),
                            receiver_module: recv_mod,
                            is_rpc_chain: is_rpc,
                            line,
                        });
                        continue;
                    }

                    let resolved_name = if is_ref {
                        symbol_aliases
                            .get(text)
                            .cloned()
                            .unwrap_or_else(|| text.to_string())
                    } else {
                        text.to_string()
                    };
                    references.push(ParsedRef {
                        name: resolved_name,
                        kind: RefKind::Symbol as u8,
                        receiver: None,
                        receiver_module: None,
                        is_rpc_chain: false,
                        line,
                    });
                }
            }
        });

        if lang.is_js() || lang == Lang::Python {
            references.extend(collect_same_file_variable_refs(
                source,
                &definitions,
                lang == Lang::Python,
            ));
        }

        if lang.is_js() {
            if let Some(ref iq) = q.import_query {
                let req_idx = iq.capture_index_for_name("require_fn");
                let reexp_idx = iq.capture_index_for_name("reexport_path");
                let imp_idx = iq.capture_index_for_name("import_path");

                TL_CURSOR.with(|cell| {
                    let mut cursor = cell.borrow_mut();
                    let mut matches = cursor.matches(iq, root, src);
                    while let Some(m) = matches.next() {
                        let mut require_ok = true;
                        let mut reexport_node = None;
                        let mut path_node = None;
                        for cap in m.captures {
                            if Some(cap.index) == req_idx {
                                if node_text(cap.node, src) != "require" {
                                    require_ok = false;
                                }
                            } else if Some(cap.index) == reexp_idx {
                                reexport_node = Some(cap.node);
                            } else if Some(cap.index) == imp_idx {
                                path_node = Some(cap.node);
                            }
                        }
                        if !require_ok {
                            continue;
                        }
                        let is_reexport = reexport_node.is_some();
                        let Some(pn) = reexport_node.or(path_node) else {
                            continue;
                        };
                        let cleaned = node_text(pn, src)
                            .trim_matches(|c| c == '\'' || c == '"' || c == '`');
                        if !cleaned.is_empty() {
                            imports.push(ParsedImport {
                                path: cleaned.to_string(),
                                is_reexport,
                            });
                        }
                    }
                });
            }
        }
    }

    let mut local_info = Vec::new();
    if opts.include_local_refs && !definitions.is_empty() {
        local_info.resize_with(definitions.len(), || None);
        for (idx, range_opt) in def_node_ranges.iter().enumerate() {
            if let Some((sb, eb)) = range_opt {
                if let Some(node) = root.descendant_for_byte_range(*sb, *eb) {
                    local_info[idx] = Some(collect_local_ast_info(node, src));
                }
            }
        }
        // Attach line-scoped references for extra definitions (properties/constants)
        for r in &references {
            let mut enclosing_idx = None;
            let mut best_span = u32::MAX;
            for (idx, def) in definitions.iter().enumerate() {
                if r.line >= def.line_start && r.line <= def.line_end {
                    let span = def.line_end - def.line_start;
                    if span <= best_span {
                        best_span = span;
                        enclosing_idx = Some(idx);
                    }
                }
            }
            if let Some(idx) = enclosing_idx {
                if local_info[idx].is_none() {
                    let entry = local_info[idx].get_or_insert_with(LocalAstInfo::default);
                    entry.local_refs.insert(r.name.clone());
                } else if def_node_ranges[idx].is_none() {
                    if let Some(ref mut entry) = local_info[idx] {
                        entry.local_refs.insert(r.name.clone());
                    }
                }
            }
        }
    }

    if opts.dedupe {
        dedupe_refs(&mut references);
        dedupe_imports(&mut imports);
    }

    Some(ParseOutput {
        definitions,
        references,
        imports,
        local_info,
    })
}

pub fn dedupe_refs(refs: &mut Vec<ParsedRef>) {
    let mut seen: FxHashSet<(String, u8, Option<String>, Option<String>, bool)> =
        FxHashSet::default();
    refs.retain(|r| {
        seen.insert((
            r.name.clone(),
            r.kind,
            r.receiver.clone(),
            r.receiver_module.clone(),
            r.is_rpc_chain,
        ))
    });
}

pub fn dedupe_imports(imports: &mut Vec<ParsedImport>) {
    let mut seen: FxHashSet<(String, bool)> = FxHashSet::default();
    imports.retain(|imp| seen.insert((imp.path.clone(), imp.is_reexport)));
}
