# Claude Agent SDK bridge

The OpenAI frontend's Claude route uses the official
`@anthropic-ai/claude-agent-sdk` **0.3.292**, including its matching native Claude
executable. It does not invoke a user's `claude` CLI or use an Anthropic HTTP
fallback. Node.js 18+ and npm must be available on the **daemon host**. The first
Claude request installs the embedded, integrity-locked dependencies using
`npm ci --ignore-scripts` into the OS cache directory under `llmux/`.
Codex-only requests do not install the SDK. A failed install is discarded and
can be retried. Installation requires npm registry access.

`LLMUX_CLAUDE_SDK_DIR` can point to a preinstalled bridge directory containing
`node_modules`; `LLMUX_NODE` selects the Node executable. Normal packaged binary
usage needs neither a source checkout nor a separately copied bridge file: the
Rust executable embeds the program, package manifest and lockfile. Dependencies
are installed on the server for remote llmux sessions, not on the Codex client.

## Request contract

Rust passes one JSON request through stdin: `{body, credential, upstream,
directory}`. `body` is an Anthropic-shaped full transcript. `credential` contains
only the selected access token/API key, never a refresh token. stdout starts
with a JSON HTTP status line followed by Anthropic SSE (or an error body).

- `query()` receives the current structured user message and replays prior
  user/assistant/image/tool blocks through its public `sessionStore` + `resume`
  API. Session entries use the pinned SDK's JSONL shape. This API is alpha;
  SDK upgrades require running the actual-SDK replay tests.
- A final client `tool_result` is included in the stored transcript **before**
  resume. `CLAUDE_CODE_RESUME_INTERRUPTED_TURN=1` continues that pending turn with
  no new streamed prompt. Passing a tool result as a fresh SDK user prompt is
  incorrect: the SDK inserts an interrupted-tool error and drops the result.
- The SDK includes its own agent identity, environment and continuation
  reminders. Caller transcript blocks retain their roles, content and IDs;
  this transport does not claim byte-identical Anthropic request prompts.
- Caller tools use an SDK-hosted MCP server with the exact caller JSON schemas.
  `mcp__llmux__` names are translated at the boundary. Handlers wait for request
  cancellation; llmux never executes the client's commands. The model's tool
  call is returned to Codex, which executes it and supplies the next result.
- `_llmux_output_format: {type:"json_schema",schema}` uses SDK `outputFormat`.
  The original schema is nested as a JSON-Schema resource under an object
  envelope because the SDK's internal tool requires object inputs. The SDK
  validates its result; the bridge unwraps and JSON-serializes `output` (including
  arrays, scalars, `false`, and `null`). Internal `StructuredOutput` calls and
  draft text are hidden; client tool calls retain their ordinary external loop.
  Local schema references keep their original resource root through `$id`.
- Model `[1m]` suffixes must reach SDK options unchanged. The SDK resolves the
  native model ID and sends its corresponding context beta header upstream.
- All built-in tools are disabled except `WebSearch` when the ingress explicitly
  requests server web search (`_llmux_web_search`). Internal WebSearch calls are
  consumed by the SDK; their output is not an external client tool call. Numeric
  usage snapshots merge within one round and sum across internal search rounds.
- `model`, `output_config.effort`, `thinking` and `max_tokens` map to SDK model,
  effort, thinking and `CLAUDE_CODE_MAX_OUTPUT_TOKENS`. Without thinking/effort,
  thinking is disabled. Auto/none tool choices are supported; forced tool choices
  must be rejected at ingress because this SDK has no equivalent control.

Each request has a private temporary cwd/HOME/config. Filesystem settings,
plugins, skills and hooks are disabled. The SDK receives an environment
allowlist plus the selected account; inherited proxy/auth/Node injection settings
are not passed. User account configuration is never read by the SDK. Temporary
transcripts are removed on completion. Rust owns an isolated process group and
kills Node and its SDK children when the client drops its response body, including
while waiting for the first byte. The npm installation process has the same
cancellation ownership. On a completed external tool boundary the bridge kills
the SDK-owned native process before returning from the async iterator: its
graceful close otherwise converts canceled permission requests into tool errors
and can start additional inference. Error output never includes raw SDK stderr or credentials.

## Verification

```sh
cd bridge
npm ci --ignore-scripts --no-audit --no-fund
npm test
```

Unit tests cover environment isolation, structured replay, usage, termination,
name mapping and internal search. Integration tests run the **actual pinned SDK
and native executable** against a local mock Anthropic endpoint: schema
preservation, no built-ins/extra inference, external tool call IDs, full ordered
user/assistant/tool-result/image replay, and HTTP authentication failure mapping.
They do not use a real account. To use an existing install for tests, set
`LLMUX_CLAUDE_SDK_DIR` to its directory.

Primary API evidence: the pinned package's `sdk.d.ts` (`Options.env`, `tools`,
`settingSources`, `SessionStore.load`, `SDKUserMessage`, and
`CLAUDE_CODE_RESUME_INTERRUPTED_TURN` descriptions), and the official
[SDK changelog](https://github.com/anthropics/claude-agent-sdk-typescript/blob/main/CHANGELOG.md).
