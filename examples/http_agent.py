#!/usr/bin/env python3
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def post(url, payload):
    data = json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(
        url,
        data=data,
        headers={"content-type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=10) as response:
        body = json.loads(response.read().decode("utf-8"))
    if not body.get("ok"):
        raise RuntimeError(body.get("error", "agent-pty request failed"))
    return body["data"]


def wait_for_http(url):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        try:
            post(url, {"op": "list"})
            return
        except (urllib.error.URLError, RuntimeError, ConnectionError):
            time.sleep(0.05)
    raise RuntimeError(f"HTTP endpoint did not become ready: {url}")


def main():
    binary = os.environ.get("AGENT_PTY_BIN", "agent-pty")
    root = os.environ.get("AGENT_PTY_ROOT") or tempfile.mkdtemp(
        prefix="agent-pty-http-example."
    )
    log_dir = os.path.join(root, "logs")
    workspace = os.path.join(root, "workspace")
    os.makedirs(log_dir, exist_ok=True)
    os.makedirs(workspace, exist_ok=True)
    port = int(os.environ.get("AGENT_PTY_HTTP_PORT", free_port()))
    url = f"http://127.0.0.1:{port}/request"

    server = subprocess.Popen(
        [binary, "serve-http", "--addr", f"127.0.0.1:{port}", "--log-dir", log_dir],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        wait_for_http(url)
        session = "http-agent"
        post(
            url,
            {
                "op": "new",
                "id": session,
                "repo": workspace,
                "shell": "/bin/sh",
                "rows": 24,
                "cols": 100,
                "env": {},
            },
        )
        post(
            url,
            {
                "op": "send",
                "id": session,
                "text": "printf 'http-agent-ok\\n' > agent-output.txt && printf 'http-agent-ok\\n'",
                "enter": True,
            },
        )
        post(
            url,
            {
                "op": "wait",
                "id": session,
                "until": "http-agent-ok",
                "timeout_ms": 10000,
            },
        )
        screen = post(url, {"op": "screen", "id": session})
        proof = post(url, {"op": "proof", "id": session})
        post(url, {"op": "kill", "id": session})
        print(
            json.dumps(
                {
                    "transport": "http",
                    "workspace": workspace,
                    "command": screen["data"]["semantic"]["command"],
                    "proof_html_path": proof["data"]["html_path"],
                },
                indent=2,
            )
        )
    finally:
        server.terminate()
        try:
            server.wait(timeout=3)
        except subprocess.TimeoutExpired:
            server.kill()
            server.wait()


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"error: {error}", file=sys.stderr)
        sys.exit(1)
