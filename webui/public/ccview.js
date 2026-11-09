/* Claude Code screen: the session drawn the way the Claude Code terminal draws it.
 *   > prompt                       (the person)
 *   ⏺ text                         (the model; white dot)
 *   ⏺ Bash(cmd)                    (a tool call; green = ok, red = error, grey blinking = running)
 *     ⎿  first lines of the result … +N lines (ctrl+o to expand)
 *   ⏺ Update(path) → ⎿ Updated path with A additions and R removals + the diff
 *   ✻ Thinking…
 * plus the welcome box on top and the input box / status line at the bottom.
 * No timestamps in the flow (Claude Code shows none) — they sit in the hover title. */
(function () {
  'use strict';
  const X = window.X;
  const E = X.esc;
  const PREVIEW = 4;

  const rel = (p, cwd) => {
    if (!p) return '';
    if (cwd && p.startsWith(cwd + '/')) return p.slice(cwd.length + 1);
    const home = /^\/Users\/[^/]+/.exec(p);
    return home ? '~' + p.slice(home[0].length) : p;
  };
  const lines = (s) => String(s || '').replace(/\n+$/, '').split('\n');
  const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;
  const stripReminder = (s) => String(s || '').replace(/<system-reminder>[\s\S]*?<\/system-reminder>\s*/g, '');

  function mcpName(name) {
    const m = /^mcp__([^_]+(?:_[^_]+)*?)__(.+)$/.exec(name);
    return m ? `${m[1]} - ${m[2]} (MCP)` : name;
  }

  // Tool head: Name(args) the way Claude Code prints it.
  function head(e, cwd) {
    const i = e.input || {};
    const q = (s) => `"${s}"`;
    switch (e.name) {
      case 'Bash': return ['Bash', i.command || e.summary];
      case 'Read': return ['Read', rel(i.file_path || e.summary, cwd)];
      case 'Write': return ['Write', rel(i.file_path || e.summary, cwd)];
      case 'Edit': case 'MultiEdit': return ['Update', rel(i.file_path || e.summary, cwd)];
      case 'NotebookEdit': return ['Edit Notebook', rel(i.notebook_path || e.summary, cwd)];
      case 'Grep': return ['Search', [i.pattern != null ? `pattern: ${q(i.pattern)}` : '', i.path ? `path: ${q(rel(i.path, cwd))}` : '', i.glob ? `glob: ${q(i.glob)}` : '', i.output_mode ? `output_mode: ${q(i.output_mode)}` : ''].filter(Boolean).join(', ')];
      case 'Glob': return ['Search', [`pattern: ${q(i.pattern || e.summary)}`, i.path ? `path: ${q(rel(i.path, cwd))}` : ''].filter(Boolean).join(', ')];
      case 'WebFetch': return ['Fetch', i.url || e.summary];
      case 'WebSearch': return ['Web Search', q(i.query || e.summary)];
      case 'TodoWrite': return ['Update Todos', null];
      case 'Agent': case 'Task': return [i.subagent_type && i.subagent_type !== 'general-purpose' ? i.subagent_type : 'Task', i.description || e.summary];
      case 'Skill': return ['Skill', i.skill || e.summary];
      default: return [mcpName(e.name), i._json ? i._json : Object.keys(i).length ? Object.entries(i).map(([k, v]) => `${k}: ${typeof v === 'string' ? q(v.length > 80 ? v.slice(0, 80) + '…' : v) : JSON.stringify(v)}`).join(', ') : e.summary];
    }
  }

  function diffBlock(oldS, newS) {
    const a = lines(oldS), b = lines(newS);
    // trim the common head/tail so only the changed hunk shows, with a line of context each side
    let s = 0; while (s < a.length && s < b.length && a[s] === b[s]) s++;
    let ea = a.length - 1, eb = b.length - 1; while (ea >= s && eb >= s && a[ea] === b[eb]) { ea--; eb--; }
    const ctxB = s > 0 ? [a[s - 1]] : [];
    const ctxA = ea + 1 < a.length ? [a[ea + 1]] : [];
    const out = [];
    for (const l of ctxB) out.push(`<div class="cc-d ctx"><span class="sg"> </span>${E(l)}</div>`);
    for (const l of a.slice(s, ea + 1).slice(0, 60)) out.push(`<div class="cc-d del"><span class="sg">-</span>${E(l)}</div>`);
    for (const l of b.slice(s, eb + 1).slice(0, 60)) out.push(`<div class="cc-d add"><span class="sg">+</span>${E(l)}</div>`);
    for (const l of ctxA) out.push(`<div class="cc-d ctx"><span class="sg"> </span>${E(l)}</div>`);
    const more = Math.max(0, ea + 1 - s - 60) + Math.max(0, eb + 1 - s - 60);
    if (more) out.push(`<div class="cc-dim">… +${more} lines</div>`);
    return `<div class="cc-diff">${out.join('')}</div>`;
  }

  // The ⎿ block under a tool head.
  function body(e, cwd, open) {
    const i = e.input || {};
    const res = e.result ? stripReminder(e.result.text) : null;
    if (res == null) return `<div class="cc-res"><span class="cc-el">⎿</span><span class="cc-dim">${e.name === 'Bash' ? 'Running…' : 'Waiting…'}</span></div>`;
    if (e.is_error) {
      const ls = lines(res);
      const shown = open ? ls : ls.slice(0, PREVIEW);
      return `<div class="cc-res err"><span class="cc-el">⎿</span><pre>${E(/^Error/.test(ls[0]) ? shown.join('\n') : 'Error: ' + shown.join('\n'))}${!open && ls.length > PREVIEW ? `\n<span class="cc-dim">… +${ls.length - PREVIEW} lines (ctrl+o to expand)</span>` : ''}</pre></div>`;
    }
    const msg = (t, extra = '') => `<div class="cc-res"><span class="cc-el">⎿</span><div>${t}${extra}</div></div>`;
    switch (e.name) {
      case 'Read': {
        const n = lines(res).filter(l => /^\s*\d+[→\t]/.test(l)).length || lines(res).length;
        return msg(`Read <b>${n}</b> lines${e.result.more ? ' <span class="cc-dim">(+more)</span>' : ''}`, open ? `<pre class="cc-out">${E(res)}</pre>` : ' <span class="cc-dim">(ctrl+o to expand)</span>');
      }
      case 'Edit': case 'MultiEdit': {
        const ed = e.edit || {};
        const pairs = e.name === 'MultiEdit' ? (i.edits || []) : [{ old_string: i.old_string, new_string: i.new_string }];
        const diffs = pairs.filter(p => p.old_string != null || p.new_string != null).map(p => diffBlock(p.old_string, p.new_string)).join('');
        return msg(`Updated <b>${E(rel(i.file_path || e.summary, cwd))}</b> with ${plural(ed.added || 0, 'addition', 'additions')} and ${plural(ed.removed || 0, 'removal', 'removals')}`, diffs);
      }
      case 'Write': {
        const ls = lines(i.content);
        return msg(`Wrote ${i.lines || ls.length} lines to <b>${E(rel(i.file_path || e.summary, cwd))}</b>`, `<pre class="cc-out">${E((open ? ls : ls.slice(0, 8)).join('\n'))}${!open && ls.length > 8 ? `\n<span class="cc-dim">… +${ls.length - 8} lines (ctrl+o to expand)</span>` : ''}</pre>`);
      }
      case 'Grep': case 'Glob': {
        const ls = lines(res).filter(Boolean);
        const files = /^Found \d+/.test(ls[0] || '') ? ls[0] : `Found ${plural(ls.length, i.output_mode === 'content' ? 'line' : 'file', i.output_mode === 'content' ? 'lines' : 'files')}`;
        return msg(E(files), open ? `<pre class="cc-out">${E(ls.join('\n'))}</pre>` : ' <span class="cc-dim">(ctrl+o to expand)</span>');
      }
      case 'TodoWrite': {
        const box = (t) => t.status === 'completed' ? '<span class="cc-todo done">☒ ' + E(t.content) + '</span>' : t.status === 'in_progress' ? '<span class="cc-todo now">☐ ' + E(t.content) + '</span>' : '<span class="cc-todo">☐ ' + E(t.content) + '</span>';
        return `<div class="cc-res"><span class="cc-el">⎿</span><div class="cc-todos">${(i.todos || []).map(box).join('')}</div></div>`;
      }
      case 'Agent': case 'Task': {
        const ls = lines(res);
        const meta = e.done_ts && e.ts ? ` · ${X.dur((e.done_ts - e.ts) / 1000)}` : '';
        return msg(`Done${meta ? ` <span class="cc-dim">(${meta.slice(3)})</span>` : ''}`, open ? `<pre class="cc-out">${E(ls.join('\n'))}</pre>` : '');
      }
      default: {
        const ls = lines(res);
        const shown = open ? ls : ls.slice(0, PREVIEW);
        return `<div class="cc-res"><span class="cc-el">⎿</span><pre class="cc-out">${E(shown.join('\n')) || '<span class="cc-dim">(No content)</span>'}${!open && ls.length > PREVIEW ? `\n<span class="cc-dim">… +${ls.length - PREVIEW} lines (ctrl+o to expand)</span>` : ''}</pre></div>`;
      }
    }
  }

  const tip = (e) => [e.ts ? new Date(e.ts).toLocaleString('ko-KR') : '', e.req ? `${X.short(e.req.account)} · ${X.ms(e.req.duration_ms)} · ${X.usd(e.req.cost_usd)}` : ''].filter(Boolean).join(' · ');

  /**
   * @param entries chat entries (lib/transcript.mjs messagesToEntries)
   * @param opt { cwd, model, branch, version, status, context, open:Set<toolId>, limit, owner }
   */
  X.renderCC = function (entries, opt = {}) {
    const cwd = opt.cwd || '';
    const open = opt.open || new Set();
    const shown = entries.filter(e => e.kind !== 'meta');
    const limit = opt.limit || 400;
    const start = Math.max(0, shown.length - limit);
    const out = [];
    out.push(`<div class="cc-welcome"><div><span class="cc-star">✻</span> Welcome to <b>Claude Code</b>${opt.version ? ` <span class="cc-dim">v${E(opt.version)}</span>` : ''}</div><div class="cc-dim">&nbsp;</div><div class="cc-dim">&nbsp;&nbsp;/help for help, /status for your current setup</div><div class="cc-dim">&nbsp;</div><div class="cc-dim">&nbsp;&nbsp;cwd: ${E(cwd || '—')}</div></div>`);
    if (start) out.push(`<button type="button" class="cc-more" data-cc-more="1">↑ 이전 ${start}개 항목 더 보기</button>`);
    for (const e of shown.slice(start)) {
      if (e.kind === 'user') {
        out.push(`<div class="cc-user" title="${E(tip(e))}"><span class="cc-gt">&gt;</span><div>${E(e.clean || e.text)}${e.more ? `<span class="cc-dim"> … +${X.n(e.more)} chars</span>` : ''}</div></div>`);
      } else if (e.kind === 'agentmsg') {
        out.push(`<div class="cc-line cc-dimline" title="${E(tip(e))}"><span class="cc-dot dim">⏺</span><div>${E(e.text.slice(0, 300))}${e.text.length > 300 ? '…' : ''}</div></div>`);
      } else if (e.kind === 'assistant') {
        out.push(`<div class="cc-line" title="${E(tip(e))}"><span class="cc-dot">⏺</span><div class="cc-md">${X.md(e.text)}${e.more ? `<span class="cc-dim"> … +${X.n(e.more)} chars</span>` : ''}</div></div>`);
      } else if (e.kind === 'thinking') {
        const isOpen = open.has('th:' + (e.ts || '') + ':' + (e.chars || 0));
        out.push(`<div class="cc-think" data-cc-toggle="th:${E(String(e.ts || ''))}:${e.chars || 0}" title="${E(tip(e))}"><span class="cc-star">✻</span> <i>Thinking…</i>${isOpen && e.text ? `<pre class="cc-thought">${E(e.text)}${e.more ? '…' : ''}</pre>` : ''}</div>`);
      } else if (e.kind === 'tool') {
        const [name, args] = head(e, cwd);
        const state = e.result ? (e.is_error ? 'err' : 'ok') : 'run';
        const isOpen = open.has(e.id);
        const agentLink = (e.name === 'Agent' || e.name === 'Task') && opt.agentLink ? ` <a class="cc-link" data-agent-tool="${E(e.id)}">서브에이전트 보기 ›</a>` : '';
        out.push(`<div class="cc-tool ${state}" data-cc-toggle="${E(e.id)}" title="${E(tip(e))}"><div class="cc-line"><span class="cc-dot ${state}">⏺</span><div><b>${E(name)}</b>${args != null ? `<span class="cc-args">(${E(args)})</span>` : ''}${agentLink}</div></div>${body(e, cwd, isOpen)}</div>`);
      }
    }
    const status = opt.status === 'running' ? `<div class="cc-working"><span class="cc-spin">✻</span> Working… <span class="cc-dim">(esc to interrupt)</span></div>` : '';
    const ctx = opt.context ? ` · Context left until auto-compact: ${Math.max(0, Math.round(100 - opt.context.pct * 100))}%` : '';
    out.push(`${status}<div class="cc-input"><span class="cc-gt">&gt;</span><span class="cc-caret"></span></div><div class="cc-foot"><span>? for shortcuts</span><span>${E([opt.model, opt.branch && opt.branch !== 'HEAD' ? '⎇ ' + opt.branch : ''].filter(Boolean).join(' · '))}${E(ctx)}</span></div>`);
    return `<div class="cc">${out.join('')}</div>`;
  };
})();
