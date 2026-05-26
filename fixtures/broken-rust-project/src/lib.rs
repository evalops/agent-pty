pub fn normalize_name(input: &str) -> String {
    input.to_string()
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
