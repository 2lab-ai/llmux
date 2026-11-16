// node --test webui/test/
// The Accounts view's label for a benched account, from the shape the daemon
// emits (`healthy`, `auth_failure_status`, `auth_failure.cause`).
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';

function loadCore() {
  const src = readFileSync(new URL('../public/core.js', import.meta.url), 'utf8');
  const ctx = {
    location: { search: '', pathname: '/', origin: 'http://x' },
    localStorage: { getItem: () => null, setItem: () => {} },
    document: { getElementById: () => null, addEventListener: () => {} },
    URLSearchParams, Date, Math, JSON, setTimeout, clearTimeout,
  };
  ctx.window = ctx;
  runInNewContext(src, ctx);
  return ctx.X;
}

// Every cause id the daemon can emit: the snake_case of each `label()` arm in
// src/auth_cause.rs (the Rust test pins id == label with `_` for spaces).
function daemonCauseIds() {
  const rs = readFileSync(new URL('../../src/auth_cause.rs', import.meta.url), 'utf8');
  const body = rs.slice(rs.indexOf('pub fn label(self)'), rs.indexOf('pub fn why(self)'));
  return [...body.matchAll(/AuthCause::\w+ => "([^"]+)"/g)].map((m) => m[1].replaceAll(' ', '_'));
}

test('a classified auth failure names its cause and fix', () => {
  const X = loadCore();
  const p = { seven_day_max: 0.99 };
  const state = (a) => X.accountState({ healthy: false, ...a }, p)[1];
  assert.equal(state({ auth_failure_status: 400, auth_failure: { cause: 'login_expired', at_ms: 1 } }),
    'Login expired 400 · sign in again');
  assert.equal(state({ auth_failure_status: 401, auth_failure: { cause: 'key_rejected', at_ms: 1 } }),
    'Key rejected 401 · add a valid key');
  assert.equal(state({ auth_failure_status: 403, auth_failure: { cause: 'access_suspended', at_ms: 1 } }),
    'Access suspended 403 · resolve with the provider, then sign in again');
  assert.equal(state({ auth_failure_status: 401 }), 'Sign-in failed 401', 'older daemon: no cause');
  assert.equal(state({ auth_failure: { cause: 'from_the_future', at_ms: 1 } }), 'Unhealthy', 'unknown cause');
});

test('every cause the daemon emits has a label and a fix in the web view', () => {
  const X = loadCore();
  const ids = daemonCauseIds();
  assert.ok(ids.length >= 11, `parsed ${ids.length} cause ids`);
  for (const id of ids) {
    const label = X.accountState({ healthy: false, auth_failure_status: 400, auth_failure: { cause: id, at_ms: 1 } },
      { seven_day_max: 0.99 })[1];
    assert.match(label, / 400 · \S/, `${id} → ${label}`);
  }
});
