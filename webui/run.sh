#!/bin/bash
# Foreground fallback when no LaunchAgent can be loaded (e.g. a host with no GUI login):
# keeps the server up, restarting 2 s after any exit. Binds 127.0.0.1 only — the public
# hostname goes through Caddy and the soma-work login (deploy/Caddyfile.example).
cd "$(dirname "$0")"
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
export BIND="${BIND:-127.0.0.1}" PORT="${PORT:-4780}"
while true; do
  node server.mjs >> server.log 2>&1
  echo "[run.sh] server exited $? at $(date '+%F %T'); restarting" >> server.log
  sleep 2
done
