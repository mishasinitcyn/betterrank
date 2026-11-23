import test from 'node:test';
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';

import { CodeIndex } from '../src/index.js';
import { buildOutline } from '../src/outline.js';

const execFileAsync = promisify(execFile);
const CLI_PATH = resolve(new URL('../src/cli.js', import.meta.url).pathname);

test('multi-root JS/TS resolves root-level package.json @pkg/* imports and reuses warm per-root parse caches', async () => {
  const base = await mkdtemp(join(tmpdir(), 'betterrank-v041-multiroot-'));
  const cacheDir = join(base, 'cache');
  const pkgRoot = join(base, 'packages/shared-intake');
  const appRoot = join(base, 'apps/workspace');
  const prevCacheDir = process.env.CODE_INDEX_CACHE_DIR;

  try {
    await mkdir(join(pkgRoot, 'src'), { recursive: true });
    await mkdir(join(appRoot, 'src/server'), { recursive: true });

    await writeFile(
      join(pkgRoot, 'package.json'),
      JSON.stringify({
        name: '@demo/shared-intake',
        main: 'src/index.ts',
        exports: {
          '.': './src/index.ts',
          './*': './src/*.ts',
        },
      }),
    );
    await writeFile(
      join(pkgRoot, 'src/index.ts'),
      `export { submitAccount } from './submit-account';\n`,
    );
    await writeFile(
      join(pkgRoot, 'src/submit-account.ts'),
      `export function submitAccount(id: string) {\n  return id.trim();\n}\n`,
    );
    await writeFile(
      join(pkgRoot, 'src/extra.ts'),
      `export const EXTRA_LIMIT = 42;\n`,
    );

    await writeFile(
      join(appRoot, 'package.json'),
      JSON.stringify({ name: '@demo/workspace' }),
    );
    await writeFile(
      join(appRoot, 'src/server/handler.ts'),
      `import { submitAccount } from '@demo/shared-intake';\nimport { EXTRA_LIMIT } from '@demo/shared-intake/extra';\nexport function run(id: string) {\n  return submitAccount(id) + String(EXTRA_LIMIT);\n}\n`,
    );

    process.env.CODE_INDEX_CACHE_DIR = cacheDir;

    // Warm single-root indexes first so both roots write per-root parse caches
    const idxPkg = new CodeIndex(pkgRoot);
    await idxPkg.stats();
    const idxApp = new CodeIndex(appRoot);
    await idxApp.stats();

    // Compose multi-root index: should reuse all 4 cached parses AND resolve @demo/shared-intake
    const multi = new CodeIndex([pkgRoot, appRoot]);
    const ensureRes = await multi._ensureReady();
    assert.equal(ensureRes.reused, 4, 'multi-root should reuse all warm per-root parsed files');

    const callers = await multi.callers({ symbol: 'submitAccount' });
    const callerFiles = callers.map(c => c.file);
    assert.ok(
      callerFiles.includes('workspace/src/server/handler.ts'),
      `expected cross-root caller workspace/src/server/handler.ts, got: ${callerFiles.join(', ')}`,
    );

    const constCallers = await multi.callers({ symbol: 'EXTRA_LIMIT' });
    assert.ok(
      constCallers.map(c => c.file).includes('workspace/src/server/handler.ts'),
      'expected subpath export @demo/shared-intake/extra caller to resolve across roots',
    );
  } finally {
    if (prevCacheDir === undefined) delete process.env.CODE_INDEX_CACHE_DIR;
    else process.env.CODE_INDEX_CACHE_DIR = prevCacheDir;
    await rm(base, { recursive: true, force: true });
  }
});

test('barrel-only multi-hop reachability and createCallerFactory scoping eliminate residual B2 caller leaks', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-v041-b2-'));

  try {
    await mkdir(join(root, 'packages/intake/src'), { recursive: true });
    await mkdir(join(root, 'apps/web/src/routers'), { recursive: true });
    await mkdir(join(root, 'apps/web/src/ui'), { recursive: true });

    await writeFile(
      join(root, 'packages/intake/package.json'),
      JSON.stringify({ name: '@demo/intake', main: 'src/index.ts' }),
    );
    await writeFile(
      join(root, 'packages/intake/src/index.ts'),
      `export { createAccount } from './create-account';\n`,
    );
    await writeFile(
      join(root, 'packages/intake/src/create-account.ts'),
      `export function createAccount(name: string) {\n  return { name };\n}\n`,
    );

    // Non-barrel runtime chain: wizard-loader -> trpc -> root -> accounts -> create-account
    await writeFile(
      join(root, 'apps/web/src/routers/accounts.ts'),
      `import { createAccount } from '@demo/intake';\nexport const accountsRouter = createTRPCRouter({\n  create: publicProcedure.mutation(() => createAccount('x')),\n});\n`,
    );
    await writeFile(
      join(root, 'apps/web/src/routers/root.ts'),
      `import { accountsRouter } from './accounts';\nexport const appRouter = { accounts: accountsRouter };\n`,
    );
    await writeFile(
      join(root, 'apps/web/src/trpc.ts'),
      `import type { appRouter } from './routers/root';\nexport const api: any = null;\n`,
    );
    await writeFile(
      join(root, 'apps/web/src/ui/wizard-loader.tsx'),
      `import { api } from '../trpc';\nexport function WizardLoader() {\n  const { mutateAsync: createAccount } = api.accounts.create.useMutation();\n  return createAccount('local');\n}\n`,
    );

    // Two routers with same-named procedure `getItem`, plus a test importing both
    await writeFile(
      join(root, 'apps/web/src/routers/review.ts'),
      `export const reviewRouter = createTRPCRouter({\n  getItem: publicProcedure\n    .query(() => 'review'),\n  reject: publicProcedure\n    .mutation(() => true),\n});\n`,
    );
    await writeFile(
      join(root, 'apps/web/src/routers/disputes.ts'),
      `export const disputesRouter = createTRPCRouter({\n  getItem: publicProcedure\n    .query(() => 'dispute'),\n});\n`,
    );
    await writeFile(
      join(root, 'apps/web/src/routers/disputes.test.ts'),
      `import { disputesRouter } from './disputes';\nimport { reviewRouter } from './review';\nconst caller = createCallerFactory(disputesRouter)({});\nconst liveCaller = createCallerFactory(reviewRouter)({});\nexport async function runTest() {\n  await caller.getItem();\n  await liveCaller.reject();\n}\n`,
    );

    const idx = new CodeIndex(root, { cachePath: '.code-index-cache/test.br.gz' });

    const createCallers = (await idx.callers({
      symbol: 'createAccount',
      file: 'packages/intake/src/create-account.ts',
    })).map(c => c.file);
    assert.ok(createCallers.includes('apps/web/src/routers/accounts.ts'));
    assert.ok(
      !createCallers.includes('apps/web/src/ui/wizard-loader.tsx'),
      `wizard-loader.tsx should NOT leak via non-barrel transitive chain: ${createCallers.join(', ')}`,
    );

    const reviewGetItemCallers = (await idx.callers({
      symbol: 'getItem',
      file: 'apps/web/src/routers/review.ts',
    })).map(c => c.file);
    assert.ok(
      !reviewGetItemCallers.includes('apps/web/src/routers/disputes.test.ts'),
      `disputes.test.ts calls caller.getItem() on disputesRouter, not reviewRouter: ${reviewGetItemCallers.join(', ')}`,
    );

    const disputesGetItemCallers = (await idx.callers({
      symbol: 'getItem',
      file: 'apps/web/src/routers/disputes.ts',
    })).map(c => c.file);
    assert.ok(disputesGetItemCallers.includes('apps/web/src/routers/disputes.test.ts'));

    // Also verify --no-tests filters out test callers
    const nonTestCallers = (await idx.callers({
      symbol: 'getItem',
      file: 'apps/web/src/routers/disputes.ts',
      noTests: true,
    })).map(c => c.file);
    assert.deepEqual(nonTestCallers, []);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('plain property reads (.reasonCodes) and hash.update() do not collide with unrelated router procedures (B4)', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-v041-b4-'));

  try {
    await mkdir(join(root, 'src/routers'), { recursive: true });
    await mkdir(join(root, 'src/lib'), { recursive: true });

    await writeFile(
      join(root, 'src/routers/delivery-check.ts'),
      `export const deliveryCheckRouter = createTRPCRouter({\n  reasonCodes: publicProcedure.query(() => ['a']),\n  update: publicProcedure.mutation(() => true),\n});\n`,
    );
    await writeFile(
      join(root, 'src/lib/worker.ts'),
      `import { createHash } from 'node:crypto';\nexport function summarize(doc: { reasonCodes: string[] }) {\n  const hash = createHash('sha256');\n  hash.update(doc.reasonCodes.join(','));\n  return hash.digest('hex');\n}\n`,
    );

    const idx = new CodeIndex(root, { cachePath: '.code-index-cache/test.br.gz' });

    const callees = await idx.callees({ symbol: 'summarize', file: 'src/lib/worker.ts', depth: 1 });
    assert.deepEqual(callees.callees, [], 'summarize should not link to delivery-check reasonCodes or update');

    const ctx = await idx.context({ symbol: 'summarize', file: 'src/lib/worker.ts' });
    assert.ok(!ctx.usedSymbols.some(s => s.name === 'reasonCodes' || s.name === 'update'));

    const reasonCallers = await idx.callers({ symbol: 'reasonCodes' });
    assert.deepEqual(reasonCallers, []);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('context includes same-file callers, constant callers include same-file reads, and Python type annotations appear in --context', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-v041-samefile-'));

  try {
    await writeFile(
      join(root, 'service.ts'),
      `function recordShadow(id: string) {\n  return id;\n}\nexport function approve(id: string) {\n  return recordShadow(id);\n}\n`,
    );
    await writeFile(
      join(root, 'policy.py'),
      `ENABLED_CHECKS = ("a", "b")\n\nclass PolicyBatch:\n    pass\n\ndef validate(checks) -> PolicyBatch:\n    unknown = set(checks) - set(ENABLED_CHECKS)\n    return PolicyBatch()\n`,
    );

    const idx = new CodeIndex(root, { cachePath: '.code-index-cache/test.br.gz' });

    const ctx = await idx.context({ symbol: 'recordShadow', file: 'service.ts' });
    assert.ok(ctx.callers.includes('service.ts'), 'context should include same-file caller');

    const constCallers = await idx.callers({ symbol: 'ENABLED_CHECKS' });
    assert.ok(
      constCallers.some(c => c.file === 'policy.py'),
      'callers on Python constant should detect same-file read set(ENABLED_CHECKS)',
    );

    const classCallers = await idx.callers({ symbol: 'PolicyBatch', context: 1 });
    assert.ok(classCallers.length === 1);
    assert.ok(
      classCallers[0].sites.some(s => s.text.some(l => l.content.includes('-> PolicyBatch:'))),
      'callers --context should include Python type annotation sites',
    );
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('outline --depth 2 collapses non-declaration JSX runs inside monolithic React components (C3)', () => {
  const jsxLines = Array.from({ length: 80 }, (_, i) => `      <div>Row ${i}</div>`).join('\n');
  const source = [
    `import React from 'react';`,
    `export default function GiantPage() {`,
    `  const a = 1;`,
    `  const b = 2;`,
    `  const c = 3;`,
    `  const handleApprove = () => {`,
    `    const x = 1;`,
    `    return x + 1;`,
    `  };`,
    `  const handleReject = () => {`,
    `    const y = 2;`,
    `    return y + 2;`,
    `  };`,
    `  return (`,
    `    <div>`,
    jsxLines,
    `    </div>`,
    `  );`,
    `}`,
  ].join('\n');

  const depth2 = buildOutline(source, 'GiantPage.tsx', [], { depth: 2 });
  const outLines = depth2.split('\n');
  assert.ok(depth2.includes('export default function GiantPage()'), 'outer component signature visible');
  assert.ok(depth2.includes('const handleApprove = () =>'), 'inner handler signature visible');
  assert.ok(depth2.includes('const handleReject = () =>'), 'second inner handler signature visible');
  assert.ok(!depth2.includes('<div>Row 40</div>'), 'giant JSX return tree must stay collapsed at --depth 2');
  assert.ok(outLines.length < 25, `expected compact --depth 2 outline (<25 lines), got ${outLines.length}`);
});

test('CLI outline accepts cwd-relative path with --root subdir and map --help documents --include-tests', async () => {
  const base = await mkdtemp(join(tmpdir(), 'betterrank-v041-cli-'));

  try {
    await mkdir(join(base, 'packages/contracts/src'), { recursive: true });
    await writeFile(
      join(base, 'packages/contracts/src/index.ts'),
      `export function hello() {\n  return 'ok';\n}\n`,
    );

    const { stdout: outlineOut } = await execFileAsync(
      process.execPath,
      [CLI_PATH, 'outline', 'packages/contracts/src/index.ts', '--root', 'packages/contracts'],
      { cwd: base },
    );
    assert.ok(outlineOut.includes('export function hello()'));

    const { stdout: mapHelp } = await execFileAsync(process.execPath, [CLI_PATH, 'map', '--help']);
    assert.ok(mapHelp.includes('--include-tests'));

    const { stdout: callersHelp } = await execFileAsync(process.execPath, [CLI_PATH, 'callers', '--help']);
    assert.ok(callersHelp.includes('--no-tests'));
  } finally {
    await rm(base, { recursive: true, force: true });
  }
});

test('parallel worker_threads parsing indexes >=120 files cleanly and persists per-root parse cache', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-v042-workers-'));

  try {
    await mkdir(join(root, 'src'), { recursive: true });
    await writeFile(
      join(root, 'src/core.ts'),
      `export function sharedCore(n: number) {\n  return n * 2;\n}\n`,
    );
    const writes = [];
    for (let i = 0; i < 130; i++) {
      writes.push(
        writeFile(
          join(root, `src/mod_${i}.ts`),
          `import { sharedCore } from './core';\nexport function fn_${i}(x: number) {\n  return sharedCore(x) + ${i};\n}\n`,
        ),
      );
    }
    await Promise.all(writes);

    const idx = new CodeIndex(root, { cachePath: '.code-index-cache/test.br.gz' });
    const st = await idx.stats();
    assert.equal(st.files, 131);
    assert.equal(st.symbols, 131);

    const callers = await idx.callers({ symbol: 'sharedCore', count: true });
    assert.equal(callers.total, 130);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
