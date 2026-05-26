pub fn normalize_name(input: &str) -> String {
    input
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_spacing_and_case() {
        assert_eq!(normalize_name("  Hello Agent PTY  "), "hello-agent-pty");
        assert_eq!(normalize_name("Multiple   Spaces"), "multiple-spaces");
    }
}
