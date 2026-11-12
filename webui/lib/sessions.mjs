// Session index: every Claude Code / Codex / SDK session that went through llmux becomes one card,
// owned by the user (issued key → llmux tenant) whose key sent its requests.
// Keys: Claude Code puts {"device_id","account_uuid","session_id"} JSON in metadata.user_id;
// llmux surfaces it as activity.user_id and keys session_labels by the same string.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { readTranscript, messagesToEntries, transcriptMeta, listSubagents, recoverMessages, parseResponse } from './transcript.mjs';
import { solveRates, avgPerRequest, priceRow, rowTokens, priceCounts, classKey, ratesFor } from './pricing.mjs';
import { parseRawMeta } from './rawmeta.mjs';
import { cleanPrompt, promptOf } from './overview.mjs';
export { promptOf };

const HOME = os.homedir();
export const SOMA_ROOT = process.env.SOMA_CLAUDE_ROOT || '/opt/soma-work/dev/.claude/projects';
export const ROOTS = (process.env.TRANSCRIPT_ROOTS || [path.join(HOME, '.claude/projects'), SOMA_ROOT].join(','))
  .split(',').map(s => s.trim()).filter(Boolean);

export function parseUid(uid) {
  if (!uid) return null;
  try { const j = JSON.parse(uid); return j && j.session_id ? { sid: j.session_id, device: j.device_id || '', accountUuid: j.account_uuid || '' } : null; } catch {}
  const m = /session_([0-9a-f-]{36})/.exec(uid);
  return m ? { sid: m[1], device: '', accountUuid: '' } : null;
}

// soma-work and Claude Code one-shot side calls (safety review, card titles, summaries, recaps,
// next-prompt suggestions). Each opens its own 1–3 request "session"; they are not work a person did.
export const AUX_RE = /SAFETY REVIEWER|Summarize this AI assistant response|<session-title-pending>|Output ONLY a JSON object|Recap in under \d+ words|\[SUGGESTION MODE|Generate a concise[^.\n]{0,40}title|Write a 5-10 word title|Use the Bash tool exactly once to run this exact command/i;
const MAIN_KINDS = new Set(['user', 'sdk', 'other', 'compact', 'recap']);
const SOMA_CWD = /^\/(?:private\/)?tmp\/U[0-9A-Z]+\/session_/;
const HOUR = 3600000;
const KEEP_HOURS = 72;
// The priced counters of an activity row, summed per bucket model (#278 adds the extended-TTL write
// subset and the web-search count, so a history bucket prices like the daemon's rows).
const COUNTS = ['input', 'output', 'cache_read', 'cache_creation', 'cache_creation_extended', 'web_search_requests'];
const zeroCounts = () => Object.fromEntries(COUNTS.map(k => [k, 0]));
const addCounts = (into, t) => { for (const k of COUNTS) into[k] += t[k] || 0; };

// Q4 thresholds (spec 26 §Goals 6).
export const FLAGS = { burstCostHour: 25, burstReq10m: 150, longRunMs: 3 * HOUR, errorBurst: 3, contextPct: 0.8 };

export const shortName = (name) => String(name || '').replace(/\s*\(U[0-9A-Z]+\)\s*$/, '').trim() || name;

// Request order and cursors: (at_ms, id) is unique per row, so a cursor "at_ms:id" pins a position that
// new requests arriving on top cannot shift (offset paging would repeat or skip rows on a live session).
const byTime = (a, b) => a.at_ms - b.at_ms || a.id - b.id;
const parseCursor = (c) => { const m = /^(\d+):(\d+)$/.exec(String(c || '')); return m ? { at_ms: Number(m[1]), id: Number(m[2]) } : null; };
export const cursorOf = (r) => `${r.at_ms}:${r.id}`;

// One page of a session's requests, newest first. `before` = the next older page (scrolling down),
// `after` = requests newer than the top row (a running session); `gap` says more arrived than one page holds.
export function requestPage(rows, { kind = '', limit = 50, before = '', after = '', offset = 0 } = {}) {
  limit = Math.min(200, Math.max(1, Number(limit) || 50));
  const list = (kind ? rows.filter(r => (r.msg_kind || 'other') === kind) : rows).slice().sort(byTime).reverse();
  const total = list.length;
  const a = parseCursor(after), b = parseCursor(before);
  if (a) {
    const newer = list.filter(r => byTime(r, a) > 0);
    return { total, limit, rows: newer.slice(0, limit), gap: newer.length > limit, more: true };
  }
  const start = b ? list.findIndex(r => byTime(r, b) < 0) : Math.max(0, Number(offset) || 0);
  if (start < 0) return { total, limit, rows: [], more: false };
  const page = list.slice(start, start + limit);
  return { total, limit, rows: page, more: start + page.length < total };
}
const slackOf = (name) => { const m = /\((U[0-9A-Z]+)\)\s*$/.exec(String(name || '')); return m ? m[1] : null; };

export class SessionIndex {
  constructor({ fetchRaw, host } = {}) {
    this.sessions = new Map();   // sid -> session
    this.labels = new Map();     // sid -> label
    this.files = new Map();      // sid -> transcript path
    this.filesAt = 0;
    this.byId = new Map();
    this.backfill = { state: 'idle', rows: 0, bytes: 0 };
    this.users = new Map();      // tenant -> {tenant,name,email,kind,suspended,created_at_ms}
    this.hourly = new Map();     // tenant -> Map(hour -> {n, err, models: Map(model -> tokens), accounts: Map})
    this.meta = new Map();       // sid -> parsed raw-io meta (remote sessions)
    this.metaQueue = [];
    this.metaBusy = false;
    this.rates = new Map(); this.avg = new Map();
    this.models = new Map();     // model id -> max_context
    this.fetchRaw = fetchRaw;
    this.host = host;
    this.somaDevices = new Set();
    if (fetchRaw) setInterval(() => this.drainMeta(), 1500).unref?.();
  }

  get(sid) {
    let s = this.sessions.get(sid);
    if (!s) {
      s = { sid, device: '', accountUuid: '', rows: new Map(), inflight: [], first_ms: null, last_ms: null, clients: new Set(), tenants: new Map(), kinds: new Map(), prompts: [], firstExcerpt: null };
      this.sessions.set(sid, s);
    }
    return s;
  }

  price(r) { return priceRow(r, this.rates, this.avg); }

  // Feed the raw daemon dashboard doc (before any slimming — it carries user_id).
  ingest(doc) {
    this.avg = avgPerRequest(doc.model_usage);
    const solved = solveRates(doc.usage_stats);
    if (solved.size) this.rates = solved;
    for (const k of doc.client_keys || []) this.setUser(k.id, { name: k.name, email: k.email, kind: k.kind, suspended: !!k.suspended, created_at_ms: k.created_at_ms });
    for (const t of doc.tenant_usage || []) this.setUser(t.tenant, { name: t.name, email: t.email });
    for (const [uid, label] of Object.entries(doc.session_labels || {})) {
      const p = parseUid(uid); if (!p) continue;
      this.labels.set(p.sid, label);
      const s = this.get(p.sid); s.device = s.device || p.device; s.accountUuid = s.accountUuid || p.accountUuid;
    }
    const live = new Map();
    for (const r of (doc.activity && doc.activity.in_flight) || []) {
      const p = parseUid(r.user_id); if (!p) continue;
      if (!live.has(p.sid)) live.set(p.sid, []);
      live.get(p.sid).push(r);
      if (r.tenant) { const s = this.get(p.sid); s.tenants.set(r.tenant, (s.tenants.get(r.tenant) || 0) || 1); }
    }
    for (const s of this.sessions.values()) s.inflight = live.get(s.sid) || [];
    for (const [sid, list] of live) { const s = this.get(sid); s.inflight = list; this.touch(s, Math.max(...list.map(r => r.started_at_ms))); }
    for (const r of (doc.activity && doc.activity.completed) || []) {
      if (r.kind !== 'request') continue;
      this.addRow(r);
    }
  }

  setModels(list) { for (const m of list || []) { if (!m || !m.id || !m.max_context) continue; const base = m.id.replace(/\[1m\]$/, ''); if (!m.id.endsWith('[1m]') || !this.models.has(base)) this.models.set(base, m.max_context); } }

  setUser(tenant, fields) {
    if (!tenant) return;
    const u = this.users.get(tenant) || { tenant };
    for (const [k, v] of Object.entries(fields)) if (v != null && v !== '') u[k] = v;
    if (tenant === 'local') { u.name = '키 없음 · 이 머신'; u.system = true; }
    this.users.set(tenant, u);
  }

  userOf(tenant) {
    if (!tenant) return null;
    const u = this.users.get(tenant) || { tenant, name: tenant };
    const order = [...this.users.keys()].filter(t => t !== 'local').sort();
    const idx = order.indexOf(tenant);
    return { tenant, name: u.name || tenant, short: shortName(u.name || tenant), slack: slackOf(u.name), email: u.email || null, hue: tenant === 'local' ? null : (idx < 0 ? 0 : idx % 8), system: !!u.system };
  }

  addRow(r) {
    // The dashboard row (at_ms) and the activity.jsonl row (ts_ms) of one request differ by
    // a few ms; ids are per daemon process, so match id within a 10 s window.
    const seen = this.byId.get(r.id);
    if (seen && seen.some(t => Math.abs(t - r.at_ms) < 10000)) return;
    this.byId.set(r.id, (seen || []).concat(r.at_ms).slice(-4));
    if (this.byId.size > 200000) this.byId.delete(this.byId.keys().next().value);
    if (r.tenant && r.client_name) this.setUser(r.tenant, { name: r.client_name });
    this.agg(r);
    const p = parseUid(r.user_id); if (!p) return;
    const s = this.get(p.sid);
    if (p.device && !s.device) s.device = p.device;
    const key = r.id + ':' + r.at_ms;
    const { user_id, excerpt, client_name, ...row } = r;
    s.rows.set(key, row);
    if (client_name) s.clients.add(client_name);
    if (r.tenant) s.tenants.set(r.tenant, (s.tenants.get(r.tenant) || 0) + 1);
    const kind = r.msg_kind || 'other';
    s.kinds.set(kind, (s.kinds.get(kind) || 0) + 1);
    if (excerpt) {
      const ex = String(excerpt).slice(0, 600);
      if (!s.firstExcerpt || r.at_ms < s.firstExcerptAt) { s.firstExcerpt = ex; s.firstExcerptAt = r.at_ms; }
      const prompt = kind === 'user' ? promptOf(ex) : null;
      if (prompt) {
        const ex = prompt;
        const prev = s.prompts.find(x => x.text === ex && Math.abs(x.at - r.at_ms) < 120000);
        if (!prev) { s.prompts.push({ at: r.at_ms, text: ex }); if (s.prompts.length > 400) s.prompts.splice(0, s.prompts.length - 400); }
      }
    }
    this.touch(s, r.at_ms, r.at_ms - (r.duration_ms || 0));
    // Memory bound: a session keeps its newest 4000 requests; `dropped` counts the older ones let go.
    if (s.rows.size > 4000) { s.rows.delete(s.rows.keys().next().value); s.dropped = (s.dropped || 0) + 1; }
  }

  agg(r) {
    const tenant = r.tenant || 'local';
    const h = Math.floor(r.at_ms / HOUR);
    if (!this.hourly.has(tenant)) this.hourly.set(tenant, new Map());
    const m = this.hourly.get(tenant);
    let b = m.get(h);
    if (!b) { b = { n: 0, err: 0, models: new Map(), accounts: new Map(), sub: new Map() }; m.set(h, b); if (m.size > KEEP_HOURS + 4) for (const k of [...m.keys()].sort((a, c) => a - c).slice(0, m.size - KEEP_HOURS)) m.delete(k); }
    b.n++; if (r.status >= 400 || r.aborted) b.err++;
    if (r.account) b.accounts.set(r.account, (b.accounts.get(r.account) || 0) + 1);
    const t = r.tokens || {};
    // Keyed by model AND price class (#278): a bucket's unpriced tokens are priced per class.
    const mk = `${r.model}|${classKey(r.price_class)}`;
    const mt = b.models.get(mk) || { model: r.model, cls: r.price_class, n: 0, ...zeroCounts(), cost: 0, priced: 0 };
    mt.n++; addCounts(mt, t);
    if (r.cost_usd != null) { mt.cost += r.cost_usd; mt.priced++; }
    b.models.set(mk, mt);
    // A history row has no daemon cost: its sub-usages (#278) are priced per model in bucketCost.
    // A daemon row's cost_usd already includes them.
    if (r.cost_usd == null) for (const s of r.sub_usage || []) {
      const model = s.model || r.model, st = s.tokens || {};
      const k = `${model}|${classKey(s.price_class)}`;
      const e = b.sub.get(k) || { model, cls: s.price_class, ...zeroCounts() };
      addCounts(e, st);
      b.sub.set(k, e);
    }
  }

  bucketCost(b) {
    let c = 0;
    for (const t of b.models.values()) {
      const unpriced = t.n - t.priced;
      if (!unpriced) { c += t.cost; continue; }
      const unbilled = !!(t.cls && t.cls.unbilled);
      const r = unbilled ? null : ratesFor(this.rates, t.model, t.cls);
      const est = unbilled ? 0 : r ? priceCounts(t, r) : (this.avg.get(t.model) || 0) * t.n;
      // mixed buckets: daemon-priced part is exact; scale the estimate to the unpriced share
      c += t.priced ? t.cost + est * (unpriced / t.n) : est;
    }
    for (const t of (b.sub || new Map()).values()) {
      if (t.cls && t.cls.unbilled) continue;
      const r = ratesFor(this.rates, t.model, t.cls);
      if (r) c += priceCounts(t, r);
    }
    return c;
  }

  touch(s, last, first) {
    if (!s.last_ms || last > s.last_ms) s.last_ms = last;
    const f = first || last; if (!s.first_ms || f < s.first_ms) s.first_ms = f;
  }

  // Replay the tail of llmux's activity.jsonl so sessions carry their history, not just
  // what this server saw since it started. Rows: {ts_ms,id,kind,user_id,tenant,tokens,excerpt,...}.
  async loadHistory(file, tailBytes = 160 * 1024 * 1024) {
    const st = fs.statSync(file);
    const start = Math.max(0, st.size - tailBytes);
    this.backfill = { state: 'loading', rows: 0, bytes: st.size - start, file };
    const { createInterface } = await import('node:readline');
    const rl = createInterface({ input: fs.createReadStream(file, { start }), crlfDelay: Infinity });
    let first = start > 0;
    for await (const line of rl) {
      if (first) { first = false; continue; } // partial line
      if (!line.startsWith('{')) continue;
      let o; try { o = JSON.parse(line); } catch { continue; }
      if (!o.ts_ms) continue;
      this.addRow({ kind: 'request', id: o.id, at_ms: o.ts_ms, method: o.method, path: o.path, endpoint: o.endpoint, account: o.account, status: o.status, duration_ms: o.duration_ms, tokens: o.tokens, group: o.group, model: o.model, effort: o.effort, fast: o.fast, ttfb_ms: o.ttfb_ms, ttft_ms: o.ttft_ms, gen_ms: o.gen_ms, aborted: o.aborted, msg_kind: o.kind, tenant: o.tenant, user_id: o.user_id, excerpt: o.excerpt, sub_usage: o.sub_usage, price_class: o.price_class, cost_usd: null, _hist: true });
      this.backfill.rows++;
    }
    this.backfill.state = 'done';
  }

  refreshFiles(force) {
    if (!force && Date.now() - this.filesAt < 30000) return;
    this.filesAt = Date.now();
    for (const root of ROOTS) {
      let dirs = []; try { dirs = fs.readdirSync(root); } catch { continue; }
      for (const d of dirs) {
        let names = []; try { names = fs.readdirSync(path.join(root, d)); } catch { continue; }
        for (const n of names) if (n.endsWith('.jsonl') && n.length === 42) this.files.set(n.slice(0, 36), path.join(root, d, n));
      }
    }
    this.somaDevices = new Set();
    for (const [sid, f] of this.files) if (f.startsWith(SOMA_ROOT)) { const s = this.sessions.get(sid); if (s && s.device) this.somaDevices.add(s.device); }
  }

  ownerOf(s) {
    let best = null, n = -1;
    for (const [t, c] of s.tenants) if (c > n) { best = t; n = c; }
    return this.userOf(best);
  }

  contextOf(rows, model) {
    const main = rows.filter(r => MAIN_KINDS.has(r.msg_kind) && r.tokens);
    const r = main[main.length - 1];
    if (!r) return null;
    const t = r.tokens;
    const used = (t.input || 0) + (t.cache_read || 0) + (t.cache_creation || 0) + (t.output || 0);
    let win = this.models.get(model) || 200000;
    if (used > win || used > 170000) win = Math.max(win, 1000000); // 200k sessions auto-compact before ~170k
    return { used, window: win, pct: used / win, parts: { input: t.input || 0, cacheRead: t.cache_read || 0, cacheWrite: t.cache_creation || 0, output: t.output || 0 }, model: r.model };
  }

  card(s, now = Date.now()) {
    let file = this.files.get(s.sid);
    const t = file ? transcriptMeta(file) : null;
    if (file && !t) { this.files.delete(s.sid); file = null; } // transcript deleted since the last scan
    const rm = this.meta.get(s.sid) || null;
    const rows = [...s.rows.values()].sort((a, b) => a.at_ms - b.at_ms);
    const main = rows.filter(r => MAIN_KINDS.has(r.msg_kind));
    const sum = (f) => rows.reduce((a, r) => a + (f(r) || 0), 0);
    const last = Math.max(s.last_ms || 0, (t && t.last_ts) || 0) || null;
    const running = s.inflight.length > 0 || (t && now - t.last_ts < 15000);
    let status;
    if (running) status = 'running';
    // A tool call is executing (no request in flight, transcript waits on the tool result).
    else if (t && t.lastRole === 'assistant' && t.lastStop === 'tool_use' && now - last < 10 * 60000) status = 'running';
    else if (last && now - last < 30 * 60000) status = t && t.lastStop === 'tool_use' && t.lastRole === 'assistant' ? 'stalled' : 'waiting';
    else if (last && now - last < 24 * HOUR) status = 'idle';
    else status = 'past';
    const label = this.labels.get(s.sid) || '';
    const cleanLabel = cleanPrompt(label);
    const cwd = (t && t.cwd) || (rm && rm.cwd) || null;
    const firstPrompt = cleanPrompt((t && t.firstPrompt) || (s.prompts.length ? s.prompts.reduce((a, b) => (a.at <= b.at ? a : b)).text : null) || (rm && rm.firstPrompt) || s.firstExcerpt || '');
    const lastPromptRaw = (t && t.lastPrompt) || (s.prompts.length ? s.prompts.reduce((a, b) => (a.at >= b.at ? a : b)).text : null) || (rm && rm.prompts && rm.prompts[rm.prompts.length - 1]) || null;
    const lastPrompt = cleanPrompt(lastPromptRaw || '');
    const probe = [label, s.firstExcerpt, t && t.firstPrompt, rm && rm.firstPrompt].filter(Boolean).join(' \n ');
    // A side call: at most 3 requests, and every prompt it carries is a side-call template.
    const aux = rows.length <= 3 && AUX_RE.test(probe) && s.prompts.every(p => AUX_RE.test(p.text));
    const place = (file && file.startsWith(SOMA_ROOT)) || SOMA_CWD.test(cwd || '') || (!file && s.device && this.somaDevices.has(s.device)) ? 'soma-work'
      : file ? 'host' : 'remote';
    const codex = rows.some(r => r.group === 'codex');
    const entry = (t && t.entrypoint) || (rm && rm.entrypoint) || null;
    const kind = codex ? 'Codex' : !entry || entry === 'cli' ? 'Claude Code' : /sdk/.test(entry) ? 'SDK' : 'Claude Code';
    const client = place === 'soma-work' ? 'soma-work' : /\/soul\//.test(cwd || '') ? 'soma' : kind;
    const accounts = {}; for (const r of rows) if (r.account) accounts[r.account] = (accounts[r.account] || 0) + 1;
    const models = {}; for (const r of rows) if (r.model) models[r.model] = (models[r.model] || 0) + 1;
    const model = (t && t.model) || Object.entries(models).sort((a, b) => b[1] - a[1])[0]?.[0] || null;
    const costEst = rows.reduce((a, r) => a + this.price(r), 0);
    const cost1h = rows.filter(r => r.at_ms > now - HOUR).reduce((a, r) => a + this.price(r), 0);
    const req10m = rows.filter(r => r.at_ms > now - 10 * 60000).length;
    const err30m = rows.filter(r => r.at_ms > now - 30 * 60000 && (r.status >= 400 || r.aborted)).length;
    const ctx = this.contextOf(rows, model);
    const lastPromptAt = s.prompts.length ? Math.max(...s.prompts.map(p => p.at)) : null;
    const flags = [];
    if (cost1h >= FLAGS.burstCostHour) flags.push({ id: 'cost', text: `1시간 $${cost1h.toFixed(0)}`, tone: 'danger' });
    if (req10m >= FLAGS.burstReq10m) flags.push({ id: 'burst', text: `10분 ${req10m}요청`, tone: 'warn' });
    if (err30m >= FLAGS.errorBurst) flags.push({ id: 'errors', text: `오류 ${err30m}건`, tone: 'danger' });
    if (status === 'stalled') flags.push({ id: 'stuck', text: '도구 응답 대기', tone: 'warn' });
    if (status === 'running' && lastPromptAt && now - lastPromptAt > FLAGS.longRunMs) flags.push({ id: 'long', text: `지시 후 ${Math.round((now - lastPromptAt) / HOUR)}시간째`, tone: 'warn' });
    if (ctx && ctx.pct >= FLAGS.contextPct) flags.push({ id: 'context', text: `컨텍스트 ${Math.round(ctx.pct * 100)}%`, tone: ctx.pct >= 0.92 ? 'danger' : 'warn' });
    const title = (t && t.title) || cleanLabel.slice(0, 120) || firstPrompt.slice(0, 120) || null;
    return {
      sid: s.sid,
      title: title || `제목 없음 · ${s.sid.slice(0, 8)}`,
      untitled: !title,
      label: cleanLabel.slice(0, 200),
      status,
      client, kind, place,
      owner: this.ownerOf(s),
      aux,
      device: s.device ? s.device.slice(0, 8) : null,
      deviceInfo: rm ? { os: rm.os, arch: rm.arch, version: rm.version, osVersion: rm.osVersion } : null,
      cwd, project: cwd ? path.basename(cwd) : null, branch: (t && t.branch) || (rm && rm.branch) || null, version: (t && t.version) || (rm && rm.version) || null,
      model,
      effort: (main[main.length - 1] || {}).effort || null,
      firstPrompt: firstPrompt.slice(0, 300) || null,
      lastPrompt: lastPrompt.slice(0, 300) || null,
      prompts: s.prompts.length || (t && t.prompts) || (rm && rm.promptCount) || 0,
      first_ms: s.first_ms || (t && t.first_ts) || null, last_ms: last,
      inflight: s.inflight.length,
      inflightKinds: s.inflight.map(r => r.kind),
      requests: rows.length, mainRequests: main.length, subagentRequests: s.kinds.get('subagent') || 0,
      sideRequests: ['title', 'status', 'recap', 'suggest', 'count', 'compact', 'audit'].reduce((a, k) => a + (s.kinds.get(k) || 0), 0),
      errors: rows.filter(r => r.status >= 400 || r.aborted).length,
      // Session totals count each request's sub-usages too (#278), as its cost does.
      tokens_in: sum(r => { const t = rowTokens(r); return t.input + t.cache_read + t.cache_creation; }),
      tokens_out: sum(r => rowTokens(r).output),
      costEst, cost1h, req10m,
      ccCost: t && t.cost,
      context: ctx,
      flags,
      accounts, models,
      transcript: t ? { bytes: t.bytes } : null,
      links: (t && t.links) || [],
      resume: t && t.entrypoint === 'cli' ? `claude --resume ${s.sid}` : null,
      hasRaw: rows.length > 0,
    };
  }

  list({ limit = 400, aux = false } = {}) {
    this.refreshFiles();
    const now = Date.now();
    const cards = [];
    const auxBy = {};
    for (const s of this.sessions.values()) {
      if (!s.rows.size && !s.inflight.length && !this.files.has(s.sid)) continue; // label-only, nothing to show
      let c; try { c = this.card(s, now); } catch (e) { console.error('[sessions] card', s.sid, e.message); continue; }
      if (c.aux) { const k = (c.owner && c.owner.tenant) || 'local'; auxBy[k] = (auxBy[k] || 0) + 1; if (!aux) continue; }
      cards.push(c);
    }
    cards.sort((a, b) => (b.last_ms || 0) - (a.last_ms || 0));
    const out = cards.slice(0, limit);
    this.queueMeta(out, now);
    return { cards: out, auxBy, total: cards.length };
  }

  // Remote sessions have no transcript here; their newest raw-io request says who/where/what.
  queueMeta(cards, now) {
    for (const c of cards) {
      if (c.place !== 'remote' || !c.hasRaw || now - (c.last_ms || 0) > 7 * 24 * HOUR) continue;
      const m = this.meta.get(c.sid);
      if (m && (now - m.fetchedAt < 10 * 60000 || m.rawId === this.newestMainRow(c.sid)?.id)) continue;
      if (!this.metaQueue.includes(c.sid)) this.metaQueue.push(c.sid);
    }
    if (this.metaQueue.length > 60) this.metaQueue.length = 60;
  }

  newestMainRow(sid) {
    const rows = this.rowsFor(sid);
    for (let i = rows.length - 1; i >= 0; i--) if (MAIN_KINDS.has(rows[i].msg_kind) && rows[i].status < 400) return rows[i];
    return rows.filter(r => r.status < 400).pop() || null;
  }

  async loadMeta(sid) {
    const r = this.newestMainRow(sid);
    if (!r || !this.fetchRaw) return this.meta.get(sid) || null;
    const have = this.meta.get(sid);
    if (have && have.rawId === r.id && have.at === r.at_ms) return have;
    const rec = await this.fetchRaw(r.id, r.at_ms).catch(() => null);
    const m = parseRawMeta(rec);
    if (m) { m.fetchedAt = Date.now(); this.meta.set(sid, m); if (this.meta.size > 400) this.meta.delete(this.meta.keys().next().value); }
    else if (have) have.fetchedAt = Date.now();
    else this.meta.set(sid, { fetchedAt: Date.now(), missing: true });
    return this.meta.get(sid);
  }

  async drainMeta() {
    if (this.metaBusy || !this.metaQueue.length) return;
    this.metaBusy = true;
    try { await this.loadMeta(this.metaQueue.shift()); } catch (e) { console.error('[sessions] meta', e.message); } finally { this.metaBusy = false; }
  }

  droppedFor(sid) { const s = this.sessions.get(sid); return (s && s.dropped) || 0; }

  rowsFor(sid) {
    const s = this.sessions.get(sid); if (!s) return [];
    return [...s.rows.values()].sort(byTime);
  }

  // Per-user view for the 사용자 tab: window usage comes from /llmux/keys/usage (exact); live state,
  // origin split, hourly spend and accounts come from this index.
  usersView(keysUsage, cards, now = Date.now()) {
    const byTenant = new Map((keysUsage && keysUsage.tenants || []).map(t => [t.tenant, t]));
    const tenants = new Set([...this.users.keys(), ...byTenant.keys(), ...this.hourly.keys()]);
    const hNow = Math.floor(now / HOUR);
    const out = [];
    for (const tenant of tenants) {
      const u = this.userOf(tenant);
      const full = this.users.get(tenant) || {};
      const mine = cards.filter(c => c.owner && c.owner.tenant === tenant);
      const live = { running: 0, waiting: 0, stalled: 0, today: 0, flagged: 0 };
      const origins = {};
      const devices = new Set();
      for (const c of mine) {
        if (c.status === 'running') live.running++; else if (c.status === 'waiting') live.waiting++; else if (c.status === 'stalled') live.stalled++;
        if (now - (c.last_ms || 0) < 24 * HOUR) {
          live.today++;
          const o = c.place === 'soma-work' ? 'soma-work' : c.place === 'remote' ? '개인 PC' : (this.host || '이 머신');
          origins[o] = (origins[o] || 0) + 1;
          if (c.device && c.place !== 'soma-work') devices.add(c.device);
        }
        if (c.flags && c.flags.length && (c.status === 'running' || c.status === 'waiting' || c.status === 'stalled')) live.flagged++;
      }
      const hb = this.hourly.get(tenant) || new Map();
      const hourly = [];
      const accounts = new Map();
      for (let h = hNow - 23; h <= hNow; h++) {
        const b = hb.get(h);
        hourly.push({ h, at: h * HOUR, cost: b ? this.bucketCost(b) : 0, req: b ? b.n : 0, err: b ? b.err : 0 });
        if (b) for (const [a, n] of b.accounts) accounts.set(a, (accounts.get(a) || 0) + n);
      }
      const usage = byTenant.get(tenant) || null;
      out.push({
        ...u, kind: full.kind || null, suspended: !!full.suspended, created_at_ms: full.created_at_ms || null,
        usage: usage ? { requests: usage.requests, ok: usage.ok, errors: usage.errors, tokens_in: usage.tokens_in, tokens_out: usage.tokens_out, cost_usd: usage.cost_usd, first_ms: usage.first_ms, last_ms: usage.last_ms, models: (usage.models || []).map(m => ({ model: m.model, group: m.group, requests: m.requests, cost_usd: m.cost_usd })) } : null,
        live, origins, devices: devices.size,
        active: mine.filter(c => c.status === 'running' || c.status === 'waiting' || c.status === 'stalled').slice(0, 6).map(c => ({ sid: c.sid, title: c.title, status: c.status, client: c.client, project: c.project })),
        hourly,
        accounts: [...accounts.entries()].sort((a, b) => b[1] - a[1]).slice(0, 6).map(([name, n]) => ({ name, n })),
        last_ms: Math.max(usage ? usage.last_ms || 0 : 0, ...mine.map(c => c.last_ms || 0), 0) || null,
      });
    }
    out.sort((a, b) => (b.usage ? b.usage.cost_usd : 0) - (a.usage ? a.usage.cost_usd : 0) || (b.last_ms || 0) - (a.last_ms || 0));
    return out;
  }

  // Chat entries. Prefer the local transcript; fall back to the newest main-thread raw-io record.
  async chat(sid, fetchRaw) {
    this.refreshFiles();
    const file = this.files.get(sid);
    const rows = this.rowsFor(sid);
    if (file) {
      const tr = readTranscript(file);
      const entries = messagesToEntries(tr.msgs);
      joinMetrics(entries, rows);
      return { source: 'transcript', file, meta: tr.meta, entries, subagents: listSubagents(file).map(({ file, ...a }) => a) };
    }
    const main = this.newestMainRow(sid);
    if (!main) return { source: 'none', entries: [], subagents: [] };
    const rec = await fetchRaw(main.id, main.at_ms);
    if (!rec) return { source: 'none', entries: [], subagents: [], note: 'raw-io 기록을 찾을 수 없습니다 (보관 기간이 지났거나 캡처가 꺼져 있음)' };
    const m = parseRawMeta(rec); if (m) { m.fetchedAt = Date.now(); this.meta.set(sid, m); }
    const req = recoverMessages(rec.request_body || '');
    const resp = parseResponse(rec.response_body || '');
    let msgs = req.messages;
    if (!msgs.length && rec.request_body) msgs = responsesInputToMessages(rec.request_body);
    msgs = msgs.concat(resp.blocks.length ? [{ role: 'assistant', content: resp.blocks, _meta: { ts: main.at_ms, model: resp.model } }] : []);
    const entries = messagesToEntries(msgs);
    // raw-io messages carry no timestamps: spread the session's own prompt times over the user turns
    const prompts = (this.sessions.get(sid)?.prompts || []).slice().sort((a, b) => a.at - b.at);
    const users = entries.filter(e => e.kind === 'user');
    const off = Math.max(0, users.length - prompts.length);
    users.forEach((e, i) => { const p = prompts[i - off]; if (p && !e.ts) e.ts = p.at; });
    let lastTs = null; for (const e of entries) { if (e.ts) lastTs = e.ts; else if (lastTs) e.ts = lastTs; }
    joinMetrics(entries, rows);
    return { source: 'raw-io', truncated: req.truncated, entries, subagents: [], rawRef: { id: main.id, at_ms: main.at_ms } };
  }

  subagentChat(sid, aid) {
    const file = this.files.get(sid); if (!file) return null;
    const a = listSubagents(file).find(x => x.id === aid); if (!a) return null;
    const tr = readTranscript(a.file);
    const msgs = tr.msgs.length ? tr.msgs : sidechainMsgs(a.file);
    return { type: a.type, description: a.description, entries: messagesToEntries(msgs) };
  }
}

function sidechainMsgs(file) {
  const out = [];
  for (const line of fs.readFileSync(file, 'utf8').split('\n')) {
    let o; try { o = JSON.parse(line); } catch { continue; }
    if ((o.type === 'user' || o.type === 'assistant') && o.message) out.push({ role: o.message.role, content: o.message.content, _meta: { ts: o.timestamp ? Date.parse(o.timestamp) : null, model: o.message.model, requestId: o.requestId, isMeta: o.isMeta } });
  }
  return out;
}

// OpenAI Responses `input` items -> Anthropic-shaped messages (Codex sessions).
function responsesInputToMessages(body) {
  let j; try { j = JSON.parse(body); } catch { return []; }
  const out = [];
  for (const it of j.input || []) {
    if (it.type === 'message' || (!it.type && it.role)) {
      const text = Array.isArray(it.content) ? it.content.map(c => c.text || '').join('\n') : String(it.content || '');
      if (it.role === 'system' || it.role === 'developer') continue;
      out.push({ role: it.role === 'assistant' ? 'assistant' : 'user', content: [{ type: 'text', text }] });
    } else if (it.type === 'function_call') {
      let input = {}; try { input = JSON.parse(it.arguments || '{}'); } catch {}
      out.push({ role: 'assistant', content: [{ type: 'tool_use', id: it.call_id, name: it.name, input }] });
    } else if (it.type === 'function_call_output') {
      out.push({ role: 'user', content: [{ type: 'tool_result', tool_use_id: it.call_id, content: typeof it.output === 'string' ? it.output : JSON.stringify(it.output) }] });
    } else if (it.type === 'reasoning') {
      out.push({ role: 'assistant', content: [{ type: 'thinking', thinking: (it.summary || []).map(s => s.text).join('\n') }] });
    }
  }
  return out;
}

// Attach llmux metrics (account, duration, ttfb, tokens, cost) to the transcript steps that
// produced them: an assistant step lands within a few seconds of its request's completion.
function joinMetrics(entries, rows) {
  const main = rows.filter(r => MAIN_KINDS.has(r.msg_kind));
  if (!main.length) return;
  const used = new Set();
  let lastReq = null;
  for (const e of entries) {
    if (!(e.kind === 'assistant' || e.kind === 'tool' || e.kind === 'thinking') || !e.ts) continue;
    if (e.requestId && e.requestId === lastReq?.rid) { e.req = lastReq.ref; continue; }
    let best = null, bestD = 8000;
    for (const r of main) {
      const k = r.id + ':' + r.at_ms; if (used.has(k)) continue;
      const d = Math.abs(r.at_ms - e.ts); if (d < bestD) { best = r; bestD = d; }
    }
    if (best) {
      used.add(best.id + ':' + best.at_ms);
      const ref = { id: best.id, at_ms: best.at_ms, account: best.account, duration_ms: best.duration_ms, ttfb_ms: best.ttfb_ms, tokens: best.tokens, cost_usd: best.cost_usd, status: best.status, model: best.model };
      e.req = ref; lastReq = { rid: e.requestId, ref };
    }
  }
}
