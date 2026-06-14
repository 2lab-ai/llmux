# llmux demo — recorded with charmbracelet/vhs (https://github.com/charmbracelet/vhs)
#
#   PATH="$PWD/target/release:$HOME/.local/bin:$PATH" \
#   LLMUX_DEMO_CONFIG=/tmp/llmux-demo.json vhs demo/llmux.tape
#
# Prerequisites (kept OUT of this tape because they touch real accounts):
#   - `llmux` (release build) and `claude` (real Claude Code) on PATH.
#   - A demo config at $LLMUX_DEMO_CONFIG (default /tmp/llmux-demo.json): a copy
#     of a real config with "proxy.port": 3457. A handful of accounts is plenty.
#
# Layout: a tmux split — right pane is the live llmux server TUI (scheduler,
# accounts, traffic), left pane runs REAL Claude Code through the proxy. Keys are
# scripted; the `claude` and `llmux` processes are real. `LLMUX_DEMO_MODE=1`
# swaps account emails for stable fakes so nothing real is exposed. The tmux uses
# a private socket (`-L lxdemo`, no config) so a host tmux-resurrect / continuum
# setup can't restore an unrelated session into the frame.

Output screenshots/llmux-demo.gif

Require tmux
Require llmux
Require claude

Set Shell "bash"
Set FontSize 13
Set Width 1560
Set Height 860
Set Padding 14
Set Framerate 24

# --- setup (hidden): two-pane tmux, llmux server on the right, claude env left ---
Hide
Type "export LLMUX_CONFIG=${LLMUX_DEMO_CONFIG:-/tmp/llmux-demo.json} LLMUX_DEMO_MODE=1; clear"
Enter
Type "tmux -L lxdemo -f /dev/null new-session -d -s d -x 236 -y 56"
Enter
Type "tmux -L lxdemo split-window -h -t d -l 42%"
Enter
# right pane: the live llmux server TUI (binds :3457 from LLMUX_CONFIG, IS the dashboard)
Type "tmux -L lxdemo send-keys -t d:0.1 'llmux server' Enter"
Enter
Sleep 4s
# left pane: point Claude Code at the proxy, keep the command line clean
Type "tmux -L lxdemo send-keys -t d:0.0 'export ANTHROPIC_BASE_URL=http://localhost:3457 ANTHROPIC_MODEL=claude-sonnet-4-6; clear' Enter"
Enter
Type "tmux -L lxdemo select-pane -t d:0.0"
Enter
Type "tmux -L lxdemo attach -t d"
Enter
Show

# --- the demo: one real Claude Code call through llmux ---
Sleep 1500ms
Type@55ms "claude --strict-mcp-config -p "
Type `"`
Type@55ms "In two sentences, what does llmux give a Claude Code user?"
Type `"`
Sleep 600ms
Enter

# Claude answers through llmux; the right pane shows the request land on a
# scheduler-picked account, tokens count up, then settle.
Sleep 16s
Sleep 3s
