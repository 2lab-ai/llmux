# Using a remote daemon

The intended topology is **one** central llmux daemon (say `llmux-host:3456`)
with every other machine running the CLI as a **pure client** of it — the CLI
analogue of what llmux Islands already does. A client never starts a local
daemon; it points `claude` at the remote proxy and presents a credential as
`x-api-key` — normally that machine's own issued client key.

Remote mode is turned on, in this precedence, by:

1. the `--remote <host[:port]>` global flag (per-invocation; `:port` defaults to
   `remote.port`, else 3456), or
2. `remote.host` in `~/.config/llmux.json`.

Neither → local mode, unchanged. One-off via the flag:

```bash
llmux --remote llmux-host:3456 run     # claude → remote proxy
llmux --remote llmux-host:3456 status  # probe the remote daemon
```

Persistently, in `~/.config/llmux.json`. The `api_key` is what the client
presents as `x-api-key`. **The standard client credential is a per-machine
issued key**: on the server run `llmux key new --name <pc> [--email …]` and
paste the `lmk-…` secret here, so usage is metered per tenant and each machine
can be suspended/rotated independently (see
[multi-tenant client keys](operational-reference.md#multi-tenant-client-keys)):

```jsonc
{
  "remote": {
    "host": "llmux-host",
    "port": 3456,
    "api_key": "lmk-…"     // this machine's issued client key
  }
}
```

Alternatively, the **remote daemon's own `proxy.api_key`** (read from the
server host's config) also works — it is the admin credential, so reserve it
for a single-owner setup where per-tenant metering and independent revocation
don't matter.

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

Full key lifecycle — issue, suspend/resume, revoke, rotate, scopes
(`default` vs `admin`), and the dashboard keys tab:
[operational-reference.md](operational-reference.md#multi-tenant-client-keys).
