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
2. **Lock it + isolate in a worktree.** Add the `agent-wip` label (prevents `agent-loop`/double-work).
   Then work in a **dedicated git worktree** — **never** the main checkout, so parallel resolves and
   the user's live tree never collide (see **Worktree isolation** below):
   `git -C <repo> worktree add -B agent/issue-<N>-<slug> "<repo>/../llmux-wt/issue-<N>" master`
   then `cd` into `<repo>/../llmux-wt/issue-<N>`. (`-B` resets that path's branch to current `master`.)
3. **Re-verify it is actually fixable in-repo** (gate item #1). If mid-work you discover it is not
   (the gpt-5.5 / out-of-repo pattern): **STOP — do not ship a non-fix.** Post a root-cause comment
   (`file:line`, or "out-of-repo: <why>"), remove `ready-to-agent`, add `agent-blocked` + the right
   downgrade (`not-in-repo` / `needs-design`), drop `agent-wip`, leave the draft PR with the analysis. Move on.
4. **Implement in the worktree.** Respect AGENTS.md: typed errors, **no `unwrap`/`expect` in prod
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
10. **Tear down the worktree** once the PR is open — the branch lives on the remote, the worktree is
    disposable: `git -C <repo> worktree remove "<repo>/../llmux-wt/issue-<N>"` then `git -C <repo>
    worktree prune`. On a **kill-switch stop, leave the worktree in place** for human inspection.

## Worktree isolation

Every resolve runs in its **own git worktree**, never the shared main checkout. This is what makes
parallel `agent-loop` resolves and the user's live editing safe simultaneously — no branch-switching
or index races in the primary tree.

- **Path convention (standard):** `<repo>/../llmux-wt/issue-<N>` — a sibling of the main checkout,
  never nested inside it. (`<repo>` = `git -C <repo> rev-parse --show-toplevel`.)
- **Create/reset:** `git -C <repo> worktree add -B agent/issue-<N>-<slug> "<repo>/../llmux-wt/issue-<N>" master`.
- **A branch can be checked out in only one worktree.** If `git worktree list` already shows one for
  this issue, reuse it (`cd` in, `git pull`/rebase onto `master`) instead of creating a second.
- **Remove when done** (`git worktree remove` + `prune`); the pushed branch + PR are the durable artifact.

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
- Working in the main checkout instead of a dedicated worktree (races other agents / the user's live tree).
