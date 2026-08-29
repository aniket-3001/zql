/*
 * The playground.
 *
 * Loads zql's engine as WebAssembly and runs every query the visitor types
 * through it — the same `sql::parse`, `plan::bind` and `exec::execute` the
 * server calls over a socket. Nothing here is simulated, and nothing is
 * fetched from anywhere but this origin.
 *
 * The fixtures are the repository's own test fixtures, mounted into an
 * in-memory filesystem by wasi.js. `sqlite('/demo/places.sqlite', 'moz_places')`
 * on this page walks a real b-tree in a real SQLite file.
 */
'use strict';

import { WASI } from './wasi.js';

const $ = (sel) => document.querySelector(sel);
const el = (tag, cls, text) => {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined) n.textContent = text;
  return n;
};

let engine = null;

// --------------------------------------------------------------- the engine

async function boot() {
  const status = $('#status');
  try {
    status.textContent = 'loading the engine…';

    // The fixture list comes from the build rather than being written here.
    // It was hardcoded once, and adding a file to the demo then meant the page
    // silently did not mount it — the query worked locally and failed on the
    // deployed site with a "no such file" that pointed at nothing obvious.
    // The build knows what it staged; the page asks it.
    const names = await (await fetch('fixtures/index.json')).json();
    const files = {};
    await Promise.all(names.map(async (name) => {
      const res = await fetch(`fixtures/${name}`);
      if (!res.ok) throw new Error(`fixtures/${name}: ${res.status}`);
      files[name] = new Uint8Array(await res.arrayBuffer());
    }));

    // The tree passed here *is* the mount, not a directory containing it:
    // `path_open` receives paths already relative to the preopen, so nesting a
    // `demo` key inside would put every fixture one level too deep — which
    // `files('/demo')` would still find by recursing, and `sqlite()` would not.
    const wasi = new WASI(files, (text) => console.debug('[zql]', text.trim()));
    const res = await fetch('zql.wasm');
    if (!res.ok) throw new Error(`zql.wasm: ${res.status}`);

    const { instance } = await WebAssembly.instantiate(await res.arrayBuffer(), wasi.imports());
    wasi.bind(instance);
    // A reactor module: `_initialize` sets up the runtime without running a
    // main that would then exit and take the exports with it.
    if (instance.exports._initialize) instance.exports._initialize();

    engine = makeEngine(instance);
    status.textContent = '';
    status.classList.add('ready');
    $('#run').disabled = false;
    document.body.classList.add('engine-ready');
    return true;
  } catch (err) {
    status.textContent = `the engine did not load: ${err.message}`;
    status.classList.add('failed');
    return false;
  }
}

function makeEngine(instance) {
  const { alloc, dealloc, query, memory } = instance.exports;
  const enc = new TextEncoder();
  const dec = new TextDecoder();

  return function run(sql) {
    const bytes = enc.encode(sql);
    const ptr = alloc(bytes.length);
    new Uint8Array(memory.buffer, ptr, bytes.length).set(bytes);

    const out = query(ptr, bytes.length);
    dealloc(ptr, bytes.length);

    // The reply is length-prefixed because the boundary is raw bytes: there is
    // no wasm-bindgen here to hand a string across for us.
    const len = new DataView(memory.buffer).getUint32(out, true);
    const json = dec.decode(new Uint8Array(memory.buffer, out + 4, len));
    dealloc(out, 4 + len);
    return JSON.parse(json);
  };
}

// --------------------------------------------------------------- rendering

function render(sql, result) {
  const out = $('#output');
  out.innerHTML = '';

  if (result.kind === 'error') {
    out.appendChild(renderError(sql, result));
    return;
  }
  if (result.kind === 'empty') {
    out.appendChild(el('p', 'note', 'Empty query — the protocol answers this with EmptyQueryResponse, not an error.'));
    return;
  }

  const table = el('table', 'result');
  const thead = el('thead');
  const hr = el('tr');
  for (const col of result.columns) {
    const th = el('th');
    th.appendChild(el('span', 'col-name', col.name));
    // The OID is the one that would go out in RowDescription. A client parsing
    // results by type sees exactly this.
    th.appendChild(el('span', 'col-type', `${col.type} · oid ${col.oid}`));
    hr.appendChild(th);
  }
  thead.appendChild(hr);
  table.appendChild(thead);

  const tbody = el('tbody');
  for (const row of result.rows) {
    const tr = el('tr');
    for (let i = 0; i < row.length; i++) {
      const td = el('td');
      if (row[i] === null) {
        // NULL is a length of -1 on the wire and genuinely absent here, so the
        // page can draw it as different from an empty string — because it is.
        td.appendChild(el('span', 'null', 'NULL'));
      } else {
        td.textContent = row[i];
        if (['integer', 'real', 'timestamp'].includes(result.columns[i]?.type)) td.className = 'num';
      }
      tr.appendChild(td);
    }
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);

  if (result.rows.length === 0) {
    out.appendChild(el('p', 'note', 'No rows.'));
  } else {
    const scroller = el('div', 'scroller');
    scroller.appendChild(table);
    out.appendChild(scroller);
  }

  const meta = el('p', 'meta');
  meta.textContent = `${result.tag} · ${(result.micros / 1000).toFixed(2)} ms`
    + (result.truncated ? ' · truncated at 500 rows for the page' : '');
  out.appendChild(meta);

  if (result.plan && result.plan.length) {
    const details = el('details', 'plan');
    details.appendChild(el('summary', null, 'the plan the binder built'));
    details.appendChild(el('pre', null, result.plan.join('\n')));
    out.appendChild(details);
  }
}

function renderError(sql, result) {
  const box = el('div', 'error');
  const head = el('p', 'error-head');
  head.appendChild(el('span', 'sqlstate', result.code));
  head.appendChild(el('span', 'error-msg', result.message));
  box.appendChild(head);

  // psql draws this caret from the P field of ErrorResponse. The engine
  // populates it, so the page can draw the same thing.
  if (result.position) {
    const line = sql.split('\n')[0];
    const caret = ' '.repeat(Math.max(0, result.position - 1)) + '^';
    box.appendChild(el('pre', 'caret', `${line}\n${caret}`));
  }
  if (result.detail) box.appendChild(el('p', 'detail', `DETAIL:  ${result.detail}`));
  if (result.hint) box.appendChild(el('p', 'hint', `HINT:  ${result.hint}`));
  return box;
}

// ------------------------------------------------------------------ running

function runCurrent() {
  if (!engine) return;
  const sql = $('#sql').value;
  const started = performance.now();
  const result = engine(sql);
  const elapsed = performance.now() - started;
  render(sql, result);
  $('#roundtrip').textContent = `${elapsed.toFixed(1)} ms including the call across the boundary`;
}

function setQuery(sql, run = true) {
  $('#sql').value = sql;
  if (run) runCurrent();
  $('#sql').focus();
}

// ------------------------------------------------------------ present mode
//
// The same page, projected. Sections become slides rather than a second
// artifact that can drift from the first.

function present() {
  const on = document.body.classList.toggle('presenting');
  if (on) {
    const slides = [...document.querySelectorAll('[data-slide]')];
    let at = 0;
    const show = () => slides.forEach((s, i) => s.classList.toggle('current', i === at));
    show();
    const key = (e) => {
      if (e.key === 'ArrowRight' || e.key === ' ' || e.key === 'PageDown') { at = Math.min(at + 1, slides.length - 1); show(); e.preventDefault(); }
      else if (e.key === 'ArrowLeft' || e.key === 'PageUp') { at = Math.max(at - 1, 0); show(); e.preventDefault(); }
      else if (e.key === 'Escape') { document.body.classList.remove('presenting'); document.removeEventListener('keydown', key); slides.forEach((s) => s.classList.remove('current')); }
    };
    document.addEventListener('keydown', key);
  }
}

// --------------------------------------------------------------------- wire

window.addEventListener('DOMContentLoaded', async () => {
  $('#run').addEventListener('click', runCurrent);
  $('#sql').addEventListener('keydown', (e) => {
    // Ctrl/Cmd+Enter runs, which is what every SQL console does.
    if ((e.ctrlKey || e.metaKey) && e.key === 'Enter') { runCurrent(); e.preventDefault(); }
  });
  document.querySelectorAll('[data-q]').forEach((b) => {
    b.addEventListener('click', () => setQuery(b.dataset.q));
  });
  $('#present')?.addEventListener('click', present);

  const ok = await boot();
  if (ok) setQuery($('#sql').dataset.initial || "SELECT 'hello' AS greeting", true);
});

// Exposed for scripts/verify-web.js, which drives this page in a real browser
// before it is allowed to deploy.
window.__zql = {
  run: (sql) => (engine ? engine(sql) : null),
  ready: () => engine !== null,
};
