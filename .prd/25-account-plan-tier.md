# Account plan tier — llmux reports each account's subscription plan

Status: **shipped** — v0.2.26 (PR #219 `b8e490f`; stable v0.2.26 + preview-2026-10-26-1105-b8e490ffdfd3), 2026-10-26.
This document records the implemented contract, not a future target.

## Problem

`/llmux/status` carries no subscription plan. Since the earliest import path its only plan hint is
the credential's bare `subscriptionType` (`max`), which cannot tell Claude Max x5 from Max x20.
Codex accounts carry none. A consumer that weighs remaining quota by plan size (the soma-work
dashboard counts remaining usage in "max accounts' worth") therefore needs a hand-written
`account → plan` map on every host. On 2026-10-26 a dev host without that map showed its whole
Claude pool as uncountable.

## Sources (measured 2026-10-26, real accounts, no token printed)

- **Claude:** `GET {upstream}/api/oauth/profile` (Bearer, `anthropic-beta: oauth-2025-04-20`)
  returns `organization.rate_limit_tier` = `default_claude_max_20x` and
  `organization.organization_type` = `claude_max` for each Max x20 account tried (4 of 4).
  llmux already calls this endpoint at login and for reset redemption, but parses only
  `account.has_claude_max` / `has_claude_pro` (→ the bare `max`).
- **Codex:** the ChatGPT access token is a JWT whose `https://api.openai.com/auth` claim carries
  `chatgpt_plan_type`. `GET /wham/usage` reports the same value as `plan_type`. Measured values:
  `pro` (the $200 plan) and `promax` (the $500 plan); 5 of 5 accounts agree between the two
  sources. The claim needs no network call.

## Goals

1. `/llmux/status` reports each account's plan, additively per account:
   - `plan` — a short display label: `max20`, `max5`, `pro`, `promax`, other known plan words
     passed through lowercase; absent when unknown.
   - `rate_limit_tier` — Claude only, the upstream string verbatim (`default_claude_max_20x`).
   - `plan_type` — Codex only, the upstream string verbatim (`pro`, `promax`).
2. `llmux status` prints the plan next to the remaining weekly usage on every account line.
3. The TUI account detail pane names the plan on its first line.
4. No configuration is needed: the daemon learns plans by itself and keeps them current.

## Non-goals

- No change to scheduling, ranking or eligibility: the plan is display/consumer data only.
- No persistence in the config file. The plan is daemon state, re-learned on start.
- No guessing: a bare `max` is never turned into `max5` / `max20`.
- The accounts table layout is unchanged (its column budget is pinned by `.prd/18`/`19`).

## Contract

- **Claude:** a background task fetches `/api/oauth/profile` for each oauth account whose access
  token is valid (more than 60 s left; an expired or unknown-expiry token is never sent), once at
  daemon start (off the readiness path), then every 10 minutes while any Claude or Codex account
  still has no plan (expired token at start, failed read, fresh login), and every 6 h once all are
  known. A failed or incomplete fetch keeps the last known plan; it never clears it.
- **Codex:** the plan is read from the access token's claim whenever the pass runs, so a token
  refresh that changes the plan is picked up on the next pass. No signature check — the value is
  display-only and comes from the token llmux already holds.
- **Other providers:** no plan (grok, openrouter, gemini, api keys).
- The store is keyed by account name; removed accounts drop out on the next pass.
- Fields are additive: an older client ignores them, a consumer of an older daemon sees none.

## Acceptance

| Scenario | Observable |
| --- | --- |
| Claude Max x20 account, valid token | status `plan` = `max20`, `rate_limit_tier` = `default_claude_max_20x` |
| Codex $500 account | status `plan` = `promax`, `plan_type` = `promax` |
| Codex $200 account | status `plan` = `pro`, `plan_type` = `pro` |
| profile fetch fails | previous plan kept; never-known stays absent |
| `llmux status` | each line shows `<plan>` and `7d <N>% left` |
| TUI detail pane | first line ` <name> · <kind> · <plan>` |

## Observed on deployment (2026-10-26)

- Two daemons upgraded to 0.2.26: every Claude Max account reads `max20 · 7d N% left`; Codex `pro` /
  `promax` match the operator's own plan list. One Claude account reports
  `rate_limit_tier = default_raven` — not a Max plan — so it carries no label (no guessing), exactly
  as the contract says; consumers see the raw value.
