# llmux web dashboard (`webui/`)

Admin-only web view of one llmux daemon: sessions as task cards, per-user (issued key) usage, requests and accounts.
Contract: [`.prd/26-web-session-dashboard.md`](../.prd/26-web-session-dashboard.md). User guide: [`docs/web-dashboard.md`](../docs/web-dashboard.md).
See it: ▶ [llmux film, 48 s](https://github.com/2lab-ai/llmux/releases/download/v0.2.26/llmux-intro-en.mp4) (accounts, requests, the raw viewer; [Korean](https://github.com/2lab-ai/llmux/releases/download/v0.2.26/llmux-intro-ko.mp4)).

- `server.mjs` — Node ≥ 20, no dependencies. Reads the daemon admin API with `proxy.api_key` from `~/.config/llmux.json`
  (server side only, never sent to the browser), replays `activity.jsonl`, reads Claude Code transcripts on this host.
- `lib/sessions.mjs` — session index (owner = key, origin, side-call folding, Q4 flags, users view).
- `lib/rawmeta.mjs` — what a raw-io request says about a remote Claude Code session (cwd, branch, version, OS, MCP, prompts).
- `lib/overview.mjs` — instructions → tools, files, requests, cost.
- `lib/pricing.mjs` — per-token rates solved from the daemon's `usage_stats`.
- `public/app.html`, `app.js`, `app.css` — the page, built from `@2lab.ai/ui` elements served at `/ui/*` from `UI_DIST`
  (default: the host's soma-work install; falls back to jsDelivr `@2lab.ai/ui@0.39.0`). `core.js`/`core.css` keep the
  activity and account renderers. `ccview.js`/`ccview.css` draw the Claude Code screen; `rawview.js` is the raw viewer
  (`GET /api/raw-full`). Requests page through `GET /api/session/:sid/requests?limit=&kind=` with cursor `before=<at_ms:id>` (the next
  older page, loaded as the list scrolls) or `after=<at_ms:id>` (new requests of a running session); `offset=` still works.

Run locally: `PORT=4780 node server.mjs` (no auth, 127.0.0.1). Behind the soma-work login: `deploy/` (LaunchAgent + Caddy).
Tests: `node --test webui/test/`.

| env | default | |
|---|---|---|
| `PORT` / `BIND` | `4780` / `127.0.0.1` | wildcard binds are refused |
| `LLMUX_URL` | `http://127.0.0.1:3456` | daemon |
| `LLMUX_CONFIG` | `~/.config/llmux.json` | admin key source |
| `LLMUX_ACTIVITY` | `~/.local/state/llmux/activity.jsonl` | history replay (last 160 MB) |
| `SOMA_CLAUDE_ROOT` | `/opt/soma-work/dev/.claude/projects` | soma-work transcripts (origin = soma-work) |
| `TRANSCRIPT_ROOTS` | `~/.claude/projects,$SOMA_CLAUDE_ROOT` | transcript search path |
| `UI_DIST` | `/opt/soma-work/dev/node_modules/@2lab.ai/ui/dist` | `@2lab.ai/ui` build |
| `AUTH=soma`, `SOMA_AUTH_ME`, `SSO_ORIGIN`, `PUBLIC_ORIGIN`, `COOKIE_DOMAIN` | off | soma-work single sign-on (admins only) |
