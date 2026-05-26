# agent-pty

Agent terminal substrate: persistent PTY sessions with replayable evidence,
semantic screen reads, predicate waits, git-aware snapshots, and policy hooks.

This is the layer below agent orchestration frameworks. It gives coding and ops
agents durable hands in a terminal: start a session, send input, observe the
screen, wait for real conditions, record every mutation, and let a human attach
or audit what happened.

## MVP shape

- Spawn persistent PTY sessions through `portable-pty`.
- Send bytes/commands to a live session.
- Capture terminal output through `vt100` and expose screen text plus structured
  spans for prompts, output, error-looking lines, URLs, and file paths.
- Wait for prompt, regex/text, exit, or idle predicates.
- Persist append-only JSONL event logs.
- Snapshot git diff/status around mutating actions when the workspace is a git
  repo.
- Expose a CLI and one-request-per-connection Unix JSON socket.
- Intercept obvious dangerous command families before they reach the PTY.

## Working CLI

```bash
agent-pty serve --socket ~/.agent-pty.sock
agent-pty new --repo ~/src/evalops/platform --name codex-1
agent-pty send codex-1 "cargo test"
agent-pty screen codex-1 --format markdown
agent-pty wait codex-1 --until "finished in"
agent-pty wait codex-1 --until "idle:2s"
agent-pty replay codex-1 --json
agent-pty kill codex-1
```

## Socket protocol

The daemon accepts newline-delimited JSON on the configured Unix socket. Each
connection carries one request and receives one response.

```json
{"op":"new","id":"codex-1","repo":"/repo","shell":"/bin/sh","rows":24,"cols":80,"env":{}}
{"op":"send","id":"codex-1","text":"cargo test","enter":true}
{"op":"wait","id":"codex-1","until":"regex:finished in","timeout_ms":30000}
{"op":"screen","id":"codex-1"}
{"op":"replay","id":"codex-1"}
{"op":"kill","id":"codex-1"}
```

## Policy gate

The first built-in policy denies high-risk commands until an approval/dry-run
flow exists:

- `rm -rf`
- `git push --force`
- `terraform apply`
- `kubectl delete`
- `vault write`
- `gh pr merge`

## Next layers

- HTTP/JSON wrapper around the same request handler.
- MCP compatibility tools for `terminal.new`, `terminal.send`,
  `terminal.screen`, `terminal.wait`, `terminal.kill`, and `terminal.replay`.
- OpenTelemetry spans around session actions and waits.
- Durable daemon restart/reconnect and session index persistence.
- Worktree/session fork support.

## Non-goals

- Not a shell replacement.
- Not a LangGraph clone.
- Not a browser automation layer.
- Not a terminal emulator UI.
