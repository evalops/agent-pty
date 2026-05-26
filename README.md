# agent-pty

Agent terminal substrate: persistent PTY sessions with semantic screen reads,
predicate waits, replayable evidence, policy gates, proof bundles, forks, and
agent-compatible transports.

This is the layer below agent orchestration frameworks. It gives coding and ops
agents durable hands in a terminal: start a session, send input, observe the
screen, wait for real conditions, record every mutation, and let a human or
another agent audit what happened.

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
- HTTP/JSON request surface.
- MCP-compatible stdio JSON-RPC tool surface.
- OTEL-style JSONL trace events for daemon requests.

## CLI

```bash
agent-pty serve --socket ~/.agent-pty.sock
agent-pty serve-http --addr 127.0.0.1:4319
agent-pty mcp-stdio

agent-pty new --repo ~/src/evalops/platform --name codex-1
agent-pty send codex-1 "cargo test"
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

The daemon accepts newline-delimited JSON on the configured Unix socket. Each
connection carries one request and receives one response.

```json
{"op":"new","id":"codex-1","repo":"/repo","shell":"/bin/sh","rows":24,"cols":80,"env":{}}
{"op":"send","id":"codex-1","text":"cargo test","enter":true}
{"op":"wait","id":"codex-1","until":"regex:finished in","timeout_ms":30000}
{"op":"screen","id":"codex-1"}
{"op":"list"}
{"op":"proof","id":"codex-1"}
{"op":"fork","id":"codex-1","name":"repair-b","copy_worktree":true}
{"op":"replay","id":"codex-1"}
{"op":"trace_path"}
{"op":"kill","id":"codex-1"}
```

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
