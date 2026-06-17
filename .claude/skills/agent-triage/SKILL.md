---
name: agent-triage
description: Use when the user says "triage", "이슈 정리", "분류", or wants to scan open llmux GitHub issues, decide which are safe for an agent to implement, label them, and open a tracking branch + draft PR for each ready one. Reads and labels issues; never writes source code.
---

# agent-triage — gate issues for the agent pipeline

`agent-triage` is a **default-reject safety gate**, not a classifier. An issue earns `ready-to-agent`
only when **all 7 gate items have citable evidence**. Everything else gets an explicit label —
**never silently skipped**. For each ready issue it opens a draft PR that acts as the work lock
for `agent-resolve`. Pairs with `agent-resolve` (does the work) and `agent-loop` (orchestrates both).

Repo: `2lab-ai/llmux`, base branch `master`. Labels were created already
(`ready-to-agent` / `needs-human` / `needs-design` / `not-in-repo` / `agent-wip` / `agent-blocked`).

## Steps

1. **Fetch un-triaged open issues.** `gh issue list --repo 2lab-ai/llmux --state open --json number,title,labels,body`.
   **Skip any issue that already has one of the six agent labels** — labels are the pipeline's
   memory; re-triaging them is thrashing (a human removes the label to re-enter).
2. **Apply the 7-point gate** to each, citing evidence per item. **ALL must pass for `ready-to-agent`:**
   1. **In-repo completeness** — name ≥1 candidate `file:path` *inside llmux* that holds the root cause. Can't name one → fail.
   2. **Spec clarity** — a one-sentence done-condition exists.
   3. **No human/product/policy decision** required.
   4. **No security surface** — no secret/auth/credential/cost/rate-limit changes.
   5. **No release surface** — no `Cargo.toml` version, `v*` tags, brew formulae, or `.github/workflows/`.
   6. **Verifiable** by `just check` (fmt + clippy -D warnings + tests), possibly with a new in-repo test.
   7. **Bounded blast radius** — roughly one module; not a broad re-architecture.
3. **Pick the label from the first failure:**
   - all 7 pass → `ready-to-agent`
   - fails #1 (root cause outside llmux) → `not-in-repo`
   - fails #2 or #7 (ambiguous / too large) → `needs-design`
   - fails #3 / #4 / #5 (needs a human, security, or release call) → `needs-human`
4. **Post the gate evidence as an issue comment** (a 7-row pass/fail table with the cited evidence) —
   this is the auditable artifact — then apply the label. The comment must exist *before* `ready-to-agent`.
5. **For each `ready-to-agent` issue, open the work lock (idempotently) — never in the main checkout:**
   - Idempotency: if `git ls-remote --heads origin "agent/issue-<N>-*"` returns a branch, **skip** (already tracked).
   - Else create the tracking branch in a **throwaway worktree** so the main tree is untouched
     (same path convention `agent-resolve` reuses — `<repo>/../llmux-wt/issue-<N>`):
     `git -C <repo> worktree add -B agent/issue-<N>-<slug> "<repo>/../llmux-wt/issue-<N>" master`
     → `git -C "<wt>" commit --allow-empty -m "chore(agent): track #<N> <slug>"`
     → `git -C "<wt>" push -u origin agent/issue-<N>-<slug>`.
     Leave the worktree for `agent-resolve` to reuse, or `git worktree remove` it — the pushed branch is what the draft PR needs.
   - `gh pr create --draft --base master --head agent/issue-<N>-<slug> --title "<type>: <issue title>" --body "Refs #<N>"`
     (draft PR linking the issue; the draft state is the "not yet worked" signal `agent-resolve` keys on).
6. **Report** a table: every open issue → assigned label → (branch + draft PR link if ready).
   **Surface borderline issues explicitly** (the `needs-*` / `not-in-repo` ones), don't bury them.

## Red flags — STOP

- Marking `ready-to-agent` without a cited `file:path` — that is trusting your own judgment over evidence. **Default is reject.**
- Silently skipping a hard issue instead of labeling it `needs-design` / `needs-human` / `not-in-repo`.
- Re-triaging an already-labeled issue (thrashing).
- Creating a second branch/PR for an issue that already has one (always `ls-remote` first).
- Editing any source file — `agent-triage` reads and labels only.

## Example: why issue #1 is NOT ready

The "gpt-5.5 remaining-context" issue fails gate item **#1**: its root cause is in the Claude Code
*client* (it infers the context window from the model-name string); no llmux `file:path` can fix the
displayed number. → label `not-in-repo`, comment with the evidence, no PR. Shipping a "fix" here
would be a non-fix. This is the exact failure mode the gate exists to catch.
