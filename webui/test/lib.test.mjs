// node --test webui/test/
import test from 'node:test';
import assert from 'node:assert/strict';
import { solveRates, priceRow, rowTokens } from '../lib/pricing.mjs';
import { cleanPrompt, buildOverview } from '../lib/overview.mjs';
import { promptOf, AUX_RE, SessionIndex, requestPage, cursorOf } from '../lib/sessions.mjs';
import { parseRawMeta, userPrompts } from '../lib/rawmeta.mjs';
import { messagesToEntries } from '../lib/transcript.mjs';

test('solveRates recovers the linear per-token rates the daemon priced with', () => {
  const rate = { in: 5e-6, out: 25e-6, cr: 0.5e-6, cc: 6.25e-6 };
  const rows = [[1000, 2000, 3e6, 1e5], [10, 50000, 9e6, 2e4], [400, 800, 1e5, 9e5], [7, 12000, 4e6, 3e3], [99, 1, 2e7, 1e4]]
    .map(([a, b, c, d]) => ({ gran: 'hour', model: 'm', tokens_in: a, tokens_out: b, cache_read: c, cache_creation: d, cost_usd: a * rate.in + b * rate.out + c * rate.cr + d * rate.cc }));
  const r = solveRates(rows).get('m');
  assert.ok(r, 'solved');
  for (const k of ['in', 'out', 'cr', 'cc']) assert.ok(Math.abs(r[k] - rate[k]) / rate[k] < 1e-6, k);
  assert.equal(+priceRow({ model: 'm', tokens: { input: 1e6, output: 0, cache_read: 0, cache_creation: 0 }, cost_usd: null }, new Map([['m', r]]), new Map()).toFixed(4), 5);
  assert.equal(priceRow({ model: 'm', cost_usd: 1.5 }, new Map(), new Map()), 1.5, 'daemon cost wins');
});

test('a history row is priced with its sub-usages at their own rates (#278)', () => {
  const rates = new Map([['exec', { in: 1e-6, out: 2e-6, cr: 0, cc: 0 }], ['adv', { in: 10e-6, out: 20e-6, cr: 0, cc: 0 }]]);
  const row = { model: 'exec', cost_usd: null, tokens: { input: 1000, output: 100 },
    sub_usage: [{ model: 'adv', tokens: { input: 500, output: 50 } }, { tokens: { input: 200, output: 0 } }, { model: 'unknown', tokens: { input: 9, output: 9 } }] };
  // exec 1000·1 + 100·2 + adv 500·10 + 50·20 + (no model → exec) 200·1; an unpriced sub model adds 0.
  assert.equal(+priceRow(row, rates, new Map([['unknown', 99]])).toFixed(6), +((1000 + 200 + 5000 + 1000 + 200) * 1e-6).toFixed(6));
  assert.equal(priceRow({ ...row, cost_usd: 0.5 }, rates, new Map()), 0.5, 'the daemon cost already includes sub-usage');
  assert.deepEqual(rowTokens(row), { input: 1709, output: 159, cache_read: 0, cache_creation: 0 });
  assert.deepEqual(rowTokens({ tokens: { input: 1, output: 2 } }), { input: 1, output: 2, cache_read: 0, cache_creation: 0 });
});

test('solveRates refuses a fit that is not linear', () => {
  const rows = [1, 2, 3, 4, 5, 6].map(i => ({ gran: 'hour', model: 'x', tokens_in: i * 100, tokens_out: i * i * 10, cache_read: 0, cache_creation: 0, cost_usd: Math.random() * 10 + 1 }));
  assert.equal(solveRates(rows).has('x'), false);
});

test('cleanPrompt strips soma-work and Slack wrappers', () => {
  assert.equal(cleanPrompt('<speaker>alex kim</speaker>\n<https://github.com/a/b/pull/7|github.com/a/b/pull/7>\n\n<context>\n <slack-name>x</slack-name>'), 'github.com/a/b/pull/7');
  assert.equal(cleanPrompt('$fix <local:using-ssot> Base directory for this skill: /opt/x\n---\nname: y'), '$fix');
});

test('promptOf keeps what a person typed', () => {
  assert.equal(promptOf('Tool loaded.'), null);
  assert.equal(promptOf('Another Claude session sent a message: hi'), null);
  assert.equal(promptOf('The user sent a new message while you were working: 바로 시작해줘\n\nThis is how Claude Code surfaces messages'), '바로 시작해줘');
  assert.equal(promptOf('백오피스 이슈 만들어줘'), '백오피스 이슈 만들어줘');
});

test('side-call templates are recognised', () => {
  for (const s of ['Generate a concise, descriptive Korean title (max 40 chars) for this conversation.', 'Use the Bash tool exactly once to run this exact command, unmodified', 'You are a SAFETY REVIEWER (guardian) for an autonomous coding agent.', 'Summarize this AI assistant response. Output ONLY a JSON object', '<session-title-pending> This session still has a temporary title']) assert.ok(AUX_RE.test(s), s);
  assert.equal(AUX_RE.test('PR #744 리뷰해줘'), false);
});

test('tool-result text is harness output; a mid-turn message is an instruction', () => {
  const e = messagesToEntries([
    { role: 'user', content: '첫 지시' },
    { role: 'assistant', content: [{ type: 'tool_use', id: 't1', name: 'Bash', input: { command: 'ls' } }] },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 't1', content: 'a\nb' }, { type: 'text', text: 'Tool loaded.' }] },
    { role: 'user', content: [{ type: 'text', text: 'The user sent a new message while you were working: 두번째 지시' }] },
    { role: 'user', content: 'Another Claude session sent a message: report' },
  ]);
  assert.deepEqual(e.map(x => x.kind), ['user', 'tool', 'meta', 'user', 'agentmsg']);
  assert.equal(e[3].text, '두번째 지시');
});

test('buildOverview numbers instructions and attributes requests, tools and files', () => {
  const entries = messagesToEntries([
    { role: 'user', content: '고쳐줘', _meta: { ts: 1000 } },
    { role: 'assistant', content: [{ type: 'tool_use', id: 'a', name: 'Edit', input: { file_path: '/r/x.ts', old_string: 'a', new_string: 'b\nc' } }], _meta: { ts: 1100 } },
    { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'a', content: 'ok' }], _meta: { ts: 1200 } },
    { role: 'assistant', content: [{ type: 'text', text: '고쳤습니다 https://github.com/o/r/pull/9' }], _meta: { ts: 1300 } },
    { role: 'user', content: '푸시해', _meta: { ts: 5000 } },
  ]);
  const ov = buildOverview(entries, [{ at_ms: 1150, cost_usd: 0.5, tokens: { input: 1, output: 2 } }, { at_ms: 6000, cost_usd: 1 }], r => r.cost_usd);
  assert.equal(ov.instructions.length, 2);
  assert.equal(ov.instructions[0].i, 1);
  assert.equal(ov.instructions[0].toolCount, 1);
  assert.equal(ov.instructions[0].cost, 0.5);
  assert.equal(ov.instructions[1].cost, 1);
  assert.equal(ov.files[0].path, '/r/x.ts');
  assert.deepEqual(ov.links, ['https://github.com/o/r/pull/9']);
  assert.deepEqual(entries.filter(e => e.kind !== 'meta').map(e => e.gi), [1, 1, 1, 2]);
});

test('parseRawMeta reads where and how a remote Claude Code runs, never credentials', () => {
  const body = JSON.stringify({ model: 'claude-opus-5-5', system: [{ type: 'text', text: 'x-anthropic-billing-header: cc_version=2.1.319.e90; cc_entrypoint=cli;' }], tools: [{ name: 'Bash' }, { name: 'mcp__github__create_pull_request' }], output_config: { effort: 'xhigh' }, thinking: { type: 'adaptive' },
    messages: [{ role: 'user', content: [{ type: 'text', text: '<system-reminder>Contents of /Users/c/repo/CLAUDE.md (project instructions)</system-reminder>' }, { type: 'text', text: '이슈 확인해줘' }] }, { role: 'user', content: [{ type: 'text', text: 'Primary working directory: /Users/c/repo\nCurrent branch: develop\nOS Version: Darwin 27.2.0' }] }] });
  const m = parseRawMeta({ id: 5, ts_ms: 9, request_body: body, request_headers: [['user-agent', 'claude-cli/2.1.319 (external, cli)'], ['x-stainless-os', 'MacOS'], ['authorization', 'Bearer secret']] });
  assert.equal(m.version, '2.1.319'); assert.equal(m.entrypoint, 'cli'); assert.equal(m.os, 'MacOS');
  assert.equal(m.cwd, '/Users/c/repo'); assert.equal(m.branch, 'develop'); assert.deepEqual(m.mcp, ['github']);
  assert.equal(m.firstPrompt, '이슈 확인해줘');
  assert.ok(!JSON.stringify(m).includes('secret'));
  assert.deepEqual(userPrompts([{ role: 'user', content: [{ type: 'tool_result', content: 'x' }, { type: 'text', text: 'Tool loaded.' }] }]), []);
});

test('sessions are owned by the key that sent their requests; side calls fold', () => {
  const ix = new SessionIndex();
  ix.ingest({ client_keys: [{ id: 'k-1', name: 'alex (U000TEST01)', email: 'c@x' }], usage_stats: [], model_usage: [] });
  const uid = (sid) => JSON.stringify({ device_id: 'dev1', account_uuid: '', session_id: sid });
  const now = Date.now();
  ix.addRow({ id: 1, at_ms: now - 1000, tenant: 'k-1', user_id: uid('11111111-1111-1111-1111-111111111111'), msg_kind: 'user', excerpt: '로그인 버그 고쳐줘', model: 'm', status: 200, tokens: { input: 1, output: 1 } });
  ix.addRow({ id: 2, at_ms: now - 900, tenant: 'local', user_id: uid('22222222-2222-2222-2222-222222222222'), msg_kind: 'sdk', excerpt: 'You are a SAFETY REVIEWER (guardian)', model: 'm', status: 200 });
  const { cards, auxBy } = ix.list();
  assert.equal(cards.length, 1);
  assert.equal(cards[0].owner.short, 'alex');
  assert.equal(cards[0].title, '로그인 버그 고쳐줘');
  assert.equal(cards[0].place, 'remote');
  assert.deepEqual(auxBy, { local: 1 });
});

test('tool inputs kept for the Claude Code screen are clipped and never the whole payload', async () => {
  const { slimInput } = await import('../lib/transcript.mjs');
  const big = 'x'.repeat(10000);
  const e = slimInput('Edit', { file_path: '/a.ts', old_string: big, new_string: 'b' });
  assert.ok(e.old_string.length < 4100 && e.old_string.includes('(+6000자)'));
  assert.equal(slimInput('Bash', { command: 'ls', description: 'list' }).command, 'ls');
  assert.ok(JSON.stringify(slimInput('mcp__x__y', { data: big })).length < 1600);
});

test('request pages walk older by cursor without repeats or gaps while new requests arrive', () => {
  const row = (id, at_ms, msg_kind = 'user') => ({ id, at_ms, msg_kind });
  // Two rows share a timestamp: the cursor still tells them apart.
  const rows = [row(1, 1000), row(2, 2000, 'subagent'), row(3, 3000), row(4, 3000), row(5, 4000), row(6, 5000, 'subagent')];
  const p1 = requestPage(rows, { limit: 2 });
  assert.deepEqual(p1.rows.map(r => r.id), [6, 5]);
  assert.equal(p1.total, 6); assert.equal(p1.more, true);
  // Three requests land on top before the next scroll; the older page is unaffected.
  const live = rows.concat([row(7, 6000), row(8, 7000), row(9, 8000)]);
  const p2 = requestPage(live, { limit: 2, before: cursorOf(p1.rows.at(-1)) });
  assert.deepEqual(p2.rows.map(r => r.id), [4, 3]);
  const p3 = requestPage(live, { limit: 2, before: cursorOf(p2.rows.at(-1)) });
  assert.deepEqual(p3.rows.map(r => r.id), [2, 1]);
  assert.equal(p3.more, false);
  assert.deepEqual(requestPage(live, { limit: 2, before: cursorOf(p3.rows.at(-1)) }).rows, []);
  // The live top-up returns only what is newer than the top row, and flags a gap past one page.
  const top = requestPage(live, { limit: 5, after: cursorOf(p1.rows[0]) });
  assert.deepEqual(top.rows.map(r => r.id), [9, 8, 7]); assert.equal(top.gap, false);
  assert.equal(requestPage(live, { limit: 2, after: cursorOf(p1.rows[0]) }).gap, true);
  // A kind filter pages within the kind; limits are clamped.
  assert.deepEqual(requestPage(live, { kind: 'subagent' }).rows.map(r => r.id), [6, 2]);
  assert.equal(requestPage(live, { limit: 9999 }).limit, 200);
});

test('a session keeps its newest 4000 requests and counts the older ones it let go', () => {
  const ix = new SessionIndex();
  const sid = '33333333-3333-3333-3333-333333333333';
  const uid = JSON.stringify({ device_id: 'dev1', account_uuid: '', session_id: sid });
  for (let i = 1; i <= 4005; i++) ix.addRow({ id: i, at_ms: 1000 + i, tenant: 'local', user_id: uid, msg_kind: 'user', model: 'm', status: 200 });
  const rows = ix.rowsFor(sid);
  assert.equal(rows.length, 4000);
  assert.equal(rows[0].id, 6);
  assert.equal(ix.droppedFor(sid), 5);
  // The oldest kept page ends the walk: ids 205 down to 6, nothing further.
  const last = requestPage(rows, { limit: 200, before: cursorOf(rows[200]) });
  assert.equal(last.rows[0].id, 205); assert.equal(last.rows.at(-1).id, 6); assert.equal(last.rows.length, 200);
  assert.equal(last.more, false);
});
