# llmux Token Usage Dashboard Schema

## Purpose

Add an `ai-token-monitor` style token, cost, model-usage, client-usage, and quota dashboard to `llmux`.

Target surfaces:

1. `llmux` TUI
2. `llmux-islands` SwiftUI/macOS notch UI

Core rule:

> `GET /llmux/dashboard` and its `DashboardDoc` should be the single source of truth. TUI and Islands should read the same statistics contract and render it differently.

---

## 1. Verified repo base

### Rust/TUI files

```text
src/dashboard.rs
src/tui/event.rs
src/tui/activity.rs
src/tui/view.rs
src/tui/ui.rs
src/session.rs
src/pricing.rs
```

Current `DashboardDoc` already carries the core telemetry substrate:

```rust
DashboardDoc {
  accounts,
  scheduler,
  poller,
  totals,
  model_usage,
  client_usage,
  windowed,
  activity,
  logs,
  codex,
  email_anonymous,
}
```

### Islands files

```text
llmux-islands/LlmuxIslands/Llmux/LlmuxStatus.swift
llmux-islands/LlmuxIslands/Llmux/LlmuxClient.swift
llmux-islands/LlmuxIslands/Llmux/IslandUsageModel.swift
llmux-islands/LlmuxIslands/Dashboard/UsageModels.swift
llmux-islands/LlmuxIslands/Dashboard/UsageTiles.swift
llmux-islands/LlmuxIslands/UI/Views/IslandUsageView.swift
```

Current Islands reads only `/llmux/status`:

```swift
func status() async throws -> LlmuxStatus
```

To show model usage, costs, heatmaps, client usage, and activity in Islands, add a `/llmux/dashboard` DTO and switch/augment polling to that endpoint.

---

# 2. Statistics model schema

## 2.1 Requiredness notation

| Mark | Meaning |
|---|---|
| Y | Always required |
| N | Optional |
| D | Defaultable/additive for older daemon compatibility |
| C | Calculated field |

---

## 2.2 `DashboardDoc`

Top-level contract for `GET /llmux/dashboard`.

| Field | Type | Req | Meaning | Source |
|---|---|---:|---|---|
| version | string | Y | daemon version | build/runtime |
| pid | u32 | Y | daemon PID | runtime |
| uptime_secs | u64 | Y | daemon uptime | runtime |
| port | u16 | Y | proxy port | config/runtime |
| current | string? | N | representative current account | scheduler |
| current_by_group | map | D | current account per group | scheduler |
| accounts | AccountDoc[] | Y | account status/quota/usage | pool + activity |
| scheduler | SchedulerDoc | Y | next account, last switch | scheduler |
| poller | PollerDoc[] | Y | usage poll health | scheduler usage poller |
| totals | GlobalTotalsDoc | Y | global requests/tokens/cost | activity aggregate |
| model_usage | ModelUsageDoc[] | D | model-level usage | activity aggregate |
| client_usage | ClientUsageDoc[] | D | client-level usage | metadata.user_id |
| windowed | WindowedStatsDoc[] | D | 24h/72h heatmap | hourly bucket ring |
| activity | ActivityDoc | Y | in-flight + recent completed | activity log |
| logs | LogLineDoc[] | Y | log tail | tracing/logging |
| codex | CodexSettingsDoc | D | Codex model/fast/effort | config/runtime |
| email_anonymous | bool | D | email masking setting | config/runtime |

---

## 2.3 `AccountDoc`

Basis for account tiles and quota UI.

| Field | Type | Req | Meaning | Source |
|---|---|---:|---|---|
| name | string | Y | account name | config |
| kind | string | Y | `oauth`, `apikey`, `codex` | account type |
| status | string | Y | `active`, `ok`, `cooldown`, `auth_failed` | scheduler |
| order | u64 | Y | selection order | scheduler |
| blocked | string? | N | why account is not selectable | scheduler |
| healthy | bool | Y | auth health | scheduler |
| five_hour | WindowDoc? | N | 5h quota | headers/poll |
| seven_day | WindowDoc? | N | 7d quota | headers/poll |
| cooldown_until | u64? | N | cooldown end epoch sec | 429 handling |
| cooldown_source | string? | N | cooldown reason | scheduler |
| in_flight | u32 | Y | active request count | activity |
| token_expires_at_ms | u64? | N | token expiry | credential |
| last_refresh_ms | u64? | N | last refresh time | credential |
| totals | LifetimeTotalsDoc | Y | proxy lifetime totals | runtime |
| session | SessionTotalsDoc | Y | activity session totals | activity |

Relationships:

```text
AccountDoc.name
  -> ModelUsageDoc.accounts[].name
  -> WindowedCellDoc.account
  -> ActivityDoc.in_flight[].account
  -> ActivityDoc.completed[].account
```

---

## 2.4 `WindowDoc`

Quota window display data.

| Field | Type | Req | Meaning |
|---|---|---:|---|
| utilization | f64 | Y | usage ratio |
| resets_at | u64 | Y | reset epoch sec |
| resets_in_secs | u64 | Y | seconds until reset |
| fetched_at_ms | u64 | Y | usage freshness timestamp |
| source | string | Y | `headers` or `poll` |

Display rules:

```text
percent = utilization * 100
stale = now_ms - fetched_at_ms > usage_max_age_secs
```

---

## 2.5 `TokenCounts`

From `src/tui/event.rs`.

| Field | Type | Req | Meaning |
|---|---|---:|---|
| input | u64 | Y | fresh input tokens |
| output | u64 | Y | output tokens |
| cache_read | Option<u64> | N | cache read tokens |
| cache_creation | Option<u64> | N | cache creation tokens |

Calculation:

```text
total_tokens =
  input + output + cache_read.unwrap_or(0) + cache_creation.unwrap_or(0)
```

Important UI rule:

```text
None = upstream did not provide the field
Some(0) = upstream explicitly reported zero
```

Display `None` as `—`, not `0`.

---

## 2.6 `GlobalTotalsDoc`

Global KPI source.

| Field | Type | Req | Meaning |
|---|---|---:|---|
| requests | u64 | Y | total request count |
| ok | u64 | Y | successful requests |
| errors | u64 | Y | failed requests |
| tokens_in | u64 | Y | input token sum |
| tokens_out | u64 | Y | output token sum |
| rpm_5m | f64 | Y | recent 5m RPM |
| in_flight | u32 | Y | total in-flight requests |
| cost_usd | f64 | D | API-equivalent USD cost |

Derived metrics:

```text
total_tokens = tokens_in + tokens_out
error_rate = errors / max(requests, 1)
avg_tokens_per_request = total_tokens / max(requests, 1)
avg_cost_per_request = cost_usd / max(requests, 1)
```

---

## 2.7 `ModelUsageDoc`

Model-level usage row.

Primary key:

```text
(group, model)
```

Never merge Claude and Codex rows just because the model text matches.

| Field | Type | Req | Meaning |
|---|---|---:|---|
| group | string | Y | `claude` or `codex` |
| model | string | Y | normalized served model |
| requests | u64 | Y | request count |
| ok | u64 | Y | successful count |
| errors | u64 | Y | error count |
| tokens_in | u64 | Y | fresh input tokens |
| tokens_out | u64 | Y | output tokens |
| cache_read | Option<u64> | N | cache read tokens |
| cache_creation | Option<u64> | N | cache creation tokens |
| last_used_ms | u64 | Y | last completed request time |
| in_flight | u32 | D | active request count |
| accounts | ModelAccountDoc[] | D | account contribution breakdown |
| efforts | ModelCountDoc[] | D | effort/reasoning distribution |
| endpoints | ModelCountDoc[] | D | endpoint distribution |

Recommended additive field:

```rust
#[serde(default)]
pub cost_usd: f64
```

Reason: TUI currently can calculate model cost via `src/pricing.rs`; Islands should not need to duplicate pricing logic.

Derived metrics:

```text
total_tokens =
  tokens_in + tokens_out + cache_read.unwrap_or(0) + cache_creation.unwrap_or(0)

cache_hit_rate =
  cache_read / max(tokens_in + cache_read, 1)

error_rate =
  errors / max(requests, 1)
```

---

## 2.8 `ModelAccountDoc`

Per-account contribution inside one model row.

| Field | Type | Req | Meaning |
|---|---|---:|---|
| name | string | Y | account name |
| requests | u64 | Y | request count |
| ok | u64 | Y | success count |
| errors | u64 | Y | error count |
| tokens_in | u64 | Y | input tokens |
| tokens_out | u64 | Y | output tokens |

---

## 2.9 `ModelCountDoc`

Labelled count for effort or endpoint distribution.

| Field | Type | Req | Meaning |
|---|---|---:|---|
| label | string | Y | effort label or endpoint class |
| requests | u64 | Y | request count |

Examples:

```text
efforts:
  none: 10
  high: 3

endpoints:
  messages: 12
  count_tokens: 1
```

---

## 2.10 `ClientUsageDoc`

Usage grouped by `metadata.user_id`.

| Field | Type | Req | Meaning |
|---|---|---:|---|
| client | string | Y | user_id or `unknown` |
| requests | u64 | Y | request count |
| ok | u64 | Y | success count |
| errors | u64 | Y | error count |
| tokens_in | u64 | Y | input tokens |
| tokens_out | u64 | Y | output tokens |

Derived:

```text
total_tokens = tokens_in + tokens_out
error_rate = errors / max(requests, 1)
```

Recommended additive fields:

```rust
#[serde(default)]
pub cost_usd: f64,
#[serde(default)]
pub last_seen_ms: u64,
```

---

## 2.11 `WindowedStatsDoc`

24h/72h heatmap data.

| Field | Type | Req | Meaning |
|---|---|---:|---|
| window | string | Y | `24h` or `72h` |
| window_secs | u64 | Y | window duration |
| cells | WindowedCellDoc[] | Y | heatmap cells |

Important display rule:

```text
windowed is best-effort, not an exact billing ledger.
```

---

## 2.12 `WindowedCellDoc`

| Field | Type | Req | Meaning |
|---|---|---:|---|
| group | string | Y | provider group |
| model | string | Y | model |
| account | string | Y | account |
| requests | u64 | Y | request count |
| ok | u64 | Y | success count |
| errors | u64 | Y | error count |
| tokens_in | u64 | Y | input tokens |
| tokens_out | u64 | Y | output tokens |
| cache_read | u64 | D | cache read tokens |
| cache_creation | u64 | D | cache creation tokens |
| tokens | u64 | Y | heatmap intensity |

Calculation:

```text
tokens = tokens_in + tokens_out + cache_read + cache_creation
```

---

## 2.13 `ActivityDoc`

Real-time activity tail.

| Field | Type | Req | Meaning |
|---|---|---:|---|
| in_flight | InFlightDoc[] | Y | active requests |
| completed | CompletedDoc[] | Y | recent completed requests |

### `InFlightDoc`

| Field | Type | Req | Meaning |
|---|---|---:|---|
| id | u64 | Y | request id |
| method | string | Y | HTTP method |
| path | string | Y | endpoint |
| account | string? | N | routed account |
| started_at_ms | u64 | Y | start time |
| group | string? | N | routed group |
| model | string? | N | served model |

### `CompletedDoc.Request`

| Field | Type | Req | Meaning |
|---|---|---:|---|
| at_ms | u64 | Y | completion time |
| method | string | Y | HTTP method |
| path | string | Y | endpoint |
| account | string? | N | account |
| status | u16 | Y | HTTP status |
| duration_ms | u64 | Y | request duration |
| tokens | TokensDoc? | N | input/output tokens |
| cost_usd | f64 | D | request cost |
| group | string? | N | group |
| model | string? | N | model |
| effort | string? | N | reasoning effort |

---

# 3. TUI UI schema

The TUI is an operational cockpit. Prefer tables, sorting, keyboard traversal, detail panes, and compact bars.

## 3.1 Existing TUI surfaces to use

```text
main cockpit
stats overlay
models view
sessions overlay
activity/log panel
account detail
```

## 3.2 Main cockpit

Purpose:

```text
Show accounts, quota, current account, global requests/tokens/cost, and recent activity.
```

Data:

```text
DashboardDoc.accounts
DashboardDoc.totals
DashboardDoc.scheduler
DashboardDoc.activity
DashboardDoc.model_usage
```

Recommended account table:

```text
ACCOUNT | GROUP | STATUS | 5H | 7D | IN-FLIGHT | REQ | TOK | BLOCKED
```

Recommended model strip:

```text
TOP MODELS
claude / opus        ███████  120k tok  34 req
codex  / gpt-5.5     ████      72k tok  18 req
```

---

## 3.3 Models view

Purpose:

```text
Compare model usage, errors, cache, cost, and active traffic.
```

Data:

```text
DashboardDoc.model_usage
```

Table columns:

```text
GROUP | MODEL | REQ | OK | ERR | LIVE | IN | OUT | CACHE | TOK | COST | LAST
```

Detail pane:

```text
selected model:
  accounts[]
  efforts[]
  endpoints[]
  cache_read/cache_creation
```

Sort options:

```text
default: total_tokens desc
optional: requests desc
optional: errors desc
optional: last_used desc
optional: cost desc
```

---

## 3.4 Stats overlay

Purpose:

```text
Dedicated statistics view.
```

Data:

```text
DashboardDoc.model_usage
DashboardDoc.client_usage
DashboardDoc.windowed
DashboardDoc.totals
```

Sections:

```text
1. model table
2. client usage panel
3. 24h/72h model/account heatmap
4. global totals
```

Display rule:

```text
Windowed heatmap must show a "best effort" label.
```

---

## 3.5 Sessions overlay

Purpose:

```text
Show session timeline grouped by metadata.user_id.
```

Existing basis:

```text
src/session.rs
src/proxy/raw_io.rs
src/tui/ui.rs
```

Session row:

```text
SESSION | CONF | SPAN | REQ | TOKENS | MODELS | ACCOUNTS | ROTATIONS
```

Security rule:

```text
Only metadata is shown. Never display raw request_body or response_body content.
```

---

## 3.6 Activity detail

Purpose:

```text
Inspect recent requests by status, latency, token, cost, model, and account.
```

Data:

```text
DashboardDoc.activity.completed
DashboardDoc.activity.in_flight
```

Columns:

```text
TIME | METHOD | PATH | STATUS | ACCOUNT | MODEL | TOKENS | COST | DUR
```

---

# 4. Islands SwiftUI UI schema

Islands is a glanceable UI. Use cards, tiles, compact charts, and sheets.

## 4.1 Current state

Current flow:

```text
IslandUsageModel.refresh()
  -> client.status()
  -> LlmuxStatus.accounts
  -> UsageAccountTile[]
```

Target flow:

```text
IslandUsageModel.refresh()
  -> client.dashboard()
  -> LlmuxDashboard.accounts
  -> UsageAccountTile[]
  -> totals/model_usage/client_usage/windowed/activity
```

---

## 4.2 Swift DTO additions

Add to `llmux-islands/LlmuxIslands/Llmux/`.

```swift
struct LlmuxDashboard: Decodable {
    let version: String
    let port: Int
    let uptimeSecs: UInt64
    let current: String?
    let currentByGroup: [String: String]
    let accounts: [LlmuxDashboardAccount]
    let totals: LlmuxDashboardTotals
    let modelUsage: [LlmuxDashboardModelUsage]
    let clientUsage: [LlmuxDashboardClientUsage]
    let windowed: [LlmuxDashboardWindowed]
    let activity: LlmuxDashboardActivity
    let emailAnonymous: Bool?

    enum CodingKeys: String, CodingKey {
        case version, port, current, accounts, totals, windowed, activity
        case uptimeSecs = "uptime_secs"
        case currentByGroup = "current_by_group"
        case modelUsage = "model_usage"
        case clientUsage = "client_usage"
        case emailAnonymous = "email_anonymous"
    }
}
```

Add to `LlmuxClient.swift`:

```swift
func dashboard() async throws -> LlmuxDashboard {
    let data = try await send(makeRequest("/llmux/dashboard"))
    return try JSONDecoder().decode(LlmuxDashboard.self, from: data)
}
```

---

## 4.3 Islands ViewModel additions

Add to `IslandUsageModel`:

```swift
@Published var dashboard: LlmuxDashboard?
@Published var totalRequests: UInt64 = 0
@Published var totalTokens: UInt64 = 0
@Published var totalCostUSD: Double = 0
@Published var topModels: [LlmuxDashboardModelUsage] = []
@Published var clientUsage: [LlmuxDashboardClientUsage] = []
@Published var windowed: [LlmuxDashboardWindowed] = []
```

Calculation:

```swift
totalTokens = totals.tokensIn + totals.tokensOut
topModels = modelUsage.sorted(by: totalTokens desc).prefix(3)
```

---

## 4.4 Closed Island

Purpose:

```text
Small always-visible status.
```

Data:

```text
accounts[].in_flight
totals.cost_usd
totals.tokens_in/out
```

Example:

```text
llmux  C:2  X:1  $0.42
```

Rules:

```text
C = sum in-flight where group=claude
X = sum in-flight where group=codex
warning color if any auth_failed account or quota > 90%
```

---

## 4.5 Expanded Island

Purpose:

```text
Account quota + core analytics cards.
```

Sections:

```text
Header
  connection state
  refresh button
  email anonymous toggle

Summary cards
  requests
  tokens
  cost
  error rate

Account tiles
  existing UsageAccountTileGrid

Top models
  top 3 ModelUsage rows

Heat strip
  windowed 24h or 72h compact cells

Recent activity
  last completed requests
```

---

## 4.6 Full usage popover

Recommended tabs:

```text
Overview
Models
Clients
Health
```

### Overview

Data:

```text
DashboardDoc.totals
DashboardDoc.accounts
DashboardDoc.model_usage
DashboardDoc.windowed
```

Cards:

```text
Total Tokens
API-equivalent Cost
Requests
Errors
In-flight
Top Model
```

### Models

Data:

```text
DashboardDoc.model_usage
```

Display:

```text
model cards/list
group badge
tokens
requests
errors
in-flight
cache read/create
cost
```

Tap behavior:

```text
model card tap -> model detail sheet
```

### Clients

Data:

```text
DashboardDoc.client_usage
```

Display:

```text
client
requests
tokens
errors
```

### Health

Data:

```text
DashboardDoc.accounts
DashboardDoc.poller
DashboardDoc.scheduler
DashboardDoc.codex
```

Display:

```text
account quota cards
poller health
last switch
next account
codex model/fast/effort
```

---

# 5. Common drilldown model

## Shared entity navigation

```text
account -> account detail
model -> model detail
client -> client detail
activity row -> request detail
quota warning -> health
heatmap cell -> filtered model/account view
```

## TUI

```text
main cockpit
  -> models view
  -> stats overlay
  -> sessions overlay
  -> activity/log panel

models view
  -> selected model detail pane

sessions overlay
  -> selected session detail

stats overlay
  -> model/client/windowed inspection
```

## Islands

```text
closed island
  -> expanded island

expanded island
  -> full usage popover

overview card
  -> matching tab

model card
  -> model detail sheet

account tile
  -> account detail sheet

warning banner
  -> health tab
```

---

# 6. Implementation phases

## Phase 1: Switch Islands to `/llmux/dashboard`

Files:

```text
llmux-islands/LlmuxIslands/Llmux/LlmuxStatus.swift
llmux-islands/LlmuxIslands/Llmux/LlmuxClient.swift
llmux-islands/LlmuxIslands/Llmux/IslandUsageModel.swift
```

Tasks:

```text
1. Add LlmuxDashboard DTO.
2. Add LlmuxClient.dashboard().
3. Use dashboard() in IslandUsageModel.refresh().
4. Keep existing tile generation from dashboard.accounts.
5. Publish totals/model_usage/client_usage/windowed/activity.
```

## Phase 2: Add Islands Overview analytics

Files:

```text
llmux-islands/LlmuxIslands/UI/Views/IslandUsageView.swift
llmux-islands/LlmuxIslands/Dashboard/UsageTiles.swift
```

Display:

```text
total requests
total tokens
total cost
error rate
top models
recent activity
```

## Phase 3: Add per-model cost to document

Files:

```text
src/dashboard.rs
src/tui/activity.rs
src/tui/view.rs
src/tui/ui.rs
src/pricing.rs
```

Recommended additive field:

```rust
#[serde(default)]
pub cost_usd: f64
```

Reason:

```text
TUI can calculate model cost in render path today.
Islands should receive the same value from the server document instead of duplicating pricing logic.
```

## Phase 4: Add data-quality labels

Files:

```text
src/dashboard.rs
src/tui/ui.rs
llmux-islands/LlmuxIslands/UI/Views/IslandUsageView.swift
```

Labels:

```text
model usage: hydrated activity/runtime
windowed: best effort
cost: API-equivalent estimate
cache: missing fields shown as unavailable
```

---

# 7. Final recommendation

## Statistics contract

Keep `DashboardDoc` as the central contract.

Minimum additive fields:

```rust
ModelUsageDoc.cost_usd
DashboardDoc.data_quality
```

## TUI

Use existing TUI surfaces:

```text
main cockpit: account/quota/current/rpm/activity
models view: model_usage 중심
stats overlay: model + client + windowed
sessions overlay: raw-io metadata session fold
activity panel: request tail
```

## Islands

Expand from `/llmux/status` to `/llmux/dashboard`:

```text
closed island: in-flight + warning + optional cost/tokens
expanded island: account tiles + KPI + top models
full popover: overview / models / clients / health
```

## First implementation steps

```text
1. Add LlmuxDashboard DTO.
2. Add LlmuxClient.dashboard().
3. Change IslandUsageModel.refresh() to use dashboard.
4. Publish totals/model_usage/windowed into SwiftUI.
5. Add ModelUsageDoc.cost_usd as an additive Rust field.
```

This keeps TUI and Islands aligned on the same `DashboardDoc` semantics while allowing each surface to render in its own UX style.
