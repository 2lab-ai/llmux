# llmux CD reference (shared procedures)

Not an invokable skill — shared mechanics for the `build` / `deploy` / `release` runbooks.
Release topology and Codex frontend procedures checked against the repository workflows on 2026-10-07.

## Topology

- Repo: `2lab-ai/llmux`, default branch `main`. 4-target build matrix
  (macos aarch64/x86_64, linux aarch64/x86_64).
- `.github/workflows/preview.yml` — on **push to main** → prerelease
  `preview-<YYYY-MM-DD-HHMM>-<sha12>` (4 binaries + SHA256SUMS).
- `.github/workflows/release.yml` — on **push of tag `v*`** → verifies tag == `Cargo.toml`
  version, then a stable release `v<x.y.z>`.
- Tap: `2lab-ai/homebrew-tap` (tapped as `2lab-ai/tap`), two formulae: `llmux` (stable,
  from latest `v*`) and `llmux-preview` (from latest `preview-*`). The tap's `bump.yml`
  renders formulae from release assets on `workflow_dispatch` or a 6h schedule.
  **Preview publication directly bumps the preview formula and Islands cask in its
  own required `publish` step** (`.github/scripts/bump-tap-preview.sh`); a missing
  token or failed push fails publication. Stable releases still need the tap workflow.
- Local daemon: `/opt/homebrew/bin/llmux server --no-tui`, control port 3456. The PATH
  binary is a brew symlink into the Cellar.

## Procedure A — hot-deploy a local build + restart

The Cellar binary is `r-xr-xr-x` (read-only); `cp` over it gives EACCES. Remove first.

```bash
cargo build --release --locked
target="$(readlink -f /opt/homebrew/bin/llmux)"   # resolve symlink → Cellar file
rm -f "$target"
cp target/release/llmux "$target"
chmod 755 "$target"
/opt/homebrew/bin/llmux restart                   # drains old daemon, respawns from current_exe()
/opt/homebrew/bin/llmux --version                 # local build reports "(dev dev)"
```

Restart is safe when `llmux status` shows `in_flight: 0` across accounts.

## Procedure B — publish brew formula + verify it landed

For a preview, first verify the successful preview `publish` job and that the tap
points to its exact tag; it already performed the bump. For a stable release,
dispatch the tap workflow and wait. Dispatch for a preview only as recovery for a
verified stale tap, after diagnosing the failed publish step. Then upgrade.

```bash
formula=llmux-preview   # or: llmux
# Stable only, or diagnosed preview recovery:
# gh workflow run bump.yml --repo 2lab-ai/homebrew-tap
# Identify that exact dispatched run, then gh run watch --repo 2lab-ai/homebrew-tap <run-id> --exit-status
brew update
brew upgrade "$formula" || brew install "2lab-ai/tap/$formula"
brew info --json=v2 "$formula" | python3 -c 'import json,sys;print(json.load(sys.stdin)["formulae"][0]["installed"][0]["version"])'
/opt/homebrew/bin/llmux --version   # expect "(preview <id>)" or "(stable <id>)"
```

After `brew upgrade` the new binary is already in the Cellar, so "hot-deploy" reduces to
`/opt/homebrew/bin/llmux restart` (no rm/cp needed — that path is only for a local
`target/release` build).

## Frontend packaging and installed verification

Rust `include_str!` embeds `bridge/claude-agent.mjs`, `bridge/package.json` and
`bridge/package-lock.json`; filtered source archives, Docker contexts and fixture
git clones must include all three. Do not bundle `node_modules`. For bridge/runtime
changes run `npm ci --ignore-scripts --no-audit --no-fund` in `bridge/`, then
`just check-bridge` as well as the required `just check`. CI runs the SDK tests.

Claude-through-Codex needs Node.js 18+ and npm on the daemon host; first use installs
the pinned SDK from embedded assets. A normal packaged smoke must not depend on a
source checkout or `LLMUX_CLAUDE_SDK_DIR`. Codex CLI belongs on the client host.
For a frontend/runtime release, use the **installed** `llmux run --codex` to prove
text plus an actual client-tool roundtrip against both a Codex and a Claude model.
Record command exit, returned tool marker, client/server build equality and
OpenAI-origin Activity. Account-policy fixes additionally need same-request bad-to-
healthy failover evidence; a healthy first selection does not prove failover.

Docs-only followups to an already verified release retain the deployed artifact.
`preview.yml` has no path filter: put `[skip ci]` in both the documentation commit
and squash-merge message to avoid a duplicate unchanged runtime preview. Still run
`just check` before commit and obtain document review; do not rebuild/restart solely
to publish documentation.

## Push auth fallback

The git remote may embed a short-lived `ghs_` token. If `git push` fails, push with the
authed `gh` token (scopes `repo`,`workflow`):

```bash
git push "https://x-access-token:$(gh auth token)@github.com/2lab-ai/llmux" <ref>
```

## Pitfalls

- `gh release view` (no tag) returns the latest **stable** release — it hides prereleases.
  Use `gh release list` / an explicit `--tag` to see `preview-*`.
- "Already up-to-date" from `brew upgrade` after a bump usually means a stale index — run
  `brew update` first, then re-check `brew info` version.
- `brew upgrade` clobbers any hot-deployed local (`dev dev`) binary — intended for
  deploy/release (we want the brew build); re-run `build` to restore a dev binary.
- Release tag must equal `Cargo.toml` version or the workflow fails the build.
- CI builds 4 targets — minutes, not seconds. Poll with `gh run watch`, don't assume.
