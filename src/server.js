import { createServer } from 'http';
import { readFile } from 'fs/promises';
import { fileURLToPath } from 'url';
import { dirname, join } from 'path';
import { CodeIndex } from './index.js';

const __dirname = dirname(fileURLToPath(import.meta.url));

let currentIndex = null;
let currentRoot = null;
let currentStats = null;

function json(res, data, status = 200) {
  res.writeHead(status, { 'Content-Type': 'application/json' });
  res.end(JSON.stringify(data));
}

function error(res, msg, status = 400) {
  json(res, { error: msg }, status);
}

function params(url) {
  const u = new URL(url, 'http://localhost');
  const get = (k, fallback) => {
    const v = u.searchParams.get(k);
    return v === null ? fallback : v;
  };
  const getInt = (k, fallback) => {
    const v = u.searchParams.get(k);
    return v === null ? fallback : parseInt(v, 10);
  };
  return { get, getInt, path: u.pathname };
}

async function readBody(req) {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  return JSON.parse(Buffer.concat(chunks).toString());
}

function requireIndex(res) {
  if (!currentIndex) {
    error(res, 'No repo indexed. POST /api/index first.', 400);
    return false;
  }
  return true;
}

const routes = {

  'GET /api/open': async (req, res) => {
    if (!currentRoot) return error(res, 'No repo indexed');
    const p = params(req.url);
    const file = p.get('file', '');
    const line = p.get('line', '');
    const ide = p.get('ide', 'cursor');
    if (!file) return error(res, 'file is required');

    const fullPath = join(currentRoot, file);
    const target = line ? `${fullPath}:${line}` : fullPath;

    const { exec: execCb } = await import('child_process');
    const cliMap = {
      cursor: 'cursor',
      vscode: 'code',
      'vscode-insiders': 'code-insiders',
    };
    const cli = cliMap[ide];

    if (cli) {
      execCb(`${cli} --goto "${target}"`, (err) => {
        if (err) {
          // CLI not found, fall back to OS open
          execCb(`open "${fullPath}"`);
        }
      });
    } else {
      execCb(`open "${fullPath}"`);
    }

    json(res, { ok: true });
  },

  'POST /api/browse': async (_req, res) => {
    const { exec } = await import('child_process');
    const { promisify } = await import('util');
    const execAsync = promisify(exec);

    try {
      let cmd;
      switch (process.platform) {
        case 'darwin':
          cmd = `osascript -e 'POSIX path of (choose folder with prompt "Select a repository folder")'`;
          break;
        case 'win32':
          cmd = `powershell -NoProfile -Command "Add-Type -AssemblyName System.Windows.Forms; $d = New-Object System.Windows.Forms.FolderBrowserDialog; $d.Description = 'Select a repository folder'; if ($d.ShowDialog() -eq 'OK') { $d.SelectedPath }"`;
          break;
        default:
          cmd = `zenity --file-selection --directory --title="Select a repository folder" 2>/dev/null || kdialog --getexistingdirectory ~ 2>/dev/null`;
      }

      const { stdout } = await execAsync(cmd, { timeout: 120000 });
      const folderPath = stdout.trim().replace(/\/$/, '');
      if (!folderPath) return error(res, 'No folder selected');
      json(res, { path: folderPath });
    } catch {
      error(res, 'Cancelled', 400);
    }
  },

  'POST /api/index': async (req, res) => {
    const body = await readBody(req);
    const root = body.root;
    if (!root) return error(res, 'root is required');

    try {
      currentIndex = new CodeIndex(root);
      const t0 = Date.now();
      await currentIndex.reindex();
      const elapsed = Date.now() - t0;
      currentRoot = root;
      currentStats = await currentIndex.stats();
      json(res, { root, stats: currentStats, elapsed });
    } catch (e) {
      currentIndex = null;
      currentRoot = null;
      error(res, e.message, 500);
    }
  },

  'GET /api/stats': async (_req, res) => {
    if (!requireIndex(res)) return;
    const stats = await currentIndex.stats();
    json(res, { root: currentRoot, stats });
  },

  'GET /api/map': async (req, res) => {
    if (!requireIndex(res)) return;
    const p = params(req.url);
    const focus = p.get('focus', '');
    const focusFiles = focus ? focus.split(',') : [];
    const format = p.get('format', 'text');
    const result = await currentIndex.map({
      focusFiles,
      offset: p.getInt('offset', undefined),
      limit: p.getInt('limit', 50),
      structured: format === 'structured',
    });
    json(res, result);
  },

  'GET /api/search': async (req, res) => {
    if (!requireIndex(res)) return;
    const p = params(req.url);
    const query = p.get('q', '');
    if (!query) return error(res, 'q is required');
    const results = await currentIndex.search({
      query,
      kind: p.get('kind', undefined),
      offset: p.getInt('offset', undefined),
      limit: p.getInt('limit', 20),
    });
    // Get total count for pagination
    const total = await currentIndex.search({
      query,
      kind: p.get('kind', undefined),
      count: true,
    });
    json(res, { results, total: total.total });
  },

  'GET /api/symbols': async (req, res) => {
    if (!requireIndex(res)) return;
    const p = params(req.url);
    const results = await currentIndex.symbols({
      file: p.get('file', undefined),
      kind: p.get('kind', undefined),
      offset: p.getInt('offset', undefined),
      limit: p.getInt('limit', 20),
    });
    const total = await currentIndex.symbols({
      file: p.get('file', undefined),
      kind: p.get('kind', undefined),
      count: true,
    });
    json(res, { results, total: total.total });
  },

  'GET /api/callers': async (req, res) => {
    if (!requireIndex(res)) return;
    const p = params(req.url);
    const symbol = p.get('symbol', '');
    if (!symbol) return error(res, 'symbol is required');
    const results = await currentIndex.callers({
      symbol,
      file: p.get('file', undefined),
      offset: p.getInt('offset', undefined),
      limit: p.getInt('limit', 20),
    });
    const total = await currentIndex.callers({
      symbol,
      file: p.get('file', undefined),
      count: true,
    });
    json(res, { results, total: total.total });
  },

  'GET /api/dependents': async (req, res) => {
    if (!requireIndex(res)) return;
    const p = params(req.url);
    const file = p.get('file', '');
    if (!file) return error(res, 'file is required');
    const results = await currentIndex.dependents({
      file,
      offset: p.getInt('offset', undefined),
      limit: p.getInt('limit', 20),
    });
    const total = await currentIndex.dependents({ file, count: true });
    json(res, { results, total: total.total });
  },

  'GET /api/deps': async (req, res) => {
    if (!requireIndex(res)) return;
    const p = params(req.url);
    const file = p.get('file', '');
    if (!file) return error(res, 'file is required');
    const results = await currentIndex.dependencies({
      file,
      offset: p.getInt('offset', undefined),
      limit: p.getInt('limit', 20),
    });
    const total = await currentIndex.dependencies({ file, count: true });
    json(res, { results, total: total.total });
  },

  'GET /api/neighborhood': async (req, res) => {
    if (!requireIndex(res)) return;
    const p = params(req.url);
    const file = p.get('file', '');
    if (!file) return error(res, 'file is required');
    const result = await currentIndex.neighborhood({
      file,
      hops: p.getInt('hops', 2),
      maxFiles: p.getInt('maxFiles', 15),
      offset: p.getInt('offset', undefined),
      limit: p.getInt('limit', undefined),
    });
    json(res, result);
  },

  'GET /api/structure': async (req, res) => {
    if (!requireIndex(res)) return;
    const p = params(req.url);
    const result = await currentIndex.structure({
      depth: p.getInt('depth', 3),
    });
    json(res, { content: result });
  },
};

export function startServer(port = 3333) {
  const server = createServer(async (req, res) => {
    // CORS
    res.setHeader('Access-Control-Allow-Origin', '*');
    res.setHeader('Access-Control-Allow-Methods', 'GET, POST, OPTIONS');
    res.setHeader('Access-Control-Allow-Headers', 'Content-Type');
    if (req.method === 'OPTIONS') { res.writeHead(204); res.end(); return; }

    // Serve UI
    if (req.url === '/' || req.url === '/index.html') {
      try {
        const html = await readFile(join(__dirname, 'ui.html'), 'utf-8');
        res.writeHead(200, { 'Content-Type': 'text/html' });
        res.end(html);
      } catch {
        res.writeHead(500); res.end('Failed to load UI');
      }
      return;
    }

    // API routes
    const routeKey = `${req.method} ${params(req.url).path}`;
    const handler = routes[routeKey];
    if (handler) {
      try {
        await handler(req, res);
      } catch (e) {
        error(res, e.message, 500);
      }
    } else {
      res.writeHead(404);
      res.end('Not found');
    }
  });

  server.listen(port, () => {
    console.log(`betterrank ui running at http://localhost:${port}`);
  });

  return server;
}
