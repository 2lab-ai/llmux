# Using a remote daemon

Remote mode lets several machines belonging to one person use one central
llmux daemon. Client commands attach to that daemon instead of starting a
local one.

```text
client Claude Code ── encrypted overlay ──> llmux-host:3456 ──> providers
client CLI/TUI     ── encrypted overlay ──┘
```

## Security requirement

llmux serves HTTP and binds on all interfaces. `proxy.api_key` authenticates
off-loopback requests, but it does not encrypt prompts or the key itself. Use
remote mode only through a trusted encrypted overlay such as Tailscale or
WireGuard. A normal LAN is not sufficient.

This is a single-user topology, not a multi-tenant deployment pattern.

## Daemon host

Install llmux, add accounts, and start the daemon on the central host:

```bash
llmux server
```

The host config contains the generated `proxy.api_key`. Protect
`~/.config/llmux.json`; clients need the key value, never the whole credential
file.

Verify locally on the host before connecting remotely:

```bash
llmux status
```

## One-off client

The global flag overrides persistent remote settings:

```bash
llmux --remote llmux-host:3456 status
llmux --remote llmux-host:3456 run
```

The API key still comes from `remote.api_key` in the client config.

## Persistent client

On each client machine, store only the remote connection:

```json
{
  "remote": {
    "host": "llmux-host",
    "port": 3456,
    "api_key": "lm-..."
  }
}
```

Then normal client commands target the remote:

```bash
llmux status
llmux dashboard
llmux run
```

`llmux run` exports the remote base URL and, when configured, its key to Claude
Code; with no remote key it removes an inherited key and warns. It never starts
a local daemon in remote mode.

## Command behavior

| Commands | Remote behavior |
| --- | --- |
| `run`, `server`, `dashboard`, `status`, `env`, `accounts` | Target or attach to the remote daemon; they do not bind locally. |
| `stop`, `restart`, `remove`, `login`, `import` | Refused. Lifecycle and credential mutation belong on the daemon host. |
| `channel`, `update` | Stay local because they manage the client machine's installed binary. |
| `api PATH` | Stays local and directly uses a credential from the client machine's config; it is a low-level upstream debug command, not a remote-daemon operation. |

The refusal is deliberate: remote mode never silently changes a different
machine's credentials or lifecycle.

## Islands

Both native shells can connect to a remote daemon. Configure the host, port,
and control key in the app. Remote Islands endpoints require HTTPS at the app
boundary; use an HTTPS endpoint provided by your trusted overlay or reverse
proxy. Redirects are denied. Loopback HTTP remains allowed.

## Troubleshooting

1. Confirm `llmux status` works on the daemon host.
2. Confirm the overlay resolves/reaches `llmux-host:3456`.
3. Confirm the client's `remote.api_key` exactly matches the host's
   `proxy.api_key`.
4. Check that you did not put a remote account credential in `remote.api_key`;
   it is the daemon control key.
5. Run `llmux --remote host:port status` to separate persistent-config issues
   from network issues.

See [Configuration](../configuration.md) for the complete `remote` schema and
[Architecture](../architecture.md#network-and-trust-boundary) for the trust
model.
