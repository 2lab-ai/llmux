# llmux decision archive

This directory explains the product contract, architecture, research, and the
decisions behind shipped behavior. It is organized here by lifecycle rather
than filename order because several records are dated snapshots or completed
convergence logs.

For installation and operation, use the [documentation map](../docs/README.md).
For current code behavior, the maintained contracts below and each owning user
guide take precedence over historical plans.

## Lifecycle labels

| Label | Meaning |
| --- | --- |
| Maintained contract | Updated when the product/system boundary changes. |
| Shipped decision | Explains a design that is in production; details may be dated and link to the maintained contract. |
| Evidence | A dated observation or capture. Provenance and timestamp are part of the claim. |
| Research | Input to a decision, not a user-facing promise. |
| Historical input | Superseded design retained to explain lineage. |
| Closed convergence log | Work-unit/acceptance evidence from a completed project; not a backlog. |

## Maintained contracts

| Document | Scope |
| --- | --- |
| [01 — Product specification](01-spec.md) | Shipped product boundary, providers, routing, scheduling, clients, security, and distribution. |
| [02 — Architecture](02-architecture.md) | Current source layout, runtime topology, request/control flows, concurrency, persistence, and native-client boundary. |

These two paths remain stable because contributor automation loads them.

## Shipped decisions and behavior records

| Record | Lifecycle | What it explains |
| --- | --- | --- |
| [05 — Dashboard overhaul findings](05-findings-dashboard-overhaul.md) | Evidence | Dated routing, Codex priority, usage-scaling, and superseded client-context findings from the dashboard work. |
| [06 — Scheduler as-built snapshot](06-scheduler-current.md) | Dated shipped snapshot | Pre-redesign scheduler state on 2026-06-14; current policy is summarized in 09 and the scheduler guide. |
| [08 — TUI animations](08-tui-animations.md) | Shipped decision | Motion vocabulary and terminal rendering constraints. |
| [09 — Scheduler perishability](09-scheduler-perishability.md) | Shipped decision | Why reset proximity changes account value and how stickiness is preserved. |
| [10 — Model usage dashboard](10-model-usage-dashboard.md) | Shipped decision | Dashboard data model and analytics acceptance; planning prose is preserved as history. |
| [13 — Remote CLI](13-remote-cli.md) | Shipped decision | One-daemon/many-client command semantics and refusal boundary. |
| [13 — Raw usage sources](13-usage-raw-sources.md) | Evidence | Verbatim provider usage inputs. The duplicate number is historical; filenames disambiguate. |
| [14 — Calendar usage and cost](14-usage-calendar-stats.md) | Shipped decision | Bucket semantics, ledger display, pricing uncertainty, and review history. |

### Grok provider

The Grok implementation used spec/trace verification rather than a numbered
root record:

- [Provider specification](../docs/grok/spec.md)
- [Vertical trace](../docs/grok/trace.md)

These are implementation evidence. End users should start at
[Models](../docs/models.md) and
[Configuration](../docs/configuration.md#grok-request-shaping).

## Research

| Record | Use |
| --- | --- |
| [03 — Research notes](03-research-notes.md) | Competitor and early Rust-port inputs. |
| [04 — CLIProxyAPI research](04-research-cliproxyapi.md) | Provider/proxy implementation observations. |
| [07 — Scheduler research](07-scheduler-research.md) | Inputs and alternatives preceding the perishability decision. |

Research records are not compatibility promises.

## llmux Islands

The current cross-platform implementation contract is the
[Linux port dossier](docs/llmux-islands-linux-port/README.md):

- [Shipped surface inventory](docs/llmux-islands-linux-port/inventory.md)
- [macOS-to-KDE platform mapping](docs/llmux-islands-linux-port/platform-mapping.md)
- [Presentation design rules](docs/llmux-islands-linux-port/design.md)
- [Shared semantic specification](docs/llmux-islands-linux-port/spec.md)
- [Vertical trace and verification matrix](docs/llmux-islands-linux-port/trace.md)
- [CI-produced visual receipts](docs/llmux-islands-linux-port/visual-receipts/README.md)

Earlier native-client documents remain as historical macOS inputs:

- [11 — Original llmux Islands specification](11-llmux-islands-spec.md)
- [12 — Original llmux Islands architecture](12-llmux-islands-architecture.md)

They predate the shared Rust core, Grok state, and Linux/KDE shell and must not
be used as the current platform inventory.

## Closed convergence logs

These directories preserve original intent, acceptance matrices, and evidence
for completed work. They are not active TODO lists.

| Project | Records |
| --- | --- |
| Email-anonymous link behavior | [SSOT](email-anon-link/ssot.md) |
| Fable usage semantics | [SSOT](fable-usage/ssot.md) · [loop](fable-usage/loop.md) |
| Islands compact label and privacy | [SSOT](islands-todo/ssot.md) · [loop](islands-todo/loop.md) |
| Token/model dashboard epic | [SSOT](token-dashboard-62/ssot.md) · [plan](token-dashboard-62/plan.md) · [loop](token-dashboard-62/loop.md) · [schema](token-dashboard-62/gist-01-schema.md) · [checklist](token-dashboard-62/gist-02-checklist.md) |

## Status and supersession rules

1. Keep factual evidence immutable unless its provenance is wrong.
2. Mark a superseded plan at the top and link its replacement; do not silently
   rewrite historical acceptance.
3. Update `01-spec.md` and `02-architecture.md` when the shipped boundary
   changes.
4. Update the narrow owning user guide in the same PR for behavior users can
   observe.
5. Add every new decision/evidence directory to this index with a lifecycle.

Documentation ownership is enforced by
[`rules/documents.md`](../rules/documents.md).
