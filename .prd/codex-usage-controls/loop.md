# Codex usage controls convergence

Status: in-progress
Date: 2026-09-11
SSOT: [ssot.md](ssot.md)

## Round 1 — evidence before code

1. Research official Codex reset listing/redemption API and evidence limits (read-only worker).
2. Locate llmux accounts, forced switch and usage paths (read-only worker); dispatcher verifies primary source before implementation briefs.
3. Pin API findings, design and full client-to-upstream-to-render traces.
4. Implement executable RED contracts, then implementation through bounded coding workers.
5. Directly rerun gates, isolated daemon and terminal-frame QA; independent review.
6. Merge/preview only with required receipts and applicable permissions; verify actual deployed surface.

## Work units

| WU | Scope | Branch/worktree | Status |
| --- | --- | --- | --- |
| R1 | Official Codex reset API research | Source links in numbered contract | Verified with source and live GET |
| R2 | Existing accounts usage and switch flow map | main, read-only | Primary paths directly verified |
| I1 | Usage/reset HTTP client, daemon service, API + metadata, backend contract tests | feat/codex-usage-controls / .worktrees/feat-codex-usage-controls | Frozen; direct gate and runtime verified |
| I2 | CLI/accounts TUI controls and owning operational docs | Same worktree; disjoint client files | Frozen; direct gate and local/attach frames verified |
| D1 | Independent 3-engine contract review | Read-only same spec/trace | R3 unanimous APPROVE; MUST-FIX none (design only) |

## Build facts

- Base: `31e3800`, current `origin/main` after fetch.
- `justfile:4-7`: `just check` = cargo fmt --check; cargo clippy --all-targets -- -D warnings; cargo test.
- `justfile:14-15`: `just build` = cargo build --release --locked.
- Baseline `just check` exited 0: fmt and clippy passed; test groups 1057 + 19 + 49 passed, 0 failed, 1 ignored. No implementation changes yet.
- Evidence: session task `bus4z54q3.output` (baseline output retained by harness).
- Main checkout has existing Cargo manifests/lockfile edits; not part of this work.

## Gap matrix

| Goal | Observed | Gap |
| --- | --- | --- |
| Reset listing and redemption API | Official pinned HTTP contract + authorized live read | No fixed weekly grant established; real consume intentionally not executed |
| Per-account remaining reset display/action | CLI + local/attach reset counts, confirmation, fake consume observed | Post-deploy real surface smoke |
| Manual usage refresh | Runtime100→43; failed503 preserves71; all contract tests pass | Post-deploy real surface smoke |
| Forced switch refresh | Runtime refresh71; local TUI fresh100% override preserved | Post-deploy real surface smoke |
| Delivery | Local verification and release build passed | Final test-only review, commit/CI/merge/preview/install |

## Verification log

- Baseline direct gate: 1125 passed / 1 ignored, fmt+clippy green.
- Existing live accounts frame captured in session scratchpad `accounts-before.txt` at 160×44. Fifteen accounts, including three Codex rows; no refresh/reset affordance in footer. The CLI display mode is remaining quota, not necessarily used quota. This capture does not establish the exact prior external-reset incident.
- Verified live admin path: `x-api-key` (not Bearer); dashboard GET succeeds. `live-dashboard-before.json` retained locally, not intended for public report.
- Codex upstream read-only GETs both HTTP200: `live-codex-usage.json` and `live-codex-rate-limit-reset-credits.json` sanitized of credentials/profile IDs. Weekly-primary 43%, owned resets3, applicable0; real reset redemption executed 0 times.
- Runtime QA fixture `usage_mock.py` + `usage-test-config.json` in scratchpad: two fake Codex accounts, mutable quota and idempotent fake consumption. Mock process runs only loopback; isolated config and state path required for daemon.
- Contract review R1 identified availability/idempotency/concurrency/identity/URL gaps. R3 unanimous APPROVE with no MUST-FIX.
- Parent full `just check` after integrated implementation: 1202 passed / 1 ignored; fmt and clippy clean (`parent-check.txt`). This predates final review fixes; rerun required.
- Runtime CLI (`runtime-qa.txt`): refresh100→43 weekly; fake consume remaining3→2 and usage0; manual switch refreshed71; failed refresh503 preserved71 and exited1; non-TTY unconfirmed consume refused.
- Full terminal frames: attach160×44 (`accounts-after.txt`), confirmation (`accounts-reset-confirm.txt`), narrow100×35 (`accounts-after-narrow.txt`); local160×44 switch onto fresh100% account and deliberate fake reset (`accounts-local-switch.txt`, `accounts-local-reset.txt`). Local reset showed remaining3→2 and used0 (remaining gauge100%). Real consume still0.
- Final correction loop: client and backend workers froze after regression fixes. Parent unsuppressed `just check` passed 1218 tests / 1 ignored (`parent-final-check.txt`). Implementation review still pending full panel agreement; not yet ready to ship.
- Frozen source manifest: `final-freeze-sha256.txt`, manifest SHA256 `d9865b633794ba190f0eb4bdd05567a72631f27b5f7e1f65d4d36f56ff8ade20` (16 implementation/doc/test files). Rechecked after release build: no file hash changed.
- Release build `cargo build --release --locked` exited0 (`release-build.txt`); binary SHA256 `df5c3f441448bd6e77652d44a8b3225c816608b50c2b5eb540189b9ca4528bfc`. This is a local build, not a published release or deployed daemon.
- Final runtime repeat passed (`runtime-qa-final-two.txt`); the initial repeat used an already-redeemed mock request ID, so correctly returned already_redeemed and failed the harness's new-spend assertion. A distinct scenario ID fixed the harness; both outputs retained. Final100×35 capture `accounts-final-narrow.txt` shows complete group/CODEX/rst fields.
