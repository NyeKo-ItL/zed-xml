//! Position encoding negotiated with the client (LSP 3.17
//! `general.positionEncodings` / `positionEncoding`) and the conversions
//! between UTF-8 byte offsets and LSP positions.
//!
//! Everything inside the server is a UTF-8 byte offset; the `character` of an
//! LSP position counts UTF-8 code units, UTF-16 code units (the default and
//! the only encoding every client supports) or Unicode scalar values
//! (UTF-32), as negotiated at initialization. The encoding is a property of
//! the connection: it is recorded per thread ([`PositionEncoding::install`])
//! by the request loop and the diagnostics worker, so that the conversion
//! helpers ([`crate::position_at`], [`crate::offset_at`],
//! [`crate::selection::LineIndex`]) need no extra parameter.
//!
//! A position never designates the point between the `\r` and the `\n` of a
//! CRLF, and a `character` beyond the end of its line means the end of the
//! line (before its line break), as the specification requires.

use std::cell::Cell;

use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PositionEncoding {
    Utf8,
    #[default]
    Utf16,
    Utf32,
}

thread_local! {
    static CURRENT: Cell<PositionEncoding> = const { Cell::new(PositionEncoding::Utf16) };
}

impl PositionEncoding {
    /// Chooses the encoding from the client's `general.positionEncodings`
    /// (in order of preference): UTF-8 when offered, the server's native
    /// offsets, then UTF-16, then UTF-32. Without the capability, or with no
    /// known value, UTF-16 as the specification mandates.
    pub(crate) fn negotiate(initialize_params: &Value) -> Self {
        let Some(offered) = initialize_params
            .pointer("/capabilities/general/positionEncodings")
            .and_then(Value::as_array)
        else {
            return Self::Utf16;
        };
        let offers = |name: &str| offered.iter().any(|value| value.as_str() == Some(name));
        [Self::Utf8, Self::Utf16, Self::Utf32]
            .into_iter()
            .find(|encoding| offers(encoding.as_str()))
            .unwrap_or(Self::Utf16)
    }

    /// Name used in the protocol (`PositionEncodingKind`).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Utf16 => "utf-16",
            Self::Utf32 => "utf-32",
        }
    }

    /// Encoding of the current thread (UTF-16 until [`Self::install`]).
    pub(crate) fn current() -> Self {
        CURRENT.with(Cell::get)
    }

    /// Makes `self` the encoding of the positions converted by this thread.
    pub(crate) fn install(self) {
        CURRENT.with(|current| current.set(self));
    }

    /// Length of `text` in code units of this encoding.
    pub(crate) fn len(self, text: &str) -> usize {
        match self {
            Self::Utf8 => text.len(),
            Self::Utf16 => text.chars().map(char::len_utf16).sum(),
            Self::Utf32 => text.chars().count(),
        }
    }

    /// Byte offset in `line` (a line without its line break) of the
    /// `character`-th code unit; the end of the line beyond it. A position
    /// inside a character (a UTF-8 continuation byte, a UTF-16 low surrogate)
    /// designates the end of that character.
    pub(crate) fn offset_in_line(self, line: &str, character: usize) -> usize {
        if self == Self::Utf8 {
            let mut offset = character.min(line.len());
            while !line.is_char_boundary(offset) {
                offset += 1;
            }
            return offset;
        }
        let mut units = 0;
        for (index, value) in line.char_indices() {
            if units >= character {
                return index;
            }
            units += match self {
                Self::Utf16 => value.len_utf16(),
                _ => 1,
            };
        }
        line.len()
    }
}

/// Largest character boundary of `source` at or before `offset`, outside a
/// CRLF.
pub(crate) fn floor_position_offset(source: &str, offset: usize) -> usize {
    let mut offset = offset.min(source.len());
    while !source.is_char_boundary(offset) {
        offset -= 1;
    }
    if offset > 0
        && source.as_bytes()[offset - 1] == b'\r'
        && source.as_bytes().get(offset) == Some(&b'\n')
    {
        offset -= 1;
    }
    offset
}

/// Content of the line starting at `start` (up to its `\n`, the `\r` of a
/// CRLF removed; a `\r` ending the document is an ordinary character, as
/// [`position_at`](crate::position_at) counts it).
pub(crate) fn line_content(source: &str, start: usize) -> &str {
    let rest = source.get(start..).unwrap_or_default();
    match rest.find('\n') {
        Some(end) => rest[..end].strip_suffix('\r').unwrap_or(&rest[..end]),
        None => rest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_carriage_return_ending_the_document_is_a_character() {
        let source = "<a/>\r\n<b/>\r";
        let end = source.len();
        let position = crate::position_at(source, end);
        assert_eq!(position, json!({"line": 1, "character": 5}));
        assert_eq!(crate::offset_at(source, 1, 5), end);
        assert_eq!(crate::offset_at(source, 0, 9), 4);
    }

    #[test]
    fn negotiates_the_client_preference_with_utf16_as_fallback() {
        let params = |encodings: Value| json!({"capabilities": {"general": {"positionEncodings": encodings}}});
        assert_eq!(
            PositionEncoding::negotiate(&json!({})),
            PositionEncoding::Utf16
        );
        assert_eq!(
            PositionEncoding::negotiate(&params(json!(["utf-16", "utf-8"]))),
            PositionEncoding::Utf8
        );
        assert_eq!(
            PositionEncoding::negotiate(&params(json!(["utf-32", "utf-16"]))),
            PositionEncoding::Utf16
        );
        assert_eq!(
            PositionEncoding::negotiate(&params(json!(["utf-32"]))),
            PositionEncoding::Utf32
        );
        assert_eq!(
            PositionEncoding::negotiate(&params(json!(["latin1", 3]))),
            PositionEncoding::Utf16
        );
    }

    #[test]
    fn measures_and_locates_characters_in_each_encoding() {
        let line = "a\u{e9}\u{1D11E}b";
        assert_eq!(PositionEncoding::Utf8.len(line), 8);
        assert_eq!(PositionEncoding::Utf16.len(line), 5);
        assert_eq!(PositionEncoding::Utf32.len(line), 4);
        for (encoding, character, offset) in [
            (PositionEncoding::Utf8, 1, 1),
            (PositionEncoding::Utf8, 2, 3),
            (PositionEncoding::Utf8, 4, 7),
            (PositionEncoding::Utf8, 99, 8),
            (PositionEncoding::Utf16, 2, 3),
            (PositionEncoding::Utf16, 3, 7),
            (PositionEncoding::Utf16, 4, 7),
            (PositionEncoding::Utf32, 3, 7),
            (PositionEncoding::Utf32, 4, 8),
        ] {
            assert_eq!(
                encoding.offset_in_line(line, character),
                offset,
                "{encoding:?} {character}"
            );
        }
    }

    #[test]
    fn keeps_offsets_out_of_crlf_line_breaks() {
        let source = "a\r\nb\u{e9}";
        assert_eq!(floor_position_offset(source, 2), 1);
        assert_eq!(floor_position_offset(source, 3), 3);
        assert_eq!(floor_position_offset(source, 5), 4);
        assert_eq!(floor_position_offset(source, 99), source.len());
        assert_eq!(line_content(source, 0), "a");
        assert_eq!(line_content(source, 3), "b\u{e9}");
        assert_eq!(line_content(source, 99), "");
    }
}
