# Documentation ownership (`rules/documents.md`)

Binding for contributors and agents. Loaded via [`AGENTS.md`](../AGENTS.md).

## Rule

**Every user-visible change updates its owning doc in the same PR.**
If nothing user-facing changed, write a one-line **N/A** reason in the PR body
(`docs: N/A — <why>`). Skipping both the update and the N/A reason means the
feature is incomplete.

## Owning-doc map

| Change surface | Owning doc(s) |
| --- | --- |
| Product story, platform support, five-minute path, high-level “what ships” | [`README.md`](../README.md) |
| Installation, account onboarding, first request, first verification | [`docs/getting-started.md`](../docs/getting-started.md) |
| Product mental model / router-vs-scheduler / trust boundary | [`docs/concepts.md`](../docs/concepts.md) |
| Current reader-facing runtime/data-flow architecture and diagram | [`docs/architecture.md`](../docs/architecture.md), [`docs/assets/architecture-overview.svg`](../docs/assets/architecture-overview.svg), and generated HTML/PNG siblings |
| Commands, TUI keys, daemon/dashboard, activity, provider operational behavior | [`docs/operational-reference.md`](../docs/operational-reference.md) |
| Scheduler eligibility/mode/operator policy | [`docs/guides/scheduling.md`](../docs/guides/scheduling.md) |
| Remote-daemon topology, command ownership, and transport requirements | [`docs/guides/remote-daemon.md`](../docs/guides/remote-daemon.md) |
| Config file keys, proxy/scheduler/routing/account types | [`docs/configuration.md`](../docs/configuration.md) |
| Context-window / common usage Q&A | [`docs/faq.md`](../docs/faq.md) |
| Model catalog / aliases / `max_context` | [`docs/models.md`](../docs/models.md) |
| Islands cross-platform user behavior/install/privacy/screenshots | [`docs/llmux-islands.md`](../docs/llmux-islands.md) |
| macOS or Linux Islands developer/build/platform detail | matching component README: [`llmux-islands/`](../llmux-islands/README.md), [`llmux-islands-linux/`](../llmux-islands-linux/README.md), or [`llmux-islands-macos-bridge/`](../llmux-islands-macos-bridge/ABI.md) |
| Captured Claude Code / multi-model **system prompt** wire text | [`docs/system-prompts/`](../docs/system-prompts/) — especially [`samples/`](../docs/system-prompts/samples/); never replace real samples with meta-only prose |
| Maintained product/system contracts | [`.prd/01-spec.md`](../.prd/01-spec.md), [`.prd/02-architecture.md`](../.prd/02-architecture.md) |
| Decision/evidence lifecycle and navigation | [`.prd/README.md`](../.prd/README.md) |
| Other product/architecture *decisions* (not how-to) | [`.prd/`](../.prd/) |
| Grok provider STV design notes | [`docs/grok/`](../docs/grok/) (design artifact; not a user how-to) |
| Agent architecture rules / conventions / runbooks | [`AGENTS.md`](../AGENTS.md) |

When unsure, update the narrowest row that a new user would open to understand
the change. Prefer one owning doc over shotgun edits.

## Same-PR checklist (docs-impact)

1. **Classify** the change against the map above (or mark N/A).  
2. **Edit** the owning doc so it matches shipped behavior (commands, flags,
   routes, surfaces).  
3. **Index** — if you added a new guide, link it from
   [`docs/README.md`](../docs/README.md); if you added a decision/evidence
   record, classify it in [`.prd/README.md`](../.prd/README.md). Add it to the
   root README only when it is a primary entry point.
4. **Samples** — if the Claude Code / multi-model **prompt surface** changed,
   re-capture or note the drift under `docs/system-prompts/`; do not invent
   prompt text.  
5. **Links** — run `just docs-check`, then open every path/visual you touched;
   no broken relative links or stale generated PNG.
6. **PR body** — either list docs files updated, or `docs: N/A — …`.

## Explicit non-goals

- No requirement to rewrite product README voice for every internal refactor.  
- No full re-dump of raw-io secrets into `samples/`.  
- No “docs later” follow-up issues as a substitute for the same-PR rule.

## Mechanical enforcement

`just docs-check` validates relative Markdown/HTML links and local anchors.

## Change-impact enforcement (future)

P1 / out of this rule’s initial ship: fail or warn in `just check` when `src/`
diff is non-empty and no `docs/` / `README` / `AGENTS` / `rules` path changed
without a `docs: N/A` trailer. Until then, this file + review is the gate.
