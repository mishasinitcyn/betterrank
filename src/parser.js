import { createRequire } from 'module';
const require = createRequire(import.meta.url);

const Parser = require('tree-sitter');

function tryRequire(mod) {
  try { return require(mod); } catch { return null; }
}

// Core grammars (always installed)
const tsGrammars = require('tree-sitter-typescript');

const GRAMMARS = {
  javascript: require('tree-sitter-javascript'),
  typescript: tsGrammars.typescript,
  tsx: tsGrammars.tsx,
  python: require('tree-sitter-python'),
};

// Optional grammars (install individually if needed)
const optGrammars = [
  ['rust', 'tree-sitter-rust'],
  ['go', 'tree-sitter-go'],
  ['ruby', 'tree-sitter-ruby'],
  ['java', 'tree-sitter-java'],
  ['c', 'tree-sitter-c'],
  ['cpp', 'tree-sitter-cpp'],
  ['c_sharp', 'tree-sitter-c-sharp'],
];
for (const [name, mod] of optGrammars) {
  const g = tryRequire(mod);
  if (g) GRAMMARS[name] = g;
}
const phpModule = tryRequire('tree-sitter-php');
if (phpModule) GRAMMARS.php = phpModule.php || phpModule;

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

// Only include extensions for grammars that are actually installed
const SUPPORTED_EXTENSIONS = Object.keys(LANG_MAP).filter(ext => {
  const lang = LANG_MAP[ext];
  return GRAMMARS[lang] != null;
});

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
    (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name value: (function_expression) @_val)) @definition)
    (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name value: (call_expression) @_val)) @definition)
    (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name value: (new_expression) @_val)) @definition)
    (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name value: (object) @_val)) @definition)
    (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name value: (array) @_val)) @definition)
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
    (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name value: (function_expression) @_val)) @definition)
    (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name value: (call_expression) @_val)) @definition)
    (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name value: (new_expression) @_val)) @definition)
    (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name value: (object) @_val)) @definition)
    (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name value: (array) @_val)) @definition)
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
    (jsx_opening_element (identifier) @jsx_ref)
    (jsx_self_closing_element (identifier) @jsx_ref)
    (jsx_opening_element (member_expression (identifier) @jsx_ref))
    (jsx_self_closing_element (member_expression (identifier) @jsx_ref))
  `,

  typescript: `
    (call_expression function: (identifier) @ref)
    (import_specifier name: (identifier) @ref)
    (import_clause (identifier) @ref)
    (type_identifier) @ref
  `,

  tsx: `
    (call_expression function: (identifier) @ref)
    (import_specifier name: (identifier) @ref)
    (import_clause (identifier) @ref)
    (type_identifier) @ref
    (jsx_opening_element (identifier) @jsx_ref)
    (jsx_self_closing_element (identifier) @jsx_ref)
    (jsx_opening_element (member_expression (identifier) @jsx_ref))
    (jsx_self_closing_element (member_expression (identifier) @jsx_ref))
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

const IMPORT_QUERIES = {
  javascript: `
    (import_statement source: (string) @import_path)
    (export_statement source: (string) @import_path)
    (call_expression function: (identifier) @require_fn arguments: (arguments (string) @import_path))
  `,

  typescript: `
    (import_statement source: (string) @import_path)
    (export_statement source: (string) @import_path)
    (call_expression function: (identifier) @require_fn arguments: (arguments (string) @import_path))
  `,
};

IMPORT_QUERIES.tsx = IMPORT_QUERIES.typescript;

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
  pair: 'property',
};

const OUTLINE_EXTRA_LANGUAGES = new Set(['javascript', 'typescript', 'tsx']);
const OUTLINE_BODY_TYPES = new Set(['statement_block', 'class_body', 'object', 'array']);
const OUTLINE_CALL_TYPES = new Set(['call_expression', 'new_expression']);

/**
 * Walk an AST subtree and count node types that reveal structural shape.
 * Returns a flat object like { if_statement: 3, for_statement: 1, call_expression: 7, ... }
 * This is intentionally coarse — we want "shape" not identity.
 */
const STRUCTURAL_NODE_TYPES = new Set([
  // Control flow
  'if_statement', 'if_expression', 'elif_clause', 'else_clause',
  'for_statement', 'for_in_statement', 'for_expression',
  'while_statement', 'loop_expression',
  'match_statement', 'match_expression', 'switch_statement', 'case_clause',
  'try_statement', 'try_expression', 'except_clause', 'catch_clause', 'finally_clause',
  'with_statement',
  // Returns / yields
  'return_statement', 'yield', 'yield_expression', 'await_expression',
  // Calls & access
  'call_expression', 'call', 'method_call_expression',
  'member_expression', 'attribute', 'subscript_expression', 'subscript',
  // Assignments
  'assignment', 'assignment_expression', 'augmented_assignment',
  // Data structures
  'list', 'list_comprehension', 'dictionary', 'dictionary_comprehension',
  'array', 'object', 'tuple',
  // Assertions / raises
  'assert_statement', 'raise_statement', 'throw_statement',
  // Boolean logic
  'boolean_operator', 'binary_expression', 'comparison_operator', 'not_operator',
  // Conditionals
  'conditional_expression', 'ternary_expression',
  // String operations
  'string', 'f_string', 'template_string',
  // Decorators
  'decorator',
]);

function buildAstProfile(node) {
  const profile = {};
  let totalNodes = 0;

  function walk(n) {
    if (STRUCTURAL_NODE_TYPES.has(n.type)) {
      profile[n.type] = (profile[n.type] || 0) + 1;
    }
    totalNodes++;
    for (let i = 0; i < n.namedChildCount; i++) {
      walk(n.namedChild(i));
    }
  }

  walk(node);
  profile._totalNodes = totalNodes;
  return profile;
}

/**
 * Extract parameter names from a function's tree-sitter node.
 * Works across languages by looking for common parameter node patterns.
 */
function extractParamNames(node) {
  const params = [];
  // Find the parameter list node
  const paramNodes = [];
  for (let i = 0; i < node.namedChildCount; i++) {
    const child = node.namedChild(i);
    if (child.type === 'parameters' || child.type === 'formal_parameters' ||
        child.type === 'parameter_list') {
      paramNodes.push(child);
    }
    // Drill into wrappers (e.g. variable_declarator -> arrow_function)
    for (let j = 0; j < child.namedChildCount; j++) {
      const gc = child.namedChild(j);
      if (gc.type === 'parameters' || gc.type === 'formal_parameters' ||
          gc.type === 'parameter_list') {
        paramNodes.push(gc);
      }
    }
  }

  for (const paramList of paramNodes) {
    for (let i = 0; i < paramList.namedChildCount; i++) {
      const p = paramList.namedChild(i);
      // Try to get the identifier name from various param shapes
      const nameNode = p.childForFieldName('name') || p.childForFieldName('pattern');
      if (nameNode && nameNode.type === 'identifier') {
        params.push(nameNode.text);
      } else if (p.type === 'identifier') {
        params.push(p.text);
      }
    }
  }
  return params;
}

/**
 * Find the body/block node of a definition, drilling into wrappers like
 * lexical_declaration → variable_declarator → arrow_function → body.
 */
function findCallArgumentBody(node, depth) {
  if (!node) return null;
  let fallback = null;

  for (let i = 0; i < node.namedChildCount; i++) {
    const child = node.namedChild(i);
    const body = findBodyNode(child, depth + 1);
    if (!body) continue;
    if (body.type === 'statement_block' || body.type === 'class_body') {
      return body;
    }
    if (!fallback) fallback = body;
  }

  return fallback;
}

function findBodyNode(node, depth = 0) {
  if (!node || depth > 12) return null;
  if (OUTLINE_BODY_TYPES.has(node.type)) return node;

  const body = node.childForFieldName('body');
  if (body) {
    const nestedBody = findBodyNode(body, depth + 1);
    return nestedBody || body;
  }

  if (OUTLINE_CALL_TYPES.has(node.type)) {
    const argBody = findCallArgumentBody(node.childForFieldName('arguments'), depth);
    if (argBody) return argBody;
  }

  for (const fieldName of ['value', 'expression', 'argument']) {
    const child = node.childForFieldName(fieldName);
    if (!child) continue;
    const nestedBody = findBodyNode(child, depth + 1);
    if (nestedBody) return nestedBody;
  }

  for (let i = 0; i < node.namedChildCount; i++) {
    const nestedBody = findBodyNode(node.namedChild(i), depth + 1);
    if (nestedBody) return nestedBody;
  }

  return null;
}

function computeBodyStartLine(node, bodyNode) {
  if (!bodyNode) return null;

  const bodyRow = bodyNode.startPosition.row;
  const defRow = node.startPosition.row;
  return bodyRow === defRow ? bodyRow + 2 : bodyRow + 1;
}

function createDefinition(name, node, filePath, langName, { bodyStartLine = null, kind } = {}) {
  const bodyNode = bodyStartLine == null ? findBodyNode(node) : null;
  const resolvedBodyStartLine = bodyStartLine ?? computeBodyStartLine(node, bodyNode);
  const profileNode = bodyNode || node;

  return {
    name,
    kind: kind || nodeKind(node.type),
    file: filePath,
    lineStart: node.startPosition.row + 1,
    lineEnd: node.endPosition.row + 1,
    signature: extractSignature(node, langName),
    bodyStartLine: resolvedBodyStartLine,
    astProfile: buildAstProfile(profileNode),
    paramNames: extractParamNames(node),
  };
}

function extractPairName(node) {
  const keyNode = node.childForFieldName('key') || node.namedChild(0);
  if (!keyNode) return null;

  if (['property_identifier', 'identifier', 'private_property_identifier', 'number'].includes(keyNode.type)) {
    return keyNode.text;
  }

  if (keyNode.type === 'string') {
    return keyNode.text.replace(/^['"`]|['"`]$/g, '');
  }

  return null;
}

function hasMultilinePairChildren(node) {
  if (!node || node.type !== 'object') return false;

  for (let i = 0; i < node.namedChildCount; i++) {
    const child = node.namedChild(i);
    if (child.type !== 'pair') continue;
    if (!extractPairName(child)) continue;
    if (child.endPosition.row > child.startPosition.row) return true;
  }

  return false;
}

function collectOutlineDefinitions(rootNode, filePath, langName) {
  if (!OUTLINE_EXTRA_LANGUAGES.has(langName)) return [];

  const definitions = [];

  function visit(node) {
    if (node.type === 'pair') {
      const name = extractPairName(node);
      if (name && node.endPosition.row > node.startPosition.row) {
        definitions.push(createDefinition(name, node, filePath, langName, {
          bodyStartLine: node.startPosition.row + 2,
          kind: 'property',
        }));
      }
    } else if (node.type === 'variable_declarator') {
      const nameNode = node.childForFieldName('name');
      const bodyNode = findBodyNode(node);
      if (
        nameNode &&
        nameNode.type === 'identifier' &&
        node.endPosition.row > node.startPosition.row &&
        !(bodyNode && bodyNode.type === 'object' && hasMultilinePairChildren(bodyNode))
      ) {
        definitions.push(createDefinition(nameNode.text, node, filePath, langName, {
          bodyStartLine: node.startPosition.row + 2,
          kind: 'variable',
        }));
      }
    }

    for (let i = 0; i < node.namedChildCount; i++) {
      visit(node.namedChild(i));
    }
  }

  visit(rootNode);
  return definitions;
}

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

function normalizeReferenceCapture(capture) {
  if (!capture) return null;
  const text = capture.node.text;
  if (!text) return null;

  if (capture.name === 'jsx_ref') {
    return /^[A-Z]/.test(text) ? text : null;
  }

  return text;
}

function normalizeImportPathCapture(capture) {
  if (!capture) return null;
  const text = capture.node.text;
  if (!text) return null;

  return text.replace(/^['"`]|['"`]$/g, '');
}

/**
 * Parse a single source file and extract definitions, references, and imports.
 * Returns null if the language is unsupported.
 */
function parseFile(filePath, source, { includeOutlineDefinitions = false } = {}) {
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
  const imports = [];

  const defQueryStr = DEF_QUERIES[langName] || null;
  if (defQueryStr) {
    try {
      const defQuery = new Parser.Query(lang, defQueryStr);
      for (const match of defQuery.matches(tree.rootNode)) {
        const nameCapture = match.captures.find(c => c.name === 'name');
        const defCapture = match.captures.find(c => c.name === 'definition');
        if (!nameCapture) continue;
        const defNode = defCapture || nameCapture;

        definitions.push(createDefinition(nameCapture.node.text, defNode.node, filePath, langName));
      }
    } catch (e) {
      // Query may fail on some grammar versions; degrade gracefully
    }
  }

  if (includeOutlineDefinitions) {
    definitions.push(...collectOutlineDefinitions(tree.rootNode, filePath, langName));
  }

  const refQueryStr = REF_QUERIES[langName] || REF_QUERIES.default;
  if (refQueryStr) {
    try {
      const refQuery = new Parser.Query(lang, refQueryStr);
      for (const match of refQuery.matches(tree.rootNode)) {
        for (const capture of match.captures) {
          if (capture.name !== 'ref' && capture.name !== 'jsx_ref') continue;
          const name = normalizeReferenceCapture(capture);
          if (!name) continue;
          references.push({
            name,
            file: filePath,
            line: capture.node.startPosition.row + 1,
          });
        }
      }
    } catch (e) {
      // Degrade gracefully
    }
  }

  const importQueryStr = IMPORT_QUERIES[langName] || null;
  if (importQueryStr) {
    try {
      const importQuery = new Parser.Query(lang, importQueryStr);
      for (const match of importQuery.matches(tree.rootNode)) {
        const requireCapture = match.captures.find(c => c.name === 'require_fn');
        if (requireCapture && requireCapture.node.text !== 'require') continue;

        const pathCapture = match.captures.find(c => c.name === 'import_path');
        const path = normalizeImportPathCapture(pathCapture);
        if (!path) continue;

        imports.push({
          path,
          file: filePath,
          line: pathCapture.node.startPosition.row + 1,
        });
      }
    } catch (e) {
      // Degrade gracefully
    }
  }

  // Associate each reference with its enclosing definition (by line range).
  // This gives us per-function reference sets for similarity analysis.
  // Sort definitions by lineStart for binary search.
  const sortedDefs = [...definitions].sort((a, b) => a.lineStart - b.lineStart);
  for (const ref of references) {
    // Find the innermost enclosing definition
    let enclosing = null;
    for (const def of sortedDefs) {
      if (ref.line >= def.lineStart && ref.line <= def.lineEnd) {
        // Pick innermost (last matching, since sorted by start and nested defs start later)
        enclosing = def;
      }
    }
    if (enclosing) {
      if (!enclosing.localRefs) enclosing.localRefs = [];
      enclosing.localRefs.push(ref.name);
    }
  }

  // No tree.delete()/parser.delete() needed — native GC handles cleanup

  return { file: filePath, definitions, references, imports };
}

export { parseFile, buildAstProfile, extractParamNames, SUPPORTED_EXTENSIONS, LANG_MAP };
