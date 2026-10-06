# Claude usage-limit reset grants

Status: shipped (read + redeem paths; redeem fixture-tested, not live-executed)
Date: 2026-10-06
Extends: [16-codex-usage-controls.md](16-codex-usage-controls.md)

## Problem and target

The reset-credit controls of `.prd/16` were Codex-only: `rst` column, detail
line, `llmux accounts resets/reset`, the TUI `R` gate and the daemon
`/llmux/reset-credits*` routes all refused non-codex accounts with 422.
claude.ai meanwhile ships the same entitlement for Pro/Max accounts — Settings
→ Usage → "제한 초기화" (Limit resets): a card per grant with its **expiry**
("만료일: 10월 23일") and a one-click reset.

Target: the same llmux surfaces show, for Claude accounts, whether the account
holds reset grants, each grant's expiry, and let the operator redeem one with
the same idempotent, receipt-backed flow Codex has. Nothing is redeemed
automatically; this change redeemed nothing while being built.

## Research and evidence (all 2026-10-06)

- **Wire source.** The claude.ai web app loads the card from
  `GET /api/organizations/{org}/usage?cedar_ember=1&skip_spend=1` — the
  ordinary usage document with an extra `cedar_ember` object (the program's
  internal name; the react-query keys are `qk_cedar_ember_01/02`). Captured
  live via the logged-in browser (React props of the `#resets` settings
  section + a same-origin fetch); the shape below is verbatim minus
  identifiers:

  ```json
  "cedar_ember": {
    "eligible": true, "ineligible_reason": null, "at_limit": false, "exhausted": [],
    "grants": [{
      "id": "opus55-launch-promax-20260921",
      "label": "Claude Opus 5.5 launch: one usage-limit reset for Pro and Max",
      "resets_total": 1, "resets_left": 1,
      "starts_at": "2026-09-22T16:00:00+00:00", "ends_at": "2026-10-22T16:00:00+00:00",
      "clears": ["five_hour", "seven_day", "seven_day_overage_included"],
      "paused": false, "usable_now": true, "use_requires_limit": false,
      "percent_used": {"five_hour": 2, "seven_day": 0, "seven_day_overage_included": 0},
      "blocking": [], "arm": null
    }],
    "next_grant_id": "opus55-launch-promax-20260921",
    "weekly_resets_at": "2026-10-10T09:00:00+00:00", "cooldown_until": null,
    "event_props": {"surface": "claude_ai", "tier": "claude_max_20x", ...}
  }
  ```

- **OAuth parity.** `GET https://api.anthropic.com/api/oauth/usage?cedar_ember=1`
  with an llmux OAuth bearer returns the same object. Without the flag the key
  is `null`.
- **The surface gate (load-bearing).** A bare bearer request answers
  `{"eligible":false,"ineligible_reason":"surface","grants":[]}` on every
  account probed (4 accounts, 2 orgs). Header isolation:
  - `anthropic-client-platform`, `Origin`, `Referer`, `x-app: cli`,
    `anthropic-beta`, `anthropic-version` — alone or combined: still `surface`.
  - `User-Agent: claude-cli/2.1.47 (external, cli)` → `ineligible_reason:
    "cli_version"` (the gate parsed the UA and rejected the version).
  - `User-Agent: claude-cli/2.1.291 (external, cli)` (the installed Claude Code
    that day) → `eligible:true` with the grant above and
    `weekly_resets_at: 2026-10-12T22:00:00+00:00`. `x-app`/beta headers
    neither helped nor hurt.
  Consequence: the Claude Code **version string is an input** —
  `DEFAULT_CLAUDE_CLI_VERSION` in `src/auth/claude_resets.rs`, overridable by
  the `claude_cli_version` config key without a rebuild. A `cli_version`
  answer is shown verbatim (`rst` = `n/e`, detail names the reason).
- **Rate-limit behavior.** Under a burst of probes the endpoint kept answering
  200 but with `cedar_ember.eligible: null` / `grants: []`, then 429. The
  parser treats a null/absent `eligible` as UNKNOWN (counts stay `None`), never
  as "no grants"; a 429 is an upstream error, never an empty success.
- **Redemption route** (from the claude.ai bundle, `shared-0-*.js` and the
  settings/wall chunks; **not executed live**):
  `POST /api/organizations/{org_uuid}/reset_rate_limits` with
  `{"program":"cedar_ember","grant_id":<id>,"request_id":<client ulid>}`.
  Response `{result, reason, reset, grant_id, resets_left, cleared[],
  weekly_resets_at, cooldown_until}`; `result ∈ {reset, already_used,
  not_limited, cooldown, ineligible, unavailable}`. The web client retries
  `unavailable` (and transport errors) with the SAME `request_id`, and treats
  429 as `rate_limited`. Refusal reasons include `grant_id_required`,
  `not_next_grant`, `blocked_by_other_limit`, `paused`, `expired`,
  `unknown_grant`, `stamp_indeterminate`, `reset_unconfirmed`.
  On `api.anthropic.com` the route exists for the org path (GET → 405 Method
  Not Allowed, i.e. POST-only) and not under `/api/oauth/` (404).
- **Organization id.** `GET /api/oauth/profile` returns
  `organization.uuid`; the Profile type now carries it (`org_uuid`). It is
  read at redemption time, before any receipt exists.
- **Grok** has no reset entitlement; API-key and OpenRouter accounts neither.
  They keep answering 422.

## Architecture decision

Project Claude grants onto the existing `ResetCredits` / `ResetCredit` wire
shape instead of a parallel type: `id→id`, `label→title`,
`starts_at→granted_at`, `ends_at→expires_at`, `reset_type =
"claude_rate_limits"`, and a derived `status` — `available` iff the grant is
live and the server says `usable_now` (what `is_redeemable` gates on), else
`blocked` / `paused` / `spent`. One list type means the CLI listing, the TUI
modal, the earliest-expiry picker and the fresh-inventory gate work unchanged
for both providers. `available_count` = Σ `resets_left` over live grants,
`applicable_count` = live grants with `usable_now` — **only when `eligible ==
Some(true)`**; otherwise both are `None` (unknown), and the eligibility
verdict is carried on the control doc (`UsageControlDoc.eligibility`) so an
ineligible account renders `n/e` + reason rather than `?`.

The explicit refresh for oauth accounts now reads `?cedar_ember=1` so the
`rst` column fills on `f` exactly as codex's does from its usage body; the
background poller is untouched (no header change, no extra query — the gate
answers `surface` to it, which the poller ignores because it never parses
`cedar_ember`).

Redemption reuses the whole `.prd/16` state machine (validate → try-lock →
pending receipt → fresh inventory → durable receipt → POST → terminal/uncertain),
switching only the final POST and adding the org-uuid read (a pure GET, before
the receipt). `ResetOutcome` gains `Cooldown` (terminal, nothing spent);
`unavailable` and unknown results are uncertain, same as codex's unknown code.

Expiry rendering (`tui::view::expiry_label`): absolute local time via the
TUI's existing `absolute_label` + `compact_duration` countdown, raw string
kept when unparseable. Used by the detail pane, the modal and the CLI listing
— for Codex credits too, which previously printed the raw RFC3339 only.

## Non-goals and residuals

- No automatic redemption; no scheduler coupling to grants.
- Live redemption through llmux is **unverified** — the user asked for no
  reset during this change. First live redeem should be done by an operator
  on one account with `llmux accounts reset <claude account>` and the result
  recorded here.
- The `arm` grant field and the web client's `at_wall=1` variant are not
  modeled.
