# agent-pty

Agent terminal substrate: persistent PTY sessions with semantic screen reads,
predicate waits, replayable evidence, policy gates, proof bundles, forks, and
agent-compatible transports.

This is the layer below agent orchestration frameworks. It gives coding and ops
agents durable hands in a terminal: start a session, send input, observe the
screen, wait for real conditions, record every mutation, and let a human or
another agent audit what happened.

## Five-Minute Quickstart

Install from the public repo:

```bash
cargo install --git https://github.com/evalops/agent-pty
```

Run the self-contained demo first. It does not require a daemon:

```bash
agent-pty demo
```

The demo creates a disposable git repo under `~/.agent-pty/demos`, opens a real
PTY session, runs a failing check, fixes the repo from inside the session, waits
for the passing check, and writes proof artifacts. The output points to:

- the demo workspace
- a Markdown report
- the generated proof bundle
- the append-only event log

For automation, use JSON output:

```bash
agent-pty demo --json
```

After that, try the persistent daemon flow:

```bash
agent-pty doctor
agent-pty serve --socket ~/.agent-pty.sock
```

In another terminal:

```bash
agent-pty status
agent-pty new --repo "$PWD" --name first-run
agent-pty send first-run "printf 'hello from agent-pty\n'"
agent-pty wait first-run --until "hello from agent-pty"
agent-pty screen first-run --format markdown
agent-pty attach first-run --read-only --timeout 2s
agent-pty proof first-run
agent-pty kill first-run
agent-pty stop
```

## What Works Now

- Persistent PTY sessions through `portable-pty`.
- `vt100` screen snapshots with spans for prompts, output, error-looking lines,
  URLs, and file paths.
- Predicate waits for literal/regex text, prompt return, idle output, and
  process exit.
- Append-only JSONL evidence logs for actions and observations.
- Git status/diff snapshots around mutating actions.
- Durable session index with workspace, shell, env, pid, dimensions, start time,
  active state, and event-log path.
- Process snapshots in observations, including root pid and child processes.
- Proof bundles as JSON and Markdown artifacts.
- Git worktree-backed forks for parallel repair attempts.
- Initial policy gate with audited denial events.
- Unix-socket JSON daemon.
- Daemon lifecycle diagnostics with `doctor`, `status`, and `stop`.
- Human attach over the Unix socket with live output and optional stdin control.
- Tmux-backed sessions that can reconnect to live processes after daemon restart.
- HTTP/JSON request surface.
- MCP-compatible stdio JSON-RPC tool surface.
- OTEL-style JSONL trace events for daemon requests.

## CLI

```bash
agent-pty demo

agent-pty doctor
agent-pty serve --socket ~/.agent-pty.sock
agent-pty status
agent-pty stop
agent-pty serve-http --addr 127.0.0.1:4319
agent-pty mcp-stdio

agent-pty new --repo ~/src/evalops/platform --name codex-1
agent-pty new --repo ~/src/evalops/platform --name durable-1 --backend tmux
agent-pty send codex-1 "cargo test"
agent-pty attach codex-1
agent-pty screen codex-1 --format markdown
agent-pty wait codex-1 --until "finished in"
agent-pty wait codex-1 --until "idle:2s"
agent-pty list
agent-pty proof codex-1
agent-pty replay codex-1 --json
agent-pty fork codex-1 --new-name repair-b --copy-worktree
agent-pty trace-path
agent-pty kill codex-1
```

## Unix Socket Protocol

The daemon accepts newline-delimited JSON on the configured Unix socket. Most
connections carry one request and receive one response. `attach` is the
exception: after the JSON handshake, the socket switches into a bidirectional
terminal stream.

```json
{"op":"new","id":"codex-1","repo":"/repo","shell":"/bin/sh","rows":24,"cols":80,"env":{}}
{"op":"new","id":"durable-1","repo":"/repo","shell":"/bin/sh","rows":24,"cols":80,"env":{},"backend":"tmux"}
{"op":"send","id":"codex-1","text":"cargo test","enter":true}
{"op":"attach","id":"codex-1","read_only":false,"history_bytes":12000}
{"op":"wait","id":"codex-1","until":"regex:finished in","timeout_ms":30000}
{"op":"screen","id":"codex-1"}
{"op":"list"}
{"op":"proof","id":"codex-1"}
{"op":"fork","id":"codex-1","name":"repair-b","copy_worktree":true}
{"op":"replay","id":"codex-1"}
{"op":"trace_path"}
{"op":"kill","id":"codex-1"}
{"op":"shutdown"}
```

## Daemon Lifecycle

Use `doctor` before starting the daemon or when a user reports that the CLI
cannot connect:

```bash
agent-pty doctor
agent-pty doctor --json
```

Use `status` and `stop` for day-to-day lifecycle checks:

```bash
agent-pty status
agent-pty status --json
agent-pty stop
```

`status --json` succeeds even when the daemon is unreachable, returning
`running: false` with the connection diagnostic in `error`. That makes it safe
for scripts and agents to call without turning "not running" into an exception.

## Human Attach

Attach streams the session transcript and live output into the current terminal.
By default, stdin is forwarded into the PTY, so a human can answer prompts or
interrupt a process directly:

```bash
agent-pty attach codex-1
```

For observation-only use, pass `--read-only`:

```bash
agent-pty attach codex-1 --read-only
```

For smoke tests and scripts, `--timeout` exits automatically:

```bash
agent-pty attach codex-1 --read-only --timeout 2s
printf 'yes\n' | agent-pty attach codex-1 --timeout 1s
```

Attach uses a streaming Unix-socket handshake, not the one-request/one-response
JSON protocol. Each attach is recorded as evidence, and any bytes typed through
the attach channel are logged as normal `send_keys` actions.

## Session Backends

The default `pty` backend is an in-process portable PTY. It is fast and has the
most complete local state, but the live process dies with the daemon.

Use `--backend tmux` when a session must survive daemon restarts:

```bash
agent-pty new --repo "$PWD" --name durable-1 --backend tmux
agent-pty send durable-1 "npm test -- --watch"
agent-pty stop
agent-pty serve --socket ~/.agent-pty.sock
agent-pty screen durable-1
```

Tmux-backed sessions are persisted in `sessions.json` with their tmux session
name. A fresh daemon using the same log directory can reconnect to the live tmux
session for `send`, `screen`, `wait`, `attach`, `proof`, `replay`, and `kill`.

## HTTP Transport

`agent-pty serve-http` exposes the same request model over `POST /request`.

```bash
curl -sS http://127.0.0.1:4319/request \
  -d '{"op":"screen","id":"codex-1"}'
```

Responses use the same envelope as the Unix socket transport:

```json
{"ok":true,"data":{"type":"screen","data":{"rows":24,"cols":80,"text":"...","spans":[]}},"error":null}
```

## MCP Tools

`agent-pty mcp-stdio` exposes these MCP-compatible tools over stdio JSON-RPC:

- `terminal.new`
- `terminal.send`
- `terminal.screen`
- `terminal.wait`
- `terminal.kill`
- `terminal.replay`
- `terminal.list`
- `terminal.proof`
- `terminal.fork`

## Evidence Model

Every action and observation is written to the session JSONL log. A proof bundle
summarizes:

- commands run
- files changed
- latest git status and diff
- blocked policy actions
- nonzero exits
- screen tail
- event-log path
- generated JSON and Markdown artifact paths

## Policy Gate

The built-in policy blocks and audits these high-risk command families before
they reach the PTY:

- `rm -rf`
- `git push --force`
- `terraform apply`
- `kubectl delete`
- `vault write`
- `gh pr merge`

Policy denials become evidence events, so a run can prove that a dangerous
mutation was avoided rather than silently skipped.

## Local Verification

```bash
cargo fmt -- --check
cargo test
cargo build
```

## Deep Tmux E2E

The deepest local check is an ignored black-box test that drives the compiled
binary through real tmux panes and real daemon processes:

```bash
cargo test --test e2e_tmux -- --ignored --nocapture
```

The test invokes:

```bash
scripts/e2e-tmux.sh --ci --artifacts target/e2e-tmux/latest
```

It creates a temporary broken Rust repo from `fixtures/broken-rust-project`,
starts a detached tmux session, and exercises:

- Unix socket daemon and CLI commands
- HTTP daemon with `curl`
- MCP stdio JSON-RPC tools
- interactive prompt handling with `read`
- Python REPL interaction
- long-running output and idle waits
- ANSI/color and cursor-return output
- blocked policy commands and audited denial evidence
- git worktree forks for passing and failing repair attempts
- proof bundles for both repair attempts
- concurrent `replay` and `proof` clients hammering the same evidence log
- daemon restart followed by replay from durable logs
- tmux pane capture and `script` terminal transcript capture

Artifacts are written under `target/e2e-tmux/latest`:

- `report.md`
- `tmux-driver-pane.txt`
- `tmux-daemon-pane.txt`
- `tmux-http-pane.txt`
- `tmux-observer-pane.txt`
- `driver.typescript`
- `agent-pty-logs/*.jsonl`
- `agent-pty-logs/traces.jsonl`
- `proofs/*.proof.md`

This harness is intentionally slower and more operationally realistic than the
normal Cargo suite. It is the check to run before claiming that terminal,
transport, replay, proof, policy, and artifact behavior work end to end.
