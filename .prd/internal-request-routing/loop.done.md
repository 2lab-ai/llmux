# internal-request-routing — done (append-only)

Loop: ./loop.md · Each round close appends one block at the bottom; nothing above it ever changes (this skill's `scripts/loop-lint.py` C2 compares against git). Rows here are snapshots (Last Measured frozen at move time). A later pass is a `re-verified` line in a loop.md round section; a regression reopens as a new loop.md row with Source `regression:<ID>`.

## R1 — closed 2026-11-12 16:20 KST

- measured 5✅ · result: one rule routes every recognised harness request in Codex and Grok main sessions · closed rows: G1 G2 G3 G4 G5 · commit/PR: branch `feat/harness-requests-follow-main-provider`

### Closed rows

| ID | Source | Universe Item | Observable Acceptance | AS-IS Evidence | Status | Gap | Dispatch | Verification Evidence | Last Measured |
|---|---|---|---|---|---|---|---|---|---|
| G1 | live #50916 | GPT session title before first main turn | A1: title → `luna`, low, schema kept | title `claude-haiku-5-5` → claude, 255 ms before main, launch hint 4.5 h old | ✅ | — | WU1 warmup seed | E2E `harness_title_before_first_main_follows_the_gpt_warmup` (RED on base: 404); isolated live replay of #50915→#50916: HTTP 200, upstream `gpt-6-luna` effort low, `{"title":"<folder> 제어 파일과 작업 큐"}` | 2026-11-12 16:06 |
| G2 | live #50904, #50909 | Grok session title and security monitor | A2/A3: Grok pin, low / medium, verdict validated | title → `claude-haiku-5-5`, 3/3 security → `claude-sonnet-5` | ✅ | — | WU1 + WU2 + WU3 | E2E `harness_requests_in_a_grok_session_use_the_grok_pin_at_lower_effort` (RED on base: 404); live replay: title `grok-4.7` effort low with `text.format` accepted → `{"title":"Astra 설명"}`; security `grok-4.7` medium → `<severity>8`, stop_sequence | 2026-11-12 16:06 |
| G3 | live #48932 | Notification status classifier | A4: kind `status`; luna low / Grok low | classified `user`, sent to `claude-haiku-5-5` | ✅ | — | WU2 + WU4 | E2E `harness_status_classifier_in_a_gpt_session_routes_to_luna` (RED on base: kind `user`); live replay: `gpt-6-luna` low and `grok-4.7` low | 2026-11-12 16:06 |
| G4 | config default | Codex opus tier | A7: `terra`, never `sol` | default `opus: sol` | ✅ | — | WU5 | E2E `harness_warmup_never_overrides_a_main_turn_and_codex_tiers_avoid_sol` (RED on base: `gpt-6.1-sol`); unit `claude_code_defaults_partial_overrides_and_validation` | 2026-11-12 16:03 |
| G5 | docs | Owning docs describe a GPT-only exception | docs match shipped behaviour | GPT-only text in 5 docs | ✅ | — | WU7 | configuration.md, operational-reference.md, provider-compatibility.md, system-prompts/families.md, README.md, 01-spec.md, 02-architecture.md | 2026-11-12 16:20 |

### Round section (as it was in loop.md)

Work units (one branch, one PR — the files overlap): WU1 `auto_classifier.rs` session registry stores
the main provider, warmup seeds, route target is Codex or Grok · WU2 `internal_requests.rs` status task
and per-provider tier policy · WU3 `forward.rs` + `provider/grok.rs` route builds, meters and converts
on either provider · WU4 `classify.rs` + `webui/` `status` kind · WU5 `schema.rs` default `opus` → `terra`
· WU6 unit + e2e for A1–A7 (RED on `origin/main` 4/4, GREEN 4/4) · WU7 docs. All ✅.

## R2 — closed 2026-11-12 16:45 KST

- measured 2✅ · result: trinity review findings on llmux#288 resolved · closed rows: G7 G8 · commit/PR: https://github.com/2lab-ai/llmux/pull/288

### Closed rows

| ID | Source | Universe Item | Observable Acceptance | AS-IS Evidence | Status | Gap | Dispatch | Verification Evidence | Last Measured |
|---|---|---|---|---|---|---|---|---|---|
| G7 | trinity astra (P2) | Monitor with a non-tier model | explicit `fable`/GPT/Grok monitor model keeps normal routing | `observe` + `Route::monitor` rerouted any monitor in a Codex/Grok session | ✅ | — | `monitor_target` requires a Claude tier | unit `grok_keeps_its_pin_and_lowers_effort_by_tier`; E2E `harness_monitor_naming_a_non_tier_model_is_not_rerouted` | 2026-11-12 16:44 |
| G8 | trinity fable (P3) | Old-style quota probe (`quota`, `max_tokens: 1`, haiku) | served by the main provider | not live-tested on Grok | ✅ | — | — | isolated build, live: Grok session → `grok-4.7` effort low HTTP 200; GPT session → `gpt-6-luna` low with `x-codex-*` and `x-llmux-quota-mode: availability` | 2026-11-12 16:42 |

### Round section (as it was in loop.md)

Review gate on llmux#288: fable APPROVE (P3 notes), astra REJECT with one P2 (monitor tier gate), grok pending at close. G7 fixed in code; G8 measured live.
