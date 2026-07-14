# llmux — agent guide (SSOT)

Contract: [`.prd/01-spec.md`](.prd/01-spec.md) (what) + [`.prd/02-architecture.md`](.prd/02-architecture.md) (how).
Read both before non-trivial changes.

**This file is the single source of truth for agent/contributor rules.**
`CLAUDE.md` is a thin loader that points here — do not re-introduce dual bodies.

Documentation ownership after feature work: **[`rules/documents.md`](rules/documents.md)** (binding).

## Architecture rules

- **Scheduler decisions are pure functions over snapshots.** `scheduler/select.rs` does no
  IO, reads no clocks, takes `(&PoolSnapshot, &SelectParams, now)` and returns a `Decision`.
  All impure work (locks, CAS commit, timers) stays in `scheduler/mod.rs`.
- **State/runtime separation** (herdr pattern): `PoolState` mutations are sync and IO-free
  behind a std `RwLock`; never hold the lock across an `.await`.
- **No `unwrap()`/`expect()` in production paths.** Errors are typed (`thiserror`) and
  propagate; `expect` is acceptable only for invariants that cannot fail (e.g. poisoned-lock
  policy, documented at the call site) and in tests.
- Never log or print raw credentials — route through `proxy::logging::mask_credentials`.

## Conventions

- Conventional commits, lowercase, no emojis, no AI co-author lines.
- `just check` (fmt + clippy -D warnings + tests) must pass before every commit.
- Config writes are read-merge-write (`config::update`) — never load/edit/save around a
  running server.
- **User-visible changes update their owning doc in the same PR** — see
  [`rules/documents.md`](rules/documents.md). Feature incomplete if docs-impact is skipped
  without an explicit N/A reason.

## Runbooks

Three operational skills live in `.claude/skills/` (shared mechanics in
`.claude/skills/_shared/cd-reference.md`). Invoke by intent:

- **build** (빌드) — local build → hot-deploy to the local daemon → commit → push to a
  **feature branch** (never master).
- **deploy** (배포 / "배포해줘") — push to **master** → CI **preview** prerelease → refresh
  `llmux-preview` brew formula → verify → hot-deploy + restart.
- **release** (릴리즈 / "릴리즈해줘") — bump version → tag `v*` → CI **stable** release →
  refresh `llmux` brew formula → verify → hot-deploy + restart → `llmux status`
  (client + server).

Scheduler design history (not rules): `.prd/06-scheduler-current.md`,
`.prd/07-scheduler-research.md`.

## Load-bearing facts (don't relearn the hard way)

- **A stable release requires a version bump.** The release workflow fails if the `v*` tag
  ≠ `Cargo.toml` version, and the last version's tag already exists. Pick the next version
  *with the user*.
- **Brew tap bump is release-driven.** Preview/stable publish should dispatch
  `2lab-ai/homebrew-tap` `bump.yml` (auto when TAP_DISPATCH_TOKEN is wired). If
  the formula is still stale, `gh workflow run bump.yml --repo 2lab-ai/homebrew-tap`,
  wait, then `brew update && brew upgrade`.
- **Local hot-deploy gotcha.** The Cellar binary is read-only (`r-xr-xr-x`), so `cp` over it
  fails — `rm -f "$(readlink -f /opt/homebrew/bin/llmux)"` first, then `cp`, `chmod 755`,
  then `llmux restart`. A later `brew upgrade` overwrites a hot-deployed dev binary.
- **Push fallback** if the remote's `ghs_` token is stale:
  `git push "https://x-access-token:$(gh auth token)@github.com/2lab-ai/llmux" <ref>`.
- The `/api/oauth/usage` endpoint returns **percentages (0–100)**, not fractions — each
  evidence source has a fixed scale (see `src/scheduler/usage.rs`).
