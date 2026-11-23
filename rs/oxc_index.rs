//! Ultra-fast arena-allocated JS/TS/TSX bulk indexer powered by `oxc_parser`.
//!
//! Replaces 4 separate `tree-sitter` AST/QueryCursor walks + full-file regex/string-strip
//! passes with a single zero-allocation `oxc_ast_visit::Visit` traversal over a thread-local
//! reusable `Allocator` arena, with SIMD `memchr` line indexing and zero-alloc ref deduping.

use crate::model::{ParsedDef, ParsedImport, ParsedRef, RefKind, SymbolKind};
use crate::parser::{is_method_noise, is_stdlib_receiver, Lang, ParseOutput};
use oxc_allocator::Allocator;
use oxc_ast::ast::*;
use oxc_ast_visit::walk;
use oxc_ast_visit::Visit;
use oxc_parser::{ParseOptions as OxcParseOptions, Parser as OxcParser};
use oxc_span::{GetSpan, SourceType, Span};
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::RefCell;
use std::sync::OnceLock;

const RPC_TERMINAL_METHODS: &[&str] = &[
    "useQuery",
    "useMutation",
    "useInfiniteQuery",
    "useSuspenseQuery",
    "useSuspenseInfiniteQuery",
    "useQueries",
    "useUtils",
    "useSubscription",
    "invalidate",
    "fetch",
    "prefetch",
    "cancel",
    "getData",
    "setData",
    "getInfiniteData",
    "setInfiniteData",
    "ensureData",
    "query",
    "mutate",
    "subscribe",
];

const RPC_ROOT_RECEIVERS: &[&str] = &["api", "trpc", "rpc", "utils"];

#[inline]
fn is_rpc_terminal(name: &str) -> bool {
    RPC_TERMINAL_METHODS.contains(&name)
}

#[inline]
fn is_rpc_root(name: &str) -> bool {
    RPC_ROOT_RECEIVERS.contains(&name)
}

thread_local! {
    static TL_ALLOC: RefCell<Allocator> = RefCell::new(Allocator::default());
}

struct LineTable {
    starts: Vec<u32>,
}

impl LineTable {
    fn build(source: &str) -> Self {
        let bytes = source.as_bytes();
        let mut starts = Vec::with_capacity((bytes.len() / 40).max(16));
        starts.push(0);
        for pos in memchr::memchr_iter(b'\n', bytes) {
            starts.push(pos as u32 + 1);
        }
        Self { starts }
    }

    #[inline]
    fn line_of(&self, byte_offset: u32) -> u32 {
        self.starts.partition_point(|&s| s <= byte_offset) as u32
    }

    #[inline]
    fn end_line_of(&self, span: Span) -> u32 {
        let end = span.end.saturating_sub(1).max(span.start);
        self.line_of(end)
    }
}

fn source_type_for_lang(lang: Lang, path: &str) -> SourceType {
    match lang {
        Lang::TypeScript => {
            if path.ends_with(".d.ts") || path.ends_with(".d.mts") || path.ends_with(".d.cts") {
                SourceType::d_ts()
            } else {
                SourceType::ts()
            }
        }
        Lang::Tsx => SourceType::tsx(),
        Lang::JavaScript => {
            if path.ends_with(".cjs") {
                SourceType::cjs().with_jsx(true)
            } else {
                SourceType::jsx()
            }
        }
        _ => SourceType::ts(),
    }
}

fn extract_sig(source: &str, span: Span, body_start: Option<u32>) -> String {
    let start = (span.start as usize).min(source.len());
    let raw_end = match body_start {
        Some(bs) if (bs as usize) > start && (bs as usize) <= source.len() => bs as usize,
        _ => {
            let end = (span.end as usize).min(source.len());
            let slice = &source[start..end];
            match slice.find('\n') {
                Some(nl) => start + nl,
                None => end,
            }
        }
    };
    let mut header = source[start..raw_end].trim();
    header = header.trim_end_matches('{').trim();

    let mut collapsed = String::with_capacity(header.len().min(128));
    let mut prev_ws = false;
    for ch in header.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                collapsed.push(' ');
                prev_ws = true;
            }
        } else {
            collapsed.push(ch);
            prev_ws = false;
        }
        if collapsed.len() >= 120 {
            break;
        }
    }
    if collapsed.len() >= 120 && header.len() > 120 {
        while collapsed.len() > 117 {
            collapsed.pop();
        }
        collapsed.push_str("...");
    }
    collapsed
}

#[inline]
fn prop_key_name<'a>(key: &PropertyKey<'a>) -> Option<&'a str> {
    match key {
        PropertyKey::StaticIdentifier(id) => Some(id.name.as_str()),
        PropertyKey::PrivateIdentifier(id) => Some(id.name.as_str()),
        PropertyKey::StringLiteral(s) => {
            let v = s.value.as_str()?;
            if v.is_empty() || v.starts_with('$') {
                None
            } else {
                Some(v)
            }
        }
        PropertyKey::NumericLiteral(n) => n.raw.as_ref().map(|r| r.as_str()),
        _ => None,
    }
}

#[inline]
fn expr_leaf_name<'a>(expr: &Expression<'a>) -> Option<&'a str> {
    match expr {
        Expression::Identifier(id) => Some(id.name.as_str()),
        Expression::StaticMemberExpression(m) => Some(m.property.name.as_str()),
        Expression::ParenthesizedExpression(p) => expr_leaf_name(&p.expression),
        Expression::TSAsExpression(a) => expr_leaf_name(&a.expression),
        Expression::TSNonNullExpression(n) => expr_leaf_name(&n.expression),
        Expression::TSSatisfiesExpression(s) => expr_leaf_name(&s.expression),
        Expression::ChainExpression(c) => match &c.expression {
            ChainElement::StaticMemberExpression(m) => Some(m.property.name.as_str()),
            _ => None,
        },
        _ => None,
    }
}

#[inline]
fn unwrap_ts_expr<'b, 'a>(mut expr: &'b Expression<'a>) -> &'b Expression<'a> {
    loop {
        match expr {
            Expression::ParenthesizedExpression(p) => expr = &p.expression,
            Expression::TSAsExpression(a) => expr = &a.expression,
            Expression::TSSatisfiesExpression(s) => expr = &s.expression,
            Expression::TSNonNullExpression(n) => expr = &n.expression,
            Expression::TSTypeAssertion(t) => expr = &t.expression,
            _ => return expr,
        }
    }
}

fn find_semantic_object<'b, 'a>(
    expr: &'b Expression<'a>,
    depth: u8,
) -> Option<&'b ObjectExpression<'a>> {
    if depth > 4 {
        return None;
    }
    match unwrap_ts_expr(expr) {
        Expression::ObjectExpression(obj) => Some(obj),
        Expression::CallExpression(call) => {
            for arg in &call.arguments {
                if let Some(ae) = arg.as_expression() {
                    if let Some(obj) = find_semantic_object(ae, depth + 1) {
                        return Some(obj);
                    }
                }
            }
            None
        }
        Expression::NewExpression(ne) => {
            for arg in &ne.arguments {
                if let Some(ae) = arg.as_expression() {
                    if let Some(obj) = find_semantic_object(ae, depth + 1) {
                        return Some(obj);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

fn is_literal_expr(expr: &Expression) -> bool {
    matches!(
        unwrap_ts_expr(expr),
        Expression::StringLiteral(_)
            | Expression::NumericLiteral(_)
            | Expression::BooleanLiteral(_)
            | Expression::NullLiteral(_)
            | Expression::BigIntLiteral(_)
            | Expression::RegExpLiteral(_)
            | Expression::TemplateLiteral(_)
    )
}

fn should_index_semantic_object(
    var_name: Option<&str>,
    init: &Expression,
    is_exported: bool,
) -> bool {
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

    if let Some(vn) = var_name {
        if non_semantic_var_re.is_match(vn) {
            return false;
        }
    }

    let val = unwrap_ts_expr(init);
    let has_semantic_var = var_name.map(|v| semantic_re.is_match(v)).unwrap_or(false);

    match val {
        Expression::ObjectExpression(_) => has_semantic_var,
        Expression::CallExpression(call) => {
            if !is_exported {
                return false;
            }
            let callee_name = expr_leaf_name(&call.callee);
            let has_semantic_callee = callee_name
                .map(|c| !non_semantic_callee_re.is_match(c))
                .unwrap_or(false);
            has_semantic_var || has_semantic_callee
        }
        Expression::NewExpression(ne) => {
            if !is_exported {
                return false;
            }
            let callee_name = expr_leaf_name(&ne.callee);
            let has_semantic_callee = callee_name
                .map(|c| !non_semantic_callee_re.is_match(c))
                .unwrap_or(false);
            has_semantic_var || has_semantic_callee
        }
        _ => false,
    }
}

fn is_rpc_root_chain(expr: &Expression) -> bool {
    let mut cur = unwrap_ts_expr(expr);
    if !matches!(cur, Expression::StaticMemberExpression(_)) {
        return false;
    }
    let mut hops = 0u8;
    while let Expression::StaticMemberExpression(m) = cur {
        if hops >= 6 {
            return false;
        }
        cur = unwrap_ts_expr(&m.object);
        hops += 1;
    }
    matches!(cur, Expression::Identifier(id) if is_rpc_root(id.name.as_str()))
}

#[inline]
fn module_export_name_str<'a>(name: &ModuleExportName<'a>) -> Option<&'a str> {
    match name {
        ModuleExportName::IdentifierName(id) => Some(id.name.as_str()),
        ModuleExportName::IdentifierReference(id) => Some(id.name.as_str()),
        ModuleExportName::StringLiteral(s) => s.value.as_str(),
    }
}

fn collect_bottom_exports<'a>(stmts: &[Statement<'a>]) -> FxHashSet<&'a str> {
    let mut exported = FxHashSet::default();
    for stmt in stmts {
        match stmt {
            Statement::ExportNamedDeclaration(exp) => {
                for spec in &exp.specifiers {
                    if let Some(n) = module_export_name_str(&spec.local) {
                        exported.insert(n);
                    }
                }
            }
            Statement::ExportDefaultDeclaration(exp) => {
                if let ExportDefaultDeclarationKind::Identifier(id) = &exp.declaration {
                    exported.insert(id.name.as_str());
                }
            }
            Statement::ExpressionStatement(es) => {
                if let Expression::AssignmentExpression(assign) = &es.expression {
                    let is_cjs_export = match &assign.left {
                        AssignmentTarget::StaticMemberExpression(m) => {
                            let obj = expr_leaf_name(&m.object).unwrap_or("");
                            (obj == "module" && m.property.name.as_str() == "exports")
                                || obj == "exports"
                        }
                        _ => false,
                    };
                    if is_cjs_export {
                        match unwrap_ts_expr(&assign.right) {
                            Expression::Identifier(id) => {
                                exported.insert(id.name.as_str());
                            }
                            Expression::ObjectExpression(obj) => {
                                for prop in &obj.properties {
                                    if let ObjectPropertyKind::ObjectProperty(p) = prop {
                                        if let Expression::Identifier(id) = unwrap_ts_expr(&p.value)
                                        {
                                            exported.insert(id.name.as_str());
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
    }
    exported
}

struct RefCollector<'a> {
    lines: &'a LineTable,
    symbol_aliases: &'a FxHashMap<&'a str, &'a str>,
    module_aliases: &'a FxHashMap<&'a str, &'a str>,
    var_defs: &'a FxHashMap<&'a str, (u32, u32)>,
    seen_var_refs: FxHashSet<&'a str>,
    seen_refs: FxHashSet<(&'a str, u8, Option<&'a str>, Option<&'a str>, bool)>,
    refs: Vec<ParsedRef>,
    seen_imports: FxHashSet<(&'a str, bool)>,
    imports: Vec<ParsedImport>,
}

impl<'a> RefCollector<'a> {
    #[inline]
    fn push_ref(
        &mut self,
        name: &'a str,
        kind: RefKind,
        receiver: Option<&'a str>,
        receiver_module: Option<&'a str>,
        is_rpc_chain: bool,
        span_start: u32,
    ) {
        if name.is_empty() {
            return;
        }
        // Pre-filter guaranteed noise before allocating any String!
        if kind == RefKind::Method && (name.len() < 3 || is_method_noise(name)) {
            return;
        }
        if kind == RefKind::Property
            && !is_rpc_chain
            && receiver_module.is_none()
            && (name.len() < 2 || is_method_noise(name))
        {
            return;
        }
        let k = kind as u8;
        if self
            .seen_refs
            .insert((name, k, receiver, receiver_module, is_rpc_chain))
        {
            self.refs.push(ParsedRef {
                name: name.to_string(),
                kind: k,
                receiver: receiver.map(|s| s.to_string()),
                receiver_module: receiver_module.map(|s| s.to_string()),
                is_rpc_chain,
                line: self.lines.line_of(span_start),
            });
        }
    }

    #[inline]
    fn push_import(&mut self, path: &'a str, is_reexport: bool) {
        if path.is_empty() {
            return;
        }
        if self.seen_imports.insert((path, is_reexport)) {
            self.imports.push(ParsedImport {
                path: path.to_string(),
                is_reexport,
            });
        }
    }

    fn visit_jsx_member(&mut self, me: &JSXMemberExpression<'a>) {
        match &me.object {
            JSXMemberExpressionObject::IdentifierReference(id) => {
                let s = id.name.as_str();
                if s.as_bytes().first().map_or(false, |b| b.is_ascii_uppercase()) {
                    self.push_ref(s, RefKind::Symbol, None, None, false, id.span.start);
                }
            }
            JSXMemberExpressionObject::MemberExpression(inner) => {
                self.visit_jsx_member(inner);
            }
            JSXMemberExpressionObject::ThisExpression(_) => {}
        }
        let prop = me.property.name.as_str();
        if prop
            .as_bytes()
            .first()
            .map_or(false, |b| b.is_ascii_uppercase())
        {
            self.push_ref(
                prop,
                RefKind::Symbol,
                None,
                None,
                false,
                me.property.span.start,
            );
        }
    }
}

impl<'a> Visit<'a> for RefCollector<'a> {
    fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
        match unwrap_ts_expr(&it.callee) {
            Expression::Identifier(id) => {
                let raw = id.name.as_str();
                if raw == "require" {
                    if let Some(first) = it.arguments.first().and_then(|a| a.as_expression()) {
                        if let Expression::StringLiteral(sl) = unwrap_ts_expr(first) {
                            if let Some(p) = sl.value.as_str() {
                                self.push_import(p, false);
                            }
                        }
                    }
                }
                let resolved = self.symbol_aliases.get(raw).copied().unwrap_or(raw);
                self.push_ref(resolved, RefKind::Symbol, None, None, false, id.span.start);
            }
            Expression::StaticMemberExpression(member) => {
                let method = member.property.name.as_str();
                let recv = expr_leaf_name(&member.object);
                if !recv.map(is_stdlib_receiver).unwrap_or(false) {
                    let recv_mod = recv.and_then(|r| self.module_aliases.get(r).copied());
                    self.push_ref(
                        method,
                        RefKind::Method,
                        recv,
                        recv_mod,
                        false,
                        member.property.span.start,
                    );
                }
            }
            _ => {}
        }
        walk::walk_call_expression(self, it);
    }

    fn visit_static_member_expression(&mut self, it: &StaticMemberExpression<'a>) {
        let prop = it.property.name.as_str();
        let recv = expr_leaf_name(&it.object);

        // If this is `<innerObj>.<proc>.useQuery` (RPC terminal), mark `<proc>` as `is_rpc_chain = true`!
        if is_rpc_terminal(prop) {
            if let Expression::StaticMemberExpression(inner) = unwrap_ts_expr(&it.object) {
                let inner_prop = inner.property.name.as_str();
                let inner_recv = expr_leaf_name(&inner.object);
                if !inner_recv.map(is_stdlib_receiver).unwrap_or(false) {
                    let recv_mod = inner_recv.and_then(|r| self.module_aliases.get(r).copied());
                    self.push_ref(
                        inner_prop,
                        RefKind::Property,
                        inner_recv,
                        recv_mod,
                        true,
                        inner.property.span.start,
                    );
                }
            }
        }

        if !recv.map(is_stdlib_receiver).unwrap_or(false) {
            let recv_mod = recv.and_then(|r| self.module_aliases.get(r).copied());
            let is_rpc = is_rpc_root_chain(&it.object);
            self.push_ref(
                prop,
                RefKind::Property,
                recv,
                recv_mod,
                is_rpc,
                it.property.span.start,
            );
        }

        walk::walk_static_member_expression(self, it);
    }

    fn visit_ts_type_reference(&mut self, it: &TSTypeReference<'a>) {
        match &it.type_name {
            TSTypeName::IdentifierReference(id) => {
                let raw = id.name.as_str();
                let resolved = self.symbol_aliases.get(raw).copied().unwrap_or(raw);
                self.push_ref(resolved, RefKind::Symbol, None, None, false, id.span.start);
            }
            TSTypeName::QualifiedName(qn) => {
                let right = qn.right.name.as_str();
                self.push_ref(
                    right,
                    RefKind::Symbol,
                    None,
                    None,
                    false,
                    qn.right.span.start,
                );
            }
            TSTypeName::ThisExpression(_) => {}
        }
        walk::walk_ts_type_reference(self, it);
    }

    fn visit_jsx_opening_element(&mut self, it: &JSXOpeningElement<'a>) {
        match &it.name {
            JSXElementName::IdentifierReference(id) => {
                let name = id.name.as_str();
                if name
                    .as_bytes()
                    .first()
                    .map_or(false, |b| b.is_ascii_uppercase())
                {
                    let resolved = self.symbol_aliases.get(name).copied().unwrap_or(name);
                    self.push_ref(resolved, RefKind::Symbol, None, None, false, id.span.start);
                }
            }
            JSXElementName::MemberExpression(me) => {
                self.visit_jsx_member(me);
            }
            _ => {}
        }
        walk::walk_jsx_opening_element(self, it);
    }

    fn visit_identifier_reference(&mut self, it: &IdentifierReference<'a>) {
        if !self.var_defs.is_empty() {
            let name = it.name.as_str();
            if let Some(&(ls, le)) = self.var_defs.get(name) {
                if !self.seen_var_refs.contains(name) {
                    let line = self.lines.line_of(it.span.start);
                    if line < ls || line > le {
                        self.seen_var_refs.insert(name);
                        self.push_ref(name, RefKind::Symbol, None, None, false, it.span.start);
                    }
                }
            }
        }
    }
}

fn push_class_defs<'a>(
    cls: &Class<'a>,
    source: &'a str,
    lines: &LineTable,
    explicit_exported: bool,
    bottom_exports: &FxHashSet<&'a str>,
    seen_defs: &mut FxHashSet<(&'a str, u32)>,
    defs: &mut Vec<ParsedDef>,
) {
    let class_name = cls.id.as_ref().map(|id| id.name.as_str());
    let class_exported = explicit_exported
        || class_name
            .map(|n| bottom_exports.contains(n))
            .unwrap_or(false);

    if let Some(name) = class_name {
        let ls = lines.line_of(cls.span.start);
        if seen_defs.insert((name, ls)) {
            let body_start_line = lines.line_of(cls.body.span.start) + 1;
            defs.push(ParsedDef {
                name: name.to_string(),
                kind: SymbolKind::Class as u8,
                exported: class_exported,
                line_start: ls,
                line_end: lines.end_line_of(cls.span),
                signature: extract_sig(source, cls.span, Some(cls.body.span.start)),
                body_start_line,
            });
        }
    }

    for elem in &cls.body.body {
        if let ClassElement::MethodDefinition(m) = elem {
            if let Some(mname) = prop_key_name(&m.key) {
                let ls = lines.line_of(m.span.start);
                if seen_defs.insert((mname, ls)) {
                    let bstart = m.value.body.as_ref().map(|b| b.span.start);
                    let body_start_line = bstart
                        .map(|bs| lines.line_of(bs) + 1)
                        .unwrap_or(ls + 1);
                    defs.push(ParsedDef {
                        name: mname.to_string(),
                        kind: SymbolKind::Method as u8,
                        exported: class_exported,
                        line_start: ls,
                        line_end: lines.end_line_of(m.span),
                        signature: extract_sig(source, m.span, bstart),
                        body_start_line,
                    });
                }
            }
        }
    }
}

fn push_var_decl_defs<'a>(
    vdecl: &VariableDeclaration<'a>,
    source: &'a str,
    lines: &LineTable,
    is_exported: bool,
    bottom_exports: &FxHashSet<&'a str>,
    seen_defs: &mut FxHashSet<(&'a str, u32)>,
    defs: &mut Vec<ParsedDef>,
    var_defs: &mut FxHashMap<&'a str, (u32, u32)>,
) {
    let decl_ls = lines.line_of(vdecl.span.start);
    let decl_le = lines.end_line_of(vdecl.span);

    for d in &vdecl.declarations {
        let BindingPattern::BindingIdentifier(id) = &d.id else {
            continue;
        };
        let name = id.name.as_str();
        let exported = is_exported || bottom_exports.contains(name);

        let is_arrow = d
            .init
            .as_ref()
            .map(|e| matches!(unwrap_ts_expr(e), Expression::ArrowFunctionExpression(_)))
            .unwrap_or(false);

        if is_exported || is_arrow {
            let ls = lines.line_of(d.span.start);
            if seen_defs.insert((name, ls)) {
                let body_byte = d.init.as_ref().and_then(|e| match unwrap_ts_expr(e) {
                    Expression::ArrowFunctionExpression(af) => Some(af.body.span().start),
                    Expression::FunctionExpression(fe) => fe.body.as_ref().map(|b| b.span.start),
                    _ => None,
                });
                let body_start_line = body_byte
                    .map(|bs| lines.line_of(bs) + 1)
                    .unwrap_or(decl_ls + 1);
                if name.len() >= 2 {
                    var_defs.insert(name, (decl_ls, decl_le));
                }
                defs.push(ParsedDef {
                    name: name.to_string(),
                    kind: SymbolKind::Variable as u8,
                    exported,
                    line_start: decl_ls,
                    line_end: decl_le,
                    signature: extract_sig(source, vdecl.span, body_byte),
                    body_start_line,
                });
            }
        }

        // Semantic object/router members (e.g. createTRPCRouter({ myProc: ... }))
        if let Some(init) = &d.init {
            if should_index_semantic_object(Some(name), init, exported) {
                if let Some(obj) = find_semantic_object(init, 0) {
                    for prop in &obj.properties {
                        if let ObjectPropertyKind::ObjectProperty(p) = prop {
                            let pls = lines.line_of(p.span.start);
                            let ple = lines.end_line_of(p.span);
                            if ple <= pls || is_literal_expr(&p.value) {
                                continue;
                            }
                            if let Some(pname) = prop_key_name(&p.key) {
                                defs.push(ParsedDef {
                                    name: pname.to_string(),
                                    kind: SymbolKind::Property as u8,
                                    exported,
                                    line_start: pls,
                                    line_end: ple,
                                    signature: extract_sig(source, p.span, None),
                                    body_start_line: pls + 1,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}

fn push_declaration_defs<'a>(
    decl: &Declaration<'a>,
    source: &'a str,
    lines: &LineTable,
    is_exported: bool,
    bottom_exports: &FxHashSet<&'a str>,
    seen_defs: &mut FxHashSet<(&'a str, u32)>,
    defs: &mut Vec<ParsedDef>,
    var_defs: &mut FxHashMap<&'a str, (u32, u32)>,
) {
    match decl {
        Declaration::FunctionDeclaration(f) => {
            if let Some(id) = &f.id {
                let name = id.name.as_str();
                let ls = lines.line_of(f.span.start);
                if seen_defs.insert((name, ls)) {
                    let bstart = f.body.as_ref().map(|b| b.span.start);
                    let body_start_line = bstart
                        .map(|bs| lines.line_of(bs) + 1)
                        .unwrap_or(ls + 1);
                    let exported = is_exported || bottom_exports.contains(name);
                    defs.push(ParsedDef {
                        name: name.to_string(),
                        kind: SymbolKind::Function as u8,
                        exported,
                        line_start: ls,
                        line_end: lines.end_line_of(f.span),
                        signature: extract_sig(source, f.span, bstart),
                        body_start_line,
                    });
                }
            }
        }
        Declaration::ClassDeclaration(c) => {
            push_class_defs(c, source, lines, is_exported, bottom_exports, seen_defs, defs);
        }
        Declaration::VariableDeclaration(v) => {
            push_var_decl_defs(
                v,
                source,
                lines,
                is_exported,
                bottom_exports,
                seen_defs,
                defs,
                var_defs,
            );
        }
        Declaration::TSInterfaceDeclaration(i) => {
            let name = i.id.name.as_str();
            let ls = lines.line_of(i.span.start);
            if seen_defs.insert((name, ls)) {
                let exported = is_exported || bottom_exports.contains(name);
                defs.push(ParsedDef {
                    name: name.to_string(),
                    kind: SymbolKind::Type as u8,
                    exported,
                    line_start: ls,
                    line_end: lines.end_line_of(i.span),
                    signature: extract_sig(source, i.span, Some(i.body.span.start)),
                    body_start_line: lines.line_of(i.body.span.start) + 1,
                });
            }
        }
        Declaration::TSTypeAliasDeclaration(t) => {
            let name = t.id.name.as_str();
            let ls = lines.line_of(t.span.start);
            if seen_defs.insert((name, ls)) {
                let exported = is_exported || bottom_exports.contains(name);
                defs.push(ParsedDef {
                    name: name.to_string(),
                    kind: SymbolKind::Type as u8,
                    exported,
                    line_start: ls,
                    line_end: lines.end_line_of(t.span),
                    signature: extract_sig(source, t.span, None),
                    body_start_line: ls + 1,
                });
            }
        }
        Declaration::TSEnumDeclaration(e) => {
            let name = e.id.name.as_str();
            let ls = lines.line_of(e.span.start);
            if seen_defs.insert((name, ls)) {
                let exported = is_exported || bottom_exports.contains(name);
                defs.push(ParsedDef {
                    name: name.to_string(),
                    kind: SymbolKind::Type as u8,
                    exported,
                    line_start: ls,
                    line_end: lines.end_line_of(e.span),
                    signature: extract_sig(source, e.span, Some(e.body.span.start)),
                    body_start_line: lines.line_of(e.body.span.start) + 1,
                });
            }
        }
        Declaration::TSExternalModuleDeclaration(m) => {
            if let Some(block) = &m.body {
                push_module_block_defs(
                    &block.body,
                    source,
                    lines,
                    bottom_exports,
                    seen_defs,
                    defs,
                    var_defs,
                );
            }
        }
        _ => {}
    }
}

fn push_module_block_defs<'a>(
    stmts: &[Statement<'a>],
    source: &'a str,
    lines: &LineTable,
    bottom_exports: &FxHashSet<&'a str>,
    seen_defs: &mut FxHashSet<(&'a str, u32)>,
    defs: &mut Vec<ParsedDef>,
    var_defs: &mut FxHashMap<&'a str, (u32, u32)>,
) {
    for stmt in stmts {
        match stmt {
            Statement::ExportDeclaration(exp) => {
                push_declaration_defs(
                    &exp.declaration,
                    source,
                    lines,
                    true,
                    bottom_exports,
                    seen_defs,
                    defs,
                    var_defs,
                );
            }
            s => {
                if let Some(inner) = s.as_declaration() {
                    push_declaration_defs(
                        inner,
                        source,
                        lines,
                        false,
                        bottom_exports,
                        seen_defs,
                        defs,
                        var_defs,
                    );
                }
            }
        }
    }
}

/// Parse a JS/TS/TSX file for bulk graph indexing using `oxc_parser`.
pub fn parse_js_index_oxc(file_path: &str, source: &str, lang: Lang) -> Option<ParseOutput> {
    TL_ALLOC.with(|cell| {
        let mut alloc = cell.borrow_mut();
        let result = parse_js_with_alloc(&alloc, file_path, source, lang);
        // Reset arena for next file; drop giant arenas (> 8 MB) so threads don't hoard RAM.
        if alloc.capacity() > 8 * 1024 * 1024 {
            *alloc = Allocator::default();
        } else {
            alloc.reset();
        }
        result
    })
}

fn parse_js_with_alloc(
    alloc: &Allocator,
    file_path: &str,
    source: &str,
    lang: Lang,
) -> Option<ParseOutput> {
    let source_type = source_type_for_lang(lang, file_path);
    let ret = OxcParser::new(alloc, source, source_type)
        .with_options(OxcParseOptions {
            parse_regular_expression: false,
            allow_return_outside_function: true,
            ..OxcParseOptions::default()
        })
        .parse();

    if ret.fatal_error && ret.program.body.is_empty() {
        return None;
    }

    let lines = LineTable::build(source);
    let program = &ret.program;
    let bottom_exports = collect_bottom_exports(&program.body);

    let mut definitions = Vec::with_capacity(16);
    let mut seen_defs: FxHashSet<(&str, u32)> = FxHashSet::default();
    let mut var_defs: FxHashMap<&str, (u32, u32)> = FxHashMap::default();

    let mut symbol_aliases: FxHashMap<&str, &str> = FxHashMap::default();
    let mut module_aliases: FxHashMap<&str, &str> = FxHashMap::default();
    let mut imported_sym_sources: FxHashMap<&str, &str> = FxHashMap::default();
    let mut initial_imports: Vec<(&str, bool)> = Vec::new();
    let mut initial_import_refs: Vec<(&str, u32)> = Vec::new();

    for stmt in &program.body {
        match stmt {
            Statement::ImportDeclaration(imp) => {
                let spec_path = imp.source.value.as_str().unwrap_or("");
                if !spec_path.is_empty() {
                    initial_imports.push((spec_path, false));
                }
                if let Some(specs) = &imp.specifiers {
                    for s in specs {
                        match s {
                            ImportDeclarationSpecifier::ImportSpecifier(is) => {
                                if let Some(orig) = module_export_name_str(&is.imported) {
                                    let local = is.local.name.as_str();
                                    initial_import_refs.push((orig, is.imported.span().start));
                                    if orig != local {
                                        symbol_aliases.insert(local, orig);
                                    }
                                    if !spec_path.is_empty() {
                                        imported_sym_sources.insert(local, spec_path);
                                    }
                                }
                            }
                            ImportDeclarationSpecifier::ImportDefaultSpecifier(id) => {
                                let local = id.local.name.as_str();
                                initial_import_refs.push((local, id.local.span.start));
                                if !spec_path.is_empty() {
                                    imported_sym_sources.insert(local, spec_path);
                                }
                            }
                            ImportDeclarationSpecifier::ImportNamespaceSpecifier(ns) => {
                                if !spec_path.is_empty() {
                                    module_aliases.insert(ns.local.name.as_str(), spec_path);
                                }
                            }
                        }
                    }
                }
            }
            Statement::ExportDeclaration(exp) => {
                push_declaration_defs(
                    &exp.declaration,
                    source,
                    &lines,
                    true,
                    &bottom_exports,
                    &mut seen_defs,
                    &mut definitions,
                    &mut var_defs,
                );
            }
            Statement::ExportNamedDeclaration(exp) => {
                for spec in &exp.specifiers {
                    if let Some(loc) = module_export_name_str(&spec.local) {
                        initial_import_refs.push((loc, spec.local.span().start));
                    }
                }
            }
            Statement::ExportFromDeclaration(exp) => {
                if let Some(src) = exp.source.value.as_str() {
                    initial_imports.push((src, true));
                }
                for spec in &exp.specifiers {
                    if let Some(loc) = module_export_name_str(&spec.local) {
                        initial_import_refs.push((loc, spec.local.span().start));
                    }
                }
            }
            Statement::ExportAllDeclaration(exp) => {
                if let Some(src) = exp.source.value.as_str() {
                    initial_imports.push((src, true));
                }
            }
            Statement::ExportDefaultDeclaration(exp) => match &exp.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                    if let Some(id) = &f.id {
                        let name = id.name.as_str();
                        let ls = lines.line_of(f.span.start);
                        if seen_defs.insert((name, ls)) {
                            let bstart = f.body.as_ref().map(|b| b.span.start);
                            definitions.push(ParsedDef {
                                name: name.to_string(),
                                kind: SymbolKind::Function as u8,
                                exported: true,
                                line_start: ls,
                                line_end: lines.end_line_of(f.span),
                                signature: extract_sig(source, f.span, bstart),
                                body_start_line: bstart
                                    .map(|bs| lines.line_of(bs) + 1)
                                    .unwrap_or(ls + 1),
                            });
                        }
                    }
                }
                ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                    push_class_defs(
                        c,
                        source,
                        &lines,
                        true,
                        &bottom_exports,
                        &mut seen_defs,
                        &mut definitions,
                    );
                }
                ExportDefaultDeclarationKind::TSInterfaceDeclaration(i) => {
                    let name = i.id.name.as_str();
                    let ls = lines.line_of(i.span.start);
                    if seen_defs.insert((name, ls)) {
                        definitions.push(ParsedDef {
                            name: name.to_string(),
                            kind: SymbolKind::Type as u8,
                            exported: true,
                            line_start: ls,
                            line_end: lines.end_line_of(i.span),
                            signature: extract_sig(source, i.span, Some(i.body.span.start)),
                            body_start_line: lines.line_of(i.body.span.start) + 1,
                        });
                    }
                }
                _ => {}
            },
            s => {
                if let Some(decl) = s.as_declaration() {
                    push_declaration_defs(
                        decl,
                        source,
                        &lines,
                        false,
                        &bottom_exports,
                        &mut seen_defs,
                        &mut definitions,
                        &mut var_defs,
                    );
                }
            }
        }
    }

    // Track `createCallerFactory(router)` / `router.createCaller()` bindings (fast pre-checked)
    if !imported_sym_sources.is_empty() && source.contains("createCaller") {
        static DIRECT_RE: OnceLock<Regex> = OnceLock::new();
        static CALL_RE: OnceLock<Regex> = OnceLock::new();
        let direct_re = DIRECT_RE.get_or_init(|| {
            Regex::new(r"\b(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*=\s*(?:createCallerFactory\s*\(\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*\)|([A-Za-z_$][A-Za-z0-9_$]*)\s*\.\s*createCaller\s*\()").unwrap()
        });
        let call_re = CALL_RE.get_or_init(|| {
            Regex::new(r"\b(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*=\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*\(").unwrap()
        });

        let mut factory_to_mod: FxHashMap<&str, &str> = FxHashMap::default();
        for caps in direct_re.captures_iter(source) {
            let var_name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let is_factory_expr = caps.get(2).is_some();
            let router_name = caps
                .get(2)
                .or_else(|| caps.get(3))
                .map(|m| m.as_str())
                .unwrap_or("");
            if let Some(&mod_spec) = imported_sym_sources.get(router_name) {
                let end_idx = caps.get(0).map(|m| m.end()).unwrap_or(0);
                let after = source[end_idx..source.len().min(end_idx + 40)].trim_start();
                if is_factory_expr && !after.starts_with('(') {
                    factory_to_mod.insert(var_name, mod_spec);
                } else {
                    module_aliases.insert(var_name, mod_spec);
                }
            }
        }
        if !factory_to_mod.is_empty() {
            for caps in call_re.captures_iter(source) {
                let var_name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
                let fn_name = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                if let Some(&mod_spec) = factory_to_mod.get(fn_name) {
                    module_aliases.insert(var_name, mod_spec);
                }
            }
        }
    }

    let mut collector = RefCollector {
        lines: &lines,
        symbol_aliases: &symbol_aliases,
        module_aliases: &module_aliases,
        var_defs: &var_defs,
        seen_var_refs: FxHashSet::default(),
        seen_refs: FxHashSet::default(),
        refs: Vec::with_capacity(32),
        seen_imports: FxHashSet::default(),
        imports: Vec::with_capacity(initial_imports.len()),
    };

    for (p, reexp) in initial_imports {
        collector.push_import(p, reexp);
    }
    for (rname, byte_pos) in initial_import_refs {
        collector.push_ref(rname, RefKind::Symbol, None, None, false, byte_pos);
    }

    collector.visit_program(program);

    Some(ParseOutput {
        definitions,
        references: collector.refs,
        imports: collector.imports,
        local_info: Vec::new(),
    })
}
