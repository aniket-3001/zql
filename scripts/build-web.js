/*
 * Builds the playground in web/.
 *
 * The page runs zql's real engine, so the engine has to be compiled for the
 * browser: bridge/ to wasm32-wasip1, which is the target that keeps std::fs
 * working so the SQLite reader can open a file. The .wasm and the fixtures are
 * build output and are not checked in — a stale copy of either would mean the
 * deployed page ran something other than what this repository contains.
 *
 * Usage:  node scripts/build-web.js
 */
'use strict';

const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');

const root = path.join(__dirname, '..');
const web = path.join(root, 'web');
const fixtures = path.join(web, 'fixtures');

const TARGET = 'wasm32-wasip1';
const kb = (p) => (fs.statSync(p).size / 1024).toFixed(0);

function run(cmd, args, opts = {}) {
  console.log(`  ${cmd} ${args.join(' ')}`);
  execFileSync(cmd, args, { cwd: root, stdio: 'inherit', ...opts });
}

// The toolchain is whatever the caller has. CI uses stable; locally this is the
// pinned 1.97.1-gnu. Both produce the same page, because the bridge is compiled
// for wasm and the host toolchain only has to be able to target it.
console.log(`compiling the engine to WebAssembly (${TARGET})`);
try {
  run('rustup', ['target', 'add', TARGET]);
} catch {
  console.log('  (rustup not available; assuming the target is installed)');
}
run('cargo', ['build', '--release', '--target', TARGET], { cwd: path.join(root, 'bridge') });

const wasm = path.join(root, 'bridge', 'target', TARGET, 'release', 'zql_bridge.wasm');
if (!fs.existsSync(wasm)) {
  console.error(`\nThe bridge did not produce ${wasm}`);
  process.exit(1);
}
fs.copyFileSync(wasm, path.join(web, 'zql.wasm'));
console.log(`  web/zql.wasm  ${kb(path.join(web, 'zql.wasm'))} KB`);

// The fixtures are the repository's own, copied rather than regenerated, so the
// page reads exactly the bytes the test suite reads.
console.log('staging the fixtures');
fs.mkdirSync(fixtures, { recursive: true });
for (const name of ['simple.db', 'hard.db', 'owners.csv']) {
  fs.copyFileSync(path.join(root, 'tests', 'fixtures', name), path.join(fixtures, name));
  console.log(`  web/fixtures/${name}  ${kb(path.join(fixtures, name))} KB`);
}

// The demo database is generated rather than committed: it is the one file that
// exists only for the page, and Python's sqlite3 writing it is the same
// argument the test fixtures make — a file zql wrote and then read would prove
// only that it agrees with itself.
console.log('generating the demo database');
const python = process.platform === 'win32' ? 'python' : 'python3';
run(python, [path.join('scripts', 'make-demo-db.py'), path.join('web', 'fixtures')]);

console.log('\nplayground built. serve web/ over http, or run: node scripts/verify-web.js');
