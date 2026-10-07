---
name: release
description: Use when the user says "릴리즈", "릴리즈해줘", "release", "cut a release", or "stable release" for llmux. Bumps the version, tags v*, lets CI publish the stable GitHub release, refreshes the llmux stable brew formula, verifies brew updated, hot-deploys + restarts locally, and verifies client + server with llmux status.
---

# release (릴리즈) — stable channel

Cut a formal **stable** release: version bump → tag `v*` → CI stable release → brew
`llmux` → local deploy → `llmux status` (client AND server).

A `v*` release ships **both** the CLI binaries and the macOS app
(`LlmuxIslands-<version>.zip`, built by the `islands` job in `release.yml`), and the tap
scheduled update or the shared reference’s reviewed template fallback refreshes
**both** the `llmux` formula and the `llmux-islands` cask — so
`brew install llmux` (CLI only) and `brew install llmux-islands` (app + CLI via `depends_on`)
track the same version. After step 6, also confirm `brew info --cask llmux-islands` == `<new>`.

Shared mechanics: `.claude/skills/_shared/cd-reference.md` (procedure A = hot-deploy,
procedure B = publish+verify brew).

Docs-only followups to a verified release use the shared reference’s `[skip ci]`
documentation path; they do not need a new version, tag or daemon restart. Filtered
build contexts must retain the three embedded bridge assets, and SDK changes need
`just check-bridge` alongside the Rust gate.

## Steps

1. **Pre-flight + version.** `just check` green, tree intentional. `Cargo.toml` is currently
   the last released version and **the matching `v*` tag already exists**, so a release
   **requires a bump**. **Default: bump the patch** (e.g. `0.2.1 → 0.2.2`) and proceed
   without asking. Only deviate — a minor/major bump or a specific number — when the user
   gave a **special instruction** for this release (then use exactly what they said).
2. **Bump.** Set `version = "<new>"` in `Cargo.toml`; run `just check` so `Cargo.lock`
   updates and the gate passes. **Also sync the sub-crate lockfiles** — `just check` only
   updates the workspace lock, but the `islands` release job builds with `cargo --locked`
   against each crate's own lock (v0.2.20 first-tag failure, 2026-08-21):
   `for d in llmux-islands-core llmux-islands-linux llmux-islands-macos-bridge; do (cd $d && cargo update -p llmux --precise <new>); done`
   The release workflow **fails if tag `v<new>` ≠ Cargo.toml
   version** — they must match exactly.
3. **Commit + push main.** `git commit -am "chore: release v<new>"`; `git push origin
   main` (token fallback if needed). Confirm with the user if a PR (not direct main) is
   required.
4. **Tag + push the tag** (this triggers `release.yml`):
   `git tag v<new> && git push origin v<new>` (token fallback:
   `git push "https://x-access-token:$(gh auth token)@github.com/2lab-ai/llmux" v<new>`).
5. **Watch the release build.**
   ```bash
   rid=$(gh run list --repo 2lab-ai/llmux --workflow release.yml -L1 --json databaseId -q '.[0].databaseId')
   gh run watch --repo 2lab-ai/llmux "$rid" --exit-status
   ```
   Then confirm: `gh release view v<new> --repo 2lab-ai/llmux` (this is the new "Latest").
6. **Publish + verify brew (stable)** — procedure B with `formula=llmux`. Inspect the
   current tap trigger: as of 2026-10-08 its llmux jobs run only on schedule, so manual
   dispatch does not update llmux. For immediate delivery, render the exact stable
   formula and cask from their templates using verified release-asset hashes; review
   and publish only those two files. Then `brew update && brew upgrade llmux`; confirm
   `brew info --json=v2 llmux | ...installed[0].version` == `<new>`.
   *(If only `llmux-preview` is currently installed, `brew install 2lab-ai/tap/llmux`;
   both provide `bin/llmux` via `link_overwrite`.)*
7. **Hot-deploy + restart.** Brew build is in the Cellar after upgrade →
   `/opt/homebrew/bin/llmux restart`. Verify `--version` reports `<new> (stable <id>)`.
8. **Final verify — client AND server.** `/opt/homebrew/bin/llmux status`: both the local
   client view and the running daemon's accounts reflect the new build. This is the owner's
   required end-state. For frontend/runtime changes, also run the shared reference’s
   installed native-Codex and Claude-SDK tool smokes; ensure daemon Node/npm and
   packaged SDK bootstrap work without a repository runtime override.
9. **Report** new version, release URL, brew version, and the `status` summary.

## Common mistakes

- **tag ≠ Cargo.toml version** — #1 failure; bump first (step 2) then tag (step 4). Re-tagging
  means deleting the bad tag locally + remotely.
- Reusing an existing version (e.g. `v0.1.0`) — always go forward.
- Assuming manual tap dispatch updates llmux: verify the current trigger and use
  the scoped reviewed fallback when llmux jobs are schedule-only.
- "Already up-to-date" → `brew update` then re-check `brew info` before trusting it.
- Pushing main/PR without the user's go-ahead (version itself defaults to a patch bump —
  see step 1).
