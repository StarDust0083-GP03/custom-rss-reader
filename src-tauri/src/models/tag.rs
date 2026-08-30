/// Maximum length for a normalized tag name.
pub const MAX_TAG_NAME_CHARS: usize = 64;
/// Article classification returns at most this many subjects.
pub const MAX_TAGS_PER_ITEM: usize = 3;

/// Normalize a user or model-provided tag to lowercase ASCII snake_case.
///
/// Returns `None` for empty or overlong names. Punctuation is treated as a
/// separator so inputs such as `machine-learning` and `machine learning`
/// converge on the same stored name.
pub fn normalize_tag(input: &str) -> Option<String> {
    let mut out = String::new();
    let mut separator = false;

    for ch in input.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            if separator && !out.is_empty() {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }

    if out.is_empty() || out.chars().count() > MAX_TAG_NAME_CHARS {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_tag;

    #[test]
    fn normalizes_names_to_snake_case() {
        assert_eq!(
            normalize_tag(" Machine Learning ").as_deref(),
            Some("machine_learning")
        );
        assert_eq!(normalize_tag("AI/ML").as_deref(), Some("ai_ml"));
        assert_eq!(
            normalize_tag("deep--learning").as_deref(),
            Some("deep_learning")
        );
    }

    #[test]
    fn rejects_empty_and_overlong_names() {
        assert_eq!(normalize_tag("---"), None);
        assert_eq!(normalize_tag(&"x".repeat(65)), None);
    }
}
