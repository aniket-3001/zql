/*
 * A minimal Chrome DevTools Protocol client.
 *
 * Enough to open a page, collect its console errors, and evaluate expressions
 * in it. Puppeteer would do this too, and would be a 300 MB dependency plus a
 * second browser download to verify a page whose entire subject is not having
 * dependencies. The protocol is a WebSocket carrying JSON; this is the twenty
 * lines of it that matter.
 *
 * Node ships a WebSocket client as of 22, so there is nothing to install.
 */
'use strict';

function connect(endpoint) {
  const ws = new WebSocket(endpoint);
  const pending = new Map();
  const listeners = [];
  let next = 1;

  const ready = new Promise((resolve, reject) => {
    ws.addEventListener('open', () => resolve());
    ws.addEventListener('error', (e) => reject(new Error(`devtools socket: ${e.message || 'failed'}`)));
  });

  ws.addEventListener('message', (event) => {
    const msg = JSON.parse(event.data);
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result);
    } else if (msg.method) {
      for (const fn of listeners) fn(msg);
    }
  });

  return {
    ready,
    on: (fn) => listeners.push(fn),
    send(method, params = {}, sessionId) {
      const id = next++;
      const payload = { id, method, params };
      if (sessionId) payload.sessionId = sessionId;
      ws.send(JSON.stringify(payload));
      return new Promise((resolve, reject) => pending.set(id, { resolve, reject }));
    },
    close: () => ws.close(),
  };
}

/**
 * Opens `url`, waits for the engine, and runs every check inside the page.
 * Returns `{ log, failures }`.
 */
async function runInBrowser(endpoint, url, checks) {
  const cdp = connect(endpoint);
  await cdp.ready;

  const { targetId } = await cdp.send('Target.createTarget', { url: 'about:blank' });
  const { sessionId } = await cdp.send('Target.attachToTarget', { targetId, flatten: true });

  const consoleErrors = [];
  cdp.on((msg) => {
    if (msg.method === 'Runtime.exceptionThrown') {
      consoleErrors.push(msg.params.exceptionDetails.exception?.description
        || msg.params.exceptionDetails.text);
    }
    if (msg.method === 'Runtime.consoleAPICalled' && msg.params.type === 'error') {
      consoleErrors.push(msg.params.args.map((a) => a.value ?? a.description).join(' '));
    }
  });

  await cdp.send('Runtime.enable', {}, sessionId);
  await cdp.send('Page.enable', {}, sessionId);
  await cdp.send('Page.navigate', { url }, sessionId);

  const evaluate = async (expression) => {
    const r = await cdp.send('Runtime.evaluate',
      { expression, awaitPromise: true, returnByValue: true }, sessionId);
    if (r.exceptionDetails) {
      throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
    }
    return r.result.value;
  };

  // The page loads WebAssembly and four fixtures over http, so readiness is
  // waited for rather than assumed. A timeout here is itself a failure worth
  // reporting: it means the engine never started.
  const deadline = Date.now() + 45000;
  let ready = false;
  while (Date.now() < deadline) {
    try {
      if (await evaluate('!!(window.__zql && window.__zql.ready())')) { ready = true; break; }
    } catch { /* the document may not have a context yet */ }
    await new Promise((r) => setTimeout(r, 250));
  }

  const log = [];
  const failures = [];

  if (!ready) {
    failures.push('the engine never became ready (WebAssembly did not load)');
    cdp.close();
    return { log, failures };
  }
  log.push('  engine loaded in the browser');

  for (const [name, sql, expect] of checks) {
    let result;
    try {
      result = await evaluate(`JSON.stringify(window.__zql.run(${JSON.stringify(sql)}))`);
    } catch (err) {
      failures.push(`${name}: threw in the page — ${err.message}`);
      log.push(`  FAIL ${name}`);
      continue;
    }
    let parsed;
    try {
      parsed = JSON.parse(result);
    } catch {
      failures.push(`${name}: the page returned nothing parseable`);
      log.push(`  FAIL ${name}`);
      continue;
    }
    // A predicate that throws — reaching into `.rows` on an error reply, say —
    // is a failed check reporting what it actually got, not a crashed run.
    let passed = false;
    try {
      passed = expect(parsed) === true;
    } catch {
      passed = false;
    }
    if (passed) {
      log.push(`  ok   ${name}`);
    } else {
      failures.push(`${name}: got ${JSON.stringify(parsed).slice(0, 240)}`);
      log.push(`  FAIL ${name}`);
    }
  }

  // A page that answers correctly while throwing in the console is still
  // broken: something on it is not working, and the next change will find out
  // the hard way which part.
  if (consoleErrors.length) {
    failures.push(`the page logged ${consoleErrors.length} console error(s): ${consoleErrors[0]}`);
  }

  cdp.close();
  return { log, failures };
}

module.exports = { runInBrowser };
