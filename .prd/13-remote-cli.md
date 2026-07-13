# 13 — Remote-first CLI (one central daemon, many client machines)

Owner intent (Z, 2026-07-14, verbatim clarification): *"원래 llmux server를 하나만 설정하고
여러 서비스에서 llmux를 로컬 서버가 아니라 리모트를 사용하려고 한 것. 옵션이 있거나
llmux.json에 remote 설정이 되어 있으면 로컬에서 서버를 안 띄우는 게 원래 의도."*

Topology: **one** central llmux daemon (e.g. `llmux-host:3456`); every other machine and
service runs the llmux CLI as a **pure client** of that daemon. Supersedes the scope of
PR #74 (held on external review — see MUST-FIX disposition below).

## Activation & precedence

Remote mode is ON for an invocation when, in this order:

1. `--remote <host[:port]>` global flag (per-invocation override; `:port` defaults to
   config `remote.port`, else 3456; api_key from config `remote.api_key`), or
2. config `remote.host` is set (`~/.config/llmux.json`, port from `remote.port` else 3456,
   key from `remote.api_key`).

Neither → local mode on `proxy.port` with `proxy.api_key` (unchanged behavior; a config
written before this feature loads with remote OFF — `#[serde(default)]`, additive).

`resolve_endpoint()` in `src/cli/mod.rs` is the single chokepoint; every client command
routes through it.

## Command matrix (remote mode)

| Command | Behavior in remote mode |
|---|---|
| `run` | Point `claude` at the remote proxy (`ANTHROPIC_BASE_URL`) and export `ANTHROPIC_API_KEY` = `remote.api_key` (off-loopback proxy gate). **Never auto-starts a local daemon.** Warn (don't fail) when `remote.api_key` is unset. |
| `server` | Never binds locally — attaches to the remote dashboard (CLI twin of llmux-islands). Non-TTY prints a reachability one-liner. |
| `dashboard` | Attach to the remote dashboard. |
| `status` | Probe + render the remote daemon. |
| `env` | Print `ANTHROPIC_BASE_URL` (+`ANTHROPIC_API_KEY`) for the remote. |
| `accounts` | List the REMOTE daemon's accounts (read-only view of the shared pool). |
| `stop` / `restart` | **Refuse explicitly** — lifecycle belongs to the daemon's own host. Error names the remote and says: run it there, or drop `--remote` / unset `remote.host`. Never silently act on a local daemon. |
| `remove` / `login` / `import` | **Refuse explicitly** — they mutate the local daemon's account config, meaningless on a pure client. Same error shape. |
| `channel` / `update` | LOCAL, allowed — they manage THIS machine's binary install, not the daemon. |
| `api` | Unchanged (debug GET against the *upstream* using local config accounts; a pure client with no accounts gets the existing "no accounts configured" error naturally). |

Design rule: in remote mode a command either **targets the remote** or **refuses loudly**.
Silently operating on the local daemon is the defect class this spec forbids (external
review MUST-FIX #1 on PR #74).

## Probe / auth

- `probe_server(base_url, api_key)` takes a full base URL (local or remote).
- HTTP 401 from a llmux-shaped endpoint classifies as `ServerProbe::Unauthorized`
  (distinct from `Foreign`), and error messages point at `remote.api_key` (remote) or
  `proxy.api_key` (local).

## Transport constraint (external review MUST-FIX #4)

Endpoints are **plain `http://`** and carry the proxy api_key plus prompt traffic.
Documented constraint: use remote mode only over a trusted, encrypted overlay (Tailscale /
WireGuard / LAN you own). A TLS/HTTPS path is out of scope here and tracked for stable.

## UX deliverables

- `--remote` flag help on the top-level CLI (+ `llmux --help` after-help block showing the
  config snippet and one-liner examples).
- README "Using a remote daemon" guide: flag, config example, command matrix summary,
  security note.

## Excluded from this change (vs old PR #74)

- `.prd/13-usage-raw-sources.md`, `.prd/token-dashboard-62/*`, `.prd/islands-todo/*`,
  `.prd/email-anon-link/*`, `todo.md`, `screenshots/llmux-demo.gif` — unrelated working
  docs bundled by the old branch (review MUST-FIX #3); left on `feat/cli-remote-daemon`.
- Kept from the old branch: the src changes, README section (rewritten), preview.yml tap
  auto-dispatch, `.gitignore` dist-backup line.
