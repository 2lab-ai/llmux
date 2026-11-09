/* Raw request/response viewer — the web twin of the llmux TUI raw viewer (src/tui/ui.rs raw_content_from_record).
 * Legs in wire order: Request (client → llmux) · Upstream Req (llmux → api) · Upstream Resp (api → llmux) ·
 * Response (llmux → client); the upstream pair exists only on translated exchanges.
 * Each leg: general block, headers, body. Bodies open as a JSON tree with only the last message expanded
 * (request) or the assembled message expanded (SSE response); 원문 shows the bytes as captured.
 * Actions: 복사 (this leg's body) · curl 복사 · 전체 복사 · 기록 저장 (the whole record JSON). */
(function () {
  'use strict';
  const X = window.X;
  const E = X.esc;
  const PAGE = 100;            // children drawn per container before "더 보기"
  const STR_PREVIEW = 240;     // characters of a long string shown folded
  const RAW_CHUNK = 1 << 20;   // raw text drawn per chunk

  const st = { rec: null, row: null, legs: [], leg: 0, mode: 'tree', trees: {}, rawShown: {} };

  /* ---------- body parsing ---------- */
  function parseSSE(text) {
    const events = [];
    let ev = null;
    for (const line of String(text).split('\n')) {
      if (line.startsWith('event:')) { ev = { event: line.slice(6).trim() }; continue; }
      if (line.startsWith('data:')) {
        const raw = line.slice(5).trim();
        let data = raw; try { data = JSON.parse(raw); } catch {}
        events.push({ event: (ev && ev.event) || (data && data.type) || 'message', data });
        ev = null;
      }
    }
    return events;
  }
  // Rebuild the assistant message from Anthropic SSE events (what the client ended up with).
  function assemble(events) {
    let msg = null; const blocks = [];
    for (const { data: d } of events) {
      if (!d || typeof d !== 'object') continue;
      if (d.type === 'message_start') msg = { ...(d.message || {}) };
      else if (d.type === 'content_block_start') { const b = { ...d.content_block }; if (b.type === 'tool_use') { b._json = ''; } blocks[d.index] = b; }
      else if (d.type === 'content_block_delta') {
        const b = blocks[d.index]; const x = d.delta || {}; if (!b) continue;
        if (x.type === 'text_delta') b.text = (b.text || '') + x.text;
        else if (x.type === 'thinking_delta') b.thinking = (b.thinking || '') + x.thinking;
        else if (x.type === 'signature_delta') b.signature = (b.signature || '') + x.signature;
        else if (x.type === 'input_json_delta') b._json += x.partial_json;
      } else if (d.type === 'content_block_stop') { const b = blocks[d.index]; if (b && b._json != null) { try { b.input = JSON.parse(b._json || '{}'); } catch { b.input = b._json; } delete b._json; } }
      else if (d.type === 'message_delta') { msg = msg || {}; Object.assign(msg, d.delta || {}); msg.usage = { ...(msg.usage || {}), ...(d.usage || {}) }; }
    }
    if (!msg && !blocks.length) return null;
    return { ...(msg || {}), content: blocks.filter(Boolean) };
  }
  function bodyModel(text, isResponse) {
    if (!text) return { kind: 'empty' };
    const t = text.trimStart();
    if (t.startsWith('{') || t.startsWith('[')) {
      try { return { kind: 'json', value: JSON.parse(t) }; } catch {}
      return { kind: 'broken', note: '본문이 raw_io.max_body_bytes에서 잘렸거나 JSON이 아닙니다. 원문으로 봅니다.' };
    }
    if (isResponse && /(^|\n)(event|data):/.test(text)) {
      const events = parseSSE(text);
      const message = assemble(events);
      return { kind: 'sse', value: message ? { 'message (assembled)': message, events } : { events } };
    }
    return { kind: 'text' };
  }

  /* ---------- default expansion ---------- */
  const ptr = (base, k) => base + '/' + String(k).replace(/~/g, '~0').replace(/\//g, '~1');
  function openDeep(set, path, v, depth) {
    set.add(path);
    if (depth <= 0 || !v || typeof v !== 'object') return;
    for (const [k, c] of Array.isArray(v) ? v.entries() : Object.entries(v)) if (c && typeof c === 'object') openDeep(set, ptr(path, k), c, depth - 1);
  }
  function defaultOpen(model, legLabel) {
    const set = new Set(['']);
    const v = model.value;
    if (!v || typeof v !== 'object') return set;
    if (Array.isArray(v.messages) && v.messages.length) {
      set.add('/messages');
      openDeep(set, ptr('/messages', v.messages.length - 1), v.messages[v.messages.length - 1], 4);
    } else if (Array.isArray(v.input) && v.input.length) { // OpenAI Responses request
      set.add('/input');
      openDeep(set, ptr('/input', v.input.length - 1), v.input[v.input.length - 1], 4);
    } else if (v['message (assembled)']) {
      openDeep(set, ptr('', 'message (assembled)'), v['message (assembled)'], 3);
    } else if (/Resp|Response/.test(legLabel)) openDeep(set, '', v, 2);
    return set;
  }

  /* ---------- tree rendering (children only for open nodes) ---------- */
  function summary(v) {
    if (Array.isArray(v)) return `[${v.length}]`;
    const keys = Object.keys(v);
    const hint = v.role ? ` role: "${v.role}"` : v.type ? ` type: "${v.type}"` : '';
    return `{${keys.length}}${hint}`;
  }
  function prim(v, path, tree) {
    if (v === null) return '<span class="jt-null">null</span>';
    if (typeof v === 'number') return `<span class="jt-num">${v}</span>`;
    if (typeof v === 'boolean') return `<span class="jt-bool">${v}</span>`;
    const s = String(v);
    const long = s.length > STR_PREVIEW || s.includes('\n');
    if (!long) return `<span class="jt-str">"${E(s)}"</span>`;
    const open = tree.strOpen.has(path);
    return open
      ? `<button type="button" class="jt-sbtn" data-s="${E(path)}">접기 · ${s.length.toLocaleString()}자</button><pre class="jt-pre">${E(s)}</pre>`
      : `<span class="jt-str">"${E(s.slice(0, STR_PREVIEW).replace(/\n/g, '⏎'))}…"</span> <button type="button" class="jt-sbtn" data-s="${E(path)}">펼치기 · ${s.length.toLocaleString()}자</button>`;
  }
  function node(k, v, path, tree, depth) {
    const keyHtml = k == null ? '' : `<span class="jt-key">${typeof k === 'number' ? k : E(JSON.stringify(k))}</span>: `;
    if (!v || typeof v !== 'object') return `<div class="jt-row" style="--d:${depth}">${keyHtml}${prim(v, path, tree)}</div>`;
    const open = tree.open.has(path);
    let html = `<div class="jt-row jt-c" style="--d:${depth}" data-p="${E(path)}"><span class="jt-caret">${open ? '▾' : '▸'}</span>${keyHtml}<span class="jt-sum">${E(summary(v))}</span></div>`;
    if (!open) return html;
    const entries = Array.isArray(v) ? [...v.entries()] : Object.entries(v);
    const lim = tree.limit.get(path) || PAGE;
    // arrays: show the tail first when long (the newest messages matter most)
    let shown = entries;
    let skipped = 0;
    if (entries.length > lim) { shown = Array.isArray(v) ? entries.slice(entries.length - lim) : entries.slice(0, lim); skipped = entries.length - lim; }
    if (skipped && Array.isArray(v)) html += `<div class="jt-row" style="--d:${depth + 1}"><button type="button" class="jt-sbtn" data-more="${E(path)}">앞쪽 ${skipped}개 더 보기</button></div>`;
    for (const [ck, cv] of shown) html += node(Array.isArray(v) ? ck : ck, cv, ptr(path, ck), tree, depth + 1);
    if (skipped && !Array.isArray(v)) html += `<div class="jt-row" style="--d:${depth + 1}"><button type="button" class="jt-sbtn" data-more="${E(path)}">나머지 ${skipped}개 더 보기</button></div>`;
    return html;
  }

  /* ---------- legs ---------- */
  function hdrTable(h) {
    if (h == null) return '<div class="rv-dim">캡처되지 않음</div>';
    if (!h.length) return '<div class="rv-dim">(없음)</div>';
    return `<table class="rv-hdr">${h.map(([k, v]) => `<tr><td>${E(k)}</td><td>${E(v)}</td></tr>`).join('')}</table>`;
  }
  function curl(method, url, headers, body) {
    const sh = (t) => String(t).replace(/'/g, "'\\''");
    let out = `curl -X '${sh(method || 'POST')}' '${sh(url)}'`;
    for (const [n, v] of headers || []) { if (['content-length', 'host', 'accept-encoding', 'connection'].includes(String(n).toLowerCase())) continue; out += ` \\\n  -H '${sh(n)}: ${sh(v)}'`; }
    if (body) out += ` \\\n  --data-raw '${sh(body)}'`;
    return out;
  }
  function buildLegs(rec, row) {
    const base = `http://localhost:${rec.port || 3456}`;
    const method = (row && row.method) || 'POST', path = (row && row.path) || '/v1/messages';
    const clientCurl = curl(method, base + path, rec.request_headers, rec.request_body);
    const legs = [{ label: 'Request', sub: 'Claude Code → llmux', headers: rec.request_headers, body: rec.request_body || '', response: false, curl: clientCurl, general: true }];
    const up = rec.upstream;
    if (up) {
      const upCurl = curl(method, up.url || '<upstream url not captured>', up.request_headers, up.request_body || '');
      if (up.url || up.request_body != null || up.request_headers) legs.push({ label: 'Upstream Req', sub: 'llmux → api', url: up.url, headers: up.request_headers, body: up.request_body || '', response: false, curl: upCurl });
      if (up.response_body != null || up.response_headers) legs.push({ label: 'Upstream Resp', sub: 'api → llmux', headers: up.response_headers, body: up.response_body || '', response: true, curl: upCurl });
    }
    legs.push({ label: 'Response', sub: 'llmux → Claude Code', headers: rec.response_headers, body: rec.response_body || '', response: true, curl: clientCurl, status: rec.status });
    for (const l of legs) { l.model = bodyModel(l.body, l.response); }
    return legs;
  }
  function general(rec, row) {
    const f = (k, v) => `<div><dt>${E(k)}</dt><dd>${v}</dd></div>`;
    const r = row || {};
    return `<dl class="rv-gen">
      ${f('request', E(`${r.method || 'POST'} ${r.path || ''}`))}
      ${f('status', `<b class="${(rec.status || 0) >= 400 ? 'bad' : 'ok'}">${E(rec.status ?? '?')}</b> · ${E(X.ms(rec.duration_ms))} elapsed`)}
      ${f('time', E(new Date(rec.ts_ms).toLocaleString('ko-KR')))}
      ${f('id', '#' + E(rec.id))}
      ${f('model', E([rec.group, rec.model].filter(Boolean).join(' ') + (r.effort ? ' · effort ' + r.effort : '') + (r.fast ? ' · fast' : '')))}
      ${f('account', E(rec.account || '?'))}
      ${r.msg_kind ? f('kind', E(r.msg_kind)) : ''}
      ${r.tokens ? f('tokens', E(`in ${X.n(r.tokens.input)} · out ${X.n(r.tokens.output)} · cache read ${X.n(r.tokens.cache_read)} · cache write ${X.n(r.tokens.cache_creation)}`)) : ''}
    </dl>`;
  }

  function treeOf(i) {
    // the conversation arrays start with only their last few turns drawn (the last one expanded); 앞쪽 N개 더 보기 loads earlier ones
    if (!st.trees[i]) { const l = st.legs[i]; st.trees[i] = { open: defaultOpen(l.model, l.label), strOpen: new Set(), limit: new Map([['/messages', 6], ['/input', 6], ['/events', 20]]) }; }
    return st.trees[i];
  }
  function bodyHtml(i) {
    const l = st.legs[i];
    const m = l.model;
    const bytes = new Blob([l.body]).size;
    const truncated = /truncated \d+ bytes\]|…\[truncated/.test(l.body.slice(-200));
    const head = `<div class="rv-sec">body · ${bytes.toLocaleString()} bytes${truncated ? ' · <span class="bad">raw_io.max_body_bytes에서 잘림</span>' : ''}${m.kind === 'sse' ? ` · SSE ${m.value.events.length} events` : ''}</div>`;
    const mode = (m.kind === 'json' || m.kind === 'sse') ? st.mode : 'raw';
    if (m.kind === 'empty') return head + '<div class="rv-dim">(비어 있음)</div>';
    if (mode === 'tree') return head + `<div class="jt" data-leg="${i}">${node(null, m.value, '', treeOf(i), 0)}</div>`;
    const pretty = st.mode === 'pretty' && m.kind === 'json';
    const text = pretty ? JSON.stringify(m.value, null, 2) : l.body;
    const shown = st.rawShown[i] || RAW_CHUNK;
    return head + (m.note ? `<div class="rv-note">${E(m.note)}</div>` : '') + `<pre class="rv-raw">${E(text.slice(0, shown))}</pre>` + (text.length > shown ? `<button type="button" class="ui-btn" data-rawmore="${i}">나머지 ${(text.length - shown).toLocaleString()}자 더 보기</button>` : '');
  }

  function paint() {
    const host = document.getElementById('raw-body');
    if (!st.rec) return;
    const l = st.legs[st.leg];
    const canTree = l.model.kind === 'json' || l.model.kind === 'sse';
    host.innerHTML = `<div class="rv">
      <div class="rv-bar">
        <div class="rv-tabs" role="tablist">${st.legs.map((x, i) => `<button type="button" role="tab" aria-selected="${i === st.leg}" class="${i === st.leg ? 'on' : ''}" data-leg="${i}">${E(x.label)}<small>${E(x.sub)}</small></button>`).join('')}</div>
        <div class="rv-acts">
          <span class="rv-seg">${[['tree', '트리'], ['raw', '원문'], ['pretty', '원문(정렬)']].map(([k, t]) => `<button type="button" data-mode="${k}" class="${(canTree ? st.mode : 'raw') === k ? 'on' : ''}" ${!canTree && k !== 'raw' ? 'disabled' : ''}>${t}</button>`).join('')}</span>
          ${canTree && st.mode === 'tree' ? '<button type="button" class="ui-btn" data-act="collapse">모두 접기</button><button type="button" class="ui-btn" data-act="last">마지막 메시지만</button>' : ''}
          <button type="button" class="ui-btn" data-act="copy">복사</button>
          <button type="button" class="ui-btn" data-act="curl">curl 복사</button>
          <button type="button" class="ui-btn" data-act="all">전체 복사</button>
          <button type="button" class="ui-btn" data-act="save">기록 저장</button>
        </div>
      </div>
      ${l.general ? `<div class="rv-sec">general</div>${general(st.rec, st.row)}` : ''}
      ${l.url ? `<div class="rv-sec">url</div><div class="rv-url">${E(l.url)}</div>` : ''}
      ${l.status != null ? `<div class="rv-sec">status</div><div>${E(l.status)}</div>` : ''}
      <details class="rv-hd" ${st.hdrOpen ? 'open' : ''}><summary class="rv-sec">headers · ${l.headers ? l.headers.length : 0}</summary>${hdrTable(l.headers)}</details>
      ${bodyHtml(st.leg)}
    </div>`;
  }

  function copy(text, what) { navigator.clipboard && navigator.clipboard.writeText(text); X.toast(`${what} 복사했습니다 (${text.length.toLocaleString()}자)`); }

  document.addEventListener('click', (e) => {
    const host = document.getElementById('raw-body');
    if (!st.rec || !host || !host.contains(e.target)) return;
    const t = e.target;
    const leg = t.closest('[data-leg]:not(.jt)'); if (leg && leg.closest('.rv-tabs')) { st.leg = +leg.dataset.leg; paint(); return; }
    const mode = t.closest('[data-mode]'); if (mode && !mode.disabled) { st.mode = mode.dataset.mode; paint(); return; }
    const hd = t.closest('.rv-hd > summary'); if (hd) { st.hdrOpen = !hd.parentElement.open; return; }
    const more = t.closest('[data-more]'); if (more) { const tr = treeOf(st.leg); tr.limit.set(more.dataset.more, (tr.limit.get(more.dataset.more) || PAGE) + PAGE * 5); paint(); return; }
    const sb = t.closest('[data-s]'); if (sb) { const tr = treeOf(st.leg); tr.strOpen.has(sb.dataset.s) ? tr.strOpen.delete(sb.dataset.s) : tr.strOpen.add(sb.dataset.s); paint(); return; }
    const c = t.closest('.jt-c[data-p]'); if (c) { const tr = treeOf(st.leg); tr.open.has(c.dataset.p) ? tr.open.delete(c.dataset.p) : tr.open.add(c.dataset.p); const y = c.getBoundingClientRect().top; paint(); const again = host.querySelector(`.jt-c[data-p="${CSS.escape(c.dataset.p)}"]`); if (again) { const sc = host.closest('.dr-body') || host.parentElement; const dy = again.getBoundingClientRect().top - y; if (sc && dy) sc.scrollTop += dy; } return; }
    const rm = t.closest('[data-rawmore]'); if (rm) { st.rawShown[rm.dataset.rawmore] = (st.rawShown[rm.dataset.rawmore] || RAW_CHUNK) + RAW_CHUNK * 4; paint(); return; }
    const a = t.closest('[data-act]'); if (!a) return;
    const l = st.legs[st.leg];
    if (a.dataset.act === 'copy') copy(l.body, `${l.label} 본문`);
    else if (a.dataset.act === 'curl') copy(l.curl, 'curl 명령');
    else if (a.dataset.act === 'all') copy(st.legs.map(x => `── ${x.label} ──\n${x.body}\n\n`).join(''), '모든 구간');
    else if (a.dataset.act === 'save') {
      const blob = new Blob([JSON.stringify(st.rec, null, 2)], { type: 'application/json' });
      const u = URL.createObjectURL(blob); const link = document.createElement('a'); link.href = u; link.download = `llmux-raw-${st.rec.id}-${st.rec.ts_ms}.json`; link.click(); setTimeout(() => URL.revokeObjectURL(u), 2000);
    } else if (a.dataset.act === 'collapse') { st.trees[st.leg] = { open: new Set(['']), strOpen: new Set(), limit: new Map([['/messages', 6], ['/input', 6], ['/events', 20]]) }; paint(); }
    else if (a.dataset.act === 'last') { delete st.trees[st.leg]; paint(); }
  });

  /** Open the viewer for one llmux request row ({id, at_ms, method, path, msg_kind, effort, fast, tokens …}). */
  X.openRaw = async function (row) {
    const sheet = document.getElementById('raw-sheet');
    document.getElementById('raw-head').textContent = `요청 원문 #${row.id}`;
    document.getElementById('raw-body').innerHTML = '<div class="ui-skeleton is-block"></div>';
    if (sheet.show) sheet.show(); else sheet.setAttribute('open', '');
    st.rec = null; st.trees = {}; st.rawShown = {}; st.leg = 0; st.mode = 'tree'; st.row = row;
    const q = new URLSearchParams(location.search).has('anon') ? '&anon' : '';
    const r = await fetch(`/api/raw-full?id=${row.id}&at_ms=${row.at_ms}${q}`, { cache: 'no-store' });
    const rec = await r.json().catch(() => ({ error: '응답을 읽을 수 없습니다' }));
    if (!r.ok || rec.error) { document.getElementById('raw-body').innerHTML = `<div class="ui-empty">${E(rec.error || 'HTTP ' + r.status)}</div>`; return; }
    st.rec = rec; st.legs = buildLegs(rec, row);
    document.getElementById('raw-head').textContent = `요청 원문 #${rec.id} · ${rec.model || ''} · ${X.short(rec.account || '')}`;
    paint();
  };
})();
