# agent-pty

Agent terminal substrate: persistent PTY sessions with replayable evidence,
semantic screen reads, predicate waits, git-aware snapshots, and policy hooks.

This is the layer below agent orchestration frameworks. It gives coding and ops
agents durable hands in a terminal: start a session, send input, observe the
screen, wait for real conditions, record every mutation, and let a human attach
or audit what happened.

## MVP shape

- Spawn persistent PTY sessions.
- Send bytes/commands to a live session.
- Capture terminal output and expose current screen text plus structured spans.
- Wait for prompt, regex, exit, or idle predicates.
- Persist append-only JSONL event logs.
- Snapshot cwd, environment, process metadata, and git diff/status around
  mutating actions.
- Expose a CLI first, with daemon/socket and MCP compatibility planned as
  stable transport layers.

## Working CLI

```bash
agent-pty serve --socket ~/.agent-pty.sock
agent-pty new --repo ~/src/evalops/platform --name codex-1
agent-pty send codex-1 "cargo test"
agent-pty screen codex-1 --format markdown
agent-pty wait codex-1 --until "regex:finished in"
agent-pty replay codex-1 --since 10m
agent-pty kill codex-1
```

## Non-goals

- Not a shell replacement.
- Not a LangGraph clone.
- Not a browser automation layer.
- Not a terminal emulator UI.
