# Why llmux exists

llmux is a local proxy between your coding CLI and your model accounts. Claude Code (or Codex CLI) talks to `http://localhost:3456`, and llmux decides which account serves each request. This page walks through what it gives you in the order you are likely to notice it: the first reason to install it comes first, and the bet underneath comes last.

## 1. You have more than one Claude subscription

Islands shows every account's 5-hour and 7-day windows and their reset timers in one place. You no longer log in and out to find out which account still has room. If you stop here, this alone is worth the install.

## 2. Add a Codex subscription and Claude can delegate to GPT

Add a Codex (ChatGPT) subscription and Claude Code can hand work to a GPT agent natively. A `.claude/agents/*.md` file with `model: gpt-6-astra[1m]` runs as a normal Claude Code subagent. llmux routes its requests to your Codex account, and the result comes back to the parent. Claude implements and GPT reviews, inside one session. See [multi-model-agents.md](multi-model-agents.md).

## 3. Add Grok and three models can review one change

With a Grok account as well, three models can review the same change and the parent collects the points where they disagree. This is project configuration (a few agent files and a prompt), not a command that ships with llmux.

## 4. Several accounts per provider: the scheduler picks

When a provider group has several accounts, the scheduler picks which account serves each request. Eligibility gates come first. Among the eligible accounts, it prefers quota that would otherwise expire unused before its reset, and it keeps a conversation on the same account to protect prompt caches. It never swaps providers: when your Claude accounts run out, llmux does not quietly answer with GPT. See [schedulers.md](schedulers.md).

## 5. What falls out: your harness stays

None of this changes your Claude Code setup. Tools, permissions, hooks, project memory and conventions stay as they are. The model becomes a routing signal: `/model fable`, `/model gpt-6-astra[1m]`, `/model grok-4.7`, or a per-agent `model:` line.

## 6. The same pool serves Codex CLI

`llmux run --codex` points Codex CLI at the same accounts. llmux does not synchronize settings, hooks or skills between Claude Code and Codex CLI. A shared configuration across CLIs is an idea under consideration, not a shipped feature.

## 7. Bonus: one daemon for several computers, and a raw request view

You can run one central daemon on a main computer and use your other computers as clients. Each client gets an issued key, so usage is attributed per computer. See [remote.md](remote.md).

For experts, the raw request viewer shows exactly what reached each model. See [ai-debugger.md](ai-debugger.md).

## The bet

The model is a consumable. The harness is capital.

A client harness is the operating environment around the model: file edits, shell execution, subagents, tool calls, context management, permissions, hooks, local conventions and project memory. That environment is the expensive part. Rebuilding it every time a new model appears is the cost llmux is built to avoid. Four problems sit behind that cost:

1. **Harness lock-in.** A Claude Code workflow does not transfer cleanly to Codex CLI or other CLIs. llmux does not make harnesses interchangeable; it lets you stay in the one you chose and still reach other models.
2. **Sync drift.** If you port a workflow once, each harness still evolves separately, and keeping them equivalent becomes its own job. llmux does not sync harnesses; it reduces how much you need to port.
3. **Model lock-in.** Trying a better model often means leaving the harness you invested in. Through llmux, it is a `/model` switch or a `model:` line in an agent file, for any model your connected accounts can serve.
4. **Subscription friction.** Quota windows reset on fixed timestamps, and quota still unused at the reset is gone. Islands makes the windows visible, and the scheduler leans toward the quota that is about to expire.

Your workflow stays put while the models behind it change.
