// What one raw-io exchange says about the session that sent it — for sessions whose Claude Code
// transcript is not on this host (people running Claude Code on their own machines with an issued key).
// Reads headers and the request body; never returns header values that carry credentials.
import { recoverMessages } from './transcript.mjs';
import { promptOf } from './overview.mjs';

const NOISE = /^\s*(<(system-reminder|command-|local-command|task-notification|user-prompt-submit-hook|bash-|session-start)|Caveat:|\[Request interrupted|\[SUGGESTION MODE)/;

function headerMap(h) {
  const out = {};
  for (const x of h || []) {
    const [k, v] = Array.isArray(x) ? x : [x && x.name, x && x.value];
    if (k) out[String(k).toLowerCase()] = String(v ?? '');
  }
  return out;
}

const last = (re, s) => { let m, v = null; re.lastIndex = 0; while ((m = re.exec(s))) v = m[1]; return v; };
const first = (re, s) => { const m = re.exec(s); return m ? m[1] : null; };

/** Real prompts a person typed (text blocks of user turns that are not harness wrappers). */
export function userPrompts(messages) {
  const out = [];
  for (const m of messages || []) {
    if (!m || m.role !== 'user') continue;
    const blocks = typeof m.content === 'string' ? [{ type: 'text', text: m.content }] : Array.isArray(m.content) ? m.content : [];
    if (blocks.some(b => b && b.type === 'tool_result')) continue;
    const text = promptOf(blocks.filter(b => b && b.type === 'text' && !NOISE.test(b.text || '')).map(b => b.text).join('\n').trim());
    if (text) out.push(text.slice(0, 2000));
  }
  return out;
}

export function parseRawMeta(rec) {
  if (!rec) return null;
  const body = rec.request_body || '';
  const h = headerMap(rec.request_headers);
  const req = recoverMessages(body);
  let j = null; try { j = JSON.parse(body); } catch {}
  const head = body.slice(0, 200000);
  const ua = h['user-agent'] || '';
  const billing = first(/cc_version=([\w.-]+)/, head);
  const entry = first(/cc_entrypoint=([\w-]+)/, head) || first(/\(external, ([\w-]+)\)/, ua);
  const tools = j && Array.isArray(j.tools) ? j.tools.map(t => t && t.name).filter(Boolean) : [];
  const mcp = [...new Set(tools.filter(n => n.startsWith('mcp__')).map(n => n.split('__')[1]))];
  const prompts = userPrompts(req.messages);
  return {
    rawId: rec.id, at: rec.ts_ms, model: rec.model, account: rec.account,
    version: (first(/claude-cli\/([\w.-]+)/, ua) || billing || null),
    entrypoint: entry || null,
    app: h['x-app'] || null,
    os: h['x-stainless-os'] || null,
    arch: h['x-stainless-arch'] || null,
    runtime: h['x-stainless-runtime-version'] || null,
    cwd: first(/Primary working directory: ([^\n"\\]+)/, body) || last(/[Ww]orking directory: ([^\s"\\]+)/g, body),
    branch: first(/Current branch: ([^\n"\\]+)/, body),
    platform: first(/Platform: ([^\n"\\]+)/, body),
    osVersion: first(/OS Version: ([^\n"\\]+)/, body),
    claudeMd: [...new Set((body.match(/Contents of ([^\s"\\]+\/(?:CLAUDE|AGENTS)\.md)/g) || []).map(s => s.slice(12)))].slice(0, 8),
    effort: (j && j.output_config && j.output_config.effort) || null,
    thinking: (j && j.thinking && j.thinking.type) || null,
    maxTokens: (j && j.max_tokens) || null,
    tools: tools.filter(n => !n.startsWith('mcp__')).slice(0, 60),
    mcp,
    messages: req.messages.length,
    truncated: !!req.truncated,
    bytes: body.length,
    prompts: prompts.slice(-80),
    promptCount: prompts.length,
    firstPrompt: prompts[0] || null,
  };
}
