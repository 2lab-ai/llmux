---
name: release
description: Use when the user says "릴리즈", "릴리즈해줘", "release", "cut a release", or "stable release" for teamagent. Bumps the version, tags v*, lets CI publish the stable GitHub release, refreshes the teamagent stable brew formula, verifies brew updated, hot-deploys + restarts locally, and verifies client + server with teamagent status.
---

# release (릴리즈) — stable channel

Cut a formal **stable** release: version bump → tag `v*` → CI stable release → brew
`teamagent` → local deploy → `teamagent status` (client AND server).

Shared mechanics: `.claude/skills/_shared/cd-reference.md` (procedure A = hot-deploy,
procedure B = publish+verify brew).

## Steps

1. **Pre-flight + version.** `just check` green, tree intentional. `Cargo.toml` is currently
   the last released version and **the matching `v*` tag already exists**, so a release
   **requires a bump**. Ask the user for the new version (propose the next patch, e.g.
   `0.1.0 → 0.1.1`). *(Decision point — never pick the version yourself.)*
2. **Bump.** Set `version = "<new>"` in `Cargo.toml`; run `just check` so `Cargo.lock`
   updates and the gate passes. The release workflow **fails if tag `v<new>` ≠ Cargo.toml
   version** — they must match exactly.
3. **Commit + push master.** `git commit -am "chore: release v<new>"`; `git push origin
   master` (token fallback if needed). Confirm with the user if a PR (not direct master) is
   required.
4. **Tag + push the tag** (this triggers `release.yml`):
   `git tag v<new> && git push origin v<new>` (token fallback:
   `git push "https://x-access-token:$(gh auth token)@github.com/2lab-ai/teamagent" v<new>`).
5. **Watch the release build.**
   ```bash
   rid=$(gh run list --repo 2lab-ai/teamagent --workflow release.yml -L1 --json databaseId -q '.[0].databaseId')
   gh run watch --repo 2lab-ai/teamagent "$rid" --exit-status
   ```
   Then confirm: `gh release view v<new> --repo 2lab-ai/teamagent` (this is the new "Latest").
6. **Publish + verify brew (stable)** — procedure B with `formula=teamagent`. Dispatch the
   tap `bump.yml`, watch it, `brew update && brew upgrade teamagent`, confirm
   `brew info --json=v2 teamagent | ...installed[0].version` == `<new>`.
   *(If only `teamagent-preview` is currently installed, `brew install 2lab-ai/tap/teamagent`;
   both provide `bin/teamagent` via `link_overwrite`.)*
7. **Hot-deploy + restart.** Brew build is in the Cellar after upgrade →
   `/opt/homebrew/bin/teamagent restart`. Verify `--version` reports `<new> (stable <id>)`.
8. **Final verify — client AND server.** `/opt/homebrew/bin/teamagent status`: both the local
   client view and the running daemon's accounts reflect the new build. This is the owner's
   required end-state.
9. **Report** new version, release URL, brew version, and the `status` summary.

## Common mistakes

- **tag ≠ Cargo.toml version** — #1 failure; bump first (step 2) then tag (step 4). Re-tagging
  means deleting the bad tag locally + remotely.
- Reusing an existing version (e.g. `v0.1.0`) — always go forward.
- Forgetting to dispatch the tap `bump.yml` — brew stays on the old stable.
- "Already up-to-date" → `brew update` then re-check `brew info` before trusting it.
- Picking the version or pushing master/PR without the user's go-ahead.
