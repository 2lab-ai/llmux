---
name: deploy
description: Use when the user says "배포", "배포해줘", "deploy", or "ship a preview" for llmux. Pushes to main, lets CI publish a preview prerelease, refreshes the llmux-preview brew formula, verifies brew actually updated, then hot-deploys that build locally and restarts.
---

# deploy (배포) — preview channel

Ship the current work as a **preview**: main → CI preview build → brew `llmux-preview`
→ local. For a local-only dev loop use `build`; for a stable release use `release`.

Shared mechanics: `.claude/skills/_shared/cd-reference.md` (procedure A = hot-deploy,
procedure B = publish+verify brew).

## Steps

1. **Pre-flight.** `just check` green; know what's uncommitted (`git status`). If dirty, ask
   whether to commit (and the message) or stash. *(Decision point.)*
2. **Land on main.** If on a branch, merge/fast-forward into main per repo norm.
   **Confirm with the user before pushing main** (public preview channel), then
   `git push origin main` (token fallback if needed).
3. **Watch the preview build.**
   ```bash
   rid=$(gh run list --repo 2lab-ai/llmux --workflow preview.yml -L1 --json databaseId -q '.[0].databaseId')
   gh run watch --repo 2lab-ai/llmux "$rid" --exit-status
   ```
   Success publishes prerelease `preview-<YYYY-MM-DD-HHMM>-<sha12>`.
4. **Confirm the prerelease.** `gh release list --repo 2lab-ai/llmux -L5` (preview is a
   *prerelease* — `gh release view` without a tag shows the stable one, not this). Note the
   new `preview-*` tag.
5. **Publish + verify brew** — procedure B with `formula=llmux-preview`. The preview
   `publish` job automatically pushes the matching formula/cask to the tap. Confirm
   the exact tag there, then `brew update && brew upgrade llmux-preview`; confirm the brew
   version (`YYYY.MM.DD.HHMM`) matches the new preview tag's timestamp.
6. **Hot-deploy + restart.** The brew build is already in the Cellar after upgrade, so:
   `/opt/homebrew/bin/llmux restart`. Verify `--version` reports `(preview <id>)`.
7. **Verify.** `/opt/homebrew/bin/llmux status` — client and daemon on the new preview build.
   For frontend/runtime changes, execute the shared reference’s installed native-Codex
   and Claude-SDK tool smokes, with daemon Node/npm available.
8. **Report** preview tag, brew version, running daemon version.

## Common mistakes

- Treating a release asset alone as publication: preview `publish` must also pass its
  automatic tap bump. Diagnose and rerun its idempotent publish job on failure; the
  tap’s manual dispatch currently targets Dbotter and skips llmux jobs.
- Rebuilding/restarting unchanged runtime for docs-only followups: follow the shared
  reference’s reviewed `[skip ci]` documentation path instead.
- `gh release view` hiding the prerelease (shows stable) — use `gh release list`.
- "Already up-to-date" after a bump → `brew update` first, re-check `brew info`.
- CI latency — poll with `gh run watch`, don't assume instant.
