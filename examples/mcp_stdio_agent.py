#!/usr/bin/env python3
import json
import os
import subprocess
import sys
import tempfile


def call(process, message):
    process.stdin.write(json.dumps(message) + "\n")
    process.stdin.flush()
    line = process.stdout.readline()
    if not line:
        raise RuntimeError("agent-pty MCP server closed stdout")
    response = json.loads(line)
    if "error" in response:
        raise RuntimeError(response["error"]["message"])
    return response["result"]


def tool(process, request_id, name, arguments):
    return call(
        process,
        {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        },
    )["structuredContent"]


def main():
    binary = os.environ.get("AGENT_PTY_BIN", "agent-pty")
    root = os.environ.get("AGENT_PTY_ROOT") or tempfile.mkdtemp(
        prefix="agent-pty-mcp-example."
    )
    log_dir = os.path.join(root, "logs")
    workspace = os.path.join(root, "workspace")
    os.makedirs(log_dir, exist_ok=True)
    os.makedirs(workspace, exist_ok=True)

    process = subprocess.Popen(
        [binary, "mcp-stdio", "--log-dir", log_dir],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        initialize = call(
            process,
            {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}},
        )
        tools = call(process, {"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
        session = "mcp-agent"
        tool(
            process,
            3,
            "terminal.new",
            {
                "id": session,
                "repo": workspace,
                "shell": "/bin/sh",
                "rows": 24,
                "cols": 100,
                "env": {},
            },
        )
        tool(
            process,
            4,
            "terminal.send",
            {
                "id": session,
                "text": "printf 'mcp-agent-ok\\n' > agent-output.txt && printf 'mcp-agent-ok\\n'",
                "enter": True,
            },
        )
        tool(
            process,
            5,
            "terminal.wait",
            {"id": session, "until": "mcp-agent-ok", "timeout_ms": 10000},
        )
        proof = tool(process, 6, "terminal.proof", {"id": session})
        tool(process, 7, "terminal.kill", {"id": session})
        print(
            json.dumps(
                {
                    "transport": "mcp",
                    "server": initialize["serverInfo"]["name"],
                    "tool_count": len(tools["tools"]),
                    "workspace": workspace,
                    "proof_html_path": proof["data"]["html_path"],
                },
                indent=2,
            )
        )
    finally:
        process.stdin.close()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"error: {error}", file=sys.stderr)
        sys.exit(1)
