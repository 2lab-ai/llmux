/* llmux web UI drafts v2 — shared core: API, formatting, Claude-Code-style chat renderer,
 * work log, raw request viewer, and the Accounts / Activities / Keys views.
 * Sessions are the primary object: one Claude Code / Codex session = one card. */
(function () {
  'use strict';
  const qs = new URLSearchParams(location.search);
  const ANON = qs.has('anon');
  const Q = ANON ? '?anon' : '';
  const X = {};
  window.X = X;
  // ?shot: screenshot-safe mode for sharing outside this machine — hides prompt previews,
  // blurs every live console except ?own=<session id>. (?anon masks e-mails in all data.)
  if (qs.has('shot')) {
    document.documentElement.classList.add('x-shot');
    const own = qs.get('own') || '';
    const css = document.createElement('style');
    css.textContent = `.x-shot .sum,.x-shot .it-last{display:none!important}.x-shot .tile:not([data-sid="${own.replace(/[^0-9a-f-]/g, '')}"]) .tb{filter:blur(6px)}`;
    document.head.appendChild(css);
  }

  /* ---------- api ---------- */
  const j = async (u) => { const r = await fetch(u, { cache: 'no-store' }); if (!r.ok) throw new Error((await r.text()).slice(0, 200)); return r.json(); };
  X.api = {
    sessions: () => j('/api/sessions' + Q),
    session: (sid, tail) => j('/api/session/' + sid + (tail ? '?tail=' + tail + (ANON ? '&anon' : '') : Q)),
    agent: (sid, aid) => j('/api/session/' + sid + '/agent/' + aid + Q),
    raw: (id, at) => j(`/api/raw?id=${id}&at_ms=${at}${ANON ? '&anon' : ''}`),
    dashboard: () => j('/api/dashboard' + Q),
    keysUsage: (w) => j('/api/keys-usage?window=' + (w || '24h') + (ANON ? '&anon' : '')),
  };
  X.poll = function (fn, ms) { let stop = false; const run = async () => { if (stop) return; if (!document.hidden) { try { await fn(); } catch (e) { console.warn(e); } } setTimeout(run, ms); }; run(); return () => { stop = true; }; };

  // Replace innerHTML only when it changed, so clicks and text selection survive polling.
  X.setHTML = (el, html) => { if (el && el._x !== html) { el._x = html; el.innerHTML = html; } };

  /* ---------- format ---------- */
  X.esc = (s) => String(s ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  X.n = (v) => { if (v == null || isNaN(v)) return '—'; const a = Math.abs(v); return a >= 1e9 ? (v / 1e9).toFixed(1) + 'G' : a >= 1e6 ? (v / 1e6).toFixed(1) + 'M' : a >= 1e3 ? (v / 1e3).toFixed(1) + 'k' : String(Math.round(v)); };
  X.usd = (v) => v == null || isNaN(v) ? '—' : v >= 1000 ? '$' + X.n(v) : v >= 1 ? '$' + v.toFixed(2) : '$' + v.toFixed(3);
  X.dur = (secs) => { if (secs == null || isNaN(secs)) return '—'; secs = Math.max(0, Math.round(secs)); const d = Math.floor(secs / 86400), h = Math.floor(secs % 86400 / 3600), m = Math.floor(secs % 3600 / 60), s = secs % 60; return d ? `${d}d ${h}h` : h ? `${h}h ${m}m` : m ? `${m}m ${s}s` : `${s}s`; };
  X.ms = (v) => v == null || isNaN(v) ? '—' : v >= 60000 ? X.dur(v / 1000) : v >= 1000 ? (v / 1000).toFixed(1) + 's' : Math.round(v) + 'ms';
  X.ago = (ms) => !ms ? '—' : (Date.now() - ms < 5000 ? 'now' : X.dur((Date.now() - ms) / 1000).split(' ')[0] + ' ago');
  X.time = (ms) => { if (!ms) return ''; const d = new Date(ms); return [d.getHours(), d.getMinutes(), d.getSeconds()].map(x => String(x).padStart(2, '0')).join(':'); };
  X.when = (ms) => { if (!ms) return '—'; const d = new Date(ms), now = new Date(); return d.toDateString() === now.toDateString() ? X.time(ms).slice(0, 5) : `${d.getMonth() + 1}/${d.getDate()} ${X.time(ms).slice(0, 5)}`; };
  X.clean = (t) => String(t || '').replace(/<speaker>[^<]*<\/speaker>/g, '').replace(/<\/?[a-z_-]+(\s[^>]*)?>/gi, ' ').replace(/\s+/g, ' ').trim();
  X.short = (acct) => !acct ? '—' : acct.includes(':') ? acct.slice(acct.indexOf(':') + 1) : acct;
  X.group = (acct) => (acct || '').split(':')[0];

  X.STATUS = {
    running: { label: '작업 중', en: 'Running', tone: 'run' },
    waiting: { label: '입력 대기', en: 'Your turn', tone: 'wait' },
    stalled: { label: '멈춤', en: 'Stalled', tone: 'warn' },
    idle: { label: '쉬는 중', en: 'Idle', tone: 'idle' },
    past: { label: '지난 세션', en: 'Past', tone: 'past' },
  };
  X.CLIENT_ICON = { 'soma-work': '◆', 'Claude Code': '✳', 'Codex': '◎', 'sdk-ts': '◇' };

  /* ---------- light markdown (assistant text) ---------- */
  X.md = function (src) {
    const parts = String(src || '').split(/```/);
    return parts.map((p, i) => {
      if (i % 2) { const nl = p.indexOf('\n'); return `<pre class="x-code">${X.esc(nl >= 0 ? p.slice(nl + 1) : p)}</pre>`; }
      return X.esc(p)
        .replace(/^### (.*)$/gm, '<b>$1</b>').replace(/^## (.*)$/gm, '<b>$1</b>').replace(/^# (.*)$/gm, '<b>$1</b>')
        .replace(/\*\*([^*]+)\*\*/g, '<b>$1</b>').replace(/`([^`\n]+)`/g, '<code>$1</code>')
        .replace(/^(\s*)[-*] /gm, '$1• ');
    }).join('');
  };

  /* ---------- chat (Claude Code transcript look) ---------- */
  const TOOL_LABEL = { mcp__slack: 'Slack', mcp__github: 'GitHub', mcp__filesystem: 'FS', 'mcp__model-command': 'cmd' };
  const toolName = (n) => { if (!n) return 'tool'; if (n.startsWith('mcp__')) { const p = n.split('__'); return `${p[1]}·${p.slice(2).join('_')}`; } return n; };
  X.metric = (r) => !r ? '' : `<span class="x-metric" title="llmux request #${r.id} · ${X.esc(r.account || '')}">${X.esc(X.short(r.account))} · ${X.ms(r.duration_ms)} · ${X.n(r.tokens ? r.tokens.output : null)} out · ${X.usd(r.cost_usd)}</span>`;

  X.renderChat = function (entries, opt = {}) {
    const showMeta = !!opt.showMeta, showThinking = opt.showThinking !== false;
    const out = [];
    for (const e of entries) {
      if (e.kind === 'meta' && !showMeta) continue;
      if (e.kind === 'user') out.push(`<div class="x-msg x-user"><span class="x-gl">›</span><div class="x-body">${X.esc(e.text)}${e.more ? `<span class="x-more">… +${X.n(e.more)} chars</span>` : ''}</div><span class="x-ts">${X.time(e.ts)}</span></div>`);
      else if (e.kind === 'agentmsg') out.push(`<div class="x-msg x-meta"><span class="x-gl">⇄</span><div class="x-body">${X.esc(e.text.slice(0, 400))}${e.text.length > 400 || e.more ? '…' : ''}</div></div>`);
      else if (e.kind === 'meta') out.push(`<div class="x-msg x-meta"><span class="x-gl">·</span><div class="x-body">${X.esc(e.text.slice(0, 300))}${e.text.length > 300 || e.more ? '…' : ''}</div></div>`);
      else if (e.kind === 'assistant') out.push(`<div class="x-msg x-asst"><span class="x-gl x-dot">⏺</span><div class="x-body">${X.md(e.text)}${e.more ? `<span class="x-more">… +${X.n(e.more)} chars</span>` : ''}</div><span class="x-ts">${X.time(e.ts)}${e.req ? '<br>' + X.metric(e.req) : ''}</span></div>`);
      else if (e.kind === 'thinking' && showThinking) out.push(`<details class="x-msg x-think"><summary><span class="x-gl">✻</span> Thinking… <span class="x-dim">${X.n(e.chars)} chars</span></summary><div class="x-body x-dim">${X.esc(e.text)}${e.more ? '…' : ''}</div></details>`);
      else if (e.kind === 'tool') {
        const res = e.result ? e.result.text : null;
        const lines = res ? res.split('\n') : [];
        const head = lines.slice(0, opt.resultLines || 4).join('\n');
        const isAgent = e.name === 'Agent' || e.name === 'Task';
        const st = e.is_error ? 'x-err' : res == null ? 'x-pending' : '';
        out.push(`<div class="x-msg x-tool ${st}" data-tool="${X.esc(e.id)}"><span class="x-gl x-dot">⏺</span><div class="x-body"><b>${X.esc(toolName(e.name))}</b>(<span class="x-arg">${X.esc(e.summary)}</span>)${e.edit ? ` <span class="x-diff">+${e.edit.added} −${e.edit.removed}</span>` : ''}
          ${isAgent && opt.agentLink ? ` <a class="x-agent" data-agent-tool="${X.esc(e.id)}">open subagent ›</a>` : ''}
          ${res != null ? `<div class="x-res"><span class="x-elbow">⎿</span><pre>${X.esc(head) || '(no output)'}${lines.length > (opt.resultLines || 4) || e.result.more ? `\n<span class="x-dim">… +${lines.length - (opt.resultLines || 4) > 0 ? lines.length - (opt.resultLines || 4) + ' lines' : ''}${e.result.more ? ' ' + X.n(e.result.more) + ' chars' : ''}</span>` : ''}</pre></div>` : `<div class="x-res"><span class="x-elbow">⎿</span><span class="x-dim">running…</span></div>`}
        </div><span class="x-ts">${X.time(e.ts)}${e.req ? '<br>' + X.metric(e.req) : ''}</span></div>`);
      }
    }
    return out.join('') || '<div class="x-empty">No transcript yet.</div>';
  };

  /* ---------- work log (per-request) ---------- */
  const KIND_ICON = { sdk: '◆', user: '›', other: '◆', subagent: '⑂', count: '#', title: 'T', suggest: '?', compact: '⇣', security: '⛨', summary: 'Σ', quota: '%', recap: '↺', audit: '✓' };
  X.renderWorklog = function (rows, opt = {}) {
    const list = rows.slice().sort((a, b) => b.at_ms - a.at_ms).slice(0, opt.limit || 400);
    if (opt.compact) return `<div class="x-wl x-wl-c x-wl-h"><span>time</span><span>kind</span><span>model · account</span><span>st</span><span>dur</span><span>out</span></div>` + list.map(r => {
      const err = r.status >= 400;
      return `<div class="x-wl x-wl-c ${err ? 'x-err' : ''} ${r.msg_kind === 'subagent' ? 'x-sub' : ''}" data-raw="${r.id}:${r.at_ms}"><span class="x-wl-t">${X.time(r.at_ms)}</span><span class="x-wl-k">${KIND_ICON[r.msg_kind] || '·'} ${X.esc(r.msg_kind || '')}</span><span class="x-wl-m">${X.esc((r.model || '').replace(/^claude-/, ''))} <span class="x-dim">${X.esc(X.short(r.account).split('@')[0])}</span></span><span class="x-wl-s">${r.status}</span><span class="x-wl-n">${X.ms(r.duration_ms)}</span><span class="x-wl-n">${X.n(r.tokens ? r.tokens.output : null)}</span></div>`;
    }).join('');
    return list.map(r => {
      const err = r.status >= 400;
      const tps = r.tokens && r.gen_ms > 50 ? Math.round(r.tokens.output / (r.gen_ms / 1000)) : null;
      return `<div class="x-wl ${err ? 'x-err' : ''} ${r.msg_kind === 'subagent' ? 'x-sub' : ''}" data-raw="${r.id}:${r.at_ms}">
        <span class="x-wl-t">${X.time(r.at_ms)}</span>
        <span class="x-wl-k" title="${X.esc(r.msg_kind)}">${KIND_ICON[r.msg_kind] || '·'} ${X.esc(r.msg_kind || '')}</span>
        <span class="x-wl-m">${X.esc(r.model || '')}${r.effort ? ` <span class="x-dim">${X.esc(r.effort)}</span>` : ''}</span>
        <span class="x-wl-a">${X.esc(X.short(r.account))}</span>
        <span class="x-wl-s ${err ? 'x-bad' : ''}">${r.status}</span>
        <span class="x-wl-n">${X.ms(r.duration_ms)}</span>
        <span class="x-wl-n">${X.n(r.tokens ? r.tokens.output : null)}</span>
        <span class="x-wl-n">${tps != null ? tps + '/s' : ''}</span>
        <span class="x-wl-n">${X.usd(r.cost_usd)}</span>
      </div>`;
    }).join('') || '<div class="x-empty">No requests seen by llmux for this session yet.</div>';
  };
  X.worklogHead = '<div class="x-wl x-wl-h"><span>time</span><span>kind</span><span>model</span><span>account</span><span>st</span><span>dur</span><span>out</span><span>t/s</span><span>$</span></div>';

  /* ---------- raw request viewer ---------- */
  X.renderRaw = function (r) {
    if (!r || r.error) return `<div class="x-empty">${X.esc(r ? r.error : 'loading…')}</div>`;
    const hdr = (h) => `<table class="x-hdr">${h.map(([k, v]) => `<tr><td>${X.esc(k)}</td><td>${X.esc(v)}</td></tr>`).join('')}</table>`;
    const blocks = r.response.blocks.map(b => b.type === 'text' ? `<div class="x-rb"><span class="x-tag">text</span><pre>${X.esc(b.text)}</pre></div>` : b.type === 'tool_use' ? `<div class="x-rb"><span class="x-tag">tool_use · ${X.esc(b.name)}</span><pre>${X.esc(b.input)}</pre></div>` : `<div class="x-rb"><span class="x-tag">${X.esc(b.type)}</span>${b.chars ? X.n(b.chars) + ' chars' : ''}</div>`).join('');
    return `<div class="x-raw">
      <div class="x-raw-h">#${r.id} · ${X.esc(r.model)} · ${X.esc(X.short(r.account))} · <b class="${r.status >= 400 ? 'x-bad' : ''}">${r.status}</b> · ${X.ms(r.duration_ms)}</div>
      <h5>Request <span class="x-dim">${X.n(r.request.bytes)}B · ${r.request.messages} messages${r.request.tools != null ? ' · ' + r.request.tools + ' tools' : ''}${r.request.truncated ? ' · <span class="x-warn">clipped at raw_io.max_body_bytes</span>' : ''}</span></h5>
      ${r.request.last ? `<div class="x-rb"><span class="x-tag">last message · ${X.esc(r.request.last.role)}</span><pre>${X.esc(r.request.last.content)}</pre></div>` : ''}
      <details><summary>request headers</summary>${hdr(r.request.headers)}</details>
      <h5>Response <span class="x-dim">${X.n(r.response.bytes)}B · stop ${X.esc(r.response.stop || '—')}${r.response.usage ? ` · in ${X.n(r.response.usage.input_tokens)} · out ${X.n(r.response.usage.output_tokens)} · cache ${X.n(r.response.usage.cache_read_input_tokens)}` : ''}</span></h5>
      ${blocks || '<div class="x-dim">no content blocks</div>'}
      <details><summary>response headers</summary>${hdr(r.response.headers)}</details>
    </div>`;
  };

  /* ---------- write-action stub ---------- */
  X.toast = function (msg) {
    let el = document.getElementById('x-toast');
    if (!el) { el = document.createElement('div'); el.id = 'x-toast'; document.body.appendChild(el); }
    el.textContent = msg; el.classList.add('on'); clearTimeout(el._t); el._t = setTimeout(() => el.classList.remove('on'), 2800);
  };
  X.stub = (what) => X.toast(`시안입니다 — "${what}"는 2차에서 llmux 쓰기 API(/llmux/*)에 연결합니다`);

  /* ---------- Accounts view (with management actions) ---------- */
  X.accountState = function (a, p) {
    const now = Date.now() / 1000;
    const w7 = a.seven_day && a.seven_day.resets_at > now ? a.seven_day.utilization : a.seven_day ? 0 : null;
    if (a.paused) return ['paused', 'Paused', 'mute'];
    if (!a.healthy) return ['error', a.auth_failure_status ? 'Sign-in failed ' + a.auth_failure_status : 'Unhealthy', 'bad'];
    if (a.cooldown_until && a.cooldown_until > now) return ['cool', 'Cooling ' + X.dur(a.cooldown_until - now), 'warn'];
    if (w7 != null && w7 >= p.seven_day_max) return ['spent', '7d used up', 'warn'];
    if (a.status === 'active') return ['active', 'Serving', 'good'];
    return ['ok', 'Ready', 'ok'];
  };
  const left = (w) => { if (!w) return null; const now = Date.now() / 1000; return w.resets_at <= now ? 1 : Math.max(0, 1 - w.utilization); };
  X.renderAccounts = function (doc) {
    const groups = ['claude', 'codex', 'grok', 'openrouter', 'gemini'];
    const p = doc.select_params;
    const rows = groups.map(g => {
      const list = doc.accounts.filter(a => X.group(a.name) === g);
      if (!list.length) return '';
      return `<tbody><tr class="x-grp"><td colspan="9">${g} <span class="x-dim">${list.length}</span></td></tr>${list.map(a => {
        const [k, label, tone] = X.accountState(a, p);
        const l5 = left(a.five_hour), l7 = left(a.seven_day);
        const bar = (v) => v == null ? '<span class="x-dim">—</span>' : `<span class="x-bar"><i style="width:${v * 100}%" class="${v < .15 ? 'lo' : v < .4 ? 'mid' : ''}"></i></span> ${Math.round(v * 100)}%`;
        const cur = Object.values(doc.current_by_group || {}).includes(a.name);
        return `<tr data-acct="${X.esc(a.name)}"><td>${cur ? '<b class="x-cur">●</b>' : ''}</td><td><b>${X.esc(X.short(a.name))}</b> <span class="x-dim">${X.esc(a.plan ? a.plan.label || a.plan.plan_type || '' : a.type)}</span></td>
          <td><span class="x-pill ${tone}">${X.esc(label)}</span></td><td>${bar(l5)}</td><td>${bar(l7)}</td>
          <td class="x-num">${X.n(a.session.requests)}</td><td class="x-num">${X.n(a.session.errors)}</td><td class="x-num">${a.in_flight || ''}</td>
          <td class="x-acts"><button data-act="Switch to ${X.esc(X.short(a.name))}">Use now</button><button data-act="${a.paused ? 'Resume' : 'Pause'} ${X.esc(X.short(a.name))}">${a.paused ? 'Resume' : 'Pause'}</button><button data-act="Set limits">Limits</button><button data-act="Re-login ${X.esc(X.short(a.name))}">Re-login</button><button class="danger" data-act="Remove ${X.esc(X.short(a.name))}">Remove</button></td></tr>`;
      }).join('')}</tbody>`;
    }).join('');
    return `<div class="x-toolbar"><b>${doc.accounts.length} accounts</b><span class="x-dim">scheduler ${X.esc(p.mode)} · 5h ≤ ${p.five_hour_max} · 7d ≤ ${p.seven_day_max}</span><span class="x-sp"></span>
      <button class="x-primary" data-act="Add account (browser login: Claude / Codex / Grok / OpenRouter / Gemini)">+ Add account</button><button data-act="Import accounts">Import</button><button data-act="Refresh usage">Refresh usage</button></div>
      <table class="x-table"><thead><tr><th></th><th>account</th><th>state</th><th>5h left</th><th>7d left</th><th class="x-num">requests</th><th class="x-num">errors</th><th class="x-num">now</th><th></th></tr></thead>${rows}</table>`;
  };

  /* ---------- Activities view (every request, all sessions) ---------- */
  X.renderActivities = function (doc, sessionsById, filter) {
    const rows = [...doc.activity.in_flight.map(r => ({ ...r, _run: true, at_ms: r.started_at_ms, msg_kind: r.kind })), ...doc.activity.completed.filter(r => r.kind === 'request')];
    const f = (filter || '').toLowerCase();
    const shown = rows.filter(r => !f || [r.model, r.account, r.msg_kind, r.client_name, String(r.status)].join(' ').toLowerCase().includes(f));
    return `<div class="x-toolbar"><b>${doc.activity.in_flight.length} running · ${doc.activity.completed.length} recent</b><span class="x-dim">${(doc.totals.rpm_5m || 0).toFixed(0)} req/min</span><span class="x-sp"></span><input class="x-filter" placeholder="filter: model, account, kind, status" value="${X.esc(filter || '')}"></div>
      <div class="x-acts-h x-wl x-wl-h"><span>time</span><span>kind</span><span>model</span><span>account</span><span>st</span><span>dur</span><span>out</span><span>client</span><span>$</span></div>
      ${shown.map(r => `<div class="x-wl ${r._run ? 'x-run' : ''} ${r.status >= 400 ? 'x-err' : ''}" ${r._run ? '' : `data-raw="${r.id}:${r.at_ms}"`}>
        <span class="x-wl-t">${X.time(r.at_ms)}</span><span class="x-wl-k">${KIND_ICON[r.msg_kind] || '·'} ${X.esc(r.msg_kind || '')}</span>
        <span class="x-wl-m">${X.esc(r.model || '')}</span><span class="x-wl-a">${X.esc(X.short(r.account))}</span>
        <span class="x-wl-s">${r._run ? '<span class="x-spin"></span>' : r.status}</span><span class="x-wl-n">${r._run ? X.ms(Date.now() - r.started_at_ms) : X.ms(r.duration_ms)}</span>
        <span class="x-wl-n">${r._run ? '—' : X.n(r.tokens ? r.tokens.output : null)}</span><span class="x-wl-a">${X.esc(r.client_name || r.tenant || '')}</span><span class="x-wl-n">${r._run ? '—' : X.usd(r.cost_usd)}</span></div>`).join('')}`;
  };

  /* ---------- Keys view (per-tenant usage) ---------- */
  X.renderKeys = function (ku, w) {
    if (!ku || ku.error) return `<div class="x-empty">${X.esc(ku ? ku.error : 'loading…')}</div>`;
    const rows = (ku.tenants || []).slice().sort((a, b) => (b.cost_usd || 0) - (a.cost_usd || 0));
    const total = rows.reduce((a, r) => a + (r.cost_usd || 0), 0) || 1;
    return `<div class="x-toolbar"><b>${rows.length} tenants</b><span class="x-dim">${X.n(ku.rows)} requests in window</span><span class="x-dim">window</span>${['1h', '24h', '7d', '30d', 'all'].map(x => `<button class="${x === w ? 'on' : ''}" data-window="${x}">${x}</button>`).join('')}<span class="x-sp"></span><button class="x-primary" data-act="Issue a new key">+ New key</button></div>
      <table class="x-table"><thead><tr><th>tenant</th><th>kind</th><th>state</th><th class="x-num">requests</th><th class="x-num">errors</th><th class="x-num">tokens</th><th class="x-num">cost</th><th>share</th><th>last used</th><th></th></tr></thead><tbody>
      ${rows.map(r => `<tr><td><b>${X.esc(r.name || r.tenant)}</b> <span class="x-dim">${X.esc(r.tenant || '')}</span></td><td class="x-dim">${X.esc(r.tenant === 'local' ? 'loopback' : 'issued key')}</td><td><span class="x-pill ok">active</span></td>
        <td class="x-num">${X.n(r.requests)}</td><td class="x-num">${X.n(r.errors)}</td><td class="x-num">${X.n((r.tokens_in || 0) + (r.tokens_out || 0))}</td><td class="x-num">${X.usd(r.cost_usd)}</td>
        <td><span class="x-bar"><i style="width:${(r.cost_usd || 0) / total * 100}%"></i></span></td><td class="x-dim">${X.ago(r.last_ms)}</td>
        <td class="x-acts"><button data-act="Suspend key">Suspend</button><button data-act="Rotate key">Rotate</button></td></tr>`).join('')}
      </tbody></table><div class="x-dim" style="margin-top:8px">${X.esc((ku.health && ku.health.importing) ? 'importing history ' + ku.health.import_pct + '%' : '')}</div>`;
  };
})();
