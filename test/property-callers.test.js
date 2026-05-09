import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

import { parseFile } from '../src/parser.js';
import { CodeIndex } from '../src/index.js';

test('parseFile captures property references from member chains', () => {
  const source = `export function Screen(api: any) {
  return api.project.getTeamStatistics.useQuery();
}
`;

  const parsed = parseFile('screen.tsx', source);
  const propertyRefs = parsed.references
    .filter(ref => ref.kind === 'property')
    .map(ref => ref.name);

  assert.ok(propertyRefs.includes('project'));
  assert.ok(propertyRefs.includes('getTeamStatistics'));
  assert.ok(propertyRefs.includes('useQuery'));
});

test('callers context includes property access lines for indexed router members', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-property-callers-'));

  try {
    await mkdir(join(root, 'src/server/api/routers'), { recursive: true });
    await mkdir(join(root, 'src/app'), { recursive: true });

    await writeFile(
      join(root, 'src/server/api/routers/project.ts'),
      `export const projectRouter = createTRPCRouter({
  getTeamStatistics: protectedProcedure
    .query(async () => {
      return {};
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
    const callers = await idx.callers({ symbol: 'getTeamStatistics', context: 1 });

    assert.equal(callers.length, 1);
    assert.equal(callers[0].file, 'src/app/screen.tsx');
    assert.ok(
      callers[0].sites.some(site =>
        site.text.some(line => line.content.includes('.getTeamStatistics.useQuery()')),
      ),
    );
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
