// Session overview: the instructions a person gave, in order, and what the agent did for each.
// Built from chat entries (transcript or raw-io) plus the llmux request rows of the session.
import { rowTokens } from './pricing.mjs';

/** Strip harness wrappers (soma-work <speaker>/<context>, Slack link markup) down to what was asked. */
export function cleanPrompt(s) {
  if (!s) return '';
  return String(s)
    .replace(/<(local|user|zworkflow|stv):[\w-]+>\s*Base directory for this skill:[\s\S]*$/, ' ')
    .replace(/Base directory for this skill:[\s\S]*$/, ' ')
    .replace(/<context>[\s\S]*?<\/context>/g, ' ')
    .replace(/<context>[\s\S]*$/g, ' ')
    .replace(/<thread-awareness>[\s\S]*?<\/thread-awareness>/g, ' ')
    .replace(/<speaker>[^<]*<\/speaker>/g, ' ')
    .replace(/<invoked_skills>[\s\S]*?<\/invoked_skills>/g, ' ')
    .replace(/<system-reminder>[\s\S]*?<\/system-reminder>/g, ' ')
    .replace(/<(https?:[^|>]+)\|([^>]+)>/g, '$2')
    .replace(/<(https?:[^>]+)>/g, '$1')
    .replace(/<@([A-Z0-9]+)>/g, '@$1')
    .replace(/<\/?[a-z][\w-]*(?:\s[^>]*)?>/gi, ' ')
    .replace(/\s+/g, ' ')
    .trim();
}

// What a person typed, from a `user`-kind excerpt: drop harness text, unwrap a mid-turn message.
export function promptOf(ex) {
  const t = String(ex || '').trim();
  const mid = /The user sent a new message while you were working:\s*([\s\S]*?)(?:\s*This is how Claude Code surfaces[\s\S]*)?$/.exec(t);
  if (mid) return mid[1].trim() || null;
  if (!t || /^Tool loaded\.?$/i.test(t) || /^(Another Claude session sent a message|<agent-message|<cross-session-message|\[Subagent hand-back\]|<(system-reminder|task-notification|local-command|command-)|Caveat:|\[Request interrupted|This session is being continued from a previous conversation)/.test(t)) return null;
  return t;
}

const FILE_TOOLS = new Set(['Edit', 'Write', 'NotebookEdit', 'MultiEdit']);
const AGENT_TOOLS = new Set(['Agent', 'Task']);
const LINK_RE = /https:\/\/github\.com\/[\w.-]+\/[\w.-]+\/(?:pull|issues)\/\d+/g;

export function buildOverview(entries, rows, priceOf) {
  // Each entry gets its instruction number (gi) so every view groups the same way; 0 = before the first.
  const groups = [];
  let cur = null, n = 0;
  for (const e of entries || []) {
    if (e.kind === 'meta') { e.gi = n; continue; }
    if (e.kind === 'user') {
      const text = cleanPrompt(e.text);
      e.gi = text ? ++n : n;
      if (!text) continue;
      e.clean = text;
      cur = { n, text, at: e.ts || null, entries: [] };
      groups.push(cur);
      continue;
    }
    if (!cur) { cur = { n: 0, text: null, at: e.ts || null, entries: [] }; groups.push(cur); }
    cur.entries.push(e);
    e.gi = cur.n;
  }
  const sorted = (rows || []).slice().sort((a, b) => a.at_ms - b.at_ms);
  const files = new Map();
  const links = new Set();
  const out = groups.map((g, i) => {
    const next = groups[i + 1];
    const lastTs = g.entries.reduce((m, e) => Math.max(m, e.done_ts || e.ts || 0), g.at || 0) || null;
    const end = (next && next.at) || lastTs;
    const tools = {}; let errors = 0, agents = 0, edits = 0;
    let final = null;
    for (const e of g.entries) {
      if (e.kind === 'tool') {
        tools[e.name] = (tools[e.name] || 0) + 1;
        if (e.is_error) errors++;
        if (AGENT_TOOLS.has(e.name)) agents++;
        if (FILE_TOOLS.has(e.name) && e.summary) {
          edits++;
          const f = files.get(e.summary) || { path: e.summary, edits: 0, added: 0, removed: 0, last: 0, tool: e.name };
          f.edits++; if (e.edit) { f.added += e.edit.added || 0; f.removed += e.edit.removed || 0; }
          f.last = Math.max(f.last, e.ts || 0); files.set(e.summary, f);
        }
        for (const m of String((e.result && e.result.text) || '').match(LINK_RE) || []) links.add(m);
      } else if (e.kind === 'assistant') {
        final = e.text;
        for (const m of String(e.text || '').match(LINK_RE) || []) links.add(m);
      }
    }
    for (const m of String(g.text || '').match(LINK_RE) || []) links.add(m);
    // Requests that completed inside this instruction's span.
    const from = g.at || 0, to = (next && next.at) || Infinity;
    let req = 0, cost = 0, tokIn = 0, tokOut = 0;
    for (const r of sorted) {
      if (r.at_ms < from || r.at_ms >= to) continue;
      // Totals count each request's sub-usages too (#278), as its cost does.
      const t = rowTokens(r);
      req++; cost += priceOf(r); tokOut += t.output;
      tokIn += t.input + t.cache_read + t.cache_creation;
    }
    const toolCount = Object.values(tools).reduce((a, b) => a + b, 0);
    return {
      i: g.n, text: g.text, at: g.at, end, durationMs: g.at && end ? Math.max(0, end - g.at) : null,
      tools, toolCount, edits, agents, errors, requests: req, cost, tokIn, tokOut,
      final: final ? cleanPrompt(final).slice(0, 400) : null,
      open: !next,
    };
  });
  return {
    instructions: out,
    files: [...files.values()].sort((a, b) => b.last - a.last),
    links: [...links].slice(-12),
  };
}
