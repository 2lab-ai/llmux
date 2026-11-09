// llmux web dashboard — zero-dependency server (spec: .prd/26-web-session-dashboard.md).
// Serves ./public, @2lab.ai/ui from UI_DIST at /ui/*, and JSON views over the daemon's admin API.
// The admin key is read from ~/.config/llmux.json on the server side only; it is
// never sent to the browser. Binds only to the hosts in BIND (no 0.0.0.0).
import http from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { SessionIndex } from './lib/sessions.mjs';
import { parseResponse, recoverMessages } from './lib/transcript.mjs';
import { buildOverview } from './lib/overview.mjs';

const ROOT = path.join(path.dirname(fileURLToPath(import.meta.url)), 'public');
const PORT = Number(process.env.PORT || 4780);
const BIND = (process.env.BIND || '127.0.0.1').split(',').map(s => s.trim()).filter(Boolean);
const DAEMON = process.env.LLMUX_URL || 'http://127.0.0.1:3456';
const CFG = process.env.LLMUX_CONFIG || path.join(os.homedir(), '.config', 'llmux.json');
const HOST = os.hostname().replace(/\.local$/, '');
const KEEP_DAYS = 14;

if (BIND.some(h => h === '0.0.0.0' || h === '::')) { console.error('refusing to bind a wildcard address'); process.exit(2); }

function adminKey() {
  try { return JSON.parse(fs.readFileSync(CFG, 'utf8'))?.proxy?.api_key || null; } catch { return null; }
}

const stripReq = ({ user_id, ...r }) => (r.excerpt ? { ...r, excerpt: String(r.excerpt).slice(0, 160) } : r);

// The daemon doc is ~2 MB; the drafts need ~15% of it.
function slim(d) {
  const today = Math.floor(Date.now() / 86400000);
  const hourNow = Math.floor(Date.now() / 3600000);
  return {
    v: 1, host: HOST, fetched_at_ms: Date.now(),
    version: d.version, pid: d.pid, uptime_secs: d.uptime_secs, port: d.port,
    current: d.current, current_by_group: d.current_by_group, upstream: d.upstream, config_path: d.config_path,
    select_params: d.select_params, refresh_ahead_secs: d.refresh_ahead_secs, evaluate_tick_secs: d.evaluate_tick_secs,
    accounts: (d.accounts || []).map(a => ({ ...a, auth_history: (a.auth_history || []).slice(0, 6) })),
    scheduler: d.scheduler, poller: d.poller, totals: d.totals, health: d.health,
    model_usage: (d.model_usage || []).map(m => ({ ...m, accounts: (m.accounts || []).slice(0, 8) })),
    windowed: d.windowed || [],
    tenant_usage: (d.tenant_usage || []).map(({ email, ...t }) => t),
    activity: {
      in_flight: (d.activity?.in_flight || []).map(stripReq),
      completed: (d.activity?.completed || []).map(stripReq),
    },
    codex: d.codex, grok: d.grok,
    usage_stats: (d.usage_stats || []).filter(u => u.gran === 'month' || (u.gran === 'day' && u.bucket > today - KEEP_DAYS) || (u.gran === 'hour' && u.bucket > hourNow - 48)),
    daily_perf: (d.daily_perf || []).filter(p => p.day > today - KEEP_DAYS),
    daily_usage: (d.daily_usage || []).filter(p => p.day > today - KEEP_DAYS),
    config_facts: d.config_facts, data_quality: d.data_quality,
    email_anonymous: d.email_anonymous, quota_display: d.quota_display, domain_abbrev: d.domain_abbrev,
    show_fable_weekly: d.show_fable_weekly, events: d.events,
  };
}

// ?anon: same live numbers, identities masked — for screenshots that leave this machine.
// Every e-mail becomes a stable alias (acct-07@x), prompt excerpts and tenant names are dropped.
function anonymize(body) {
  const d = JSON.parse(body);
  const names = [...new Set((d.accounts || []).map(a => a.name))].sort();
  const alias = new Map();
  const emailRe = /[\w.+-]+@[\w.-]+\.[a-z]{2,}/gi;
  const mask = (s) => s.replace(emailRe, (m) => {
    if (!alias.has(m)) alias.set(m, 'acct-' + String(alias.size + 1).padStart(2, '0') + '@x');
    return alias.get(m);
  });
  for (const n of names) mask(n); // aliases follow account order, not traffic order
  const walk = (o) => {
    if (Array.isArray(o)) return o.map(walk);
    if (o && typeof o === 'object') {
      const out = {};
      for (const [k, v] of Object.entries(o)) {
        if (k === 'excerpt' || k === 'config_path') continue;
        if (k === 'client_name' || (k === 'name' && 'tenant' in o)) { out[k] = typeof v === 'string' ? (v === 'local' ? v : 'client') : v; continue; }
        out[mask(k)] = walk(v);
      }
      return out;
    }
    return typeof o === 'string' ? mask(o) : o;
  };
  return JSON.stringify({ ...walk(d), anon: true });
}

const sessions = new SessionIndex({ fetchRaw, host: HOST });
const EMAIL = /[\w.+-]+@[\w.-]+\.[a-z]{2,}/gi;
const maskStr = (str) => str.replace(EMAIL, (m) => 'acct-' + m.length.toString(36) + m.charCodeAt(0).toString(36) + '@x');
const maskDeep = (o) => typeof o === 'string' ? maskStr(o) : Array.isArray(o) ? o.map(maskDeep) : o && typeof o === 'object' ? Object.fromEntries(Object.entries(o).map(([k, v]) => [maskStr(k), maskDeep(v)])) : o;

async function fetchRaw(id, at_ms) {
  const key = adminKey(); if (!key) return null;
  const r = await fetch(`${DAEMON}/llmux/raw-io?id=${id}&at_ms=${at_ms}`, { headers: { 'x-api-key': key }, signal: AbortSignal.timeout(20000) });
  if (!r.ok) return null;
  return r.json();
}
const SECRET_HEADERS = /^(authorization|x-api-key|cookie|set-cookie|proxy-authorization|anthropic-api-key|openai-api-key|chatgpt-account-id)$/i;
function rawSummary(rec) {
  const req = recoverMessages(rec.request_body || '');
  const resp = parseResponse(rec.response_body || '');
  const last = req.messages[req.messages.length - 1];
  const hdr = (h) => (h || []).map(x => Array.isArray(x) ? x : [x.name, x.value]).map(([k, v]) => [k, SECRET_HEADERS.test(k) ? '••••' : String(v).slice(0, 300)]);
  return {
    id: rec.id, ts_ms: rec.ts_ms, group: rec.group, model: rec.model, account: rec.account, status: rec.status, duration_ms: rec.duration_ms,
    request: { bytes: (rec.request_body || '').length, truncated: req.truncated, messages: req.messages.length, tools: req.tools ?? null, last: last ? { role: last.role, content: JSON.stringify(last.content).slice(0, 4000) } : null, headers: hdr(rec.request_headers) },
    response: { bytes: (rec.response_body || '').length, stop: resp.stop, usage: resp.usage, blocks: resp.blocks.map(b => b.type === 'text' ? { type: 'text', text: b.text.slice(0, 4000) } : b.type === 'tool_use' ? { type: 'tool_use', name: b.name, input: JSON.stringify(b.input).slice(0, 1500) } : b.type === 'thinking' ? { type: 'thinking', chars: (b.thinking || '').length } : { type: b.type }), headers: hdr(rec.response_headers) },
  };
}
async function daemonJson(p, timeout = 15000) {
  const r = await fetch(DAEMON + p, { headers: { 'x-api-key': adminKey() || '' }, signal: AbortSignal.timeout(timeout) });
  if (!r.ok) throw Object.assign(new Error('daemon answered HTTP ' + r.status), { status: 502 });
  return r.json();
}
const loadModels = () => daemonJson('/llmux/models').then(j => sessions.setModels(j.models)).catch(() => {});
loadModels(); setInterval(loadModels, 10 * 60000).unref();
const kuCache = new Map();
async function keysUsage(w) {
  const hit = kuCache.get(w);
  if (hit && Date.now() - hit.at < 15000) return hit.v;
  const v = await daemonJson(`/llmux/keys/usage?window=${w}`);
  kuCache.set(w, { at: Date.now(), v });
  return v;
}
let listCache = { at: 0, key: '', v: null };
function sessionList(aux) {
  const key = aux ? 'aux' : 'main';
  if (listCache.v && listCache.key === key && Date.now() - listCache.at < 1500) return listCache.v;
  listCache = { at: Date.now(), key, v: sessions.list({ limit: 100000, aux }) };
  return listCache.v;
}
const WINDOWS = ['1h', '24h', '7d', '14d', '30d', '90d', 'all'];

// Keep the session index warm even when no browser is polling.
setInterval(() => { getDoc().catch(() => {}); }, 2000);
const ACTIVITY = process.env.LLMUX_ACTIVITY || path.join(process.env.XDG_STATE_HOME || path.join(os.homedir(), '.local/state'), 'llmux/activity.jsonl');
sessions.loadHistory(ACTIVITY).then(() => console.log('[sessions] history', sessions.backfill)).catch(e => console.error('[sessions] history failed', e.message));

let cache = { at: 0, body: null };
let pending = null;
async function getDoc() {
  if (cache.body && Date.now() - cache.at < 900) return cache.body;
  if (!pending) {
    pending = (async () => {
      const key = adminKey();
      if (!key) throw Object.assign(new Error('no proxy.api_key in ' + CFG), { status: 503 });
      const r = await fetch(DAEMON + '/llmux/dashboard', { headers: { 'x-api-key': key }, signal: AbortSignal.timeout(4000) });
      if (!r.ok) throw Object.assign(new Error('daemon answered HTTP ' + r.status), { status: 502 });
      const raw = await r.json();
      try { sessions.ingest(raw); } catch (e) { console.error('[sessions] ingest', e); }
      cache = { at: Date.now(), body: JSON.stringify(slim(raw)) };
      return cache.body;
    })().finally(() => { pending = null; });
  }
  return pending;
}

const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.svg': 'image/svg+xml', '.png': 'image/png', '.json': 'application/json' };
const ROUTES = { '/': 'app.html', '/users': 'app.html', '/activity': 'app.html', '/accounts': 'app.html' };
const UI_DIST = process.env.UI_DIST || '/opt/soma-work/dev/node_modules/@2lab.ai/ui/dist';
const UI_CDN = 'https://cdn.jsdelivr.net/npm/@2lab.ai/ui@0.39.0/dist/';

// ---------- soma-work single sign-on (AUTH=soma) ----------
// The soma-work dashboard (SSO_ORIGIN) owns login. Its session cookie (soma_dash_token) is host-only,
// so a bridge route on the soma-work host (/__llmux/bridge, proxied here by Caddy) reads it, asks
// soma-work /auth/me whether the viewer is an admin, and copies the same JWT into `llmux_sso`
// scoped to COOKIE_DOMAIN. This server then validates `llmux_sso` the same way on every request.
const AUTH = process.env.AUTH === 'soma';
const SOMA_ME = process.env.SOMA_AUTH_ME || 'http://127.0.0.1:33000/auth/me';
const SSO_ORIGIN = (process.env.SSO_ORIGIN || '').replace(/\/$/, '');
const PUBLIC_ORIGIN = (process.env.PUBLIC_ORIGIN || '').replace(/\/$/, '');
const COOKIE_DOMAIN = process.env.COOKIE_DOMAIN || '';
const meCache = new Map();
const cookieOf = (req, name) => { const m = (req.headers.cookie || '').match(new RegExp('(?:^|;\\s*)' + name + '=([^;]*)')); if (!m) return null; try { return decodeURIComponent(m[1]); } catch { return null; } };
async function somaMe(token) {
  if (!token) return null;
  const hit = meCache.get(token);
  if (hit && Date.now() - hit.at < 60000) return hit.v;
  let v = null;
  try {
    const r = await fetch(SOMA_ME, { headers: { cookie: 'soma_dash_token=' + encodeURIComponent(token), accept: 'application/json' }, signal: AbortSignal.timeout(5000) });
    if (r.ok) { const j = await r.json(); v = { isAdmin: !!j.isAdmin, user: j.user || null }; }
  } catch (e) { console.error('[auth] soma /auth/me', e.message); return { error: true }; }
  meCache.set(token, { at: Date.now(), v });
  if (meCache.size > 500) meCache.delete(meCache.keys().next().value);
  return v;
}
const jwtExp = (t) => { try { return JSON.parse(Buffer.from(t.split('.')[1], 'base64url').toString()).exp || 0; } catch { return 0; } };
const safeNext = (n) => (typeof n === 'string' && /^\/[^/\\]/.test(n + 'x') && !n.includes('//') && n.length < 500 ? n : '/');
const page = (res, code, title, body) => { res.writeHead(code, { 'content-type': 'text/html; charset=utf-8', 'cache-control': 'no-store' }); res.end(`<!doctype html><meta charset=utf-8><meta name=viewport content="width=device-width,initial-scale=1"><title>llmux · ${title}</title><body style="margin:0;display:grid;place-items:center;height:100vh;background:#f7f7f7;font:15px/1.5 Inter,-apple-system,system-ui,sans-serif;color:#18181a"><div style="background:#fff;border:1px solid #e4e4e4;border-radius:14px;padding:28px 32px;max-width:440px"><b style="font-size:17px">${title}</b><p style="color:#57575d">${body}</p></div>`); };

async function bridge(req, res, url) {
  // Arrives on the soma-work host: the browser sends its host-only soma_dash_token here.
  const next = safeNext(url.searchParams.get('next') || cookieOf(req, 'llmux_next') || '/');
  const token = cookieOf(req, 'soma_dash_token');
  const me = await somaMe(token);
  const clearNext = 'llmux_next=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax';
  if (me && me.error) return page(res, 503, '로그인 확인 실패', 'soma-work 대시보드에 연결할 수 없습니다. 잠시 뒤 다시 시도하세요.');
  if (!me) {
    res.writeHead(302, { location: '/login', 'set-cookie': `llmux_next=${encodeURIComponent(next)}; Path=/; Max-Age=900; HttpOnly; Secure; SameSite=Lax`, 'cache-control': 'no-store' });
    return res.end();
  }
  if (!me.isAdmin) { res.setHeader('set-cookie', clearNext); return page(res, 403, '관리자만 볼 수 있습니다', `${(me.user && (me.user.name || me.user.email)) || '이 계정'}은 soma-work 관리자(ADMIN_USERS)가 아닙니다.`); }
  const maxAge = Math.max(60, jwtExp(token) - Math.floor(Date.now() / 1000)) || 86400;
  res.writeHead(302, {
    location: PUBLIC_ORIGIN + next,
    'set-cookie': [`llmux_sso=${encodeURIComponent(token)}; Domain=${COOKIE_DOMAIN}; Path=/; Max-Age=${maxAge}; HttpOnly; Secure; SameSite=Lax`, clearNext],
    'cache-control': 'no-store',
  });
  return res.end();
}

async function handler(req, res) {
  const url = new URL(req.url, 'http://x');
  if (AUTH && url.pathname === '/__llmux/bridge') return bridge(req, res, url);
  if (AUTH && url.pathname !== '/favicon.ico') {
    const me = await somaMe(cookieOf(req, 'llmux_sso'));
    const isApi = url.pathname.startsWith('/api/');
    if (me && me.error) return page(res, 503, '로그인 확인 실패', 'soma-work 대시보드에 연결할 수 없습니다.');
    if (!me) {
      if (isApi) { res.writeHead(401, { 'content-type': 'application/json' }); return res.end('{"error":"login required"}'); }
      res.writeHead(302, { location: `${SSO_ORIGIN}/__llmux/bridge?next=${encodeURIComponent(safeNext(url.pathname + url.search))}`, 'cache-control': 'no-store' });
      return res.end();
    }
    if (!me.isAdmin) return page(res, 403, '관리자만 볼 수 있습니다', 'soma-work 관리자 계정으로 로그인하세요.');
    if (url.pathname === '/api/whoami') { res.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' }); return res.end(JSON.stringify({ user: me.user, isAdmin: me.isAdmin, logout: SSO_ORIGIN + '/auth/logout' })); }
  }
  const sec = { 'x-content-type-options': 'nosniff', 'referrer-policy': 'no-referrer', 'cache-control': 'no-store' };
  if (req.method !== 'GET' && req.method !== 'HEAD') { res.writeHead(405, sec); return res.end(); }
  if (url.pathname === '/api/dashboard') {
    try {
      const body = url.searchParams.has('anon') ? anonymize(await getDoc()) : await getDoc();
      res.writeHead(200, { ...sec, 'content-type': 'application/json' }); return res.end(body);
    } catch (e) {
      res.writeHead(e.status || 502, { ...sec, 'content-type': 'text/plain' }); return res.end(String(e.message || e));
    }
  }
  if (url.pathname === '/favicon.ico') { res.writeHead(204, sec); return res.end(); }
  const anon = url.searchParams.has('anon');
  const sendJson = (obj, code = 200) => { res.writeHead(code, { ...sec, 'content-type': 'application/json' }); res.end(JSON.stringify(anon ? maskDeep(obj) : obj)); };
  if (url.pathname === '/api/sessions') {
    try { await getDoc(); } catch {}
    const aux = url.searchParams.has('aux');
    const r = sessionList(aux);
    const limit = Number(url.searchParams.get('limit')) || 600;
    const users = {}; for (const t of sessions.users.keys()) users[t] = sessions.userOf(t);
    return sendJson({ at: Date.now(), host: HOST, backfill: sessions.backfill, total: r.total, auxBy: r.auxBy, users, sessions: r.cards.slice(0, limit) });
  }
  if (url.pathname === '/api/users') {
    try { await getDoc(); } catch {}
    const w = WINDOWS.includes(url.searchParams.get('window')) ? url.searchParams.get('window') : '24h';
    let ku = null, err = null;
    try { ku = await keysUsage(w); } catch (e) { err = String(e.message || e); }
    const r = sessionList(false);
    return sendJson({ at: Date.now(), host: HOST, window: w, error: err, from_ms: ku && ku.from_ms, to_ms: ku && ku.to_ms, auxBy: r.auxBy, users: sessions.usersView(ku, r.cards) });
  }
  let m;
  if ((m = /^\/api\/session\/([0-9a-f-]{36})$/.exec(url.pathname))) {
    const sid = m[1];
    try {
      const s = sessions.sessions.get(sid);
      // Cheap change key: polling a long session re-sends megabytes only when something moved.
      sessions.refreshFiles();
      const f = sessions.files.get(sid);
      let fsz = 0; try { fsz = f ? fs.statSync(f).size : 0; } catch {}
      const v = `${s ? s.rows.size : 0}:${s ? s.last_ms : 0}:${s ? s.inflight.length : 0}:${fsz}`;
      if (url.searchParams.get('v') === v) return sendJson({ same: true, v });
      const chat = await sessions.chat(sid, fetchRaw);
      const card = s ? sessions.card(s) : null;
      const requests = sessions.rowsFor(sid).map(r => ({ ...r, cost: sessions.price(r) }));
      const overview = buildOverview(chat.entries, requests, r => r.cost);
      const tail = Number(url.searchParams.get('tail')) || 0;
      if (tail) { chat.entries = chat.entries.filter(e => e.kind !== 'meta').slice(-tail); chat.subagents = (chat.subagents || []).slice(-5); }
      const meta = sessions.meta.get(sid) || null;
      const kinds = {}; for (const r of requests) kinds[r.msg_kind || 'other'] = (kinds[r.msg_kind || 'other'] || 0) + 1;
      return sendJson({ v, card, chat, overview, meta: meta && !meta.missing ? { ...meta, prompts: undefined } : null, requestCount: requests.length, requestKinds: kinds });
    } catch (e) { return sendJson({ error: String(e.message || e) }, 500); }
  }
  // One page of a session's llmux requests, newest first (the 요청 tab loads pages on demand).
  if ((m = /^\/api\/session\/([0-9a-f-]{36})\/requests$/.exec(url.pathname))) {
    const all = sessions.rowsFor(m[1]);
    const kind = url.searchParams.get('kind') || '';
    const list = (kind ? all.filter(r => (r.msg_kind || 'other') === kind) : all).slice().reverse();
    const limit = Math.min(200, Math.max(1, Number(url.searchParams.get('limit')) || 50));
    const offset = Math.max(0, Number(url.searchParams.get('offset')) || 0);
    return sendJson({ total: list.length, offset, limit, rows: list.slice(offset, offset + limit).map(r => ({ ...r, cost: sessions.price(r) })) });
  }
  if ((m = /^\/api\/session\/([0-9a-f-]{36})\/agent\/([0-9a-z]+)$/.exec(url.pathname))) {
    const r = sessions.subagentChat(m[1], m[2]);
    return r ? sendJson(r) : sendJson({ error: 'not found' }, 404);
  }
  if (url.pathname === '/api/keys-usage') {
    const w = WINDOWS.includes(url.searchParams.get('window')) ? url.searchParams.get('window') : '24h';
    try { return sendJson(await keysUsage(w)); } catch (e) { return sendJson({ error: String(e.message || e) }, 502); }
  }
  // The whole raw-io record, verbatim, for the raw viewer (bodies are bounded at capture: raw_io.max_body_bytes).
  if (url.pathname === '/api/raw-full') {
    const id = Number(url.searchParams.get('id')), at = Number(url.searchParams.get('at_ms'));
    const rec = await fetchRaw(id, at).catch(() => null);
    if (!rec) return sendJson({ error: 'raw-io 기록을 찾을 수 없습니다 (보관 기간이 지났거나, 캡처가 꺼져 있거나, 모르는 id)' }, 404);
    const mask = (h) => (h || []).map(x => Array.isArray(x) ? x : [x.name, x.value]).map(([k, v]) => [k, SECRET_HEADERS.test(k) ? '•••redacted' : String(v)]);
    const up = rec.upstream ? { ...rec.upstream, request_headers: rec.upstream.request_headers ? mask(rec.upstream.request_headers) : null, response_headers: rec.upstream.response_headers ? mask(rec.upstream.response_headers) : null } : null;
    const body = JSON.stringify({ ...rec, request_headers: rec.request_headers ? mask(rec.request_headers) : null, response_headers: rec.response_headers ? mask(rec.response_headers) : null, upstream: up, port: Number(new URL(DAEMON).port) || 3456 });
    res.writeHead(200, { ...sec, 'content-type': 'application/json' });
    return res.end(anon ? JSON.stringify(maskDeep(JSON.parse(body))) : body);
  }
  if (url.pathname === '/api/raw') {
    const id = Number(url.searchParams.get('id')), at = Number(url.searchParams.get('at_ms'));
    const rec = await fetchRaw(id, at).catch(() => null);
    return rec ? sendJson(rawSummary(rec)) : sendJson({ error: 'raw-io record not found (archived, capture off, or unknown id)' }, 404);
  }
  if (url.pathname === '/api/health') {
    res.writeHead(200, { ...sec, 'content-type': 'application/json' });
    return res.end(JSON.stringify({ ok: true, host: HOST, daemon: DAEMON, cached_age_ms: cache.at ? Date.now() - cache.at : null }));
  }
  if ((m = /^\/ui\/([A-Za-z0-9_][A-Za-z0-9_.-]*\.(?:js|css))$/.exec(url.pathname))) {
    const f = path.join(UI_DIST, m[1]);
    return fs.readFile(f, (err, buf) => {
      if (err) { res.writeHead(302, { ...sec, location: UI_CDN + m[1] }); return res.end(); }
      res.writeHead(200, { 'x-content-type-options': 'nosniff', 'cache-control': 'public, max-age=3600', 'content-type': TYPES[path.extname(f)] });
      res.end(req.method === 'HEAD' ? undefined : buf);
    });
  }
  const rel = ROUTES[url.pathname] || url.pathname.replace(/^\/+/, '');
  const file = path.normalize(path.join(ROOT, rel));
  if (!file.startsWith(ROOT + path.sep) || !TYPES[path.extname(file)]) { res.writeHead(404, sec); return res.end('not found'); }
  fs.readFile(file, (err, buf) => {
    if (err) { res.writeHead(404, sec); return res.end('not found'); }
    res.writeHead(200, { ...sec, 'content-type': TYPES[path.extname(file)] });
    res.end(req.method === 'HEAD' ? undefined : buf);
  });
}

// One bad request must never take the server down.
process.on('uncaughtException', (e) => console.error('[llmux-webui] uncaught', e));
process.on('unhandledRejection', (e) => console.error('[llmux-webui] unhandled', e));
const safe = (req, res) => Promise.resolve(handler(req, res)).catch((e) => {
  console.error('[llmux-webui] handler', req.url, e);
  try { if (!res.headersSent) res.writeHead(500, { 'content-type': 'text/plain' }); res.end('internal error'); } catch {}
});

for (const h of BIND) {
  http.createServer(safe).listen(PORT, h, () => console.log(`[llmux-webui] http://${h}:${PORT}/  (daemon ${DAEMON}, host ${HOST})`))
    .on('error', e => console.error(`[llmux-webui] bind ${h}:${PORT} failed: ${e.message}`));
}
