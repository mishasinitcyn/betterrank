import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

import { parseFile } from '../src/parser.js';
import { CodeIndex } from '../src/index.js';

test('parseFile captures PascalCase JSX consumers and skips intrinsic tags', () => {
  const source = `export function App() {
  return (
    <div>
      <Providers>
        <I18nProvider />
      </Providers>
    </div>
  );
}
`;

  const parsed = parseFile('app.tsx', source);
  const refNames = parsed.references.map(ref => ref.name);

  assert.ok(refNames.includes('Providers'));
  assert.ok(refNames.includes('I18nProvider'));
  assert.ok(!refNames.includes('div'));
});

test('callers context includes JSX usage lines for components', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-jsx-callers-'));

  try {
    await mkdir(join(root, 'src/app'), { recursive: true });
    await mkdir(join(root, 'src/components'), { recursive: true });

    await writeFile(
      join(root, 'src/components/providers.tsx'),
      `export function Providers({ children }) {
  return <section>{children}</section>;
}
`,
    );

    await writeFile(
      join(root, 'src/app/layout.tsx'),
      `import { Providers } from "../components/providers";

export function RootLayout() {
  return (
    <Providers>
      <div />
    </Providers>
  );
}
`,
    );

    const idx = new CodeIndex(root, { cachePath: '.code-index-cache/test-index.json' });
    const callers = await idx.callers({ symbol: 'Providers', context: 1 });

    assert.equal(callers.length, 1);
    assert.equal(callers[0].file, 'src/app/layout.tsx');
    assert.ok(callers[0].sites.some(site => site.text.some(line => line.content.includes('<Providers>'))));
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
