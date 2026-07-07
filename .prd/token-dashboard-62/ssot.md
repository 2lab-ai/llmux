# token-dashboard-62 — SSOT (frozen)

- Sources:
  - GitHub issue [2lab-ai/llmux#62](https://github.com/2lab-ai/llmux/issues/62) (Epic: Token usage dashboard across TUI and llmux-islands) — body fetched 2026-07-04 via `gh issue view 62`
  - Gist spec (mirror of issue attachments): `gist-01-schema.md` (1051 lines) + `gist-02-checklist.md` (115 lines) — raw files downloaded 2026-07-04 into this directory from gist `icedac/92cd499b645c8d687884cd39a7a5aa73`
  - **Precedence rule: gist > issue body on conflict** (gist is more detailed; declared in plan.md v3, carried here)
  - Implementation plan (derived, dual-review v3 APPROVE): `./plan.md` — a *derived view*, not SSOT; universe below is built from issue ∪ gist
- Repo(s) / env: `2lab-ai/llmux` (local `~/2lab.ai/llmux`); surfaces = Rust TUI (`src/`) + `llmux-islands` SwiftUI macOS app
- Report channel: none designated → report in chat (fallback per skill §0)
- Frozen: 2026-07-04 — changes only by explicit user event → revise this file + full re-measure

## Verbatim dictation

(none — SSOT is written spec: issue #62 + gist. User directive was `/goal /ssot-converge ~/2lab.ai/llmux/.prd/token-dashboard-62/plan.md`.)

## Universe (issue ∪ gist — every item)

| ID | Source | Item |
|---|---|---|
| U1 | issue P1 + gist-02 L15-28 (+gist-01 L1016) | `LlmuxDashboard` DTO + 12 nested DTOs (gist names verbatim) + 13th `LlmuxDashboardDataQuality` (optional decode) |
| U2 | issue P1 + gist-02 L29 | `LlmuxClient.dashboard()` |
| U3 | issue P1 + gist-02 L30 | `IslandUsageModel.refresh()` fetches `/llmux/dashboard` |
| U4 | issue P1 + gist-02 L31 | Existing account tile behavior preserved (fed from dashboard `accounts[]`) |
| U5 | issue P1 + gist-02 L32 | Publish totals / model_usage / client_usage / windowed / activity to SwiftUI |
| U6 | gist-02 L33 (gist-only) | Fallback to `/llmux/status` for older daemons if dashboard decode fails |
| U7 | issue P2 + gist-02 L54-58 | Summary cards: requests / tokens / API-equivalent cost / error rate |
| U8 | issue P2 + gist-02 L59 | Top model cards/list |
| U9 | issue P2 + gist-02 L60 | Compact 24h/72h heat strip |
| U10 | issue P2 + gist-02 L61 | Recent activity list |
| U11 | issue P2 + gist-02 L62 | Health warning banner |
| U12 | issue P2 + gist-02 L63-64 | Model detail sheet + account detail sheet |
| U13 | gist-01 §4 (L227, L719 etc.) | v1 hard rules: closed-island format `llmux C:2 X:1 $0.42` + 4 tabs; warning color iff any `auth_failed` account or quota>90%; cache nil → `—` (never 0); model row key = `(group, model)` |
| U14 | issue P3 + gist-02 L80-81 | `ModelUsageDoc.cost_usd` additive `#[serde(default)]` + populated per-row from `pricing.rs` |
| U15 | issue P3 + gist-02 L82 | `GlobalTotalsDoc.cost_usd` = sum of model row costs |
| U16 | issue P3 + gist-02 L83-84 | Backward-compat serialization: older docs parse + tests for new-field serialization |
| U17 | gist-01 L331-338 ("Recommended additive") | `ClientUsageDoc.cost_usd` + `last_seen_ms` — optional scope, adopted by default (see Tensions) |
| U18 | gist-01 §7 L1016 ("Minimum additive") | `DashboardDoc.data_quality` field — 4 label strings (gist-01 L998-1001), `#[serde(default)]`, backcompat |
| U19 | issue P4 + gist-01 L355/L543 | windowed labeled "best effort" — TUI + Islands |
| U20 | issue P4 + gist-01 L1000 | cost labeled "API-equivalent estimate" — **rendered** in TUI + Islands |
| U21 | issue P4 + gist-01 L1001 | cache missing fields shown as unavailable (`—`), not zero — TUI + Islands |
| U22 | gist-01 L998 (gist-only) | model usage scope label "hydrated activity/runtime" — TUI + Islands |
| U23 | issue AC1 | `llmux dashboard` (TUI) still renders with older dashboard docs |
| U24 | issue AC4 + gist-02 L114 | TUI and Islands show consistent totals for same daemon state (manual parity) |
| U25 | issue AC5 | No prompt/response content displayed in either surface |
| U26 | issue AC6 | `just check` passes |
| U27 | issue AC7 + gist-02 L112-113 | `xcodegen generate` + `xcodebuild` for LlmuxIslands passes (implies reproducible build gate + test target for DTO decode tests) |
| U28 | gist-02 L115 | Manual: older daemon/status fallback does not crash Islands |

Out of scope (issue+gist agreed): hosted/multi-user analytics, external analytics backend, prompt/response content display, scheduler changes, credential handling changes, TUI cockpit overhaul, sessions overlay (#34), client attribution (#32).

## Tensions & defaults

- **선행결정 1 (fable merge order) — RESOLVED by measurement 2026-07-04**: `feat/fable-usage` and `feat/islands-fable` are both already merged into `origin/master` (HEAD `4cc97ac`; `git merge-base --is-ancestor` both MERGED, ahead=0). Plan's recommended path already happened; #62 starts on top of `4cc97ac`. Consequence: plan.md's Swift-side ground truth (measured at `8eaa2bf`) is stale → §3 re-measures all Swift rows.
- **선행결정 2 (`ClientUsageDoc.cost_usd`+`last_seen_ms`)**: gist says "Recommended additive" (not minimum), issue body silent → **default: ADOPT in S1** (plan recommendation; same additive pattern, saves a deploy). User may flip → then U17 becomes ⚫ with this note as justification.
- gist vs issue detail conflicts → gist wins (declared above).
- `DashboardDoc.data_quality` is NOT optional — gist §7 lists it as minimum additive (U18). Label strings' SSOT = server field; TUI/Islands read from it, with a shared byte-identical fallback constant for old daemons.

## Clarifications

- 2026-07-04: no open questions — both prior gates had safe defaults (1 resolved by measurement, 2 adopted per plan recommendation). Recorded here for user flip.

## Security Notes

(none — no agent-directed instructions found in issue #62 body or gist files.)
