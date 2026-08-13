# The accidental AI debugger

llmux was built as a router. But once every **model request** your agent makes
flows through `localhost:3456`, and llmux keeps the raw bytes of both halves of
every exchange, you get something you didn't install it for: **DevTools for your
agent's model traffic**.

![llmux raw viewer demo — from the live activity feed into the raw request/response viewer: request body, then the Response tab with the SSE stream and rate-limit headers](../screenshots/llmux-raw-viewer-demo.gif)

[Original raw-viewer recording (mp4)](../screenshots/llmux-raw-viewer-demo.mp4)

- **Live per-request receipts.** The activity feed prints one row per completed
  request: what it was (`user` turn, `subagent`, `security` pass, `compact`,
  `count`, …), model + effort, serving account, status, latency, tokens,
  API-equivalent cost, and a session-tagged input preview. "Which subagent just
  burned 236k tokens on that turn" has a one-glance answer.
- **A DevTools-style raw viewer.** Open any request (`🔍 request` on an expanded
  row) into a modal over the dashboard. A translated codex/grok exchange shows all
  four legs of the wire — `Request` (what Claude Code sent) → `Upstream Req` (what
  llmux rewrote it into) → `Upstream Resp` (the provider's verbatim reply) →
  `Response` (what your client received). Headers, SSE events, rate-limit state,
  request bodies: scroll, pan, and read the actual bytes.
- **Copy as curl.** One keypress reconstructs a `curl` for a tab's side of the
  exchange — credential values stay `•••redacted`, so substitute your own before
  replaying. Raw bodies copy to the clipboard, and `save all` writes the whole
  record JSON to `~/Downloads`. A provider bug report with the exact failing frame
  attached is one keypress away.
- **Wire truth, archived.** Captures persist to `raw-io.jsonl`, and the repo's
  [captured system prompts](system-prompts/) were taken from this same wire —
  what Claude Code actually injects per model, not what the docs say it injects.
- **Email masking for screen shares.** `email_anonymous` masks every account email
  behind a fixed pool of famous-CS-name fakes across the TUI and Islands, and
  credential header values (`authorization`, keys/tokens/cookies) are redacted at
  capture time. Raw payloads still contain whatever your session contained — treat
  the viewer's body panes accordingly. The recordings on this page were taken live
  with masking on, with remaining identifiers pixelated in post.

![llmux usage & cost tab and raw request headers — calendar cost buckets per model, failure states surfaced honestly in the status banner, and the request general/headers view](../screenshots/llmux-usage-raw-demo.gif)

[Original usage-tab recording (mp4)](../screenshots/llmux-usage-raw-demo.mp4)

The kinds of questions this answers in seconds, because the evidence is already on
screen: why is this request 428 KB; did `context_management` edits actually apply
upstream; what did the provider really return before the adapter converted it;
which account is about to hit its 5-hour window, and why did the scheduler switch.
Debugging an agent stack without seeing the wire is guesswork — this makes the wire
a first-class surface.

Viewer key reference: [operational-reference.md](operational-reference.md#raw-requestresponse-viewer).
