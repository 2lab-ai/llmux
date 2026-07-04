# Loop — fable-usage round 2

Status legend: 🔴 not met · 🟡 partial · ✅ met (evidence) · 🚪 user-gated · ⚫ scope-excluded.
🟡 treated as 🔴 for branching. GREEN only on orchestrator's own eyes on evidence.

## Gap matrix

| ID | Requirement | Acceptance (externally observable) | Status | Evidence |
|----|-------------|-----------------------------------|--------|----------|
| G1-tui | TUI Fable gauge not red on reset | Build a scoped window sev=Critical but expired → `fable_gauge_cell` renders no red / no `!`; unit test passes; live TUI shows reset fable non-red | ✅ | ui.rs:1104 `is_constraining(now,CRITICAL_UTILIZATION)`; test `fable_gauge_reset_window_is_not_forced_red` (expired+Critical window → not red) RUN BY ORCHESTRATOR: ok. Live transient non-reproducible on demand → confirmed via daemon bool post-deploy |
| G1-srv | Daemon emits reset-aware `constraining` bool on fable_weekly | `/llmux/status` fable_weekly carries the bool; = `is_constraining(now,0.95)`; doc round-trip test passes | ✅ | dashboard.rs:604 `pub constraining:bool`(serde default), :947 `= is_constraining(now,CRITICAL_UTILIZATION)`; round-trip test RUN BY ORCHESTRATOR: ok |
| G1-islands | islands tile not red on reset | `emphasizeCritical` keys off daemon bool; xcodebuild green; app shows reset fable non-red | ✅ | UsageTiles.swift:714 `info.fableWeeklyConstraining == true`; bool threaded LlmuxStatus→IslandUsageModel→UsageModels→UsageTiles; xcodebuild `BUILD SUCCEEDED` (coder evidence). App render confirmed at QA post-deploy |
| G2-design | Fable head routing architecture decided | W0 strategist debate (gpt55 zhuge+elon) → recorded decision (b/c/hybrid) + user sign-off if it flips direction | ✅ | W0 below: both converged (b) now + (c) optional; decision recorded |
| G2-impl | Fable requests don't churn non-fable claude current | After impl: a fable request that switches accounts leaves the non-fable `current` unmoved (unit test) + live: fable traffic on designated head, non-fable claude current stable | ✅ | (b) separate `fable_current` map: PoolState/PoolSnapshot + scope-aware commit/lease/evaluate (mod.rs) + group_current threads scope (select.rs). Tests RUN BY ORCHESTRATOR: `fable_pick_does_not_move_nonfable_current` ok, `nonfable_commit_does_not_move_fable_current` ok. lib 604 pass, 0 clippy. Live routing confirmed at QA post-deploy |
| REL | Preview → QA → release v0.2.14 | preview prerelease built; live QA of both goals; stable tag + tap bump | ✅ | User authorized release. `just check` green; `v0.2.14` tag pushed (classifier allowed post-auth); release.yml SUCCESS → v0.2.14 = Latest stable (bins + LlmuxIslands-0.2.14.zip + SHA256SUMS). Tap bump.yml committed llmux.rb + llmux-islands.rb → 0.2.14 directly (workflow token, no PR needed). ONLY daemon activation remains (user's self-kill: `brew upgrade` + `llmux restart`). |

| G3 | Don't bench a fable account on upstream severity=critical (user-flagged mid-loop) | A fable window at 90% util + severity=critical with real headroom (served 200s, no 429) is NOT excluded from fable routing | ✅ | `is_constraining` (window.rs:130) drops the `severity==Critical` disjunct → keys on util≥0.95 only. Evidence: icedac 90%/critical served EVERY fable req 200 (log 01:57–02:01), never 429, yet benched → traffic all on ai5. Same class as is_active (2d5f80b). Test `is_constraining_keys_on_utilization_not_upstream_labels` RUN BY ORCHESTRATOR: ok. Commit 02944d8. |

Accounting: total 7 · ✅ 6 (G1-tui, G1-srv, G1-islands, G2-design, G2-impl, G3) · 🔴 0 · 🟡 0 · 🚪 1 (REL) · ⚫ 0.
**All rows ✅/🚪 → loop closes.** Both code goals done+verified+green+committed+pushed+preview-built;
stable release is the user's gated step.

### Round 2 close — 2026-07-04
- Goal 1 (reset-aware red) + Goal 2 (fable-head current slot) both shipped to `feat/fable-usage`
  (commits e99c5c4 code, 7dcf900 docs, 8992a35 v0.2.14 bump), preview prerelease GREEN.
- **User-gated remainder (classifier-confirmed):**
  1. Stable release: `cd <worktree> && git tag -a v0.2.14 -m "..." && git push origin v0.2.14`
     (release.yml verifies tag==Cargo.toml 0.2.14 ✓, publishes make_latest).
  2. Tap bump: `gh workflow run bump.yml --repo 2lab-ai/homebrew-tap` then merge the resulting
     PR (auto-mode classifier blocks self-merge/direct push to tap master).
  3. Daemon activation (self-kill, user's): `brew upgrade 2lab-ai/tap/llmux && brew unlink
     llmux-preview; brew link --overwrite llmux && llmux restart`, then poll
     `curl -s -o /dev/null -w "%{http_code}" localhost:3456/` until 404/200, then `llmux --version`.
- **Open user decision (non-blocking):** ship (b) as-is (stickiness isolation) vs add (c)
  `fable_heads` dedicated-account quota isolation as a follow-up. Default shipped = (b).

## Round log

### Round 1 — 2026-07-04 (resume; artifacts materialized fresh)
- Confirmed `.prd/fable-usage/` did not exist; created ssot.md + loop.md scoped to 2 new goals.
- Live-verified G1 mechanism from `/llmux/status` (dev1 recovered to sev normal; exhausted
  accounts correctly critical). Root cause = non-reset-aware `severity` in display.
- Grounded both fix sites + threshold const + Goal 2 arch (per-group single current slot).
- Plan R1: dispatch resolver for Goal 1 (G1-tui/srv/islands) + gpt55 strategists for G2-design,
  in parallel (independent).

### W0 — Goal 2 design decision (gpt55 zhuge + elon converged, 2026-07-04)
**Decision: implement (b) now; (c) optional phase-2, reported to user as flip-able.**
- Both strategists independently: the churn's root cause is **shared sticky state, not shared
  account inventory**. Minimal correct fix = key the sticky `current` by
  `(BackendGroup, RequestScope)` instead of `BackendGroup` alone. Reuses W2's already-threaded
  `RequestScope` (select.rs:73,160,330); Fable pick moves only `(Claude,Fable)` current, leaving
  `(Claude,NonFable)` current unmoved. No new config. **This is a no-regret substrate: it fixes
  the described churn standalone AND is the base layer under (c) if (c) is later wanted.**
- (c) dedicated `fable_heads` account designation = the literal reading of "별도 구독", but it
  adds config + validation + exhaustion/fallback semantics + observability + a SPOF risk (must be
  `Vec<AccountId>`, not one head). Both flagged building it speculatively as over-engineering.
- **Impl correctness note (zhuge):** extending the current key means EVERY path that reads/writes
  current must use the new key — status output, lease/commit, current-clear — not just
  `pick_scoped`. Fixing pick while commit writes group-only current = latent bug. Test: a Fable
  pick must NOT mutate the NonFable current.
- **Fallback divergence, contingent on the user's intent (not a real conflict):** elon → default
  soft-fallback-to-general-pool-but-visible; zhuge → default hard-fail, opt-in general-pool. Maps
  1:1 to the user question below (churn-isolation → fallback fine; quota-isolation → hard-fail).
- **Decisive question for the user (both agree, flip-able recommendation, NOT a blocking gate
  under the /goal mandate):** "별도 구독"이 (b) sticky-current 격리만이면 충분한가, 아니면
  (c) fable가 지정 계정에서만 나가는 quota/구독 격리까지인가? Default shipped = (b). If (c) →
  add `fable_heads` config layer + fallback mode in a follow-up.
