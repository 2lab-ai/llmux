# Model catalog

llmux exposes the **known** models — a curated set plus the live grok pin — as
a machine-readable catalog. This is deliberately not an exhaustive list of
everything routable: at request time the grok provider forwards ANY `grok-*` id
verbatim (see [alias semantics](#alias-semantics)), so a request or config pin
naming an id outside the curated set still works. Such an out-of-catalog pin
appears here as a **synthesized row** with null metadata (see
[out-of-catalog pin](#out-of-catalog-grok-pin)).

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
| `group`       | string              | Backend group: `claude`, `codex`, or `grok`.                  |

`max_context: null` means the context window is not published for that id —
not that it is zero.

## Alias semantics

- **grok family alias** — `"grok"` is dynamic: it attaches to whichever grok id
  is the current live pin (`POST /llmux/grok` / `config.grok.default_model`). A
  bare `grok` request routes to that pin, so the catalog advertises the alias on
  exactly the pinned entry. Any `grok-*` id also passes through verbatim.
- **codex variant aliases** — `sol` / `terra` / `luna` resolve to the latest gpt
  generation of that variant (`gpt-5.6-sol` / `-terra` / `-luna`), and the bare
  `gpt-5.6` id resolves to the `sol` flagship. These are advertised statically
  on the corresponding entries.
- **claude aliases** — the claude rows carry short user-curated aliases that
  both ROUTE to the claude group and are RESOLVED by the proxy: a bare alias is
  rewritten to its catalog id before the request leaves llmux, so the alias
  `GET /models` advertises is actually honored upstream.

  | request slug     | catalog id            | model on the wire  |
  | ---------------- | --------------------- | ------------------ |
  | `fable`          | `claude-fable-5[1m]`  | `claude-fable-5`   |
  | `opus`, `opus-5` | `claude-opus-5[1m]`   | `claude-opus-5`    |
  | `sonnet`, `sonnet-5` | `claude-sonnet-5[1m]` | `claude-sonnet-5` |
  | `haiku`          | `claude-haiku-4-5`    | `claude-haiku-4-5` |

  Matching is trimmed and case-insensitive (`"  OPUS  "` resolves). Only aliases
  are rewritten: a real catalog id is not an alias and passes through untouched
  — the `[1m]` suffix strip is a separate, subsequent step, which is why
  `claude-opus-5[1m]` still reaches upstream as `claude-opus-5` — and foreign
  slugs (`grok-4.5`, `gpt-5.6-sol`) are never rewritten. The mapping has a
  single source, the `CLAUDE_MODELS` const in `src/catalog.rs` behind
  `resolve_claude_alias`; adding a curated row carries its aliases
  automatically. llmux still does not otherwise *shape* claude requests, and the
  `efforts` menu on claude rows is the Claude Code `/effort` level list, per the
  user contract.
- **alias stability** — aliases float to the current generation, ids do not.
  `opus` tracks the newest curated Opus and moved from `claude-opus-4-8[1m]` to
  `claude-opus-5[1m]` on 2026-07-27 (4.8 stays in the catalog; it just no longer
  owns an alias). Anyone who needs one specific model must send its full catalog
  id — that is the stable handle. Usage and pricing are booked against the
  resolved id, not the alias, so alias traffic lands on the same row as id
  traffic.

### Out-of-catalog grok pin

The curated grok set is just `grok-4.5`. `config.grok.default_model` may pin ANY
`grok-*` slug — including ids not in the curated table below (e.g.
`grok-4.3`, `grok-code-fast-1`). Because the provider forwards such ids
verbatim, the pin is real and routable, so the `"grok"` family alias must have
an owner. When the pin matches no curated id, the catalog appends a
**synthesized** grok row: `id` = the pin, `name` = the pin verbatim, `aliases`
= `["grok"]`, `efforts` from the thinking-level lookup (empty unless the id is a
known reasoner — e.g. pinning `grok-4.3` yields `none, low, medium, high`), and
`max_context` = `null`. The null metadata reflects that llmux has no published
context/name for an id it does not curate.

## Current catalog

| id                  | aliases      | name                | efforts                              | max_context | group  |
| ------------------- | ------------ | ------------------- | ------------------------------------ | ----------- | ------ |
| claude-fable-5[1m]  | fable        | Claude Fable 5      | low, medium, high, xhigh, max        | 1000000     | claude |
| claude-opus-5[1m]   | opus, opus-5 | Claude Opus 5 [1M]  | low, medium, high, xhigh, max        | 1000000     | claude |
| claude-opus-5       | —            | Claude Opus 5       | low, medium, high, xhigh, max        | 200000      | claude |
| claude-opus-4-8[1m] | —            | Claude Opus 4.8     | low, medium, high, xhigh, max        | 1000000     | claude |
| claude-opus-4-6[1m] | —            | Claude Opus 4.6     | low, medium, high, xhigh, max        | 1000000     | claude |
| claude-sonnet-5[1m] | sonnet, sonnet-5 | Claude Sonnet 5 [1M]| low, medium, high, xhigh, max        | 1000000     | claude |
| claude-sonnet-5     | —            | Claude Sonnet 5     | low, medium, high, xhigh, max        | 200000      | claude |
| claude-haiku-4-5    | haiku        | Claude Haiku 4.5    | low, medium, high, xhigh, max        | 200000      | claude |
| gpt-5.6-sol         | sol, gpt-5.6 | GPT-5.6-Sol         | low, medium, high, xhigh, max, ultra | 372000      | codex  |
| gpt-5.6-terra       | terra        | GPT-5.6-Terra       | low, medium, high, xhigh, max, ultra | 372000      | codex  |
| gpt-5.6-luna        | luna         | GPT-5.6-Luna        | low, medium, high, xhigh, max        | 372000      | codex  |
| gpt-5.5             | —            | GPT-5.5             | low, medium, high, xhigh             | 272000      | codex  |
| grok-4.5            | grok (pinned)| Grok 4.5            | low, medium, high                    | 500000      | grok   |

"grok (pinned)" means the `grok` alias appears on that row only while it is the
live grok pin; any other pinned `grok-*` id appears instead as a synthesized row
(see [Out-of-catalog grok pin](#out-of-catalog-grok-pin)).

## Sources

Evidence gathered 2026-07-14; the claude rows and their aliases were re-curated
2026-07-27. The codex and grok evidence below is unchanged from 2026-07-14.

- **Claude rows** — user-curated 2026-07-27 from the Claude Code model picker.
  The `[1m]` suffix marks the 1M-context variant ids. Effort menus are the
  Claude Code `/effort` levels (`low, medium, high, xhigh, max`), applied per
  the user contract; llmux does not itself shape claude requests. The claude
  rows now live as the `CLAUDE_MODELS` const in `src/catalog.rs`, which is also
  the source for alias→id resolution in `src/provider/anthropic.rs`.
- **Codex context windows and effort menus** — the openai/codex model catalog
  (`models-manager/models.json`), fetched 2026-07-14. `gpt-5.6-sol` / `-terra`
  support low→ultra; `gpt-5.6-luna` low→max; `gpt-5.5` low→xhigh (context
  272000). The legacy `gpt-5.5-codex` / `gpt-5-codex` ids are no longer curated.
- **Grok context window / name** — the live `cli-chat-proxy` `/v1/models` probe
  2026-07-14 (`grok-4.5` ctx 500000). Grok effort menus come from the provider's
  per-model thinking-level table. The curated grok set is just `grok-4.5`; other
  known grok ids (`grok-4.3`, `grok-3-mini`, …) pass through at request time and
  synthesize a null-metadata row when pinned.
