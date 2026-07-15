# Getting started

This path installs the `llmux` CLI/daemon, adds one account, launches Claude
Code through the proxy, and verifies the result. The native Islands UI is
optional and comes afterward.

## 1. Install the CLI

### macOS

Stable:

```bash
brew install 2lab-ai/tap/llmux
```

Rolling preview:

```bash
brew install 2lab-ai/tap/llmux-preview
```

The active channel is derived from the installed Homebrew formula. After
installation, `llmux channel` prints it, `llmux update` upgrades it, and
`llmux channel stable|preview` switches it.

### Linux

Stable releases publish native binaries for Linux x86_64 and aarch64. The
following installs the matching asset and verifies it against the release
checksum manifest:

```bash
case "$(uname -m)" in
  x86_64) asset=llmux-linux-x86_64 ;;
  aarch64|arm64) asset=llmux-linux-aarch64 ;;
  *) echo "unsupported prebuilt architecture: $(uname -m)" >&2; exit 1 ;;
esac

tmpdir="$(mktemp -d)"
cd "$tmpdir"
curl -fLO "https://github.com/2lab-ai/llmux/releases/latest/download/$asset"
curl -fLO "https://github.com/2lab-ai/llmux/releases/latest/download/SHA256SUMS"
grep " $asset$" SHA256SUMS | sha256sum -c -
sudo install -m 0755 "$asset" /usr/local/bin/llmux
llmux --version
```

Preview prereleases contain the same four CLI architectures, but they are not
the target of GitHub's `/releases/latest` URL. Select the newest `preview-*`
release explicitly if you want that channel on Linux.

### Build from source

Install the stable Rust toolchain, then:

```bash
git clone https://github.com/2lab-ai/llmux.git
cd llmux
CARGO_BUILD_JOBS=2 cargo build --release --locked
sudo install -m 0755 target/release/llmux /usr/local/bin/llmux
```

`CARGO_BUILD_JOBS=2` is intentional for local development: it caps compilation
parallelism instead of occupying every CPU core. Release CI builds macOS and
Linux artifacts on GitHub-hosted runners.

## 2. Add an account

Start with one Claude subscription:

```bash
llmux login
```

The browser OAuth flow stores the resulting credential in
`~/.config/llmux.json` (or `$XDG_CONFIG_HOME/llmux.json`) with private file
permissions. Repeat the command for additional Claude accounts.

Optional account types:

```bash
llmux login --api    # paste an Anthropic API key
llmux login --codex  # ChatGPT/Codex browser OAuth
llmux login --grok   # xAI device-code OAuth
llmux import         # supported local credential stores
```

List what was added without printing secrets:

```bash
llmux accounts
```

## 3. Send the first request

```bash
llmux run
```

This command:

1. probes the configured daemon endpoint;
2. starts a detached local daemon when none is ready;
3. exports the llmux base URL to the child process; and
4. launches `claude`, passing through arguments after `--`.

If you prefer to see the TUI in the foreground, run `llmux server` in one
terminal and `llmux run` in another. Manual shell wiring also works:

```bash
eval "$(llmux env)"
claude
```

## 4. Verify the daemon

```bash
llmux status
llmux accounts --verbose
```

Expected state:

- the daemon reports running on port `3456` unless you changed it;
- at least one configured account appears;
- each configured provider group has an eligible current account after its
  first request or usage observation; and
- `llmux dashboard` can attach without binding a second server.

If the daemon is unhealthy, `llmux restart` performs a cooperative stop and
starts the installed binary again.

## 5. Select a backend

Inside Claude Code:

```text
/model fable
/model gpt-5.6-sol
/model grok
```

The string selects an account group. For Responses-family providers, llmux
then resolves or pins the upstream model according to live configuration. See
[Models](models.md) for aliases and [Configuration](configuration.md) for
routing and request-shaping rules.

## 6. Optional native UI

- macOS: `brew install 2lab-ai/tap/llmux-islands`
- Arch/KDE: build the repository package with `makepkg -si`

The [llmux Islands guide](llmux-islands.md) contains both install paths,
privacy controls, remote setup, and real platform screenshots.

## Next steps

- Read [How llmux works](concepts.md) before changing scheduler or routing
  settings.
- Keep the [Operational reference](operational-reference.md) nearby for TUI
  keys and lifecycle commands.
- Use [Remote daemon](guides/remote-daemon.md) only over a trusted encrypted
  overlay.
- Check the [FAQ](faq.md) for installation, context-window, and connectivity
  issues.
