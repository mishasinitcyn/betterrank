import { createRequire } from 'module';
const require = createRequire(import.meta.url);

const Parser = require('tree-sitter');

// Load native grammars eagerly — no WASM, no async init needed
const tsGrammars = require('tree-sitter-typescript');
const phpModule = require('tree-sitter-php');

const GRAMMARS = {
  javascript: require('tree-sitter-javascript'),
  typescript: tsGrammars.typescript,
  tsx: tsGrammars.tsx,
  python: require('tree-sitter-python'),
  rust: require('tree-sitter-rust'),
  go: require('tree-sitter-go'),
  ruby: require('tree-sitter-ruby'),
  java: require('tree-sitter-java'),
  c: require('tree-sitter-c'),
  cpp: require('tree-sitter-cpp'),
  c_sharp: require('tree-sitter-c-sharp'),
  php: phpModule.php || phpModule,
};

const LANG_MAP = {
  '.js': 'javascript',
  '.mjs': 'javascript',
  '.cjs': 'javascript',
  '.jsx': 'javascript',
  '.ts': 'typescript',
  '.tsx': 'tsx',
  '.py': 'python',
  '.rs': 'rust',
  '.go': 'go',
  '.rb': 'ruby',
  '.java': 'java',
  '.c': 'c',
  '.h': 'c',
  '.cpp': 'cpp',
  '.hpp': 'cpp',
  '.cc': 'cpp',
  '.cs': 'c_sharp',
  '.php': 'php',
};

const SUPPORTED_EXTENSIONS = Object.keys(LANG_MAP);

function getLanguage(ext) {
  const langName = LANG_MAP[ext];
  if (!langName) return null;
  return GRAMMARS[langName] || null;
}

function getLangName(ext) {
  return LANG_MAP[ext] || null;
}

// --- Tree-sitter query strings per language ---

const DEF_QUERIES = {
  javascript: `
    (function_declaration name: (identifier) @name) @definition
    (lexical_declaration (variable_declarator name: (identifier) @name value: (arrow_function) @_val)) @definition
    (class_declaration name: (identifier) @name) @definition
    (method_definition name: (property_identifier) @name) @definition
    (export_statement declaration: (function_declaration name: (identifier) @name) @definition)
    (export_statement declaration: (class_declaration name: (identifier) @name) @definition)
    (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name value: (arrow_function) @_val)) @definition)
  `,

  typescript: `
    (function_declaration name: (identifier) @name) @definition
    (lexical_declaration (variable_declarator name: (identifier) @name value: (arrow_function) @_val)) @definition
    (class_declaration name: (type_identifier) @name) @definition
    (method_definition name: (property_identifier) @name) @definition
    (interface_declaration name: (type_identifier) @name) @definition
    (type_alias_declaration name: (type_identifier) @name) @definition
    (enum_declaration name: (identifier) @name) @definition
    (export_statement declaration: (function_declaration name: (identifier) @name) @definition)
    (export_statement declaration: (class_declaration name: (type_identifier) @name) @definition)
    (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name value: (arrow_function) @_val)) @definition)
    (export_statement declaration: (interface_declaration name: (type_identifier) @name) @definition)
    (export_statement declaration: (type_alias_declaration name: (type_identifier) @name) @definition)
    (export_statement declaration: (enum_declaration name: (identifier) @name) @definition)
  `,

  python: `
    (function_definition name: (identifier) @name) @definition
    (class_definition name: (identifier) @name) @definition
    (decorated_definition definition: (function_definition name: (identifier) @name) @definition)
    (decorated_definition definition: (class_definition name: (identifier) @name) @definition)
  `,

  rust: `
    (function_item name: (identifier) @name) @definition
    (struct_item name: (type_identifier) @name) @definition
    (enum_item name: (type_identifier) @name) @definition
    (trait_item name: (type_identifier) @name) @definition
    (impl_item trait: (type_identifier) @name) @definition
    (type_item name: (type_identifier) @name) @definition
  `,

  go: `
    (function_declaration name: (identifier) @name) @definition
    (method_declaration name: (field_identifier) @name) @definition
    (type_declaration (type_spec name: (type_identifier) @name)) @definition
  `,

  java: `
    (class_declaration name: (identifier) @name) @definition
    (interface_declaration name: (identifier) @name) @definition
    (method_declaration name: (identifier) @name) @definition
    (enum_declaration name: (identifier) @name) @definition
  `,

  ruby: `
    (method name: (identifier) @name) @definition
    (singleton_method name: (identifier) @name) @definition
    (class name: (constant) @name) @definition
  `,

  c: `
    (function_definition declarator: (function_declarator declarator: (identifier) @name)) @definition
    (struct_specifier name: (type_identifier) @name) @definition
    (enum_specifier name: (type_identifier) @name) @definition
  `,

  cpp: `
    (function_definition declarator: (function_declarator declarator: (identifier) @name)) @definition
    (class_specifier name: (type_identifier) @name) @definition
    (struct_specifier name: (type_identifier) @name) @definition
    (enum_specifier name: (type_identifier) @name) @definition
    (namespace_definition name: (namespace_identifier) @name) @definition
  `,

  c_sharp: `
    (class_declaration name: (identifier) @name) @definition
    (interface_declaration name: (identifier) @name) @definition
    (method_declaration name: (identifier) @name) @definition
    (struct_declaration name: (identifier) @name) @definition
    (enum_declaration name: (identifier) @name) @definition
  `,

  php: `
    (function_definition name: (name) @name) @definition
    (class_declaration name: (name) @name) @definition
    (method_declaration name: (name) @name) @definition
    (interface_declaration name: (name) @name) @definition
  `,
};

// TSX uses the same queries as TypeScript
DEF_QUERIES.tsx = DEF_QUERIES.typescript;

const REF_QUERIES = {
  // NOTE: We intentionally omit obj.method() patterns (member_expression,
  // attribute calls, selector_expression) because without type information,
  // common method names (get, close, execute, run) create massive spurious
  // cross-wiring. Bare function calls and imports provide the structural
  // backbone; IMPORTS edges from import statements connect files.
  javascript: `
    (call_expression function: (identifier) @ref)
    (import_specifier name: (identifier) @ref)
    (import_clause (identifier) @ref)
  `,

  typescript: `
    (call_expression function: (identifier) @ref)
    (import_specifier name: (identifier) @ref)
    (import_clause (identifier) @ref)
    (type_identifier) @ref
  `,

  python: `
    (call function: (identifier) @ref)
    (decorator (identifier) @ref)
  `,

  rust: `
    (call_expression function: (identifier) @ref)
    (call_expression function: (scoped_identifier name: (identifier) @ref))
    (type_identifier) @ref
  `,

  go: `
    (call_expression function: (identifier) @ref)
    (type_identifier) @ref
  `,

  java: `
    (object_creation_expression type: (type_identifier) @ref)
    (type_identifier) @ref
  `,

  default: `
    (call_expression function: (identifier) @ref)
  `,
};

REF_QUERIES.tsx = REF_QUERIES.typescript;

const KIND_MAP = {
  function_declaration: 'function',
  function_definition: 'function',
  arrow_function: 'function',
  method_definition: 'function',
  method_declaration: 'function',
  function_item: 'function',
  singleton_method: 'function',
  class_declaration: 'class',
  class_definition: 'class',
  class_specifier: 'class',
  struct_item: 'class',
  struct_specifier: 'class',
  struct_declaration: 'class',
  interface_declaration: 'type',
  type_alias_declaration: 'type',
  type_item: 'type',
  enum_declaration: 'type',
  enum_item: 'type',
  enum_specifier: 'type',
  trait_item: 'type',
  impl_item: 'type',
  type_declaration: 'type',
  type_spec: 'type',
  lexical_declaration: 'variable',
  variable_declaration: 'variable',
  variable_declarator: 'variable',
  namespace_definition: 'namespace',
  decorated_definition: 'function',
};

function nodeKind(nodeType) {
  return KIND_MAP[nodeType] || 'other';
}

function extractSignature(node, langName) {
  const text = node.text;
  if (langName === 'python') {
    // Python signatures often span multiple lines. We need the colon that
    // terminates the def/class line, NOT colons inside type annotations.
    // Strategy: find the closing paren, then the next colon after it.
    // For classes without parens (class Foo:), fall back to first colon.
    const parenClose = text.indexOf(')');
    let colonIdx;
    if (parenClose !== -1) {
      colonIdx = text.indexOf(':', parenClose + 1);
    } else {
      colonIdx = text.indexOf(':');
    }
    if (colonIdx !== -1) {
      // Collapse multiline signature into a single line
      const sig = text.substring(0, colonIdx + 1).replace(/\s*\n\s*/g, ' ').trim();
      return sig.length > 300 ? sig.substring(0, 300) + '...' : sig;
    }
    const firstLine = text.split('\n')[0];
    return firstLine.trim();
  }
  const braceIdx = text.indexOf('{');
  let end = text.indexOf('\n');
  if (end === -1) end = text.length;
  if (braceIdx !== -1 && braceIdx < end) end = braceIdx;
  const sig = text.substring(0, end).trim();
  return sig.length > 200 ? sig.substring(0, 200) + '...' : sig;
}

/**
 * Parse a single source file and extract definitions + references.
 * Returns null if the language is unsupported.
 */
function parseFile(filePath, source) {
  const dotIdx = filePath.lastIndexOf('.');
  if (dotIdx === -1) return null;
  const ext = filePath.substring(dotIdx);
  const lang = getLanguage(ext);
  if (!lang) return null;
  const langName = getLangName(ext);

  const parser = new Parser();
  parser.setLanguage(lang);
  const tree = parser.parse(source);

  const definitions = [];
  const references = [];

  const defQueryStr = DEF_QUERIES[langName] || null;
  if (defQueryStr) {
    try {
      const defQuery = new Parser.Query(lang, defQueryStr);
      for (const match of defQuery.matches(tree.rootNode)) {
        const nameCapture = match.captures.find(c => c.name === 'name');
        const defCapture = match.captures.find(c => c.name === 'definition');
        if (!nameCapture) continue;
        const defNode = defCapture || nameCapture;

        definitions.push({
          name: nameCapture.node.text,
          kind: nodeKind(defNode.node.type),
          file: filePath,
          lineStart: defNode.node.startPosition.row + 1,
          lineEnd: defNode.node.endPosition.row + 1,
          signature: extractSignature(defNode.node, langName),
        });
      }
    } catch (e) {
      // Query may fail on some grammar versions; degrade gracefully
    }
  }

  const refQueryStr = REF_QUERIES[langName] || REF_QUERIES.default;
  if (refQueryStr) {
    try {
      const refQuery = new Parser.Query(lang, refQueryStr);
      for (const match of refQuery.matches(tree.rootNode)) {
        const refCapture = match.captures.find(c => c.name === 'ref');
        if (!refCapture) continue;
        references.push({
          name: refCapture.node.text,
          file: filePath,
          line: refCapture.node.startPosition.row + 1,
        });
      }
    } catch (e) {
      // Degrade gracefully
    }
  }

  // No tree.delete()/parser.delete() needed — native GC handles cleanup

  return { file: filePath, definitions, references };
}

export { parseFile, SUPPORTED_EXTENSIONS, LANG_MAP };
