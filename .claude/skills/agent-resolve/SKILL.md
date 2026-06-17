---
name: agent-resolve
description: Use when the user says "resolve", "해결해줘", "픽스해줘", or wants to take one ready-to-agent llmux issue/PR, implement the fix on its branch, get `just check` green, and hand back a review-ready PR. Does NOT merge or deploy by default.
---

# agent-resolve — one issue → green, review-ready PR

**Core safety boundary: `agent-resolve` produces a green, review-ready PR and STOPS.** It does **not**
merge to `master` and does **not** 배포. In this repo "배포" pushes to a **public** preview channel
(master → CI preview → brew); that recovery cost is asymmetric, so **a human gates merge + deploy**.

> The original request was "resolve and 배포". That is intentionally downgraded to a human gate
> (validated by strategist consult). Auto-ship exists only behind an explicit opt-in (bottom) and is
> **OFF by default**. Build/deploy mechanics live in `.claude/skills/_shared/cd-reference.md`.

## Steps

1. **Pick one.** If the user named an issue/PR, use it. Else the oldest `ready-to-agent` draft PR
   that is not yet `agent-wip`:
   `gh pr list --repo 2lab-ai/llmux --draft --label ready-to-agent --json number,headRefName,createdAt`.
2. **Lock it.** Add the `agent-wip` label (prevents `agent-loop`/double-work). Check out its branch
   (`agent/issue-<N>-<slug>`).
3. **Re-verify it is actually fixable in-repo** (gate item #1). If mid-work you discover it is not
   (the gpt-5.5 / out-of-repo pattern): **STOP — do not ship a non-fix.** Post a root-cause comment
   (`file:line`, or "out-of-repo: <why>"), remove `ready-to-agent`, add `agent-blocked` + the right
   downgrade (`not-in-repo` / `needs-design`), drop `agent-wip`, leave the draft PR with the analysis. Move on.
4. **Implement on the branch.** Respect AGENTS.md: typed errors, **no `unwrap`/`expect` in prod
   paths**, never log raw creds (`proxy::logging::mask_credentials`), config writes via `config::update`
   (read-merge-write).
5. **Gate: `just check` green** (fmt + clippy -D warnings + tests). Add/adjust tests for the change.
6. **Kill-switches (hard — enforce, don't rationalize past them):**
   - `just check` still red after **2** honest fix attempts → stop, `agent-blocked` + comment, drop `agent-wip`.
   - diff > **~400 LOC** or > **~15 files** → stop, escalate `needs-design`, do not commit.
   - touches secrets/auth, `Cargo.toml` version, `v*` tags, brew, or `.github/workflows/` → stop, `needs-human`.
   - **never** commit to / push `master` (feature branch only).
7. **Commit** (conventional, lowercase, no emoji, **no AI co-author line**) and `git push` the branch
   (token fallback in cd-reference if the remote `ghs_` token is stale).
8. **Flip to review-ready:** `gh pr ready <N>`. Set the PR body: "Agent-authored. `just check` green.
   **Needs human review + merge.** After merge, deploy with the `deploy` skill." Remove `agent-wip`.
9. **Report:** PR link, what changed, `just check` result, and the explicit next human step
   (review → merge → `deploy`).

## Optional auto-ship — explicit opt-in only, OFF by default

Only if the user **explicitly** says "resolve and 배포" / "ship it" / passes `--ship`: after the PR is
green **and a human has merged it**, run the `deploy` skill. `agent-resolve` **never** merges its own PR
and **never** deploys unmerged/unreviewed code, even with `--ship`.

## Red flags — STOP

- Auto-merging or 배포-ing a self-authored PR by default. The public channel is human-gated.
- Shipping a "fix" for an out-of-repo issue (the gpt-5.5 trap). If you can't point at the llmux line
  you changed **and** show it moves the behavior, it is not fixed.
- Committing with `just check` red, or with an emoji / AI co-author line.
- Touching `master`, `Cargo.toml` version, tags, workflows, or secrets.
- Pushing past a kill-switch threshold "because it's almost done."
