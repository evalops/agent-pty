use std::fs;

#[test]
fn release_metadata_docs_and_ci_are_present() {
    let root = env!("CARGO_MANIFEST_DIR");

    let cargo = fs::read_to_string(format!("{root}/Cargo.toml")).unwrap();
    assert!(cargo.contains("rust-version = \"1.85\""));
    assert!(cargo.contains("repository = \"https://github.com/evalops/agent-pty\""));
    assert!(cargo.contains("publish = false"));
    assert!(
        cargo.contains("keywords = [\"pty\", \"terminal\", \"agents\", \"evidence\", \"tmux\"]")
    );

    let ci = fs::read_to_string(format!("{root}/.github/workflows/ci.yml")).unwrap();
    assert!(ci.contains("cargo fmt -- --check"));
    assert!(ci.contains("cargo clippy --all-targets --all-features -- -D warnings"));
    assert!(ci.contains("cargo test --test e2e_tmux -- --ignored --nocapture"));
    assert!(ci.contains("sudo apt-get install -y tmux"));

    let install = fs::read_to_string(format!("{root}/docs/INSTALL.md")).unwrap();
    assert!(install.contains("cargo install --git https://github.com/evalops/agent-pty"));
    assert!(install.contains("agent-pty demo"));

    let release = fs::read_to_string(format!("{root}/docs/RELEASE.md")).unwrap();
    assert!(release.contains("Confirm GitHub Actions is green on `main`"));
    assert!(release.contains("Do not publish to crates.io"));
}
