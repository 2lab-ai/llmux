# Using a remote daemon

The intended topology is **one** central llmux daemon (say `llmux-host:3456`)
with every other machine running the CLI as a **pure client** of it — the CLI
analogue of what llmux Islands already does. A client never starts a local
daemon; it points `claude` at the remote proxy and presents the remote's proxy
`x-api-key`.

Remote mode is turned on, in this precedence, by:

1. the `--remote <host[:port]>` global flag (per-invocation; `:port` defaults to
   `remote.port`, else 3456), or
2. `remote.host` in `~/.config/llmux.json`.

Neither → local mode, unchanged. One-off via the flag:

```bash
llmux --remote llmux-host:3456 run     # claude → remote proxy
llmux --remote llmux-host:3456 status  # probe the remote daemon
```

Persistently, in `~/.config/llmux.json`. The `api_key` here is the **remote
daemon's** `proxy.api_key` (read from the remote host's own config), presented
as `x-api-key`:

```jsonc
{
  "remote": {
    "host": "llmux-host",
    "port": 3456,
    "api_key": "lm-…"      // the REMOTE daemon's proxy.api_key
  }
}
```

## What each command does in remote mode

In remote mode every command either **targets the remote** or **refuses
loudly** — it never silently acts on a local daemon.

| Commands | Behavior |
|---|---|
| `run`, `server`, `dashboard`, `status`, `env`, `accounts` | Target the REMOTE daemon (read/attach only). `run` exports `ANTHROPIC_BASE_URL` + `ANTHROPIC_API_KEY` (the remote key) so the off-loopback client-auth gate passes; no local daemon is started, and the proxy still swaps in the real upstream account so subscription mode is preserved at the account layer. `accounts` shows the remote's shared account pool. |
| `stop`, `restart`, `remove`, `login`, `import` | **Refused** with an error naming the remote — lifecycle and account mutation belong to the daemon's own host. Run them there, or drop `--remote` / unset `remote.host`. |
| `channel`, `update` | LOCAL and allowed — they manage THIS machine's binary install, not the daemon. |

## Transport security

Endpoints are plain `http://` and carry the proxy api_key plus prompt traffic
in the clear. Use remote mode **only over a trusted, encrypted overlay
(Tailscale / WireGuard) ONLY** — ownership is not encryption, so a LAN alone is
not enough. A TLS/HTTPS path is out of scope for
now.

## Multi-tenant client keys

For several machines sharing one daemon with per-tenant metering, issue
per-machine `lmk-` keys instead of sharing the proxy key:
[operational-reference.md](operational-reference.md#multi-tenant-client-keys).
