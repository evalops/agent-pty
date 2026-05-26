#!/usr/bin/env bash
set -euo pipefail

BIN="${AGENT_PTY_BIN:-agent-pty}"
ROOT="${AGENT_PTY_ROOT:-$(mktemp -d /tmp/agent-pty-cli-example.XXXXXX)}"
SOCKET="${AGENT_PTY_SOCKET:-$ROOT/agent-pty.sock}"
LOG_DIR="${AGENT_PTY_LOG_DIR:-$ROOT/logs}"
WORKSPACE="${AGENT_PTY_WORKSPACE:-$ROOT/workspace}"
SESSION="${AGENT_PTY_SESSION:-cli-agent}"

mkdir -p "$ROOT" "$LOG_DIR" "$WORKSPACE"

cleanup() {
  "$BIN" --socket "$SOCKET" stop >/dev/null 2>&1 || true
}
trap cleanup EXIT

"$BIN" --socket "$SOCKET" serve --log-dir "$LOG_DIR" >"$ROOT/daemon.log" 2>&1 &

for _ in $(seq 1 200); do
  if [[ -S "$SOCKET" ]]; then
    break
  fi
  sleep 0.025
done
if [[ ! -S "$SOCKET" ]]; then
  echo "agent-pty socket did not appear: $SOCKET" >&2
  exit 1
fi

"$BIN" --socket "$SOCKET" new --repo "$WORKSPACE" --name "$SESSION" --shell /bin/sh >/dev/null
"$BIN" --socket "$SOCKET" send "$SESSION" "printf 'cli-agent-ok\n' > agent-output.txt && printf 'cli-agent-ok\n'" >/dev/null
"$BIN" --socket "$SOCKET" wait "$SESSION" --until "cli-agent-ok" --timeout 10s >/dev/null
"$BIN" --socket "$SOCKET" screen "$SESSION" --format json >"$ROOT/screen.json"
proof_html="$("$BIN" --socket "$SOCKET" proof "$SESSION" --html)"
"$BIN" --socket "$SOCKET" kill "$SESSION" >/dev/null

printf '{\n'
printf '  "transport": "cli",\n'
printf '  "workspace": "%s",\n' "$WORKSPACE"
printf '  "screen_json": "%s",\n' "$ROOT/screen.json"
printf '  "proof_html_path": "%s"\n' "$proof_html"
printf '}\n'
