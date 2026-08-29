//! The dashboard page: one self-contained HTML document.
//!
//! No external stylesheet, no font, no script file, no favicon request. Partly
//! because there is no framework to serve them with, and partly because the
//! whole claim of this project is that it needs nothing installed — a page that
//! fetches a CDN would undercut that on camera.

/// The page, served at `/`.
pub fn html(port: u16) -> String {
    PAGE.replace("{{PORT}}", &port.to_string())
}

const PAGE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>zql</title>
<style>
  :root {
    color-scheme: light dark;
    --bg: #fbfbfa;      --panel: #ffffff;  --line: #e4e2dd;
    --ink: #1a1917;     --dim: #6b6862;    --accent: #b8551f;
    --ok: #2f7d4f;      --bad: #b3261e;
    --mono: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  }
  @media (prefers-color-scheme: dark) {
    :root {
      --bg: #16150f;    --panel: #1e1d16;  --line: #34322a;
      --ink: #f2efe6;   --dim: #9a968a;    --accent: #e8894f;
      --ok: #6cc08b;    --bad: #ef8b82;
    }
  }
  * { box-sizing: border-box; }
  body {
    margin: 0; padding: 2rem 1.5rem; background: var(--bg); color: var(--ink);
    font: 15px/1.55 ui-sans-serif, system-ui, -apple-system, Segoe UI, sans-serif;
  }
  .wrap { max-width: 1100px; margin: 0 auto; }
  header { display: flex; align-items: baseline; gap: .75rem; flex-wrap: wrap; }
  h1 { font-size: 1.5rem; margin: 0; letter-spacing: -.02em; }
  .tag { font: 600 11px/1 var(--mono); letter-spacing: .08em; text-transform: uppercase;
         color: var(--accent); border: 1px solid var(--accent); border-radius: 999px;
         padding: .3rem .5rem; }
  .sub { color: var(--dim); font-size: .875rem; margin: .35rem 0 1.5rem; }
  .stats { display: grid; gap: .75rem; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
           margin-bottom: 1.5rem; }
  .stat { background: var(--panel); border: 1px solid var(--line); border-radius: 10px;
          padding: .85rem 1rem; }
  .stat b { display: block; font: 600 1.6rem/1.1 var(--mono); letter-spacing: -.02em; }
  .stat span { color: var(--dim); font-size: .74rem; text-transform: uppercase;
               letter-spacing: .07em; }
  .live { display: inline-flex; align-items: center; gap: .4rem; color: var(--dim);
          font-size: .8rem; }
  .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--dim); }
  .dot.on { background: var(--ok); animation: pulse 2s ease-in-out infinite; }
  @keyframes pulse { 50% { opacity: .35; } }
  .panel { background: var(--panel); border: 1px solid var(--line); border-radius: 10px;
           overflow: hidden; }
  .scroll { overflow-x: auto; }
  table { border-collapse: collapse; width: 100%; font-size: .875rem; }
  th { text-align: left; font: 600 .72rem/1 var(--mono); letter-spacing: .07em;
       text-transform: uppercase; color: var(--dim); padding: .7rem .9rem;
       border-bottom: 1px solid var(--line); white-space: nowrap; }
  td { padding: .6rem .9rem; border-bottom: 1px solid var(--line); vertical-align: top; }
  tr:last-child td { border-bottom: 0; }
  td.sql { font-family: var(--mono); font-size: .82rem; max-width: 34rem;
           white-space: pre-wrap; word-break: break-word; }
  td.num { font-family: var(--mono); text-align: right; white-space: nowrap; }
  .badge { font: 600 .68rem/1 var(--mono); letter-spacing: .05em; text-transform: uppercase;
           padding: .25rem .45rem; border-radius: 5px; border: 1px solid var(--line);
           color: var(--dim); white-space: nowrap; }
  .badge.ok  { color: var(--ok); border-color: color-mix(in oklab, var(--ok) 45%, transparent); }
  .badge.bad { color: var(--bad); border-color: color-mix(in oklab, var(--bad) 45%, transparent); }
  .err { color: var(--bad); font-family: var(--mono); font-size: .78rem; }
  .empty { padding: 2.5rem 1rem; text-align: center; color: var(--dim); }
  tr.fresh { animation: land .5s ease-out; }
  @keyframes land { from { background: color-mix(in oklab, var(--accent) 14%, transparent); } }
  footer { color: var(--dim); font-size: .78rem; margin-top: 1.25rem; }
  code { font-family: var(--mono); }
</style>
</head>
<body>
<div class="wrap">
  <header>
    <h1>zql</h1>
    <span class="tag">zero dependencies</span>
    <span class="live"><span class="dot" id="dot"></span><span id="state">connecting…</span></span>
  </header>
  <p class="sub">Live query log. Point <code>psql</code> at port {{PORT}} and watch.</p>

  <div class="stats">
    <div class="stat"><b id="n-queries">0</b><span>queries</span></div>
    <div class="stat"><b id="n-rows">0</b><span>rows returned</span></div>
    <div class="stat"><b id="n-ms">0 ms</b><span>slowest query</span></div>
    <div class="stat"><b id="n-errors">0</b><span>errors</span></div>
  </div>

  <div class="panel">
    <div class="scroll">
      <table>
        <thead><tr>
          <th>Time</th><th>Query</th><th>Status</th>
          <th style="text-align:right">Rows</th><th style="text-align:right">Time</th><th>Index</th>
        </tr></thead>
        <tbody id="log"></tbody>
      </table>
    </div>
    <div class="empty" id="empty">No queries yet — run one and it will appear here.</div>
  </div>

  <footer>Streamed over server-sent events from a Rust binary with an empty
  <code>[dependencies]</code>.</footer>
</div>

<script>
  var log = document.getElementById('log');
  var empty = document.getElementById('empty');
  var queries = 0, rows = 0, errors = 0, slowest = 0;

  function text(node, value) { node.textContent = value; }

  function cell(row, value, className) {
    var td = document.createElement('td');
    if (className) td.className = className;
    td.textContent = value;
    row.appendChild(td);
    return td;
  }

  function add(event) {
    empty.style.display = 'none';
    queries++;
    rows += event.rows || 0;
    if (event.status !== 'ok') errors++;
    if (event.ms > slowest) slowest = event.ms;

    text(document.getElementById('n-queries'), queries);
    text(document.getElementById('n-rows'), rows.toLocaleString());
    text(document.getElementById('n-ms'), slowest + ' ms');
    text(document.getElementById('n-errors'), errors);

    var tr = document.createElement('tr');
    tr.className = 'fresh';
    cell(tr, event.at, 'num');

    var sql = cell(tr, event.sql, 'sql');
    if (event.detail) {
      var note = document.createElement('div');
      note.className = 'err';
      note.textContent = event.detail;
      sql.appendChild(note);
    }

    var status = document.createElement('td');
    var badge = document.createElement('span');
    badge.className = 'badge ' + (event.status === 'ok' ? 'ok' : 'bad');
    badge.textContent = event.status;
    status.appendChild(badge);
    tr.appendChild(status);

    cell(tr, event.status === 'ok' ? event.rows.toLocaleString() : '—', 'num');
    cell(tr, event.ms + ' ms', 'num');
    cell(tr, event.index || '—', 'num');

    log.insertBefore(tr, log.firstChild);
    // Newest first, and bounded: a long demo should not grow the DOM forever.
    while (log.children.length > 200) log.removeChild(log.lastChild);
  }

  var source = new EventSource('/events');
  source.onopen = function () {
    document.getElementById('dot').className = 'dot on';
    text(document.getElementById('state'), 'live');
  };
  source.onerror = function () {
    document.getElementById('dot').className = 'dot';
    text(document.getElementById('state'), 'reconnecting…');
  };
  source.onmessage = function (message) {
    try { add(JSON.parse(message.data)); } catch (e) { /* ignore a partial frame */ }
  };
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_is_self_contained() {
        let page = html(5432);
        // Nothing may be fetched from anywhere: the whole claim is that this
        // needs nothing installed.
        for forbidden in ["http://", "https://", "//cdn", "<link", "src="] {
            assert!(
                !page.contains(forbidden),
                "the page references something external: {forbidden}"
            );
        }
    }

    #[test]
    fn the_port_is_substituted() {
        assert!(html(5433).contains("port 5433"));
        assert!(!html(5433).contains("{{PORT}}"));
    }
}
