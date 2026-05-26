# Release Checklist

This project is intentionally conservative about releases. The supported install
path today is `cargo install --git https://github.com/evalops/agent-pty`.

Before tagging a release:

1. Run the local verification matrix:

   ```bash
   cargo fmt -- --check
   cargo clippy --all-targets --all-features -- -D warnings
   bash -n scripts/e2e-tmux.sh
   python3 -m py_compile examples/http_agent.py examples/mcp_stdio_agent.py
   cargo test
   cargo build --release
   cargo test --test e2e_tmux -- --ignored --nocapture
   ```

2. Run `agent-pty demo` from the release binary and inspect the generated proof
   Markdown and HTML.
3. Confirm GitHub Actions is green on `main`.
4. Update `Cargo.toml` version and release notes.
5. Tag from `main`:

   ```bash
   git tag v0.1.0
   git push origin v0.1.0
   ```

Do not publish to crates.io until the project has an explicit license and crate
publishing policy.
