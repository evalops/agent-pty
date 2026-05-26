# agent-pty Integration Examples

These examples show how an agent or EvalOps runner can drive `agent-pty`
without embedding terminal logic.

Each script creates a disposable workspace by default, runs a real terminal
session, waits on an evidence-producing condition, writes proof artifacts, and
prints a JSON summary.

## CLI Daemon

```bash
AGENT_PTY_BIN=target/debug/agent-pty bash examples/cli_repair_agent.sh
```

This uses the Unix socket daemon and the same commands a coding agent would run:
`new`, `send`, `wait`, `screen`, `proof`, and `kill`.

## HTTP/JSON

```bash
AGENT_PTY_BIN=target/debug/agent-pty python3 examples/http_agent.py
```

This starts `agent-pty serve-http`, posts JSON requests to `/request`, and reads
the structured proof response.

## MCP Stdio

```bash
AGENT_PTY_BIN=target/debug/agent-pty python3 examples/mcp_stdio_agent.py
```

This starts `agent-pty mcp-stdio` and calls `terminal.*` tools over JSON-RPC
stdio, mirroring how an MCP host would invoke the terminal substrate.
