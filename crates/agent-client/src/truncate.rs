/// Truncate a string to at most `max_bytes` bytes, ensuring the slice ends on a
/// valid UTF-8 character boundary.
///
/// Use this whenever you need a bounded-length subslice of a `&str` for display
/// or logging — raw byte slicing (`&s[..n]`) panics if `n` falls inside a
/// multi-byte character (e.g. an em dash `'—'` which is 3 bytes).
pub fn truncate_str(s: &str, max_bytes: usize) -> &str {
    let end = s
        .char_indices()
        .take_while(|(i, c)| *i + c.len_utf8() <= max_bytes)
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_within_limit() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn ascii_at_limit() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn ascii_beyond_limit() {
        assert_eq!(truncate_str("hello world", 5), "hello");
    }

    #[test]
    fn multibyte_inside_char() {
        // em dash '—' is 3 bytes (0xE2 0x80 0x94), so byte 5 is mid-character
        let s = "abc—def";
        assert_eq!(truncate_str(s, 5), "abc");
    }

    #[test]
    fn multibyte_at_boundary() {
        let s = "abc—def";
        // '—' occupies bytes 3..6, so truncating to exactly 6 includes it
        assert_eq!(truncate_str(s, 6), "abc—");
    }

    #[test]
    fn empty_string() {
        assert_eq!(truncate_str("", 100), "");
    }

    #[test]
    fn zero_max_bytes() {
        assert_eq!(truncate_str("hello", 0), "");
    }
}
