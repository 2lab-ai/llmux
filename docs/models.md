# Model catalog

llmux exposes the **known** models — a curated set plus the live grok and
openrouter pins — as a machine-readable catalog. This is deliberately not an
exhaustive list of everything routable: at request time the grok provider
forwards ANY `grok-*` id verbatim and the openrouter provider forwards any
`or-<vendor>/<slug>` verbatim (see [alias semantics](#alias-semantics)), so a
request or config pin naming an id outside the curated set still works. Such an
out-of-catalog pin appears here as a **synthesized row** with null metadata (see
[out-of-catalog grok pin](#out-of-catalog-grok-pin) and
[out-of-catalog openrouter pin](#out-of-catalog-openrouter-pin)).

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
| `group`       | string              | Backend group: `claude`, `codex`, `grok`, or `openrouter`.    |

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
  read 2026-10-06): `sol` → `gpt-6.1-sol` (released 2026-09-29), `luna` →
  `gpt-6-luna` (2026-09-22), `astra` → `gpt-6-astra`, and `terra` →
  `gpt-5.6-terra` (generation 6 shipped no terra tier, so moving it would
  resolve to a slug that 404s upstream). The bare generation ids resolve to
  that generation's flagship: `gpt-5.6` → `gpt-5.6-sol`, `gpt-6` →
  `gpt-6-astra`, `gpt-6.1` → `gpt-6.1-sol`. `sol` moved from `gpt-5.6-sol` and
  `luna` from `gpt-5.6-luna` on 2026-10-06 — the 5.6 rows stay in the catalog
  and remain addressable by full id.
  `astra` and the bare `gpt-6` id advertise on `gpt-6-astra[1m]` — a deliberate
  asymmetry: on astra the bare aliases advertise the 1M row, so `astra` /
  `gpt-6` resolve to that row's upstream slug and to the ~1,050,000-token window
  this catalog publishes for it, while the explicit base id `gpt-6-astra` is the
  way to pick the openai/codex catalog's 272000. The 6.1-sol / 6-sol / 6-luna
  pairs follow the 5.6 convention instead (aliases on the base row; the `[1m]`
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
  | `fable`, `fable-5-1` | `claude-fable-5-1[1m]` | `claude-fable-5-1` |
  | `opus`, `opus-5-5` | `claude-opus-5-5[1m]` | `claude-opus-5-5` |
  | `opus-5`         | `claude-opus-5[1m]`   | `claude-opus-5`    |
  | `sonnet`, `sonnet-5-5` | `claude-sonnet-5-5[1m]` | `claude-sonnet-5-5` |
  | `sonnet-5`       | `claude-sonnet-5[1m]` | `claude-sonnet-5`  |
  | `haiku`, `haiku-5-5` | `claude-haiku-5-5[1m]` | `claude-haiku-5-5` |

  Matching is trimmed and case-insensitive (`"  OPUS  "` resolves), and an
  alias may carry the client-side `[1m]` context suffix — `fable[1m]` resolves
  exactly like `fable`, because alias resolution runs before the suffix strip.
  The strip is syntactic; the resolved model's context limit still applies.
  `haiku[1m]` now resolves to `claude-haiku-5-5[1m]` (1M published context).
  Only aliases are rewritten: a real catalog id is not an alias and passes
  through untouched — the `[1m]` suffix strip is a separate, subsequent step,
  which is why `claude-opus-5[1m]` still reaches upstream as `claude-opus-5` —
  and foreign slugs (`grok-4.6`, `gpt-5.6-sol`) are never rewritten. The
  mapping has a single source, the `CLAUDE_MODELS` const in `src/catalog.rs` behind
  `resolve_claude_alias`; adding a curated row carries its aliases
  automatically. llmux still does not otherwise *shape* claude requests, and the
  `efforts` menu on claude rows is the Claude Code `/effort` level list, per the
  user contract.
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
- **alias stability** — aliases float to the current generation, ids do not.
  `opus` tracks the newest curated Opus and moved from `claude-opus-4-8[1m]` to
  `claude-opus-5[1m]` on 2026-07-27 (4.8 stays in the catalog; it just no longer
  owns an alias), then from `claude-opus-5[1m]` to `claude-opus-5-5[1m]` on
  2026-09-23. `sonnet` moved from `claude-sonnet-5[1m]` to
  `claude-sonnet-5-5[1m]` on 2026-10-06 (Sonnet 5.5 released 2026-09-28), and
  on the same day the codex `sol` alias moved from `gpt-5.6-sol` to
  `gpt-6.1-sol` and `luna` from `gpt-5.6-luna` to `gpt-6-luna`. The version
  aliases `opus-5` / `sonnet-5` and the bare generation id `gpt-5.6` did NOT
  move — they stay on their generation, because floating a version-pinned
  alias onto a new model would be silent substitution. Anyone who needs one specific model must send
  its full catalog id — that is the stable handle. On 2026-10-08, `haiku`
  moved from `claude-haiku-4-5` to `claude-haiku-5-5[1m]`; the explicit 4.5 id
  remains available. Usage and pricing are booked against the
  resolved id, not the alias, so alias traffic lands on the same row as id
  traffic.

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
| claude-fable-5-1[1m] | fable, fable-5-1 | Claude Fable 5.1 | low, medium, high, xhigh, max        | 1000000     | claude |
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
| claude-haiku-4-5    | —            | Claude Haiku 4.5    | low, medium, high, xhigh, max        | 200000      | claude |
| gpt-6.1-sol[1m]     | —            | GPT-6.1-Sol [1M]    | low, medium, high, xhigh, max, ultra | 1000000     | codex  |
| gpt-6.1-sol         | sol, gpt-6.1 | GPT-6.1-Sol         | low, medium, high, xhigh, max, ultra | 272000      | codex  |
| gpt-6-astra[1m]     | astra, gpt-6 | GPT-6-Astra [1M]    | low, medium, high, xhigh, max, ultra | 1000000     | codex  |
| gpt-6-astra         | —            | GPT-6-Astra         | low, medium, high, xhigh, max, ultra | 272000      | codex  |
| gpt-6-sol[1m]       | —            | GPT-6-Sol [1M]      | low, medium, high, xhigh, max, ultra | 1000000     | codex  |
| gpt-6-sol           | —            | GPT-6-Sol           | low, medium, high, xhigh, max, ultra | 272000      | codex  |
| gpt-6-luna[1m]      | —            | GPT-6-Luna [1M]     | low, medium, high, xhigh, max        | 1000000     | codex  |
| gpt-6-luna          | luna         | GPT-6-Luna          | low, medium, high, xhigh, max        | 272000      | codex  |
| gpt-5.6-sol[1m]     | —            | GPT-5.6-Sol [1M]    | low, medium, high, xhigh, max, ultra | 1000000     | codex  |
| gpt-5.6-sol         | gpt-5.6      | GPT-5.6-Sol         | low, medium, high, xhigh, max, ultra | 272000      | codex  |
| gpt-5.6-terra[1m]   | —            | GPT-5.6-Terra [1M]  | low, medium, high, xhigh, max, ultra | 1000000     | codex  |
| gpt-5.6-terra       | terra        | GPT-5.6-Terra       | low, medium, high, xhigh, max, ultra | 272000      | codex  |
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
rate and is reported unpriced, never as a free `$0`).

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

## Selecting a model

There are three ways to select a catalog model:

- **Claude Code**: `/model <id>` (the [picker below](#claude-code-model-picker)
  lists this catalog), or `--model <id>` at launch.
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

Channel note: `haiku` → `claude-haiku-5-5[1m]` is on stable since v0.2.25
(2026-10-08; [release availability](operational-reference.md#release-availability)).

## Codex model picker

`llmux run --codex` fetches `/llmux/models` and supplies a temporary Codex catalog
with backend groups, effort menus and context windows. Native coding instructions
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

One row per catalog entry, in catalog order:

- `model` — the entry `id` **verbatim**, including a `[1m]` suffix (Claude Code
  takes the string as-is, and the providers strip the suffix upstream). It is
  the string llmux routes on, so selecting a row equals typing `/model <id>`.
- `label` — the entry `name`.
- `description` — `<group> · efforts <first…last> · ctx <max_context>`, dropping
  the efforts part for an empty menu and the ctx part for an unpublished window.
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
| `ANTHROPIC_DEFAULT_FABLE_MODEL`  | `fable` owner (`claude-fable-5-1[1m]`)  |
| `ANTHROPIC_DEFAULT_SONNET_MODEL` | `sonnet` owner (`claude-sonnet-5-5[1m]`)  |
| `ANTHROPIC_DEFAULT_HAIKU_MODEL`  | `haiku` owner (`claude-haiku-5-5[1m]`)      |

The values are DERIVED from the catalog alias owners at launch, never
hardcoded: re-curating an alias onto a new row moves the export with it, and an
alias no row owns is not exported at all (that family keeps Claude Code's own
default). Measured effect: `ANTHROPIC_DEFAULT_OPUS_MODEL=claude-opus-5-5[1m]`
→ status line "Claude Opus 5.5 [1M]" and `/context` 221.2k/800k, versus 200k
without it. Only the `_MODEL` variants are set — Claude Code's `_NAME`,
`_DESCRIPTION` and `_SUPPORTED_CAPABILITIES` variants are left alone.

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
and [Haiku 5.5 specifications](https://platform.claude.com/docs/en/models/haiku-5-5/overview):

| Model / prompt length | Input | Output | Cache read | Cache write, 5m | Cache write, 1h |
| --- | --- | --- | --- | --- | --- |
| Haiku 5.5, ≤100,000 tokens | $0.10 | $0.50 | $0.01 | $0.125 | $0.20 |
| Haiku 5.5, >100,000 tokens | $0.50 | $2.50 | $0.05 | $0.625 | $1.00 |
| Sonnet 5.5 | $2.00 | $10.00 | $0.10 | $2.50 | $4.00 |
| Fable 5.1 | $10.00 | $50.00 | $0.25 | $12.50 | $20.00 |

Sonnet 5.5 cache reads were reduced from $0.20 to $0.10 on
[2026-10-07](https://www.anthropic.com/claude-haiku-5-5). Fable 5.1 now has its
own $0.25 cache-read reference rate. Explicit older Sonnet 5, Fable 5 and
Haiku 4.5 keep their own prices.

**Estimator scope:** `src/pricing.rs` uses static reference rates: Haiku 5.5's
≤100K tier and 5-minute cache writes. It does **not** apply Haiku's >100K tier
(5× all rates), 1-hour write rates, service-tier premiums or historical prices.
Aggregated counters do not retain the request-length/TTL detail needed to
reconstruct these charges; crossing 100K in accumulated monthly usage does not
mean any individual request crossed it. Displayed costs can therefore understate
long-context/1h-cache API cost. These are API-equivalent estimates, never a
subscription invoice. Existing config price overrides still take precedence.

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
- **Claude pricing** — the Anthropic pricing page
  (platform.claude.com/docs/en/about-claude/pricing), read 2026-09-28, per 1M
  tokens as input / 5-minute cache write / 1-hour cache write / cache read /
  output: Fable 5.1 $10 / $12.50 / $20 / $0.25 / $50; Fable 5 $10 / $12.50 /
  $20 / $1.00 / $50; Opus 5.5 $4 / $5 / $8 / $0.20 / $20; Opus 5, 4.8, 4.7,
  4.6, 4.5 $5 / $6.25 / $10 / $0.50 / $25; Opus 4.1 and 4 (retired) $15 /
  $18.75 / $30 / $1.50 / $75; Sonnet 5.5 and 5 $2 / $2.50 / $4 / $0.20 / $10;
  Sonnet 4.6, 4.5, 4 $3 / $3.75 / $6 / $0.30 / $15; Haiku 4.5 $1 / $1.25 / $2 /
  $0.10 / $5; Haiku 3.5 (retired, `claude-3-5-haiku-*`) $0.80 / $1 / $1.60 /
  $0.08 / $4. Both write rates are modeled: each request's 1-hour share of its
  cache writes (Anthropic's `cache_creation.ephemeral_1h_input_tokens`) is
  billed at the 1-hour rate and the rest at the 5-minute rate; a request that
  reports no split is billed entirely at the 5-minute rate. Dated snapshots
  price like their bare id: llmux parses the
  version out of the id, so `claude-opus-4-20250514` is Opus 4 while
  `claude-opus-4-5-20251101` is Opus 4.5. An unlisted version of a known family
  takes that family's default row (Opus 5, Sonnet 4.x, Haiku 4.5, Fable 5);
  any other claude id, including other Claude 3.x models, takes the Opus 5
  rates. The Mythos models are not in the catalog and have no row.
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
  was needed — only adding the slug to the provider passthrough list. Pricing
  ($10/M input, $50/M output, $1/M cached input, no cache-creation charge)
  comes from third-party pricing trackers for the 2026-09 launch standard tier,
  not from an OpenAI page read directly.
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
- **Grok pricing** — docs.x.ai/developers/pricing, read 2026-09-28: `grok-4.7`
  and `grok-4.6` are $2.00 in / $6.00 out / $0.50 cached input per 1M tokens,
  `grok-4.5` $2.00 / $6.00 / $0.30. A request whose prompt (fresh input + cached
  input + cache writes) is **≥ 200,000 tokens** is billed ALL of its tokens at
  the long-context rates: $4.00 / $12.00 / $1.00 cached (grok-4.5: $0.60
  cached). llmux models that tier per request, and every aggregate (model rows,
  Usage tab, keys panel) keeps long-context requests separate so its cost is
  the sum of its requests' costs. Unknown grok ids take the grok-4.5 row, tier
  included. OpenAI models carry the same kind of tier at a **272,000**-token
  prompt: gpt-6-astra $10 / $50 / $1 cached becomes $20 / $75 / $2, gpt-6-sol
  $2 / $10 / $0.20 becomes $4 / $15 / $0.40, gpt-6-luna $0.10 / $0.50 / $0.01
  becomes $0.20 / $0.75 / $0.02, gpt-5.6-sol $4 / $20 / $0.40 becomes $8 / $30 /
  $0.80, gpt-5.6-terra $2 / $12 / $0.20 becomes $4 / $18 / $0.40, gpt-5.6-luna
  $0.20 / $1.20 / $0.02 becomes $0.40 / $1.80 / $0.04, gpt-5.5 $5 / $30 / $0.50
  becomes $10 / $45 / $1 (OpenAI pricing page, read 2026-09-28; the whole request
  reprices; gpt-6.1-sol $2 / $10 / $0.10 becomes $4 / $15 / $0.20). Every gpt-5.6 / gpt-6 / gpt-6.1 row also carries OpenAI's cache-write rate (1.25× input; 2× that in the long tier), e.g. gpt-6-sol $2.50 / $5.00. `gpt-5.5-codex` and unknown codex ids are not on that page, so they
  stay flat. Claude rows carry no tier: Claude 4.6 and later bill the full 1M
  window at standard rates. These are API-list-price equivalents; codex traffic
  is subscription-billed.
