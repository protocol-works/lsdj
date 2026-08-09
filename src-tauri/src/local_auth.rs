//! Ephemeral capabilities for loopback-only child services.

/// Mint a 256-bit capability without touching process-global state or disk.
pub fn generate_capability() -> String {
    let bytes: [u8; 32] = rand::random();
    hex::encode(bytes)
}

/// Length-oblivious constant-time comparison for short secret byte strings.
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let max_len = left.len().max(right.len());
    let mut difference = left.len() ^ right.len();
    for index in 0..max_len {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0)
                ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_full_width_and_not_reused() {
        let first = generate_capability();
        let second = generate_capability();
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_ne!(first, second);
    }

    #[test]
    fn secret_comparison_rejects_wrong_values_and_lengths() {
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"samf"));
        assert!(!constant_time_eq(b"same", b"same-longer"));
    }
}
