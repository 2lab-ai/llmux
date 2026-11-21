# Model catalog

llmux exposes the **known** models — a curated set plus the live grok and
openrouter pins — as a machine-readable catalog. This is deliberately not an
exhaustive list of everything routable: at request time the grok provider
forwards ANY `grok-*` id verbatim, the openrouter provider forwards any
`or-<vendor>/<slug>` verbatim and the gemini provider forwards any `gemini-*`
id verbatim (see [alias semantics](#alias-semantics)), so a
request or config pin naming an id outside the curated set still works. Such an
out-of-catalog pin appears here as a **synthesized row** with null metadata (see
[out-of-catalog grok pin](#out-of-catalog-grok-pin) and
[out-of-catalog openrouter pin](#out-of-catalog-openrouter-pin)). Gemini has no
pin, so it never synthesizes a row: its aliases are static (see [Gemini](#gemini)).

## Endpoints

- `GET /models`
- `GET /llmux/models`

Both return the **same** llmux metadata payload under the data-plane `x-api-key`
gate, with legacy keyless loopback access. `/llmux/models` is an explicit exception
to the admin-only control routes:

```json
{ "models": [ /* ModelEntry, ... */ ] }
```

`GET /v1/models` is a separate OpenAI-shaped catalog: `{ "object": "list",
"data": [{ "id": "…", "object": "model", "created": 0, "owned_by": "claude" }] }`.
It accepts Bearer or `x-api-key` llmux credentials and is served locally, not
proxied upstream. Use `/llmux/models` for efforts, aliases and context metadata.

## Response schema

Each element of `models` is a `ModelEntry`:

| Key           | Type                | Meaning                                                        |
| ------------- | ------------------- | ------------------------------------------------------------- |
| `id`          | string              | Concrete upstream model id.                                   |
| `aliases`     | array of strings    | Extra request slugs that resolve to this id (may be empty).   |
| `name`        | string              | Human-facing display name.                                    |
| `efforts`     | array of strings    | Accepted `reasoning.effort` values, low→high (may be empty).  |
| `max_context` | integer or `null`   | Context window in tokens; `null` when unpublished.            |
| `group`       | string              | Backend group: `claude`, `codex`, `grok`, `openrouter`, or `gemini`. |

`max_context: null` means the context window is not published for that id —
not that it is zero.

## Alias semantics

- **grok family alias** — `"grok"` is dynamic: it attaches to whichever grok id
  is the current live pin (`POST /llmux/grok` / `config.grok.default_model`). A
  bare `grok` request routes to that pin, so the catalog advertises the alias on
  exactly the pinned entry — matched on the FULL id, with no preference for a
  `[1m]` twin. Pinning `grok-4.7` therefore leaves the alias on the base row;
  pinning `grok-4.7[1m]` puts it on the twin, which is an explicit operator
  choice (see [The grok `[1m]` twin](#the-grok-1m-twin): llmux does not make
  the 1M-denominated id the advertised default of the family, because the
  denominator Claude Code infers from it overstates xAI's real 500k ceiling).
  Any `grok-*` id also passes through verbatim —
  after one trailing `[1m]` is stripped (`CLIENT_CONTEXT_SUFFIX` in
  `src/provider/grok.rs`), so `grok-4.7[1m]` reaches xAI as `grok-4.7` and still
  matches the per-model thinking-level table; `grok[1m]` resolves to the pin
  exactly like `grok`. The same strip applies to the PIN itself, so a pinned
  `grok-4.7[1m]` also leaves llmux as `grok-4.7` — the suffix is display
  metadata on both sides.
- **codex variant aliases** — `sol` / `terra` / `luna` / `astra` resolve to the
  NEWEST gpt generation that ships that variant (openai/codex `models.json`,
  read 2026-10-06, 2026-11-10 and 2026-11-18): `sol` → `gpt-6.2-sol` and
  `astra` → `gpt-6.2-astra` (both released 2026-11-17), `terra` →
  `gpt-6.1-terra` (2026-10-26) and `luna` → `gpt-6-luna` (2026-09-22).
  Generation 6.2 ships no terra or luna tier, and generation 6 shipped no terra
  tier, so `terra` stayed on `gpt-5.6-terra` until generation 6.1 shipped one.
  The bare generation ids resolve to that generation's flagship: `gpt-5.6` →
  `gpt-5.6-sol`, `gpt-6` → `gpt-6-astra`, `gpt-6.1` → `gpt-6.1-sol`, `gpt-6.2`
  → `gpt-6.2-sol`. `sol` moved from `gpt-5.6-sol` and `luna` from
  `gpt-5.6-luna` on 2026-10-06, `terra` from `gpt-5.6-terra` on 2026-11-10, and
  `sol` from `gpt-6.1-sol` and `astra` from `gpt-6-astra` on 2026-11-18 — the
  older rows stay in the catalog and remain addressable by full id.
  `astra` advertises on `gpt-6.2-astra[1m]` and the bare `gpt-6` id on
  `gpt-6-astra[1m]` — a deliberate asymmetry: on astra the bare aliases
  advertise the 1M row, so `astra` / `gpt-6` resolve to that row's upstream slug
  and to the ~1,050,000-token window this catalog publishes for it, while the
  explicit base ids `gpt-6.2-astra` / `gpt-6-astra` are the way to pick the
  openai/codex catalog's 272000. The 6.2-sol / 6.1-sol / 6.1-terra / 6-sol /
  6-luna pairs follow the 5.6 convention instead (aliases on the base row; the `[1m]`
  twin is an explicit opt-in). That is catalog resolution, **not** a client-side
  window: Claude Code sizes its readout off the id you submit, and a bare alias
  is an id it does not know. Claude Code 2.1.323 and later assume 1M for such an
  id (measured 2026-11-21 with 2.1.330 through the daemon: `astra` reports
  `modelUsage.contextWindow` 1000000); older clients assumed 200k, and only a
  `sol[1m]` / `astra[1m]` id (or the `[1M]` picker row) moved the client-side
  denominator. These are advertised statically on the corresponding entries. The provider always
  strips a trailing `[1m]` before the request leaves llmux
  (`CLIENT_CONTEXT_SUFFIX` in `src/provider/codex.rs`, mirrored in
  `src/provider/grok.rs`), so bare and suffixed aliases reach the backend as the
  same upstream slug.
- **claude aliases** — the following wire-ID table describes incoming Messages.
  Incoming Responses uses the Claude Agent SDK and preserves the resolved `[1m]`
  suffix so the SDK can apply its context beta; see [Codex frontend](codex-frontend/README.md).
  For Messages, the claude rows carry short user-curated aliases that
  both ROUTE to the claude group and are RESOLVED by the proxy: a bare alias is
  rewritten to its catalog id before the request leaves llmux, so the alias
  `GET /models` advertises is actually honored upstream.

  | request slug     | catalog id            | model on the wire  |
  | ---------------- | --------------------- | ------------------ |
  | `fable`, `fable-5-2` | `claude-fable-5-2[1m]` | `claude-fable-5-2` |
  | `fable-5-1`      | `claude-fable-5-1[1m]` | `claude-fable-5-1` |
  | `opus`, `opus-5-5` | `claude-opus-5-5[1m]` | `claude-opus-5-5` |
  | `opus-5`         | `claude-opus-5[1m]`   | `claude-opus-5`    |
  | `sonnet`, `sonnet-5-5` | `claude-sonnet-5-5[1m]` | `claude-sonnet-5-5` |
  | `sonnet-5`       | `claude-sonnet-5[1m]` | `claude-sonnet-5`  |
  | `haiku`, `haiku-5-5` | `claude-haiku-5-5[1m]` | `claude-haiku-5-5` |
  | `claude-haiku-4-5`, `claude-haiku-4-5-YYYYMMDD` (superseded 2026-10-23; Claude Code / Agent SDK–shaped requests with default sampling only) | `claude-haiku-5-5[1m]` | `claude-haiku-5-5` |

  Matching is trimmed and case-insensitive (`"  OPUS  "` resolves), and an
  alias may carry the client-side `[1m]` context suffix — `fable[1m]` resolves
  exactly like `fable`, because alias resolution runs before the suffix strip.
  The strip is syntactic; the resolved model's context limit still applies.
  `haiku[1m]` now resolves to `claude-haiku-5-5[1m]` (1M published context).
  Only aliases and, on Claude Code / Agent SDK–shaped requests with default
  sampling, the superseded Haiku 4.5 ids (see alias stability below) are
  rewritten: a real catalog id is not an alias and passes
  through untouched — the `[1m]` suffix strip is a separate, subsequent step,
  which is why `claude-opus-5[1m]` still reaches upstream as `claude-opus-5` —
  and foreign slugs (`grok-4.6`, `gpt-5.6-sol`) are never rewritten. The
  mapping has a single source, the `CLAUDE_MODELS` const in `src/catalog.rs` behind
  `resolve_claude_alias` (superseded ids: `SUPERSEDED_CLAUDE_IDS`, same file,
  applied by `src/provider/anthropic.rs` only to requests that float — usage
  accounting resolves aliases only);
  adding a curated row carries its aliases
  automatically. llmux still does not otherwise *shape* claude requests — the
  one exception is Claude Code's bare `quota` probe, which gets the Claude Code
  system block ([provider contract](provider-compatibility.md#haiku-55-catalog-update)) —
  and the `efforts` menu on claude rows is the Claude Code `/effort` level list,
  per the user contract — minus the levels a model lacks: `claude-opus-4-6[1m]`
  lists low, medium, high, max (no `xhigh`; corrected 2026-11-18).
- **openrouter `or-` aliases** — the openrouter rows advertise the id a client
  SENDS (`or-nemotron-3-ultra`); the OpenRouter slug it is rewritten to on the
  wire (`nvidia/nemotron-3-ultra-550b-a55b:free`) is a different string,
  because the advertised id has to carry the `or-` prefix that routes the
  request to the openrouter group in the first place. The curated mapping:

  | request slug                | model on the wire                        |
  | --------------------------- | ---------------------------------------- |
  | `or-free`                   | `openrouter/free`                        |
  | `or-nemotron-3-ultra`       | `nvidia/nemotron-3-ultra-550b-a55b:free` |
  | `or-nemotron-3.5-lightning` | `nvidia/nemotron-3.5-lightning:free`     |
  | `or-dots-3-note`            | `dots-studio/dots-3-note-preview:free`   |
  | `or-north-mini-code`        | `cohere/north-mini-code:free`            |
  | `or-gemma-4-31b`            | `google/gemma-4-31b-it:free`             |

  `or-ox-alpha`, `or-glm-5.2`, `or-laguna-s-2.1` and `or-gpt-oss-20b` were
  retired on 2026-11-18 ([#243](https://github.com/2lab-ai/llmux/issues/243)):
  OpenRouter no longer serves their slugs. Sent today, they are uncurated: the
  `or-` selector is dropped and the rest (`ox-alpha`, …) is forwarded, so
  OpenRouter's own error reaches the client (see the last rule below).

  Three rules sit around that table, and the table is a convenience layer, not
  a gate:

  - **bare `or`** — like bare `grok`, it resolves to the live pin
    (`config.openrouter.default_model`, default
    `nvidia/nemotron-3-ultra-550b-a55b:free` since 2026-11-18, was
    `stealth/ox-alpha`), and so does a request that names no model at all.
  - **`or-<vendor>/<slug>` escape hatch** — anything containing a `/` is used
    VERBATIM minus the `or-` selector, so the ~400 OpenRouter models outside
    the curated set are reachable: `or-google/gemma-4-26b-a4b-it:free` →
    `google/gemma-4-26b-a4b-it:free`. A bare `openrouter/…` slug also routes here and
    rides through unchanged.
  - **no silent substitution** — an uncurated bare name passes through as it
    was typed, so OpenRouter's own 404 reaches the user instead of llmux
    answering from a model nobody asked for.

  Matching is trimmed and case-insensitive, and one trailing `[1m]` is stripped
  first, exactly as on the claude and codex paths.
- **gemini aliases** — static, not a pin (the `GEMINI_MODELS` const in
  `src/catalog.rs`): `gemini` and `gemini-3.8-flash` resolve to
  `gemini-3.8-flash-high[1m]`; the Gemini 4 element aliases `argon` →
  `gemini-4-argon[1m]`, `carbon` → `gemini-4-carbon[1m]`, `hydrogen` →
  `gemini-4-hydrogen[1m]` and `neon` → `gemini-4-neon` (the 512K base row, the
  window Antigravity publishes for Neon; `neon`, `carbon` and `hydrogen` since
  2026-11-21). Unlike `grok` and `or`, no config key moves them:
  `gemini.default_model` only names the model used when a non-gemini name (or a
  request that names no model) lands on the gemini group. One trailing `[1m]` is
  stripped (case-insensitively) before the request leaves llmux, so `-m gemini`
  and `-m gemini-3.8-flash-high[1m]` both reach the backend as
  `gemini-3.8-flash-high`. Claude Code is the exception: its request is refused
  locally (see [Gemini](#gemini)). The thinking level is part of the upstream id.
  A 3.8 Flash row names it (`-high` / `-medium` / `-low`) and ignores the
  client's effort. A bare family id — `gemini-3.8-flash`, or a Gemini 4 id
  `gemini-4-neon` / `-carbon` / `-hydrogen` / `-argon`, or an alias of one, with or
  without `[1m]` — takes its level from `output_config.effort` (Codex:
  `reasoning.effort`) and reaches the backend as `<family>-<level>`:
  `none` / `minimal` / `low` → `-low`, `medium` → `-medium`, no effort or
  `high` → `-high`, and `xhigh` / `max` / `ultra` → `-xhigh` on Neon and
  Hydrogen, `-high` on Carbon, Argon and Flash. So `-m argon` reaches the backend
  as `gemini-4-argon-high` (it went out as the bare `gemini-4-argon`, which
  Antigravity does not serve, until 2026-11-21;
  [#244](https://github.com/2lab-ai/llmux/issues/244)). An id that already names
  its level (`gemini-4-neon-xhigh`) and any other `gemini-*` id pass through
  verbatim. Source: [`gemini/spec.md`](gemini/spec.md) §R3.
- **alias stability** — aliases float to the current generation, ids do not.
  `opus` tracks the newest curated Opus and moved from `claude-opus-4-8[1m]` to
  `claude-opus-5[1m]` on 2026-07-27 (4.8 stays in the catalog; it just no longer
  owns an alias), then from `claude-opus-5[1m]` to `claude-opus-5-5[1m]` on
  2026-09-23. `sonnet` moved from `claude-sonnet-5[1m]` to
  `claude-sonnet-5-5[1m]` on 2026-10-06 (Sonnet 5.5 released 2026-09-28), and
  on the same day the codex `sol` alias moved from `gpt-5.6-sol` to
  `gpt-6.1-sol` and `luna` from `gpt-5.6-luna` to `gpt-6-luna`. On
  2026-11-10 `fable` moved from `claude-fable-5-1[1m]` to
  `claude-fable-5-2[1m]` (Fable 5.2 released 2026-11-09) and `terra` from
  `gpt-5.6-terra` to `gpt-6.1-terra`. On 2026-11-18 `sol` moved from
  `gpt-6.1-sol` to `gpt-6.2-sol` and `astra` from `gpt-6-astra[1m]` to
  `gpt-6.2-astra[1m]` (both released 2026-11-17). On 2026-11-21 the
  new `neon`, `carbon` and `hydrogen` aliases joined `argon` (no alias moved).
  The version
  aliases `opus-5` / `sonnet-5` / `fable-5-1` and the bare generation ids
  `gpt-5.6` / `gpt-6` / `gpt-6.1` did NOT move — they stay on their generation, because floating a version-pinned
  alias onto a new model would be silent substitution. Anyone who needs one specific model must send
  its full catalog id — that is the stable handle. On 2026-10-08, `haiku`
  moved from `claude-haiku-4-5` to `claude-haiku-5-5[1m]`; the explicit 4.5 id
  remained available then. On 2026-10-23 (user decision) that changed for
  Haiku 4.5 only, and conditionally. The explicit ids `claude-haiku-4-5` and
  the dated `claude-haiku-4-5-YYYYMMDD` resolve to the current `haiku` owner on
  Claude Code / Agent SDK–shaped Messages requests — the first system block is
  a billing header (`x-anthropic-billing-header: …`) or exactly one of the
  Claude Code / Agent SDK identity sentences — that also use default sampling
  (no `top_p`, no `top_k`, `temperature` absent or 1), and on Claude Code's
  bare `quota` probe. Claude Code and the Claude Agent SDK send those explicit ids
  for their built-in Haiku tier (session titles, `quota` probes,
  `model: haiku` agents outside `llmux run`), so the alias float alone never
  reached that traffic. A bare or custom-system request naming a 4.5 id keeps
  reaching Haiku 4.5, because Haiku 5.5 refuses such requests on the
  subscription endpoint, and so does one that sets `temperature` other than 1,
  `top_p` or `top_k`, which Haiku 5.5 rejects as deprecated
  ([probe table](provider-compatibility.md#haiku-55-catalog-update));
  the Codex-frontend (Responses) path floats aliases only. The
  `claude-haiku-4-5` row stays retired: the picker is Claude Code-facing, and
  Claude Code requests float. This reverses the 2026-10-08 sentence above for
  Haiku 4.5 only; every other full id and version alias
  (`claude-opus-4-8[1m]`, `opus-5`, `sonnet-5`, …) still stays put. Usage and
  pricing are booked against the resolved id, not the alias, so alias traffic
  lands on the same row as id traffic. Haiku 4.5 ids are the exception on the
  accounting side: records from before the change keep their Haiku 4.5
  identity and Haiku 4.5 pricing (Haiku 4.5 served them); from the change
  onward a floated request is recorded as `claude-haiku-5-5`, the model that
  serves it, and a request that still reaches Haiku 4.5 keeps its 4.5 id.

### Out-of-catalog grok pin

The curated grok set is `grok-4.7` (the default pin, plus its opt-in
`grok-4.7[1m]` twin), `grok-4.6` and `grok-4.5`.
`config.grok.default_model` may pin ANY
`grok-*` slug — including ids not in the curated table below (e.g.
`grok-4.3`, `grok-code-fast-1`). Because the provider forwards such ids
verbatim, the pin is real and routable, so the `"grok"` family alias must have
an owner. When the pin matches no curated id, the catalog appends a
**synthesized** grok row: `id` = the pin, `name` = the pin verbatim, `aliases`
= `["grok"]`, `efforts` from the thinking-level lookup (empty unless the id is a
known reasoner — e.g. pinning `grok-4.3` yields `none, low, medium, high`), and
`max_context` = `null`. The null metadata reflects that llmux has no published
context/name for an id it does not curate.

### Out-of-catalog openrouter pin

`config.openrouter.default_model` may pin ANY OpenRouter slug, including one
outside the curated table below — the provider forwards it verbatim, so the pin
is real and routable and the bare `or` alias must have an owner. When the pin
matches no curated row's wire slug, the catalog appends a **synthesized**
openrouter row: `id` = `or-<pin>` (the string a client can actually type),
`name` = the pin verbatim, `aliases` = `["or"]`, `efforts` = empty, and
`max_context` = `null` — llmux has no published context or effort menu for a
model it does not curate.

## Current catalog

| id                  | aliases      | name                | efforts                              | max_context | group  |
| ------------------- | ------------ | ------------------- | ------------------------------------ | ----------- | ------ |
| claude-fable-5-2[1m] | fable, fable-5-2 | Claude Fable 5.2 | low, medium, high, xhigh, max        | 1000000     | claude |
| claude-fable-5-1[1m] | fable-5-1   | Claude Fable 5.1    | low, medium, high, xhigh, max        | 1000000     | claude |
| claude-fable-5[1m]  | —            | Claude Fable 5      | low, medium, high, xhigh, max        | 1000000     | claude |
| claude-opus-5-5[1m] | opus, opus-5-5 | Claude Opus 5.5 [1M] | low, medium, high, xhigh, max      | 1000000     | claude |
| claude-opus-5-5     | —            | Claude Opus 5.5     | low, medium, high, xhigh, max        | 200000      | claude |
| claude-opus-5[1m]   | opus-5       | Claude Opus 5 [1M]  | low, medium, high, xhigh, max        | 1000000     | claude |
| claude-opus-5       | —            | Claude Opus 5       | low, medium, high, xhigh, max        | 200000      | claude |
| claude-opus-4-8[1m] | —            | Claude Opus 4.8     | low, medium, high, xhigh, max        | 1000000     | claude |
| claude-opus-4-6[1m] | —            | Claude Opus 4.6     | low, medium, high, max               | 1000000     | claude |
| claude-sonnet-5-5[1m] | sonnet, sonnet-5-5 | Claude Sonnet 5.5 [1M] | low, medium, high, xhigh, max | 1000000     | claude |
| claude-sonnet-5-5   | —            | Claude Sonnet 5.5   | low, medium, high, xhigh, max        | 200000      | claude |
| claude-sonnet-5[1m] | sonnet-5     | Claude Sonnet 5 [1M]| low, medium, high, xhigh, max        | 1000000     | claude |
| claude-sonnet-5     | —            | Claude Sonnet 5     | low, medium, high, xhigh, max        | 200000      | claude |
| claude-haiku-5-5[1m] | haiku, haiku-5-5 | Claude Haiku 5.5 [1M] | low, medium, high, xhigh, max | 1000000 | claude |
| claude-haiku-5-5 | — | Claude Haiku 5.5 | low, medium, high, xhigh, max | 1000000 | claude |
| gpt-6.2-sol[1m]     | —            | GPT-6.2-Sol [1M]    | low, medium, high, xhigh, max, ultra | 1000000     | codex  |
| gpt-6.2-sol         | sol, gpt-6.2 | GPT-6.2-Sol         | low, medium, high, xhigh, max, ultra | 500000      | codex  |
| gpt-6.2-astra[1m]   | astra        | GPT-6.2-Astra [1M]  | low, medium, high, xhigh, max, ultra | 1000000     | codex  |
| gpt-6.2-astra       | —            | GPT-6.2-Astra       | low, medium, high, xhigh, max, ultra | 272000      | codex  |
| gpt-6.1-sol[1m]     | —            | GPT-6.1-Sol [1M]    | low, medium, high, xhigh, max, ultra | 1000000     | codex  |
| gpt-6.1-sol         | gpt-6.1      | GPT-6.1-Sol         | low, medium, high, xhigh, max, ultra | 500000      | codex  |
| gpt-6.1-terra[1m]   | —            | GPT-6.1-Terra [1M]  | low, medium, high, xhigh, max, ultra | 1000000     | codex  |
| gpt-6.1-terra       | terra        | GPT-6.1-Terra       | low, medium, high, xhigh, max, ultra | 500000      | codex  |
| gpt-6-astra[1m]     | gpt-6        | GPT-6-Astra [1M]    | low, medium, high, xhigh, max, ultra | 1000000     | codex  |
| gpt-6-astra         | —            | GPT-6-Astra         | low, medium, high, xhigh, max, ultra | 272000      | codex  |
| gpt-6-sol[1m]       | —            | GPT-6-Sol [1M]      | low, medium, high, xhigh, max, ultra | 1000000     | codex  |
| gpt-6-sol           | —            | GPT-6-Sol           | low, medium, high, xhigh, max, ultra | 272000      | codex  |
| gpt-6-luna[1m]      | —            | GPT-6-Luna [1M]     | low, medium, high, xhigh, max        | 1000000     | codex  |
| gpt-6-luna          | luna         | GPT-6-Luna          | low, medium, high, xhigh, max        | 272000      | codex  |
| gpt-5.6-sol[1m]     | —            | GPT-5.6-Sol [1M]    | low, medium, high, xhigh, max, ultra | 1000000     | codex  |
| gpt-5.6-sol         | gpt-5.6      | GPT-5.6-Sol         | low, medium, high, xhigh, max, ultra | 272000      | codex  |
| gpt-5.6-terra[1m]   | —            | GPT-5.6-Terra [1M]  | low, medium, high, xhigh, max, ultra | 1000000     | codex  |
| gpt-5.6-terra       | —            | GPT-5.6-Terra       | low, medium, high, xhigh, max, ultra | 272000      | codex  |
| gpt-5.6-luna        | —            | GPT-5.6-Luna        | low, medium, high, xhigh, max        | 272000      | codex  |
| grok-4.7            | grok (pinned)| Grok 4.7            | low, medium, high, xhigh             | 500000      | grok   |
| grok-4.7[1m]        | —            | Grok 4.7 [1M] (500k upstream) | low, medium, high, xhigh   | 500000      | grok   |
| grok-4.6            | —            | Grok 4.6            | low, medium, high, xhigh             | 500000      | grok   |
| grok-4.5            | —            | Grok 4.5            | low, medium, high                    | 500000      | grok   |
| or-free             | —            | OpenRouter Free Models Router | —                          | 200000      | openrouter |
| or-nemotron-3-ultra | or (pinned)  | NVIDIA Nemotron 3 Ultra (free) | medium, high              | 1000000     | openrouter |
| or-nemotron-3.5-lightning | —      | NVIDIA Nemotron 3.5 Lightning (free) | —                   | 1000000     | openrouter |
| or-dots-3-note      | —            | Dots3-Note Preview (free) | —                              | 512000      | openrouter |
| or-north-mini-code  | —            | Cohere North Mini Code (free) | —                          | 256000      | openrouter |
| or-gemma-4-31b      | —            | Google Gemma 4 31B (free) | —                              | 262144      | openrouter |
| gemini-3.8-flash-high[1m] | gemini, gemini-3.8-flash | Gemini 3.8 Flash (High) [1M] | — | 1048576  | gemini |
| gemini-3.8-flash-high | —          | Gemini 3.8 Flash (High) | —                                | 1048576     | gemini |
| gemini-3.8-flash-medium[1m] | —    | Gemini 3.8 Flash (Medium) [1M] | —                         | 1048576     | gemini |
| gemini-3.8-flash-medium | —        | Gemini 3.8 Flash (Medium) | —                              | 1048576     | gemini |
| gemini-3.8-flash-low[1m] | —       | Gemini 3.8 Flash (Low) [1M] | —                            | 1048576     | gemini |
| gemini-3.8-flash-low | —           | Gemini 3.8 Flash (Low) | —                                 | 1048576     | gemini |
| gemini-4-neon[1m]   | —            | Gemini 4 Neon [1M]  | low, medium, high, xhigh             | 1048576     | gemini |
| gemini-4-neon       | neon         | Gemini 4 Neon       | low, medium, high, xhigh             | 524288      | gemini |
| gemini-4-carbon[1m] | carbon       | Gemini 4 Carbon [1M] | low, medium, high                   | 1048576     | gemini |
| gemini-4-carbon     | —            | Gemini 4 Carbon     | low, medium, high                    | 1048576     | gemini |
| gemini-4-hydrogen[1m] | hydrogen   | Gemini 4 Hydrogen [1M] | low, medium, high, xhigh          | 1048576     | gemini |
| gemini-4-hydrogen   | —            | Gemini 4 Hydrogen   | low, medium, high, xhigh             | 1048576     | gemini |
| gemini-4-argon[1m]  | argon        | Gemini 4 Argon [1M] | low, medium, high                    | 1048576     | gemini |
| gemini-4-argon      | —            | Gemini 4 Argon      | low, medium, high                    | 1048576     | gemini |

"grok (pinned)" means the `grok` alias appears on that row only while
`grok-4.7` is the live grok pin; any other pinned `grok-*` id takes the alias
to its own curated row, or to a synthesized row when it is out of catalog
(see [Out-of-catalog grok pin](#out-of-catalog-grok-pin)). A base-slug pin
never hands the alias to the `[1m]` twin; pinning the suffixed id does
(see [The grok `[1m]` twin](#the-grok-1m-twin)). "or (pinned)" reads
the same way for the openrouter pin (see
[Out-of-catalog openrouter pin](#out-of-catalog-openrouter-pin)); with the
default pin it sits on `or-nemotron-3-ultra` (on `or-ox-alpha` until
2026-11-18). The openrouter ids are what a client
sends — the slug that reaches OpenRouter is the one in the
[alias table](#alias-semantics), and every curated openrouter row is a free
model (priced `$0` in and out; an UNCURATED openrouter model has no known
rate and is reported unpriced, never as a free `$0`). The gemini aliases carry
no "(pinned)": they are fixed on their `[1m]` rows, except `neon`, which is
fixed on the 512K base row (see [Gemini](#gemini)).

### The grok `[1m]` twin

`grok-4.7[1m]` and `grok-4.7` are the SAME upstream model — the provider strips
one trailing `[1m]` before the request leaves llmux — which is why both rows
advertise **500000**, the upper entry of xAI's window list from the live
`/v1/models` probe. As of 2026-10-12 (first seen that day; the 2026-09-23 read
still reported 500000) that endpoint publishes `context_window: 256000` next to
`context_windows: [256000, 500000]`; llmux sends no window selector, and a
306,103-input-token `grok-4.7` request through the daemon was still accepted
that day, so the rows keep the 500000 upper entry — the ceiling itself was not
re-probed (see [Model sweep: 2026-10-12](#model-sweep-2026-10-12)). The
twin exists only because of how Claude Code sized its own readout: for an id it
did not know it assumed 200k, unless the id ended in `[1m]`, which it read as a
1M window (800k usable). Measured 2026-09-28 with Claude Code 2.1.283:
`--model grok[1m]` → `/context` 143.1k/800k, `--model grok` → 200k. **Since
Claude Code 2.1.323 an unknown id gets 1M too** (its changelog: "Changed the
context window assumed for a model ID that Claude Code doesn't recognize from
200K to 1M"; `CLAUDE_CODE_DISABLE_1M_CONTEXT=1` keeps 200K). Measured 2026-11-21
with Claude Code 2.1.330 through the daemon: `grok` and `grok[1m]` both report
`modelUsage.contextWindow` 1000000. On a current client the bare `grok` therefore
carries the same 500k caveat as the twin; llmux has no guard for it yet.

That 800k readout is **wrong about the backend**: xAI still cuts the request off
at 500,000, so a session driven past 500k is rejected upstream while the client
still shows headroom. The twin is therefore an **explicit opt-in**, not the
face llmux puts on the family:

- with the default pin `grok-4.7` the `grok` alias stays on the BASE row, so
  `/llmux/models` and the picker present the conservative id as the family
  default (the client window is unaffected either way, because the client
  sizes off the id you submit: a bare `grok` was a 200k session before Claude
  Code 2.1.323 and is a 1M one since);
- the twin is listed after it, named `Grok 4.7 [1M] (500k upstream)` so the
  picker itself discloses the gap;
- to use the bigger denominator, pick that row in `/model` or type
  `/model grok-4.7[1m]` (or `grok[1m]`, which resolves to the pin) — and stay
  under 500k;
- alias ownership follows the pin by exact id, so an operator who wants the
  1M-denominated row to BE the advertised family default can pin it:
  `config.grok.default_model = "grok-4.7[1m]"` moves the `grok` alias onto the
  twin. llmux simply never chooses that for you. That pin changes only what
  the catalog advertises — the provider normalises a pinned `[1m]` exactly as
  it normalises a requested one, so the request still reaches xAI as
  `grok-4.7` (and still finds its effort menu).

This is deliberately NOT the astra arrangement: there the alias sits on the
`[1m]` row because the 1M figure matches the backend, here it would not.

### The codex `[1m]` rows

`gpt-6-astra[1m]` / `gpt-5.6-sol[1m]` / `gpt-5.6-terra[1m]` are the codex side of the same `[1m]`
convention the claude rows use: the suffix is a **client-side context-denominator
opt-in**, not a different upstream model. Claude Code parses it out of the
configured model string to size its context readout; llmux strips one trailing
`[1m]` before resolving the model, so upstream never sees it and
`gpt-5.6-sol[1m]` reaches the backend as `gpt-5.6-sol`. The strip happens ahead
of every resolution rule, so a suffixed alias works too (`sol[1m]` →
`gpt-6.2-sol`), and it applies to routing as well (`sol[1m]` classifies to the
codex group exactly like `sol`). A client that sends the id verbatim — curl, an
SDK — gets the model it asked for instead of falling back to the configured pin.

The advertised 1000000 is the opt-in denominator; the measured upstream input
ceiling is close to it. Probes 2026-08-21 against the ChatGPT-account codex
backend accepted 555,029 / ~801k / ~869k / 910,229 input tokens on
`gpt-5.6-sol` and were rejected at ~936k with `Your input exceeds the context
window of this model` (`gpt-5.6-terra` accepted 555,029). OpenAI publishes
1,050,000 total for the gpt-5.6 family. The base rows keep the openai/codex
catalog's current 272000 (re-read 2026-10-08), client metadata rather than a
newly measured upstream limit. The `[1m]` twins keep their published 1M metadata. There is deliberately
no `gpt-5.6-luna[1m]` (luna still returns "Model not found" upstream).

`gpt-6-astra[1m]` advertises the same 1000000 on the strength of OpenAI's
published 1,050,000-token window for Astra — it has **not** been probed through
the daemon, unlike the 5.6 rows above (the only astra probe so far is the
2026-09-07 acceptance check, which confirms the backend takes the slug, not its
ceiling). On astra the bare aliases sit on the `[1m]` row — `gpt-6` on
`gpt-6-astra[1m]`, and `astra` on `gpt-6.2-astra[1m]` since 2026-11-18 —
opposite the 5.6 convention, so the ergonomic name resolves to the 1M row's
slug and advertised window; the explicit base ids `gpt-6-astra` /
`gpt-6.2-astra` keep the openai/codex catalog's 272000 and advertise no
aliases. Note this is the
CATALOG window, not Claude Code's: the client sizes off the submitted id, so a
bare `astra` was a 200k session before Claude Code 2.1.323 (type `astra[1m]` or
pick the `[1M]` picker row for the 1M denominator) and is a 1M one since
(measured 2026-11-21 with 2.1.330: `modelUsage.contextWindow` 1000000). The catalog
also lists an 872,000 `max_context_window` for astra, which llmux does not
advertise.

`gpt-6.1-sol[1m]`, `gpt-6-sol[1m]` and `gpt-6-luna[1m]` (added 2026-10-06)
follow the same pattern as astra's twin — 1000000 advertised on the strength of
OpenAI's published 1,050,000 window for those models, unprobed through the
daemon — but keep the 5.6 alias convention: the bare aliases sit on the BASE
rows (`gpt-6.1` on `gpt-6.1-sol`, `luna` on `gpt-6-luna`), and the `[1m]` twin
is an explicit opt-in. models.json lists 272,000 / 872,000 for `gpt-6-sol` and
`gpt-6-luna`; for `gpt-6.1-sol` it listed the same until commit `6c0cee5`
(2026-11-17) raised the context_window to 500,000, which the base row
advertises since 2026-11-18 (the Codex models page says the same: "GPT-6.1 Sol
uses a default context window of 500,000 tokens in Codex"). llmux does not
advertise the 872,000 `max_context_window`.

`gpt-6.1-terra[1m]` (added 2026-11-10) follows the same pattern: 1000000
advertised on the strength of OpenAI's published 1,050,000 window (128K
output), with `terra` on the base row. Its base row is the one exception to
272000: the models.json entry for `gpt-6.1-terra` lists context_window
**500000** (max_context_window 872000), so the base row advertises 500000.
Neither row has been probed through the daemon yet; a live probe follows the
preview deploy.

`gpt-6.2-sol[1m]` and `gpt-6.2-astra[1m]` (added 2026-11-18) advertise 1000000
on the same basis (OpenAI's published 1,050,000 window, 128K output; the Codex
models page says both reach 1M through `model_context_window`). Their base rows
carry the models.json context_window: **500000** for `gpt-6.2-sol`, 272000 for
`gpt-6.2-astra` (max_context_window 872000 on both). `sol` and the bare
`gpt-6.2` sit on the `gpt-6.2-sol` base row; `astra` sits on the
`gpt-6.2-astra[1m]` twin, the astra arrangement. Neither slug has been probed as
a passthrough slug through the daemon yet; before they were passthrough slugs, a
2026-11-18 probe asked for `gpt-6.2-sol` and was served by the codex pin
(`gpt-6-astra`).

### Gemini

Added 2026-10-15; the Gemini 4 rows grew from Argon to four models on
2026-11-21. The fourteen gemini rows are seven row pairs, each a base row and a
`[1m]` twin:

- **Gemini 3.8 Flash**, one pair per thinking level. The level is part of the
  id (`gemini-3.8-flash-high` / `-medium` / `-low`), so these rows have an empty
  effort menu.
- **Gemini 4 Neon, Carbon, Hydrogen and Argon**, one pair per model. The level
  is the row's effort menu: low, medium, high and xhigh on Neon and Hydrogen;
  low, medium and high on Carbon and Argon. Antigravity serves these models
  only under thinking-level ids (`gemini-4-neon-xhigh`, `gemini-4-argon-high`),
  so llmux sends `<id>-<level>` with the level taken from the client's effort,
  `-high` when none is sent (see [the rule](#alias-semantics) under gemini
  aliases). Google publishes the `gemini-4-neon-xhigh` form; the other suffixed
  ids come from third parties and are not yet verified by llmux
  ([#244](https://github.com/2lab-ai/llmux/issues/244),
  [#310](https://github.com/2lab-ai/llmux/issues/310)).

As on astra, and unlike [the grok twin](#the-grok-1m-twin), the family aliases
sit on the `[1m]` rows (`gemini`, `gemini-3.8-flash`, `argon`, `carbon`,
`hydrogen`): the backend's window is 1,048,576 tokens, so the 1M the suffix
advertises is honest there. **Neon is the exception.** The API gives it
1,048,576, but the Antigravity models page (read 2026-11-21) says "Up to 512K
tokens", so the bare `gemini-4-neon` row advertises 524,288 and owns `neon`;
`gemini-4-neon[1m]` keeps 1,048,576 as an explicit opt-in. The suffix is display
metadata only; a twin and its base row reach the backend as the same id.

These ids are served by the **Antigravity subscription backend** (Google Cloud
Code `v1internal`, reached with an account added by `llmux login --gemini`),
not by the public Gemini API. Google decides per account which ids it may use:

- **Every Gemini 4 model needs a paid plan** ([Antigravity models](https://antigravity.google/docs/models),
  read 2026-11-21):

  | Model | Free | Plus | Pro | Ultra | Enterprise |
  | --- | --- | --- | --- | --- | --- |
  | Gemini 3.8 Flash | ✓ | ✓ | ✓ | ✓ | ✓ |
  | Gemini 4 Hydrogen | ✗ | ✗ | ✓ (non-trial Pro only) | ✓ | ✓ |
  | Gemini 4 Neon | ✗ | ✗ | ✓ (paid Pro, or a trial on eligible premium devices) | ✓ | ✓ |
  | Gemini 4 Argon | ✗ | ✗ | ✓ (paid Pro, or a trial on eligible premium devices) | ✓ | ✓ |
  | Gemini 4 Carbon | ✗ | ✗ | ✗ | ✓ | ✓ |

  On an account without the model Cloud Code answers 404 NOT_FOUND (measured
  for Argon on Google AI Plus, 2026-10-15 and again 2026-11-21), and llmux
  relays that to the client as an Anthropic error with status 404. It does not
  substitute Flash. The rows exist so that an account on the right plan can
  serve them. Third parties also report that Cloud Code hides the Gemini 4 ids
  from clients older than hub 2.29, so the default `gemini.user_agent` moved
  to 2.34.0 on 2026-11-21 (see [configuration](configuration.md#gemini-backend)).
- **Flash** — the live `fetchAvailableModels` read on 2026-10-15 listed
  `gemini-3.8-flash-high`, `-medium` and `-low` with a 1,048,576-token context
  and 65,536 output tokens.

**Claude Code cannot select gemini models (2026-10-15).** The Antigravity
backend refused Claude Code requests (429 `RESOURCE_EXHAUSTED`) while the same
account served Codex with 200 in the same minute, so llmux does not forward
Claude Code traffic to the gemini group. `llmux run` leaves gemini rows out of
the [Claude Code `/model` picker](#claude-code-model-picker), and a Claude Code
request that routes to the gemini group (typed `/model`, `--model` or an
agent's `model:` line) gets a local `400 invalid_request_error`. Its message is
the requested model, `: `, and then:

> `gemini models are not available from Claude Code: Google's Antigravity backend refuses Claude Code requests, so llmux does not forward them. Use gemini from Codex: llmux run --codex -- -m gemini`

No request reaches Google. Use gemini from Codex
(`llmux run --codex -- -m gemini`, `-m argon`, or any gemini id); gemini usage
stays visible in the TUI and `llmux status`. Other Anthropic-Messages clients
are not affected by this gate. Details:
[`gemini/spec.md`](gemini/spec.md#claude-code-2026-10-15).

Costs shown for gemini rows use the public API list prices (see
[Sources](#sources)) for display only; subscription traffic is not billed per
token.

Gemini 4 Carbon (GA 2026-11-10), Hydrogen (GA 2026-11-12) and Neon (GA
2026-11-18) got rows on 2026-11-21. None of the Gemini 4 rows has been served
through the daemon yet: its Google AI Free/Plus account can use none of them
([#244](https://github.com/2lab-ai/llmux/issues/244),
[#310](https://github.com/2lab-ai/llmux/issues/310) stay open until a Pro or
Ultra probe). Google names no Helium, Krypton or Oxygen model id, so there are
no such aliases.

## Selecting a model

There are three ways to select a catalog model:

- **Claude Code**: `/model <id>` (the [picker below](#claude-code-model-picker)
  lists this catalog except the gemini rows), or `--model <id>` at launch.
- **Per agent**: `model: <id>` in `.claude/agents/<name>.md`. The agent's
  requests carry that id and llmux routes them by name, so one Claude Code
  session can run Claude, GPT and Grok workers natively (measured 2026-10-08;
  see [multi-model agents](multi-model-agents.md)).
- **Codex**: `-m <id>`, or the injected [catalog picker](#codex-model-picker).

The `/model` picker is a convenience for typing; agent delegation does not
depend on it. Prefer explicit ids such as `gpt-6-astra[1m]` or `grok-4.7` in
agent files so routing is readable; aliases (`astra`, `grok`) also work. Claude
Code logs an `unrecognized_model` warning for non-Claude ids and still sends the
request.

Gemini is the exception: it is selectable from Codex only. In Codex,
`-m gemini` selects `gemini-3.8-flash-high[1m]`, `-m argon` / `-m carbon` /
`-m hydrogen` select their model's `[1m]` row and `-m neon` selects
`gemini-4-neon` (`llmux run --codex -- exec -m neon '…'`); Codex's reasoning
effort picks the Gemini 4 thinking level. Claude Code, including a per-agent
`model:` line, gets a local 400 for a gemini model (see [Gemini](#gemini)).

Channel note: `haiku` → `claude-haiku-5-5[1m]` is on stable since v0.2.25
(2026-10-08; [release availability](operational-reference.md#release-availability)).

## Codex model picker

`llmux run --codex` fetches `/llmux/models` and supplies a temporary Codex catalog
with backend groups, effort menus and context windows (the 3.8 Flash rows have an
empty effort menu: pick the `-high`, `-medium` or `-low` id instead; a Gemini 4
row offers its thinking levels, and like every row whose menu has it, starts at
`medium` — `src/cli/run.rs` `codex_catalog`). Native coding instructions
come from the local Codex model cache when available; the fallback is an embedded
coding prompt. Claude and other translated models use direct client tools.
`--no-model-picker`, or an explicit `-c model_catalog_json=...`, preserves your
client catalog; a fetch failure warns and does not block launch. User `-m` and
`-c` arguments override injected launch defaults. No persistent Codex settings
file is edited. See [Codex frontend](codex-frontend/README.md) for supported controls.

## Antigravity (agy) model picker

`llmux run --agy` launches agy in gateway mode; agy reads its picker from the daemon's
`GET /v1beta/models` (the llmux catalog as the Gemini model list — no client-side
catalog file). One entry per model under its base id (`[1m]` twins merged; a
`[1m]`-only row such as `claude-opus-4-8` is served as its `[1m]` row), at most 32
entries (agy's limit for ids it does not know), the start model first.
`outputTokenLimit` is a per-group default agy sends back as `maxOutputTokens`: claude
64,000, codex 128,000, gemini 65,536, grok 32,768, openrouter 16,384. `thinking` is true
for rows with an effort menu (agy refuses `--effort` otherwise; every Gemini 4 family row
has one) and for level-named gemini ids, whose level agy moves into `thinkingLevel`; on
the gemini group that level picks the family's level id (`gemini-4-neon` + `HIGH` →
`gemini-4-neon-high`). agy 1.5.13 offers only `low` / `medium` / `high` there. The start
model is `agy.default_model`, else `codex.default_model`; the 2026-11-21 catalog has 36
candidates, of which `gpt-5.6-luna`, `or-dots-3-note`, `or-north-mini-code` and
`or-gemma-4-31b` fall outside the 32. See
[agy frontend](agy-frontend/README.md#discovery-and-limits).

The following Claude Code sections describe that client’s picker and context
display workarounds; Codex gets context metadata from its injected catalog.

## Claude Code `/model` picker

`llmux run` puts this catalog into Claude Code's `/model` picker. Before
spawning `claude` it fetches `GET {base_url}/llmux/models` from the very proxy
it is about to point the client at (local or `--remote`, same api-key gate, 3s
cap) and passes the lineup as `claude --settings '<json>'` — nothing is written
to any settings file:

```json
{ "modelPicker": { "options": [
  { "model": "grok-4.7", "label": "Grok 4.7", "description": "grok · efforts low…xhigh · ctx 500000" }
] } }
```

One row per catalog entry, in catalog order, except the gemini rows: llmux
does not forward Claude Code traffic to the gemini group (see [Gemini](#gemini)),
so the picker does not offer them. The [Codex picker](#codex-model-picker) keeps
them.

- `model` — the entry `id` **verbatim**, including a `[1m]` suffix (Claude Code
  takes the string as-is, and the providers strip the suffix upstream). It is
  the string llmux routes on, so selecting a row equals typing `/model <id>`.
- `label` — the entry `name`.
- `description` — `<group> · efforts <first…last> · ctx <max_context>`, dropping
  the efforts part for an empty menu and the ctx part for an unpublished window
  (`or-free`, with no effort menu, therefore reads `openrouter · ctx 200000`).
  The effort part is a RANGE (the menu's ends), not the whole menu.

`replaceBuiltInOptions` is deliberately left unset: the built-in Anthropic rows
stay, and Claude Code skips a listed model the built-in lineup already covers.
Requires Claude Code **v2.1.242 or later** (where `modelPicker` was added); an
older client ignores the key and shows only its built-in lineup.

Two opt-outs, plus one failure mode:

- `llmux run --no-model-picker` — spawn `claude` with no `--settings` at all.
- Your own `--settings` in the pass-through args (`llmux run -- --settings …`,
  either spelling) — **your document wins**: llmux never merges into it and
  prints `warning: --settings given, llmux model picker lineup not injected`.
  Claude Code reads one `--settings` document, so precedence is
  all-or-nothing.
- A `modelPicker` key in your own `~/.claude/settings.json` is shadowed by the
  injected document for that launch (Claude Code layers `--settings` over user
  settings key by key; llmux only inspects the pass-through argv when deciding
  to inject). Use `--no-model-picker` to keep your own lineup.
- Catalog fetch failure (daemon unreachable, non-200, unparseable body) —
  `warning: model picker not injected: <reason>` and `claude` starts unchanged.
  The picker is a convenience; it must never block a launch.

Gateway model discovery (`CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY`) is NOT
used: it keeps only ids containing `claude`/`anthropic` and needs a credential
header, both of which defeat the purpose here. The separate `/v1/models` route
now serves an OpenAI-shaped list; Claude Code picker injection uses `/llmux/models`.

### Alias exports

The picker lineup alone cannot make `opus` mean what this catalog says it
means. Measured 2026-09-28 with Claude Code 2.1.283: `sonnet`, `opus`, `haiku`,
`fable` (and `sonnet[1m]` / `opus[1m]` / `fable[1m]`, plus `best` and
`opusplan`) are NATIVE aliases the client resolves against its OWN model
records before a request is built — `/model opus` reaches llmux as
`claude-opus-5-5`, with the client's 200k compaction window, so the catalog's
promise that `opus` is `claude-opus-5-5[1m]` never applies.

So the same fetch that builds the lineup also exports, for the same launch:

| env var                          | value = the catalog id owning the alias |
| -------------------------------- | --------------------------------------- |
| `ANTHROPIC_DEFAULT_OPUS_MODEL`   | `opus` owner (`claude-opus-5-5[1m]`)    |
| `ANTHROPIC_DEFAULT_FABLE_MODEL`  | `fable` owner (`claude-fable-5-2[1m]`)  |
| `ANTHROPIC_DEFAULT_SONNET_MODEL` | `sonnet` owner (`claude-sonnet-5-5[1m]`)  |
| `ANTHROPIC_DEFAULT_HAIKU_MODEL`  | `haiku` owner (`claude-haiku-5-5[1m]`)      |

The values are DERIVED from the catalog alias owners at launch, never
hardcoded: re-curating an alias onto a new row moves the export with it, and an
alias no row owns is not exported at all (that family keeps Claude Code's own
default). Measured effect: `ANTHROPIC_DEFAULT_OPUS_MODEL=claude-opus-5-5[1m]`
→ status line "Claude Opus 5.5 [1M]" and `/context` 221.2k/800k, versus 200k
without it. Only the `_MODEL` variants are set — Claude Code's `_NAME`,
`_DESCRIPTION` and `_SUPPORTED_CAPABILITIES` variants are left alone.

Since 2026-11-10 the `fable` export is `claude-fable-5-2[1m]`, which matches
what Claude Code 2.1.321 itself resolves `fable` to (`claude-fable-5-2`).
Fable 5.2 needs Claude Code v2.1.321 or later. Fable runs at 1M natively, so
the client accepts the `[1m]` suffix on it but the suffix changes nothing —
the same as for `claude-fable-5-1[1m]`.

- A var you already export is left alone (your value wins for that family; the
  other three are still exported).
- `llmux run --no-model-picker` disables the exports too — it is the single
  "leave my Claude Code alone" switch. Your own `--settings`, by contrast,
  drops only the picker document: the exports are env vars, not a settings
  document, so they cannot collide with your lineup.
- A failed catalog fetch exports nothing, exactly as it injects no lineup.

**These four vars are the only mechanism that changes what a bare native alias
means, and they do not generalize.** An id Claude Code does not know (`astra`,
`gpt-6`, `grok`, `or-nemotron-3-ultra`, …) has no such var: it is submitted verbatim
and gets the client's assumption for an unknown id. That assumption was 200k
until Claude Code 2.1.323 and is 1M since (measured 2026-11-21 with Claude Code
2.1.330 through the daemon: `grok`, `astra` and `grok[1m]` all report
`modelUsage.contextWindow` 1000000; `CLAUDE_CODE_DISABLE_1M_CONTEXT=1` restores
200k). Catalog alias ownership decides the UPSTREAM slug, never the client's
window — bare `astra` resolves to `gpt-6.2-astra[1m]`'s slug upstream either
way. On an older client the SUBMITTED id has to end in `[1m]` to move the
client-side denominator: pick the `[1M]` row in the picker, or type
`/model astra[1m]` / `/model grok[1m]`. On a current one, mind the upstream
ceiling a 1M session can overrun: xAI cuts grok off at
[500k](#the-grok-1m-twin).

## Model sweep: 2026-11-21

Checked all four providers against their current primary sources for 2026-11-18 → 2026-11-21
(all fetched 2026-11-21). One new text model shipped, Gemini 4 Neon. The Gemini 4 rows grew from
Argon alone to Neon, Carbon, Hydrogen and Argon, one row pair per model, with the new aliases
`neon`, `carbon` and `hydrogen`; the thinking level is now the Gemini 4 rows' effort menu, and
llmux sends a bare Gemini 4 id or alias upstream as its thinking-level id. The default Gemini
User-Agent moved to hub 2.34.0, the Agent SDK bridge to 0.3.321, and `grok-4.7-build` /
`grok-4.7-build-fast` got price rows. Catalog size: 58 curated rows (6 gemini rows added).

| Provider | Result | Sources checked |
| --- | --- | --- |
| Claude | No new model id, and no price, context, max-output or effort change on any of the 15 rows. The newest model entry in the release notes is still 2026-11-09 (Fable 5.2, Mythos 5.2). Claude Code 2.1.328, 2.1.329 and 2.1.330 (npm `latest`, 2026-11-20) add no model, and their aliases are unchanged: `fable` → `claude-fable-5-2`, `opus` → `claude-opus-5-5`, `sonnet` → `claude-sonnet-5-5`, `haiku` → `claude-haiku-5-5`. The Agent SDK the Codex frontend's Claude route runs moved from 0.3.292 to **0.3.321** (Claude Code 2.1.321): the model-config page says Fable 5.2 "requires Claude Code v2.1.321 or later" and Haiku 5.5 "Use v2.1.293 or later", so the old pin was below both. 0.3.321's `SDKAssistantMessageError` union is the same as 0.3.292's, and the bridge's actual-SDK tests pass on it. Claude Code 2.1.323 (2026-11-12, before this window) changed the window it assumes for an id it does not recognize from 200K to 1M. Measured 2026-11-21 with Claude Code 2.1.330 through the daemon, `grok`, `astra` and `grok[1m]` all report `modelUsage.contextWindow` 1000000. This page's 200k statements are corrected (see [Alias exports](#alias-exports) and [the grok twin](#the-grok-1m-twin)). | [models overview](https://platform.claude.com/docs/en/models/overview), [pricing](https://platform.claude.com/docs/en/about-claude/pricing), [release notes](https://platform.claude.com/docs/en/release-notes/overview), [deprecations](https://platform.claude.com/docs/en/about-claude/model-deprecations), [news](https://www.anthropic.com/news), [Claude Code changelog](https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md), [Claude Code model config](https://code.claude.com/docs/en/model-config), npm `@anthropic-ai/claude-code-darwin-arm64@2.1.330`, npm `@anthropic-ai/claude-agent-sdk@0.3.321` (`claudeCodeVersion` 2.1.321) |
| Codex / OpenAI | No new picker model and no drift. `models.json` has had no commit since `6c0cee5` (2026-11-17), and every row's context, efforts and Standard / long-context / Fast price still matches. The only new id is `gpt-reserve` ("Luna Reserve"): a hidden fallback in the live server list that the Codex TUI switches to when ordinary usage is exhausted. It is not in `models.json`, has no published price and is not a row. The Codex docs moved: `developers.openai.com/codex/changelog` and `/codex/models` now redirect (308) to `learn.chatgpt.com/docs/changelog` and `/docs/models`. | [models.json @ 6c0cee5](https://github.com/openai/codex/blob/6c0cee502d215e5df2dd9a303cf2e252229c9e12/codex-rs/models-manager/models.json), [ChatGPT & Codex changelog](https://learn.chatgpt.com/docs/changelog), [Codex models](https://learn.chatgpt.com/docs/models), [pricing](https://developers.openai.com/api/docs/pricing), [API changelog](https://developers.openai.com/api/docs/changelog), model pages ([gpt-6.2-sol](https://developers.openai.com/api/docs/models/gpt-6.2-sol), [gpt-6.2-astra](https://developers.openai.com/api/docs/models/gpt-6.2-astra), [gpt-6.1-sol](https://developers.openai.com/api/docs/models/gpt-6.1-sol), [gpt-6.1-terra](https://developers.openai.com/api/docs/models/gpt-6.1-terra), [gpt-5.6-sol](https://developers.openai.com/api/docs/models/gpt-5.6-sol)) |
| Grok / xAI | No new text model id; `grok-4.7`, `grok-4.6` and `grok-4.5` keep the same prices, 500k context and efforts. `grok-build-0.1` reached end of life on 2026-11-19 (not a curated row). A `grok` request through the daemon on 2026-11-21 was served as `grok-4.7-build`, an id docs.x.ai does not document (it appears in fixtures of xai-org/grok-build, apparently Grok Build's name for standard Grok 4.7 traffic). llmux books grok usage under the requested id (`grok-4.7`), but a pin or client naming `grok-4.7-build` or `grok-4.7-build-fast` used to take the `grok-4.5` group fallback. Added price rows: `grok-4.7-build` at `grok-4.7`'s rates, and `grok-4.7-build-fast` ("Grok 4.7 Fast") at $4.00 in / $1.00 cached / $12.00 out, $6.00 / $1.50 / $18.00 at 200k prompt tokens or more (the fallback was about half that). | [release notes](https://docs.x.ai/developers/release-notes), [models](https://docs.x.ai/developers/models), [pricing](https://docs.x.ai/developers/pricing), [grok-4.7](https://docs.x.ai/developers/models/grok-4.7), [grok-4.6](https://docs.x.ai/developers/models/grok-4.6), [grok-4.5](https://docs.x.ai/developers/models/grok-4.5), [grok-build-0.1](https://docs.x.ai/developers/models/grok-build-0.1) |
| Gemini / Google (Antigravity) | New: **Gemini 4 Neon** (`gemini-4-neon`, GA 2026-11-18): $3.00 in / $15.00 out / $0.15 cached, flat; 1,048,576 unified on the API but "Up to 512K tokens" on Antigravity; thinking levels low, medium, high and xhigh; paid Pro, Ultra and Enterprise. Rows, one pair per Gemini 4 model, the level in the effort menu: `gemini-4-neon` (524,288, alias `neon`) and `gemini-4-neon[1m]` (1,048,576); `gemini-4-carbon[1m]` (alias `carbon`) and `gemini-4-carbon`; `gemini-4-hydrogen[1m]` (alias `hydrogen`) and `gemini-4-hydrogen`; Argon's pair gains the menu low, medium, high. Antigravity serves Gemini 4 only under thinking-level ids (Google publishes `gemini-4-neon-xhigh`; third parties report the rest and a 400 on bare `gemini-4-neon`), so a bare Gemini 4 id or alias now leaves llmux as `<id>-<level>`, the level from the client's effort (`-high` when none). Before, `argon` went out as the bare `gemini-4-argon`, which most likely failed on every plan ([#244](https://github.com/2lab-ai/llmux/issues/244)). The default `gemini.user_agent` moved from `antigravity/hub/2.23.1` to `2.34.0` (hub manifest, read 2026-11-21); third parties report the Gemini 4 ids hidden or 404 below 2.29. A config that still holds exactly the old default is moved at load; any other value is kept. Pricing: `gemini-4-neon`, `gemini-4-neon-*` and `neon` at Neon's rate, and bare `carbon` / `hydrogen` at their families' (before, the Flash fallback priced Neon 4× too low). Google names no Helium, Krypton or Oxygen model id. The Flash, Argon, Carbon and Hydrogen ids, context and prices are unchanged. | [Gemini API changelog](https://aistudio.google.com/docs/changelog), [models](https://aistudio.google.com/docs/models), [pricing](https://aistudio.google.com/docs/pricing), [Gemini 4 Neon](https://aistudio.google.com/docs/models/gemini-4-neon), [Gemini 4 Carbon](https://aistudio.google.com/docs/models/gemini-4-carbon), [Gemini 4 Argon](https://aistudio.google.com/docs/models/gemini-4-argon), [Gemini 4 Hydrogen](https://aistudio.google.com/docs/models/gemini-4-hydrogen), [Neon announcement](https://blog.google/innovation-and-ai/models-and-research/gemini-models/introducing-gemini-4-neon/), [Neon model card](https://deepmind.google/models/model-cards/gemini-4-neon/), [Antigravity models](https://antigravity.google/docs/models), [Antigravity changelog](https://antigravity.google/docs/changelog), [Neon in Antigravity](https://antigravity.google/blog/gemini-4-neon-in-google-antigravity/), Antigravity hub manifest (`latest-arm64-mac.yml`, `version: 2.34.0`); third-party id evidence: router-for-me/CLIProxyAPI `internal/registry/models/models.json` @ c1040cc0d9, cheaper-inference/OmniRoute PR #20666, YukieMiona/dsh-antigravity-oauth#13 |
| OpenRouter | Not re-checked in this sweep; no row changed. | — |

Not changed, recorded:

- **Gemini 4 ids are not yet verified by llmux.** The daemon's Google AI Free/Plus account lists
  none of Neon, Carbon, Argon or plain Hydrogen, so no suffixed id has been served through llmux.
  [#244](https://github.com/2lab-ai/llmux/issues/244) and
  [#310](https://github.com/2lab-ai/llmux/issues/310) stay open until a Pro or Ultra account
  probes `fetchAvailableModels` and one stream each on a bare and a suffixed id, at the new
  User-Agent. The native client also sends `generationConfig.thinkingConfig.thinkingLevel`
  matching the suffix; llmux does not, and whether Gemini 4 needs it is unknown.
- **Claude Code's 1M assumption against grok's 500k.** On Claude Code 2.1.323 and later a bare
  `grok` session sizes itself at 1M while xAI cuts requests off at 500,000, so a long session can
  be rejected upstream before the client compacts. Recorded; llmux does not guard it yet.
- **Codex `gpt-reserve`** is hidden and unpriced, and has no passthrough: a request naming it gets
  the codex pin.
- **Codex Ultrafast** is not modelled: llmux sends only `service_tier: "priority"` (Fast), so it
  can neither request nor price Ultrafast (3× Standard on 6.2 through 2027-02-28, 6× from
  2027-03-01; 6× on `gpt-6.1-sol` and `gpt-6-astra`).
- **`gpt-5.6-sol` promotional price** still ends 2026-11-30, and OpenAI still publishes no later
  price.
- **`grok-build-0.1`** reached end of life on 2026-11-19. It was never a curated row; a config
  that pins it now fails upstream.
- **Sonnet 4.5 retirement.** `claude-sonnet-4-5-20250929` / `claude-sonnet-4-5` retire on
  2026-11-30 (replacement `claude-sonnet-5-5`). Neither is a row; the price row stays for history.
- **Gemini 4 Helium** was only teased (2026-11-19): no model id, no docs page, so no alias.
- **Evidence from before the change** (llmux 0.2.26 preview, the owner's Mac daemon, 2026-11-21):
  `argon` → 404 (Cloud Code NOT_FOUND, Google AI Plus account); `grok` → 200, served
  `grok-4.7-build`; `gemini` → 502 (transient upstream rate limit); `sonnet`, `haiku`, `fable`,
  `opus`, `terra`, `luna`, `sol`, `astra`, `gpt-6`, `gpt-6.1` and `gpt-6.2` → 200 on their
  catalog ids. Probes after this change follow the deploy.

## Model sweep: 2026-11-18

Checked all four providers against their current primary sources for 2026-11-10 → 2026-11-18
(all fetched 2026-11-18). Two new text models joined the catalog, `gpt-6.2-sol` and
`gpt-6.2-astra`, each with a `[1m]` twin, and the `sol` and `astra` aliases moved to them.
`gpt-6.1-sol`'s base context rose to 500000 and `claude-opus-4-6[1m]` lost `xhigh` from its effort
menu. The four dead OpenRouter rows were retired and the default `or` pin moved to
`or-nemotron-3-ultra` ([#243](https://github.com/2lab-ai/llmux/issues/243)). Gemini 4 Carbon and
Hydrogen got reference prices but no rows. Catalog size: 53 curated rows (4 codex rows added, 4
openrouter rows removed).

| Provider | Result | Sources checked |
| --- | --- | --- |
| Claude | No new model id and no price or context change. Claude Code 2.1.327 (npm `latest`, 2026-11-17) and Agent SDK 0.3.327 carry no unknown `claude-*` id, and their aliases are unchanged: `fable` → `claude-fable-5-2`, `opus` → `claude-opus-5-5`, `sonnet` → `claude-sonnet-5-5`, `haiku` → `claude-haiku-5-5`. Corrected `claude-opus-4-6[1m]`'s efforts to low, medium, high, max. The 2.1.327 binary gives Opus 4.6 the `effort` and `max_effort` capabilities but not `xhigh_effort`, which every other catalog row has. The model-config page agrees: Opus 4.6 accepts low, medium, high and max, and `xhigh` falls back to `high`. This did not change in the window; llmux had advertised the full list. Every row re-checked against the pricing page: unchanged. | [models overview](https://platform.claude.com/docs/en/models/overview), [pricing](https://platform.claude.com/docs/en/about-claude/pricing), [release notes](https://platform.claude.com/docs/en/release-notes/overview), [deprecations](https://platform.claude.com/docs/en/about-claude/model-deprecations), [news](https://www.anthropic.com/news), [Claude Code changelog](https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md), [Claude Code model config](https://code.claude.com/docs/en/model-config), npm `@anthropic-ai/claude-code-darwin-arm64@2.1.327`, npm `@anthropic-ai/claude-agent-sdk@0.3.327` |
| Codex / OpenAI | Added `gpt-6.2-sol` (GPT-6.2-Sol) and `gpt-6.2-astra` (GPT-6.2-Astra), both released 2026-11-17, each with a `[1m]` twin. `models.json` @ `6c0cee502d21` ("Add GPT-6.2 Sol and Astra to model catalogs", 2026-11-17) is the only commit to touch the file since the last sweep. It gives sol context_window 500000 and astra 272000 (max_context_window 872000 on both), efforts low…ultra (default low), minimal client 0.166.0, and picker priority 1 (sol) and 2 (astra). The base rows carry the `models.json` figure; the twins carry OpenAI's published 1,050,000 as 1000000. Reference prices (Standard, ≤272K input): sol $2.00 in / $0.10 cached / $2.50 cache write 30m / $5.00 cache write 12h / $10.00 out; astra $8.00 / $0.40 / $10.00 / $20.00 / $40.00. Above 272K input the whole request bills at 2× input and cache rates and 1.5× output; Fast is 2×. Before these rows both ids took the codex group fallback (gpt-5.5's $5 / $30). `sol` moved from `gpt-6.1-sol` to `gpt-6.2-sol`, and `astra` from `gpt-6-astra[1m]` to `gpt-6.2-astra[1m]` (astra keeps its alias on the `[1m]` row). The new bare `gpt-6.2` resolves to `gpt-6.2-sol`. `gpt-6.1` stays on `gpt-6.1-sol` and `gpt-6` on `gpt-6-astra[1m]`; `terra` and `luna` are unchanged, because 6.2 has no terra or luna tier. Both slugs joined the codex passthrough list and the TUI `m` quick-pick; neither has been probed as a passthrough slug yet. The same commit raised `gpt-6.1-sol`'s context_window from 272000 to 500000, so its base row now advertises 500000; the Codex models page agrees ("GPT-6.1 Sol uses a default context window of 500,000 tokens in Codex"). No other row's price, context or efforts changed. | [models.json @ 6c0cee5](https://github.com/openai/codex/blob/6c0cee502d215e5df2dd9a303cf2e252229c9e12/codex-rs/models-manager/models.json), [commit 6c0cee5](https://github.com/openai/codex/commit/6c0cee502d215e5df2dd9a303cf2e252229c9e12), [gpt-6.2-sol](https://developers.openai.com/api/docs/models/gpt-6.2-sol), [gpt-6.2-astra](https://developers.openai.com/api/docs/models/gpt-6.2-astra), [pricing](https://developers.openai.com/api/docs/pricing), [latest-model guide](https://developers.openai.com/api/docs/guides/latest-model?model=gpt-6.2-astra), [API changelog](https://developers.openai.com/api/docs/changelog), [Codex changelog](https://developers.openai.com/codex/changelog), [Codex models](https://developers.openai.com/codex/models) |
| Grok / xAI | No new text model id and no row change: `grok-4.7`, `grok-4.6` and `grok-4.5` keep 500k context and the same prices and efforts. Grok 4.8 is not on the API (its docs page returns 404), and Grok 5 has no API id. `grok-4.5`'s docs still list `xhigh` while the live menu llmux follows does not; this is unchanged and was already recorded. | [release notes](https://docs.x.ai/developers/release-notes), [models](https://docs.x.ai/developers/models), [pricing](https://docs.x.ai/developers/pricing), [grok-4.7](https://docs.x.ai/developers/models/grok-4.7), [grok-4.6](https://docs.x.ai/developers/models/grok-4.6), [grok-4.5](https://docs.x.ai/developers/models/grok-4.5) |
| Gemini / Google (Antigravity) | Two new GA text models, neither added as a row. `gemini-4-carbon` went GA on 2026-11-10; Antigravity offers it on Ultra and Enterprise. This supersedes the 2026-11-10 entry, which said Carbon had no published id. `gemini-4-hydrogen` went GA on 2026-11-12; Antigravity offers it on paid Pro, Ultra and Enterprise. Both have a 1,048,576-token context. Neither is visible on the daemon's Free/Plus Antigravity account, and Google publishes no Antigravity id form for them except `gemini-4-hydrogen-xhigh`, so the rows wait for a Pro/Ultra probe ([#310](https://github.com/2lab-ai/llmux/issues/310)). Any `gemini-*` id passes through, and both used to fall to the 3.8 Flash fallback ($0.75 / $3.75), so they now have reference prices. `gemini-4-carbon*`: $2.00 in / $10.00 out / $0.10 cached through 2026-11-30, then $4.00 / $20.00 / $0.20 from 2026-12-01 (Argon's dated card). `gemini-4-hydrogen*`: a flat $1.50 / $7.50 / $0.075 ("flat across its entire 1 million-token context window"). The Flash and Argon rows are unchanged. | [Gemini API changelog](https://aistudio.google.com/docs/changelog), [models](https://aistudio.google.com/docs/models), [pricing](https://aistudio.google.com/docs/pricing), [Gemini 4 Carbon](https://aistudio.google.com/docs/models/gemini-4-carbon), [Gemini 4 Hydrogen](https://aistudio.google.com/docs/models/gemini-4-hydrogen), [Gemini 4 Argon](https://aistudio.google.com/docs/models/gemini-4-argon), [Carbon announcement](https://blog.google/innovation-and-ai/models-and-research/gemini-models/gemini-4-carbon-now-available/), [Hydrogen announcement](https://blog.google/innovation-and-ai/models-and-research/gemini-models/introducing-gemini-4-hydrogen/), [Antigravity models](https://antigravity.google/docs/models/), [Antigravity changelog](https://antigravity.google/docs/changelog) |
| OpenRouter | Retired `or-ox-alpha`, `or-glm-5.2`, `or-laguna-s-2.1` and `or-gpt-oss-20b`. They have had 0 endpoints since 2026-11-10, and on 2026-11-18 none was listed in `/api/v1/models` ([#243](https://github.com/2lab-ai/llmux/issues/243)). The default `or` pin moved from `stealth/ox-alpha` to `nvidia/nemotron-3-ultra-550b-a55b:free`, so a bare `or` lands on the existing `or-nemotron-3-ultra` row (free, live since 2026-06, 1 endpoint, 1,000,000 context). Unlike the grok pin rolls, a config whose `openrouter.default_model` still names `stealth/ox-alpha` (or another retired slug) is moved to the new default at load, because every config written before the roll serialized the old default and the slug can no longer be served; a live slug an operator chose is kept. Usage recorded under the retired slugs keeps its $0 price. The other six rows were live and free, with unchanged context. | [`GET /api/v1/models`](https://openrouter.ai/api/v1/models), `GET /api/v1/models/<slug>/endpoints` for every curated slug, [#243](https://github.com/2lab-ai/llmux/issues/243) |

Not changed, recorded:

- **`gpt-5.5` is dead on the ChatGPT backend.** A probe through the daemon on 2026-11-18 returned
  400 "The 'gpt-5.5' model is not supported when using Codex with a ChatGPT account." It is
  missing from the live Codex server list; OpenAI retired it from Codex with ChatGPT sign-in on
  2026-10-14. The row was removed on 2026-11-19 (catalog 53 → 52 rows,
  [#309](https://github.com/2lab-ai/llmux/issues/309)); the passthrough stays so a request that
  names it gets that 400 instead of silently running on the codex pin, and its price row stays
  for recorded usage.
- **Gemini 4 Carbon / Hydrogen rows** wait for a Pro or Ultra account to probe the Antigravity id
  form ([#310](https://github.com/2lab-ai/llmux/issues/310)). This is the same open question as
  Argon's ([#244](https://github.com/2lab-ai/llmux/issues/244)).
- **GPT-6.2 Responses behaviour.** GPT-6.2 appends the top-level `instructions` after `input`, so
  reusable instructions stay cacheable only as a developer message at the start of `input`. The
  API drops reasoning items after prefix edits, after some model switches (downgrades included)
  and after end-user or organization changes. `temperature`, `top_p` and `logprobs` are not
  supported. Recorded for the codex translation; this sweep does not change it.
- **Opus 4.8 fast mode** is removed on 2026-11-30 (deprecated 2026-10-30). After that date
  `claude-opus-4-8` with `speed: "fast"` returns an error. The base row is unaffected, and the
  price row keeps its fast tier for history.
- **`gpt-5.6-sol` promotional price** ends 2026-11-30. The price after that is still not
  published.
- **`or-dots-3-note`** carries an OpenRouter `expiration_date` of 2026-12-31.
- **Evidence from before the change.** On 2026-11-18, a request for `gpt-6.2-sol` through the
  daemon (llmux 0.2.26 preview) was served by the codex pin, `gpt-6-astra`, and so was one for
  `gpt-6.2-astra`. Neither slug was a passthrough slug yet, so the request silently took the
  pin. The same probe showed `sol` served by `gpt-6.1-sol` and `astra` by `gpt-6-astra`.

## Reference price corrections: 2026-11-11

Not a model sweep: no catalog row, alias or context window changed. Rates read
2026-11-11 from the official pricing pages
([#278](https://github.com/2lab-ai/llmux/issues/278)).

| Provider | Change | Sources checked |
| --- | --- | --- |
| Claude | Added reference prices for `claude-mythos-5-2` ($8.00 in / $40.00 out / $0.20 cache read / $10.00 cache write, 5m), `claude-mythos-5-1` ($10.00 / $50.00 / $0.25 / $12.50) and `claude-mythos-5` ($10.00 / $50.00 / $1.00 / $12.50). All three are limited availability and are not catalog rows; before this a request naming them was priced at the Opus tier. 1-hour writes are $16 / $20 / $20 (see the next row). | [pricing](https://platform.claude.com/docs/en/about-claude/pricing) |
| Claude | 1-hour cache writes are priced at their own rate, 2× input, on every row (Fable 5.2 / Mythos 5.2 $16, Fable 5.1 / 5 and Mythos 5.1 / 5 $20, Opus 5.5 $8, Opus 5 / 4.x $10, Sonnet 5.5 / 5 $4, Sonnet 4.6 / 4.5 $6, Haiku 5.5 $0.20, Haiku 4.5 $2), on the 1-hour part each response reports in `usage.cache_creation.ephemeral_1h_input_tokens`; the rest of the writes keep the 5-minute rate. Before this every write was priced at the 5-minute rate. Server web searches (`usage.server_tool_use.web_search_requests`) add $10 per 1,000 searches; web fetch has no extra charge and is not counted. | [pricing](https://platform.claude.com/docs/en/about-claude/pricing), [prompt caching](https://platform.claude.com/docs/en/build-with-claude/prompt-caching) |
| Codex / OpenAI | `gpt-5.6-terra` cache write 0 → $2.50 and `gpt-5.6-luna` 0 → $0.25 (the "30m write" column, 1.25× input; the model pages state the same multiplier). `gpt-5.5` (and the codex fallback that uses its row) lists no cache-write rate; the prompt-caching guide says models before GPT-5.6 have "no additional cache-write charge", so a written token bills as ordinary input and the row's write rate is now its input rate ($5.00, was 0 — written tokens left fresh input with the cache-write fix and would otherwise be free). | [pricing](https://developers.openai.com/api/docs/pricing), [prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching), [gpt-5.6-terra](https://developers.openai.com/api/docs/models/gpt-5.6-terra), [gpt-5.6-luna](https://developers.openai.com/api/docs/models/gpt-5.6-luna) |
| Codex / OpenAI | 12-hour cache writes are priced at their own rate, 2.5× input, on every GPT-5.6 and later row (see [the table below](#current-openai-cache-write-reference-prices)), on the 12-hour part each response reports in `input_tokens_details.cache_write_tokens_details.ttl_12h_tokens`; the rest keep the 30-minute rate. `gpt-5.5` has no separate write charge at either TTL, so a write at any TTL bills at its input rate. | [pricing](https://developers.openai.com/api/docs/pricing), [prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching), [Responses API reference](https://developers.openai.com/api/reference/resources/responses/methods/create) |
| Grok / xAI | No separate cache-write price is listed, so `grok-4.5` / `grok-4.6` / `grok-4.7` (and the grok fallback) price a written token at the input rate: write rate 0 → $2.00. | [models](https://docs.x.ai/developers/models) |
| Claude | Added reference prices for the retired `claude-opus-4-1` and `claude-opus-4` (and their dated snapshots and `claude-opus-4-0`): $15.00 in / $75.00 out / $1.50 cache read / $18.75 cache write (5m) / $30.00 (1h). Before this they fell to the $5 Opus tier. | [pricing](https://platform.claude.com/docs/en/about-claude/pricing) |
| Claude | Haiku 5.5's long-context tier is priced per request: a prompt over 100,000 tokens (input + cache reads + cache writes) bills the whole request at 5× every rate ($0.50 / $2.50 / $0.05 / $0.625 5m / $1.00 1h). | [pricing](https://platform.claude.com/docs/en/about-claude/pricing) |
| Codex / OpenAI | Long-context tier on every listed GPT row: a prompt over 272,000 tokens bills the whole request at 2× input, cached input and both cache writes and 1.5× output ([table below](#per-request-price-classes)). Codex Fast mode (recorded per request since 2026-09) is priced: 2× Standard on the 6.x and 5.6 rows, long context included; 2.5× on `gpt-5.5` ($12.50 / $1.25 / $75.00). The page lists no long-context Fast rate for `gpt-5.5`, so such a request prices at the long Standard rates. `gpt-5.5-codex` and other unlisted codex ids keep a flat price. | [pricing](https://developers.openai.com/api/docs/pricing) |
| Grok / xAI | Long-context tier: a prompt of 200,000 tokens or more bills the whole request at 2× ($4.00 / $12.00 and cached $0.60 on `grok-4.5`, $1.00 on `grok-4.6` / `grok-4.7`; writes at the long input rate). | [pricing](https://docs.x.ai/developers/pricing) |
| Gemini / Google | Dated rates: `gemini-4-argon` is $2.00 / $10.00 / $0.10 through 2026-11-30 and $4.00 / $20.00 / $0.20 from 2026-12-01; Gemini 3.8 Flash is $0.75 / $3.75 / $0.075 through 2026-12-31 and $1.50 / $7.50 / $0.15 from 2027-01-01. Each request is priced at the rates of its own UTC day. | [pricing](https://aistudio.google.com/docs/pricing) |

## Model sweep: 2026-11-10

Checked all four providers against their current primary sources for 2026-10-23 → 2026-11-10
(all fetched 2026-11-10). Two new text models joined the catalog, `claude-fable-5-2` and
`gpt-6.1-terra`, and the `fable` and `terra` aliases moved to them. No existing row changed its
price, context window or effort menu. Catalog size: 53 curated rows (was 50).

| Provider | Result | Sources checked |
| --- | --- | --- |
| Claude | Added `claude-fable-5-2[1m]` (Claude Fable 5.2, released 2026-11-09, all Claude API customers): 1,000,000 context (the default and the maximum, no 200K mode), 128K max output, efforts low…max. Reference prices $8.00 in / $40.00 out / $0.20 cache read / $10.00 cache write (5m) / $16.00 (1h), no long-context tier; before this row the id fell to the Fable 5 tier and was overpriced. One `[1m]` row and no bare twin, like Fable 5.1: Claude Code 2.1.321 (npm `latest` since 2026-11-09, and the minimum for Fable 5.2) accepts the suffix on Fable but ignores it. `fable` moved from `claude-fable-5-1[1m]` to the new row, matching Claude Code 2.1.321, which resolves its own `fable` to `claude-fable-5-2`; `fable-5-1` stays on 5.1. Every existing row re-checked against the pricing page: no price, context or max-output change. | [Fable 5.2 model page](https://platform.claude.com/docs/en/models/fable-5-2/overview), [What's new in Fable 5.2](https://platform.claude.com/docs/en/models/fable-5-2/whats-new-fable-5-2), [pricing](https://platform.claude.com/docs/en/about-claude/pricing), [release notes](https://platform.claude.com/docs/en/release-notes/overview), [announcement](https://www.anthropic.com/claude-fable-and-mythos-5-2), [models overview](https://platform.claude.com/docs/en/models/overview), [deprecations](https://platform.claude.com/docs/en/about-claude/model-deprecations), [Claude Code changelog](https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md), [Claude Code model config](https://code.claude.com/docs/en/model-config), npm `@anthropic-ai/claude-code-darwin-arm64@2.1.321` |
| Codex / OpenAI | Added `gpt-6.1-terra` (GPT-6.1 Terra, released 2026-10-26) and its `[1m]` twin. `models.json` @ `caae161dd0c8` (2026-11-09); its only change since the last sweep is commit `126a0ee92ed2`, which adds this entry: context_window 500000, max_context_window 872000, efforts low…ultra (default low), minimal client 0.153.0. The base row carries 500000; the twin carries OpenAI's published 1,050,000 (128K output) as 1000000. Reference prices $1.00 in / $0.05 cached / $1.25 cache write (30-minute retention) / $5.00 out; before this row the id fell to the `gpt-6.1-sol` rate, twice its list price. Above 272K input tokens the whole request bills at 2× input and cache rates and 1.5× output; llmux did not model that then (priced per request since 2026-11-12, [price classes](#per-request-price-classes)). `terra` moved from `gpt-5.6-terra` to `gpt-6.1-terra`. The slug is in the codex passthrough list but has not been probed through the daemon; a live probe follows the preview deploy. `sol`, `luna`, `astra` and the bare generation ids are unchanged, and every existing price still matches. | [models.json @ caae161](https://github.com/openai/codex/blob/caae161dd0c82f2e04e57e6f9d738fafbce503ba/codex-rs/models-manager/models.json), [commit 126a0ee](https://github.com/openai/codex/commit/126a0ee92ed2c5e73cd8a8f7e37563e8527552d7), [gpt-6.1-terra](https://developers.openai.com/api/docs/models/gpt-6.1-terra), [pricing](https://developers.openai.com/api/docs/pricing), [API changelog](https://developers.openai.com/api/docs/changelog), [Codex changelog](https://developers.openai.com/codex/changelog), [Codex models](https://developers.openai.com/codex/models) |
| Grok / xAI | No new text model id since `grok-4.7` (September). `grok-4.7`, `grok-4.6` and `grok-4.5` are unchanged: 500k context, the same prices and efforts. Grok 4.8 and Grok 5 have been announced or are in training, but neither has an API id. | [Release notes](https://docs.x.ai/developers/release-notes), [models](https://docs.x.ai/developers/models), [pricing](https://docs.x.ai/developers/pricing), [grok-4.7](https://docs.x.ai/developers/models/grok-4.7), [grok-4.6](https://docs.x.ai/developers/models/grok-4.6), [grok-4.5](https://docs.x.ai/developers/models/grok-4.5) |
| Gemini / Google (Antigravity) | No new servable text id and no row change. Gemini 4 Carbon was announced on 2026-10-28 with no published id, for a closed cyber-defender programme only (superseded: it went GA as `gemini-4-carbon` on 2026-11-10, see [Model sweep: 2026-11-18](#model-sweep-2026-11-18)). Argon's list price doubles to $4.00 / $20.00 / $0.20 from 2026-12-01; llmux's static rate did not model that then (dated rates since 2026-11-12, [price classes](#per-request-price-classes)). The Antigravity models page now lists Argon for paid Google AI Pro (or a trial on eligible premium devices), Ultra and Enterprise, not Free or Plus (see [Gemini](#gemini)). Third-party reports, not from Google and not verified by llmux, say Antigravity serves Argon as `gemini-4-argon-high` / `-medium` / `-low` and gates it on a client User-Agent of 2.29 or later; llmux sends the bare `gemini-4-argon` with UA `antigravity/hub/2.23.1` ([#244](https://github.com/2lab-ai/llmux/issues/244)). | [Gemini API changelog](https://aistudio.google.com/docs/changelog), [models](https://aistudio.google.com/docs/models), [pricing](https://aistudio.google.com/docs/pricing), [Gemini 4 Argon](https://aistudio.google.com/docs/models/gemini-4-argon), [Antigravity models](https://antigravity.google/docs/models/), [Antigravity changelog](https://antigravity.google/docs/changelog), [Gemini 4 Carbon](https://blog.google/innovation-and-ai/models-and-research/gemini-models/introducing-gemini-4-carbon/) |

Not changed, recorded:

- **`gpt-5.5` on Codex.** It was retired from Codex for ChatGPT sign-in on 2026-10-14
  ([Codex changelog](https://developers.openai.com/codex/changelog)), and llmux uses the
  ChatGPT-account backend. The row and its passthrough stay; whether it still answers is
  untested. (Superseded: the 2026-11-18 probe returned 400 and the row was removed on
  2026-11-19, [#309](https://github.com/2lab-ai/llmux/issues/309).)
- **`gpt-6.1-sol` context.** The live Codex server list (`~/.codex/models_cache.json`, fetched
  2026-11-09, Codex CLI 0.167.0) and OpenAI's docs give it 500000 in Codex, while `models.json`
  still says 272000. The row keeps the `models.json` value (`models.json` moved to 500000 on
  2026-11-17 and the row followed on 2026-11-18).
- **`gpt-5.6-sol` promotional price** now runs until 2026-11-30. The price after that is not
  published.
- **Fable 5.2 passthrough behaviour.** Upstream, editing an earlier turn always fails the
  prefix check with a 400, and Fable 5.2 thinking blocks can be read only by Fable 5.2 and
  Mythos 5.2 (switching to another model drops them). llmux passes claude requests through and
  changes neither.
- **Mythos 5.2** (2026-11-09) is limited to organisations verified through Anthropic programmes,
  so it is not a row, the same as Mythos 5.1.
- **`grok-4.5` efforts.** xAI's public docs list `xhigh`, but the live `cli-chat-proxy` menu,
  which llmux follows, did not on its last read. Unchanged.
- **OpenRouter.** The curated `or-ox-alpha` (the default `or` pin), `or-glm-5.2`,
  `or-laguna-s-2.1` and `or-gpt-oss-20b` had 0 endpoints on 2026-11-10
  ([#243](https://github.com/2lab-ai/llmux/issues/243); retired 2026-11-18).

## Model sweep: 2026-10-23

Checked all four providers against their current primary sources for 2026-10-12 → 2026-10-23.
No provider lists a new text model id and no context window changed. Two stale reference prices were
corrected (Codex, Grok rows). Separately, by user decision the explicit Haiku 4.5 ids now float to the
`haiku` owner on Claude Code / Agent SDK–shaped requests with default sampling and the `claude-haiku-4-5` row is retired
(see [the alias notes](#alias-semantics) above).

| Provider | Result | Sources checked |
| --- | --- | --- |
| Claude | No new model id: the lineup is still Fable 5.1, Opus 5.5, Sonnet 5.5, Haiku 5.5 (newest, 2026-10-07). Reference prices unchanged. In-window but not catalog changes: Opus 5.5 fast mode left research preview (2026-10-15) and API version `2026-10-21` shipped. | [current lineup](https://platform.claude.com/docs/en/models/overview), [pricing](https://platform.claude.com/docs/en/about-claude/pricing), [release notes](https://platform.claude.com/docs/en/release-notes/overview) |
| Codex / OpenAI | `models.json` unchanged since `d63a9b8344cf` (2026-10-06); checked at HEAD `30587d78850a` (2026-10-23): same picker ids, contexts and `minimal_client_version`s. New 12-hour cache-write tier (2026-10-20) is not modeled (priced since 2026-11-11). Corrected `gpt-5.6-terra` to the model page's $2.00 in / $0.20 cached / $12.00 out (was $2.50 / $0.25 / $15.00 from the 2026-07-10 row). | [Codex model catalog @ 30587d78](https://github.com/openai/codex/blob/30587d78850abf2bfc484ffb0ab0f71497cc9c05/codex-rs/models-manager/models.json), [API changelog](https://developers.openai.com/api/docs/changelog), [gpt-5.6-terra](https://developers.openai.com/api/docs/models/gpt-5.6-terra) |
| Grok / xAI | No new text model id (only `grok-voice-3.0`, a speech model, 2026-10-14). `grok-4.7` unchanged (500k, $2.00 / $0.50 cached / $6.00 below 200k). Corrected `grok-4.5` cached input to the listed $0.30 (was $0.50). | [Release notes](https://docs.x.ai/developers/release-notes), [models](https://docs.x.ai/developers/models) |
| Gemini / Google | No new id affecting the catalog rows. `gemini-4-argon` reached GA on 2026-10-13 under the same id; context and prices unchanged ($2 / $10 / $0.10 through 2026-11-30, Flash $0.75 / $3.75 / $0.075 through 2026-12-31 — both later doublings priced by date since 2026-11-12, [price classes](#per-request-price-classes)). | [changelog](https://ai.google.dev/gemini-api/docs/changelog), [models](https://ai.google.dev/gemini-api/docs/models), [pricing](https://ai.google.dev/gemini-api/docs/pricing) |

## Model sweep: 2026-10-15

Added Gemini as the fifth backend group. Claude, Codex and Grok were not
re-swept that day; their latest check is [2026-10-12](#model-sweep-2026-10-12).

| Provider | Result | Sources checked |
| --- | --- | --- |
| Gemini / Google (Antigravity) | Added eight rows: `gemini-3.8-flash-high`, `-medium`, `-low` and `gemini-4-argon`, each with a `[1m]` twin. Aliases: `gemini` / `gemini-3.8-flash` → `gemini-3.8-flash-high[1m]`, `argon` → `gemini-4-argon[1m]`. Context 1,048,576 on every row (3.8 Flash: 1,048,576 in / 65,536 out; Argon: 1,048,576 unified, 65,536 out by default). The live `fetchAvailableModels` read on a Google AI Plus account listed the three Flash ids (ctx 1,048,576 / out 65,536); Argon is Ultra/Enterprise only and returned 404 NOT_FOUND on that account. Reference prices per 1M tokens: 3.8 Flash $0.75 in / $3.75 out / $0.075 cached input (through 2026-12-31), Argon $2.00 / $10.00 / $0.10 (through 2026-11-30); both then double, which llmux's static rates did not model then (dated rates since 2026-11-12). | [Gemini 3.8 Flash](https://ai.google.dev/gemini-api/docs/models/gemini-3.8-flash) (updated 2026-09-02), [Gemini 4 Argon](https://ai.google.dev/gemini-api/docs/models/gemini-4-argon) (updated 2026-10-14), [pricing](https://ai.google.dev/gemini-api/docs/pricing) (updated 2026-10-14), [Antigravity models](https://antigravity.google/docs/models/) (fetched 2026-10-15), live `fetchAvailableModels` (2026-10-15) |

## Model sweep: 2026-10-12

Checked all three subscription providers against their current primary sources.
No provider lists a new model id, and no catalog row or reference price changed
(one live availability finding on Codex, recorded in its row).

| Provider | Result | Sources checked |
| --- | --- | --- |
| Claude | Current lineup unchanged: Fable 5.1, Opus 5.5, Sonnet 5.5, Haiku 5.5 are all present. There is no `claude-fable-5-5` on the platform (Claude Code 2.1.296 carries no such id either); Mythos 5.1 is limited availability (Claude Code 2.1.296 knows `claude-mythos-5-1`) and is not a catalog row. Pricing re-read: every reference rate in `src/pricing.rs` still matches (cache read: Fable 5.1 $0.25, Opus 5.5 $0.20, Sonnet 5.5 $0.10, Haiku 5.5 $0.01 in the ≤100K tier). | [current lineup](https://platform.claude.com/docs/en/models/overview), [pricing](https://platform.claude.com/docs/en/about-claude/pricing) |
| Codex / OpenAI | `models.json` @ `a9b0815167c2` lists the same picker ids as on 2026-10-08 (GPT-6.1 Sol, GPT-6 Astra/Sol/Luna, GPT-5.6 Sol/Terra/Luna, GPT-5.5; 272K / 872K metadata unchanged, GPT-5.5 272K / 272K). The hidden `gpt-daybreak-*` and `codex-auto-review` ids remain non-picker. Live probe through the daemon the same day: every picker id tried (`gpt-6-astra`, `gpt-6.1-sol`, `gpt-6-sol`, `gpt-5.6-sol`) returned HTTP 400 `The '<model>' model is not supported when using Codex with a ChatGPT account.` — all on the scheduler's active codex account, whose token (refreshed 01:30Z that day; last 200 on that account 2026-10-10) now carries plan `free` (the other codex accounts carry `pro`; official Codex CLI 0.160.1 on a pro account answers normally). After pausing that account, `gpt-6.1-sol` and `gpt-6-astra` returned 200 on a pro account (`gpt-6-sol` / `gpt-5.6-sol` not re-tried after the pause). An account-eligibility problem, not a catalog change; llmux's missing failover is [#204](https://github.com/2lab-ai/llmux/issues/204). | [Codex model catalog](https://github.com/openai/codex/blob/main/codex-rs/models-manager/models.json) |
| Grok / xAI | No new text model beyond the `grok-4.7` family (September: `grok-4.7` on the public API, Grok 4.7 Fast for Cursor/Grok Build only — the uncurated `grok-4.7-build-fast` — and `grok-voice-transcribe-2.0`; October: only the `grok-voice-transcribe-1.0` EOL on 2026-10-02). Live `cli-chat-proxy` `/v1/models` with a real subscription token: the same four ids (`grok-4.7`, `grok-4.7-build-fast`, `grok-4.6`, `grok-4.5`) and the same effort menus, but the response shape changed — `context_window` now reads **256000** on every id, next to a new `context_windows: [256000, 500000]` list and a compaction hint (`auto_compact_threshold_percent: 80`). llmux sends no window selector, so the row figure was re-checked by an acceptance probe through the daemon: a `grok-4.7` request with `usage.input_tokens` 306103 (+1152 cache read) returned HTTP 200, i.e. `grok-4.7` still takes prompts well above the published 256k default (only `grok-4.7` was probed; the 500000 ceiling itself was not). The rows keep **500000**, the upper `context_windows` entry that the 2026-07-14 … 2026-09-23 reads reported as `context_window`; the selectable-window / compaction fields are a feature question ([#203](https://github.com/2lab-ai/llmux/issues/203)), not a catalog fact. See [The grok `[1m]` twin](#the-grok-1m-twin). | [Release notes](https://docs.x.ai/developers/release-notes), live `/v1/models` and daemon probe (2026-10-12) |

## Model sweep: 2026-10-08

Checked all three subscription providers against their current primary sources:

| Provider | Result | Sources checked |
| --- | --- | --- |
| Claude | Added `claude-haiku-5-5` (released 2026-10-07), 1M context / 128K max output; floated `haiku`. Both base and `[1m]` rows publish 1M; the suffix additionally selects Claude Code's context display. On stable since v0.2.25 (2026-10-08). | [Haiku specifications](https://platform.claude.com/docs/en/models/haiku-5-5/overview), [current lineup](https://platform.claude.com/docs/en/models/overview) |
| Codex / OpenAI | Current listed coding IDs are already present: GPT-6.1 Sol, GPT-6 Astra/Sol/Luna and GPT-5.6 Sol/Terra/Luna, GPT-5.5. Corrected the three 5.6 base rows from older 372K metadata to the current catalog's 272K; existing 1M twins remain. Hidden specialty/auto-review IDs and non-text APIs are not ordinary picker additions. | [Codex model catalog](https://github.com/openai/codex/blob/main/codex-rs/models-manager/models.json), [API model index](https://developers.openai.com/api/docs/models), [current guidance](https://developers.openai.com/api/docs/guides/latest-model) |
| Grok / xAI | Latest general-purpose text model remains `grok-4.7` (released 2026-09-21), already present. Grok 4.7 Fast is a service variant limited to Cursor/Grok Build, not a newly documented public API model ID; recent voice/image releases do not belong in this catalog. | [Release notes](https://docs.x.ai/developers/release-notes), [models](https://docs.x.ai/developers/models) |

### Current Claude reference prices

USD per million tokens, verified 2026-10-08 against [Anthropic pricing](https://platform.claude.com/docs/en/about-claude/pricing)
and [Haiku 5.5 specifications](https://platform.claude.com/docs/en/models/haiku-5-5/overview),
re-checked 2026-11-10, when the Fable 5.2 row was added, and 2026-11-11, when the
Mythos rows and the 1-hour write rates were added:

| Model / prompt length | Input | Output | Cache read | Cache write, 5m | Cache write, 1h |
| --- | --- | --- | --- | --- | --- |
| Haiku 5.5, ≤100,000 tokens | $0.10 | $0.50 | $0.01 | $0.125 | $0.20 |
| Haiku 5.5, >100,000 tokens | $0.50 | $2.50 | $0.05 | $0.625 | $1.00 |
| Haiku 4.5 | $1.00 | $5.00 | $0.10 | $1.25 | $2.00 |
| Sonnet 5.5 | $2.00 | $10.00 | $0.10 | $2.50 | $4.00 |
| Sonnet 5 | $2.00 | $10.00 | $0.20 | $2.50 | $4.00 |
| Sonnet 4.6 / 4.5 | $3.00 | $15.00 | $0.30 | $3.75 | $6.00 |
| Opus 5.5 | $4.00 | $20.00 | $0.20 | $5.00 | $8.00 |
| Opus 5 / 4.8 / 4.7 / 4.6 / 4.5 | $5.00 | $25.00 | $0.50 | $6.25 | $10.00 |
| Fable 5.2 | $8.00 | $40.00 | $0.20 | $10.00 | $16.00 |
| Fable 5.1 | $10.00 | $50.00 | $0.25 | $12.50 | $20.00 |
| Fable 5 | $10.00 | $50.00 | $1.00 | $12.50 | $20.00 |
| Mythos 5.2 (limited availability, added 2026-11-11) | $8.00 | $40.00 | $0.20 | $10.00 | $16.00 |
| Mythos 5.1 (limited availability, added 2026-11-11) | $10.00 | $50.00 | $0.25 | $12.50 | $20.00 |
| Mythos 5 (limited availability, added 2026-11-11) | $10.00 | $50.00 | $1.00 | $12.50 | $20.00 |

Sonnet 5.5 cache reads were reduced from $0.20 to $0.10 on
[2026-10-07](https://www.anthropic.com/claude-haiku-5-5). Fable 5.1 now has its
own $0.25 cache-read reference rate. Explicit older Sonnet 5 and Fable 5 keep
their own prices. Haiku 4.5 records from before 2026-10-23 keep Haiku 4.5
identity and pricing. From then on a request that floats is served by Haiku 5.5
and recorded (and priced) as `claude-haiku-5-5`; a bare, custom-system or
non-default-sampling request naming a 4.5 id still reaches Haiku 4.5 and keeps
its 4.5 id and price
([alias stability](#alias-semantics)).

Server web search is $10 per 1,000 searches on top of the tokens; web fetch has
no extra charge (same pricing page, read 2026-11-11).

**Estimator scope:** `src/pricing.rs` prices each request with the rates of
its own [price class](#per-request-price-classes). Haiku 5.5's >100K tier
follows the page's per-request rule: "a request whose prompt is over 100,000
tokens pays higher prices. A request's prompt length counts all of its input
tokens, including cache reads and cache writes. Each request is priced on its
own" ([pricing](https://platform.claude.com/docs/en/about-claude/pricing), read
2026-11-12). llmux measures that prompt on the request's top-level usage — the
whole request, every executor iteration of an advisor turn included — and
never on an aggregate's summed tokens.
Writes a response does not report as 1-hour take the 5-minute rate and those it
does the 1-hour rate (`usage.cache_creation.ephemeral_1h_input_tokens`, since
2026-11-11; earlier history has no split and keeps the 5-minute price). Each
request's server web searches add $10 per 1,000. Claude fast mode and US-only
inference are priced from what the response reports (`usage.speed`,
`usage.inference_geo`). Aggregates keep the
1-hour write subset, the search count and the tokens of each non-standard
class, so they price as the sum of their requests. These are API-equivalent
estimates, never a subscription invoice. Existing config price overrides still
take precedence: an override without `cache_creation_extended` prices 1-hour
writes at its own `cache_creation` rate, and one without `long_context` prices
a long request flat ([configuration](configuration.md#reference-price-overrides-pricing)).
The web-search fee is not a token rate and is not overridden.

### Current OpenAI cache-write reference prices

USD per million tokens, the "30m write" and "12h write" columns of the
[OpenAI pricing page](https://developers.openai.com/api/docs/pricing) (standard
tier, ≤272K input), read 2026-11-11 (the gpt-6.2 rows 2026-11-18). Writes are a part of input; the 12-hour
part is the one a response reports in
`input_tokens_details.cache_write_tokens_details.ttl_12h_tokens`.

| Model | Input | Cache write, 30m | Cache write, 12h |
| --- | --- | --- | --- |
| gpt-6.2-sol | $2.00 | $2.50 | $5.00 |
| gpt-6.2-astra | $8.00 | $10.00 | $20.00 |
| gpt-6-astra | $10.00 | $12.50 | $25.00 |
| gpt-6.1-sol | $2.00 | $2.50 | $5.00 |
| gpt-6.1-terra | $1.00 | $1.25 | $2.50 |
| gpt-6-sol | $2.00 | $2.50 | $5.00 |
| gpt-6-luna | $0.10 | $0.125 | $0.25 |
| gpt-5.6-sol | $4.00 | $5.00 | $10.00 |
| gpt-5.6-terra | $2.00 | $2.50 | $5.00 |
| gpt-5.6-luna | $0.20 | $0.25 | $0.50 |
| gpt-5.5 | $5.00 | $5.00 (input rate) | $5.00 (input rate) |

`gpt-5.5` and earlier models have "no additional cache-write charge"
([prompt caching](https://developers.openai.com/api/docs/guides/prompt-caching)):
a written token bills as ordinary input, so its write rate at either TTL is the
input rate. grok rows likewise carry the input rate as the write rate (xAI
lists no cache-write price); Gemini rows have no write rate.

### Per-request price classes

Some rates depend on facts of one request, not of any total (#278, #283). When
llmux records a finished request it resolves the request's **price class** once
from those facts and keeps it with the request: in `activity.jsonl`
(`price_class`, absent for the standard class), in the key-usage store and in
every aggregate, which keeps the tokens of each non-standard class apart. So a
model row, a usage row, a tenant cell or a key-usage report costs exactly the
sum of its requests, and history keeps the rates of its own class. A record
with no class (everything recorded before 2026-11-12) is the standard class
and prices as before. A sub-usage (advisor, compaction, billed fallback
attempt) is classified on its own prompt and model, at its own speed and the
request's geography: US-only inference covers every inference of the request.
Its speed is the `speed` its `usage.iterations` entry reports; when the entry
reports none, a declined fallback attempt ran at the speed the request asked
for that attempt ("Each [fallback] entry ... can override ... `speed` for that
attempt only", [refusals and fallback](https://platform.claude.com/docs/en/build-with-claude/refusals-and-fallback),
read 2026-11-12: the entry's override, else the request's `speed`), an advisor
call at standard speed (a separate sub-inference on another model; fast mode
is requested for the request's own model), and a compaction at the request's
served speed.

| Fact | When | Rates |
| --- | --- | --- |
| Long context | The prompt (input + cache reads + cache writes) is over 272,000 tokens on a listed GPT row, 200,000 or more on `grok-4.5` / `grok-4.6` / `grok-4.7` / `grok-4.7-build` / `grok-4.7-build-fast` (and the grok fallback), over 100,000 on `claude-haiku-5-5`. Built-in data, not configurable. | The whole request at the long rates below. |
| Fast | The request was served at the Fast tier: codex Fast mode (the requested `service_tier: "priority"`: the codex backend does not report the tier that served a request — it answered `"default"` to every priority request in llmux's raw-io capture) or Claude fast mode (the response's `usage.speed: "fast"`). | The Fast rates below; Claude fast mode is 2× every standard rate on Fable 5.2, Mythos 5.2, Opus 5.5, Opus 5 and Opus 4.8 ($16 / $80, $16 / $80, $8 / $40, $10 / $50, $10 / $50, cache rates doubled too). Rows with no fast price stay standard. |
| US-only inference | The response's `usage.inference_geo` is `"us"` (claude group). | 1.1× every rate the other facts select (on top of fast mode, and of Haiku 5.5's long tier), on Claude 4.6 and later rows. |
| Dated rates | The request's UTC day is on or after a built-in rate change (the pricing page names no time zone, so the change is at 00:00 UTC). | That change's rates. |
| Unbilled refusal | An Anthropic response with `stop_reason: "refusal"`, no output tokens and a `stop_details.category` that is not `bio`, `frontier_llm` or `reasoning_extraction` (claude group only). | No cost; the tokens stay visible. |

Long-context rates, USD per million tokens (input / cached input / cache write
30m or 5m / cache write 12h or 1h / output), read 2026-11-11 from the
[OpenAI](https://developers.openai.com/api/docs/pricing),
[xAI](https://docs.x.ai/developers/pricing) and
[Anthropic](https://platform.claude.com/docs/en/about-claude/pricing) pricing
pages:

| Model | Boundary | Long rates | Fast | Fast + long |
| --- | --- | --- | --- | --- |
| gpt-6.2-sol (read 2026-11-18) | > 272,000 | $4 / $0.20 / $5 / $10 / $15 | 2× Standard | 2× long |
| gpt-6.2-astra (read 2026-11-18) | > 272,000 | $16 / $0.80 / $20 / $40 / $60 | 2× Standard | 2× long |
| gpt-6-astra | > 272,000 | $20 / $2 / $25 / $50 / $75 | 2× Standard | 2× long |
| gpt-6.1-sol | > 272,000 | $4 / $0.20 / $5 / $10 / $15 | 2× Standard | 2× long |
| gpt-6.1-terra | > 272,000 | $2 / $0.10 / $2.50 / $5 / $7.50 | 2× Standard | 2× long |
| gpt-6-sol | > 272,000 | $4 / $0.40 / $5 / $10 / $15 | 2× Standard | 2× long |
| gpt-6-luna | > 272,000 | $0.20 / $0.02 / $0.25 / $0.50 / $0.75 | 2× Standard | 2× long |
| gpt-5.6-sol | > 272,000 | $8 / $0.80 / $10 / $20 / $30 | 2× Standard | 2× long |
| gpt-5.6-terra | > 272,000 | $4 / $0.40 / $5 / $10 / $18 | 2× Standard | 2× long |
| gpt-5.6-luna | > 272,000 | $0.40 / $0.04 / $0.50 / $1 / $1.80 | 2× Standard | 2× long |
| gpt-5.5 | > 272,000 | $10 / $1 / $10 (input rate) / $10 / $45 | $12.50 / $1.25 / $75 (2.5×) | long rates (none published) |
| grok-4.5 | ≥ 200,000 | $4 / $0.60 / $4 (input rate) / — / $12 | — | — |
| grok-4.6, grok-4.7, grok-4.7-build | ≥ 200,000 | $4 / $1.00 / $4 (input rate) / — / $12 | — | — |
| grok-4.7-build-fast (Grok 4.7 Fast, read 2026-11-21; below 200,000: $4 / $1.00 / $4 / — / $12) | ≥ 200,000 | $6 / $1.50 / $6 (input rate) / — / $18 | — | — |
| claude-haiku-5-5 | > 100,000 | $0.50 / $0.05 / $0.625 / $1.00 / $2.50 | — | — |

Dated rate changes ([Google pricing](https://aistudio.google.com/docs/pricing),
read 2026-11-11): `gemini-4-argon` (and `argon`, `gemini-4-argon-*`) $2.00 /
$10.00 / $0.10 through 2026-11-30, $4.00 / $20.00 / $0.20 from 2026-12-01;
Gemini 3.8 Flash (every thinking variant, and the gemini fallback) $0.75 /
$3.75 / $0.075 through 2026-12-31, $1.50 / $7.50 / $0.15 from 2027-01-01.
Read 2026-11-18: `gemini-4-carbon` (and `gemini-4-carbon-*`) carries Argon's
card, $2.00 / $10.00 / $0.10 through 2026-11-30 and $4.00 / $20.00 / $0.20 from
2026-12-01; `gemini-4-hydrogen` (and `gemini-4-hydrogen-*`) is a flat $1.50 /
$7.50 / $0.075. Read 2026-11-21: `gemini-4-neon` (and `neon`,
`gemini-4-neon-*`) is a flat $3.00 / $15.00 / $0.15, no dated change. Since
2026-11-21 `carbon` and `hydrogen` price like their families too, and every
Gemini 4 thinking-level id the provider sends (`gemini-4-neon-high`, …) takes
its family's rate.

Claude fast mode ([fast mode](https://platform.claude.com/docs/en/build-with-claude/fast-mode),
read 2026-11-12) "is priced at a multiplier on standard rates across the full
context window" and "Prompt caching multipliers apply on top of fast mode
pricing"; US-only inference
([data residency](https://platform.claude.com/docs/en/manage-claude/data-residency),
read 2026-11-12) is "1.1x the standard rate across all token pricing
categories" on Claude 4.6 and later, and "Data residency multipliers apply on
top of fast mode pricing". Both are classified from the served usage, so a
request that asked for fast mode but ran at standard speed (`usage.speed:
"standard"`) prices standard, and a workspace whose default geo is `us`
prices at 1.1× without the request parameter. Opus 4.5 and Sonnet 4.5 share
their price rows with Opus 4.6 / 4.7 and Sonnet 4.6; they reject
`inference_geo`, so they never report US-only inference. llmux does not price
Claude's Priority Tier, Batch or regional cloud-platform rates. A codex
request is Fast when llmux sent `service_tier: "priority"`; the response's
`service_tier` is not used, because the codex backend reports `"default"` for
priority requests (the OpenAI API documents an echo of the served tier, the
ChatGPT codex backend does not give one). llmux never requests flex or
Ultrafast.

`gpt-5.6-sol`'s rates are promotional "until November 30, 2026"; OpenAI has not
published the later price, so the row keeps the current one.

Haiku 5.5's published API limits are metadata, not a new live subscription
compatibility claim. See the [provider contract](provider-compatibility.md#haiku-55-catalog-update).

## Sources

Evidence gathered 2026-07-14; the claude rows and their aliases were re-curated
2026-07-27 and again 2026-09-23 (the `claude-opus-5-5` pair, with the floating
`opus` alias rolled onto it), the codex context windows were re-probed
2026-08-21 (the codex effort menus are unchanged from 2026-07-14), and the grok
rows were re-probed 2026-08-26 (unchanged) and again 2026-09-23 (the new
`grok-4.7` row, and the default pin moved 4.6 → 4.7 — see below).

- **Claude rows** — user-curated 2026-07-27 from the Claude Code model picker.
  The `[1m]` suffix marks the 1M-context variant ids. Effort menus are the
  Claude Code `/effort` levels (`low, medium, high, xhigh, max`), applied per
  the user contract; llmux does not itself shape claude requests. Opus 4.6 has
  no `xhigh` (corrected 2026-11-18): the Claude Code 2.1.327 binary gives its
  record the `effort` and `max_effort` capabilities but not `xhigh_effort`, and
  [model config](https://code.claude.com/docs/en/model-config) (read
  2026-11-18) lists low, medium, high, max for Opus 4.6, with `xhigh` falling
  back to `high`. The claude
  rows now live as the `CLAUDE_MODELS` const in `src/catalog.rs`, which is also
  the source for alias→id resolution in `src/provider/anthropic.rs`.
- **claude-opus-5-5** — Anthropic announcement 2026-09-22
  (`claude-opus-5-5`, 1M context, 128k max output, $4/M input, $20/M output,
  cache read $0.20/M, cache write $5/M); the Claude Code 2.1.280 binary model
  record lists `claude-opus-5-5` with `supports_1m_suffix` (and the literal
  string `claude-opus-5-5[1m]`), which is why the `[1m]` row exists.
- **claude-fable-5-2** — [Fable 5.2 model page](https://platform.claude.com/docs/en/models/fable-5-2/overview)
  and [pricing](https://platform.claude.com/docs/en/about-claude/pricing), read
  2026-11-10: `claude-fable-5-2` (dateless; the id is its own snapshot),
  released 2026-11-09, 1,000,000 context, 128K max output, $8/M input, $40/M
  output, cache read $0.20/M, cache write $10/M (5m) and $16/M (1h), no
  long-context tier. Claude Code 2.1.321 (npm, 2026-11-09) is the minimum
  client; its baked model record has `native_1m` and no `supports_1m_suffix`,
  and it resolves its own `fable` alias to `claude-fable-5-2`
  ([changelog](https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md),
  [model config](https://code.claude.com/docs/en/model-config)). That is why
  there is one `[1m]` row and no bare twin, and why `fable` moved to it.
- **Codex effort menus and base context windows** — the openai/codex model
  catalog (`models-manager/models.json`), fetched 2026-07-14. `gpt-5.6-sol` /
  `-terra` support low→ultra; `gpt-5.6-luna` low→max; `gpt-5.5` low→xhigh
  (context 272000; row removed 2026-11-19, #309). The legacy `gpt-5.5-codex` / `gpt-5-codex` ids are no longer
  curated.
- **gpt-6-astra** — same catalog re-fetched 2026-09-07: slug `gpt-6-astra`,
  display name "GPT-6-Astra", context_window 272000, max_context_window 872000,
  six reasoning levels (low, medium, high, xhigh, max, ultra), default effort
  low, minimal client version 0.153.0. The catalog lists NO `gpt-6-sol` /
  `-terra` / `-luna`. A live probe on 2026-09-07 confirmed the ChatGPT-account
  codex backend ACCEPTS `gpt-6-astra` with llmux's existing header set
  (`originator: codex_cli_rs`, no client-version header), so no header change
  was needed — only adding the slug to the provider passthrough list. Pricing:
  $10/M input, $50/M output, $1/M cached input and $12.50/M cache write (30-minute
  retention), from the [OpenAI pricing page](https://developers.openai.com/api/docs/pricing)
  (read 2026-10-06, re-checked 2026-11-11; standard tier, ≤272K input). The
  launch-era row came from third-party trackers and listed no cache-creation
  charge.
- **gpt-6.1-terra** — openai/codex
  [models.json @ caae161](https://github.com/openai/codex/blob/caae161dd0c82f2e04e57e6f9d738fafbce503ba/codex-rs/models-manager/models.json)
  (entry added in [commit 126a0ee](https://github.com/openai/codex/commit/126a0ee92ed2c5e73cd8a8f7e37563e8527552d7),
  2026-10-26), read 2026-11-10: slug `gpt-6.1-terra`, context_window 500000,
  max_context_window 872000, efforts low…ultra (default low), minimal client
  version 0.153.0. The [model page](https://developers.openai.com/api/docs/models/gpt-6.1-terra)
  and [pricing page](https://developers.openai.com/api/docs/pricing), read the
  same day: released 2026-10-26, 1,050,000 context, 128K max output, $1.00/M
  input, $0.05/M cached input, $1.25/M cache write (30-minute retention),
  $5.00/M output; above 272K input tokens the request bills at 2× input and
  cache rates and 1.5× output (priced per request since 2026-11-12, see
  [price classes](#per-request-price-classes)). Not probed through the daemon yet.
- **gpt-6.2-sol / gpt-6.2-astra** — openai/codex
  [models.json @ 6c0cee5](https://github.com/openai/codex/blob/6c0cee502d215e5df2dd9a303cf2e252229c9e12/codex-rs/models-manager/models.json)
  ([commit 6c0cee5](https://github.com/openai/codex/commit/6c0cee502d215e5df2dd9a303cf2e252229c9e12),
  "Add GPT-6.2 Sol and Astra to model catalogs", 2026-11-17), read 2026-11-18:
  context_window 500000 (sol) / 272000 (astra), max_context_window 872000,
  efforts low…ultra (default low), minimal client version 0.166.0, picker
  priority 1 and 2. The same commit raised `gpt-6.1-sol`'s context_window from
  272000 to 500000. The [gpt-6.2-sol](https://developers.openai.com/api/docs/models/gpt-6.2-sol)
  and [gpt-6.2-astra](https://developers.openai.com/api/docs/models/gpt-6.2-astra)
  model pages and the [pricing page](https://developers.openai.com/api/docs/pricing),
  read the same day: released 2026-11-17, 1,050,000 context, 128K max output;
  sol $2.00/M input, $0.10/M cached, $2.50/M 30-minute write, $5.00/M 12-hour
  write, $10.00/M output; astra $8.00 / $0.40 / $10.00 / $20.00 / $40.00; above
  272K input the request bills at 2× input and cache rates and 1.5× output;
  Fast 2×. Not probed as passthrough slugs through the daemon yet.
- **Codex `[1m]` context window** — live probes through the daemon against the
  ChatGPT-account codex backend, 2026-08-21: `gpt-5.6-sol` accepted 910,229
  input tokens and was rejected at ~936k (`Your input exceeds the context window
  of this model`); `gpt-5.6-terra` accepted 555,029. This supersedes the earlier
  "369,755 pass / ~380k rejected" note that made 372000 look probe-confirmed.
- **OpenRouter rows** — the live `GET https://openrouter.ai/api/v1/models`
  probe on 2026-08-21: 420 models, 21 of them with `pricing.prompt == "0"`; the
  curated rows take their wire slug, display name, `max_context`, and
  effort menu (`reasoning.supported_efforts`, re-sorted low→high) from that
  response. Ten rows were curated then; four were retired on 2026-11-18, when
  the same endpoint (478 models) no longer listed their slugs and
  `/api/v1/models/<slug>/endpoints` returned none
  ([#243](https://github.com/2lab-ai/llmux/issues/243)). The six that remain
  were live and free that day, context unchanged. They are the `OPENROUTER_MODELS` const in `src/catalog.rs`, which
  is also the source for `or-…` → slug resolution in
  `src/provider/openrouter.rs`. The design record, including the probe evidence
  that OpenRouter serves a NATIVE Anthropic Messages endpoint, is
  [`openrouter/spec.md`](openrouter/spec.md).
- **Grok context window / name** — the live `cli-chat-proxy` `/v1/models` probe
  2026-07-14 (`grok-4.5` ctx 500000). The `grok-4.6` row was verified the same
  way against the live `cli-chat-proxy` `/v1/models` on 2026-08-13 (ctx 500000,
  efforts `low, medium, high, xhigh`) and re-probed unchanged on 2026-08-26 with
  a real subscription token: `grok-4.6` `reasoning_efforts` `xhigh, high,
  medium, low` (upstream default `high`, ctx 500000), `grok-4.5` `high, medium,
  low` — no `xhigh` — ctx 500000. That asymmetry is why an above-`high` request
  keeps `xhigh` on `grok-4.6` and clamps to `high` on `grok-4.5`. The `grok-4.7`
  row (released 2026-09-21) was verified the same way against the live
  `cli-chat-proxy` `/v1/models` on 2026-09-23 with a real subscription token:
  `reasoning_efforts` `xhigh, high, medium, low` (upstream default `high`), ctx
  500000 — no `none`, so an effort of `none` clamps to `low`. The same response
  also carries `grok-4.7-build-fast` (identical menu and window); it is NOT
  curated, but it is in the provider thinking-level table, so it gets that
  effort menu when pinned or sent. The default pin moved `grok-4.6` → `grok-4.7`
  on 2026-09-23, and the opt-in `grok-4.7[1m]` picker twin was added 2026-09-28
  (same upstream model, same 500000, never the alias owner — see
  [The grok `[1m]` twin](#the-grok-1m-twin)). Grok effort
  menus come from the provider's per-model thinking-level table. The curated
  grok set is `grok-4.7` (the default pin, plus its `[1m]` twin), `grok-4.6`
  and `grok-4.5`; other
  known grok ids (`grok-4.3`, `grok-3-mini`, …) pass through at request time and
  synthesize a null-metadata row when pinned. Re-probed 2026-10-12 with a real
  subscription token: the same four ids and effort menus, but `context_window`
  now reads 256000 on every id next to `context_windows: [256000, 500000]` and
  `auto_compact_threshold_percent: 80`. The rows keep 500000, the upper
  `context_windows` entry; a 306,103-input-token `grok-4.7` request through the
  daemon was accepted (HTTP 200), so the 256000 default is not enforced for
  `grok-4.7`. Neither the 500000 ceiling nor `grok-4.6` / `grok-4.5` above
  256000 was re-probed.
- **Grok pricing** — docs.x.ai/developers/pricing, read 2026-09-23: `grok-4.7`
  is $2.00 in / $6.00 out / $0.50 cached input per 1M tokens (the same page now
  also LISTS grok-4.6's $0.50 cached input, which llmux had carried from
  grok-4.5). Rates double for prompts ≥200k tokens; llmux prices that
  long-context tier per request since 2026-11-12 (see
  [price classes](#per-request-price-classes)). Read 2026-11-21: "Grok 4.7
  Fast" — the `grok-4.7-build-fast` id — is $4.00 / $12.00 / $1.00 cached
  below 200k and $6.00 / $18.00 / $1.50 at or above it, and `grok-4.7-build`
  (no docs.x.ai page; the id the proxy reported serving a `grok` request) is
  priced as `grok-4.7`.
- **Gemini rows** — added 2026-10-15 as the `GEMINI_MODELS` const in
  `src/catalog.rs`. Ids, context and output: the live `fetchAvailableModels`
  read on 2026-10-15 (`gemini-3.8-flash-high`, `-medium`, `-low`; ctx
  1,048,576 / out 65,536), the
  [Gemini 3.8 Flash page](https://ai.google.dev/gemini-api/docs/models/gemini-3.8-flash)
  (updated 2026-09-02) and the
  [Gemini 4 Argon page](https://ai.google.dev/gemini-api/docs/models/gemini-4-argon)
  (updated 2026-10-14). Argon's tier: the
  [Antigravity models page](https://antigravity.google/docs/models/) (fetched
  2026-10-15). The Neon, Carbon and Hydrogen rows (2026-11-21): the
  [Neon](https://aistudio.google.com/docs/models/gemini-4-neon),
  [Carbon](https://aistudio.google.com/docs/models/gemini-4-carbon) and
  [Hydrogen](https://aistudio.google.com/docs/models/gemini-4-hydrogen) model
  pages (1,048,576 unified, thinking levels) and the
  [Antigravity models page](https://antigravity.google/docs/models) (plans,
  Neon "Up to 512K tokens", `gemini-4-neon-xhigh`), all read 2026-11-21; the
  per-model effort menus are the provider's `FAMILIES` table in
  `src/provider/gemini.rs`. Design record, including the live wire probes:
  [`gemini/spec.md`](gemini/spec.md).
- **Gemini pricing** — [ai.google.dev pricing](https://ai.google.dev/gemini-api/docs/pricing),
  updated 2026-10-14: 3.8 Flash $0.75 in / $3.75 out / $0.075 cached input per
  1M tokens through 2026-12-31, Argon $2.00 / $10.00 / $0.10 through
  2026-11-30 (both then double; re-read 2026-11-11 at
  [aistudio.google.com/docs/pricing](https://aistudio.google.com/docs/pricing),
  where ai.google.dev pricing now redirects — llmux prices each request at the
  rates of its own UTC day). Gemini 4 Carbon and Hydrogen:
  [pricing](https://aistudio.google.com/docs/pricing),
  [Gemini 4 Carbon](https://aistudio.google.com/docs/models/gemini-4-carbon) and
  [Gemini 4 Hydrogen](https://aistudio.google.com/docs/models/gemini-4-hydrogen),
  read 2026-11-18 (see the dated rates above). Gemini 4 Neon:
  [pricing](https://aistudio.google.com/docs/pricing) and
  [Gemini 4 Neon](https://aistudio.google.com/docs/models/gemini-4-neon), read
  2026-11-21. llmux uses these for cost display only;
  Antigravity subscription traffic is not billed per token.
