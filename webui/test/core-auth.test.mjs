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

test('a classified auth failure names its cause and fix', () => {
  const X = loadCore();
  const p = { seven_day_max: 0.99 };
  const state = (a) => X.accountState({ healthy: false, ...a }, p)[1];
  assert.equal(state({ auth_failure_status: 400, auth_failure: { cause: 'login_expired', at_ms: 1 } }),
    'Login expired 400 · sign in again');
  assert.equal(state({ auth_failure_status: 401, auth_failure: { cause: 'key_rejected', at_ms: 1 } }),
    'Key rejected 401 · add a valid key');
  assert.equal(state({ auth_failure_status: 401 }), 'Sign-in failed 401', 'older daemon: no cause');
  assert.equal(state({ auth_failure: { cause: 'from_the_future', at_ms: 1 } }), 'Unhealthy', 'unknown cause');
});
