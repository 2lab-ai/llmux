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
  read 2026-10-06 and 2026-11-10): `sol` → `gpt-6.1-sol` (released
  2026-09-29), `terra` → `gpt-6.1-terra` (2026-10-26), `luna` → `gpt-6-luna`
  (2026-09-22) and `astra` → `gpt-6-astra`. Generation 6 shipped no terra
  tier, so `terra` stayed on `gpt-5.6-terra` until generation 6.1 shipped one.
  The bare generation ids resolve to that generation's flagship: `gpt-5.6` →
  `gpt-5.6-sol`, `gpt-6` → `gpt-6-astra`, `gpt-6.1` → `gpt-6.1-sol`. `sol`
  moved from `gpt-5.6-sol` and `luna` from `gpt-5.6-luna` on 2026-10-06, and
  `terra` from `gpt-5.6-terra` on 2026-11-10 — the 5.6 rows stay in the
  catalog and remain addressable by full id.
  `astra` and the bare `gpt-6` id advertise on `gpt-6-astra[1m]` — a deliberate
  asymmetry: on astra the bare aliases advertise the 1M row, so `astra` /
  `gpt-6` resolve to that row's upstream slug and to the ~1,050,000-token window
  this catalog publishes for it, while the explicit base id `gpt-6-astra` is the
  way to pick the openai/codex catalog's 272000. The 6.1-sol / 6.1-terra /
  6-sol / 6-luna pairs follow the 5.6 convention instead (aliases on the base row; the `[1m]`
  twin is an explicit opt-in). That is catalog resolution, **not** a client-side
  window: Claude Code sizes its readout off the id you submit, and a bare alias
  — an id it does not know — gets its 200k assumption. Type `sol[1m]` /
  `astra[1m]` (or pick the `[1M]` picker row) to move the client-side
  denominator too. These are advertised statically on the corresponding entries. The provider always
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
  per the user contract.
- **openrouter `or-` aliases** — the openrouter rows advertise the id a client
  SENDS (`or-ox-alpha`); the OpenRouter slug it is rewritten to on the wire
  (`stealth/ox-alpha`) is a different string, because the advertised id has to
  carry the `or-` prefix that routes the request to the openrouter group in the
  first place. The curated mapping:

  | request slug                | model on the wire                        |
  | --------------------------- | ---------------------------------------- |
  | `or-ox-alpha`               | `stealth/ox-alpha`                       |
  | `or-free`                   | `openrouter/free`                        |
  | `or-glm-5.2`                | `z-ai/glm-5.2:free`                      |
  | `or-nemotron-3-ultra`       | `nvidia/nemotron-3-ultra-550b-a55b:free` |
  | `or-nemotron-3.5-lightning` | `nvidia/nemotron-3.5-lightning:free`     |
  | `or-dots-3-note`            | `dots-studio/dots-3-note-preview:free`   |
  | `or-laguna-s-2.1`           | `poolside/laguna-s-2.1:free`             |
  | `or-north-mini-code`        | `cohere/north-mini-code:free`            |
  | `or-gemma-4-31b`            | `google/gemma-4-31b-it:free`             |
  | `or-gpt-oss-20b`            | `openai/gpt-oss-20b:free`                |

  Three rules sit around that table, and the table is a convenience layer, not
  a gate:

  - **bare `or`** — like bare `grok`, it resolves to the live pin
    (`config.openrouter.default_model`, default `stealth/ox-alpha`), and so
    does a request that names no model at all.
  - **`or-<vendor>/<slug>` escape hatch** — anything containing a `/` is used
    VERBATIM minus the `or-` selector, so the ~400 OpenRouter models outside
    the curated set are reachable: `or-openai/gpt-oss-20b:free` →
    `openai/gpt-oss-20b:free`. A bare `openrouter/…` slug also routes here and
    rides through unchanged.
  - **no silent substitution** — an uncurated bare name passes through as it
    was typed, so OpenRouter's own 404 reaches the user instead of llmux
    answering from a model nobody asked for.

  Matching is trimmed and case-insensitive, and one trailing `[1m]` is stripped
  first, exactly as on the claude and codex paths.
- **gemini aliases** — static, not a pin: `gemini` and `gemini-3.8-flash`
  resolve to `gemini-3.8-flash-high[1m]`, and `argon` to `gemini-4-argon[1m]`
  (the `GEMINI_MODELS` const in `src/catalog.rs`). Unlike `grok` and `or`, no
  config key moves them: `gemini.default_model` only names the model used when a
  non-gemini name (or a request that names no model) lands on the gemini group. One
  trailing `[1m]` is stripped (case-insensitively) before the request leaves
  llmux, so `-m gemini` and `-m gemini-3.8-flash-high[1m]` both reach the
  backend as `gemini-3.8-flash-high`, and `-m argon` as `gemini-4-argon`. Any
  other `gemini-*` id passes through verbatim. Claude Code is the exception:
  its request is refused locally (see [Gemini](#gemini)). The
  thinking level is part of the id, so every gemini name ignores the client's
  effort, with one exception: a bare `gemini-3.8-flash` sent by the client
  picks its variant from `output_config.effort` (`none` / `minimal` / `low` →
  `-low`, `medium` → `-medium`, anything else or no effort → `-high`). Source:
  [`gemini/spec.md`](gemini/spec.md) §R3.
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
  `gpt-5.6-terra` to `gpt-6.1-terra`. The version
  aliases `opus-5` / `sonnet-5` / `fable-5-1` and the bare generation id
  `gpt-5.6` did NOT move — they stay on their generation, because floating a version-pinned
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
| claude-opus-4-6[1m] | —            | Claude Opus 4.6     | low, medium, high, xhigh, max        | 1000000     | claude |
| claude-sonnet-5-5[1m] | sonnet, sonnet-5-5 | Claude Sonnet 5.5 [1M] | low, medium, high, xhigh, max | 1000000     | claude |
| claude-sonnet-5-5   | —            | Claude Sonnet 5.5   | low, medium, high, xhigh, max        | 200000      | claude |
| claude-sonnet-5[1m] | sonnet-5     | Claude Sonnet 5 [1M]| low, medium, high, xhigh, max        | 1000000     | claude |
| claude-sonnet-5     | —            | Claude Sonnet 5     | low, medium, high, xhigh, max        | 200000      | claude |
| claude-haiku-5-5[1m] | haiku, haiku-5-5 | Claude Haiku 5.5 [1M] | low, medium, high, xhigh, max | 1000000 | claude |
| claude-haiku-5-5 | — | Claude Haiku 5.5 | low, medium, high, xhigh, max | 1000000 | claude |
| gpt-6.1-sol[1m]     | —            | GPT-6.1-Sol [1M]    | low, medium, high, xhigh, max, ultra | 1000000     | codex  |
| gpt-6.1-sol         | sol, gpt-6.1 | GPT-6.1-Sol         | low, medium, high, xhigh, max, ultra | 272000      | codex  |
| gpt-6.1-terra[1m]   | —            | GPT-6.1-Terra [1M]  | low, medium, high, xhigh, max, ultra | 1000000     | codex  |
| gpt-6.1-terra       | terra        | GPT-6.1-Terra       | low, medium, high, xhigh, max, ultra | 500000      | codex  |
| gpt-6-astra[1m]     | astra, gpt-6 | GPT-6-Astra [1M]    | low, medium, high, xhigh, max, ultra | 1000000     | codex  |
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
| gpt-5.5             | —            | GPT-5.5             | low, medium, high, xhigh             | 272000      | codex  |
| grok-4.7            | grok (pinned)| Grok 4.7            | low, medium, high, xhigh             | 500000      | grok   |
| grok-4.7[1m]        | —            | Grok 4.7 [1M] (500k upstream) | low, medium, high, xhigh   | 500000      | grok   |
| grok-4.6            | —            | Grok 4.6            | low, medium, high, xhigh             | 500000      | grok   |
| grok-4.5            | —            | Grok 4.5            | low, medium, high                    | 500000      | grok   |
| or-ox-alpha         | or (pinned)  | Ox Alpha (free)     | low, high, max                       | 1048576     | openrouter |
| or-free             | —            | OpenRouter Free Models Router | —                          | 200000      | openrouter |
| or-glm-5.2          | —            | Z.ai GLM 5.2 (free) | high, xhigh                          | 256000      | openrouter |
| or-nemotron-3-ultra | —            | NVIDIA Nemotron 3 Ultra (free) | medium, high              | 1000000     | openrouter |
| or-nemotron-3.5-lightning | —      | NVIDIA Nemotron 3.5 Lightning (free) | —                   | 1000000     | openrouter |
| or-dots-3-note      | —            | Dots3-Note Preview (free) | —                              | 512000      | openrouter |
| or-laguna-s-2.1     | —            | Poolside Laguna S 2.1 (free) | —                           | 262144      | openrouter |
| or-north-mini-code  | —            | Cohere North Mini Code (free) | —                          | 256000      | openrouter |
| or-gemma-4-31b      | —            | Google Gemma 4 31B (free) | —                              | 262144      | openrouter |
| or-gpt-oss-20b      | —            | OpenAI gpt-oss-20b (free) | low, medium, high              | 131072      | openrouter |
| gemini-3.8-flash-high[1m] | gemini, gemini-3.8-flash | Gemini 3.8 Flash (High) [1M] | — | 1048576  | gemini |
| gemini-3.8-flash-high | —          | Gemini 3.8 Flash (High) | —                                | 1048576     | gemini |
| gemini-3.8-flash-medium[1m] | —    | Gemini 3.8 Flash (Medium) [1M] | —                         | 1048576     | gemini |
| gemini-3.8-flash-medium | —        | Gemini 3.8 Flash (Medium) | —                              | 1048576     | gemini |
| gemini-3.8-flash-low[1m] | —       | Gemini 3.8 Flash (Low) [1M] | —                            | 1048576     | gemini |
| gemini-3.8-flash-low | —           | Gemini 3.8 Flash (Low) | —                                 | 1048576     | gemini |
| gemini-4-argon[1m]  | argon        | Gemini 4 Argon [1M] | —                                    | 1048576     | gemini |
| gemini-4-argon      | —            | Gemini 4 Argon      | —                                    | 1048576     | gemini |

"grok (pinned)" means the `grok` alias appears on that row only while
`grok-4.7` is the live grok pin; any other pinned `grok-*` id takes the alias
to its own curated row, or to a synthesized row when it is out of catalog
(see [Out-of-catalog grok pin](#out-of-catalog-grok-pin)). A base-slug pin
never hands the alias to the `[1m]` twin; pinning the suffixed id does
(see [The grok `[1m]` twin](#the-grok-1m-twin)). "or (pinned)" reads
the same way for the openrouter pin (see
[Out-of-catalog openrouter pin](#out-of-catalog-openrouter-pin)); with the
default pin it sits on `or-ox-alpha`. The openrouter ids are what a client
sends — the slug that reaches OpenRouter is the one in the
[alias table](#alias-semantics), and every curated openrouter row is a free
model (priced `$0` in and out; an UNCURATED openrouter model has no known
rate and is reported unpriced, never as a free `$0`). The gemini aliases carry
no "(pinned)": they are fixed on their `[1m]` rows (see [Gemini](#gemini)).

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
twin exists only because of how Claude Code sizes its own readout: for an id it
does not know it assumes 200k, unless the id ends in `[1m]`, which it reads as a
1M window (800k usable). Measured 2026-09-28 with Claude Code 2.1.283:
`--model grok[1m]` → `/context` 143.1k/800k, `--model grok` → 200k.

That 800k readout is **wrong about the backend**: xAI still cuts the request off
at 500,000, so a session driven past 500k is rejected upstream while the client
still shows headroom. The twin is therefore an **explicit opt-in**, not the
face llmux puts on the family:

- with the default pin `grok-4.7` the `grok` alias stays on the BASE row, so
  `/llmux/models` and the picker present the conservative id as the family
  default (the client window is unaffected either way — a bare `grok` is a
  200k session in Claude Code regardless of who owns the alias, because the
  client sizes off the id you submit);
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
`gpt-5.6-sol`), and it applies to routing as well (`sol[1m]` classifies to the
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
no `gpt-5.6-luna[1m]` (luna still returns "Model not found" upstream) and no
`gpt-5.5[1m]` row (272k family).

`gpt-6-astra[1m]` advertises the same 1000000 on the strength of OpenAI's
published 1,050,000-token window for Astra — it has **not** been probed through
the daemon, unlike the 5.6 rows above (the only astra probe so far is the
2026-09-07 acceptance check, which confirms the backend takes the slug, not its
ceiling). On astra the bare aliases `astra` / `gpt-6` sit on the `[1m]` row —
opposite the 5.6 convention — so the ergonomic name resolves to the 1M row's
slug and advertised window; the explicit base id `gpt-6-astra` keeps the
openai/codex catalog's 272000 and advertises no aliases. Note this is the
CATALOG window, not Claude Code's: a bare `astra` is still a 200k session in
the client, which sizes off the submitted id — type `astra[1m]` or pick the
`[1M]` picker row for the 1M denominator. The catalog
also lists an 872,000 `max_context_window` for astra, which llmux does not
advertise.

`gpt-6.1-sol[1m]`, `gpt-6-sol[1m]` and `gpt-6-luna[1m]` (added 2026-10-06)
follow the same pattern as astra's twin — 1000000 advertised on the strength of
OpenAI's published 1,050,000 window for those models, unprobed through the
daemon — but keep the 5.6 alias convention: the bare `sol` / `luna` aliases
sit on the BASE rows (`gpt-6.1-sol`, `gpt-6-luna`, catalog context 272000),
and the `[1m]` twin is an explicit opt-in. models.json lists the same
272,000 / 872,000 pair for all three, which llmux does not advertise.

`gpt-6.1-terra[1m]` (added 2026-11-10) follows the same pattern: 1000000
advertised on the strength of OpenAI's published 1,050,000 window (128K
output), with `terra` on the base row. Its base row is the one exception to
272000: the models.json entry for `gpt-6.1-terra` lists context_window
**500000** (max_context_window 872000), so the base row advertises 500000.
Neither row has been probed through the daemon yet; a live probe follows the
preview deploy.

### Gemini

Added 2026-10-15. The eight gemini rows are four upstream models, each with a
`[1m]` twin: Gemini 3.8 Flash at three thinking levels and Gemini 4 Argon. The
thinking level is part of the id (`-high` / `-medium` / `-low`), so every row
has an empty effort menu. As on astra, and unlike
[the grok twin](#the-grok-1m-twin), the family aliases sit on the `[1m]` rows:
the backend's real window is 1,048,576 tokens, so the 1M the suffix advertises
is honest here. The suffix is display metadata only; a twin and its base row
reach the backend as the same id.

These ids are served by the **Antigravity subscription backend** (Google Cloud
Code `v1internal`, reached with an account added by `llmux login --gemini`),
not by the public Gemini API. Google decides per account which ids it may use:

- **Argon needs paid Google AI Pro, Ultra or Enterprise.** Google lists Argon
  for paid Google AI Pro plans (or a trial on eligible premium devices), Ultra
  and Enterprise, and not for Free or Google AI Plus
  ([Antigravity models](https://antigravity.google/docs/models/), fetched
  2026-11-10; the 2026-10-15 read listed Ultra and Enterprise only). On a
  Google AI Plus account `gemini-4-argon` returns 404 NOT_FOUND, and llmux
  relays that to the client as an Anthropic error with status 404. It does not
  substitute Flash. The row exists so that a Pro, Ultra or Enterprise account
  can serve it. Third-party reports say Antigravity serves Argon as
  `gemini-4-argon-high` / `-medium` / `-low` rather than the bare id llmux
  sends; llmux has not verified that ([#244](https://github.com/2lab-ai/llmux/issues/244)).
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
`-m gemini` selects `gemini-3.8-flash-high[1m]` and `-m argon` selects
`gemini-4-argon[1m]` (`llmux run --codex -- exec -m gemini '…'`). Claude Code,
including a per-agent `model:` line, gets a local 400 for a gemini model (see
[Gemini](#gemini)).

Channel note: `haiku` → `claude-haiku-5-5[1m]` is on stable since v0.2.25
(2026-10-08; [release availability](operational-reference.md#release-availability)).

## Codex model picker

`llmux run --codex` fetches `/llmux/models` and supplies a temporary Codex catalog
with backend groups, effort menus and context windows (gemini rows have an empty
effort menu: pick the `-high`, `-medium` or `-low` id instead). Native coding instructions
come from the local Codex model cache when available; the fallback is an embedded
coding prompt. Claude and other translated models use direct client tools.
`--no-model-picker`, or an explicit `-c model_catalog_json=...`, preserves your
client catalog; a fetch failure warns and does not block launch. User `-m` and
`-c` arguments override injected launch defaults. No persistent Codex settings
file is edited. See [Codex frontend](codex-frontend/README.md) for supported controls.

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
`gpt-6`, `grok`, `or-ox-alpha`, …) has no such var: it is submitted verbatim
and gets the client's 200k assumption. Catalog alias ownership decides the
UPSTREAM slug, never the client's window — bare `astra` resolves to
`gpt-6-astra[1m]`'s slug upstream and is still a 200k session in Claude Code.
To move the client-side denominator for those, the SUBMITTED id has to end in
`[1m]`: pick the `[1M]` row in the picker, or type `/model astra[1m]` /
`/model grok[1m]` (and for grok, mind the
[500k upstream ceiling](#the-grok-1m-twin)).

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
| Gemini / Google (Antigravity) | No new servable text id and no row change. Gemini 4 Carbon was announced on 2026-10-28 with no published id, for a closed cyber-defender programme only. Argon's list price doubles to $4.00 / $20.00 / $0.20 from 2026-12-01; llmux's static rate did not model that then (dated rates since 2026-11-12, [price classes](#per-request-price-classes)). The Antigravity models page now lists Argon for paid Google AI Pro (or a trial on eligible premium devices), Ultra and Enterprise, not Free or Plus (see [Gemini](#gemini)). Third-party reports, not from Google and not verified by llmux, say Antigravity serves Argon as `gemini-4-argon-high` / `-medium` / `-low` and gates it on a client User-Agent of 2.29 or later; llmux sends the bare `gemini-4-argon` with UA `antigravity/hub/2.23.1` ([#244](https://github.com/2lab-ai/llmux/issues/244)). | [Gemini API changelog](https://aistudio.google.com/docs/changelog), [models](https://aistudio.google.com/docs/models), [pricing](https://aistudio.google.com/docs/pricing), [Gemini 4 Argon](https://aistudio.google.com/docs/models/gemini-4-argon), [Antigravity models](https://antigravity.google/docs/models/), [Antigravity changelog](https://antigravity.google/docs/changelog), [Gemini 4 Carbon](https://blog.google/innovation-and-ai/models-and-research/gemini-models/introducing-gemini-4-carbon/) |

Not changed, recorded:

- **`gpt-5.5` on Codex.** It was retired from Codex for ChatGPT sign-in on 2026-10-14
  ([Codex changelog](https://developers.openai.com/codex/changelog)), and llmux uses the
  ChatGPT-account backend. The row and its passthrough stay; whether it still answers is
  untested.
- **`gpt-6.1-sol` context.** The live Codex server list (`~/.codex/models_cache.json`, fetched
  2026-11-09, Codex CLI 0.167.0) and OpenAI's docs give it 500000 in Codex, while `models.json`
  still says 272000. The row keeps the `models.json` value.
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
  ([#243](https://github.com/2lab-ai/llmux/issues/243)).

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
its own [price class](#per-request-price-classes): Haiku 5.5's >100K tier is
applied to a request whose own prompt is over 100,000 tokens, never to a sum.
Writes a response does not report as 1-hour take the 5-minute rate and those it
does the 1-hour rate (`usage.cache_creation.ephemeral_1h_input_tokens`, since
2026-11-11; earlier history has no split and keeps the 5-minute price). Each
request's server web searches add $10 per 1,000. Claude Fast mode and data
residency premiums are not applied (llmux records neither). Aggregates keep the
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
tier, ≤272K input), read 2026-11-11. Writes are a part of input; the 12-hour
part is the one a response reports in
`input_tokens_details.cache_write_tokens_details.ttl_12h_tokens`.

| Model | Input | Cache write, 30m | Cache write, 12h |
| --- | --- | --- | --- |
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
attempt) is classified on its own prompt and model.

| Fact | When | Rates |
| --- | --- | --- |
| Long context | The prompt (input + cache reads + cache writes) is over 272,000 tokens on a listed GPT row, 200,000 or more on `grok-4.5` / `grok-4.6` / `grok-4.7` (and the grok fallback), over 100,000 on `claude-haiku-5-5`. Built-in data, not configurable. | The whole request at the long rates below. |
| Fast | Codex Fast mode (`service_tier: "priority"`) was in effect. | The Fast rates below. |
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
| grok-4.6, grok-4.7 | ≥ 200,000 | $4 / $1.00 / $4 (input rate) / — / $12 | — | — |
| claude-haiku-5-5 | > 100,000 | $0.50 / $0.05 / $0.625 / $1.00 / $2.50 | — | — |

Dated rate changes ([Google pricing](https://aistudio.google.com/docs/pricing),
read 2026-11-11): `gemini-4-argon` (and `argon`, `gemini-4-argon-*`) $2.00 /
$10.00 / $0.10 through 2026-11-30, $4.00 / $20.00 / $0.20 from 2026-12-01;
Gemini 3.8 Flash (every thinking variant, and the gemini fallback) $0.75 /
$3.75 / $0.075 through 2026-12-31, $1.50 / $7.50 / $0.15 from 2027-01-01.

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
  the user contract; llmux does not itself shape claude requests. The claude
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
  (context 272000). The legacy `gpt-5.5-codex` / `gpt-5-codex` ids are no longer
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
- **Codex `[1m]` context window** — live probes through the daemon against the
  ChatGPT-account codex backend, 2026-08-21: `gpt-5.6-sol` accepted 910,229
  input tokens and was rejected at ~936k (`Your input exceeds the context window
  of this model`); `gpt-5.6-terra` accepted 555,029. This supersedes the earlier
  "369,755 pass / ~380k rejected" note that made 372000 look probe-confirmed.
- **OpenRouter rows** — the live `GET https://openrouter.ai/api/v1/models`
  probe on 2026-08-21: 420 models, 21 of them with `pricing.prompt == "0"`; the
  ten curated rows take their wire slug, display name, `max_context`, and
  effort menu (`reasoning.supported_efforts`, re-sorted low→high) from that
  response. They are the `OPENROUTER_MODELS` const in `src/catalog.rs`, which
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
  [price classes](#per-request-price-classes)).
- **Gemini rows** — added 2026-10-15 as the `GEMINI_MODELS` const in
  `src/catalog.rs`. Ids, context and output: the live `fetchAvailableModels`
  read on 2026-10-15 (`gemini-3.8-flash-high`, `-medium`, `-low`; ctx
  1,048,576 / out 65,536), the
  [Gemini 3.8 Flash page](https://ai.google.dev/gemini-api/docs/models/gemini-3.8-flash)
  (updated 2026-09-02) and the
  [Gemini 4 Argon page](https://ai.google.dev/gemini-api/docs/models/gemini-4-argon)
  (updated 2026-10-14). Argon's tier: the
  [Antigravity models page](https://antigravity.google/docs/models/) (fetched
  2026-10-15). Design record, including the live wire probes:
  [`gemini/spec.md`](gemini/spec.md).
- **Gemini pricing** — [ai.google.dev pricing](https://ai.google.dev/gemini-api/docs/pricing),
  updated 2026-10-14: 3.8 Flash $0.75 in / $3.75 out / $0.075 cached input per
  1M tokens through 2026-12-31, Argon $2.00 / $10.00 / $0.10 through
  2026-11-30 (both then double; re-read 2026-11-11 at
  [aistudio.google.com/docs/pricing](https://aistudio.google.com/docs/pricing),
  where ai.google.dev pricing now redirects — llmux prices each request at the
  rates of its own UTC day). llmux uses these for cost display only;
  Antigravity subscription traffic is not billed per token.
