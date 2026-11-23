#!/usr/bin/env node
import { createWriteStream } from 'fs';
import { chmod, copyFile, access, mkdir } from 'fs/promises';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';
import { readFile } from 'fs/promises';
import { Readable } from 'stream';
import { pipeline } from 'stream/promises';

const __dirname = dirname(fileURLToPath(import.meta.url));
const PKG_ROOT = join(__dirname, '..');
const TARGET_BIN = join(PKG_ROOT, 'bin', 'betterrank-native');

const PLATFORM_MAP = {
  'linux-x64': 'betterrank-linux-x64',
  'darwin-arm64': 'betterrank-darwin-arm64',
  'darwin-x64': 'betterrank-darwin-x64',
};

async function exists(p) {
  try {
    await access(p);
    return true;
  } catch {
    return false;
  }
}

async function main() {
  if (await exists(TARGET_BIN)) return;

  // Local dev checkout: copy compiled cargo binary if present
  const localRelease = join(PKG_ROOT, 'target', 'release', 'betterrank');
  if (await exists(localRelease)) {
    await mkdir(dirname(TARGET_BIN), { recursive: true });
    await copyFile(localRelease, TARGET_BIN);
    await chmod(TARGET_BIN, 0o755);
    return;
  }

  const key = `${process.platform}-${process.arch}`;
  const asset = PLATFORM_MAP[key];
  if (!asset) {
    console.error(`betterrank: unsupported platform ${key} (supported: ${Object.keys(PLATFORM_MAP).join(', ')})`);
    process.exit(1);
  }

  const pkg = JSON.parse(await readFile(join(PKG_ROOT, 'package.json'), 'utf-8'));
  const url = `https://github.com/mishasinitcyn/betterrank/releases/download/v${pkg.version}/${asset}`;

  const res = await fetch(url, { redirect: 'follow' });
  if (!res.ok || !res.body) {
    console.error(`betterrank: failed to download native binary from ${url} (HTTP ${res.status})`);
    process.exit(1);
  }

  await mkdir(dirname(TARGET_BIN), { recursive: true });
  const tmpPath = `${TARGET_BIN}.tmp.${process.pid}`;
  await pipeline(Readable.fromWeb(res.body), createWriteStream(tmpPath));
  await chmod(tmpPath, 0o755);
  const { rename } = await import('fs/promises');
  await rename(tmpPath, TARGET_BIN);
}

main().catch(err => {
  console.error(`betterrank postinstall failed: ${err.message}`);
  process.exit(1);
});
