/*
 * Drives the playground in a real browser and fails if it does not work.
 *
 * The page is the one artifact no other suite covers: it loads zql's engine as
 * WebAssembly onto a hand-written WASI host, and neither of those is exercised
 * by `cargo test`. A page that deploys but answers wrongly — or silently fails
 * to start the module and shows an empty console — is worse than one that fails
 * to deploy, so this runs before publishing and exits non-zero on any of:
 *
 *   - the engine failing to load
 *   - any console error
 *   - any expected answer coming back wrong
 *   - the SQLite reader failing to read a real database file
 *
 * The answers below are not "whatever the page returns today". They are the
 * values `sqlite3` and the test suite already assert, restated here so a
 * regression in the wasm build fails rather than deploying quietly.
 *
 * Usage:  node scripts/verify-web.js
 */
'use strict';

const http = require('http');
const fs = require('fs');
const path = require('path');
const { execFileSync, spawn } = require('child_process');

const root = path.join(__dirname, '..');
const web = path.join(root, 'web');
const PORT = Number(process.env.WEB_PORT || 8139);

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.wasm': 'application/wasm',
  '.db': 'application/octet-stream',
  '.sqlite': 'application/octet-stream',
  '.csv': 'text/csv',
  '.txt': 'text/plain; charset=utf-8',
};

function chromePath() {
  if (process.env.CHROME_PATH) return process.env.CHROME_PATH;
  const candidates = process.platform === 'win32'
    ? ['C:/Program Files/Google/Chrome/Application/chrome.exe',
       'C:/Program Files (x86)/Google/Chrome/Application/chrome.exe',
       'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe']
    : ['/usr/bin/google-chrome', '/usr/bin/chromium-browser', '/usr/bin/chromium',
       '/opt/google/chrome/chrome'];
  return candidates.find((p) => fs.existsSync(p));
}

// Every check is a query and something that must be true of the answer. The
// expectations come from the test suite and from sqlite3, not from this page.
const CHECKS = [
  ['the engine answers at all', "SELECT 1 AS n",
    (r) => r.kind === 'rows' && r.rows[0][0] === '1'],

  ['NULL is absent, not empty', "SELECT NULL AS a, '' AS b",
    (r) => r.rows[0][0] === null && r.rows[0][1] === ''],

  ['three-valued logic', "SELECT (TRUE OR NULL), (FALSE AND NULL), (NULL=NULL), (NULL IS NULL)",
    (r) => r.rows[0][0] === 't' && r.rows[0][1] === 'f'
        && r.rows[0][2] === null && r.rows[0][3] === 't'],

  ['division widens', "SELECT 3/2 AS d",
    (r) => r.rows[0][0] === '1.5'],

  ['wire OIDs are advertised', "SELECT 1 AS i, 1.5 AS f, 'x' AS t, true AS b",
    (r) => JSON.stringify(r.columns.map((c) => c.oid)) === '[20,701,25,16]'],

  ['scalar functions', "SELECT lower('ABC'), length('写真'), substr('hello',2,3), round(2.567,2)",
    (r) => r.rows[0][0] === 'abc' && r.rows[0][1] === '2'
        && r.rows[0][2] === 'ell' && r.rows[0][3] === '2.57'],

  // The headline. If this fails the page is not demonstrating the project.
  ['SQLite: a real b-tree walk', "SELECT COUNT(*) AS n FROM sqlite('/demo/places.sqlite','moz_places')",
    (r) => r.rows[0][0] === '422'],

  ['SQLite: INTEGER PRIMARY KEY is the rowid, not NULL',
    "SELECT id FROM sqlite('/demo/places.sqlite','moz_places') ORDER BY id LIMIT 1",
    (r) => r.rows[0][0] === '1'],

  ['SQLite: REAL affinity survives',
    "SELECT frecency FROM sqlite('/demo/places.sqlite','moz_places') WHERE id = 422",
    (r) => r.columns[0].oid === 701 && r.rows[0][0] === '0'],

  ['SQLite: NULL and empty title stay different',
    "SELECT title FROM sqlite('/demo/places.sqlite','moz_places') WHERE id IN (421,422) ORDER BY id",
    (r) => r.rows[0][0] === null && r.rows[1][0] === ''],

  ['SQLite: the index is skipped, not read as rows',
    "SELECT COUNT(*) FROM sqlite('/demo/places.sqlite','moz_bookmarks')",
    (r) => r.rows[0][0] === '60'],

  ['SQLite: a missing table lists the real ones',
    "SELECT * FROM sqlite('/demo/places.sqlite','nope')",
    (r) => r.kind === 'error' && r.code === '42P01' && /moz_places/.test(r.hint || '')],

  ['SQLite: awkward values from hard.db',
    "SELECT \"visitCount\" FROM sqlite('/demo/hard.db','quirks')",
    (r) => r.kind === 'rows' && r.rows[0][0] === '42'],

  ['GROUP BY and HAVING',
    "SELECT visit_count > 70 AS busy, COUNT(*) AS n FROM sqlite('/demo/places.sqlite','moz_places') GROUP BY visit_count > 70",
    (r) => r.kind === 'rows' && r.rows.length === 2],

  ['a join across two tables',
    "SELECT COUNT(*) FROM sqlite('/demo/places.sqlite','moz_places') AS p JOIN sqlite('/demo/places.sqlite','moz_bookmarks') AS b ON p.id = b.fk",
    (r) => r.kind === 'rows' && Number(r.rows[0][0]) > 0],

  // The directory walk is the piece Node's own WASI could not do, so it is the
  // piece most worth asserting: it is the hand-written fd_readdir under test.
  ['files(): the hand-written fd_readdir works',
    "SELECT COUNT(*) AS n FROM files('/demo')",
    (r) => r.kind === 'rows' && Number(r.rows[0][0]) >= 4],

  ['csv(): quoting and type sniffing', "SELECT * FROM csv('/demo/owners.csv')",
    (r) => r.kind === 'rows' && r.rows.length > 0],

  ['SHOW SOURCES lists five', "SHOW SOURCES",
    (r) => r.rows.length === 5],

  ['EXPLAIN prints a plan', "EXPLAIN SELECT ext, COUNT(*) FROM files('/demo') GROUP BY ext",
    (r) => r.kind === 'rows' && r.rows.some((x) => /Aggregate/.test(x[0]))],

  ['a syntax error carries a caret position', "SELECT * form files",
    (r) => r.kind === 'error' && r.code === '42601' && r.position > 0 && /FROM/.test(r.hint || '')],

  ['writes are refused by name', "INSERT INTO t VALUES (1)",
    (r) => r.kind === 'error' && r.code === '0A000' && /data modification/.test(r.message)],

  ['the depth limit holds', "SELECT " + "1+".repeat(5000) + "1",
    (r) => r.kind === 'error' && r.code === '54001'],

  ['division by zero', "SELECT 1/0",
    (r) => r.kind === 'error' && r.code === '22012'],

  ['an empty query is not an error', "   ",
    (r) => r.kind === 'empty'],
];

// --------------------------------------------------------------------- serve

function serve() {
  return new Promise((resolve) => {
    const server = http.createServer((req, res) => {
      const rel = decodeURIComponent(req.url.split('?')[0]).replace(/^\/+/, '') || 'index.html';
      const file = path.join(web, rel);
      if (!file.startsWith(web) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) {
        res.writeHead(404); res.end('not found'); return;
      }
      res.writeHead(200, { 'Content-Type': TYPES[path.extname(file)] || 'application/octet-stream' });
      fs.createReadStream(file).pipe(res);
    });
    server.listen(PORT, () => resolve(server));
  });
}

// ------------------------------------------------------------------- drive

async function main() {
  for (const required of ['index.html', 'app.js', 'wasi.js', 'style.css', 'zql.wasm']) {
    if (!fs.existsSync(path.join(web, required))) {
      console.error(`missing web/${required} — run: node scripts/build-web.js`);
      process.exit(1);
    }
  }

  const chrome = chromePath();
  if (!chrome) {
    console.error('no Chrome or Chromium found. Set CHROME_PATH.');
    process.exit(1);
  }

  const server = await serve();
  const profile = fs.mkdtempSync(path.join(require('os').tmpdir(), 'zql-verify-'));
  const browser = spawn(chrome, [
    '--headless=new', '--disable-gpu', '--no-sandbox',
    '--remote-debugging-port=0', `--user-data-dir=${profile}`,
    'about:blank',
  ], { stdio: ['ignore', 'ignore', 'pipe'] });

  // Chrome prints the DevTools endpoint on stderr; there is no other way to
  // learn the port when it was asked to choose one.
  const endpoint = await new Promise((resolve, reject) => {
    let buf = '';
    const timer = setTimeout(() => reject(new Error('chrome did not report a debugging port')), 30000);
    browser.stderr.on('data', (chunk) => {
      buf += chunk;
      const m = buf.match(/ws:\/\/[^\s]+/);
      if (m) { clearTimeout(timer); resolve(m[0]); }
    });
  });

  const failures = [];
  try {
    const { runInBrowser } = require('./cdp.js');
    const result = await runInBrowser(endpoint, `http://127.0.0.1:${PORT}/`, CHECKS);
    failures.push(...result.failures);
    console.log(result.log.join('\n'));
  } finally {
    browser.kill();
    server.close();
    // Chrome releases its profile lazily, and on Windows the unlink races its
    // shutdown. A temp directory left behind is not a reason to fail a run that
    // has already answered the question it was asked.
    try {
      await new Promise((r) => setTimeout(r, 300));
      fs.rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
    } catch { /* the OS will collect it */ }
  }

  if (failures.length) {
    console.error(`\n${failures.length} check(s) failed:`);
    for (const f of failures) console.error(`  - ${f}`);
    process.exit(1);
  }
  console.log(`\nall ${CHECKS.length} checks passed in a real browser`);
}

main().catch((err) => { console.error(err); process.exit(1); });
