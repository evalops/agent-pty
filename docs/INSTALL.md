# Installing agent-pty

`agent-pty` is currently distributed from the public GitHub repository.

## Requirements

- Rust 1.85 or newer
- `/bin/sh`
- `tmux` for reconnect sessions and the deep smoke harness
- `python3` for the runnable HTTP/MCP examples

## Install

```bash
cargo install --git https://github.com/evalops/agent-pty
```

Verify the binary:

```bash
agent-pty doctor
agent-pty demo
```

The demo prints the workspace, report, proof Markdown, proof HTML, and event-log
paths for a disposable run.

## Build From Source

```bash
git clone https://github.com/evalops/agent-pty
cd agent-pty
cargo build --release
./target/release/agent-pty demo
```

## Upgrade

```bash
cargo install --git https://github.com/evalops/agent-pty --force
```

## Local Verification

Before trusting a local build for agent work, run:

```bash
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo test --test e2e_tmux -- --ignored --nocapture
```
