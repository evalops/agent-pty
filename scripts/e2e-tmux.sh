#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACTS="$ROOT/target/e2e-tmux/$(date +%Y%m%d-%H%M%S)"
CI=0
TIMEOUT_SECONDS=180

while [[ $# -gt 0 ]]; do
  case "$1" in
    --ci)
      CI=1
      shift
      ;;
    --artifacts)
      ARTIFACTS="$2"
      shift 2
      ;;
    --timeout)
      TIMEOUT_SECONDS="$2"
      shift 2
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 2
  fi
}

need tmux
need git
need cargo
need curl
need python3
need script

rm -rf "$ARTIFACTS"
mkdir -p "$ARTIFACTS"
ARTIFACTS="$(cd "$ARTIFACTS" && pwd)"

BIN="$ROOT/target/debug/agent-pty"
WORK="$ARTIFACTS/work"
REPO="$WORK/broken-rust-project"
LOG_DIR="$ARTIFACTS/agent-pty-logs"
HTTP_LOG_DIR="$ARTIFACTS/http-logs"
MCP_LOG_DIR="$ARTIFACTS/mcp-logs"
SOCKET="$ARTIFACTS/agent-pty.sock"
SESSION="agent-pty-e2e-$$"
HTTP_PORT="$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)"
HTTP_ADDR="127.0.0.1:$HTTP_PORT"
HTTP_URL="http://$HTTP_ADDR/request"

cargo build --manifest-path "$ROOT/Cargo.toml" >/dev/null

mkdir -p "$WORK" "$LOG_DIR" "$HTTP_LOG_DIR" "$MCP_LOG_DIR" "$ARTIFACTS/proofs"
cp -R "$ROOT/fixtures/broken-rust-project/." "$REPO/"
git -C "$REPO" init >/dev/null
git -C "$REPO" add .
git -C "$REPO" -c user.name='Agent PTY E2E' -c user.email='agent-pty-e2e@example.test' commit -m initial >/dev/null

DRIVER="$ARTIFACTS/driver.sh"
cat >"$DRIVER" <<'DRIVER'
#!/usr/bin/env bash
set -euo pipefail

RESULTS="$ARTIFACTS/results.env"
REPORT="$ARTIFACTS/report.md"
DRIVER_LOG="$ARTIFACTS/driver.log"
mkdir -p "$ARTIFACTS/concurrent" "$ARTIFACTS/proofs"
: >"$RESULTS"
: >"$DRIVER_LOG"

log() {
  echo "[driver] $*" | tee -a "$DRIVER_LOG"
}

record() {
  printf '%s=ok\n' "$1" >>"$RESULTS"
}

capture() {
  local name="$1"
  shift
  log "+ $*"
  set +e
  "$@" >"$ARTIFACTS/$name.out" 2>&1
  local status=$?
  set -e
  cat "$ARTIFACTS/$name.out" >>"$DRIVER_LOG"
  return "$status"
}

must() {
  local name="$1"
  shift
  if ! capture "$name" "$@"; then
    echo "command failed: $*" >&2
    exit 1
  fi
}

must_fail() {
  local name="$1"
  shift
  if capture "$name" "$@"; then
    echo "command unexpectedly succeeded: $*" >&2
    exit 1
  fi
}

assert_contains() {
  local file="$1"
  local pattern="$2"
  if ! grep -F "$pattern" "$file" >/dev/null; then
    echo "missing pattern '$pattern' in $file" >&2
    exit 1
  fi
}

wait_socket() {
  local socket="$1"
  for _ in $(seq 1 100); do
    [[ -S "$socket" ]] && return 0
    sleep 0.1
  done
  echo "socket did not appear: $socket" >&2
  exit 1
}

write_report() {
  {
    echo "# agent-pty tmux E2E report"
    echo
    echo "- artifacts: $ARTIFACTS"
    echo "- repo: $REPO"
    echo "- socket: $SOCKET"
    echo "- http: $HTTP_URL"
    echo
    echo "## Results"
    echo
    echo "- Unix CLI: ok"
    echo "- HTTP: ok"
    echo "- MCP: ok"
    echo "- tmux capture: ok"
    echo "- policy denial: ok"
    echo "- policy approval: ok"
    echo "- semantic screen: ok"
    echo "- prompt interaction: ok"
    echo "- human attach: ok"
    echo "- REPL interaction: ok"
    echo "- long-running logs: ok"
    echo "- tmux reconnect: ok"
    echo "- daemon restart replay: ok"
    echo "- fork comparison: ok"
    echo "- concurrent evidence access: ok"
  } >"$REPORT"
}

wait_socket "$SOCKET"

must cli_new "$BIN" --socket "$SOCKET" new --repo "$REPO" --name base
must cli_list "$BIN" --socket "$SOCKET" list
assert_contains "$ARTIFACTS/cli_list.out" "base active=true"

must cli_send_fail "$BIN" --socket "$SOCKET" send base "cargo test"
must cli_wait_fail "$BIN" --socket "$SOCKET" wait base --until "test result: FAILED" --timeout 30s
record unix_cli

must_fail policy_denial "$BIN" --socket "$SOCKET" send base "vault write secret/nope value=bad"
assert_contains "$ARTIFACTS/policy_denial.out" "vault write requires approval"
must policy_replay "$BIN" --socket "$SOCKET" replay base
assert_contains "$ARTIFACTS/policy_replay.out" "policy denied vault write"
record policy

APPROVED_POLICY_CMD="printf 'approved-policy\n' # vault write"
must policy_approval "$BIN" --socket "$SOCKET" approve base "$APPROVED_POLICY_CMD" --rule "vault write" --ttl 2m
APPROVAL_TOKEN="$(tr -d '\r\n' <"$ARTIFACTS/policy_approval.out")"
must policy_approved_send "$BIN" --socket "$SOCKET" send base "$APPROVED_POLICY_CMD" --approval "$APPROVAL_TOKEN"
must policy_approved_wait "$BIN" --socket "$SOCKET" wait base --until "approved-policy" --timeout 10s
must policy_approved_replay "$BIN" --socket "$SOCKET" replay base
assert_contains "$ARTIFACTS/policy_approved_replay.out" "policy approval created"
assert_contains "$ARTIFACTS/policy_approved_replay.out" "policy approved"
record policy_approval

must semantic_send "$BIN" --socket "$SOCKET" send base "printf 'error: semantic failure at src/lib.rs:1 http://localhost:1234/build\n'"
must semantic_wait "$BIN" --socket "$SOCKET" wait base --until "semantic failure" --timeout 10s
must semantic_screen "$BIN" --socket "$SOCKET" screen base --format json
assert_contains "$ARTIFACTS/semantic_screen.out" '"semantic"'
assert_contains "$ARTIFACTS/semantic_screen.out" '"error_lines"'
assert_contains "$ARTIFACTS/semantic_screen.out" 'http://localhost:1234/build'
assert_contains "$ARTIFACTS/semantic_screen.out" 'src/lib.rs:1'
record semantic_screen

must prompt_send "$BIN" --socket "$SOCKET" send base 'printf "ready? "; read answer; printf "answer=%s\n" "$answer"'
must prompt_wait_ready "$BIN" --socket "$SOCKET" wait base --until "ready?" --timeout 10s
must prompt_answer "$BIN" --socket "$SOCKET" send base "yes"
must prompt_wait_answer "$BIN" --socket "$SOCKET" wait base --until "answer=yes" --timeout 10s
record prompt

must attach_read "$BIN" --socket "$SOCKET" attach base --read-only --timeout 250ms
assert_contains "$ARTIFACTS/attach_read.out" "answer=yes"
must attach_prompt "$BIN" --socket "$SOCKET" send base 'printf "attach-ready> "; read line; printf "attach-got=%s\n" "$line"'
must attach_wait_ready "$BIN" --socket "$SOCKET" wait base --until "attach-ready>" --timeout 10s
must attach_write sh -c 'printf "from-attach\n" | "$1" --socket "$2" attach base --timeout 1s' sh "$BIN" "$SOCKET"
assert_contains "$ARTIFACTS/attach_write.out" "attach-got=from-attach"
must attach_replay "$BIN" --socket "$SOCKET" replay base
assert_contains "$ARTIFACTS/attach_replay.out" "attach human"
record attach

must repl_start "$BIN" --socket "$SOCKET" send base "python3 -q"
must repl_wait_prompt "$BIN" --socket "$SOCKET" wait base --until ">>>" --timeout 10s
must repl_print "$BIN" --socket "$SOCKET" send base "print('repl-ok')"
must repl_wait "$BIN" --socket "$SOCKET" wait base --until "repl-ok" --timeout 10s
must repl_exit "$BIN" --socket "$SOCKET" send base "exit()"
must repl_idle "$BIN" --socket "$SOCKET" wait base --until "idle:500ms" --timeout 10s
record repl

must logs_send "$BIN" --socket "$SOCKET" send base 'for i in 1 2 3 4 5; do printf "log-%s\n" "$i"; sleep 0.1; done'
must logs_wait "$BIN" --socket "$SOCKET" wait base --until "log-5" --timeout 10s
must logs_idle "$BIN" --socket "$SOCKET" wait base --until "idle:500ms" --timeout 10s
record logs

must ansi_send "$BIN" --socket "$SOCKET" send base 'printf "\033[31mansi-red\033[0m\nspinner-a\rspinner-done\n"'
must ansi_wait "$BIN" --socket "$SOCKET" wait base --until "spinner-done" --timeout 10s

must fork_a "$BIN" --socket "$SOCKET" fork base --new-name fix-a --copy-worktree
must fork_b "$BIN" --socket "$SOCKET" fork base --new-name fix-b --copy-worktree
must fix_a "$BIN" --socket "$SOCKET" send fix-a "cp repairs/fix_good.rs src/lib.rs && cargo test && echo 'tests passed'"
must fix_a_wait_ok "$BIN" --socket "$SOCKET" wait fix-a --until "test result: ok" --timeout 45s
must fix_a_wait_marker "$BIN" --socket "$SOCKET" wait fix-a --until "tests passed" --timeout 10s
must fix_b "$BIN" --socket "$SOCKET" send fix-b "cp repairs/fix_bad.rs src/lib.rs && cargo test"
must fix_b_wait "$BIN" --socket "$SOCKET" wait fix-b --until "test result: FAILED" --timeout 45s
record fork

must proof_a "$BIN" --socket "$SOCKET" proof fix-a
must proof_b "$BIN" --socket "$SOCKET" proof fix-b
cp "$LOG_DIR/proofs/fix-a.proof.md" "$ARTIFACTS/proofs/fix-a.proof.md"
cp "$LOG_DIR/proofs/fix-b.proof.md" "$ARTIFACTS/proofs/fix-b.proof.md"
cp "$LOG_DIR/proofs/fix-a.proof.html" "$ARTIFACTS/proofs/fix-a.proof.html"
must proof_a_html "$BIN" --socket "$SOCKET" proof fix-a --html
assert_contains "$ARTIFACTS/proofs/fix-a.proof.md" "cargo test"
assert_contains "$ARTIFACTS/proofs/fix-a.proof.md" "test result: ok"
assert_contains "$ARTIFACTS/proofs/fix-a.proof.md" "tests passed"
assert_contains "$ARTIFACTS/proofs/fix-b.proof.md" "test result: FAILED"
assert_contains "$ARTIFACTS/proofs/fix-a.proof.html" "agent-pty proof: fix-a"
assert_contains "$ARTIFACTS/proofs/fix-a.proof.html" "Commands Run"
assert_contains "$ARTIFACTS/proof_a_html.out" "fix-a.proof.html"

for i in $(seq 1 12); do
  "$BIN" --socket "$SOCKET" replay fix-a --json >"$ARTIFACTS/concurrent/replay-$i.json" &
  "$BIN" --socket "$SOCKET" proof fix-a >"$ARTIFACTS/concurrent/proof-$i.txt" &
done
wait
python3 - "$ARTIFACTS/concurrent" <<'PY'
import json
import pathlib
import sys
root = pathlib.Path(sys.argv[1])
for path in root.glob("replay-*.json"):
    data = json.loads(path.read_text())
    assert data, path
PY
record concurrent

must http_new curl -sS -X POST "$HTTP_URL" -d "{\"op\":\"new\",\"id\":\"http-e2e\",\"repo\":\"$REPO\",\"shell\":\"/bin/sh\",\"rows\":24,\"cols\":80,\"env\":{}}"
must http_send curl -sS -X POST "$HTTP_URL" -d "{\"op\":\"send\",\"id\":\"http-e2e\",\"text\":\"printf 'http-e2e-ok\\\\n'\",\"enter\":true}"
must http_wait curl -sS -X POST "$HTTP_URL" -d "{\"op\":\"wait\",\"id\":\"http-e2e\",\"until\":\"http-e2e-ok\",\"timeout_ms\":10000}"
assert_contains "$ARTIFACTS/http_wait.out" "http-e2e-ok"
must http_kill curl -sS -X POST "$HTTP_URL" -d '{"op":"kill","id":"http-e2e"}'
record http

printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' \
  "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"terminal.new\",\"arguments\":{\"id\":\"mcp-e2e\",\"repo\":\"$REPO\",\"shell\":\"/bin/sh\",\"rows\":24,\"cols\":80,\"env\":{}}}}" \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"terminal.send","arguments":{"id":"mcp-e2e","text":"printf '\''mcp-e2e-ok\\n'\''","enter":true}}}' \
  '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"terminal.wait","arguments":{"id":"mcp-e2e","until":"mcp-e2e-ok","timeout_ms":10000}}}' \
  '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"terminal.proof","arguments":{"id":"mcp-e2e"}}}' \
  '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"terminal.kill","arguments":{"id":"mcp-e2e"}}}' \
  | "$BIN" mcp-stdio --log-dir "$MCP_LOG_DIR" >"$ARTIFACTS/mcp.out"
assert_contains "$ARTIFACTS/mcp.out" "terminal.proof"
assert_contains "$ARTIFACTS/mcp.out" "mcp-e2e-ok"
record mcp

RECONNECT_ID="tmux-reconnect-$$"
rm -f "$REPO/tmux-resume.flag"
must tmux_backend_new "$BIN" --socket "$SOCKET" new --repo "$REPO" --name "$RECONNECT_ID" --shell /bin/sh --backend tmux
must tmux_backend_send "$BIN" --socket "$SOCKET" send "$RECONNECT_ID" "printf 'tmux-before-restart\n'; (while [ ! -f tmux-resume.flag ]; do sleep 0.1; done; printf 'tmux-after-restart\n') &"
must tmux_backend_wait_before "$BIN" --socket "$SOCKET" wait "$RECONNECT_ID" --until "tmux-before-restart" --timeout 10s
tmux respawn-pane -k -t "$DAEMON_PANE" "cd '$ROOT' && '$BIN' --socket '$SOCKET' serve --log-dir '$LOG_DIR' 2>&1 | tee -a '$ARTIFACTS/daemon-pane.log'"
sleep 1
wait_socket "$SOCKET"
printf 'go\n' >"$REPO/tmux-resume.flag"
must tmux_backend_wait_after "$BIN" --socket "$SOCKET" wait "$RECONNECT_ID" --until "tmux-after-restart" --timeout 10s
must tmux_backend_send_post "$BIN" --socket "$SOCKET" send "$RECONNECT_ID" "printf 'tmux-post-reconnect\n'"
must tmux_backend_wait_post "$BIN" --socket "$SOCKET" wait "$RECONNECT_ID" --until "tmux-post-reconnect" --timeout 10s
must tmux_backend_kill "$BIN" --socket "$SOCKET" kill "$RECONNECT_ID"
record tmux_reconnect

tmux respawn-pane -k -t "$DAEMON_PANE" "cd '$ROOT' && '$BIN' --socket '$SOCKET' serve --log-dir '$LOG_DIR' 2>&1 | tee -a '$ARTIFACTS/daemon-pane.log'"
sleep 1
wait_socket "$SOCKET"
must restart_replay "$BIN" --socket "$SOCKET" replay fix-a --json
assert_contains "$ARTIFACTS/restart_replay.out" "tests passed"
record restart

must trace_path "$BIN" --socket "$SOCKET" trace-path
assert_contains "$ARTIFACTS/trace_path.out" "traces.jsonl"
if [[ ! -s "$LOG_DIR/traces.jsonl" ]]; then
  echo "missing trace log" >&2
  exit 1
fi

write_report
echo "agent-pty-e2e-complete"
touch "$ARTIFACTS/done"
DRIVER
chmod +x "$DRIVER"

DRIVER_RUNNER="$ARTIFACTS/run-driver-with-typescript.sh"
cat >"$DRIVER_RUNNER" <<'RUNNER'
#!/usr/bin/env bash
set -euo pipefail

if script --version >/dev/null 2>&1; then
  exec script -q -c "$DRIVER" "$ARTIFACTS/driver.typescript"
fi

exec script -q "$ARTIFACTS/driver.typescript" "$DRIVER"
RUNNER
chmod +x "$DRIVER_RUNNER"

cleanup() {
  tmux kill-session -t "$SESSION" >/dev/null 2>&1 || true
}
if [[ "$CI" == "1" ]]; then
  trap cleanup EXIT
else
  trap cleanup INT TERM EXIT
fi

tmux new-session -d -s "$SESSION" -n daemon "cd '$ROOT' && '$BIN' --socket '$SOCKET' serve --log-dir '$LOG_DIR' 2>&1 | tee '$ARTIFACTS/daemon-pane.log'"
tmux set-option -t "$SESSION" remain-on-exit on >/dev/null
tmux set-window-option -t "$SESSION" remain-on-exit on >/dev/null
DAEMON_PANE="$(tmux display-message -p -t "$SESSION:daemon" '#{pane_id}')"
HTTP_PANE="$(tmux split-window -P -F '#{pane_id}' -t "$SESSION:daemon" -h "cd '$ROOT' && '$BIN' serve-http --addr '$HTTP_ADDR' --log-dir '$HTTP_LOG_DIR' 2>&1 | tee '$ARTIFACTS/http-pane.log'")"
OBSERVER_PANE="$(tmux new-window -P -F '#{pane_id}' -t "$SESSION" -n observer "for i in \$(seq 1 $TIMEOUT_SECONDS); do date; '$BIN' --socket '$SOCKET' list || true; [ -f '$ARTIFACTS/done' ] && break; sleep 1; done 2>&1 | tee '$ARTIFACTS/observer-pane.log'")"
DRIVER_PANE="$(tmux new-window -P -F '#{pane_id}' -t "$SESSION" -n driver "env ROOT='$ROOT' BIN='$BIN' ARTIFACTS='$ARTIFACTS' REPO='$REPO' LOG_DIR='$LOG_DIR' MCP_LOG_DIR='$MCP_LOG_DIR' SOCKET='$SOCKET' HTTP_URL='$HTTP_URL' DAEMON_PANE='$DAEMON_PANE' DRIVER='$DRIVER' '$DRIVER_RUNNER'; echo driver-pane-held-for-capture; while [ ! -f '$ARTIFACTS/cleanup' ]; do sleep 1; done")"

for _ in $(seq 1 "$TIMEOUT_SECONDS"); do
  if [[ -f "$ARTIFACTS/done" ]]; then
    break
  fi
  if [[ -f "$ARTIFACTS/report.md" ]] && ! tmux list-panes -t "$SESSION:driver" -F '#{pane_current_command}' | grep -q script; then
    break
  fi
  sleep 1
done

tmux capture-pane -p -t "$DRIVER_PANE" -S -3000 >"$ARTIFACTS/tmux-driver-pane.txt" || true
tmux capture-pane -p -t "$OBSERVER_PANE" -S -3000 >"$ARTIFACTS/tmux-observer-pane.txt" || true
tmux capture-pane -p -t "$DAEMON_PANE" -S -3000 >"$ARTIFACTS/tmux-daemon-pane.txt" || true
tmux capture-pane -p -t "$HTTP_PANE" -S -3000 >"$ARTIFACTS/tmux-http-pane.txt" || true
tmux list-panes -a -F '#{session_name}:#{window_name}.#{pane_index} #{pane_pid} #{pane_current_command}' >"$ARTIFACTS/tmux-panes.txt" || true
touch "$ARTIFACTS/cleanup"

if [[ ! -f "$ARTIFACTS/done" ]]; then
  echo "tmux E2E timed out or failed; artifacts: $ARTIFACTS" >&2
  sed -n '1,200p' "$ARTIFACTS/driver.log" >&2 || true
  exit 1
fi

grep -F "agent-pty-e2e-complete" "$ARTIFACTS/tmux-driver-pane.txt" >/dev/null
grep -F "Unix CLI: ok" "$ARTIFACTS/report.md" >/dev/null
grep -F "HTTP: ok" "$ARTIFACTS/report.md" >/dev/null
grep -F "MCP: ok" "$ARTIFACTS/report.md" >/dev/null
grep -F "human attach: ok" "$ARTIFACTS/report.md" >/dev/null
grep -F "policy approval: ok" "$ARTIFACTS/report.md" >/dev/null
grep -F "semantic screen: ok" "$ARTIFACTS/report.md" >/dev/null
grep -F "tmux reconnect: ok" "$ARTIFACTS/report.md" >/dev/null
grep -F "daemon restart replay: ok" "$ARTIFACTS/report.md" >/dev/null
grep -F "fork comparison: ok" "$ARTIFACTS/report.md" >/dev/null
grep -F "tests passed" "$ARTIFACTS/proofs/fix-a.proof.md" >/dev/null
grep -F "Commands Run" "$ARTIFACTS/proofs/fix-a.proof.html" >/dev/null

echo "$ARTIFACTS"
