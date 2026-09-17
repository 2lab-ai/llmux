# Claude Code `/model` picker from the llmux catalog

Status: shipped
Date: 2026-09-17

## Problem and target

`llmux run` spawns `claude` with `ANTHROPIC_BASE_URL` pointed at the proxy
(`src/cli/run.rs`), but Claude Code's `/model` picker still shows only the
built-in Anthropic lineup. Every non-Claude model llmux routes (`gpt-5.6-sol`,
`gpt-6-astra`, `grok-4.6`, `or-*`) is reachable only by typing `/model <id>` from
memory. The catalog that knows these ids already exists (`src/catalog.rs`,
served by `GET /models` / `GET /llmux/models`, documented in `docs/models.md`);
it just never reaches the picker.

Target: `llmux run` makes the picker list the llmux catalog — every curated row
plus the live grok/openrouter pins — labelled by backend group, without
replacing the built-in rows and without touching the user's settings files.

Reference the user pointed at: `gargpratyush/jev-router`. Analysed at
`a5694a6` (2026-09-17): it has no catalog-change feature (a 4-row source
constant, edited by editing source, no `/v1/models`, no reload). The one
transferable mechanism is that it injects a custom row into Claude Code's
picker via `ANTHROPIC_CUSTOM_MODEL_OPTION*` env at spawn time
(`bin/jev-claude.mjs:21-33`). That mechanism carries ONE row; llmux needs
many, so it uses Claude Code's `modelPicker` setting instead (below).

## Research and evidence

Claude Code 2.1.274 is installed locally (`claude --version`, 2026-09-17).
Official docs fetched 2026-09-17:

- `code.claude.com/docs/en/settings-reference#modelpicker`: "List the models
  the `/model` picker offers, in the order you write them and under labels you
  choose … Each row's `model` is taken verbatim, so it accepts anything
  `--model` accepts … Requires Claude Code v2.1.242 or later." Scope: "Claude
  Code reads the key from managed settings, `--settings`, and user settings, and
  ignores it in project and local settings". Shape: `{"modelPicker": {"options":
  [{"model": …, "label"?: …, "description"?: …}], "replaceBuiltInOptions"?:
  bool}}`. "With it off, Claude Code skips a listed model that the built-in
  lineup already covers." "Claude Code drops a row it can't parse and keeps the
  rest." Rows it "can't serve" are dropped and rows it can't select are greyed.
  A non-Claude id does survive behind a custom `ANTHROPIC_BASE_URL`: verified
  live 2026-09-17 (Claude Code v2.1.274, worktree binary in `--remote` mode,
  `.prd/model-picker/loop.md` gap matrix) — "Grok 4.6" was listed, selectable,
  and the turn was routed to group `grok`.
- `code.claude.com/docs/en/model-config`: "Claude Code skips validation for the
  model ID set in `ANTHROPIC_CUSTOM_MODEL_OPTION`"; "behind an LLM gateway or a
  custom `ANTHROPIC_BASE_URL`, your provider or gateway defines the model names,
  so Claude Code passes any string through without checking it."
- `code.claude.com/docs/en/llm-gateway-protocol#model-discovery`: gateway
  discovery (`CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1`, `GET
  /v1/models?limit=1000`, 3 s timeout) "keeps an entry when its `id` contains
  `claude` or `anthropic`" and "when neither credential header's value
  resolves, Claude Code skips discovery". Both rule it out for llmux: the
  non-Claude ids are the point, and local `llmux run` deliberately exports no
  credential (subscription mode, `src/cli/run.rs:18-33`). Not used.

## Contract

1. `llmux run` (local and remote) fetches the catalog from the proxy it is
   about to point `claude` at — `GET {base_url}/llmux/models`, same gate as
   every other route, short timeout — and turns each `ModelEntry` into one
   `modelPicker` row:
   - `model`: the entry `id` verbatim (the string llmux routes on; `[1m]`
     suffixes included — `provider::anthropic` strips them upstream).
   - `label`: the entry `name`.
   - `description`: `"<group> · efforts <low…high> · ctx <max_context>"`,
     omitting parts that are empty/null. Group names are the catalog's
     (`claude`, `codex`, `grok`, `openrouter`).
   - Order = catalog order. `replaceBuiltInOptions` stays unset (built-in rows
     remain; Claude Code de-duplicates ids the built-in lineup already covers).
2. The lineup is passed as `--settings '<json>'` prepended to the pass-through
   args. It is NOT written to any settings file.
3. Not injected when: the pass-through args already carry `--settings` (the
   user's lineup wins; llmux prints one warning line), `llmux run
   --no-model-picker` is given, or the catalog fetch fails (warning line, launch
   continues unchanged — the picker must never block a launch).
4. `docs/models.md` gains a "Claude Code picker" section; `docs/cli.md` (or
   wherever `llmux run` is documented) gains the flag and the `--settings`
   precedence rule.

Acceptance (execute → expected observation):

- `llmux run` then `/model` → the picker shows rows labelled with llmux catalog
  names (e.g. "Claude Fable 5.1", "GPT-5.6 Sol", "Grok 4.6", the openrouter
  pin) after the built-in rows; selecting "Grok 4.6" and sending a turn →
  llmux activity log shows a request on group `grok`, model `grok-4.6`.
- `llmux run -- --settings '{"modelPicker":{"options":[]}}'` → llmux prints the
  precedence warning and passes the user's `--settings` through unchanged.
- `llmux run --no-model-picker` → `claude` is spawned with no `--settings`.
- Daemon unreachable for the catalog fetch → one warning, `claude` still
  starts.

## Implementation (WU 1, 2026-09-17)

Landed on `feat/run-model-picker`. The live receipt (picker screen + grok
activity line) was taken 2026-09-17 against the running daemon from the
worktree binary in `--remote localhost:3456` mode (see loop.md gap matrix);
Shipped: PR #161 squash-merged as main 464f5bb, deployed as
`preview-2026-09-17-0944-464f5bb7ab85`, and re-verified with the installed
binary (`/model` shows the catalog rows). This document describes the
implemented contract, not a future target.

- `src/cli/run.rs`: `model_picker_settings` (pure row builder),
  `row_description`, `has_user_settings` / `injects_model_picker` (pure
  decisions), `fetch_catalog` (3 s cap, `x-api-key` like `probe_server`,
  sanitized error strings — never the key, never the body) and `picker_args`
  (the IO-at-the-edge wrapper that warns and degrades to no injection).
  `--settings <json>` is prepended to the pass-through args, so every other
  flag the user passes still has the last word.
- `src/cli/mod.rs`: `RunArgs::no_model_picker`.
- `src/catalog.rs` unchanged: `ModelEntry::efforts` is `&'static [&'static
  str]` and cannot be `Deserialize`, so the CLI carries its own `CatalogRow`
  mirror. `model_picker_settings_round_trips_the_real_catalog` parses the
  document `catalog::catalog` actually serializes, so a field rename upstream
  fails a test instead of silently emptying the picker.
- Effort rendering is the menu's ENDS (`efforts low…max`), per the
  `<low…high>` form in the contract above — not the full menu.
- Docs: contract §4 named `docs/cli.md`, which does not exist in this repo —
  `llmux run` is documented in `docs/operational-reference.md` (§Running Claude
  Code through llmux) and that paragraph plus a new `docs/models.md` section
  ("Claude Code `/model` picker") and one README clause carry the flag, the row
  shape and the precedence rule.
- Gate: `just check` green (fmt + clippy `-D warnings` + `cargo test`, 1237 lib
  tests, +8 new in `cli::run::tests`).

## Non-goals

- Editing the catalog from config (the catalog stays the curated source table
  in `src/catalog.rs`; a config override is a separate spec if ever wanted).
- Serving `/v1/models` from the catalog (still proxied upstream).
- Merging llmux rows into a user-supplied `--settings` document.
