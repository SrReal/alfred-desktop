pub const SYSTEM_PROMPT: &str = concat!(
    include_str!("persona_identity.md"),
    include_str!("persona_rules.md")
);

#[cfg(test)]
mod tests {
    #[test]
    fn edition_prompt_keeps_shared_permission_and_tool_rules() {
        let rules = include_str!("persona_rules.md");
        assert!(!rules.trim().is_empty());
        assert!(super::SYSTEM_PROMPT.ends_with(rules));
        assert!(rules.contains("permisos"));
    }
}
