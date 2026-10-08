# Using a remote daemon

Optional: one main computer runs the daemon and holds the accounts; your other
computers connect as clients. Per-computer usage shows up separately on the
daemon's keys tab.

The default is a local daemon on every computer, and nothing on this page
applies until you turn remote mode on. In remote mode there is **one** llmux
daemon (say `llmux-host:3456`) and every other computer runs the CLI as a
**pure client** of it. A client never starts a local daemon; `run` points
Claude Code at the remote Messages endpoint and `run --codex` points Codex at
remote Responses. Claude Code uses `x-api-key`; Codex uses Bearer
authentication with the same issued client key. llmux Islands can also connect
to a remote daemon, but it has extra requirements (see
[Transport security](#transport-security)).

Remote mode is turned on, in this precedence, by:

1. the `--remote <host[:port]>` global flag (per-invocation; `:port` defaults to
   `remote.port`, else 3456), or
2. `remote.host` in `~/.config/llmux.json`.

Neither → local mode, unchanged. One-off via the flag:

```bash
llmux --remote llmux-host:3456 run     # claude → remote proxy
llmux --remote llmux-host:3456 run --codex  # codex → remote Responses
llmux --remote llmux-host:3456 status  # probe the remote daemon (admin-scope key)
```

Persistently, in `~/.config/llmux.json`. The `api_key` is what the client
presents as `x-api-key` (Claude/control API) or Bearer (Codex Responses). **The
standard client credential is a per-computer issued key**: on the server run
`llmux key new --name <pc> [--email …]` and paste the `lmk-…` secret here, so
usage is metered per computer and each computer can be suspended or rotated
independently (see
[per-computer client keys](operational-reference.md#multi-tenant-client-keys)):

```jsonc
{
  "remote": {
    "host": "llmux-host",
    "port": 3456,
    "api_key": "lmk-…"     // this machine's issued client key
  }
}
```

A default issued key reaches the data plane only (`/v1/*` and `/models`). That
covers `run`, `run --codex`, and `env`. `dashboard`, `status`, and `accounts`
read the daemon's `/llmux/*` control plane and need an admin-scope credential:
a key issued with `--admin`, or the remote daemon's own `proxy.api_key` (read
from the server host's config). The `proxy.api_key` is the daemon's admin
credential, so reserve it for a setup where per-computer metering and
independent revocation don't matter.

## What each command does in remote mode

In remote mode every command either **targets the remote** or **refuses
loudly** — it never silently acts on a local daemon.

| Commands | Behavior |
|---|---|
| `run`, `env` | Target the REMOTE data plane; a default issued key is enough. `run` exports `ANTHROPIC_BASE_URL` + `ANTHROPIC_API_KEY` (the remote key) so the off-loopback client-auth gate passes; no local daemon is started, and the proxy still swaps in the real upstream account so subscription mode is preserved at the account layer. `env` prints the same exports for your shell. `llmux env --codex` prints `OPENAI_BASE_URL` / `OPENAI_API_KEY` for the selected endpoint — **preview channel only as of 2026-10-08 (absent from stable 0.2.24)**; see [release availability](operational-reference.md#release-availability). |
| `run --codex` | Launch the local Codex CLI against remote `/v1/responses`, using the configured remote key; a default issued key is enough. Picker metadata comes from that daemon; user Codex configuration files are not changed. No local daemon is started or restarted, including with `--force`. |
| `dashboard`, `status`, `accounts`, `server` (attach) | Target the REMOTE control plane (`/llmux/*`; reads plus the attach-mode mutations: manual switch, usage refresh, scheduler mode, config-tab settings), which requires an admin-scope credential: a key issued with `--admin`, or the daemon's own `proxy.api_key`. A default `lmk-` key gets 401/403 on these. `accounts` shows the remote's shared account pool. |
| `stop`, `restart`, `remove`, `login`, `import` | **Refused** with an error naming the remote — lifecycle and account mutation belong to the daemon's own host. Run them there, or drop `--remote` / unset `remote.host`. The one browser-login path that does work from here is the attached dashboard's `n` picker: the OAuth flow runs in THIS client and the minted credential is relayed to the remote daemon over `POST /llmux/inject-account`, which needs an admin credential. |
| `channel`, `update` | LOCAL and allowed — they manage THIS machine's binary install, not the daemon. |

Install Codex CLI on each client that uses `run --codex`. Claude models on this
frontend require Node.js 18+ and npm on the **remote daemon host**, where the pinned
SDK is installed on first request. `llmux env` exports the Claude Code
variables; `llmux env --codex` (preview only, see above) exports the OpenAI
pair. Invalid explicit OpenAI credentials are rejected even on loopback; remote
requests require a valid key. See [Codex frontend](codex-frontend/README.md).

## Transport security

Endpoints are plain `http://` and carry the api_key plus prompt traffic in the
clear. Use remote mode **only over a trusted, encrypted overlay (Tailscale /
WireGuard)** — ownership is not encryption, so a LAN alone is not enough.
llmux does not terminate TLS.

The CLI has no HTTPS requirement. The llmux Islands app does: it enforces HTTPS
for any non-loopback endpoint and denies redirects. That is an app rule, not a
daemon feature. To connect Islands to a remote daemon, put an HTTPS reverse
proxy in front of the daemon and give Islands an admin-scope key (a key issued
with `--admin`, or the daemon's `proxy.api_key`). Project launch from Islands
is local-only.

## Multi-tenant client keys

In practice this means per-computer attribution for one person's machines;
llmux is not for sharing accounts across people (see the README's [Compliance & caveats](../README.md#compliance--caveats)).

Full key lifecycle — issue, suspend/resume, revoke, rotate, scopes
(`default` vs `admin`), and the dashboard keys tab:
[operational-reference.md](operational-reference.md#multi-tenant-client-keys).
