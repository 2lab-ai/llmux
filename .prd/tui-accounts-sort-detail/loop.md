# Accounts table: Fbl n/a, sort modes, detail modal — loop

Status: shipped (preview-2026-09-18-0609-850db8b80519)
Date: 2026-09-18

## Build facts (measured 2026-09-18, main @ eccf1ac = v0.2.23)

- Gate: `just check` = `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` + `cargo test`.
- Fbl cell: `src/tui/ui.rs` `fable_gauge_cell` :5006 (absent branch :5017 renders `○ cold` for every group); caller :4597; the 5h Claude-only rule to mirror is `five_hour_text` :4918-4930.
- Order: `src/tui/triage.rs` `intervention_order` :253 (tier → 5h permille → idx) is THE display order via `view.rs` `display_order` :494; 18 call sites in `src/tui/mod.rs` (row cursors) + `ui.rs:223` (`FrameCtx.order`). Scheduler's real pick order lives in `src/scheduler/select.rs` `selection_order` :870 (group = None) reusing `eligibility` + `rank` :821.
- Mouse: `mod.rs` `on_mouse` :1432; input/raw modals swallow first (:1443/:1454); right-click on `account_row_chrome` opens the context menu (:1620-1648) and pins the REAL id via `display_order`; there is no left-click handler for account rows. Modal pattern: `InputModal` (:549) + `on_key_input_modal` (:1780) + `draw_input_modal` (`ui.rs:3385`) + post-draw clamp/close (:5218-5228). Key dispatch order: `on_key` :1372 (modals → Mode → overlay).
- Data available per account: `AccountSnapshot` (`src/scheduler/mod.rs:981-1016`), `PoolSnapshot.{current,fable_current,manual_pin}`, `DashboardView.{session_totals, poll_health, usage_controls, refresh_ahead, select_params}`, `select::eligibility/blocking_reason/effective_limits`.
- Live daemon at start: `0.2.23 (stable)`; 20 accounts (claude 15, codex 4, grok 1).

## Round 1 — 2026-09-18

| WU | Scope (files) | Owner | Gate | Verify |
| --- | --- | --- | --- | --- |
| A Fbl `-` for non-Claude | `src/tui/ui.rs` (`fable_gauge_cell` + caller + 1 test) | opus-coder | 53ce5ad · lib 1271 | `fable_gauge_cell_is_n_a_for_non_claude_groups` (RED with the guard removed, GREEN restored); live frame 1 below |
| B group + name / next sort, `o` toggle | `src/scheduler/select.rs` (`group_selection_order`), `src/tui/triage.rs` (`intervention_order` deleted → `display_order` + `AccountSort`), `src/tui/view.rs`, `src/tui/mod.rs` (App/Chrome `account_sort`, 19 call sites, `o` ×2), `src/tui/ui.rs` (FrameCtx.order, title, footer), docs | opus-coder | f570fe6 · lib 1276 | 7 tests; live frames 1–3 |
| C click → account detail modal | `src/tui/mod.rs` (App/Chrome `account_modal`, left-click on row in MAIN + accounts tab, key/mouse swallow, post-draw clamp/close), `src/tui/ui.rs` (`draw_account_modal`, 10 sections, `draw_accounts_overlay` now returns row hits), docs | opus-coder | 559b156 · lib 1280 | 4 tests; live frames 4–6 |
| D review fixes (trinity R1) | `select.rs` `group_selection_order` = `pick_scoped` parity by construction (head from the decision; ineligible current no longer pinned; heuristic-degraded + manual pin honored); modal gate per-group `headers_only`; 3 stale comments | opus-coder | 81f42e1 · lib 1282 | `group_selection_order_head_agrees_with_pick_in_every_regime` (6 regimes, RED on the pre-fix body at the clearly-better case), `account_modal_gate_is_group_scoped` (RED pre-fix: modal read `usage stale`) |
| docs | `docs/operational-reference.md` (TUI keys paragraph), `.prd/19`, this folder | dispatcher | — | links open |

WUs ran sequentially (B and C both own `mod.rs`/`ui.rs`).

Dispatcher gate re-run after C (2026-09-18): `just check` exit 0 — fmt/clippy silent; lib **1280 passed / 0 failed** (+10 vs main), cli 22, e2e 80 (1 ignored), grok_usage 5, keys_history 31, relogin 7, token_limits 5, usage_controls 32.

Live receipt (worktree release binary `0.2.23 (dev dev)`, `llmux dashboard` attached read-only to the running `0.2.23 (preview 2026-09-18-0311)` daemon, 20 accounts, tmux 200×45; captures in the session scratchpad `receipt/1-main-name.txt` … `6-modal-closed.txt`):

1. MAIN, `accounts · sort name`: rows 1–15 CLAUDE (ai10, ai11, ai12, ai2, ai3, ai4, ai5, ai7, ai8, ai9, ai, dev1, icedac, info, notify — name order), 16–19 CODEX, 20 GROK. Every CODEX/GROK row reads `-` under both `5h` and `7d Fbl`; the pre-fix frame (`probe-accounts.txt`) had `○ cold` there and the groups interleaved.
2. After `o`: title `accounts · sort next`; Claude block leads with the current `ai4` (►), Codex block leads with its current `icedac` (►), exhausted (`7d 100.0% > 99%`) rows at each block's tail.
3. Accounts tab: same title, footer `… t eta/utc  o sort  Esc back  q quit`.
4. SGR left-click at row 6 (2nd data row = `ai11`) → modal `🔍 account — claude:ai11@insightquest.io · oauth · CLAUDE` with sections identity / status / token / windows (5h, 5h raw, 7d, 7d raw) / scoped (Fable 71%) / cooldown / limits (override + effective) / lifetime / poll / resets, footer `↑↓ scroll · esc close`.
5. PgDn scrolls; 6. Esc closes, table intact.

Trinity R1 (grok-4.6 / gpt-6-astra / fable): astra REJECT — MUST-FIX ① `group_selection_order` ≠ `pick` (ineligible current pinned first, heuristic-degraded ranking ignored) ② modal gate uses the global `headers_only`, selector decides per group; fable APPROVE (3 non-blocking comment drifts); grok pending. Dispatcher verified ① and ② against `select.rs:402-575` / `ui.rs:224` → WU D.

Trinity R2 (positions forwarded verbatim + D diff as appended evidence): grok APPROVE (withdrew its R1 "current-first is convention" residual — "UI가 next라고 광고하면 head는 pick의 결정이어야 한다"); fable APPROVE (withdrew its R1 round-robin parity argument as over-generalized); astra REJECT with one narrowed MUST-FIX — the modal still calls `select::eligibility` (`gate(.., heuristic_degraded=false)`, `select.rs:127-134`) so in an all-heuristic-parked group the selector's actual pick reads `blocked cooldown` in the modal. Dispatcher confirmed at `ui.rs:3490` → WU E.

| WU | Scope (files) | Owner | Gate | Verify |
| --- | --- | --- | --- | --- |
| E modal gate = `gate_scoped` with per-group `heuristic_degraded_mode` + `degraded` row | `src/tui/ui.rs` (`account_modal_lines` status section + 1 test) | opus-coder | d21be2a · lib 1283 | `account_modal_gate_honors_heuristic_degraded_mode` (RED on the old gate: `gate ▌ 2m 00s` for the account `pick_scoped` chose) |

Dispatcher gate re-run on d21be2a (2026-09-18): `just check` exit 0 — lib **1283 passed / 0 failed** (+13 vs main), cli 22, e2e 80 (1 ignored), grok_usage 5, keys_history 31, relogin 7, token_limits 5, usage_controls 32.

Trinity R3 (E diff appended as evidence): astra APPROVE ("일반 `eligibility`의 degraded=false 고정이 제거됐다"); fable APPROVE (withdrew its R2 "비차단" classification; verified the coder's flagged `triage::urgent` side effect is not real — CoolingDown is tier 2, `urgent = tier ≤ 1`, so the modal's urgent flag is unchanged in every regime); grok APPROVE (withdrew its R2 "blocked는 사실" argument). **Unanimous APPROVE, 0 MUST-FIX, round 3/5.** Recorded follow-up (pre-existing, not this PR): table status column `ui.rs:5017` and detail pane `ui.rs:5890` still gate with `eligibility(.., ctx.headers_only)`, so in a group-wide heuristic lockout the table shows `▌ 2m 00s` for the account the modal (and the daemon) call ready; durable fix = per-group `(headers_only, heuristic_degraded)` on `FrameCtx` + both call sites → `gate_scoped`. Same class: pool-wide `selection_order` / `next_in_line`.

## Gap matrix

| Clause | Status | Evidence |
| --- | --- | --- |
| Fbl `-` non-Claude | closed | `fable_gauge_cell_is_n_a_for_non_claude_groups`; live frame 1 (`-` on rows 16–20) |
| group order | closed | `display_order_groups_claude_codex_grok_openrouter`; live frames 1–2 |
| name sort | closed | `display_order_by_name_sorts_within_group`; live frame 1 |
| next sort | closed | `display_order_by_next_follows_group_selection_order` + `group_selection_order_head_agrees_with_pick_in_every_regime`; live frame 2 |
| `o` toggle + title + footer | closed | `sort_key_toggles_mode_and_title`; live frames 2–3 |
| click → modal, pinned id, close/scroll/swallow | closed | 3 tests; live frames 4–6 |
| modal lists every section | closed | `account_modal_renders_every_section`; live frame 4 |
| modal gate = selector's gate | closed | `account_modal_gate_is_group_scoped`, `account_modal_gate_honors_heuristic_degraded_mode` |
| preview deployed + live receipt | closed | ship table below; deployed frames d1–d4 |

## Ship (2026-09-18)

| Step | Receipt |
| --- | --- |
| PR #166 CI | CI (macos + ubuntu check) pass, Islands parity (semantic core, macOS shared-core shell, Arch KDE shell) pass; mergeStateStatus CLEAN |
| merge | squash → main `850db8b` |
| preview.yml | run 35313671826 success → prerelease `preview-2026-09-18-0609-850db8b80519` (06:16Z) |
| tap | `Formula/llmux-preview.rb` version `2026.09.18.0609` (bumped by the release flow; the manual `bump.yml` dispatch needs a `tag` input and was not needed) |
| brew | `2lab-ai/tap/llmux-preview 2026.09.18.0311 -> 2026.09.18.0609`; `/opt/homebrew/bin/llmux --version` → `llmux 0.2.23 (preview 2026-09-18-0609-850db8b80519)` |
| restart | detached one-shot `llmux restart` → `restarted llmux server (pid 98671) on port 3456 → llmux 0.2.23 (preview 2026-09-18-0609-850db8b80519)`; `llmux status` server `running`, uptime 21s, `accounts: 20 (20 ready)` |
| live smoke (deployed binary, `llmux dashboard`, tmux 200×45) | d1: header `preview 2026-09-18-0609-850db8b80519 :3456 pid 98671`, `accounts · sort name`, CLAUDE 1–15 by name, CODEX 16–19, GROK 20, `-` under `7d Fbl` on every non-Claude row. d2: `o` → `sort next`; Claude head = current `ai4` (►); **Codex head = `icedac ready` while the exhausted current `ai2` (►!) sits below** — the R1 parity fix observed live (pre-fix code would have pinned `ai2` first). d3: left-click row 3 → modal `claude:ai12@insightquest.io · oauth · CLAUDE`, `order #3 of 20 · sort name`, gate `ready`. d4: Esc → 0 modal glyphs, table intact. |

Worktree `feat-tui-accounts-sort-detail` removed after merge (clean, branch deleted). Report artifact (private): https://claude.ai/artifact/9HNa7UGrKPGSwwVVQETC3W
