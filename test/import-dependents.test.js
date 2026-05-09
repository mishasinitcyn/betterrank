import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

import { parseFile } from '../src/parser.js';
import { CodeIndex } from '../src/index.js';

test('parseFile captures exported router consts and import paths', () => {
  const source = `import { createTRPCRouter } from "~/server/api/trpc";

export const projectRouter = createTRPCRouter({});
`;

  const parsed = parseFile('src/server/api/routers/project.ts', source);
  const definitionNames = parsed.definitions.map(def => def.name);
  const importPaths = parsed.imports.map(entry => entry.path);

  assert.ok(definitionNames.includes('projectRouter'));
  assert.ok(importPaths.includes('~/server/api/trpc'));
});

test('parseFile captures Python import modules for explicit import edges', () => {
  const source = `import app.utils as utils
from .core import runner
from ..lib.tools import execute
`;

  const parsed = parseFile('app/jobs/task.py', source);
  const importPaths = parsed.imports.map(entry => entry.path);

  assert.ok(importPaths.includes('app.utils'));
  assert.ok(importPaths.includes('.core'));
  assert.ok(importPaths.includes('..lib.tools'));
});

test('parseFile indexes direct top-level router members but skips nested payload keys', () => {
  const source = `const hardwareSchema = z.object({
  exo: z.object({
    camera: z.string(),
  }),
});

export const projectRouter = createTRPCRouter({
  getTeamStatistics: protectedProcedure
    .input(
      z.object({
        projectId: z.string(),
      }),
    )
    .query(async () => {
      return {};
    }),
});
`;

  const parsed = parseFile('src/server/api/routers/project.ts', source);
  const definitionNames = parsed.definitions.map(def => def.name);

  assert.ok(definitionNames.includes('projectRouter'));
  assert.ok(definitionNames.includes('getTeamStatistics'));
  assert.ok(!definitionNames.includes('exo'));
  assert.ok(!definitionNames.includes('camera'));
  assert.ok(!definitionNames.includes('projectId'));
});

test('parseFile keeps import references in plain TypeScript files', () => {
  const source = `import { projectRouter } from "~/server/api/routers/project";

export const appRouter = {
  project: projectRouter,
};
`;

  const parsed = parseFile('src/server/api/root.ts', source);
  const refNames = parsed.references.map(ref => ref.name);

  assert.ok(refNames.includes('projectRouter'));
});

test('symbols and search expose indexed top-level router members', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-object-members-'));

  try {
    await mkdir(join(root, 'src/server/api/routers'), { recursive: true });

    await writeFile(
      join(root, 'src/server/api/routers/project.ts'),
      `const hardwareSchema = z.object({
  exo: z.object({
    camera: z.string(),
  }),
});

export const projectRouter = createTRPCRouter({
  getTeamStatistics: protectedProcedure
    .query(async () => {
      return {};
    }),
  getSubmissionStats: protectedProcedure
    .query(async () => {
      return {};
    }),
});
`,
    );

    const idx = new CodeIndex(root, { cachePath: '.code-index-cache/test-index.json' });
    const symbols = await idx.symbols({ file: 'src/server/api/routers/project.ts' });
    const symbolNames = symbols.map(symbol => symbol.name);
    const search = await idx.search({ query: 'getTeamStat', limit: 10 });

    assert.ok(symbolNames.includes('getTeamStatistics'));
    assert.ok(symbolNames.includes('getSubmissionStats'));
    assert.ok(!symbolNames.includes('exo'));
    assert.equal(search[0]?.name, 'getTeamStatistics');
    assert.equal(search[0]?.file, 'src/server/api/routers/project.ts');
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('dependents resolves side-effect imports and preserves them after target updates', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-import-dependents-'));

  try {
    await mkdir(join(root, 'src/app'), { recursive: true });
    await mkdir(join(root, 'src/lib'), { recursive: true });

    await writeFile(
      join(root, 'src/lib/setup.ts'),
      `console.log("setup");
`,
    );

    await writeFile(
      join(root, 'src/app/root.ts'),
      `import "../lib/setup";

export function bootstrap() {
  return true;
}
`,
    );

    const idx = new CodeIndex(root, { cachePath: '.code-index-cache/test-index.json' });

    const initialDependents = await idx.dependents({ file: 'src/lib/setup.ts' });
    assert.deepEqual(initialDependents, ['src/app/root.ts']);

    await new Promise(resolve => setTimeout(resolve, 25));
    await writeFile(
      join(root, 'src/lib/setup.ts'),
      `console.log("setup v2");
`,
    );

    const updatedDependents = await idx.dependents({ file: 'src/lib/setup.ts' });
    assert.deepEqual(updatedDependents, ['src/app/root.ts']);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('dependents resolves ~/ alias imports for router files', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-import-alias-'));

  try {
    await mkdir(join(root, 'src/server/api/routers'), { recursive: true });
    await mkdir(join(root, 'src/server/api'), { recursive: true });

    await writeFile(
      join(root, 'src/server/api/routers/project.ts'),
      `export const projectRouter = { ready: true };
`,
    );

    await writeFile(
      join(root, 'src/server/api/root.ts'),
      `import { projectRouter } from "~/server/api/routers/project";

export const appRouter = {
  project: projectRouter,
};
`,
    );

    const idx = new CodeIndex(root, { cachePath: '.code-index-cache/test-index.json' });
    const dependents = await idx.dependents({ file: 'src/server/api/routers/project.ts' });

    assert.deepEqual(dependents, ['src/server/api/root.ts']);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('dependents resolves Python relative and absolute imports', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-python-imports-'));

  try {
    await mkdir(join(root, 'pkg'), { recursive: true });

    await writeFile(
      join(root, 'pkg/helpers.py'),
      `def run():
    return True
`,
    );

    await writeFile(
      join(root, 'pkg/main.py'),
      `from .helpers import run

def execute():
    return run()
`,
    );

    await writeFile(
      join(root, 'pkg/job.py'),
      `import pkg.helpers as helpers

def execute():
    return helpers.run()
`,
    );

    const idx = new CodeIndex(root, { cachePath: '.code-index-cache/test-index.json' });
    const dependents = await idx.dependents({ file: 'pkg/helpers.py' });
    const neighborhood = await idx.neighborhood({ file: 'pkg/main.py', hops: 1, maxFiles: 5 });

    assert.deepEqual(dependents, ['pkg/job.py', 'pkg/main.py']);
    assert.ok(neighborhood.files.includes('pkg/helpers.py'));
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('incremental indexing handles newly added files without graph crashes', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-incremental-add-'));

  try {
    await mkdir(join(root, 'src'), { recursive: true });

    await writeFile(
      join(root, 'src/alpha.ts'),
      `export function alpha() {
  return 1;
}
`,
    );

    const idx = new CodeIndex(root, { cachePath: '.code-index-cache/test-index.json' });
    const initial = await idx.search({ query: 'alpha', limit: 5 });
    assert.equal(initial[0]?.name, 'alpha');

    await new Promise(resolve => setTimeout(resolve, 25));
    await writeFile(
      join(root, 'src/beta.ts'),
      `export function beta() {
  return 2;
}
`,
    );

    const updated = await idx.search({ query: 'beta', limit: 5 });
    assert.equal(updated[0]?.name, 'beta');
    assert.equal(updated[0]?.file, 'src/beta.ts');
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
