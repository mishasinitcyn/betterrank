import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

import { CodeIndex } from '../src/index.js';

test('map --focus prioritizes the focused file neighborhood over global winners', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-focused-map-'));

  try {
    await mkdir(join(root, 'src/lib'), { recursive: true });
    await mkdir(join(root, 'src/consumers'), { recursive: true });
    await mkdir(join(root, 'src/routes'), { recursive: true });

    await writeFile(
      join(root, 'src/lib/shared.ts'),
      `export function sharedUtil() {
  return 1;
}
`,
    );

    for (const name of ['a', 'b', 'c', 'd', 'e', 'f']) {
      await writeFile(
        join(root, `src/consumers/${name}.ts`),
        `import { sharedUtil } from "../lib/shared";

export function use${name.toUpperCase()}() {
  return sharedUtil();
}
`,
      );
    }

    await writeFile(
      join(root, 'src/routes/project.ts'),
      `import { createTRPCRouter } from "../trpc";
import { sharedUtil } from "../lib/shared";

export const projectRouter = createTRPCRouter({
  getTeamStatistics: protectedProcedure
    .query(async () => {
      return sharedUtil();
    }),
});
`,
    );

    await writeFile(
      join(root, 'src/routes/root.ts'),
      `import { projectRouter } from "./project";

export const appRouter = {
  project: projectRouter,
};
`,
    );

    await writeFile(
      join(root, 'src/trpc.ts'),
      `export function createTRPCRouter(value: unknown) {
  return value;
}
`,
    );

    const idx = new CodeIndex(root, { cachePath: '.code-index-cache/test-index.json' });
    const focused = await idx.map({
      focusFiles: ['src/routes/project.ts'],
      structured: true,
      limit: 8,
    });

    assert.equal(focused.files[0]?.path, 'src/routes/project.ts');
    assert.ok(focused.files[0].symbols.some(symbol => symbol.name === 'getTeamStatistics'));
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
