// llmux web dashboard — page logic. Spec: .prd/26-web-session-dashboard.md.
// Readers: an admin of the host's soma-work. First screen answers "who is doing what" (Q1) and flags
// what is abnormal (Q4); the 사용자 view answers "who uses how much" (Q2); the detail card answers
// "what happened in this session" (Q3) and "what went over the wire" (Q5).
// @2lab.ai/ui elements receive data only; this page owns every fetch.
const X = window.X;
const $ = (id) => document.getElementById(id);
const E = X.esc;
const HOUR = 3600000;

const st = {
  view: viewFromPath(),
  user: new URLSearchParams(location.search).get('user') || '',
  origin: 'all',
  q: '',
  aux: false,
  sel: null,        // open session id
  selUser: null,    // open user tenant
  tab: 'ov',
  window: '24h',
  data: null,       // /api/sessions
  users: null,      // /api/users
  doc: null,        // /api/dashboard (accounts, activity)
  detail: null,     // /api/session/:sid
  openGroups: new Set(),
  ccOpen: new Set(), ccLimit: 400,
  req: { limit: 50, kind: '', total: 0, dropped: 0, rows: [], more: false, busy: '', error: '', gen: 0 },
};

function viewFromPath() {
  const p = location.pathname.replace(/\/$/, '');
  return p === '/users' ? 'users' : p === '/activity' ? 'activity' : p === '/accounts' ? 'accounts' : 'sessions';
}

/* ---------------- formatting ---------------- */
const usd = (v) => (v == null || isNaN(v) ? '—' : v >= 1000 ? '$' + X.n(v) : v >= 10 ? '$' + v.toFixed(0) : v >= 1 ? '$' + v.toFixed(2) : '$' + v.toFixed(3));
const modelShort = (m) => String(m || '').replace(/^claude-/, '').replace(/-\d{8}$/, '');
const ago = (ms) => (!ms ? '—' : Date.now() - ms < 60000 ? '방금' : Date.now() - ms < HOUR ? Math.round((Date.now() - ms) / 60000) + '분 전' : Date.now() - ms < 24 * HOUR ? Math.round((Date.now() - ms) / HOUR) + '시간 전' : Math.round((Date.now() - ms) / 86400000) + '일 전');
const dur = (ms) => (ms == null ? '—' : ms < 60000 ? Math.round(ms / 1000) + '초' : ms < HOUR ? Math.round(ms / 60000) + '분' : ms < 86400000 ? (ms / HOUR).toFixed(1) + '시간' : (ms / 86400000).toFixed(1) + '일');
const placeText = (c) => (c.place === 'soma-work' ? 'soma-work' : c.place === 'remote' ? '개인 PC' : (st.data && st.data.host) || '이 머신');
const placeLong = (c) => {
  if (c.place === 'soma-work') return `soma-work (${(st.data && st.data.host) || '호스트'})`;
  if (c.place === 'remote') { const d = c.deviceInfo || {}; return `개인 PC${d.os ? ' · ' + d.os + (d.arch ? ' ' + d.arch : '') : ''}${c.device ? ' · 기기 ' + c.device : ''}`; }
  return `${(st.data && st.data.host) || '이 머신'}의 콘솔`;
};
const KIND_LABEL = { user: '지시', other: '응답', sdk: 'SDK', subagent: '서브에이전트', title: '제목', recap: '요약', suggest: '다음 입력 제안', count: '토큰 계산', compact: '압축', audit: '감사' };
const relPath = (p, cwd) => (cwd && p && p.startsWith(cwd + '/') ? p.slice(cwd.length + 1) : p);
const refOf = (u) => { const m = /github\.com\/([\w.-]+)\/([\w.-]+)\/(pull|issues)\/(\d+)/.exec(u); return m ? `${m[2]}#${m[4]}` : u.replace(/^https?:\/\//, '').slice(0, 30); };

/* ---------------- app bar ---------------- */
let who = null;
function paintBar() {
  const d = st.data;
  const live = d ? d.sessions.filter(c => c.status === 'running').length : 0;
  const wait = d ? d.sessions.filter(c => c.status === 'waiting' || c.status === 'stalled').length : 0;
  $('bar').data = {
    brand: { label: 'llmux', href: '/', mark: true },
    nav: [
      { id: 'sessions', label: '세션', href: '/', current: st.view === 'sessions', badge: live || undefined, title: '세션 하나 = Claude Code·Codex·soma-work 대화 하나' },
      { id: 'users', label: '사용자', href: '/users', current: st.view === 'users', title: '발급 키(사용자)별 사용량과 지금 하는 일' },
      { id: 'activity', label: '활동', href: '/activity', current: st.view === 'activity', title: '요청 하나하나' },
      { id: 'accounts', label: '계정', href: '/accounts', current: st.view === 'accounts', title: '구독 계정 상태·잔량' },
    ],
    status: st.offline ? { tone: 'fail', label: '데몬 연결 끊김', description: st.offline } : { tone: live ? 'run' : 'idle', label: `${d ? d.host : ''} · 실행 ${live} · 대기 ${wait}`, live: !!live },
    account: who ? { name: (who.user && (who.user.name || who.user.email)) || '관리자', role: '관리자', href: who.logout } : undefined,
    theme: document.documentElement.dataset.theme || 'system',
  };
}
$('bar').addEventListener('ui-navigate', (e) => { e.preventDefault(); go(e.detail.id); });
$('bar').addEventListener('ui-theme-change', (e) => { const t = e.detail.theme; if (t === 'system') { delete document.documentElement.dataset.theme; localStorage.removeItem('llmux-theme'); } else { document.documentElement.dataset.theme = t; localStorage.setItem('llmux-theme', t); } paintBar(); });

function go(view) {
  st.view = view;
  const path = view === 'sessions' ? '/' : '/' + view;
  if (location.pathname !== path) history.pushState(null, '', path + (st.user && view === 'sessions' ? '?user=' + encodeURIComponent(st.user) : ''));
  for (const v of ['sessions', 'users', 'activity', 'accounts']) $('v-' + v).hidden = v !== view;
  document.title = 'llmux · ' + { sessions: '세션', users: '사용자', activity: '활동', accounts: '계정' }[view];
  paintBar();
  if (view === 'users') loadUsers();
  if (view === 'activity' || view === 'accounts') paintOther();
  if (view === 'sessions') paintBoard();
}
addEventListener('popstate', () => go(viewFromPath()));

/* ---------------- sessions board ---------------- */
function filtered() {
  const d = st.data; if (!d) return [];
  const q = st.q.trim().toLowerCase();
  return d.sessions.filter(c => {
    if (st.user && (!c.owner || c.owner.tenant !== st.user)) return false;
    if (st.origin === 'soma' && c.place !== 'soma-work') return false;
    if (st.origin === 'remote' && c.place !== 'remote') return false;
    if (st.origin === 'host' && c.place !== 'host') return false;
    if (q && ![c.title, c.cwd, c.firstPrompt, c.lastPrompt, c.owner && c.owner.name, c.sid, c.branch].some(v => v && String(v).toLowerCase().includes(q))) return false;
    return true;
  });
}

function stepText(c) {
  const k = c.inflightKinds || [];
  if (k.length) {
    const sub = k.filter(x => x === 'subagent').length;
    return `모델 응답 생성 중${sub ? ` · 서브에이전트 ${sub}` : ''}${k.length - sub ? ` · 메인 ${k.length - sub}` : ''}`;
  }
  return '도구 실행 중';
}

function cardOf(c) {
  const active = c.status === 'running' || c.status === 'waiting' || c.status === 'stalled';
  const status = c.status === 'running' ? 'running' : c.status === 'waiting' || c.status === 'stalled' ? 'waiting' : 'done';
  const metrics = [];
  if (c.project) metrics.push({ text: c.project + (c.branch && c.branch !== 'HEAD' ? ` · ${c.branch}` : ''), glyph: 'file', title: c.cwd || '' });
  metrics.push({ text: placeText(c), title: placeLong(c) });
  metrics.push({ text: `${X.n(c.requests)} 요청`, title: `메인 ${c.mainRequests} · 서브에이전트 ${c.subagentRequests} · 부속 ${c.sideRequests}${c.errors ? ' · 오류 ' + c.errors : ''}` });
  if (c.model) metrics.push({ text: modelShort(c.model), glyph: 'model', effort: c.effort || undefined, title: c.model });
  for (const f of c.flags || []) metrics.unshift({ text: f.text, tone: f.tone === 'danger' ? 'danger' : 'warn', title: '주의' });
  const subRun = (c.inflightKinds || []).filter(k => k === 'subagent').length;
  return {
    id: c.sid,
    title: c.title,
    summary: c.lastPrompt || c.firstPrompt || undefined,
    status,
    statusText: c.status === 'stalled' ? '도구 응답 대기' : c.status === 'waiting' ? '입력 대기' : c.status === 'idle' ? '쉬는 중' : c.status === 'past' ? '지난 세션' : undefined,
    since: c.status === 'running' ? c.first_ms || undefined : c.last_ms || undefined,
    badge: c.owner ? { text: c.owner.short, hue: c.owner.hue == null ? 7 : c.owner.hue, title: `${c.owner.name}${c.owner.email ? ' · ' + c.owner.email : ''} · 키 ${c.owner.tenant}`, self: !!(who && who.user && c.owner.slack && who.user.slackUserId === c.owner.slack) } : undefined,
    lead: { glyph: c.client === 'Codex' ? 'openai' : 'claude', title: `${c.client} · ${placeLong(c)}` },
    task: c.status === 'running' ? { label: stepText(c) } : undefined,
    agents: subRun ? { total: subRun, running: subRun } : undefined,
    costUsd: c.costEst,
    contextPct: c.context ? Math.round(c.context.pct * 100) : undefined,
    contextLevel: c.context ? (c.context.pct >= 0.92 ? 'danger' : c.context.pct >= 0.8 ? 'warn' : '') : undefined,
    titles: { cost: '추정 비용 (llmux 단가)', tokens: '입력(캐시 포함)+출력 토큰', context: c.context ? `마지막 메인 요청 ${X.n(c.context.used)} / ${X.n(c.context.window)} 토큰 (추정 창)` : undefined },
    metrics: metrics.slice(0, 5),
    links: (c.links || []).slice(-3).map(u => ({ kind: /\/pull\//.test(u) ? 'pr' : 'issue', href: u, text: refOf(u), title: u })),
    rows: ['summary', 'task', 'links', 'agents', 'usage'],
    attention: active && (c.flags || []).length > 0,
    bodyMode: 'steps',
    details: [c.firstPrompt && `처음 지시: ${c.firstPrompt.slice(0, 90)}`, c.prompts ? `지시 ${c.prompts}회 · ${dur((c.last_ms || 0) - (c.first_ms || 0))}` : null].filter(Boolean),
  };
}

const DAY = (ms) => { const d = new Date(ms); d.setHours(0, 0, 0, 0); return d.getTime(); };
function paintBoard() {
  if (!st.data) return;
  const list = filtered();
  const waiting = [], running = [], done = [];
  for (const c of list) (c.status === 'running' ? running : c.status === 'waiting' || c.status === 'stalled' ? waiting : done).push(c);
  const today = DAY(Date.now());
  const groups = new Map();
  for (const c of done.slice(0, 240)) {
    const d = DAY(c.last_ms || 0);
    const label = d === today ? '오늘' : d === today - 86400000 ? '어제' : new Date(d).toLocaleDateString('ko-KR', { month: 'long', day: 'numeric', weekday: 'short' });
    if (!groups.has(d)) groups.set(d, { id: 'd' + d, label, open: d >= today - 86400000, cards: [] });
    groups.get(d).cards.push(cardOf(c));
  }
  const userLabel = st.user && st.data.users[st.user] ? st.data.users[st.user].short : null;
  $('board').data = {
    columns: [
      { id: 'waiting', label: '응답 필요', emptyText: '사람의 입력을 기다리는 세션이 없습니다', cards: waiting.map(cardOf) },
      { id: 'running', label: '실행 중', emptyText: '지금 도는 세션이 없습니다', cards: running.map(cardOf) },
      { id: 'done', label: '완료', emptyText: '완료된 세션이 없습니다', cards: [], groups: [...groups.values()] },
    ],
    load: 'ready',
    openLabel: '상세 보기',
    announce: { needs: waiting.length, running: running.length },
  };
  $('s-count').textContent = `${userLabel ? userLabel + ' · ' : ''}${list.length}개${done.length > 240 ? ' (완료는 최근 240개)' : ''}`;
  paintChips(); paintAttention(list); paintAux();
}

function paintChips() {
  const d = st.data; if (!d) return;
  const per = new Map();
  for (const c of d.sessions) {
    if (!c.owner) continue;
    const p = per.get(c.owner.tenant) || { u: c.owner, run: 0, wait: 0, n: 0, last: 0 };
    p.n++; if (c.status === 'running') p.run++; if (c.status === 'waiting' || c.status === 'stalled') p.wait++;
    p.last = Math.max(p.last, c.last_ms || 0);
    per.set(c.owner.tenant, p);
  }
  const rows = [...per.values()].sort((a, b) => (b.run - a.run) || (b.wait - a.wait) || (b.last - a.last));
  const chip = (id, label, n, hue, title) => `<button type="button" class="uchip${st.user === id ? ' on' : ''}" data-user="${E(id)}" aria-pressed="${st.user === id}" title="${E(title || '')}">${hue != null ? `<i class="dot" data-hue="${hue}"></i>` : ''}<span>${E(label)}</span>${n ? `<b>${n}</b>` : ''}</button>`;
  $('s-users').innerHTML = chip('', '전체', d.sessions.filter(c => c.status === 'running').length, null, '모든 사용자')
    + rows.map(p => chip(p.u.tenant, p.u.short, p.run || null, p.u.hue == null ? 7 : p.u.hue, `${p.u.name} · 실행 ${p.run} · 대기 ${p.wait} · 세션 ${p.n} · 마지막 ${ago(p.last)}`)).join('');
}
$('s-users').addEventListener('click', (e) => {
  const b = e.target.closest('[data-user]'); if (!b) return;
  st.user = st.user === b.dataset.user ? '' : b.dataset.user;
  history.replaceState(null, '', '/' + (st.user ? '?user=' + encodeURIComponent(st.user) : ''));
  paintBoard();
});

function paintAttention(list) {
  const hot = list.filter(c => (c.flags || []).length && (c.status === 'running' || c.status === 'waiting' || c.status === 'stalled'));
  const el = $('s-attn');
  el.hidden = !hot.length;
  if (!hot.length) return;
  el.innerHTML = `<span class="attn-h">주의 ${hot.length}</span>` + hot.slice(0, 8).map(c => `<button type="button" class="attn-i" data-open="${c.sid}" title="${E(c.title)}"><b>${E(c.owner ? c.owner.short : '?')}</b> ${E(c.title.slice(0, 40))} <span class="${c.flags.some(f => f.tone === 'danger') ? 'bad' : 'warn'}">${E(c.flags.map(f => f.text).join(' · '))}</span></button>`).join('');
}
$('s-attn').addEventListener('click', (e) => { const b = e.target.closest('[data-open]'); if (b) openSession(b.dataset.open); });

function paintAux() {
  const d = st.data; if (!d) return;
  const total = Object.values(d.auxBy || {}).reduce((a, b) => a + b, 0);
  const mine = st.user ? (d.auxBy || {})[st.user] || 0 : total;
  $('s-aux').innerHTML = mine ? `<button type="button" class="linkish" id="aux-t">${st.aux ? '부속 호출 숨기기' : `부속 호출 ${mine.toLocaleString()}건 숨김 · 보기`}</button><span class="hint">안전 검토·카드 제목·요약·다음 입력 제안처럼 1~3요청짜리 자동 호출</span>` : '';
}
$('s-aux').addEventListener('click', (e) => { if (e.target.closest('#aux-t')) { st.aux = !st.aux; loadSessions(); } });

$('s-q').addEventListener('input', (e) => { st.q = e.target.value; paintBoard(); });
customElements.whenDefined('ui-segmented').then(() => {
  $('s-origin').data = { options: [{ value: 'all', label: '전체' }, { value: 'soma', label: 'soma-work' }, { value: 'remote', label: '개인 PC' }, { value: 'host', label: '이 머신' }], value: st.origin, label: '출처' };
});
$('s-origin').addEventListener('ui-change', (e) => { st.origin = e.detail.value; paintBoard(); });

const board = $('board');
board.addEventListener('ui-open', (e) => openSession(e.detail.cardId));
board.addEventListener('ui-toggle', (e) => { if (e.detail && e.detail.expanded && e.detail.cardId) openSession(e.detail.cardId); });

async function loadSessions() {
  const r = await fetch('/api/sessions' + (st.aux ? '?aux=1' : ''), { cache: 'no-store' });
  if (r.status === 401) { location.reload(); return; }
  st.data = await r.json();
  paintBar();
  if (st.view === 'sessions') paintBoard();
}

/* ---------------- detail drawer ---------------- */
const panel = $('panel');
function showPanel(mode) {
  $('pbody').hidden = mode !== 'session';
  $('ubody').hidden = mode !== 'user';
  if (!panel.hasAttribute('open')) { panel.show ? panel.show() : panel.setAttribute('open', ''); }
}
panel.addEventListener('ui-close', () => closePanel());
panel.addEventListener('ui-mode-change', dockPad);
panel.addEventListener('ui-resize', dockPad);
function dockPad() {
  const docked = panel.hasAttribute('open') && panel.getAttribute('data-mode') !== 'sheet' && innerWidth >= 1180;
  $('main').style.marginRight = docked ? (panel.getBoundingClientRect().width || 760) + 'px' : '';
  $('main').classList.toggle('docked', docked);
}
function closePanel() {
  st.sel = null; st.selUser = null; st.detail = null;
  if (panel.hide) panel.hide(); else panel.removeAttribute('open');
  $('main').style.marginRight = ''; $('main').classList.remove('docked');
  const u = new URL(location.href); u.searchParams.delete('session'); history.replaceState(null, '', u.pathname + u.search);
}
// Escape closes the top layer only: with the raw sheet open it closes the sheet (ui-drawer's own key), not the session too.
// Capture phase, so the sheet is still open when this looks.
addEventListener('keydown', (e) => { if (e.key === 'Escape' && (st.sel || st.selUser) && !$('raw-sheet').hasAttribute('open')) closePanel(); }, true);

async function openSession(sid) {
  if (!sid) return;
  if (st.sel !== sid) { st.sel = sid; st.selUser = null; st.detail = null; st.tab = 'ov'; st.openGroups = new Set(); st.ccOpen = new Set(); st.ccLimit = 400; st.req = newReq(st.req && st.req.limit); $('p-raw').innerHTML = ''; $('p-agent').innerHTML = ''; }
  const u = new URL(location.href); u.searchParams.set('session', sid); history.replaceState(null, '', u.pathname + u.search);
  showPanel('session');
  const c = st.data && st.data.sessions.find(x => x.sid === sid);
  if (c) paintHead(c);
  $('t-ov').innerHTML = '<div class="ui-skeleton is-block"></div>';
  await loadDetail(true);
  requestAnimationFrame(dockPad);
}

async function loadDetail(first) {
  const sid = st.sel; if (!sid) return;
  const v = !first && st.detail && st.detail.card && st.detail.card.sid === sid ? st.detail.v : '';
  const r = await fetch('/api/session/' + sid + (v ? '?v=' + encodeURIComponent(v) : ''), { cache: 'no-store' });
  if (!r.ok || st.sel !== sid) return;
  const d = await r.json();
  if (d.same) return;
  st.detail = d;
  if (d.card) paintHead(d.card);
  paintDetail(first);
}

function paintHead(c) {
  const o = c.owner;
  $('ph').innerHTML = `<div class="ph-t"><span class="ph-title" title="${E(c.title)}">${E(c.title)}</span></div>
    <div class="ph-m">${o ? `<span class="ui-owner-badge ownerpill" data-hue="${o.hue == null ? 7 : o.hue}" title="${E(o.name + ' · 키 ' + o.tenant)}">${E(o.short)}</span>` : ''}
    ${c.client !== 'soma-work' ? `<span class="ui-chip is-sm">${E(c.client)}</span>` : ''}<span class="ui-chip is-sm">${E(placeLong(c))}</span>
    ${c.project ? `<span class="ui-chip is-sm" title="${E(c.cwd || '')}">${E(c.project)}${c.branch && c.branch !== 'HEAD' ? ' · ' + E(c.branch) : ''}</span>` : ''}</div>`;
  const status = { running: 'running', waiting: 'waiting', stalled: 'waiting', idle: 'idle', past: 'done' }[c.status] || 'idle';
  const actions = [{ id: 'sid', label: '세션 ID 복사', tone: 'quiet' }];
  if (c.resume) actions.unshift({ id: 'resume', label: 'claude --resume 복사', primary: true });
  if (c.cwd) actions.push({ id: 'cwd', label: '폴더 경로 복사', tone: 'quiet' });
  $('p-status').data = { status, words: { running: '실행 중', waiting: c.status === 'stalled' ? '도구 응답 대기' : '입력 대기', idle: '쉬는 중', done: '지난 세션', failed: '실패', stopped: '멈춤' }, startedAt: c.first_ms || undefined, endedAt: status === 'running' ? undefined : c.last_ms || undefined, actions };
}
$('p-status').addEventListener('ui-action', (e) => {
  const c = st.detail && st.detail.card; if (!c) return;
  const v = { resume: c.resume, sid: c.sid, cwd: c.cwd }[e.detail.id];
  if (v) { navigator.clipboard && navigator.clipboard.writeText(v); X.toast('복사했습니다: ' + v.slice(0, 60)); }
});

function paintDetail(first) {
  const d = st.detail; if (!d) return;
  const c = d.card || {};
  const ov = d.overview || { instructions: [], files: [], links: [] };
  const subs = (d.chat && d.chat.subagents) || [];
  $('p-tabs').data = {
    tabs: [
      { id: 'ov', label: '개요' },
      { id: 'log', label: '작업 로그', badge: ov.instructions.length || undefined },
      { id: 'conv', label: '대화' },
      { id: 'cc', label: 'Claude Code 화면' },
      { id: 'req', label: '요청', badge: d.requestCount || undefined },
      { id: 'agents', label: '에이전트', badge: subs.length || undefined, disabled: !subs.length },
    ],
    value: st.tab,
    label: '세션 상세',
  };
  paintOverview(d, c, ov);
  if (st.tab === 'log' || first) paintWorkLog(d, ov);
  if (st.tab === 'conv' || first) paintConversation(d, c);
  if (st.tab === 'cc') paintCC();
  if (st.tab === 'req') { if (!st.req.rows.length) loadRequests(); else refreshRequests(); }
  if (st.tab === 'agents' || first) paintAgents(d, c, subs);
}
$('p-tabs').addEventListener('ui-change', (e) => { st.tab = e.detail.value; paintDetail(false); });

/* Claude Code screen (ccview.js) */
function paintCC() {
  const d = st.detail; if (!d) return;
  const c = d.card || {};
  X.setHTML($('p-cc'), X.renderCC((d.chat && d.chat.entries) || [], { cwd: c.cwd, model: c.model, branch: c.branch, version: c.version, status: c.status, context: c.context, open: st.ccOpen, limit: st.ccLimit, agentLink: ((d.chat && d.chat.subagents) || []).length > 0 }));
}
$('p-cc').addEventListener('click', (e) => {
  if (e.target.closest('a[href]') || (window.getSelection && String(window.getSelection()).length)) return;
  const ag = e.target.closest('[data-agent-tool]');
  if (ag) { const sub = ((st.detail.chat && st.detail.chat.subagents) || []).find(a => a.toolUseId === ag.dataset.agentTool); st.tab = 'agents'; paintDetail(false); if (sub) $('p-tree').dispatchEvent(new CustomEvent('ui-select', { detail: { id: sub.id } })); return; }
  const more = e.target.closest('[data-cc-more]'); if (more) { st.ccLimit += 400; paintCC(); return; }
  const t = e.target.closest('[data-cc-toggle]'); if (!t) return;
  const k = t.dataset.ccToggle; st.ccOpen.has(k) ? st.ccOpen.delete(k) : st.ccOpen.add(k); paintCC();
});

function paintOverview(d, c, ov) {
  const m = d.meta || {};
  const tools = ov.instructions.reduce((a, i) => a + i.toolCount, 0);
  const flags = (c.flags || []).map(f => `<div class="ui-notice" data-tone="${f.tone === 'danger' ? 'fail' : 'wait'}"><strong>${E(f.text)}</strong> ${E({ cost: '최근 1시간 비용이 기준($25)을 넘었습니다.', burst: '최근 10분 요청 수가 기준(150)을 넘었습니다.', errors: '최근 30분에 오류·중단이 3건 이상입니다.', stuck: '도구 호출 결과를 10분 넘게 기다리고 있습니다.', long: '마지막 지시 뒤 3시간 넘게 혼자 돌고 있습니다.', context: '컨텍스트 창이 거의 찼습니다. 곧 자동 압축되거나 품질이 떨어집니다.' }[f.id] || '')}</div>`).join('');
  const accounts = Object.entries(c.accounts || {}).sort((a, b) => b[1] - a[1]);
  const facts = [
    ['사용자', c.owner ? `${c.owner.name}${c.owner.email ? ' · ' + c.owner.email : ''} (키 ${c.owner.tenant})` : '—'],
    ['출처', `${c.client} · ${placeLong(c)}${c.version ? ' · Claude Code ' + c.version : ''}${m.osVersion ? ' · ' + m.osVersion : ''}`],
    ['작업 폴더', c.cwd || '—'],
    ['브랜치', c.branch || '—'],
    ['모델', `${c.model || '—'}${c.effort ? ' · effort ' + c.effort : ''}${m.thinking ? ' · thinking ' + m.thinking : ''}`],
    ['구독 계정', accounts.length ? accounts.slice(0, 4).map(([a, n]) => `${X.short(a)} ${n}`).join(' · ') + (accounts.length > 4 ? ` 외 ${accounts.length - 4}` : '') : '—'],
    m.mcp && m.mcp.length ? ['MCP', m.mcp.join(', ')] : null,
    m.claudeMd && m.claudeMd.length ? ['지침 파일', m.claudeMd.map(p => relPath(p, c.cwd)).join(' · ')] : null,
    ['기록 출처', d.chat ? (d.chat.source === 'transcript' ? 'Claude Code 기록 파일 (이 호스트)' : d.chat.source === 'raw-io' ? `llmux 원문 (가장 최근 요청 #${d.chat.rawRef ? d.chat.rawRef.id : ''}${d.chat.truncated ? ', 앞부분 잘림' : ''})` : '없음') : '—'],
    ['세션 ID', c.sid],
  ].filter(Boolean);
  const instr = ov.instructions.slice().reverse().map(i => {
    const top = Object.entries(i.tools).sort((a, b) => b[1] - a[1]).slice(0, 4).map(([n, k]) => `${n} ${k}`).join(' · ');
    return `<button type="button" class="ins${i.open && c.status === 'running' ? ' live' : ''}" data-g="${i.i}">
      <span class="ins-n">#${i.i}</span>
      <span class="ins-b"><span class="ins-t">${E(i.text || '(지시 이전 작업)')}</span>
        <span class="ins-m">${i.at ? X.when(i.at) : ''}${i.durationMs != null ? ' · ' + dur(i.durationMs) : ''} · 도구 ${i.toolCount}${top ? ` (${E(top)})` : ''}${i.edits ? ' · 파일 수정 ' + i.edits : ''}${i.agents ? ' · 에이전트 ' + i.agents : ''} · 요청 ${i.requests} · ${usd(i.cost)}${i.errors ? ` · <b class="bad">오류 ${i.errors}</b>` : ''}</span>
        ${i.final ? `<span class="ins-f">→ ${E(i.final)}</span>` : ''}</span></button>`;
  }).join('');
  const files = ov.files.slice(0, 40).map(f => `<div class="file"><span class="fp" title="${E(f.path)}">${E(relPath(f.path, c.cwd))}</span><span class="fn">${f.edits}회${f.added || f.removed ? ` <i class="add">+${f.added}</i> <i class="del">−${f.removed}</i>` : ''}</span></div>`).join('');
  const links = [...new Set([...(c.links || []), ...(ov.links || [])])];
  $('t-ov').innerHTML = `${flags}
    <ui-stat-strip id="ov-stats" lang="ko"></ui-stat-strip>
    <div class="ov-grid">
      <div class="ov-l">
        <h4 class="ui-section-head">지시 <span class="ui-count">${ov.instructions.length}</span></h4>
        <div class="inslist">${instr || '<div class="ui-empty">기록된 지시가 없습니다</div>'}</div>
        ${files ? `<h4 class="ui-section-head">바꾼 파일 <span class="ui-count">${ov.files.length}</span></h4><div class="files">${files}</div>` : ''}
      </div>
      <div class="ov-r">
        ${c.context ? '<ui-context-window id="ov-ctx" lang="ko"></ui-context-window>' : ''}
        ${links.length ? `<h4 class="ui-section-head">링크</h4><div class="lks">${links.map(u => `<a class="ui-chip is-sm" href="${E(u)}" target="_blank" rel="noopener">${E(refOf(u))}</a>`).join('')}</div>` : ''}
        <h4 class="ui-section-head">정보</h4>
        <dl class="ui-dl">${facts.map(([k, v]) => `<div><dt>${E(k)}</dt><dd>${E(v)}</dd></div>`).join('')}</dl>
      </div>
    </div>`;
  $('ov-stats').data = {
    layout: 'cards',
    metrics: [
      { key: 'cost', label: '추정 비용', value: c.costEst, format: 'usd', hint: 'llmux 단가로 계산' },
      { key: 'req', label: '요청', value: c.requests, format: 'int', hint: `메인 ${c.mainRequests} · 서브에이전트 ${c.subagentRequests} · 부속 ${c.sideRequests}` },
      { key: 'tok', label: '토큰', value: (c.tokens_in || 0) + (c.tokens_out || 0), format: 'tokens', hint: `출력 ${X.n(c.tokens_out)}` },
      { key: 'ins', label: '지시', value: ov.instructions.filter(i => i.text).length, format: 'int' },
      { key: 'tools', label: '도구 호출', value: tools, format: 'int' },
      { key: 'span', label: '기간', value: c.first_ms && c.last_ms ? c.last_ms - c.first_ms : null, format: 'duration' },
      { key: 'err', label: '오류', value: c.errors, format: 'int' },
    ],
  };
  if (c.context) $('ov-ctx').data = { model: modelShort(c.context.model || c.model), window: c.context.window, parts: c.context.parts };
}
$('t-ov').addEventListener('click', (e) => {
  const b = e.target.closest('[data-g]'); if (!b) return;
  st.tab = 'log'; st.openGroups.add('g' + b.dataset.g); paintDetail(false);
  requestAnimationFrame(() => { const g = $('p-log').shadowRoot && $('p-log').shadowRoot.querySelector(`[data-group="g${b.dataset.g}"], [data-key="g${b.dataset.g}"]`); g && g.scrollIntoView({ block: 'start' }); });
});

/* work log: one group per instruction, tool calls as ui-tool-call-card slots */
// Grouped by the server's instruction number (entry.gi, see lib/overview.mjs) so 개요 and 작업 로그 agree.
function groupsOf(entries) {
  const groups = new Map();
  for (const e of entries || []) {
    if (e.kind === 'meta' || e.gi == null) continue;
    let g = groups.get(e.gi);
    if (!g) { g = { id: 'g' + e.gi, i: e.gi, text: null, at: e.ts, entries: [] }; groups.set(e.gi, g); }
    if (e.kind === 'user') { if (e.clean) { g.text = e.clean; g.at = e.ts || g.at; } continue; }
    g.entries.push(e);
  }
  return [...groups.values()].sort((a, b) => a.i - b.i);
}
const TOOL_ICON = { Bash: 'terminal', Read: 'file', Write: 'edit', Edit: 'edit', MultiEdit: 'edit', NotebookEdit: 'edit', Grep: 'search', Glob: 'search', WebFetch: 'globe', WebSearch: 'globe', Agent: 'agent', Task: 'agent', TodoWrite: 'list' };
// One instruction can hold thousands of steps (a long Codex run had 11,000, with 4,600 tool calls): drawing them all,
// each with a tool card, froze the page. The log draws the newest WL_ENTRIES steps of a group and full tool cards for
// the newest WL_CARDS tools of an open group; the Claude Code screen still has every step.
const WL_ENTRIES = 400, WL_CARDS = 40;
function paintWorkLog(d, ov) {
  const entries = (d.chat && d.chat.entries) || [];
  const groups = groupsOf(entries);
  const byI = new Map(ov.instructions.map(i => [i.i, i]));
  const last = groups[groups.length - 1];
  const cards = [];
  const wl = groups.map(g => {
    const info = byI.get(g.i);
    const open = st.openGroups.has(g.id) || g === last;
    const skipped = Math.max(0, g.entries.length - WL_ENTRIES);
    const shown = skipped ? g.entries.slice(skipped) : g.entries;
    let budget = WL_CARDS;
    const carded = new Set();
    for (let k = shown.length - 1; k >= 0 && budget > 0 && open; k--) if (shown[k].kind === 'tool' && shown[k].name !== 'Agent' && shown[k].name !== 'Task') { carded.add(k); budget--; }
    const items = skipped ? [{ id: g.id + '-skip', kind: 'notice', at: shown[0].ts, text: `앞선 ${skipped.toLocaleString()}개 단계는 줄였습니다 · 전체는 Claude Code 화면에서 볼 수 있습니다` }] : [];
    shown.forEach((e, k) => {
      const id = `${g.id}-${k + skipped}`;
      if (e.kind === 'tool') {
        const status = e.result ? (e.is_error ? 'error' : 'success') : 'running';
        if (e.name === 'Agent' || e.name === 'Task') {
          items.push({ id, kind: 'agent', at: e.ts, agent: { id, name: e.summary || 'agent', status: e.result ? (e.is_error ? 'failed' : 'done') : 'running', startedAt: e.ts, duration: e.done_ts && e.ts ? e.done_ts - e.ts : undefined, activity: e.prompt ? e.prompt.text.slice(0, 160) : undefined } });
        } else {
          items.push({ id, kind: 'tool', at: e.ts, tool: { name: e.name, status, summary: e.summary } });
          if (carded.has(k)) cards.push({ id, data: { id, name: e.name, summary: e.summary, icon: TOOL_ICON[e.name] || (e.name.startsWith('mcp__') ? 'plug' : 'wrench'), status, args: e.edit ? { file: e.summary, lines: e.edit } : e.summary, result: e.result ? e.result.text + (e.result.more ? `\n… (+${e.result.more}자)` : '') : undefined, at: e.ts, duration: e.done_ts && e.ts ? e.done_ts - e.ts : undefined, maxCodeHeight: 320, openOnError: true, note: e.req ? `${X.short(e.req.account)} · ${X.ms(e.req.duration_ms)} · ${usd(e.req.cost_usd)}` : undefined } });
        }
      } else if (e.kind === 'agentmsg') {
        items.push({ id, kind: 'notice', at: e.ts, text: X.clean(e.text).slice(0, 600) });
      } else if (e.kind === 'assistant') {
        items.push({ id, kind: 'narration', at: e.ts, text: e.text });
      } else if (e.kind === 'thinking') {
        items.push({ id, kind: 'narration', at: e.ts, label: '생각', text: e.text ? e.text + (e.more ? ' …' : '') : `(${X.n(e.chars)}자)` });
      }
    });
    const lastNarr = [...items].reverse().find(x => x.kind === 'narration' && !x.label);
    if (lastNarr && (g !== last || (st.detail.card && st.detail.card.status !== 'running'))) lastNarr.kind = 'final';
    return {
      id: g.id, ref: g.i ? '#' + g.i : undefined, title: g.text ? g.text.slice(0, 160) : '지시 이전 작업', at: g.at, count: g.entries.length, open,
      meta: info ? [info.durationMs != null ? { label: '소요', value: dur(info.durationMs) } : null, { label: '도구', value: String(info.toolCount) }, { label: '요청', value: String(info.requests) }, { label: '비용', value: usd(info.cost) }].filter(Boolean) : undefined,
      entries: g.text ? [{ id: g.id + '-i', kind: 'instruction', at: g.at, text: g.text }, ...items] : items,
    };
  });
  const log = $('p-log');
  log.data = { groups: wl, live: st.detail.card && st.detail.card.status === 'running', emptyText: '기록된 작업이 없습니다', startText: d.chat && d.chat.source === 'raw-io' ? 'llmux 원문에서 다시 만든 기록입니다' : undefined };
  // tool cards for open groups only (hundreds of closed cards would cost more than they show)
  const want = new Set(cards.map(c => c.id)), have = new Map();
  for (const el of [...log.querySelectorAll('ui-tool-call-card')]) { if (want.has(el.dataset.id)) have.set(el.dataset.id, el); else el.remove(); }
  for (const c of cards) {
    let el = have.get(c.id);
    if (!el) { el = document.createElement('ui-tool-call-card'); el.setAttribute('lang', 'ko'); el.slot = 'tool-' + c.id; el.dataset.id = c.id; log.appendChild(el); }
    el.data = c.data;
  }
}
$('p-log').addEventListener('ui-toggle', (e) => {
  const id = e.detail && (e.detail.groupId || e.detail.id);
  if (!id || !/^g\d+$/.test(id)) return;
  if (e.detail.open || e.detail.expanded) st.openGroups.add(id); else st.openGroups.delete(id);
  if (st.detail) paintWorkLog(st.detail, st.detail.overview || { instructions: [] });
});

function paintConversation(d, c) {
  const entries = (d.chat && d.chat.entries) || [];
  const turns = [];
  let toolsSince = {};
  const flushFacts = () => { const f = Object.entries(toolsSince); toolsSince = {}; return f.length ? [['도구', f.map(([n, k]) => `${n}${k > 1 ? ' ×' + k : ''}`).join(', ')]] : undefined; };
  entries.forEach((e, k) => {
    if (e.kind === 'user') turns.push({ id: 'u' + k, role: 'user', name: c.owner ? c.owner.short : '사용자', at: e.ts, text: X.clean(e.text) });
    else if (e.kind === 'tool') toolsSince[e.name] = (toolsSince[e.name] || 0) + 1;
    else if (e.kind === 'assistant') {
      const prev = turns[turns.length - 1];
      const facts = flushFacts();
      if (prev && prev.role === 'assistant' && !facts) { prev.markdown += '\n\n' + e.text; return; }
      turns.push({ id: 'a' + k, role: 'assistant', name: modelShort(e.model || c.model) || 'Claude', at: e.ts, markdown: e.text, facts });
    }
  });
  const tail = flushFacts();
  if (tail && c.status === 'running') turns.push({ id: 'p', role: 'pending', name: modelShort(c.model) || 'Claude', progress: { startedAt: c.last_ms || Date.now(), verb: '작업 중' }, facts: tail });
  $('p-conv').data = { turns: turns.slice(-200), hasMore: turns.length > 200, assistantName: 'Claude', emptyText: '대화 기록이 없습니다', live: c.status === 'running' };
}

/* 요청: newest first. The first page loads with the tab; the next older page loads by itself when the end of the
   list scrolls into view (GET /api/session/:sid/requests?before=<cursor>); a running session's new requests are
   put on top (?after=<cursor>). Cursors are "at_ms:id", so rows arriving on top never repeat or skip a row. */
function newReq(limit) { return { limit: limit || 50, kind: '', total: 0, dropped: 0, rows: [], more: false, busy: '', error: '', gen: 0 }; }
const cursorOf = (r) => `${r.at_ms}:${r.id}`;
const reqSeen = (rows) => new Set(rows.map(cursorOf));
async function fetchReq(sid, q, extra) {
  const r = await fetch(`/api/session/${sid}/requests?limit=${q.limit}${q.kind ? '&kind=' + encodeURIComponent(q.kind) : ''}${extra}`, { cache: 'no-store' });
  if (!r.ok) throw new Error('HTTP ' + r.status);
  return r.json();
}
// One request at a time per list; a filter change (new gen) or another session drops a late answer.
async function reqJob(kind, extra, apply) {
  const sid = st.sel, q = st.req; if (!sid || (q.busy && kind !== 'first')) return;
  const gen = kind === 'first' ? ++q.gen : q.gen;
  q.busy = kind; q.error = ''; if (kind !== 'top') paintReqFoot();
  let page = null, err = '';
  try { page = await fetchReq(sid, q, extra); } catch (e) { err = String(e.message || e); }
  if (st.sel !== sid || st.req !== q || q.gen !== gen) return;
  q.busy = '';
  if (err) { if (kind !== 'top') q.error = err; paintReqFoot(); return; }
  apply(page);
  paintReqHead(); paintReqFoot();
  requestAnimationFrame(fillView);
}
function loadRequests() {
  const q = st.req; Object.assign(q, { rows: [], total: 0, more: false }); $('p-runs').replaceChildren(); paintReqHead();
  watchReqEnd();
  return reqJob('first', '', (p) => { Object.assign(q, { rows: p.rows, total: p.total, more: p.more, dropped: p.dropped || 0 }); addRuns(p.rows, 'append'); });
}
function loadMoreRequests() {
  const q = st.req; if (q.busy || q.error || !q.more || !q.rows.length) return;
  return reqJob('more', '&before=' + cursorOf(q.rows[q.rows.length - 1]), (p) => {
    const seen = reqSeen(q.rows), add = p.rows.filter(r => !seen.has(cursorOf(r)));
    q.rows = q.rows.concat(add); addRuns(add, 'append');
    Object.assign(q, { total: p.total, more: p.more, dropped: p.dropped || 0 });
  }).then(() => { if (st.req === q && !q.more && q.total > q.rows.length) refreshRequests(); }); // at the end, newer requests that came in on top
}
function refreshRequests() {
  const q = st.req; if (q.busy || !q.rows.length) return;
  return reqJob('top', '&after=' + cursorOf(q.rows[0]), (p) => {
    if (p.gap) { loadRequests(); return; }
    const seen = reqSeen(q.rows), add = p.rows.filter(r => !seen.has(cursorOf(r)));
    q.total = p.total; if (!add.length) return;
    // Keep what the reader is looking at in place while rows are added above it.
    const b = reqScroller(), h0 = b ? b.scrollHeight : 0;
    q.rows = add.concat(q.rows); addRuns(add, 'prepend');
    if (b && b.scrollTop > 0) b.scrollTop += b.scrollHeight - h0;
  });
}
// The drawer body is the scroll box (open shadow root of ui-drawer).
const reqScroller = () => (panel.shadowRoot && panel.shadowRoot.querySelector('.dr-body')) || null;
const END_MARGIN = 800;
let reqIO = null;
function watchReqEnd() {
  if (reqIO || !('IntersectionObserver' in window)) return;
  reqIO = new IntersectionObserver((es) => { if (st.tab === 'req' && es.some(e => e.isIntersecting)) loadMoreRequests(); }, { root: reqScroller(), rootMargin: `0px 0px ${END_MARGIN}px 0px` });
  reqIO.observe($('p-more'));
}
// The observer only reports changes: when a page leaves the end still in view (short rows, tall drawer), go on.
function fillView() {
  const b = reqScroller(), end = $('p-more');
  if (st.tab !== 'req' || !b || !end.offsetParent) return;
  if (end.getBoundingClientRect().top < b.getBoundingClientRect().bottom + END_MARGIN) loadMoreRequests();
}
function paintReqFoot() {
  const q = st.req, left = Math.max(0, q.total - q.rows.length);
  $('p-more').innerHTML = q.error ? `<span class="hint">불러오지 못했습니다 (${E(q.error)})</span><button type="button" class="ui-btn" data-rmore>다시 시도</button>`
    : q.busy === 'first' || q.busy === 'more' ? '<span class="hint"><span class="ui-spinner is-sm" aria-hidden="true"></span> 불러오는 중…</span>'
    : q.more ? `<button type="button" class="ui-btn" data-rmore>${Math.min(q.limit, left).toLocaleString()}건 더 보기</button><span class="hint">남은 요청 ${left.toLocaleString()}건 · 스크롤하면 이어서 불러옵니다</span>`
    : q.total && q.dropped ? `<span class="hint">보관된 최근 요청까지 모두 불러왔습니다 · ${q.rows.length.toLocaleString()}건. ${q.kind ? '이 화면은 세션당 최근 4,000건까지만 보관합니다' : `그보다 오래된 ${q.dropped.toLocaleString()}건은 보관하지 않습니다 (세션당 최근 4,000건까지)`}</span>`
    : q.total ? `<span class="hint">마지막 요청까지 모두 불러왔습니다 · ${q.rows.length.toLocaleString()}건</span>`
    : q.gen ? '<span class="hint">이 세션에 기록된 llmux 요청이 없습니다</span>' : '';
}
function paintReqHead() {
  const q = st.req;
  const kinds = (st.detail && st.detail.requestKinds) || {};
  $('p-pager').innerHTML = `<span class="hint" aria-live="polite">최신순 · ${q.rows.length.toLocaleString()} / ${q.total.toLocaleString()}건 불러옴</span>
    <span class="rv-acts"><select class="ui-select-chip" data-rk aria-label="종류"><option value="">모든 종류 (${Object.values(kinds).reduce((a, b) => a + b, 0)})</option>${Object.entries(kinds).sort((a, b) => b[1] - a[1]).map(([k, n]) => `<option value="${E(k)}" ${q.kind === k ? 'selected' : ''}>${E(KIND_LABEL[k] || k)} (${n})</option>`).join('')}</select>
    <select class="ui-select-chip" data-rl aria-label="한 번에 불러올 개수">${[25, 50, 100, 200].map(n => `<option value="${n}" ${q.limit === n ? 'selected' : ''}>${n}건씩</option>`).join('')}</select></span>`;
}
const runOf = (r) => ({
  id: `${r.id}:${r.at_ms}`,
  title: `${KIND_LABEL[r.msg_kind] || r.msg_kind || '요청'} · ${modelShort(r.model)}${r.effort ? ' · ' + r.effort : ''} · #${r.id}`,
  status: r.status >= 400 || r.aborted ? 'failed' : 'done',
  startedAt: r.at_ms - (r.duration_ms || 0), endedAt: r.at_ms, durationMs: r.duration_ms,
  tokens: r.tokens ? { input: r.tokens.input, output: r.tokens.output, cacheRead: r.tokens.cache_read, cacheWrite: r.tokens.cache_creation } : undefined,
  costUsd: r.cost, approx: r.cost_usd == null,
  canOpen: true,
  notes: [{ label: '계정', value: X.short(r.account) }, { label: '첫 바이트', value: X.ms(r.ttfb_ms) }, r.status >= 400 ? { label: '상태', value: String(r.status) } : null].filter(Boolean),
});
// Each loaded page is its own <ui-task-runs>: adding a page draws only that page, never the whole list again
// (one list of 2,000 rows takes ~270 ms to redraw, which stalls the scroll on every page).
function addRuns(rows, where) {
  if (!rows.length) return;
  const el = document.createElement('ui-task-runs');
  el.setAttribute('lang', 'ko'); el.setAttribute('flush', '');
  el.data = { title: '', runs: rows.map(runOf), totals: false };
  $('p-runs')[where](el);
}
$('p-pager').addEventListener('change', (e) => {
  if (e.target.matches('[data-rk]')) { st.req.kind = e.target.value; loadRequests(); }
  if (e.target.matches('[data-rl]')) { st.req.limit = Number(e.target.value); loadRequests(); }
});
$('p-more').addEventListener('click', (e) => {
  if (!e.target.closest('[data-rmore]')) return;
  st.req.error = ''; if (st.req.rows.length) loadMoreRequests(); else loadRequests();
});
$('p-runs').addEventListener('ui-open', (e) => {
  const [id, at] = String(e.detail.id).split(':').map(Number);
  const row = st.req.rows.find(r => r.id === id && r.at_ms === at) || { id, at_ms: at };
  X.openRaw(row);
});

async function paintAgents(d, c, subs) {
  $('p-tree').data = {
    title: '서브에이전트',
    nodes: [{ id: 'main', name: c.title || '메인', status: c.status === 'running' ? 'running' : 'done', model: modelShort(c.model) }]
      .concat(subs.map(a => ({ id: a.id, parentId: 'main', name: a.type || 'agent', role: a.description || undefined, status: 'done', endedAt: a.mtime, summary: a.description || undefined }))),
    density: 'rich',
    selectedId: st.agent || null,
  };
}
$('p-tree').addEventListener('ui-select', async (e) => {
  const id = e.detail.id; if (!id || id === 'main' || !st.sel) return;
  st.agent = id;
  $('p-agent').innerHTML = '<div class="x-empty">불러오는 중…</div>';
  const r = await X.api.agent(st.sel, id).catch(() => null);
  $('p-agent').innerHTML = r ? `<div class="x-dim" style="padding:0 0 8px">${E(r.type || '')} · ${E(r.description || '')}</div>` + X.renderChat(r.entries || []) : '<div class="x-empty">기록을 찾을 수 없습니다</div>';
});

/* ---------------- users ---------------- */
async function loadUsers() {
  const r = await fetch('/api/users?window=' + st.window, { cache: 'no-store' });
  if (!r.ok) return;
  st.users = await r.json();
  paintUsers();
  if (st.selUser) paintUserDetail();
}
function paintUsers() {
  const d = st.users; if (!d) return;
  const used = d.users.filter(u => u.usage && u.usage.requests);
  const idle = d.users.filter(u => !(u.usage && u.usage.requests));
  const tot = used.reduce((a, u) => ({ cost: a.cost + (u.usage.cost_usd || 0), req: a.req + (u.usage.requests || 0), err: a.err + (u.usage.errors || 0), tok: a.tok + (u.usage.tokens_in || 0) + (u.usage.tokens_out || 0) }), { cost: 0, req: 0, err: 0, tok: 0 });
  const running = d.users.reduce((a, u) => a + u.live.running, 0);
  $('u-count').textContent = `${used.length}명 사용 · 키 ${d.users.filter(u => !u.system).length}개`;
  $('u-src').textContent = d.error ? `사용량을 불러오지 못했습니다: ${d.error}` : '사용량: llmux /llmux/keys/usage (정확값) · 지금 상태: 세션 기록';
  $('u-stats').data = {
    period: { options: [{ value: '1h', label: '1시간' }, { value: '24h', label: '24시간' }, { value: '7d', label: '7일' }, { value: '30d', label: '30일' }], value: st.window, label: '기간' },
    layout: 'cards',
    metrics: [
      { key: 'cost', label: '비용', value: tot.cost, format: 'usd', breakdown: { title: '사용자별', rows: used.slice(0, 8).map(u => ({ label: u.short, value: u.usage.cost_usd })) } },
      { key: 'req', label: '요청', value: tot.req, format: 'compact' },
      { key: 'tok', label: '토큰', value: tot.tok, format: 'tokens' },
      { key: 'err', label: '오류', value: tot.err, format: 'int' },
      { key: 'users', label: '사용한 사람', value: used.filter(u => !u.system).length, format: 'int' },
      { key: 'run', label: '지금 실행 중 세션', value: running, format: 'int' },
    ],
  };
  $('u-break').data = { title: '비용 비중', unit: 'usd', parts: used.map(u => ({ key: u.tenant, label: u.short, value: u.usage.cost_usd || 0 })), maxRows: 8, totalLabel: '합계' };
  const hours = (used[0] && used[0].hourly) || [];
  $('u-chart').data = {
    title: '최근 24시간 시간대별 비용 (추정)',
    format: 'usd',
    series: used.slice(0, 6).map((u, i) => ({ key: u.tenant, label: u.short, color: ['blue', 'orange', 'green', 'purple', 'red', 'teal'][i] })),
    bars: hours.map((h, k) => ({ key: String(h.h), label: new Date(h.at).getHours() + '시', values: Object.fromEntries(used.slice(0, 6).map(u => [u.tenant, u.hourly[k] ? +u.hourly[k].cost.toFixed(2) : 0])) })),
    height: 180,
  };
  const max = Math.max(1, ...used.map(u => u.usage.cost_usd || 0));
  const row = (u) => {
    const us = u.usage || {};
    const spark = u.hourly.map(h => h.cost);
    const sm = Math.max(0.01, ...spark);
    const origins = Object.entries(u.origins || {}).map(([k, v]) => `${k} ${v}`).join(' · ');
    return `<button type="button" class="urow${st.selUser === u.tenant ? ' on' : ''}" data-tenant="${E(u.tenant)}">
      <span class="ui-avatar av" data-hue="${u.hue == null ? 7 : u.hue}">${E((u.short || '?').slice(0, 1).toUpperCase())}</span>
      <span class="u-id"><strong>${E(u.short)}</strong><small>${E(u.email || (u.system ? '이 머신에서 키 없이 보낸 요청 (콘솔·soma-work 자동 작업)' : u.tenant))}${u.suspended ? ' · <b class="bad">정지됨</b>' : ''}</small></span>
      <span class="u-live">${u.live.running ? `<span class="ui-status is-pill" data-tone="run">실행 ${u.live.running}</span>` : ''}${u.live.waiting + u.live.stalled ? `<span class="ui-status is-pill" data-tone="wait">대기 ${u.live.waiting + u.live.stalled}</span>` : ''}${u.live.flagged ? `<span class="ui-status is-pill" data-tone="fail">주의 ${u.live.flagged}</span>` : ''}<small>${E(origins || '오늘 세션 없음')}</small></span>
      <span class="u-num"><b>${usd(us.cost_usd)}</b><i class="bar"><i style="width:${Math.round(100 * (us.cost_usd || 0) / max)}%"></i></i></span>
      <span class="u-num"><b>${X.n(us.requests)}</b><small>요청${us.errors ? ` · <span class="bad">오류 ${us.errors}</span>` : ''}</small></span>
      <svg class="spark" viewBox="0 0 96 24" aria-label="24시간 비용 추이"><path d="${spark.map((v, i) => `${i ? 'L' : 'M'}${(i * 96 / 23).toFixed(1)},${(23 - 21 * v / sm).toFixed(1)}`).join(' ')}"/></svg>
      <span class="u-last"><small>${ago(u.last_ms)}</small></span>
    </button>`;
  };
  $('u-list').innerHTML = `<div class="u-head"><span></span><span>사용자</span><span>지금</span><span>비용 (${E(st.window)})</span><span>요청</span><span>24시간</span><span>마지막</span></div>`
    + used.map(row).join('')
    + (idle.length ? `<details class="u-idle"><summary>이 기간에 사용 없음 ${idle.length}명</summary>${idle.map(row).join('')}</details>` : '');
}
$('u-stats').addEventListener('ui-period', (e) => { st.window = e.detail.value; loadUsers(); });
$('u-list').addEventListener('click', (e) => { const b = e.target.closest('[data-tenant]'); if (b) openUser(b.dataset.tenant); });

function openUser(tenant) {
  st.selUser = tenant; st.sel = null; st.detail = null;
  showPanel('user');
  paintUserDetail();
  paintUsers();
  requestAnimationFrame(dockPad);
}
function paintUserDetail() {
  const u = st.users && st.users.users.find(x => x.tenant === st.selUser); if (!u) return;
  const us = u.usage || {};
  const mine = (st.data ? st.data.sessions : []).filter(c => c.owner && c.owner.tenant === u.tenant);
  const active = mine.filter(c => c.status === 'running' || c.status === 'waiting' || c.status === 'stalled');
  const recent = mine.filter(c => !active.includes(c)).slice(0, 12);
  $('ph').innerHTML = `<div class="ph-t"><span class="ui-avatar av" data-hue="${u.hue == null ? 7 : u.hue}">${E((u.short || '?').slice(0, 1).toUpperCase())}</span><span class="ph-title">${E(u.name)}</span></div>
    <div class="ph-m"><span class="ui-chip is-sm">키 ${E(u.tenant)}</span>${u.email ? `<span class="ui-chip is-sm">${E(u.email)}</span>` : ''}${u.kind ? `<span class="ui-chip is-sm">${E(u.kind)}</span>` : ''}${u.created_at_ms ? `<span class="ui-chip is-sm">발급 ${E(new Date(u.created_at_ms).toLocaleDateString('ko-KR'))}</span>` : ''}</div>`;
  const srow = (c) => `<button type="button" class="srow" data-open="${c.sid}"><span class="ui-status is-pill" data-tone="${c.status === 'running' ? 'run' : c.status === 'waiting' || c.status === 'stalled' ? 'wait' : 'done'}">${{ running: '실행 중', waiting: '입력 대기', stalled: '도구 대기', idle: '쉬는 중', past: '지난' }[c.status]}</span><span class="s-t"><strong>${E(c.title)}</strong><small>${E(placeText(c))}${c.project ? ' · ' + E(c.project) : ''} · ${ago(c.last_ms)} · ${usd(c.costEst)}${(c.flags || []).length ? ` · <b class="warn">${E(c.flags.map(f => f.text).join(' · '))}</b>` : ''}</small></span></button>`;
  $('ubody').innerHTML = `<ui-stat-strip id="ud-stats" lang="ko"></ui-stat-strip>
    <ui-bar-chart id="ud-chart" lang="ko"></ui-bar-chart>
    <h4 class="ui-section-head">지금 <span class="ui-count">${active.length}</span></h4><div class="slist">${active.map(srow).join('') || '<div class="ui-empty">진행 중인 세션이 없습니다</div>'}</div>
    <h4 class="ui-section-head">최근 세션</h4><div class="slist">${recent.map(srow).join('') || '<div class="ui-empty">최근 세션이 없습니다</div>'}</div>
    <div class="ov-grid2">
      <div><h4 class="ui-section-head">모델 (${E(st.window)})</h4><dl class="ui-dl">${(us.models || []).sort((a, b) => b.cost_usd - a.cost_usd).map(m => `<div><dt>${E(modelShort(m.model))}</dt><dd>${usd(m.cost_usd)} · ${X.n(m.requests)} 요청</dd></div>`).join('') || '<div class="ui-empty">—</div>'}</dl></div>
      <div><h4 class="ui-section-head">쓴 구독 계정 (24시간)</h4><dl class="ui-dl">${u.accounts.map(a => `<div><dt>${E(X.short(a.name))}</dt><dd>${X.n(a.n)} 요청</dd></div>`).join('') || '<div class="ui-empty">—</div>'}</dl></div>
    </div>
    <p class="hint"><a href="/?user=${encodeURIComponent(u.tenant)}" data-board="${E(u.tenant)}">세션 보드에서 ${E(u.short)}만 보기 →</a></p>`;
  $('ud-stats').data = { layout: 'cards', metrics: [
    { key: 'cost', label: `비용 (${st.window})`, value: us.cost_usd, format: 'usd' },
    { key: 'req', label: '요청', value: us.requests, format: 'compact' },
    { key: 'err', label: '오류', value: us.errors, format: 'int' },
    { key: 'tok', label: '토큰', value: (us.tokens_in || 0) + (us.tokens_out || 0), format: 'tokens' },
    { key: 'today', label: '오늘 세션', value: u.live.today, format: 'int', hint: Object.entries(u.origins || {}).map(([k, v]) => `${k} ${v}`).join(' · ') },
  ] };
  $('ud-chart').data = { title: '최근 24시간 시간대별 비용 (추정)', format: 'usd', bars: u.hourly.map(h => ({ key: String(h.h), label: new Date(h.at).getHours() + '시', value: +h.cost.toFixed(2), title: `${h.req} 요청${h.err ? ' · 오류 ' + h.err : ''}` })), height: 150 };
}
$('ubody').addEventListener('click', (e) => {
  const s = e.target.closest('[data-open]'); if (s) { openSession(s.dataset.open); return; }
  const b = e.target.closest('[data-board]'); if (b) { e.preventDefault(); st.user = b.dataset.board; closePanel(); go('sessions'); }
});

/* ---------------- activity / accounts (core.js renderers) ---------------- */
function paintOther() {
  if (!st.doc) return;
  if (st.view === 'accounts') X.setHTML($('accounts'), X.renderAccounts(st.doc));
  if (st.view === 'activity') X.setHTML($('acts'), X.renderActivities(st.doc, null, st.actFilter || ''));
}
document.addEventListener('click', async (e) => {
  const a = e.target.closest('[data-act]'); if (a) { X.toast(`"${a.dataset.act}"는 TUI 또는 \`llmux\` 명령에서 하세요 (웹 쓰기 동작은 다음 단계)`); return; }
  if (st.view === 'activity') { const raw = e.target.closest('[data-raw]'); if (raw) { const [id, at] = raw.dataset.raw.split(':').map(Number); const all = st.doc ? (st.doc.activity.completed || []) : []; const row = all.find(r => r.id === id && Math.abs(r.at_ms - at) < 5) || { id, at_ms: at }; X.openRaw({ ...row, msg_kind: row.msg_kind || row.kind_detail, at_ms: at }); } }
});
document.addEventListener('input', (e) => { if (e.target.classList.contains('x-filter')) { st.actFilter = e.target.value; const pos = e.target.selectionStart; paintOther(); const f = document.querySelector('.x-filter'); if (f) { f.focus(); f.setSelectionRange(pos, pos); } } });

/* ---------------- polling ---------------- */
X.poll(loadSessions, 3000);
let lastDetailPoll = 0;
X.poll(async () => {
  if (!st.sel) return;
  const live = st.detail && st.detail.card && (st.detail.card.status === 'running' || st.detail.card.status === 'stalled');
  if (!live && Date.now() - lastDetailPoll < 30000) return;
  lastDetailPoll = Date.now();
  await loadDetail(false);
}, 5000);
X.poll(async () => { if (st.view === 'users') await loadUsers(); }, 15000);
X.poll(async () => {
  try { st.doc = await X.api.dashboard(); if (st.offline) { st.offline = null; $('offline').hidden = true; paintBar(); } paintOther(); }
  catch (e) { st.offline = String(e.message || e).slice(0, 120); $('offline').hidden = false; $('offline').textContent = 'llmux 데몬에 연결할 수 없습니다 — 세션 기록(파일·과거 요청)만 보여 줍니다.'; paintBar(); }
}, 4000);
fetch('/api/whoami').then(r => (r.ok ? r.json() : null)).then(w => { who = w; paintBar(); }).catch(() => {});
go(st.view);
const pre = new URLSearchParams(location.search).get('session'); if (pre) setTimeout(() => openSession(pre), 400);
