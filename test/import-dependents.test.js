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
