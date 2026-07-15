# FAQ

## Does llmux replace Claude Code?

No. Claude Code remains the harness that owns tools, context, permissions,
hooks, subagents, and repository conventions. llmux sits behind it as the
account/model traffic layer.

## Can I use only Claude accounts?

Yes. Codex and Grok are optional. Multi-account Claude scheduling and
Anthropic API-key fallback are first-class paths.

## Why cannot xbrew install llmux Islands on Arch?

No Linux Islands recipe is published to xbrew, pacman, or the AUR yet. The
macOS cask names do not imply Linux package availability, so these fail:

```text
xbrew install llmux-islands
xbrew install llmux-islands-preview
```

Install the repository Arch package:

```bash
sudo pacman -S --needed base-devel git
git clone --depth=1 https://github.com/2lab-ai/llmux.git
cd llmux/llmux-islands-linux/packaging/arch
CARGO_BUILD_JOBS=2 MAKEFLAGS=-j2 makepkg -si
llmux-islands-linux
```

The package is `llmux-islands-git`; the executable is
`llmux-islands-linux`. See [llmux Islands](llmux-islands.md#arch-linux-kde).

## Why did an Islands build use every CPU core?

Cargo and native Qt/C++ compilation parallelize by default. Container builds
can add another parallel layer. For a normal Arch install, avoid the
maintainer-only Docker path and cap the package build:

```bash
CARGO_BUILD_JOBS=2 MAKEFLAGS=-j2 makepkg -si
```

For a direct Rust build:

```bash
cd llmux/llmux-islands-linux
CARGO_BUILD_JOBS=2 QMAKE=/usr/bin/qmake6 cargo build --release --locked
```

GitHub Actions runs the clean-Arch Docker parity build; local users do not need
it to install or launch the app.

## What does `[1m]` do on a Codex model name?

`[1m]` is a Claude Code-facing model annotation. It affects the client's
context presentation; it does not enlarge a Codex model's real upstream
window.

There are two separate effects:

1. `gpt-*` still classifies to the Codex account group, even with the suffix.
2. The decorated string `gpt-5.5[1m]` is not a known Codex passthrough ID, so
   the provider falls back to `codex.default_model` — currently
   `gpt-5.6-sol` — rather than sending literal `gpt-5.5[1m]` upstream.

Use bare `/model gpt-5.5` when exact gpt-5.5 passthrough is required. Use
`/model gpt-5.6-sol` for the default flagship. If client-side context
presentation must differ from the upstream name, review `codex.client_model`
and set an honest auto-compaction threshold; do not treat the displayed window
as an upstream guarantee.

## A long Codex session stops near its real context limit. What should I do?

Claude Code owns local compaction. One practical recovery is to compact on a
known Claude 1M model, then return to the exact Codex model:

```text
/model opus[1m]
/compact
/model gpt-5.5
```

This reduces the active transcript. It does not change llmux accounts or the
upstream Codex window. Current known context sizes and model IDs are in
[Models](models.md).

## Why does a known Codex model sometimes differ from the configured pin?

The requested model is honored when it is a known accepted concrete ID.
`sol`/`terra`/`luna` aliases resolve to the current `gpt-5.6-*` generation and
bare `gpt-5.6` resolves to `gpt-5.6-sol`. Only absent, decorated, or unknown
strings use `codex.default_model`.

See [Operational routing](operational-reference.md#routing-and-upstream-models).

## Where are config, logs, and captured prompts stored?

- Config/credentials: `~/.config/llmux.json` by default.
- Daemon log: `$XDG_STATE_HOME/llmux/server.log`.
- Activity metadata: `$XDG_STATE_HOME/llmux/activity.jsonl`.
- Raw payload capture: `$XDG_STATE_HOME/llmux/raw-io.jsonl`.
- Codex/Grok Responses trace: `$XDG_STATE_HOME/llmux/codex-trace.jsonl`.

When `$XDG_STATE_HOME` is unset, the usual base is `~/.local/state`, so the
files live under `~/.local/state/llmux/`.

`raw_io` is enabled by default with 90-day pruning and may contain sensitive
prompt/response bodies. Review or disable it in
[Configuration](configuration.md#raw-io-and-traces).

## `llmux run` works, but `llmux dashboard` cannot attach

First check:

```bash
llmux status
```

If a local target is down, run `llmux restart`. If another program owns port
3456, llmux will not kill it; change the port or stop that program. For a
remote target, `restart` is intentionally refused: confirm the host, overlay
route, and `remote.api_key`, then run status/restart on the daemon host.

## Can Islands connect to an HTTP remote daemon?

No. Both native shells allow HTTP for loopback, but remote endpoints must use
HTTPS and an API key; redirects are denied. The CLI remote mode can use HTTP
only because it is intended to run inside a trusted encrypted overlay such as
Tailscale or WireGuard.

## Is llmux for sharing accounts across a team?

No. llmux is for one human using their own accounts. It is not multi-tenant
isolation, credential pooling, resale, or shared subscription brokerage.
