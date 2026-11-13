//! Exact, fallible lossy text copies shared by native discovery snapshots.

use std::collections::TryReserveError;

/// Borrowed SDK text with the exact size of its lossy UTF-8 representation.
/// Callers charge `len()` to their snapshot budget before asking for a copy.
pub(crate) struct LossyText<'a> {
    bytes: &'a [u8],
    len: usize,
}

impl<'a> LossyText<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        let len = bytes
            .utf8_chunks()
            .map(|chunk| chunk.valid().len() + usize::from(!chunk.invalid().is_empty()) * 3)
            .sum();
        Self { bytes, len }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn copy(&self) -> Result<String, TryReserveError> {
        let mut text = String::new();
        text.try_reserve_exact(self.len)?;
        for chunk in self.bytes.utf8_chunks() {
            text.push_str(chunk.valid());
            if !chunk.invalid().is_empty() {
                text.push('\u{fffd}');
            }
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_copies_match_standard_utf8_replacement() {
        let check = |bytes: &[u8]| {
            let expected = String::from_utf8_lossy(bytes);
            let text = LossyText::new(bytes);
            assert_eq!(text.len(), expected.len());
            assert_eq!(text.copy().unwrap(), expected);
        };
        check(b"");
        // Exhaust every two-byte prefix, including continuation bytes,
        // overlong sequences and truncated multibyte characters.
        for first in 0..=u8::MAX {
            for second in 0..=u8::MAX {
                check(&[first, second]);
            }
        }
        for text in ["ASCII", "caf\u{e9}", "\u{20ac}", "\u{1f980}"] {
            for end in 0..=text.len() {
                check(&text.as_bytes()[..end]);
            }
        }
        for bytes in [
            &b"\xed\xa0\x80"[..],                  // Surrogate.
            &b"\xf4\x90\x80\x80"[..],              // Beyond Unicode's maximum.
            &b"\xe2\x82A\xff\xf0\x9f\xa6\x80"[..], // Mixed valid and malformed chunks.
        ] {
            check(bytes);
        }
    }
}
