import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

import { CodeIndex } from '../src/index.js';

test('context resolves property symbols with scoped references and callers', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-property-context-'));

  try {
    await mkdir(join(root, 'src/server/api/routers'), { recursive: true });
    await mkdir(join(root, 'src/app'), { recursive: true });

    await writeFile(
      join(root, 'src/server/api/routers/project.ts'),
      `function loadStats() {
  return 1;
}

export const projectRouter = createTRPCRouter({
  getTeamStatistics: protectedProcedure
    .query(async () => {
      return loadStats();
    }),
});
`,
    );

    await writeFile(
      join(root, 'src/app/screen.tsx'),
      `export function Screen(api: any) {
  return api.project.getTeamStatistics.useQuery();
}
`,
    );

    const idx = new CodeIndex(root, { cachePath: '.code-index-cache/test-index.json' });
    const result = await idx.context({ symbol: 'getTeamStatistics' });

    assert.ok(result);
    assert.equal(result.definition.kind, 'property');
    assert.ok(result.usedSymbols.some(symbol => symbol.name === 'loadStats'));
    assert.deepEqual(result.callers, ['src/app/screen.tsx']);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
