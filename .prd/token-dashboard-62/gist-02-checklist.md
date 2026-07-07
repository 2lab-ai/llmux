# Implementation Checklist: llmux Token Usage Dashboard Epic

## Phase 1: Islands reads `/llmux/dashboard`

Files:

```text
llmux-islands/LlmuxIslands/Llmux/LlmuxStatus.swift
llmux-islands/LlmuxIslands/Llmux/LlmuxClient.swift
llmux-islands/LlmuxIslands/Llmux/IslandUsageModel.swift
```

Checklist:

- [ ] Add `LlmuxDashboard` DTO.
- [ ] Add dashboard nested DTOs:
  - [ ] `LlmuxDashboardAccount`
  - [ ] `LlmuxDashboardWindow`
  - [ ] `LlmuxDashboardTotals`
  - [ ] `LlmuxDashboardModelUsage`
  - [ ] `LlmuxDashboardModelAccount`
  - [ ] `LlmuxDashboardModelCount`
  - [ ] `LlmuxDashboardClientUsage`
  - [ ] `LlmuxDashboardWindowed`
  - [ ] `LlmuxDashboardWindowedCell`
  - [ ] `LlmuxDashboardActivity`
  - [ ] `LlmuxDashboardInFlight`
  - [ ] `LlmuxDashboardCompleted`
- [ ] Add `LlmuxClient.dashboard()`.
- [ ] Change `IslandUsageModel.refresh()` to fetch dashboard.
- [ ] Preserve existing account tile behavior.
- [ ] Publish dashboard totals and model usage to SwiftUI.
- [ ] Keep fallback behavior for older daemons if dashboard decode fails.

## Phase 2: Islands analytics UI

Files:

```text
llmux-islands/LlmuxIslands/UI/Views/IslandUsageView.swift
llmux-islands/LlmuxIslands/Dashboard/UsageTiles.swift
```

Optional new files:

```text
llmux-islands/LlmuxIslands/Dashboard/AnalyticsCards.swift
llmux-islands/LlmuxIslands/Dashboard/ModelUsageCards.swift
llmux-islands/LlmuxIslands/Dashboard/WindowedHeatmapView.swift
```

Checklist:

- [ ] Add summary cards:
  - [ ] requests
  - [ ] tokens
  - [ ] API-equivalent cost
  - [ ] error rate
- [ ] Add top model cards/list.
- [ ] Add compact 24h/72h heat strip.
- [ ] Add recent activity list.
- [ ] Add health warning banner.
- [ ] Add model detail sheet.
- [ ] Add account detail sheet.

## Phase 3: Rust document field improvements

Files:

```text
src/dashboard.rs
src/tui/activity.rs
src/tui/view.rs
src/tui/ui.rs
src/pricing.rs
```

Checklist:

- [ ] Add `ModelUsageDoc.cost_usd` as `#[serde(default)]`.
- [ ] Populate model row cost using existing `src/pricing.rs` logic.
- [ ] Keep `GlobalTotalsDoc.cost_usd` as sum of model row costs.
- [ ] Verify older dashboard docs still parse.
- [ ] Add tests for model cost serialization.

## Phase 4: Data-quality labels

Files:

```text
src/dashboard.rs
src/tui/ui.rs
llmux-islands/LlmuxIslands/UI/Views/IslandUsageView.swift
```

Checklist:

- [ ] Add or render labels for:
  - [ ] model usage scope
  - [ ] windowed best-effort status
  - [ ] API-equivalent cost status
  - [ ] cache missing-vs-zero policy
- [ ] TUI shows compact labels.
- [ ] Islands shows tooltip/help text.

## Phase 5: Verification

- [ ] `cargo fmt`
- [ ] `cargo clippy -- -D warnings`
- [ ] `cargo test`
- [ ] `just check`
- [ ] `xcodegen generate` under `llmux-islands/`
- [ ] `xcodebuild` for `LlmuxIslands`
- [ ] Manual check: TUI and Islands show the same totals from `/llmux/dashboard`.
- [ ] Manual check: older daemon/status fallback does not crash Islands.
