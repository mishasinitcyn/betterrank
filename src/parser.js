import { createRequire } from 'module';
const require = createRequire(import.meta.url);

// Lazy-loaded tree-sitter core + grammars so warm queries that don't parse
// any files pay zero native module load cost.
let _Parser = null;
function getParserClass() {
  if (!_Parser) _Parser = require('tree-sitter');
  return _Parser;
}

const GRAMMAR_LOADERS = {
  javascript: () => require('tree-sitter-javascript'),
  typescript: () => require('tree-sitter-typescript').typescript,
  tsx: () => require('tree-sitter-typescript').tsx,
  python: () => require('tree-sitter-python'),
  rust: () => require('tree-sitter-rust'),
  go: () => require('tree-sitter-go'),
  ruby: () => require('tree-sitter-ruby'),
  java: () => require('tree-sitter-java'),
  c: () => require('tree-sitter-c'),
  cpp: () => require('tree-sitter-cpp'),
  c_sharp: () => require('tree-sitter-c-sharp'),
  php: () => {
    const mod = require('tree-sitter-php');
    return mod.php || mod;
  },
};

const CORE_LANGUAGES = new Set(['javascript', 'typescript', 'tsx', 'python']);
const OPT_GRAMMAR_PACKAGES = {
  rust: 'tree-sitter-rust',
  go: 'tree-sitter-go',
  ruby: 'tree-sitter-ruby',
  java: 'tree-sitter-java',
  c: 'tree-sitter-c',
  cpp: 'tree-sitter-cpp',
  c_sharp: 'tree-sitter-c-sharp',
  php: 'tree-sitter-php',
};

function isPackageAvailable(pkg) {
  try {
    require.resolve(pkg);
    return true;
  } catch {
    return false;
  }
}

const AVAILABLE_LANGUAGES = new Set(CORE_LANGUAGES);
for (const [lang, pkg] of Object.entries(OPT_GRAMMAR_PACKAGES)) {
  if (isPackageAvailable(pkg)) AVAILABLE_LANGUAGES.add(lang);
}

const loadedGrammars = new Map();
const parserCache = new Map();
const queryCache = new Map();

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
const SUPPORTED_EXTENSIONS = Object.keys(LANG_MAP).filter(ext =>
  AVAILABLE_LANGUAGES.has(LANG_MAP[ext]),
);

function getLanguage(ext) {
  const langName = LANG_MAP[ext];
  if (!langName || !AVAILABLE_LANGUAGES.has(langName)) return null;
  if (loadedGrammars.has(langName)) return loadedGrammars.get(langName);
  try {
    const grammar = GRAMMAR_LOADERS[langName]();
    loadedGrammars.set(langName, grammar);
    return grammar;
  } catch {
    loadedGrammars.set(langName, null);
    return null;
  }
}

function getLangName(ext) {
  return LANG_MAP[ext] || null;
}

function getCachedParser(langName, lang) {
  let parser = parserCache.get(langName);
  if (!parser) {
    const Parser = getParserClass();
    parser = new Parser();
    parser.setLanguage(lang);
    parserCache.set(langName, parser);
  }
  return parser;
}

function getCachedQuery(langName, kind, lang, queryStr) {
  const key = `${langName}:${kind}`;
  if (queryCache.has(key)) return queryCache.get(key);
  try {
    const Parser = getParserClass();
    const query = new Parser.Query(lang, queryStr);
    queryCache.set(key, query);
    return query;
  } catch {
    queryCache.set(key, null);
    return null;
  }
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

DEF_QUERIES.tsx = DEF_QUERIES.typescript;

const REF_QUERIES = {
  javascript: `
    (call_expression function: (identifier) @ref)
    (call_expression function: (member_expression property: (property_identifier) @method_ref))
    (import_specifier name: (identifier) @ref)
    (import_clause (identifier) @ref)
    (member_expression property: (property_identifier) @prop_ref)
    (jsx_opening_element (identifier) @jsx_ref)
    (jsx_self_closing_element (identifier) @jsx_ref)
    (jsx_opening_element (member_expression (identifier) @jsx_ref))
    (jsx_self_closing_element (member_expression (identifier) @jsx_ref))
  `,

  typescript: `
    (call_expression function: (identifier) @ref)
    (call_expression function: (member_expression property: (property_identifier) @method_ref))
    (import_specifier name: (identifier) @ref)
    (import_clause (identifier) @ref)
    (member_expression property: (property_identifier) @prop_ref)
    (type_identifier) @ref
  `,

  tsx: `
    (call_expression function: (identifier) @ref)
    (call_expression function: (member_expression property: (property_identifier) @method_ref))
    (import_specifier name: (identifier) @ref)
    (import_clause (identifier) @ref)
    (member_expression property: (property_identifier) @prop_ref)
    (type_identifier) @ref
    (jsx_opening_element (identifier) @jsx_ref)
    (jsx_self_closing_element (identifier) @jsx_ref)
    (jsx_opening_element (member_expression (identifier) @jsx_ref))
    (jsx_self_closing_element (member_expression (identifier) @jsx_ref))
  `,

  python: `
    (call function: (identifier) @ref)
    (call function: (attribute attribute: (identifier) @method_ref))
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
  method_definition: 'method',
  method_declaration: 'method',
  function_item: 'function',
  singleton_method: 'method',
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

const FUNCTION_ANCESTOR_TYPES = new Set([
  'function_declaration',
  'function_expression',
  'arrow_function',
  'method_definition',
  'method_declaration',
  'function_definition',
  'function_item',
  'singleton_method',
]);

const CLASS_ANCESTOR_TYPES = new Set([
  'class_declaration',
  'class_definition',
  'class_specifier',
  'impl_item',
  'class',
]);

const LITERAL_VALUE_TYPES = new Set([
  'string',
  'template_string',
  'number',
  'true',
  'false',
  'null',
  'undefined',
  'regex',
]);

const OUTLINE_EXTRA_LANGUAGES = new Set(['javascript', 'typescript', 'tsx']);
const INDEX_EXTRA_LANGUAGES = new Set(['javascript', 'typescript', 'tsx']);
const OUTLINE_BODY_TYPES = new Set(['statement_block', 'class_body', 'object', 'array']);
const OUTLINE_CALL_TYPES = new Set(['call_expression', 'new_expression']);
const SEMANTIC_CONTAINER_NAME_RE = /(router|routes|handlers|actions|reducers|selectors|queries|mutations|registry|registries|map|maps|config|configs|options|endpoints|procedures)$/i;
const NON_SEMANTIC_CONTAINER_NAME_RE = /(schema|shape|validator|payload|params?|props?|input|output|response|request|dto)$/i;
const NON_SEMANTIC_CALLEE_NAME_RE = /^(object|array|enum|union|literal|record|tuple|pick|omit|extend|merge|intersection|partial|strictObject|looseObject)$/i;

/**
 * Check whether a definition node is nested inside another function/method body
 * (e.g., local helper arrows like `const helper = () => ...` inside a function).
 */
function isNestedInsideFunction(node) {
  let cur = node.parent;
  while (cur) {
    if (FUNCTION_ANCESTOR_TYPES.has(cur.type)) return true;
    cur = cur.parent;
  }
  return false;
}

/**
 * Check whether a function node is directly inside a class body (e.g., Python methods).
 */
function isDirectClassMethod(node) {
  let cur = node.parent;
  while (cur) {
    if (FUNCTION_ANCESTOR_TYPES.has(cur.type)) return false;
    if (CLASS_ANCESTOR_TYPES.has(cur.type)) return true;
    cur = cur.parent;
  }
  return false;
}

/**
 * Extract parameter names from a function's tree-sitter node.
 * Used by compare.js; skipped during normal indexing for speed/footprint.
 */
function extractParamNames(node) {
  const params = [];
  const paramNodes = [];
  for (let i = 0; i < node.namedChildCount; i++) {
    const child = node.namedChild(i);
    if (child.type === 'parameters' || child.type === 'formal_parameters' ||
        child.type === 'parameter_list') {
      paramNodes.push(child);
    }
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

function resolveKind(node, explicitKind) {
  if (explicitKind) return explicitKind;
  let kind = KIND_MAP[node.type] || 'other';
  if (kind === 'function' && isDirectClassMethod(node)) {
    return 'method';
  }
  if (node.type === 'decorated_definition') {
    const inner = node.childForFieldName('definition');
    if (inner?.type === 'class_definition') return 'class';
    if (inner?.type === 'function_definition' && isDirectClassMethod(node)) return 'method';
  }
  return kind;
}

function createDefinition(
  name,
  node,
  filePath,
  langName,
  { bodyStartLine = null, kind, includeParamNames = false } = {},
) {
  const bodyNode = findBodyNode(node);
  const resolvedBodyStartLine = bodyStartLine ?? computeBodyStartLine(node, bodyNode);

  const def = {
    name,
    kind: resolveKind(node, kind),
    file: filePath,
    lineStart: node.startPosition.row + 1,
    lineEnd: node.endPosition.row + 1,
    signature: extractSignature(node, langName, bodyNode),
    bodyStartLine: resolvedBodyStartLine,
  };

  if (includeParamNames) {
    def.paramNames = extractParamNames(node);
  }

  return def;
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

function isTopLevelVariableDeclarator(node) {
  if (!node || node.type !== 'variable_declarator') return false;
  const decl = node.parent;
  if (!decl || decl.type !== 'lexical_declaration') return false;
  const container = decl.parent;
  return !!container && (container.type === 'program' || container.type === 'export_statement');
}

function extractDeclaratorName(node) {
  const nameNode = node?.childForFieldName('name');
  return nameNode?.type === 'identifier' ? nameNode.text : null;
}

function extractCalleeLeafName(node) {
  if (!node) return null;

  if (['identifier', 'property_identifier', 'private_property_identifier', 'type_identifier'].includes(node.type)) {
    return node.text;
  }

  if (node.type === 'member_expression') {
    const propertyNode = node.childForFieldName('property');
    if (propertyNode) return extractCalleeLeafName(propertyNode);
    return extractCalleeLeafName(node.namedChild(node.namedChildCount - 1));
  }

  return null;
}

function extractCallLikeCalleeName(node) {
  if (!node || (node.type !== 'call_expression' && node.type !== 'new_expression')) return null;

  const calleeNode = node.childForFieldName('function')
    || node.childForFieldName('constructor')
    || node.namedChild(0);
  return extractCalleeLeafName(calleeNode);
}

function shouldIndexSemanticObjectMembers(node) {
  if (!isTopLevelVariableDeclarator(node)) return false;

  const variableName = extractDeclaratorName(node);
  const valueNode = node.childForFieldName('value');
  const calleeName = extractCallLikeCalleeName(valueNode);

  const hasSemanticVariableName = !!variableName
    && SEMANTIC_CONTAINER_NAME_RE.test(variableName)
    && !NON_SEMANTIC_CONTAINER_NAME_RE.test(variableName);
  const hasSemanticCalleeName = !!calleeName && !NON_SEMANTIC_CALLEE_NAME_RE.test(calleeName);

  if (valueNode?.type === 'object') {
    return hasSemanticVariableName;
  }

  if (valueNode?.type === 'call_expression' || valueNode?.type === 'new_expression') {
    return hasSemanticVariableName || hasSemanticCalleeName;
  }

  return false;
}

function collectDirectPairDefinitions(node, filePath, langName, opts = {}) {
  if (!node || node.type !== 'object') return [];

  const definitions = [];
  for (let i = 0; i < node.namedChildCount; i++) {
    const child = node.namedChild(i);
    if (child.type !== 'pair') continue;

    const name = extractPairName(child);
    if (!name) continue;
    if (child.endPosition.row <= child.startPosition.row) continue;

    // Skip plain literal entries (e.g. query string maps like REF_QUERIES = { default: `...` })
    const valueNode = child.childForFieldName('value');
    if (valueNode && LITERAL_VALUE_TYPES.has(valueNode.type)) continue;

    definitions.push(createDefinition(name, child, filePath, langName, {
      bodyStartLine: child.startPosition.row + 2,
      kind: 'property',
      includeParamNames: opts.includeParamNames,
    }));
  }

  return definitions;
}

function collectIndexDefinitions(rootNode, filePath, langName, opts = {}) {
  if (!INDEX_EXTRA_LANGUAGES.has(langName)) return [];

  const definitions = [];

  function visit(node) {
    if (node.type === 'variable_declarator' && shouldIndexSemanticObjectMembers(node)) {
      const bodyNode = findBodyNode(node);
      if (bodyNode && bodyNode.type === 'object') {
        definitions.push(...collectDirectPairDefinitions(bodyNode, filePath, langName, opts));
      }
      return;
    }

    for (let i = 0; i < node.namedChildCount; i++) {
      visit(node.namedChild(i));
    }
  }

  visit(rootNode);
  return definitions;
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

function extractSignature(node, langName, bodyNode = null) {
  const text = node.text;
  if (langName === 'python') {
    const parenClose = text.indexOf(')');
    let colonIdx;
    if (parenClose !== -1) {
      colonIdx = text.indexOf(':', parenClose + 1);
    } else {
      colonIdx = text.indexOf(':');
    }
    if (colonIdx !== -1) {
      const sig = text.substring(0, colonIdx + 1).replace(/\s*\n\s*/g, ' ').trim();
      return sig.length > 300 ? sig.substring(0, 300) + '...' : sig;
    }
    const firstLine = text.split('\n')[0];
    return firstLine.trim();
  }

  // Use tree-sitter's bodyNode start offset for functions/methods/classes so
  // destructured parameters like `function foo({ a, b }) {` aren't truncated at `{`.
  if (
    bodyNode &&
    (bodyNode.type === 'statement_block' || bodyNode.type === 'class_body' || bodyNode.type === 'block') &&
    bodyNode.startIndex > node.startIndex
  ) {
    const relEnd = bodyNode.startIndex - node.startIndex;
    const sig = text.substring(0, relEnd).replace(/\s*\n\s*/g, ' ').trim();
    return sig.length > 200 ? sig.substring(0, 200) + '...' : sig;
  }

  const newlineIdx = text.indexOf('\n');
  let line = (newlineIdx === -1 ? text : text.substring(0, newlineIdx)).trim();
  if (line.endsWith('{')) line = line.slice(0, -1).trimEnd();
  return line.length > 200 ? line.substring(0, 200) + '...' : line;
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

function createImportEntry(path, filePath, node) {
  if (!path || !filePath || !node) return null;
  return {
    path,
    file: filePath,
    line: node.startPosition.row + 1,
  };
}

function collectPythonImports(rootNode, filePath) {
  const imports = [];

  function push(path, node) {
    const entry = createImportEntry(path, filePath, node);
    if (entry) imports.push(entry);
  }

  function visit(node) {
    if (node.type === 'import_statement') {
      for (let i = 0; i < node.namedChildCount; i++) {
        const child = node.namedChild(i);
        if (child.type === 'dotted_name') {
          push(child.text, child);
          continue;
        }
        if (child.type === 'aliased_import') {
          const moduleNode = child.namedChild(0);
          if (moduleNode?.type === 'dotted_name') {
            push(moduleNode.text, moduleNode);
          }
        }
      }
    } else if (node.type === 'import_from_statement') {
      const moduleNode = node.childForFieldName('module_name');
      if (moduleNode) push(moduleNode.text, moduleNode);
    }

    for (let i = 0; i < node.namedChildCount; i++) {
      visit(node.namedChild(i));
    }
  }

  visit(rootNode);
  return imports;
}

/**
 * Parse a single source file and extract definitions, references, and imports.
 * Returns null if the language is unsupported.
 */
function parseFile(
  filePath,
  source,
  {
    includeOutlineDefinitions = false,
    includeLocalRefs = false,
    includeParamNames = false,
  } = {},
) {
  const dotIdx = filePath.lastIndexOf('.');
  if (dotIdx === -1) return null;
  const ext = filePath.substring(dotIdx);
  const lang = getLanguage(ext);
  if (!lang) return null;
  const langName = getLangName(ext);

  const parser = getCachedParser(langName, lang);
  const tree = parser.parse(source);

  const definitions = [];
  const references = [];
  const imports = [];

  const defQueryStr = DEF_QUERIES[langName] || null;
  if (defQueryStr) {
    const defQuery = getCachedQuery(langName, 'def', lang, defQueryStr);
    if (defQuery) {
      const seenDefKeys = new Set();
      for (const match of defQuery.matches(tree.rootNode)) {
        const nameCapture = match.captures.find(c => c.name === 'name');
        const defCapture = match.captures.find(c => c.name === 'definition');
        if (!nameCapture) continue;
        const defNode = (defCapture || nameCapture).node;

        // Skip local nested helper functions/arrows inside other function bodies
        // when indexing the repo graph (keep them available for outline expansion).
        if (!includeOutlineDefinitions && isNestedInsideFunction(defNode)) continue;

        // Deduplicate overlapping captures (e.g. export_statement + inner function_declaration)
        const dedupKey = `${nameCapture.node.text}:${defNode.startPosition.row}`;
        if (seenDefKeys.has(dedupKey)) continue;
        seenDefKeys.add(dedupKey);

        definitions.push(
          createDefinition(nameCapture.node.text, defNode, filePath, langName, {
            includeParamNames,
          }),
        );
      }
    }
  }

  definitions.push(
    ...collectIndexDefinitions(tree.rootNode, filePath, langName, { includeParamNames }),
  );

  if (includeOutlineDefinitions) {
    definitions.push(...collectOutlineDefinitions(tree.rootNode, filePath, langName));
  }

  // Skip reference & import queries when only generating a file outline
  if (!includeOutlineDefinitions) {
    const refQueryStr = REF_QUERIES[langName] || REF_QUERIES.default;
    if (refQueryStr) {
      const refQuery = getCachedQuery(langName, 'ref', lang, refQueryStr);
      if (refQuery) {
        for (const match of refQuery.matches(tree.rootNode)) {
          for (const capture of match.captures) {
            if (
              capture.name !== 'ref' &&
              capture.name !== 'jsx_ref' &&
              capture.name !== 'prop_ref' &&
              capture.name !== 'method_ref'
            ) continue;
            const name = normalizeReferenceCapture(capture);
            if (!name) continue;
            const kind = capture.name === 'method_ref'
              ? 'method'
              : capture.name === 'prop_ref'
                ? 'property'
                : 'symbol';
            references.push({
              name,
              kind,
              file: filePath,
              line: capture.node.startPosition.row + 1,
            });
          }
        }
      }
    }

    const importQueryStr = IMPORT_QUERIES[langName] || null;
    if (importQueryStr) {
      const importQuery = getCachedQuery(langName, 'import', lang, importQueryStr);
      if (importQuery) {
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
      }
    }

    if (langName === 'python') {
      imports.push(...collectPythonImports(tree.rootNode, filePath));
    }
  }

  if (includeLocalRefs && references.length > 0 && definitions.length > 0) {
    const sortedDefs = [...definitions].sort((a, b) => a.lineStart - b.lineStart);
    const localRefSets = new Map();
    for (const ref of references) {
      let enclosing = null;
      for (const def of sortedDefs) {
        if (ref.line >= def.lineStart && ref.line <= def.lineEnd) {
          enclosing = def;
        }
      }
      if (enclosing) {
        let set = localRefSets.get(enclosing);
        if (!set) {
          set = new Set();
          localRefSets.set(enclosing, set);
        }
        set.add(ref.name);
      }
    }
    for (const [def, set] of localRefSets) {
      def.localRefs = [...set];
    }
  }

  return { file: filePath, definitions, references, imports };
}

export { parseFile, extractParamNames, SUPPORTED_EXTENSIONS, LANG_MAP };
