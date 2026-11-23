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

function getLanguageFamily(filePath) {
  const dotIdx = filePath.lastIndexOf('.');
  if (dotIdx === -1) return 'other';
  const lang = LANG_MAP[filePath.substring(dotIdx)];
  if (!lang) return 'other';
  if (lang === 'javascript' || lang === 'typescript' || lang === 'tsx') return 'js';
  if (lang === 'c' || lang === 'cpp') return 'c';
  return lang;
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
    (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name)) @definition)
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
    (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name)) @definition)
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
    (call_expression function: (member_expression object: (_) @method_recv property: (property_identifier) @method_ref))
    (import_specifier name: (identifier) @import_ref)
    (import_clause (identifier) @import_ref)
    (export_specifier name: (identifier) @import_ref)
    (member_expression object: (_) @prop_recv property: (property_identifier) @prop_ref)
    (jsx_opening_element (identifier) @jsx_ref)
    (jsx_self_closing_element (identifier) @jsx_ref)
    (jsx_opening_element (member_expression (identifier) @jsx_ref))
    (jsx_self_closing_element (member_expression (identifier) @jsx_ref))
  `,

  typescript: `
    (call_expression function: (identifier) @ref)
    (call_expression function: (member_expression object: (_) @method_recv property: (property_identifier) @method_ref))
    (import_specifier name: (identifier) @import_ref)
    (import_clause (identifier) @import_ref)
    (export_specifier name: (identifier) @import_ref)
    (member_expression object: (_) @prop_recv property: (property_identifier) @prop_ref)
    (type_identifier) @ref
  `,

  tsx: `
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
  `,

  python: `
    (call function: (identifier) @ref)
    (call function: (attribute object: (_) @method_recv attribute: (identifier) @method_ref))
    (attribute object: (_) @prop_recv attribute: (identifier) @prop_ref)
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
    (export_statement source: (string) @reexport_path)
    (call_expression function: (identifier) @require_fn arguments: (arguments (string) @import_path))
  `,

  typescript: `
    (import_statement source: (string) @import_path)
    (export_statement source: (string) @reexport_path)
    (call_expression function: (identifier) @require_fn arguments: (arguments (string) @import_path))
  `,

  python: `
    (import_statement) @py_import
    (import_from_statement) @py_import_from
  `,
};

IMPORT_QUERIES.tsx = IMPORT_QUERIES.typescript;

// Standard-library / builtin receivers whose methods should never wire to user code.
const STDLIB_RECEIVERS = new Set([
  // JS / TS builtins
  'Promise', 'Object', 'Array', 'Math', 'JSON', 'Number', 'String', 'Boolean',
  'Date', 'Map', 'Set', 'WeakMap', 'WeakSet', 'RegExp', 'Symbol', 'Reflect',
  'Proxy', 'Error', 'TypeError', 'RangeError', 'Buffer', 'Uint8Array', 'BigInt',
  'console', 'process', 'window', 'document', 'globalThis', 'URL', 'URLSearchParams',
  'Intl', 'Response', 'Request', 'Headers', 'FormData', 'AbortController', 'crypto',
  // Test frameworks
  'vi', 'jest', 'expect', 'assert', 'describe', 'it', 'test', 'pytest', 'unittest',
  // Python builtins / stdlib
  'dict', 'list', 'set', 'tuple', 'str', 'int', 'float', 'bool', 'bytes',
  'os', 'sys', 're', 'json', 'math', 'time', 'datetime', 'pathlib', 'Path',
  'logging', 'logger', 'typing', 'collections', 'itertools', 'functools',
  'asyncio', 'subprocess', 'shutil', 'tempfile', 'copy', 'dataclasses', 'enum',
  'hashlib', 'hmac', 'base64', 'uuid', 'urllib', 'io', 'csv', 'struct', 'random',
]);

// Ubiquitous built-in / runtime method names that should not be cross-wired
// via untyped `obj.method()` member access.
const METHOD_NOISE_NAMES = new Set([
  'constructor', 'toString', 'valueOf', 'toJSON', 'hasOwnProperty',
  'map', 'filter', 'reduce', 'forEach', 'flat', 'flatMap', 'some', 'every',
  'find', 'findIndex', 'includes', 'indexOf', 'lastIndexOf', 'push', 'pop',
  'shift', 'unshift', 'slice', 'splice', 'concat', 'join', 'split', 'trim',
  'trimStart', 'trimEnd', 'replace', 'replaceAll', 'match', 'matchAll',
  'startsWith', 'endsWith', 'toLowerCase', 'toUpperCase', 'substring', 'substr',
  'keys', 'values', 'entries', 'from', 'of', 'isArray', 'assign', 'freeze',
  'then', 'catch', 'finally', 'bind', 'call', 'apply', 'resolve', 'reject',
  'get', 'set', 'has', 'add', 'delete', 'clear', 'size', 'length', 'prototype',
  'log', 'warn', 'error', 'info', 'debug', 'trace',
  'json', 'text', 'blob', 'arrayBuffer', 'clone', 'headers', 'status',
  'all', 'first', 'run', 'exec', 'test', 'sort', 'reverse', 'fill',
  'emit', 'on', 'off', 'once', 'addEventListener', 'removeEventListener',
  'preventDefault', 'stopPropagation', 'stringify', 'parse', 'append', 'close',
  'open', 'start', 'stop', 'init', 'copy', 'setdefault', 'update', 'items',
  '__init__', '__repr__', '__str__', '__eq__', '__hash__', '__len__',
  '__iter__', '__next__', '__enter__', '__exit__', '__getitem__', '__setitem__',
]);

const RPC_TERMINAL_METHODS = new Set([
  'useQuery', 'useMutation', 'useInfiniteQuery', 'useSuspenseQuery',
  'useSuspenseInfiniteQuery', 'useQueries', 'useUtils', 'useSubscription',
  'invalidate', 'fetch', 'prefetch', 'cancel', 'getData', 'setData',
  'getInfiniteData', 'setInfiniteData', 'ensureData', 'query', 'mutate',
  'subscribe',
]);

const RPC_ROOT_RECEIVERS = new Set(['api', 'trpc', 'rpc', 'utils']);

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

const JS_LANGS = new Set(['javascript', 'typescript', 'tsx']);
const OUTLINE_BODY_TYPES = new Set(['statement_block', 'class_body', 'object', 'array']);
const OUTLINE_CALL_TYPES = new Set(['call_expression', 'new_expression']);
const SEMANTIC_CONTAINER_NAME_RE = /(router|routes|handlers|actions|reducers|selectors|queries|mutations|registry|registries|map|maps|config|configs|options|endpoints|procedures)$/i;
const NON_SEMANTIC_CONTAINER_NAME_RE = /(schema|shape|validator|payload|params?|props?|input|output|response|request|dto|mock|fixture|stub|fake|test)$/i;
const NON_SEMANTIC_CALLEE_NAME_RE = /^(object|array|enum|union|literal|record|tuple|pick|omit|extend|merge|intersection|partial|strictObject|looseObject|createMock.*|mock.*|vi|jest)$/i;
const PY_CONSTANT_NAME_RE = /^[A-Z][A-Z0-9_]+$/;

function isNestedInsideFunction(node) {
  let cur = node.parent;
  while (cur) {
    if (FUNCTION_ANCESTOR_TYPES.has(cur.type)) return true;
    cur = cur.parent;
  }
  return false;
}

function isDirectClassMethod(node) {
  let cur = node.parent;
  while (cur) {
    if (FUNCTION_ANCESTOR_TYPES.has(cur.type)) return false;
    if (CLASS_ANCESTOR_TYPES.has(cur.type)) return true;
    cur = cur.parent;
  }
  return false;
}

function isNodeExported(node, langName) {
  if (!JS_LANGS.has(langName)) return true;
  let cur = node;
  while (cur) {
    if (cur.type === 'export_statement') return true;
    cur = cur.parent;
  }
  return false;
}

/**
 * Collect names re-exported at the bottom of a JS/TS file:
 *   export { foo, bar as baz }
 *   export default Foo
 *   module.exports = { foo, bar }
 */
function collectJsBottomExports(rootNode) {
  const exported = new Set();
  for (let i = 0; i < rootNode.namedChildCount; i++) {
    const stmt = rootNode.namedChild(i);
    if (stmt.type === 'export_statement') {
      // Skip `export ... from '...'` (re-export of another file)
      if (stmt.childForFieldName('source')) continue;
      for (let j = 0; j < stmt.namedChildCount; j++) {
        const ch = stmt.namedChild(j);
        if (ch.type === 'export_clause') {
          for (let k = 0; k < ch.namedChildCount; k++) {
            const spec = ch.namedChild(k);
            if (spec.type === 'export_specifier') {
              const nameNode = spec.childForFieldName('name') || spec.namedChild(0);
              if (nameNode) exported.add(nameNode.text);
            }
          }
        } else if (ch.type === 'identifier') {
          exported.add(ch.text);
        }
      }
    } else if (stmt.type === 'expression_statement') {
      const expr = stmt.namedChild(0);
      if (expr?.type === 'assignment_expression') {
        const left = expr.childForFieldName('left');
        const right = expr.childForFieldName('right');
        if (left?.text === 'module.exports' || left?.text?.startsWith('exports.')) {
          if (right?.type === 'identifier') {
            exported.add(right.text);
          } else if (right?.type === 'object') {
            for (let k = 0; k < right.namedChildCount; k++) {
              const p = right.namedChild(k);
              if (p.type === 'shorthand_property_identifier') exported.add(p.text);
              else if (p.type === 'pair') {
                const v = p.childForFieldName('value');
                if (v?.type === 'identifier') exported.add(v.text);
              }
            }
          }
        }
      }
    }
  }
  return exported;
}

/**
 * Collect JS/TS import aliases (`import { orig as local }`), imported symbol
 * module paths (`reviewRouter -> './review'`), namespace imports (`import * as m`),
 * and tRPC caller bindings (`const caller = createCallerFactory(router)`).
 */
function collectJsImportInfo(rootNode, source) {
  const symbolAliases = new Map(); // localName -> originalName
  const moduleAliases = new Map(); // localReceiver -> moduleSpecifier
  const importedSymSources = new Map(); // localSymName -> moduleSpecifier

  for (let i = 0; i < rootNode.namedChildCount; i++) {
    const stmt = rootNode.namedChild(i);
    if (stmt.type !== 'import_statement') continue;
    const sourceNode = stmt.childForFieldName('source');
    const specPath = sourceNode ? sourceNode.text.replace(/^['"`]|['"`]$/g, '') : null;

    for (let j = 0; j < stmt.namedChildCount; j++) {
      const clause = stmt.namedChild(j);
      if (clause.type !== 'import_clause') continue;
      for (let k = 0; k < clause.namedChildCount; k++) {
        const ch = clause.namedChild(k);
        if (ch.type === 'identifier' && specPath) {
          importedSymSources.set(ch.text, specPath);
        } else if (ch.type === 'namespace_import' && specPath) {
          const id = ch.namedChild(0);
          if (id?.type === 'identifier') moduleAliases.set(id.text, specPath);
        } else if (ch.type === 'named_imports') {
          for (let m = 0; m < ch.namedChildCount; m++) {
            const spec = ch.namedChild(m);
            if (spec.type !== 'import_specifier') continue;
            const orig = spec.childForFieldName('name');
            const alias = spec.childForFieldName('alias');
            if (orig && alias && orig.text !== alias.text) {
              symbolAliases.set(alias.text, orig.text);
              if (specPath) importedSymSources.set(alias.text, specPath);
            } else if (orig && specPath) {
              importedSymSources.set(orig.text, specPath);
            }
          }
        }
      }
    }
  }

  // Fast pre-checked binding pass for tRPC server/test callers:
  //   const caller = createCallerFactory(myRouter)(ctx)
  //   const createMyCaller = createCallerFactory(myRouter); const caller = createMyCaller(ctx)
  //   const caller = myRouter.createCaller(ctx)
  if (importedSymSources.size > 0 && source && source.includes('createCaller')) {
    const factoryToMod = new Map();
    const directRe = /\b(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*=\s*(?:createCallerFactory\s*\(\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*\)|([A-Za-z_$][A-Za-z0-9_$]*)\s*\.\s*createCaller\s*\()/g;
    let m;
    while ((m = directRe.exec(source)) !== null) {
      const varName = m[1];
      const routerName = m[2] || m[3];
      const modSpec = importedSymSources.get(routerName);
      if (!modSpec) continue;
      // Check whether this was invoked immediately `createCallerFactory(router)(ctx)` or stored as a factory
      const after = source.slice(directRe.lastIndex, directRe.lastIndex + 40).trimStart();
      if (m[2] && !after.startsWith('(')) {
        factoryToMod.set(varName, modSpec);
      } else {
        moduleAliases.set(varName, modSpec);
      }
    }
    if (factoryToMod.size > 0) {
      const callRe = /\b(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*=\s*([A-Za-z_$][A-Za-z0-9_$]*)\s*\(/g;
      while ((m = callRe.exec(source)) !== null) {
        const modSpec = factoryToMod.get(m[2]);
        if (modSpec) moduleAliases.set(m[1], modSpec);
      }
    }
  }

  return { symbolAliases, moduleAliases };
}

/**
 * Determine whether a JS/TS `@prop_ref` property_identifier belongs to a chained
 * RPC proxy expression (e.g. `api.review.getEpisode.useQuery()` or `utils.review.getEpisode`).
 */
function isRpcChainProp(propNode) {
  const member = propNode?.parent;
  if (!member || member.type !== 'member_expression') return false;

  const grand = member.parent;
  if (grand?.type === 'member_expression' && grand.childForFieldName('object') === member) {
    const nextProp = grand.childForFieldName('property');
    if (nextProp && RPC_TERMINAL_METHODS.has(nextProp.text)) {
      return true;
    }
  }

  // Check if receiver chain starts at a known RPC root (`api.x.y`, `trpc.x.y`, `utils.x.y`)
  const obj = member.childForFieldName('object');
  if (obj?.type === 'member_expression') {
    let cur = obj;
    let hops = 0;
    while (cur && cur.type === 'member_expression' && hops < 6) {
      cur = cur.childForFieldName('object');
      hops++;
    }
    if (cur?.type === 'identifier' && RPC_ROOT_RECEIVERS.has(cur.text)) {
      return true;
    }
  }

  return false;
}

/**
 * Strip comments and string literals from source in pure JS while preserving
 * line breaks, so same-file constant reads can be detected without N-API walks
 * or docstring/comment false positives.
 */
function stripCommentsAndStrings(source, isPython) {
  let out = '';
  const len = source.length;
  let i = 0;

  while (i < len) {
    const ch = source[i];
    const next = source[i + 1];

    if (isPython) {
      if (ch === '#') {
        while (i < len && source[i] !== '\n') i++;
        continue;
      }
      if ((ch === '"' || ch === "'") && source[i + 1] === ch && source[i + 2] === ch) {
        const q = ch;
        i += 3;
        while (i < len) {
          if (source[i] === '\\') { i += 2; continue; }
          if (source[i] === '\n') out += '\n';
          if (source[i] === q && source[i + 1] === q && source[i + 2] === q) {
            i += 3;
            break;
          }
          i++;
        }
        out += ' ';
        continue;
      }
      if (ch === '"' || ch === "'") {
        const q = ch;
        i++;
        while (i < len && source[i] !== '\n') {
          if (source[i] === '\\') { i += 2; continue; }
          if (source[i] === q) { i++; break; }
          i++;
        }
        out += ' ';
        continue;
      }
      out += ch;
      i++;
      continue;
    }

    // JS / TS
    if (ch === '/' && next === '/') {
      i += 2;
      while (i < len && source[i] !== '\n') i++;
      continue;
    }
    if (ch === '/' && next === '*') {
      i += 2;
      while (i < len && !(source[i] === '*' && source[i + 1] === '/')) {
        if (source[i] === '\n') out += '\n';
        i++;
      }
      i += 2;
      out += ' ';
      continue;
    }
    if (ch === '"' || ch === "'" || ch === '`') {
      const q = ch;
      i++;
      while (i < len) {
        if (source[i] === '\\') { i += 2; continue; }
        if (source[i] === '\n') out += '\n';
        if (source[i] === q) { i++; break; }
        i++;
      }
      out += ' ';
      continue;
    }
    out += ch;
    i++;
  }

  return out;
}

function collectSameFileVariableRefs(source, definitions, filePath, langName) {
  const varDefs = definitions.filter(d => d.kind === 'variable' && d.name && d.name.length >= 2);
  if (varDefs.length === 0) return [];

  const isPy = langName === 'python';
  const stripped = stripCommentsAndStrings(source, isPy);
  const lines = stripped.split('\n');
  const refs = [];

  for (const def of varDefs) {
    const escaped = def.name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    const re = new RegExp(`(?<![A-Za-z0-9_$])${escaped}(?![A-Za-z0-9_$])`);
    for (let i = 0; i < lines.length; i++) {
      const lineNum = i + 1;
      if (lineNum >= def.lineStart && lineNum <= def.lineEnd) continue;
      const line = lines[i];
      if (!line.includes(def.name)) continue;
      if (!isPy && /^\s*export\s*\{/.test(line)) continue;
      if (re.test(line)) {
        refs.push({
          name: def.name,
          kind: 'symbol',
          file: filePath,
          line: lineNum,
        });
        break;
      }
    }
  }

  return refs;
}

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
  if (!node || depth > 8) return null;
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
  { bodyStartLine = null, kind, exported, includeParamNames = false } = {},
) {
  const bodyNode = findBodyNode(node);
  const resolvedBodyStartLine = bodyStartLine ?? computeBodyStartLine(node, bodyNode);
  const resolvedExported = exported !== undefined ? exported : isNodeExported(node, langName);

  const def = {
    name,
    kind: resolveKind(node, kind),
    exported: resolvedExported,
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
    const cleaned = keyNode.text.replace(/^['"`]|['"`]$/g, '');
    // Skip MongoDB / JSON-schema operator keys like `$expr`, `$in`, `$match`
    if (cleaned.startsWith('$')) return null;
    return cleaned;
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

function extractDeclaratorName(node) {
  const nameNode = node?.childForFieldName('name');
  return nameNode?.type === 'identifier' ? nameNode.text : null;
}

function extractCalleeLeafName(node) {
  if (!node) return null;

  if (['identifier', 'property_identifier', 'private_property_identifier', 'type_identifier'].includes(node.type)) {
    return node.text;
  }

  if (node.type === 'member_expression' || node.type === 'attribute') {
    const propertyNode = node.childForFieldName('property') || node.childForFieldName('attribute');
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

function shouldIndexSemanticObjectMembers(declaratorNode, isExported) {
  const variableName = extractDeclaratorName(declaratorNode);
  const valueNode = declaratorNode.childForFieldName('value');
  const calleeName = extractCallLikeCalleeName(valueNode);

  if (variableName && NON_SEMANTIC_CONTAINER_NAME_RE.test(variableName)) return false;

  const hasSemanticVariableName = !!variableName
    && SEMANTIC_CONTAINER_NAME_RE.test(variableName);
  const hasSemanticCalleeName = !!calleeName && !NON_SEMANTIC_CALLEE_NAME_RE.test(calleeName);

  // Only index object-literal members of non-exported containers if the variable name
  // explicitly looks like a router/handler container; avoid indexing test mock fixtures.
  if (!isExported) {
    return valueNode?.type === 'object' && hasSemanticVariableName;
  }

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

    const valueNode = child.childForFieldName('value');
    if (valueNode && LITERAL_VALUE_TYPES.has(valueNode.type)) continue;

    definitions.push(createDefinition(name, child, filePath, langName, {
      bodyStartLine: child.startPosition.row + 2,
      kind: 'property',
      exported: opts.exported ?? true,
      includeParamNames: opts.includeParamNames,
    }));
  }

  return definitions;
}

/**
 * Fast top-level-only collector for semantic object members (e.g. tRPC routers,
 * exported handler maps). Inspects ONLY direct top-level statements of `rootNode`
 * instead of recursively walking the entire AST across the N-API boundary.
 */
function collectIndexDefinitions(rootNode, filePath, langName, opts = {}) {
  if (!JS_LANGS.has(langName)) return [];

  const definitions = [];

  for (let i = 0; i < rootNode.namedChildCount; i++) {
    let stmt = rootNode.namedChild(i);
    let isExported = false;
    if (stmt.type === 'export_statement') {
      isExported = true;
      stmt = stmt.childForFieldName('declaration');
      if (!stmt) continue;
    }
    if (stmt.type !== 'lexical_declaration' && stmt.type !== 'variable_declaration') continue;

    for (let j = 0; j < stmt.namedChildCount; j++) {
      const decl = stmt.namedChild(j);
      if (decl.type !== 'variable_declarator') continue;
      if (!shouldIndexSemanticObjectMembers(decl, isExported)) continue;

      const bodyNode = findBodyNode(decl);
      if (bodyNode && bodyNode.type === 'object') {
        definitions.push(...collectDirectPairDefinitions(bodyNode, filePath, langName, {
          ...opts,
          exported: isExported,
        }));
      }
    }
  }

  return definitions;
}

/**
 * Collect Python module-level constants (UPPER_CASE assignments and names listed
 * in `__all__`) directly from top-level statements of `rootNode`.
 */
function collectPythonTopLevelConstants(rootNode, filePath) {
  const definitions = [];
  const allExports = new Set();
  const topAssignments = [];

  for (let i = 0; i < rootNode.namedChildCount; i++) {
    const stmt = rootNode.namedChild(i);
    if (stmt.type !== 'expression_statement') continue;
    const assign = stmt.namedChild(0);
    if (!assign || assign.type !== 'assignment') continue;

    const left = assign.childForFieldName('left');
    const right = assign.childForFieldName('right');
    if (!left || left.type !== 'identifier') continue;

    if (left.text === '__all__' && right && (right.type === 'list' || right.type === 'tuple')) {
      for (let k = 0; k < right.namedChildCount; k++) {
        const el = right.namedChild(k);
        if (el.type === 'string') {
          const name = el.text.replace(/^['"]|['"]$/g, '');
          if (name) allExports.add(name);
        }
      }
      continue;
    }

    topAssignments.push({ name: left.text, node: assign });
  }

  for (const { name, node } of topAssignments) {
    if (!PY_CONSTANT_NAME_RE.test(name) && !allExports.has(name)) continue;
    definitions.push(createDefinition(name, node, filePath, 'python', {
      kind: 'variable',
      exported: true,
    }));
  }

  return definitions;
}

function collectOutlineDefinitions(rootNode, filePath, langName) {
  if (!JS_LANGS.has(langName)) return [];

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
    if (node.type === 'assignment') {
      const firstLine = text.split('\n')[0].trim();
      return firstLine.length > 200 ? firstLine.substring(0, 200) + '...' : firstLine;
    }
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

function createImportEntry(path, filePath, node, { isReexport = false } = {}) {
  if (!path || !filePath || !node) return null;
  return {
    path,
    file: filePath,
    line: node.startPosition.row + 1,
    isReexport,
  };
}

/**
 * Collect Python imports, submodule imports (`from . import taxonomy as tax`),
 * imported symbol references (`from .catalog import CONST`, `from mod import fn as alias`),
 * and local alias mappings (`alias -> original` and `alias -> modulePath`).
 */
function collectPythonImportsAndAliases(rootNode, filePath, lang) {
  const imports = [];
  const importRefs = [];
  const symbolAliases = new Map(); // localName -> originalSymbolName
  const moduleAliases = new Map(); // localName -> moduleSpecifier

  function pushImport(path, node) {
    const entry = createImportEntry(path, filePath, node);
    if (entry) imports.push(entry);
  }

  function joinPythonMod(base, sub) {
    if (!base) return sub;
    if (/^\.+$/.test(base)) return `${base}${sub}`;
    return `${base}.${sub}`;
  }

  const pyImportQuery = getCachedQuery('python', 'import', lang, IMPORT_QUERIES.python);
  if (!pyImportQuery) {
    return { imports, importRefs, symbolAliases, moduleAliases };
  }

  for (const match of pyImportQuery.matches(rootNode)) {
    const capture = match.captures[0];
    if (!capture) continue;
    const node = capture.node;

    if (capture.name === 'py_import') {
      for (let i = 0; i < node.namedChildCount; i++) {
        const child = node.namedChild(i);
        if (child.type === 'dotted_name') {
          pushImport(child.text, child);
          const leaf = child.text.split('.').pop();
          if (leaf) moduleAliases.set(leaf, child.text);
          continue;
        }
        if (child.type === 'aliased_import') {
          const moduleNode = child.childForFieldName('name') || child.namedChild(0);
          const aliasNode = child.childForFieldName('alias') || child.namedChild(1);
          if (moduleNode?.type === 'dotted_name') {
            pushImport(moduleNode.text, moduleNode);
            if (aliasNode?.type === 'identifier') {
              moduleAliases.set(aliasNode.text, moduleNode.text);
            }
          }
        }
      }
    } else if (capture.name === 'py_import_from') {
      const moduleNode = node.childForFieldName('module_name');
      const modulePath = moduleNode ? moduleNode.text : null;
      if (modulePath) {
        pushImport(modulePath, moduleNode);
      }

      for (let i = 0; i < node.namedChildCount; i++) {
        const child = node.namedChild(i);
        if (child === moduleNode) continue;

        let origName = null;
        let localName = null;
        if (child.type === 'dotted_name' || child.type === 'identifier') {
          origName = child.text;
          localName = origName;
        } else if (child.type === 'aliased_import') {
          const n = child.childForFieldName('name') || child.namedChild(0);
          const a = child.childForFieldName('alias') || child.namedChild(1);
          if (n) origName = n.text;
          if (a) localName = a.text;
        }

        if (!origName) continue;

        if (modulePath) {
          const subPath = joinPythonMod(modulePath, origName);
          pushImport(subPath, child);
          if (localName) moduleAliases.set(localName, subPath);
        }

        importRefs.push({
          name: origName,
          kind: 'symbol',
          file: filePath,
          line: child.startPosition.row + 1,
        });

        if (localName && localName !== origName) {
          symbolAliases.set(localName, origName);
        }
      }
    }
  }

  return { imports, importRefs, symbolAliases, moduleAliases };
}

/**
 * Collect AST-only identifier references and local lexical bindings inside a
 * definition node (skipping strings, docstrings, and comments).
 * Used on-demand by `context()` and `callees()`.
 */
function collectLocalAstInfo(defNode) {
  const refs = new Set();
  const localBindings = new Set();

  const SKIP_TYPES = new Set([
    'string', 'string_content', 'string_fragment', 'template_string',
    'comment', 'regex', 'number',
  ]);

  function walk(node, isRoot = false) {
    if (!node || SKIP_TYPES.has(node.type)) return;

    if (!isRoot) {
      // Track local variable / parameter / inner function bindings so callees()
      // doesn't confuse `const apply = () => ...` with an external symbol.
      if (node.type === 'variable_declarator') {
        const n = node.childForFieldName('name');
        if (n?.type === 'identifier') localBindings.add(n.text);
      } else if (node.type === 'assignment') {
        const l = node.childForFieldName('left');
        if (l?.type === 'identifier') localBindings.add(l.text);
      } else if (node.type === 'required_parameter' || node.type === 'optional_parameter') {
        const p = node.childForFieldName('pattern');
        if (p?.type === 'identifier') localBindings.add(p.text);
      } else if (node.type === 'parameters' || node.type === 'formal_parameters') {
        for (let i = 0; i < node.namedChildCount; i++) {
          const ch = node.namedChild(i);
          if (ch.type === 'identifier') localBindings.add(ch.text);
        }
      }

      if (node.type === 'identifier' || node.type === 'type_identifier') {
        // Skip declaration names and property keys
        const parent = node.parent;
        const isDeclKey =
          (parent?.type === 'pair' && parent.childForFieldName('key') === node) ||
          (parent?.type === 'property_signature' && parent.childForFieldName('name') === node);
        if (!isDeclKey) {
          refs.add(node.text);
        }
      } else if (node.type === 'property_identifier') {
        // Only include if receiver is NOT a stdlib object and not a generic built-in method name
        const parent = node.parent;
        if (parent?.type === 'member_expression' && !METHOD_NOISE_NAMES.has(node.text)) {
          const recv = extractCalleeLeafName(parent.childForFieldName('object'));
          if (!recv || !STDLIB_RECEIVERS.has(recv)) {
            refs.add(node.text);
          }
        }
      }
    }

    for (let i = 0; i < node.namedChildCount; i++) {
      walk(node.namedChild(i), false);
    }
  }

  walk(defNode, true);
  return { localRefs: [...refs], localBindings: [...localBindings] };
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
  const defNodeByDef = includeLocalRefs ? new Map() : null;
  const references = [];
  const imports = [];
  const jsBottomExports = JS_LANGS.has(langName) ? collectJsBottomExports(tree.rootNode) : null;

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

        if (!includeOutlineDefinitions && isNestedInsideFunction(defNode)) continue;

        const name = nameCapture.node.text;
        const dedupKey = `${name}:${defNode.startPosition.row}`;
        if (seenDefKeys.has(dedupKey)) continue;
        seenDefKeys.add(dedupKey);

        const exported = isNodeExported(defNode, langName)
          || (jsBottomExports ? jsBottomExports.has(name) : false);

        const def = createDefinition(name, defNode, filePath, langName, {
          exported,
          includeParamNames,
        });
        definitions.push(def);
        if (defNodeByDef) defNodeByDef.set(def, defNode);
      }
    }
  }

  // Mark methods of bottom-exported JS/TS classes as exported
  if (JS_LANGS.has(langName)) {
    const exportedClassRanges = definitions
      .filter(d => d.kind === 'class' && d.exported)
      .map(d => [d.lineStart, d.lineEnd]);
    if (exportedClassRanges.length > 0) {
      for (const d of definitions) {
        if (d.kind === 'method' && !d.exported) {
          if (exportedClassRanges.some(([s, e]) => d.lineStart >= s && d.lineEnd <= e)) {
            d.exported = true;
          }
        }
      }
    }
  }

  // Fast top-level-only semantic container members (tRPC routers, exported handler maps)
  const extraIndexDefs = collectIndexDefinitions(tree.rootNode, filePath, langName, { includeParamNames });
  definitions.push(...extraIndexDefs);

  // Python module-level constants (`UPPER_CASE` and `__all__` members)
  if (langName === 'python') {
    definitions.push(...collectPythonTopLevelConstants(tree.rootNode, filePath));
  }

  if (includeOutlineDefinitions) {
    definitions.push(...collectOutlineDefinitions(tree.rootNode, filePath, langName));
  }

  // Skip reference & import queries when only generating a file outline
  if (!includeOutlineDefinitions) {
    let symbolAliases = null;
    let moduleAliases = null;

    if (JS_LANGS.has(langName)) {
      const jsInfo = collectJsImportInfo(tree.rootNode, source);
      symbolAliases = jsInfo.symbolAliases;
      moduleAliases = jsInfo.moduleAliases;
    }

    if (langName === 'python') {
      const py = collectPythonImportsAndAliases(tree.rootNode, filePath, lang);
      imports.push(...py.imports);
      references.push(...py.importRefs);
      symbolAliases = py.symbolAliases;
      moduleAliases = py.moduleAliases;
    }

    const refQueryStr = REF_QUERIES[langName] || REF_QUERIES.default;
    if (refQueryStr) {
      const refQuery = getCachedQuery(langName, 'ref', lang, refQueryStr);
      if (refQuery) {
        for (const match of refQuery.matches(tree.rootNode)) {
          let methodRecv = null;
          let propRecv = null;
          for (const capture of match.captures) {
            if (capture.name === 'method_recv') methodRecv = extractCalleeLeafName(capture.node);
            else if (capture.name === 'prop_recv') propRecv = extractCalleeLeafName(capture.node);
          }

          for (const capture of match.captures) {
            if (
              capture.name !== 'ref' &&
              capture.name !== 'import_ref' &&
              capture.name !== 'jsx_ref' &&
              capture.name !== 'prop_ref' &&
              capture.name !== 'method_ref'
            ) continue;
            let name = normalizeReferenceCapture(capture);
            if (!name) continue;

            if (capture.name === 'method_ref') {
              if (methodRecv && STDLIB_RECEIVERS.has(methodRecv)) continue;
              const receiverModule = moduleAliases?.get(methodRecv) || null;
              references.push({
                name,
                kind: 'method',
                receiver: methodRecv,
                receiverModule,
                file: filePath,
                line: capture.node.startPosition.row + 1,
              });
              continue;
            }

            if (capture.name === 'prop_ref') {
              if (propRecv && STDLIB_RECEIVERS.has(propRecv)) continue;
              const receiverModule = moduleAliases?.get(propRecv) || null;
              const isRpcChain = JS_LANGS.has(langName) ? isRpcChainProp(capture.node) : false;
              references.push({
                name,
                kind: 'property',
                receiver: propRecv,
                receiverModule,
                isRpcChain,
                file: filePath,
                line: capture.node.startPosition.row + 1,
              });
              continue;
            }

            // Resolve import aliases (e.g. `fn_impl()` -> `compute_quality_metrics`)
            if (capture.name === 'ref' && symbolAliases?.has(name)) {
              name = symbolAliases.get(name);
            }

            references.push({
              name,
              kind: 'symbol',
              file: filePath,
              line: capture.node.startPosition.row + 1,
            });
          }
        }
      }
    }

    // Same-file constant/variable reads (e.g. Python `set(ENABLED_CHECKS)`, TS `return CONST.has(x)`)
    if (JS_LANGS.has(langName) || langName === 'python') {
      const sameFileVarRefs = collectSameFileVariableRefs(source, definitions, filePath, langName);
      if (sameFileVarRefs.length > 0) references.push(...sameFileVarRefs);
    }

    const importQueryStr = langName === 'python' ? null : (IMPORT_QUERIES[langName] || null);
    if (importQueryStr) {
      const importQuery = getCachedQuery(langName, 'import', lang, importQueryStr);
      if (importQuery) {
        for (const match of importQuery.matches(tree.rootNode)) {
          const requireCapture = match.captures.find(c => c.name === 'require_fn');
          if (requireCapture && requireCapture.node.text !== 'require') continue;

          const reexportCapture = match.captures.find(c => c.name === 'reexport_path');
          const pathCapture = reexportCapture || match.captures.find(c => c.name === 'import_path');
          const path = normalizeImportPathCapture(pathCapture);
          if (!path) continue;

          imports.push({
            path,
            file: filePath,
            line: pathCapture.node.startPosition.row + 1,
            isReexport: !!reexportCapture,
          });
        }
      }
    }
  }

  // On-demand AST-only local references & lexical bindings (no docstring/string noise)
  if (includeLocalRefs && definitions.length > 0) {
    if (defNodeByDef) {
      for (const [def, defNode] of defNodeByDef) {
        const info = collectLocalAstInfo(defNode);
        def.localRefs = info.localRefs;
        def.localBindings = info.localBindings;
      }
    }
    // Also attach line-scoped references for extra definitions (properties/constants)
    const sortedDefs = [...definitions].sort((a, b) => a.lineStart - b.lineStart);
    const extraRefSets = new Map();
    for (const ref of references) {
      let enclosing = null;
      for (const def of sortedDefs) {
        if (ref.line >= def.lineStart && ref.line <= def.lineEnd) {
          enclosing = def;
        }
      }
      if (enclosing && !enclosing.localRefs) {
        let set = extraRefSets.get(enclosing);
        if (!set) {
          set = new Set();
          extraRefSets.set(enclosing, set);
        }
        set.add(ref.name);
      }
    }
    for (const [def, set] of extraRefSets) {
      def.localRefs = [...set];
    }
  }

  return { file: filePath, definitions, references, imports };
}

export {
  parseFile,
  extractParamNames,
  getLanguageFamily,
  STDLIB_RECEIVERS,
  METHOD_NOISE_NAMES,
  SUPPORTED_EXTENSIONS,
  LANG_MAP,
};
