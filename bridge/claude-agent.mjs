// The executable bridge is embedded in llmux. stdout is a private framed protocol:
// one JSON HTTP status line, followed by Anthropic Messages SSE. No diagnostics,
// credentials, prompts, or SDK stderr are ever copied to stdout/stderr.
import { randomUUID } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createRequire } from 'node:module';
import { once } from 'node:events';
import { spawn } from 'node:child_process';

export const SDK_VERSION = '0.3.292';
const PREFIX = 'mcp__llmux__';
class InvalidRequest extends Error { status = 400; }

export function buildRequest(body, credential, upstream, directory, inherited = process.env) {
  if (!Array.isArray(body.messages) || body.messages.length === 0)
    throw new InvalidRequest('messages must be a nonempty array');
  if (body.messages.at(-1).role !== 'user')
    throw new InvalidRequest('messages must end with a user message');
  for (const field of ['temperature', 'top_p', 'top_k'])
    if (body[field] != null) throw new InvalidRequest(`Claude Agent SDK does not support ${field}.`);
  if (body.tool_choice?.disable_parallel_tool_use === true)
    throw new InvalidRequest('Claude Agent SDK does not support disabling parallel tool calls.');
  const choice = body.tool_choice?.type ?? 'auto';
  if (!['auto', 'none'].includes(choice)) throw new InvalidRequest('SDK supports auto or none tool choice');
  const tools = choice === 'none' ? [] : (body.tools ?? []);
  const webSearch = choice !== 'none' && body._llmux_web_search === true;
  const structured = body._llmux_output_format !== undefined;
  if (structured && (body._llmux_output_format?.type !== 'json_schema' || !body._llmux_output_format.schema || typeof body._llmux_output_format.schema !== 'object'))
    throw new InvalidRequest('Invalid structured output schema.');
  const names = new Set(tools.map(t => t.name));
  const mapContent = content => Array.isArray(content) ? content.map(block =>
    block.type === 'tool_use' && names.has(block.name)
      ? {...block, name: PREFIX + block.name} : block) : content;
  const messages = body.messages.map(message => ({...message, content: mapContent(message.content)}));
  const sessionId = randomUUID();
  let parentUuid = null;
  const finalHasToolResult = Array.isArray(messages.at(-1).content) && messages.at(-1).content.some(b => b.type === 'tool_result');
  const entries = (finalHasToolResult ? messages : messages.slice(0, -1)).map(message => {
    const uuid = randomUUID();
    const entry = {type: message.role, uuid, parentUuid, sessionId,
      timestamp: new Date().toISOString(), cwd: directory, isSidechain: false,
      message: message.role === 'assistant' ? {
        id: `msg_${uuid}`, type: 'message', model: body.model,
        stop_reason: Array.isArray(message.content) && message.content.some(b => b.type === 'tool_use') ? 'tool_use' : 'end_turn',
        stop_sequence: null, usage: {input_tokens: 0, output_tokens: 0}, ...message,
      } : message};
    parentUuid = uuid;
    return entry;
  });
  // Env is an allowlist, not process.env with a few deletes. In particular no
  // inherited ANTHROPIC_*, CLAUDE_*, NODE_OPTIONS, proxy, cloud or llmux auth.
  const env = {};
  for (const key of ['PATH', 'LANG', 'LC_ALL', 'SYSTEMROOT', 'WINDIR', 'TMPDIR', 'TMP', 'TEMP'])
    if (inherited[key]) env[key] = inherited[key];
  Object.assign(env, {HOME: directory, USERPROFILE: directory, CLAUDE_CONFIG_DIR: join(directory, 'config'),
    XDG_CONFIG_HOME: join(directory, 'xdg'), ANTHROPIC_BASE_URL: upstream,
    CLAUDE_AGENT_SDK_CLIENT_APP: 'llmux', CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1',
    CLAUDE_CODE_DISABLE_AUTO_MEMORY: '1', DISABLE_AUTOUPDATER: '1',
    CLAUDE_CODE_STARTUP_FAILURE_RESULTS: '1', CLAUDE_CODE_RETRY_WATCHDOG: '0',
    CLAUDE_CODE_MAX_RETRIES: '0', ENABLE_TOOL_SEARCH: 'false'});
  if (credential.type === 'oauth') env.CLAUDE_CODE_OAUTH_TOKEN = credential.token;
  else if (credential.type === 'apikey') env.ANTHROPIC_API_KEY = credential.token;
  else throw new InvalidRequest('unsupported Claude credential');
  if (body.max_tokens !== undefined) {
    if (!Number.isSafeInteger(body.max_tokens) || body.max_tokens < 1) throw new InvalidRequest('invalid max_tokens');
    env.CLAUDE_CODE_MAX_OUTPUT_TOKENS = String(body.max_tokens);
  }
  let systemPrompt = body.system;
  if (Array.isArray(systemPrompt)) systemPrompt = systemPrompt.map(b => b.text).join('\n\n');
  if (finalHasToolResult) env.CLAUDE_CODE_RESUME_INTERRUPTED_TURN = '1';
  const options = {cwd: directory, env, tools: webSearch ? ['WebSearch'] : [],
    settingSources: [], settings: {disableAllHooks: true}, plugins: [], skills: [],
    strictMcpConfig: true, systemPrompt: systemPrompt ?? '', includePartialMessages: true,
    permissionMode: 'default', verbatimPrompts: true,
    model: body.model, persistSession: entries.length > 0,
    // A client tool handler never returns; only WebSearch can start another round.
    maxTurns: webSearch || structured ? 16 : 1, thinking: {type: body.output_config?.effort ? 'adaptive' : 'disabled'},
    stderr: () => {},
  };
  if (structured) {
    // SDK StructuredOutput is a tool: its input must be an object. Nest the
    // requested schema as its own JSON-Schema resource so local $refs keep their
    // original root, and unwrap after the SDK validates the result.
    options.outputFormat = {type:'json_schema',schema:{type:'object',
      properties:{output:{$id:'urn:llmux:structured-output',...body._llmux_output_format.schema}},
      required:['output'],additionalProperties:false}};
  }
  if (entries.length) {
    options.resume = sessionId;
    options.sessionStore = {load: async () => entries, append: async () => {}};
  }
  if (body.output_config?.effort) options.effort = body.output_config.effort;
  if (body.thinking) options.thinking = body.thinking.type === 'enabled'
    ? {type: 'enabled', budgetTokens: body.thinking.budget_tokens} : {type: body.thinking.type};
  return {options, tools, webSearch, structured, finalHasToolResult, prompt: {type: 'user', message: finalHasToolResult ? {role:'user',content:''} : messages.at(-1),
    parent_tool_use_id: null, session_id: sessionId, client_composed: true}};
}

// Complete SDKAssistantMessageError union from the pinned 0.3.292 sdk.d.ts.
// Account-policy errors are distinct from expired authentication: refreshing an
// otherwise valid token cannot fix an organization ban, hold, or verification.
const SDK_ERROR_STATUS = Object.freeze({
  authentication_failed: 401,
  oauth_org_not_allowed: 403,
  account_on_hold: 403,
  verification_required: 403,
  billing_error: 402,
  rate_limit: 429,
  overloaded: 503,
  invalid_request: 400,
  model_not_found: 404,
  server_error: 500,
  unknown: 502,
  // A normal output limit is handled by its subsequent message_delta, not an
  // HTTP error. Without a stream, an isolated synthetic error is incomplete.
  max_output_tokens: 502,
  // The native SDK labels cloud credential loading errors transient. Never
  // refresh or disqualify the selected Claude account for a host/cloud error.
  cloud_credential_error: 503,
});
function sdkErrorCode(error) {
  const code = typeof error === 'string' ? error : error?.error;
  return Object.hasOwn(SDK_ERROR_STATUS, code) ? code : undefined;
}
export function errorStatus(error) {
  if (error instanceof InvalidRequest) return 400;
  const code = sdkErrorCode(error) ?? sdkErrorCode(error?.type);
  if (code) return SDK_ERROR_STATUS[code];
  const type = typeof error === 'string' ? error : error?.type;
  const statuses = {authentication_error:401,rate_limit_error:429,invalid_request_error:400,
    permission_error:403,not_found_error:404,overloaded_error:503,api_error:500};
  return Object.hasOwn(statuses,type) ? statuses[type] : 502;
}

export async function run(input, runtime, output = process.stdout) {
  const directory = input.directory ?? await mkdtemp(join(tmpdir(), 'llmux-agent-'));
  const abortController = new AbortController();
  let query;
  let sdkProcess;
  let sentHeaders = false;
  let stopped = false;
  let failed = false;
  const write = async value => { if (!output.write(value)) await once(output, 'drain'); };
  const headers = async (status, errorCode) => {
    if (!sentHeaders) { await write(JSON.stringify({status,...(errorCode ? {error_code:errorCode} : {})}) + '\n'); sentHeaders = true; }
  };
  const event = async value => { await headers(200); await write(`event: ${value.type}\ndata: ${JSON.stringify(value)}\n\n`); };
  const failure = async (status, message, code) => {
    failed = true;
    if (!sentHeaders) { await headers(status,sdkErrorCode(code)); await write(JSON.stringify({type:'error',error:{type:({400:'invalid_request_error',401:'authentication_error',402:'billing_error',403:'permission_error',404:'not_found_error',429:'rate_limit_error',503:'overloaded_error'})[status] ?? 'api_error',message}})); }
    else await event({type:'error',error:{type:'api_error',message}});
    cancel();
  };
  // The SDK's graceful close grants ~2 seconds after stdin EOF, during which
  // it can turn a canceled tool permission into another inference request.
  // Our external-tool boundary is final: terminate the native process first.
  const cancel = () => { sdkProcess?.kill('SIGKILL'); abortController.abort(); query?.close(); };
  process.once('SIGTERM', cancel);
  process.once('SIGINT', cancel);
  try {
    const request = buildRequest(input.body, input.credential, input.upstream, directory);
    const options = {...request.options, abortController, spawnClaudeCodeProcess: spec => {
      sdkProcess = spawn(spec.command,spec.args,{cwd:spec.cwd,env:spec.env,stdio:['pipe','pipe','pipe'],windowsHide:true});
      return sdkProcess;
    }};
    if (request.tools.length) {
      // Use the official MCP server transport with exact caller JSON schemas.
      // SDK's Zod convenience helper would otherwise narrow arbitrary schemas.
      const server = new runtime.McpServer({name:'llmux',version:'1.0.0'}, {capabilities:{tools:{}}});
      server.server.setRequestHandler(runtime.ListToolsRequestSchema, async () => ({tools: request.tools.map(t => ({
        name:t.name, description:t.description ?? '', inputSchema:t.input_schema,
        _meta:{'anthropic/alwaysLoad':true},
      }))}));
      // Never send a synthetic tool result, including on cancellation: that
      // would let the SDK begin another paid model turn before it shuts down.
      server.server.setRequestHandler(runtime.CallToolRequestSchema, async () => new Promise(() => {}));
      options.mcpServers = {llmux:{type:'sdk',name:'llmux',instance:server,alwaysLoad:true}};
    }
    options.canUseTool = async (name, args) => {
      if ((name === 'WebSearch' && request.webSearch) || (name === 'StructuredOutput' && request.structured))
        return {behavior:'allow',updatedInput:args};
      if (request.tools.some(t => PREFIX + t.name === name)) return new Promise(() => {});
      return {behavior:'deny',message:'Client owns tool execution.'};
    };
    // Keep stdin open for MCP callbacks until the model turn has been returned.
    async function* prompt() { if (!request.finalHasToolResult) yield request.prompt; await new Promise(resolve => abortController.signal.addEventListener('abort',resolve,{once:true})); }
    query = runtime.query({prompt:prompt(),options});
    let started = false, nextIndex = 0, external = false, internal = false;
    const indices = new Map();
    const usage = {};
    let roundUsage = {}, delta;
    for await (const item of query) {
      if (item.parent_tool_use_id) continue;
      if (item.type === 'assistant' && item.error) {
        // The SDK emits this synthetic diagnostic BEFORE the real max_tokens
        // delta. Preserve that partial response and its honest stop reason.
        if (item.error === 'max_output_tokens' && started) continue;
        const code = sdkErrorCode(item) ?? 'unknown';
        await failure(errorStatus(code), 'Claude Agent SDK request failed (' + code + ').', code); break;
      }
      if (item.type === 'rate_limit_event' && item.rate_limit_info?.status === 'rejected') { await failure(429,'Claude account rate limit exceeded.','rate_limit'); break; }
      if (item.type === 'system' && item.subtype === 'api_retry') { await failure(sdkErrorCode(item) ? errorStatus(item) : item.error_status ?? 502,'Claude Agent SDK upstream request failed.',sdkErrorCode(item)); break; }
      if (item.type === 'result') {
        if (request.structured && !item.is_error && Object.hasOwn(item, 'structured_output')) {
          if (!item.structured_output || !Object.hasOwn(item.structured_output,'output')) {await failure(502,'SDK omitted the structured output value.'); break;}
          const encoded = JSON.stringify(item.structured_output.output);
          if (encoded === undefined) {await failure(502,'SDK returned an invalid structured output.'); break;}
          if (!started) {started = true; await event({type:'message_start',message:{id:'msg_' + randomUUID(),type:'message',role:'assistant',model:input.body.model,content:[],stop_reason:null,usage:item.usage ?? {}}});}
          await event({type:'content_block_start',index:nextIndex,content_block:{type:'text',text:''}});
          await event({type:'content_block_delta',index:nextIndex,delta:{type:'text_delta',text:encoded}});
          await event({type:'content_block_stop',index:nextIndex});
          await event({type:'message_delta',delta:{stop_reason:'end_turn',stop_sequence:null},usage});
          await event({type:'message_stop'}); stopped = true;
        }
        if (!stopped) await failure(502, 'Claude Agent SDK ended before a complete model response.');
        break;
      }
      if (item.type !== 'stream_event') continue;
      const frame = structuredClone(item.event);
      if (frame.type === 'message_start') {
        indices.clear(); external = false; internal = false;
        if (!started) { started = true; await event(frame); }
        roundUsage = {...frame.message.usage};
      } else if (frame.type === 'content_block_start') {
        const block = frame.content_block;
        if (request.structured && (block.type === 'text' || (block.type === 'tool_use' && block.name === 'StructuredOutput'))) {internal = true; indices.set(frame.index,null); continue;}
        if (block.type === 'tool_use' && block.name === 'WebSearch' && request.webSearch) {internal = true; indices.set(frame.index,null); continue;}
        if (block.type === 'tool_use') {
          if (!request.tools.some(t => PREFIX + t.name === block.name)) {await failure(502,'SDK emitted an unregistered tool.'); break;}
          block.name = block.name.slice(PREFIX.length); external = true;
        }
        indices.set(frame.index,nextIndex++); frame.index = indices.get(frame.index); await event(frame);
      } else if (frame.type === 'content_block_delta' || frame.type === 'content_block_stop') {
        const index = indices.get(frame.index); if (index === null) continue;
        if (index === undefined) {await failure(502,'SDK emitted an invalid content block index.'); break;}
        frame.index = index; await event(frame);
      } else if (frame.type === 'message_delta') {
        Object.assign(roundUsage, frame.usage);
        delta = frame;
      } else if (frame.type === 'message_stop') {
        for (const [key,value] of Object.entries(roundUsage)) if (typeof value === 'number') usage[key] = (usage[key] ?? 0) + value;
        if (!external && (internal || request.structured) && delta?.delta?.stop_reason !== 'max_tokens') continue;
        if (!delta) {await failure(502,'SDK omitted the response stop reason.'); break;}
        await event({...delta,usage}); await event(frame); stopped = true; cancel(); break;
      } else if (frame.type === 'error') {await failure(errorStatus(frame.error),'Claude Agent SDK upstream stream failed.',sdkErrorCode(frame.error?.type)); break;}
    }
    if (!stopped && !failed) await failure(502,'Claude Agent SDK stream ended before a complete response.');
  } catch (error) {
    if (!abortController.signal.aborted) await failure(errorStatus(error), error instanceof InvalidRequest ? error.message : 'Claude Agent SDK bridge failed. Check the pinned SDK installation and selected account.');
  } finally {
    cancel();
    process.removeListener('SIGTERM',cancel); process.removeListener('SIGINT',cancel);
    await rm(directory,{recursive:true,force:true});
  }
}

async function main() {
  let bytes = '';
  for await (const chunk of process.stdin) bytes += chunk;
  const input = JSON.parse(bytes);
  const require = createRequire(join(process.argv[2] ?? process.argv[1], 'package.json'));
  const sdk = await import(pathToFileURL(require.resolve('@anthropic-ai/claude-agent-sdk')).href);
  const {McpServer} = await import(pathToFileURL(require.resolve('@modelcontextprotocol/sdk/server/mcp.js')).href);
  const {ListToolsRequestSchema, CallToolRequestSchema} = await import(pathToFileURL(require.resolve('@modelcontextprotocol/sdk/types.js')).href);
  await run(input, {...sdk,McpServer,ListToolsRequestSchema,CallToolRequestSchema});
}
if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  main().catch(() => {
    process.stdout.write(JSON.stringify({status:502}) + '\n' + JSON.stringify({type:'error',error:{type:'api_error',message:'Claude Agent SDK failed to initialize. Run llmux run --codex to install the pinned SDK.'}}));
    process.exitCode = 1;
  });
}
