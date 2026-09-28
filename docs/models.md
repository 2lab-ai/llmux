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

Both return the **same** payload and sit behind the same loopback + proxy
api-key gate as every other route:

```json
{ "models": [ /* ModelEntry, ... */ ] }
```

Registering root `/models` reserves a path that previously fell through to the
upstream proxy fallback. Anthropic exposes no root `/models`, and `/v1/models`
is left untouched (still proxied upstream), so nothing regresses.

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
- **codex variant aliases** — `sol` / `terra` / `luna` resolve to the latest gpt
  generation of that variant (`gpt-5.6-sol` / `-terra` / `-luna`), and the bare
  `gpt-5.6` id resolves to the `sol` flagship. `astra` and the bare `gpt-6` id
  resolve to `gpt-6-astra[1m]` — deliberate asymmetry with the 5.6 rows: on
  astra the bare aliases advertise the 1M row, so `astra` / `gpt-6` resolve to
  that row's upstream slug and to the ~1,050,000-token window this catalog
  publishes for it, while the explicit base id `gpt-6-astra` is the way to pick
  the openai/codex catalog's 272000. That is catalog resolution, **not** a
  client-side window: Claude Code sizes its readout off the id you submit, and
  a bare `astra` / `gpt-6` — an id it does not know — gets its 200k assumption.
  Type `astra[1m]` (or pick the `[1M]` picker row) to move the client-side
  denominator too.
  Generation 6 shipped a SINGLE tier, so `sol` / `terra` / `luna` stay on 5.6
  (there is no `gpt-6-sol` and a request for one would 404 upstream). These are
  advertised statically on the corresponding entries. The provider always
  strips a trailing `[1m]` before the request leaves llmux
  (`CLIENT_CONTEXT_SUFFIX` in `src/provider/codex.rs`, mirrored in
  `src/provider/grok.rs`), so bare and suffixed aliases reach the backend as the
  same upstream slug.
- **claude aliases** — the claude rows carry short user-curated aliases that
  both ROUTE to the claude group and are RESOLVED by the proxy: a bare alias is
  rewritten to its catalog id before the request leaves llmux, so the alias
  `GET /models` advertises is actually honored upstream.

  | request slug     | catalog id            | model on the wire  |
  | ---------------- | --------------------- | ------------------ |
  | `fable`, `fable-5-1` | `claude-fable-5-1[1m]` | `claude-fable-5-1` |
  | `opus`, `opus-5-5` | `claude-opus-5-5[1m]` | `claude-opus-5-5` |
  | `opus-5`         | `claude-opus-5[1m]`   | `claude-opus-5`    |
  | `sonnet`, `sonnet-5` | `claude-sonnet-5[1m]` | `claude-sonnet-5` |
  | `haiku`          | `claude-haiku-4-5`    | `claude-haiku-4-5` |

  Matching is trimmed and case-insensitive (`"  OPUS  "` resolves), and an
  alias may carry the client-side `[1m]` context suffix — `fable[1m]` resolves
  exactly like `fable`, because alias resolution runs before the suffix strip.
  That strip is syntactic only — it does not promise a 1M-capable target
  (`haiku[1m]` resolves to the ordinary `claude-haiku-4-5` row).
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
  2026-09-23. The version alias `opus-5` did NOT move — it stays on
  `claude-opus-5[1m]`, because floating a version-pinned alias onto a new model
  would be silent substitution. Anyone who needs one specific model must send
  its full catalog id — that is the stable handle. Usage and pricing are booked against the
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
| claude-sonnet-5[1m] | sonnet, sonnet-5 | Claude Sonnet 5 [1M]| low, medium, high, xhigh, max        | 1000000     | claude |
| claude-sonnet-5     | —            | Claude Sonnet 5     | low, medium, high, xhigh, max        | 200000      | claude |
| claude-haiku-4-5    | haiku        | Claude Haiku 4.5    | low, medium, high, xhigh, max        | 200000      | claude |
| gpt-6-astra[1m]     | astra, gpt-6 | GPT-6-Astra [1M]    | low, medium, high, xhigh, max, ultra | 1000000     | codex  |
| gpt-6-astra         | —            | GPT-6-Astra         | low, medium, high, xhigh, max, ultra | 272000      | codex  |
| gpt-5.6-sol[1m]     | —            | GPT-5.6-Sol [1M]    | low, medium, high, xhigh, max, ultra | 1000000     | codex  |
| gpt-5.6-sol         | sol, gpt-5.6 | GPT-5.6-Sol         | low, medium, high, xhigh, max, ultra | 372000      | codex  |
| gpt-5.6-terra[1m]   | —            | GPT-5.6-Terra [1M]  | low, medium, high, xhigh, max, ultra | 1000000     | codex  |
| gpt-5.6-terra       | terra        | GPT-5.6-Terra       | low, medium, high, xhigh, max, ultra | 372000      | codex  |
| gpt-5.6-luna        | luna         | GPT-5.6-Luna        | low, medium, high, xhigh, max        | 372000      | codex  |
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
advertise **500000**, xAI's real window from the live `/v1/models` probe. The
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
catalog's 372000 — the window a client gets without opting in — exactly as the
claude base rows keep 200000 next to their `[1m]` twins. There is deliberately
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
header, both of which defeat the purpose here. `/v1/models` stays proxied
upstream, untouched.

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
| `ANTHROPIC_DEFAULT_SONNET_MODEL` | `sonnet` owner (`claude-sonnet-5[1m]`)  |
| `ANTHROPIC_DEFAULT_HAIKU_MODEL`  | `haiku` owner (`claude-haiku-4-5`)      |

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
  synthesize a null-metadata row when pinned.
- **Grok pricing** — docs.x.ai/developers/pricing, read 2026-09-23: `grok-4.7`
  is $2.00 in / $6.00 out / $0.50 cached input per 1M tokens (the same page now
  also LISTS grok-4.6's $0.50 cached input, which llmux had carried from
  grok-4.5). Rates double for prompts ≥200k tokens; llmux does not model that
  long-context tier.
