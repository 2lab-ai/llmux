# PRD/decision record — perf telemetry v1 + config editor (2026-07-17)

Status: **shipped** — PR [#128](https://github.com/2lab-ai/llmux/pull/128)
(perf telemetry, perf tab, sessions interactions) + PR
[#129](https://github.com/2lab-ai/llmux/pull/129) (mouse-editable config tab,
misc daemon facts), preview `2026-07-17-1405-aab485ff55e8`. Operator how-to
lives in [docs/operational-reference.md](../docs/operational-reference.md)
("Perf tab", "Config tab") and [docs/configuration.md](../docs/configuration.md);
this file records the design decisions and their reasoning. The contract below
was settled by a 3-engine review panel (grok-4.5 / gpt-5.6 / fable) run to
unanimity — 5 rounds for the plan, 5 merge-gate rounds per PR.

## Problem

llmux proxied every request but kept no timing story: no answer to "how fast
is this provider/model *actually*, day over day, and is fast-mode worth it".
And the config tab was a read-only listing — every knob change meant editing
`llmux.json` by hand and restarting, with no statement of which settings even
could apply live.

## Decisions — perf telemetry (#128)

1. **Two capture points, honestly named.** `ttfb_ms` = upstream dispatch →
   first successful body chunk. `ttft_ms` = dispatch → first non-empty
   `content_block_delta` of ANY delta type (text / thinking / partial_json) —
   thinking counts as generation because the numerator (`output_tokens`)
   includes it. Derived throughput is labeled **"est"** everywhere; the terms
   "generation speed" / "decode speed" are banned from the UI because hidden
   reasoning makes a true decode rate unobservable from the wire.
2. **`gen_ms` is measured inside the stream pump**, first delta → stream EOF,
   not reconstructed from request-level timestamps — request-start baselines
   contaminate the span with queueing and conversion time.
3. **Aggregation keeps raw sums** per `(day, group, model, fast)`:
   Σoutput / Σms, never an average of per-request rates. Confidence is
   per-statistic (`n < 5` → dimmed, still shown — low traffic is a signal);
   quiet days render as chart gaps and `—` rows, never fabricated zeros.
4. **`fast` is a three-state dimension** (`Some(true)` / `Some(false)` /
   `None` = legacy lines predating the field). Pre-field history must not be
   forged into "off"; Unknown is its own series and never mixes.
5. **Aborted** = transport break + protocol failure (`response.failed`,
   SSE `error` event) **minus client disconnects** — a client that hangs up
   must not poison the provider's error rate or its measured throughput.
6. **Persistence is replay-shaped**: additive `#[serde(default)]` fields on
   `PersistedRequest`, daily cells rebuilt by the same fold as live events,
   90-day retention behind wall-clamped high-water marks (same discipline as
   `.prd/14`). Attach clients get the rows on `GET /llmux/dashboard`
   (`daily_perf`).
7. **New `perf` tab, not a Stats extension** — chart (braille, gap +
   confidence segments), date×provider health matrix (n / err% / ttfb / e2e /
   est), series table with single-day drill-down. Named "observed
   performance": passive telemetry, deliberately not an active healthcheck.
8. Sessions rows carry an e2e `t/s` column from raw-io `duration_ms`; every
   activity row shows `t/s`, and the expanded detail shows
   `e2e / est / ttfb / first output / aborted`.

## Decisions — config editor (#129)

1. **The acceptance denominator is the whole `schema.rs`.** Every config leaf
   is classified — live-editable / restart-required / session-only /
   read-only-with-reason — and the classification is machine-enforced: the
   reconciliation test destructures every schema struct with no rest pattern
   (a new field anywhere fails to compile until classified) and matches
   leaves exactly (`==`; only runtime-keyed collections use bounded
   `prefix.` matches).
2. **Persist-first, then flip.** `apply_settings` validates ALL requested
   changes, writes the config once (read-merge-write), then flips the
   `SettingsLive` atomic holders. Endpoints refuse (500) when the daemon has
   no config path — a pathless daemon must never mutate live shape behind a
   200. Live-apply claims are backed by actual consumers reading the holders
   (the per-request `AppState.config` snapshot is not enough).
3. **The ack is typed on both sides.** The daemon returns
   `SettingsAck { ok, applied, restart_required }`; the TUI parses it BEFORE
   any local state moves — a malformed / empty / `ok=false` 2xx is reported
   as unverified and mutates nothing. A restart-required change is labeled
   `saved: … (restart)` in the tab; it never masquerades as applied.
   Compat: the ack keeps the legacy `email_anonymous` echo because the
   shipped Islands client rejects acks without it (additive wire evolution,
   not contract dilution).
4. **Click target = the value cell only**, blast-radius confirms for the
   dangerous edits (scheduler mode, routing enable, raw-io enable — spells
   out that full payloads hit disk —, upstream/remote, ceilings→0, retention
   decrease), input mode for numbers/strings with type+range validation.
5. Misc tab became the daemon-facts surface (config path, accounts, raw-io
   state + file sizes on a 30s cache — no fs stat per render) and the
   keybinding reference for the new surfaces.

## What we refused

- Per-request average rates (Simpson's-paradox-prone) — sums only.
- A fixed per-sample ms floor (systematically deletes the fastest healthy
  samples; survivor bias in provider comparison).
- "Healthcheck" naming for passive observation.
- Whole-row click targets and confirm-everything modals — confirm gates are
  sized to blast radius.
- Editing collections (accounts, pricing, events, per-account limits) from
  the config tab — each already has a dedicated surface or is config-file
  domain; the rows say so instead of pretending.

## Verification

Both PRs: `just check` green (fmt + clippy -D warnings + full tests), 5/5 CI,
3-engine unanimous merge gates. Notable regression tests: legacy-line
`fast: None` classification, replay/live fold equality, chart gap vs dim
segments, deterministic client-disconnect (channel-overflow) case, schema
reconciliation compile+leaf gates, attach settings round-trips against a real
loopback responder (success / ok=false / malformed 2xx / 4xx / 5xx).
