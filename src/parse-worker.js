import { parentPort, workerData } from 'worker_threads';
import { readFile } from 'fs/promises';
import { join } from 'path';
import { parseFile } from './parser.js';
import { isMinifiedSource } from './walk.js';

function dedupeReferences(references) {
  const seen = new Set();
  const out = [];
  for (const r of references) {
    const key = `${r.name}\0${r.kind}\0${r.receiver || ''}\0${r.receiverModule || ''}\0${r.isRpcChain ? 1 : 0}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({
      name: r.name,
      kind: r.kind,
      receiver: r.receiver || null,
      receiverModule: r.receiverModule || null,
      isRpcChain: !!r.isRpcChain,
    });
  }
  return out;
}

function dedupeImports(imports) {
  const seen = new Set();
  const out = [];
  for (const imp of imports) {
    const key = `${imp.path}\0${imp.isReexport ? 1 : 0}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({ path: imp.path, isReexport: !!imp.isReexport });
  }
  return out;
}

async function run() {
  const { rootPath, items } = workerData;
  const results = [];
  let skippedCount = 0;

  const BATCH_SIZE = 32;
  for (let i = 0; i < items.length; i += BATCH_SIZE) {
    const batch = items.slice(i, i + BATCH_SIZE);
    const loaded = await Promise.all(
      batch.map(async entry => {
        try {
          const source = await readFile(join(rootPath, entry.relPath), 'utf-8');
          return { ...entry, source };
        } catch {
          return null;
        }
      }),
    );

    for (const item of loaded) {
      if (!item) continue;
      const { graphPath, relPath, currentMtime, source } = item;
      if (isMinifiedSource(source, graphPath)) {
        skippedCount++;
        results.push({ graphPath, relPath, currentMtime, skipped: true });
        continue;
      }
      try {
        const res = parseFile(graphPath, source);
        if (res) {
          results.push({
            graphPath,
            relPath,
            currentMtime,
            skipped: false,
            definitions: res.definitions,
            references: dedupeReferences(res.references),
            imports: dedupeImports(res.imports),
          });
        }
      } catch {
        // skip unparseable files
      }
    }
  }

  parentPort.postMessage({ results, skippedCount });
}

run().catch(err => {
  setImmediate(() => { throw err; });
});
