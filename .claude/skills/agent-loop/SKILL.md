---
name: agent-loop
description: Use when the user says "loop", "반복", "쭉 돌려줘", or wants to run triage then resolve repeatedly until there are no agent-workable llmux issues/PRs left. Thin orchestrator over the agent-triage and agent-resolve skills; dry-run by default.
---

# agent-loop — orchestrate triage → resolve until drained

`agent-loop` has **no judgment of its own**. It runs `agent-triage` once, then `agent-resolve` on the
resulting ready PRs one at a time, and **stops on clear conditions**. **Labels are its memory** — it
never re-evaluates issues already marked `needs-*` / `not-in-repo` / `agent-blocked`. Inherits
`agent-resolve`'s boundary: **it does not merge or 배포.**

## Steps

1. **Mode.** Default is **DRY-RUN**: run agent-triage's evaluation and print the resolve plan, change
   nothing. Act only if the user said "apply" / "실제로 해" / passed `--apply`.
2. **Run `agent-triage` once.** (Labels issues, opens draft PRs for ready ones, skips already-labeled.)
3. **Resolve loop.** Over `ready-to-agent` draft PRs, oldest first, run `agent-resolve` on **one per
   iteration**. After each, verify the PR is genuinely green (`just check` + CI) before counting it done.
4. **Stop when ANY:**
   - no `ready-to-agent` draft PRs remain (the user's "no issues/PRs left" condition),
   - `--max-iterations` reached (**default 3**),
   - **2 consecutive** `agent-resolve` runs end in `agent-blocked`,
   - an `agent-resolve` run hit a kill-switch needing a human (`needs-design` / `needs-human` / security / release).
5. **Report** a run summary: issues triaged (+ labels), PRs made review-ready, PRs blocked (+ why),
   and exactly what is left for a human (review → merge → `deploy`).

## Anti-thrashing

Never re-triage an issue that already carries a `needs-*` / `not-in-repo` / `agent-blocked` label —
a human must clear the label for it to re-enter the pipeline. Without this, the same un-fixable issue
(e.g. the gpt-5.5 client-limitation one) gets re-evaluated to the same dead end every iteration.

## Red flags — STOP

- Running in apply mode without explicit user opt-in.
- Re-triaging labeled issues (thrashing) — labels are memory.
- Merging or deploying — `agent-loop` inherits `agent-resolve`'s human-gated boundary.
- Running unbounded — always honor `--max-iterations` and the stop conditions.
