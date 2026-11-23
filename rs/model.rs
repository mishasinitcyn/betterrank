use rkyv::{Archive, Deserialize, Serialize};

pub const GRAPH_CACHE_MAGIC: [u8; 8] = *b"BRG00001";
pub const PARSE_CACHE_MAGIC: [u8; 8] = *b"BRP00001";
pub const CACHE_FORMAT_VERSION: u32 = 1;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Archive, Serialize, Deserialize)]
pub enum SymbolKind {
    Function = 0,
    Method = 1,
    Class = 2,
    Type = 3,
    Variable = 4,
    Property = 5,
    Namespace = 6,
    Other = 7,
}

impl SymbolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SymbolKind::Function => "function",
            SymbolKind::Method => "method",
            SymbolKind::Class => "class",
            SymbolKind::Type => "type",
            SymbolKind::Variable => "variable",
            SymbolKind::Property => "property",
            SymbolKind::Namespace => "namespace",
            SymbolKind::Other => "other",
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => SymbolKind::Function,
            1 => SymbolKind::Method,
            2 => SymbolKind::Class,
            3 => SymbolKind::Type,
            4 => SymbolKind::Variable,
            5 => SymbolKind::Property,
            6 => SymbolKind::Namespace,
            _ => SymbolKind::Other,
        }
    }

    pub fn matches_filter(self, requested: &str) -> bool {
        if requested == "function" {
            self == SymbolKind::Function || self == SymbolKind::Method
        } else {
            self.as_str() == requested
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Archive, Serialize, Deserialize)]
pub enum RefKind {
    Symbol = 0,
    Method = 1,
    Property = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LangFamily {
    Js,
    Python,
    Rust,
    Go,
    Other,
}

pub fn language_family(path: &str) -> LangFamily {
    let ext = match path.rfind('.') {
        Some(idx) => &path[idx..],
        None => return LangFamily::Other,
    };
    match ext {
        ".js" | ".mjs" | ".cjs" | ".jsx" | ".ts" | ".tsx" => LangFamily::Js,
        ".py" => LangFamily::Python,
        ".rs" => LangFamily::Rust,
        ".go" => LangFamily::Go,
        _ => LangFamily::Other,
    }
}

pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    ".js", ".mjs", ".cjs", ".jsx", ".ts", ".tsx", ".py", ".rs", ".go",
];

pub fn is_supported_extension(path: &str) -> bool {
    let ext = match path.rfind('.') {
        Some(idx) => &path[idx..],
        None => return false,
    };
    SUPPORTED_EXTENSIONS.contains(&ext)
}

#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
pub struct ParsedDef {
    pub name: String,
    pub kind: u8,
    pub exported: bool,
    pub line_start: u32,
    pub line_end: u32,
    pub signature: String,
    /// Only used by `outline` (0 when omitted in index mode)
    pub body_start_line: u32,
}

#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
pub struct ParsedRef {
    pub name: String,
    pub kind: u8,
    pub receiver: Option<String>,
    pub receiver_module: Option<String>,
    pub is_rpc_chain: bool,
    pub line: u32,
}

#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
pub struct ParsedImport {
    pub path: String,
    pub is_reexport: bool,
}

#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
pub struct ParsedFileRecord {
    pub rel_path: String,
    pub mtime_ms: u64,
    pub skipped: bool,
    pub definitions: Vec<ParsedDef>,
    pub references: Vec<ParsedRef>,
    pub imports: Vec<ParsedImport>,
}

#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
pub struct ParseCacheData {
    pub version: u32,
    pub files: Vec<ParsedFileRecord>,
}

/// File entry in the zero-copy mmap graph cache.
/// `symbol_count < 0` means the file was scanned and skipped (minified/generated)
/// so its mtime is tracked without adding a graph file node.
#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub mtime_ms: u64,
    pub symbol_count: i32,
    pub file_score: f32,
}

#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
pub struct SymbolEntry {
    pub file_idx: u32,
    pub name: String,
    pub kind: u8,
    pub exported: bool,
    pub line_start: u32,
    pub line_end: u32,
    pub signature: String,
    pub score: f32,
}

/// Compact CSR-encoded zero-copy graph cache stored in `.brbin` and `mmap`'d on warm queries.
#[derive(Debug, Clone, Archive, Serialize, Deserialize)]
pub struct GraphCacheData {
    pub version: u32,
    pub files: Vec<FileEntry>,
    pub symbols: Vec<SymbolEntry>,
    /// File indices sorted lexicographically by `files[idx].path` for O(log F) lookup
    pub file_order: Vec<u32>,
    /// Symbol indices sorted lexicographically by `symbols[idx].name` for O(log S) lookup
    pub sym_name_order: Vec<u32>,
    /// Symbol indices sorted descending by `symbols[idx].score` (PageRank)
    pub ranked_syms: Vec<u32>,
    /// CSR: file -> defined symbol indices (len = files.len() + 1)
    pub def_offsets: Vec<u32>,
    pub def_syms: Vec<u32>,
    /// CSR: file -> outgoing IMPORTS target file indices
    pub imp_out_offsets: Vec<u32>,
    pub imp_out_targets: Vec<u32>,
    pub imp_out_reexport: Vec<u8>,
    /// CSR: file -> incoming IMPORTS source file indices
    pub imp_in_offsets: Vec<u32>,
    pub imp_in_sources: Vec<u32>,
    /// CSR: symbol -> incoming REFERENCES source file indices
    pub ref_in_offsets: Vec<u32>,
    pub ref_in_sources: Vec<u32>,
    /// CSR: file -> outgoing REFERENCES target symbol indices
    pub ref_out_offsets: Vec<u32>,
    pub ref_out_targets: Vec<u32>,
    pub total_edges: u32,
}

#[derive(Debug, Clone)]
pub struct WorkspacePackage {
    pub name: String,
    pub dir: String,
    pub main: Option<String>,
    pub exports: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct TsPathMapping {
    pub dir: String,
    pub base_url: String,
    pub paths: Vec<(String, Vec<String>)>,
}

#[derive(Debug, Clone, Default)]
pub struct ResolverCtx {
    pub workspace_packages: Vec<WorkspacePackage>,
    pub python_roots: Vec<String>,
    pub ts_path_mappings: Vec<TsPathMapping>,
}

#[derive(Debug, Clone)]
pub struct RootEntry {
    pub path: std::path::PathBuf,
    pub prefix: String,
}
