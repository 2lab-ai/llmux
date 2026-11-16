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

// `llmux status` lists a login as expiring inside LOGIN_EXPIRY_WARN
// (src/scheduler/select.rs); the web view must use the same window.
function daemonLoginWarnSecs() {
  const rs = readFileSync(new URL('../../src/scheduler/select.rs', import.meta.url), 'utf8');
  const m = /pub const LOGIN_EXPIRY_WARN: Duration = Duration::from_secs\(([\d_ *]+)\);/.exec(rs);
  assert.ok(m, 'LOGIN_EXPIRY_WARN not found in select.rs');
  return m[1].split('*').map((n) => Number(n.replaceAll('_', '').trim())).reduce((a, b) => a * b, 1);
}

test('a login inside the warning window says when it ends', () => {
  const X = loadCore();
  const p = { seven_day_max: 0.99 };
  const hour = 3600 * 1000;
  const at = (ms, a = {}) => X.accountState({ healthy: true, login_expires_at_ms: Date.now() + ms, ...a }, p);
  const [kind, label, tone] = at(8 * hour + 60 * 1000);
  assert.equal(kind, 'ok');
  assert.match(label, /^Ready · login ends in 8h [01]m · sign in again$/);
  assert.equal(tone, 'warn');
  assert.match(at(2 * hour, { status: 'active' })[1], /^Serving · login ends in 1h 59m|^Serving · login ends in 2h 0m/);
  assert.equal(at(-hour)[1], 'Ready · login ended · sign in again');
  assert.deepEqual([...at(10 * 24 * hour)], ['ok', 'Ready', 'ok'], 'outside the window: unchanged');
  assert.deepEqual([...X.accountState({ healthy: true }, p)], ['ok', 'Ready', 'ok'], 'end unknown: unchanged');
  const benched = X.accountState({ healthy: false, auth_failure_status: 400, auth_failure: { cause: 'login_expired', at_ms: 1 },
    login_expires_at_ms: Date.now() - hour }, p);
  assert.equal(benched[1], 'Login expired 400 · sign in again', 'a benched account keeps its cause');
});

test('the web view warns in the same window as llmux status', () => {
  const X = loadCore();
  const warn = daemonLoginWarnSecs();
  assert.equal(X.LOGIN_EXPIRY_WARN_S, warn);
  // The cutoff itself: a minute inside the window warns, a minute outside does not.
  const p = { seven_day_max: 0.99 };
  const tone = (secs) => X.accountState({ healthy: true, login_expires_at_ms: Date.now() + secs * 1000 }, p)[2];
  assert.equal(tone(warn - 60), 'warn', 'just inside the window');
  assert.equal(tone(warn + 60), 'ok', 'just outside the window');
});

// `.prd/25` amendment 2026-11-16: the Accounts view names each account's plan
// and whether it is a personal subscription or a team seat, from the daemon's
// `plan` object (src/proxy/plans.rs AccountPlan).
test('the plan and personal / team come from the daemon plan object', () => {
  const X = loadCore();
  const seat = { type: 'oauth', plan: { label: 'max5', rate_limit_tier: 'default_claude_max_5x',
    account_type: 'team', organization_type: 'claude_team', seat_tier: 'premium', plan_name: 'Team' } };
  assert.deepEqual([...X.accountPlan(seat)], ['max5', 'team',
    'org claude_team · tier default_claude_max_5x · seat premium · named "Team"']);
  assert.deepEqual([...X.accountPlan({ type: 'codex', plan: { label: 'promax', plan_type: 'promax', account_type: 'personal' } })],
    ['promax', 'personal', 'plan_type promax']);
  assert.deepEqual([...X.accountPlan({ type: 'oauth' })], ['?', '?', ''], 'not read yet');
  assert.deepEqual([...X.accountPlan({ type: 'oauth', plan: { label: 'max20' } })], ['max20', '?', ''], 'older daemon');
  assert.deepEqual([...X.accountPlan({ type: 'grok' })], ['-', '-', ''], 'no plans on this provider');
});

test('the accounts table has plan and type columns and marks a team seat', () => {
  const X = loadCore();
  const now = Date.now() / 1000;
  const account = (name, type, plan) => ({ name, type, plan, healthy: true, status: 'ok', paused: false,
    seven_day: { utilization: 0.5, resets_at: now + 3600 }, session: { requests: 1, errors: 0 }, in_flight: 0 });
  const html = X.renderAccounts({
    select_params: { mode: 'drain', seven_day_max: 0.99 },
    current_by_group: {},
    accounts: [
      account('claude:seat@x.com', 'oauth', { label: 'max5', account_type: 'team', organization_type: 'claude_team' }),
      account('claude:me@x.com', 'oauth', { label: 'max20', account_type: 'personal', organization_type: 'claude_max' }),
      account('grok:me@x.com', 'grok', null),
    ],
  });
  assert.match(html, /<th>account<\/th><th>plan<\/th><th>type<\/th><th>state<\/th>/);
  assert.match(html, />max5<\/td>\s*<td><span class="x-pill team">Team<\/span>/);
  assert.match(html, />max20<\/td>\s*<td><span class="x-pill mute">Personal<\/span>/);
  assert.match(html, /colspan="10"/);
});
