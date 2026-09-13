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

/// Serialize a vector as little-endian `f32` bytes for BLOB storage.
pub fn encode_embedding(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

/// Inverse of [`encode_embedding`].
///
/// Returns `None` for an empty or partial trailing value, so a truncated row
/// degrades to "no embedding" instead of decoding into garbage that would
/// silently distort similarity.
pub fn decode_embedding(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return None;
    }
    Some(
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| f32::from_le_bytes(*chunk))
            .collect(),
    )
}

/// Text handed to the sentence encoder for a tag name.
///
/// Storage stays `snake_case` (it is the canonical, comparable form), but the
/// encoder was trained on prose: `machine_learning` is out of distribution for
/// its tokenizer, and separating the words measurably improves neighbourhoods.
/// Measured on a 358-tag library, names merged at cosine ≥ 0.85 went from 10
/// to 37 purely from this change.
pub fn embedding_text(name: &str) -> String {
    name.replace(['_', '-'], " ")
}

/// Deterministic fingerprint of an ordered set of strings.
///
/// Where a caller must prove it is updating exactly the state it read, it
/// hashes that state and compares before writing, so a second window or a
/// background job cannot make a stale preview commit anyway.
pub fn state_hash(parts: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        // Separator, so ["ab", "c"] and ["a", "bc"] cannot collide.
        hasher.update([0x1f]);
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::{embedding_text, normalize_tag};

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

    #[test]
    fn embedding_bytes_round_trip_and_reject_partial_rows() {
        use super::{decode_embedding, encode_embedding};

        let vector = vec![0.0f32, 1.5, -2.25, f32::MIN_POSITIVE];
        let bytes = encode_embedding(&vector);
        assert_eq!(bytes.len(), vector.len() * 4);
        assert_eq!(decode_embedding(&bytes), Some(vector));

        // A truncated BLOB must not decode into a shorter, wrong vector.
        assert_eq!(decode_embedding(&bytes[..bytes.len() - 1]), None);
        assert_eq!(decode_embedding(&[]), None);
    }

    #[test]
    fn embedding_text_separates_words_without_changing_storage() {
        assert_eq!(embedding_text("machine_learning"), "machine learning");
        assert_eq!(embedding_text("ai-security"), "ai security");
        // Single tokens and CJK names pass through untouched.
        assert_eq!(embedding_text("c"), "c");
        assert_eq!(embedding_text("机器学习"), "机器学习");
        // Idempotent, so the encoder-input cache cannot double-transform.
        assert_eq!(embedding_text(&embedding_text("a_b")), "a b");
    }
}
