import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';

import { CodeIndex } from '../src/index.js';
import { buildOutline, parseLineRange } from '../src/outline.js';

const CLI_PATH = resolve(import.meta.dirname, '../src/cli.js');

test('strict IMPORTS vs REFERENCES eliminates deps false positives and resolves workspace @pkg/* barrels', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-v04-strict-'));
  try {
    await mkdir(join(root, 'packages/shared-intake/src'), { recursive: true });
    await mkdir(join(root, 'apps/web/src'), { recursive: true });
    await mkdir(join(root, 'apps/unrelated/src'), { recursive: true });

    await writeFile(
      join(root, 'packages/shared-intake/package.json'),
      JSON.stringify({
        name: '@acme/shared-intake',
        main: 'src/index.ts',
        exports: { '.': './src/index.ts', './*': './src/*.ts' },
      }),
    );
    await writeFile(
      join(root, 'packages/shared-intake/src/create-account.ts'),
      `type Db = { query: () => void };
export const ACCOUNT_PATH = "/accounts";
export function createAccount(db: Db) {
  return ACCOUNT_PATH;
}
`,
    );
    await writeFile(
      join(root, 'packages/shared-intake/src/index.ts'),
      `export { createAccount, ACCOUNT_PATH } from './create-account';\n`,
    );
    await writeFile(
      join(root, 'apps/web/src/signup.ts'),
      `import { createAccount, ACCOUNT_PATH } from '@acme/shared-intake';
export function runSignup(db: any) {
  return createAccount(db) + ACCOUNT_PATH;
}
`,
    );
    // Unrelated file with a local callback named `createAccount` and type `Db` — NO import from shared-intake
    await writeFile(
      join(root, 'apps/unrelated/src/wizard.tsx'),
      `export function Wizard({ createAccount }: { createAccount: (x: any) => void; db?: Db }) {
  return createAccount({});
}
`,
    );

    const idx = new CodeIndex(root, { cachePath: '.cache/index.br.gz' });

    // B1: deps of signup.ts must ONLY contain the resolved workspace import, never unrelated files
    const deps = await idx.dependencies({ file: 'apps/web/src/signup.ts' });
    assert.deepEqual(deps, ['packages/shared-intake/src/index.ts']);

    // B2: callers of createAccount --file must include signup.ts (via barrel) and index.ts re-export, and EXCLUDE wizard.tsx
    const callers = await idx.callers({
      symbol: 'createAccount',
      file: 'packages/shared-intake/src/create-account.ts',
    });
    const callerFiles = callers.map(c => c.file);
    assert.ok(callerFiles.includes('apps/web/src/signup.ts'));
    assert.ok(callerFiles.includes('packages/shared-intake/src/index.ts'));
    assert.ok(!callerFiles.includes('apps/unrelated/src/wizard.tsx'));

    // A1: exported primitive constant ACCOUNT_PATH is indexed and has callers with context
    const constCallers = await idx.callers({ symbol: 'ACCOUNT_PATH', context: 1 });
    assert.ok(constCallers.some(c => c.file === 'apps/web/src/signup.ts' && c.sites.length > 0));

    // B4: unexported local `type Db` must have 0 cross-file callers
    const counts = await idx.getCallerCounts('packages/shared-intake/src/create-account.ts');
    assert.equal(counts.get('Db'), undefined);
    assert.equal(counts.get('createAccount'), 2);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('Python constants, from . import mod as alias, mod.fn(), import as, and AST-only context', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-v04-python-'));
  try {
    await mkdir(join(root, 'rules_pkg'), { recursive: true });
    await mkdir(join(root, 'tests'), { recursive: true });

    await writeFile(join(root, 'rules_pkg/__init__.py'), `from .engine import validate_policy\n`);
    await writeFile(
      join(root, 'rules_pkg/catalog.py'),
      `ENABLED_CHECKS = ["blur", "angle"]
CURRENT_VERSION = 2
`,
    );
    await writeFile(
      join(root, 'rules_pkg/engine.py'),
      `from . import catalog as cat

def _inner_check(item):
    return item in cat.ENABLED_CHECKS

def validate_policy(item):
    """Validate python fields for a given item."""
    return _inner_check(item)
`,
    );
    await writeFile(
      join(root, 'tests/test_engine.py'),
      `import rules_pkg as lib
from rules_pkg.engine import validate_policy as fn_impl

def test_calls():
    assert lib.validate_policy("blur")
    assert fn_impl("angle")
`,
    );
    // Cross-language .mjs file defining `python` and `fields` — must NEVER leak into Python context
    await writeFile(
      join(root, 'tests/exports.test.mjs'),
      `export function python(cases) { return cases; }
export function fields(x) { return x; }
`,
    );

    const idx = new CodeIndex(root, { cachePath: '.cache/index.br.gz' });

    // A1: `from . import catalog as cat` creates an IMPORTS edge from engine.py -> catalog.py
    const deps = await idx.dependents({ file: 'rules_pkg/catalog.py' });
    assert.deepEqual(deps, ['rules_pkg/engine.py']);

    // A1: Python UPPER_CASE constant callers via `cat.ENABLED_CHECKS`
    const constCallers = await idx.callers({ symbol: 'ENABLED_CHECKS' });
    assert.deepEqual(constCallers.map(c => c.file), ['rules_pkg/engine.py']);

    // A2: `lib.validate_policy(...)` and `fn_impl(...)` both wire to validate_policy
    const fnCallers = await idx.callers({ symbol: 'validate_policy' });
    assert.ok(fnCallers.some(c => c.file === 'tests/test_engine.py'));

    // B3: context on validate_policy must NOT match "python" or "fields" from its docstring
    const ctx = await idx.context({ symbol: 'validate_policy' });
    assert.ok(ctx);
    assert.ok(!ctx.usedSymbols.some(s => s.file.endsWith('.mjs')));
    assert.ok(ctx.usedSymbols.some(s => s.name === '_inner_check'));

    // A6: trace climbs intra-file caller `_inner_check` -> `validate_policy`
    const tree = await idx.trace({ symbol: '_inner_check', depth: 3 });
    assert.ok(tree.callers.some(c => c.name === 'validate_policy' && c.file === 'rules_pkg/engine.py'));
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('tRPC property callers disambiguate by receiver and avoid cross-router leakage', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-v04-trpc-'));
  try {
    await mkdir(join(root, 'src/server/api/routers'), { recursive: true });
    await mkdir(join(root, 'src/app'), { recursive: true });

    await writeFile(
      join(root, 'src/server/api/routers/review.ts'),
      `export const reviewRouter = createTRPCRouter({
  getItem: protectedProcedure.query(async () => {
    return Promise.resolve(1);
  }),
});
`,
    );
    await writeFile(
      join(root, 'src/server/api/routers/disputes.ts'),
      `export const disputesRouter = createTRPCRouter({
  getItem: protectedProcedure.query(async () => {
    return 2;
  }),
});
`,
    );
    await writeFile(
      join(root, 'src/app/review-page.tsx'),
      `export function ReviewPage(api: any) {
  return api.review.getItem.useQuery();
}
`,
    );
    await writeFile(
      join(root, 'src/app/disputes-page.tsx'),
      `export function DisputesPage(api: any) {
  return api.disputes.getItem.useQuery();
}
`,
    );

    const idx = new CodeIndex(root, { cachePath: '.cache/index.br.gz' });
    const reviewCallers = await idx.callers({
      symbol: 'getItem',
      file: 'src/server/api/routers/review.ts',
    });
    assert.deepEqual(reviewCallers.map(c => c.file), ['src/app/review-page.tsx']);

    // B4: Promise.resolve() inside getItem must NOT wire to an unrelated resolve function
    const callees = await idx.callees({
      symbol: 'getItem',
      file: 'src/server/api/routers/review.ts',
      depth: 2,
    });
    assert.equal(callees.callees.length, 0);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('multi-root CodeIndex preserves Python class method callers and preserves src/**/vendor/', async () => {
  const parent = await mkdtemp(join(tmpdir(), 'betterrank-v04-multi-'));
  const repoA = join(parent, 'repo-utils');
  const repoB = join(parent, 'repo-workers');
  try {
    await mkdir(join(repoA, 'utils_pkg/routing'), { recursive: true });
    await mkdir(join(repoA, 'src/vendor'), { recursive: true });
    await mkdir(join(repoA, 'vendor'), { recursive: true });
    await mkdir(join(repoB, 'worker'), { recursive: true });

    await writeFile(join(repoA, 'pyproject.toml'), `[project]\nname = "utils-pkg"\n`);
    await writeFile(
      join(repoA, 'utils_pkg/routing/router.py'),
      `class ProjectRouter:
    def get_check_policy(self, project_id):
        return {"id": project_id}
`,
    );
    await writeFile(
      join(repoA, 'src/vendor/heads.ts'),
      `export function getVendorHead() { return "v1"; }\n`,
    );
    await writeFile(
      join(repoA, 'vendor/ignored.ts'),
      `export function shouldBeIgnored() { return 0; }\n`,
    );
    await writeFile(
      join(repoB, 'worker/run.py'),
      `from utils_pkg.routing.router import ProjectRouter

def process(pid):
    r = ProjectRouter()
    return r.get_check_policy(pid)
`,
    );

    const multi = new CodeIndex([repoA, repoB], { cachePath: '.cache/multi.br.gz' });
    const callers = await multi.callers({ symbol: 'get_check_policy' });
    assert.deepEqual(callers.map(c => c.file), ['repo-workers/worker/run.py']);

    // A4: src/vendor/heads.ts IS indexed, root vendor/ignored.ts is NOT
    const hits = await multi.search({ query: 'getVendorHead' });
    assert.equal(hits.length, 1);
    const ignored = await multi.search({ query: 'shouldBeIgnored' });
    assert.equal(ignored.length, 0);
  } finally {
    await rm(parent, { recursive: true, force: true });
  }
});

test('map excludes tests by default, warns on missing --focus, and CLI supports --version and outline controls', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-v04-cli-'));
  try {
    await mkdir(join(root, 'src'), { recursive: true });
    await writeFile(
      join(root, 'src/service.ts'),
      `import { helper } from './helper';
import { a } from './a';
import { b } from './b';

/**
 * Multi-line JSDoc comment
 * explaining the service.
 */
export const SERVICE_VERSION = "1.0.0";

export function BigComponent() {
  const innerHandler = () => {
    return helper();
  };
  const secondHandler = () => {
    return a() + b();
  };
  return innerHandler() + secondHandler();
}
`,
    );
    await writeFile(
      join(root, 'src/service.test.ts'),
      `export const mockCtx = { db: 1, user: 2, data: 3 };
export function testHelper() { return 1; }
`,
    );

    const idx = new CodeIndex(root, { cachePath: '.cache/index.br.gz' });
    const mapDefault = await idx.map({ structured: true });
    assert.ok(!mapDefault.files.some(f => f.path.endsWith('.test.ts')));

    const mapWithTests = await idx.map({ structured: true, includeTests: true });
    assert.ok(mapWithTests.files.some(f => f.path.endsWith('.test.ts')));

    const missMap = await idx.map({ focusFiles: ['src/does-not-exist.ts'] });
    assert.equal(missMap.missingFocusFiles.length, 1);

    // outline: visible imports by default, --no-comments, --depth 2, --lines, and exported const expand
    const src = await import('node:fs/promises').then(fs => fs.readFile(join(root, 'src/service.ts'), 'utf-8'));
    const defaultOutline = buildOutline(src, 'src/service.ts');
    assert.match(defaultOutline, /import \{ helper \}/, 'imports should stay visible by default');

    const noCommentsOutline = buildOutline(src, 'src/service.ts', [], { noComments: true });
    assert.ok(!noCommentsOutline.includes('Multi-line JSDoc comment'));

    const deepOutline = buildOutline(src, 'src/service.ts', [], { depth: 2 });
    assert.match(deepOutline, /const innerHandler =/);
    assert.match(deepOutline, /const secondHandler =/);

    const sliced = buildOutline(src, 'src/service.ts', [], { lineRange: parseLineRange('9-11') });
    assert.equal(sliced.split('\n').length, 3);

    const expandedConst = buildOutline(src, 'src/service.ts', ['SERVICE_VERSION']);
    assert.match(expandedConst, /export const SERVICE_VERSION = "1.0.0";/);

    // CLI --version exits 0 and prints semver
    const ver = execFileSync(process.execPath, [CLI_PATH, '--version'], { encoding: 'utf-8' }).trim();
    assert.match(ver, /^\d+\.\d+\.\d+$/);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
