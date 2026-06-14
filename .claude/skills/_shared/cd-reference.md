# teamagent CD reference (shared procedures)

Not an invokable skill — shared mechanics for the `build` / `deploy` / `release` runbooks.
All facts verified 2026-06-14.

## Topology

- Repo: `2lab-ai/teamagent`, default branch `master`. 4-target build matrix
  (macos aarch64/x86_64, linux aarch64/x86_64).
- `.github/workflows/preview.yml` — on **push to master** → prerelease
  `preview-<YYYY-MM-DD-HHMM>-<sha12>` (4 binaries + SHA256SUMS).
- `.github/workflows/release.yml` — on **push of tag `v*`** → verifies tag == `Cargo.toml`
  version, then a stable release `v<x.y.z>`.
- Tap: `2lab-ai/homebrew-tap` (tapped as `2lab-ai/tap`), two formulae: `teamagent` (stable,
  from latest `v*`) and `teamagent-preview` (from latest `preview-*`). The tap's `bump.yml`
  renders formulae from release assets and runs on **`workflow_dispatch` or a 6h schedule —
  NOT instantly on release**. Trigger it explicitly for a prompt brew update.
- Local daemon: `/opt/homebrew/bin/teamagent server --no-tui`, control port 3456. The PATH
  binary is a brew symlink into the Cellar.

## Procedure A — hot-deploy a local build + restart

The Cellar binary is `r-xr-xr-x` (read-only); `cp` over it gives EACCES. Remove first.

```bash
cargo build --release --locked
target="$(readlink -f /opt/homebrew/bin/teamagent)"   # resolve symlink → Cellar file
rm -f "$target"
cp target/release/teamagent "$target"
chmod 755 "$target"
/opt/homebrew/bin/teamagent restart                   # drains old daemon, respawns from current_exe()
/opt/homebrew/bin/teamagent --version                 # local build reports "(dev dev)"
```

Restart is safe when `teamagent status` shows `in_flight: 0` across accounts.

## Procedure B — publish brew formula + verify it landed

The tap bump is not automatic. Dispatch it, wait, then upgrade. Use `teamagent-preview`
for a deploy, `teamagent` for a release.

```bash
formula=teamagent-preview   # or: teamagent
gh workflow run bump.yml --repo 2lab-ai/homebrew-tap
sleep 5
rid=$(gh run list --repo 2lab-ai/homebrew-tap --workflow bump.yml -L1 --json databaseId -q '.[0].databaseId')
gh run watch --repo 2lab-ai/homebrew-tap "$rid" --exit-status
brew update
brew upgrade "$formula" || brew install "2lab-ai/tap/$formula"
brew info --json=v2 "$formula" | python3 -c 'import json,sys;print(json.load(sys.stdin)["formulae"][0]["installed"][0]["version"])'
/opt/homebrew/bin/teamagent --version   # expect "(preview <id>)" or "(stable <id>)"
```

After `brew upgrade` the new binary is already in the Cellar, so "hot-deploy" reduces to
`/opt/homebrew/bin/teamagent restart` (no rm/cp needed — that path is only for a local
`target/release` build).

## Push auth fallback

The git remote may embed a short-lived `ghs_` token. If `git push` fails, push with the
authed `gh` token (scopes `repo`,`workflow`):

```bash
git push "https://x-access-token:$(gh auth token)@github.com/2lab-ai/teamagent" <ref>
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
