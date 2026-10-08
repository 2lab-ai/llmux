# Multi-model agents in Claude Code

Claude implements, GPT reviews, inside the same Claude Code session. With llmux
in front of Claude Code, a custom agent whose `model:` is a GPT id is served by
your Codex account while the parent session stays on Claude. Add a Grok agent
and one change is handled by three models: Claude delegates and synthesizes,
GPT and Grok review independently, and you collect where they disagree. The
three-model run below was measured on 2026-10-08.

## How it works

**The model id is the routing signal.** Claude Code custom agents
(`.claude/agents/<name>.md`) accept a full model ID in the frontmatter `model:`
field ([Claude Code docs](https://code.claude.com/docs/en/sub-agents#choose-a-model)).
`llmux run` sets `ANTHROPIC_BASE_URL=http://localhost:3456`, so each agent's
request reaches llmux carrying that id, and llmux routes by name: Claude ids to
Claude accounts, `gpt-*` to Codex, `grok-*` to Grok, `or-*` to OpenRouter. One
trailing `[1m]` is stripped before the upstream id is resolved; the suffix only
tells Claude Code to use a 1M-context display.

**The agents are native Claude Code subagents.** Each runs with its own context
and only the tools in its `tools:` list, and returns its result to the parent
like any subagent. The parent's memory is not automatically shared: an agent
sees the prompt the parent writes for it plus what its own tools can read.

**llmux does not orchestrate.** It routes requests. Rounds, synthesis rules and
verdict formats are your agent files and prompts. Within a provider group the
scheduler picks the serving account ([schedulers.md](schedulers.md)), preferring
quota that would otherwise expire unused; it never substitutes GPT for Claude or
the reverse.

## Step 1 — Claude + GPT

You need llmux, a Claude Code version that supports custom agents with
`model:`, and two logins. The receipt below was measured on preview build 2026-10-08-0306, the
same source that became stable v0.2.25; a stable build was not measured separately:

```bash
llmux login          # Claude account
llmux login --codex  # Codex / ChatGPT account
```

Create `.claude/agents/gpt-reviewer.md` in your project:

```markdown
---
name: gpt-reviewer
description: Independent code reviewer running on GPT. Use for a second-opinion review of a file or diff.
model: gpt-6-astra[1m]
tools: Read
---
You are an independent code reviewer. Read the file you are given, list concrete defects with line numbers, and end with one line: VERDICT: <APPROVE|REQUEST_CHANGES>. Also state which model you are, in one line starting with MODEL:.
```

Start `llmux run` and ask the parent to delegate. This one-agent form was not
recorded separately; the measured run is [Step 3](#step-3--three-models-one-change).

```text
Delegate a review of sample.py to the gpt-reviewer subagent (use the Agent tool; do not review the file yourself). Then quote its VERDICT: line.
```

**Check where it ran.** In `llmux dashboard`, the Activity feed shows a row with
kind `subagent`, model `gpt-6-astra`, group `codex` and the serving Codex account
(Islands lists it in recent activity). That row is the routing proof; the
`MODEL:` line is self-report. Expand the row and open the raw viewer from its
`🔍 request` line to read the exact request the agent sent ([ai-debugger.md](ai-debugger.md)).

## Step 2 — add Grok

Run `llmux login --grok`, then create `.claude/agents/grok-reviewer.md`:

```markdown
---
name: grok-reviewer
description: Independent code reviewer running on Grok. Use for a third-opinion review of a file or diff.
model: grok-4.7
tools: Read
---
You are an independent code reviewer. Read the file you are given, list concrete defects with line numbers, and end with one line: VERDICT: <APPROVE|REQUEST_CHANGES>. Also state which model you are, in one line starting with MODEL:.
```

Model ids valid on 2026-10-08 (`GET /models`, see [models.md](models.md)):
`claude-fable-5-1[1m]`, `claude-opus-5-5[1m]`, `claude-sonnet-5-5[1m]`,
`claude-haiku-5-5[1m]`, `gpt-6.1-sol`, `gpt-6-astra[1m]`,
`gpt-6-luna`, `gpt-5.6-sol`, `gpt-5.6-terra`, `gpt-5.5`, `grok-4.7`,
`grok-4.6`, `grok-4.5`, `or-ox-alpha`. Aliases such as `astra`, `sol` and
`grok` also work in `model:`; explicit ids keep the routing readable in the
agent file.

## Step 3 — three models, one change

Measured setup: Claude Code 2.1.294, a project directory with the two agent
files above, and this `sample.py` (use your own file instead; the defects below
are specific to this one):

```python
def average(values):
    total = 0
    for v in values:
        total += v
    return total / len(values)

def find_user(users, name):
    for u in users:
        if u["name"] == name:
            return u
    return users[0]
```

The parent ran on Claude Fable:

```bash
llmux run -- --model fable \
  -p "Delegate a review of sample.py to the gpt-reviewer subagent AND separately to the grok-reviewer subagent (use the Agent tool for each; do not review the file yourself). Then output a short synthesis: for each reviewer, quote its MODEL: line and VERDICT: line verbatim, then list which defects only one of them found." \
  --output-format stream-json --verbose --allowedTools Read,Agent,Task --max-turns 12
```

This is the exact headless command that produced the receipt. It passed
`--allowedTools Read,Agent,Task` explicitly. A second headless run on the same
day without that flag (same `gpt-reviewer` file, one agent, no `--allowedTools`)
also delegated and returned `MODEL: gpt-6-astra[1m]` / `VERDICT: REQUEST_CHANGES`
with zero permission denials in 3 turns, so the flag is not required for
delegation; it is shown because it is what the receipt used. Interactively,
start `llmux run` and type the same prompt. Claude Code reported
success after 7 turns and 183 s, parent model `claude-fable-5-1[1m]`. The
parent's final text, verbatim from its first reviewer heading to the end (the
opening lines before that heading are omitted):

```markdown
**gpt-reviewer**
```
MODEL: gpt-6-astra[1m] (as identified by the runtime instructions)
VERDICT: REQUEST_CHANGES
```

**grok-reviewer**
```
MODEL: grok-4.7
VERDICT: REQUEST CHANGES — empty-input crashes and a silent wrong-user fallback make both functions unsafe to call as written.
```

**Shared findings.** Both reviewers flagged the same three defects: `average` divides by zero on empty input, `find_user` silently returns the first user when no name matches, and `find_user` raises IndexError on an empty list.

**Found only by grok-reviewer**
- Line 9: `u["name"]` assumes every element is a mapping with a `"name"` key. A missing key raises KeyError and a non-mapping element raises TypeError, aborting the search before later matches are considered.

**Found only by gpt-reviewer**
- None.

PARENT MODEL: claude-fable-5-1[1m]
```

The useful part is the difference: three defects both reviewers found, and one
(the unchecked `"name"` key on line 9) that only Grok reported.

Selected daemon activity rows during the run (5 of 26; emails masked), from
`activity.jsonl` and the dashboard Activity feed. Omitted rows are three further
`gpt-6-astra` subagent turns, the parent's other turns, Claude Code's own
internal Opus/Sonnet subagent, security-monitor and token-count calls. Each `excerpt` is the start of the prompt the parent
wrote for that agent:

```text
kind=sdk       model=claude-fable-5-1  group=claude  account=claude:…  status=200
kind=subagent  model=gpt-6-astra       group=codex   account=codex:…   status=200  excerpt="Review the Python file at this absolute path…"
kind=subagent  model=grok-4.7          group=grok    account=grok:…    status=200  excerpt="Review the Python file at this absolute path…"
kind=subagent  model=gpt-6-astra       group=codex   account=codex:…   status=200
kind=subagent  model=grok-4.7          group=grok    account=grok:…    status=200
```

**Make it repeatable.** Put the prompt in a project slash command or skill so a
round is one command and the synthesis format is versioned with the repo; later
rounds (disagreements sent back to each reviewer) are more prompts in the same place.

## Limits and honest notes

- **No built-in consensus.** llmux ships no `trinity` command and no automatic
  consensus. Review rounds, synthesis rules and verdict formats are yours.
- **Collaboration is not wire parity.** Codex and Grok receive translated
  Messages requests: `max_tokens` is not sent to Codex at all, sampling
  parameters are refused, and prior `thinking` blocks are dropped. See
  [provider-compatibility.md](provider-compatibility.md).
- **No shared memory.** Each agent gets the prompt the parent writes plus its
  own tool access. Name files, paths and constraints in that prompt.
- **`unrecognized_model` warning.** Claude Code's stderr printed
  `[claude-code:unrecognized_model] {"model":"gpt-6-astra[1m]","query_source":"agent:custom:gpt-reviewer"}`
  and the same for `grok-4.7`. In the measured run the requests were still sent
  and served.
- **Cost figures.** Claude Code's reported cost is an API-equivalent figure;
  subscription accounts are billed by their own quota windows.
- **Account terms.** See [Compliance & caveats](../README.md#compliance--caveats).

## Troubleshooting

- **The agent answered as Claude.** Check its `model:` id against `GET /models`
  and confirm the `subagent` row in Activity shows group `codex` or `grok`.
- **429 or a parked account.** Activity shows the cooldown. The scheduler picks
  another eligible account in the same group, never another provider.
- **`no eligible account for group codex`.** Run `llmux login --codex`.

## Receipt

- 2026-10-08, llmux 0.2.24 preview `2026-10-08-0306-f95dcb75dd51`, Claude Code
  2.1.294, parent `claude-fable-5-1[1m]`, agents `gpt-6-astra[1m]` and `grok-4.7`.
- Command, transcript excerpt, activity rows: [Step 3](#step-3--three-models-one-change);
  Claude Code result: success, 7 turns, 183 s.
- A headless `-p` run of the same Claude Code client; GUI clicks were not recorded.
