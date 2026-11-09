// Claude Code session reconstruction.
// Two sources, joined by session_id:
//  1. the Claude Code transcript on this host (<config>/projects/<cwd>/<session_id>.jsonl) —
//     complete, exactly what the Claude Code chat shows;
//  2. llmux raw-io (GET /llmux/raw-io?id=&at_ms=) — any client on any machine, but the
//     request body is clipped at raw_io.max_body_bytes (8 MiB), so long sessions lose
//     their newest turns from the request side.
import fs from 'node:fs';
import path from 'node:path';

const MAX_TEXT = 6000;
const MAX_RESULT = 2400;

const MID_TURN = /The user sent a new message while you were working:\s*([\s\S]*?)(?:\n\s*This is how Claude Code surfaces[\s\S]*)?$/;
const clip = (s, n) => (s && s.length > n ? { text: s.slice(0, n), more: s.length - n } : { text: s || '', more: 0 });

export function toolSummary(name, input) {
  input = input || {};
  const pick = (k) => (typeof input[k] === 'string' ? input[k] : '');
  switch (name) {
    case 'Bash': return pick('command').split('\n')[0].slice(0, 200);
    case 'Read': case 'Write': case 'Edit': case 'NotebookEdit': return pick('file_path') || pick('notebook_path');
    case 'Grep': return `${pick('pattern')}${input.path ? ' · ' + input.path : ''}`.slice(0, 200);
    case 'Glob': return pick('pattern');
    case 'Agent': case 'Task': return `${pick('subagent_type') || 'agent'} · ${pick('description')}`;
    case 'WebFetch': return pick('url');
    case 'WebSearch': return pick('query');
    case 'TodoWrite': return `${(input.todos || []).length} todos`;
    case 'Skill': return pick('skill');
    default: {
      const s = JSON.stringify(input);
      return s.length > 160 ? s.slice(0, 160) + '…' : s;
    }
  }
}

// What the Claude Code screen needs from a tool call's input (clipped; never the whole payload).
const clipS = (v, n) => (typeof v === 'string' && v.length > n ? v.slice(0, n) + `\n… (+${v.length - n}자)` : v);
export function slimInput(name, input) {
  input = input || {};
  switch (name) {
    case 'Edit': return { file_path: input.file_path, old_string: clipS(input.old_string, 4000), new_string: clipS(input.new_string, 4000), replace_all: input.replace_all };
    case 'MultiEdit': return { file_path: input.file_path, edits: (input.edits || []).slice(0, 12).map(e => ({ old_string: clipS(e.old_string, 1500), new_string: clipS(e.new_string, 1500) })) };
    case 'Write': return { file_path: input.file_path, content: clipS(input.content, 4000), lines: typeof input.content === 'string' ? input.content.split('\n').length : null };
    case 'NotebookEdit': return { notebook_path: input.notebook_path, new_source: clipS(input.new_source, 2000) };
    case 'Bash': return { command: clipS(input.command, 3000), description: input.description, run_in_background: input.run_in_background };
    case 'Read': return { file_path: input.file_path, offset: input.offset, limit: input.limit };
    case 'Grep': return { pattern: input.pattern, path: input.path, glob: input.glob, type: input.type, output_mode: input.output_mode };
    case 'Glob': return { pattern: input.pattern, path: input.path };
    case 'TodoWrite': return { todos: (input.todos || []).slice(0, 40).map(t => ({ content: t.content, status: t.status })) };
    case 'Agent': case 'Task': return { description: input.description, subagent_type: input.subagent_type, prompt: clipS(input.prompt, 1200) };
    case 'WebFetch': return { url: input.url, prompt: clipS(input.prompt, 400) };
    case 'WebSearch': return { query: input.query };
    case 'Skill': return { skill: input.skill, args: clipS(input.args, 400) };
    default: { const t = JSON.stringify(input); return t.length > 1500 ? { _json: t.slice(0, 1500) + '…' } : input; }
  }
}

function resultText(content) {
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) return content.map(c => c.type === 'text' ? c.text : c.type === 'image' ? '[image]' : '').join('\n');
  return '';
}

function editStats(input) {
  if (!input) return null;
  if (typeof input.old_string === 'string' && typeof input.new_string === 'string') {
    const a = input.old_string.split('\n').length, b = input.new_string.split('\n').length;
    return { removed: a, added: b };
  }
  if (typeof input.content === 'string') return { added: input.content.split('\n').length, removed: 0 };
  return null;
}

// Normalise a list of Anthropic-shaped {role, content} messages (+ optional per-message meta)
// into chat entries. Tool results are folded into their tool_use entry.
export function messagesToEntries(msgs) {
  const entries = [];
  const tools = new Map();
  for (const m of msgs) {
    const meta = m._meta || {};
    const blocks = typeof m.content === 'string' ? [{ type: 'text', text: m.content }] : (m.content || []);
    // Text riding along with tool results is harness output ("Tool loaded.", reminders), except a message the
    // person typed mid-turn, which Claude Code wraps as "The user sent a new message while you were working: …".
    const withResults = m.role === 'user' && blocks.some(b => b && b.type === 'tool_result');
    for (const b of blocks) {
      if (m.role === 'user') {
        if (b.type === 'tool_result') {
          const t = tools.get(b.tool_use_id);
          const txt = resultText(b.content);
          if (t) { Object.assign(t, { result: clip(txt, MAX_RESULT), is_error: !!b.is_error, done_ts: meta.ts }); }
          const mid = MID_TURN.exec(txt);
          if (mid) entries.push({ kind: 'user', mid: true, ...clip(mid[1].trim(), MAX_TEXT), ts: meta.ts });
          continue;
        }
        if (b.type === 'text') {
          const text = b.text || '';
          const mid = MID_TURN.exec(text);
          if (mid) { entries.push({ kind: 'user', mid: true, ...clip(mid[1].trim(), MAX_TEXT), ts: meta.ts }); continue; }
          const sys = meta.isMeta || withResults || /^<(system-reminder|command-|local-command|task-notification|user-prompt-submit-hook)/.test(text.trim()) || /^This session is being continued from a previous conversation/.test(text.trim());
          // Another agent (a subagent hand-back, a peer session) wrote this turn, not the person.
          const agentMsg = !sys && /^\s*(Another Claude session sent a message|<agent-message|<cross-session-message|\[Subagent hand-back\])/.test(text);
          entries.push({ kind: sys ? 'meta' : agentMsg ? 'agentmsg' : 'user', ...clip(text, MAX_TEXT), ts: meta.ts });
        } else if (b.type === 'image') entries.push({ kind: 'user', text: '[image]', more: 0, ts: meta.ts });
      } else if (m.role === 'assistant') {
        if (b.type === 'text' && b.text) entries.push({ kind: 'assistant', ...clip(b.text, MAX_TEXT), ts: meta.ts, model: meta.model, requestId: meta.requestId });
        else if (b.type === 'thinking' || b.type === 'redacted_thinking') entries.push({ kind: 'thinking', chars: (b.thinking || '').length, ...clip(b.thinking || '', 1200), ts: meta.ts });
        else if (b.type === 'tool_use') {
          const t = { kind: 'tool', id: b.id, name: b.name, summary: toolSummary(b.name, b.input), edit: editStats(b.input), input: slimInput(b.name, b.input), ts: meta.ts, requestId: meta.requestId, model: meta.model };
          if ((b.name === 'Agent' || b.name === 'Task') && b.input) t.prompt = clip(String(b.input.prompt || ''), 600);
          tools.set(b.id, t); entries.push(t);
        }
      }
    }
  }
  return entries;
}

// ---------- Claude Code transcript (.jsonl) ----------
const cache = new Map(); // file -> {mtime, size, value}

export function readTranscript(file) {
  const st = fs.statSync(file);
  const c = cache.get(file);
  if (c && c.mtime === st.mtimeMs && c.size === st.size) return c.value;
  const lines = fs.readFileSync(file, 'utf8').split('\n');
  const meta = { file, title: null, cwd: null, branch: null, version: null, entrypoint: null, first_ts: null, last_ts: null, model: null, cost: null, prompts: 0, steps: 0, lastStop: null, lastRole: null };
  const msgs = [];
  const usageByReq = new Map();
  for (const line of lines) {
    if (!line) continue;
    let o; try { o = JSON.parse(line); } catch { continue; }
    if (o.type === 'ai-title' && o.aiTitle) meta.title = o.aiTitle;
    if (o.type === 'cost-state' && typeof o.totalCostUSD === 'number') meta.cost = o.totalCostUSD;
    if (o.cwd && !meta.cwd) meta.cwd = o.cwd;
    if (o.gitBranch) meta.branch = o.gitBranch;
    if (o.version) meta.version = o.version;
    if (o.entrypoint) meta.entrypoint = o.entrypoint;
    if (o.timestamp) { const t = Date.parse(o.timestamp); if (!meta.first_ts) meta.first_ts = t; meta.last_ts = t; }
    if ((o.type === 'user' || o.type === 'assistant') && o.message && !o.isSidechain) {
      const m = o.message;
      const ts = o.timestamp ? Date.parse(o.timestamp) : null;
      if (o.type === 'assistant') {
        meta.steps++; meta.model = m.model || meta.model; meta.lastStop = m.stop_reason || meta.lastStop; meta.lastRole = 'assistant';
        if (o.requestId && m.usage) usageByReq.set(o.requestId, { usage: m.usage, model: m.model, ts });
      } else {
        const isPrompt = typeof m.content === 'string' || (Array.isArray(m.content) && m.content.some(b => b.type === 'text'));
        if (isPrompt && !o.isMeta) meta.prompts++;
        meta.lastRole = 'user';
      }
      msgs.push({ role: m.role, content: m.content, _meta: { ts, isMeta: !!o.isMeta, requestId: o.requestId, model: m.model } });
    }
  }
  const value = { meta, msgs, usageByReq };
  cache.set(file, { mtime: st.mtimeMs, size: st.size, value });
  if (cache.size > 64) cache.delete(cache.keys().next().value);
  return value;
}

// Cheap metadata for the session list: head for cwd/branch, tail for title/last ts.
const metaCache = new Map();
export function transcriptMeta(file) {
  let st; try { st = fs.statSync(file); } catch { return null; }
  const c = metaCache.get(file);
  if (c && c.mtime === st.mtimeMs) return c.value;
  const fd = fs.openSync(file, 'r');
  const read = (pos, len) => { const b = Buffer.alloc(len); const n = fs.readSync(fd, b, 0, len, pos); return b.subarray(0, n).toString('utf8'); };
  const head = read(0, Math.min(st.size, 512 * 1024));
  const tail = read(Math.max(0, st.size - 192 * 1024), Math.min(st.size, 192 * 1024));
  fs.closeSync(fd);
  const v = { title: null, cwd: null, branch: null, entrypoint: null, version: null, last_ts: st.mtimeMs, first_ts: null, model: null, lastStop: null, lastRole: null, lastPrompt: null, bytes: st.size, cost: null };
  // Head lines can be huge (pasted prompts), so read the stable fields with regexes.
  const rx = (re) => { const m = re.exec(head); return m ? m[1] : null; };
  v.cwd = rx(/"cwd":"((?:[^"\\]|\\.)*)"/);
  v.entrypoint = rx(/"entrypoint":"([^"]+)"/);
  const ft = rx(/"timestamp":"([^"]+)"/); if (ft) v.first_ts = Date.parse(ft);
  for (const line of head.split('\n')) {
    let o; try { o = JSON.parse(line); } catch { continue; }
    if (o.type === 'user' && !o.isMeta && o.message && typeof o.message.content === 'string') { v.firstPrompt = o.message.content.slice(0, 200); break; }
  }
  v.links = [...new Set((tail.match(/https:\/\/github\.com\/[\w.-]+\/[\w.-]+\/(?:pull|issues)\/\d+/g) || []))].slice(-4);
  for (const line of tail.split('\n')) {
    let o; try { o = JSON.parse(line); } catch { continue; }
    if (o.type === 'ai-title' && o.aiTitle) v.title = o.aiTitle;
    if (o.type === 'cost-state' && typeof o.totalCostUSD === 'number') v.cost = o.totalCostUSD;
    if (o.gitBranch) v.branch = o.gitBranch;
    if (o.version) v.version = o.version;
    if (o.timestamp) v.last_ts = Date.parse(o.timestamp);
    if (o.isSidechain) continue;
    if (o.type === 'assistant' && o.message) { v.model = o.message.model || v.model; v.lastStop = o.message.stop_reason; v.lastRole = 'assistant'; }
    if (o.type === 'user' && o.message) {
      v.lastRole = 'user';
      const txt = typeof o.message.content === 'string' ? o.message.content : Array.isArray(o.message.content) ? (o.message.content.find(b => b.type === 'text') || {}).text : null;
      if (!o.isMeta && txt && !/^\s*(<(task-notification|system-reminder|local-command|command-|user-prompt-submit)|Caveat:|\[Request interrupted)/.test(txt)) v.lastPrompt = txt.slice(0, 240);
    }
  }
  metaCache.set(file, { mtime: st.mtimeMs, value: v });
  return v;
}

export function listSubagents(file) {
  const dir = path.join(file.slice(0, -'.jsonl'.length), 'subagents');
  let names = []; try { names = fs.readdirSync(dir); } catch { return []; }
  return names.filter(n => n.endsWith('.jsonl')).map(n => {
    const id = n.slice('agent-'.length, -'.jsonl'.length);
    let meta = {}; try { meta = JSON.parse(fs.readFileSync(path.join(dir, `agent-${id}.meta.json`), 'utf8')); } catch {}
    const st = fs.statSync(path.join(dir, n));
    return { id, file: path.join(dir, n), type: meta.agentType, description: meta.description, toolUseId: meta.toolUseId, bytes: st.size, mtime: st.mtimeMs };
  }).sort((a, b) => a.mtime - b.mtime);
}

// ---------- raw-io fallback ----------
// The request body may end with "…[truncated N bytes]". Recover every complete element
// of the top-level "messages" array by scanning with a string-aware bracket counter.
export function recoverMessages(body) {
  try { const j = JSON.parse(body); return { messages: j.messages || [], truncated: false, model: j.model, tools: (j.tools || []).length }; } catch {}
  const k = body.indexOf('"messages"');
  if (k < 0) return { messages: [], truncated: true };
  let i = body.indexOf('[', k) + 1;
  const out = [];
  let depth = 0, start = -1, inStr = false, esc = false;
  for (; i < body.length; i++) {
    const ch = body[i];
    if (inStr) { if (esc) esc = false; else if (ch === '\\') esc = true; else if (ch === '"') inStr = false; continue; }
    if (ch === '"') { inStr = true; continue; }
    if (ch === '{') { if (depth === 0) start = i; depth++; }
    else if (ch === '}') { depth--; if (depth === 0 && start >= 0) { try { out.push(JSON.parse(body.slice(start, i + 1))); } catch {} start = -1; } }
    else if (ch === ']' && depth === 0) break;
  }
  const m = /"model"\s*:\s*"([^"]+)"/.exec(body.slice(0, 4096));
  return { messages: out, truncated: true, model: m && m[1] };
}

// Parse an Anthropic SSE (or plain JSON) response body into assistant content blocks.
export function parseResponse(body) {
  if (!body) return { blocks: [], stop: null, usage: null };
  const t = body.trimStart();
  if (t.startsWith('{')) {
    try { const j = JSON.parse(t); return { blocks: j.content || [], stop: j.stop_reason, usage: j.usage, model: j.model }; } catch {}
  }
  const blocks = []; let stop = null, usage = null, model = null;
  for (const line of body.split('\n')) {
    if (!line.startsWith('data:')) continue;
    let e; try { e = JSON.parse(line.slice(5).trim()); } catch { continue; }
    if (e.type === 'message_start') { model = e.message && e.message.model; usage = e.message && e.message.usage; }
    else if (e.type === 'content_block_start') { const b = { ...e.content_block }; if (b.type === 'tool_use') { b._json = ''; b.input = {}; } if (b.type === 'text') b.text = b.text || ''; if (b.type === 'thinking') b.thinking = b.thinking || ''; blocks[e.index] = b; }
    else if (e.type === 'content_block_delta') {
      const b = blocks[e.index]; if (!b) continue; const d = e.delta || {};
      if (d.type === 'text_delta') b.text += d.text; else if (d.type === 'thinking_delta') b.thinking += d.thinking; else if (d.type === 'input_json_delta') b._json += d.partial_json;
    } else if (e.type === 'content_block_stop') { const b = blocks[e.index]; if (b && b.type === 'tool_use') { try { b.input = JSON.parse(b._json || '{}'); } catch {} delete b._json; } }
    else if (e.type === 'message_delta') { stop = (e.delta && e.delta.stop_reason) || stop; usage = { ...(usage || {}), ...(e.usage || {}) }; }
  }
  return { blocks: blocks.filter(Boolean), stop, usage, model };
}
