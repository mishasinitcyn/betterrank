import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';

import { CodeIndex } from '../src/index.js';
import { buildOutline } from '../src/outline.js';
import { parseFile } from '../src/parser.js';
import { isMinifiedSource } from '../src/walk.js';

const CLI_PATH = resolve(import.meta.dirname, '../src/cli.js');

test('isMinifiedSource detects minified bundles while allowing normal source files', () => {
  const normal = Array.from({ length: 200 }, (_, i) => `export function fn${i}(x: number) { return x + ${i}; }`).join('\n');
  assert.equal(isMinifiedSource(normal, 'src/normal.ts'), false);

  const minified = 'function it(e){return e}' + 'var a=1;'.repeat(1000);
  assert.equal(isMinifiedSource(minified, 'apps/demo/assets/quasar.js'), true);

  assert.equal(isMinifiedSource('export const x = 1;', 'vendor/lib.min.js'), true);
  assert.equal(isMinifiedSource('export const x = 1;', 'vendor/app.bundle.mjs'), true);
});

test('CodeIndex skips minified files, keeps them out of map, and caches skip mtime', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-minified-'));
  try {
    await mkdir(join(root, 'src'), { recursive: true });
    await mkdir(join(root, 'assets'), { recursive: true });

    await writeFile(
      join(root, 'src/app.ts'),
      `export function runApp(input: string) {
  return input.trim();
}
`,
    );

    const fakeBundle = 'function it(e){return e}' + 'function f' + Array.from({ length: 600 }, (_, i) => `a${i}(){return ${i}}`).join('');
    await writeFile(join(root, 'assets/quasar.js'), fakeBundle);

    const idx = new CodeIndex(root, { cachePath: '.cache/index.br.gz' });
    const first = await idx.map({ structured: true, limit: 20 });
    assert.deepEqual(first.files.map(f => f.path), ['src/app.ts']);

    // Second run with fresh instance should load gzip cache and treat quasar.js as unchanged (0 re-parses)
    const idx2 = new CodeIndex(root, { cachePath: '.cache/index.br.gz' });
    const ensureRes = await idx2.cache.ensure();
    assert.equal(ensureRes.changed, 0);
    assert.equal(ensureRes.deleted, 0);

    // Verify cache file is gzipped (0x1f 0x8b)
    const cacheBytes = await readFile(join(root, '.cache/index.br.gz'));
    assert.equal(cacheBytes[0], 0x1f);
    assert.equal(cacheBytes[1], 0x8b);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('outline keeps class bodies open and annotates both class header and individual methods', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-class-outline-'));
  try {
    await mkdir(join(root, 'src'), { recursive: true });

    const repoSource = `export class NotesRepository {
  constructor(private db: any) {}

  async read(id: string) {
    const row = await this.db.get(id);
    return row ?? null;
  }

  async rename(id: string, path: string) {
    const updated = await this.db.update(id, path);
    return updated;
  }

  async trash(id: string) {
    const removed = await this.db.remove(id);
    return removed;
  }
}
`;
    await writeFile(join(root, 'src/repository.ts'), repoSource);

    await writeFile(
      join(root, 'src/routes.ts'),
      `import { NotesRepository } from './repository';

export async function handleRoute(db: any) {
  const repo = new NotesRepository(db);
  await repo.read('1');
  return repo.rename('1', '/new');
}
`,
    );

    // Unrelated file calling url.read / bare trash() should NOT falsely wire to NotesRepository methods
    await writeFile(
      join(root, 'src/unrelated.ts'),
      `export function other(stream: any) {
  const x = stream.read;
  return trash();
}
`,
    );

    const idx = new CodeIndex(root, { cachePath: '.cache/index.br.gz' });
    const counts = await idx.getCallerCounts('src/repository.ts');

    assert.equal(counts.get('NotesRepository'), 1);
    assert.equal(counts.get('read'), 1);
    assert.equal(counts.get('rename'), 1);
    assert.equal(counts.get('trash'), undefined);

    const outline = buildOutline(repoSource, 'src/repository.ts', [], { callerCounts: counts });
    assert.match(outline, /export class NotesRepository \{.*← 1 caller/);
    assert.match(outline, /async read\(id: string\) \{/);
    assert.match(outline, /async rename\(id: string, path: string\) \{/);
    assert.match(outline, /async trash\(id: string\) \{/);
    assert.ok(!outline.includes('... (17 lines)'), 'class body must not collapse as a single block');
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test('parseFile preserves destructured parameter signatures and filters local nested helpers & literal maps', () => {
  const source = `const REF_QUERIES = {
  javascript: \`
    (call_expression) @ref
  \`,
};

export function parseConfig(filePath: string, { strict = true, depth = 2 } = {}) {
  const localHelper = (x: number) => {
    return x * 2;
  };
  return localHelper(depth);
}
`;

  const parsed = parseFile('src/config.ts', source);
  const names = parsed.definitions.map(d => d.name);

  assert.ok(names.includes('parseConfig'));
  assert.ok(!names.includes('localHelper'), 'nested local arrow functions should not pollute repo index');
  assert.ok(!names.includes('javascript'), 'literal-valued map entries should not be indexed as properties');

  const fn = parsed.definitions.find(d => d.name === 'parseConfig');
  assert.match(fn.signature, /\{ strict = true, depth = 2 \} = \{\}/);
});

test('CLI neighborhood returns files, imports, and symbols by default without requiring --limit', async () => {
  const root = await mkdtemp(join(tmpdir(), 'betterrank-cli-hood-'));
  try {
    await mkdir(join(root, 'src'), { recursive: true });
    await writeFile(
      join(root, 'src/lib.ts'),
      `export function helperUtil(x: number) {
  return x + 1;
}
`,
    );
    await writeFile(
      join(root, 'src/main.ts'),
      `import { helperUtil } from './lib';

export function entryPoint() {
  return helperUtil(42);
}
`,
    );

    const out = execFileSync(
      process.execPath,
      [CLI_PATH, 'neighborhood', 'src/main.ts', '--hops', '2', '--max-files', '10', '--root', root],
      { encoding: 'utf-8' },
    );

    assert.match(out, /Files \(2\):/);
    assert.match(out, /src\/main\.ts/);
    assert.match(out, /src\/lib\.ts/);
    assert.match(out, /Imports \(1\):/);
    assert.match(out, /Symbols \(2\):/);
    assert.ok(!out.includes('Use --limit N'));

    const countOut = execFileSync(
      process.execPath,
      [CLI_PATH, 'neighborhood', 'src/main.ts', '--count', '--root', root],
      { encoding: 'utf-8' },
    );
    assert.match(countOut, /^files: 2/m);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
