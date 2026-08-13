# Why llmux exists

The model is a consumable. The harness is capital.

Claude Code is not just a chat box. It is the operating environment around the model: file edits, shell execution, subagents, tool calls, context management, permissions, hooks, local conventions, and project memory. Rebuilding that environment every time a new frontier model appears is the expensive part.

llmux makes a different bet:

- **Keep Claude Code as the canonical harness.** Do not port your workflow to every vendor CLI.
- **Move the model boundary behind a local proxy.** Claude Code talks to `http://localhost:3456`; llmux decides which account/backend serves the request.
- **Use every account deliberately.** Multiple Claude subscription/API-key accounts, plus optional Codex and Grok accounts, live in one cockpit with quota-aware routing instead of manual juggling.
- **Treat model choice as a setting, not a migration.** `fable`, `opus`, `gpt-5.6-sol`, `grok-4.6`, and future model names become routing signals, not reasons to rebuild your agent stack.

The result: your workflow stays still while the model market moves.

## The problem llmux removes

1. **Harness lock-in.** A Claude Code workflow does not transfer cleanly to Codex CLI or Gemini CLI.
2. **Sync drift.** Even if you port once, every harness evolves separately. Keeping them equivalent becomes its own job.
3. **Model lock-in.** Trying a better model often means abandoning the harness you already invested in.
4. **Subscription friction.** Flat-rate accounts are useful only if you can route work to the right account before the quota window disappears.

llmux breaks the chain by standardizing on one harness and making the account/model layer swappable behind it.
