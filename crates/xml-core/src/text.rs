//! Decoding of XML and DTD files read from disk (schemas, DTDs, catalogs,
//! workspace scan).
//!
//! Open documents come from the editor as Unicode text; files the server
//! reads itself are bytes. They are decoded following XML 1.0 appendix F:
//! a byte order mark (UTF-8, UTF-16 LE/BE) wins, then UTF-16 recognised from
//! the `<?` of an XML declaration without a mark, then the `encoding` of the
//! XML declaration for the single-byte encodings that need no table
//! (ISO-8859-1, US-ASCII), otherwise UTF-8. The byte order mark is removed,
//! as editors do when they open such a file, so offsets computed on the
//! decoded text match the positions of the editor.

/// The byte order mark, as it appears at the start of a decoded text.
pub const BYTE_ORDER_MARK: char = '\u{FEFF}';

/// `source` without its leading byte order mark, and the length in bytes of
/// the removed mark (0 or 3).
pub fn strip_bom(source: &str) -> (&str, usize) {
    match source.strip_prefix(BYTE_ORDER_MARK) {
        Some(rest) => (rest, BYTE_ORDER_MARK.len_utf8()),
        None => (source, 0),
    }
}

/// Encoding detected for a file's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    /// ISO-8859-1 (also used for a declared US-ASCII, its subset).
    Latin1,
}

/// Detects the encoding of `bytes` and the length of its byte order mark.
pub fn detect_encoding(bytes: &[u8]) -> (TextEncoding, usize) {
    match bytes {
        [0xEF, 0xBB, 0xBF, ..] => (TextEncoding::Utf8, 3),
        [0xFF, 0xFE, ..] => (TextEncoding::Utf16Le, 2),
        [0xFE, 0xFF, ..] => (TextEncoding::Utf16Be, 2),
        [0x00, b'<', 0x00, b'?', ..] => (TextEncoding::Utf16Be, 0),
        [b'<', 0x00, b'?', 0x00, ..] => (TextEncoding::Utf16Le, 0),
        _ => (declared_encoding(bytes).unwrap_or(TextEncoding::Utf8), 0),
    }
}

/// Decodes the bytes of an XML or DTD file, `None` when they are not valid
/// in the detected encoding.
pub fn decode_bytes(bytes: &[u8]) -> Option<String> {
    let (encoding, bom) = detect_encoding(bytes);
    let body = bytes.get(bom..)?;
    let text = match encoding {
        TextEncoding::Utf8 => std::str::from_utf8(body).ok()?.to_owned(),
        TextEncoding::Utf16Le | TextEncoding::Utf16Be => {
            if body.len() % 2 != 0 {
                return None;
            }
            let (pairs, _) = body.as_chunks::<2>();
            let units = pairs.iter().map(|&pair| match encoding {
                TextEncoding::Utf16Le => u16::from_le_bytes(pair),
                _ => u16::from_be_bytes(pair),
            });
            char::decode_utf16(units)
                .collect::<Result<String, _>>()
                .ok()?
        }
        TextEncoding::Latin1 => body.iter().map(|&byte| char::from(byte)).collect(),
    };
    // A UTF-8 mark encoded in UTF-16 is not a mark: only one is removed.
    Some(match text.strip_prefix(BYTE_ORDER_MARK) {
        Some(rest) if bom == 0 => rest.to_owned(),
        _ => text,
    })
}

/// Single-byte encoding declared by `<?xml ... encoding="..."?>`, when it
/// can be decoded without a table.
fn declared_encoding(bytes: &[u8]) -> Option<TextEncoding> {
    let head = bytes.get(..bytes.len().min(256))?;
    if !head.starts_with(b"<?xml") {
        return None;
    }
    let end = head.windows(2).position(|pair| pair == b"?>")?;
    let declaration = std::str::from_utf8(head.get(..end)?).ok()?;
    let value = declaration.split("encoding").nth(1)?;
    let value = value.trim_start().strip_prefix('=')?.trim_start();
    let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let label = value.get(1..)?.split(quote).next()?.trim();
    match label.to_ascii_lowercase().as_str() {
        "iso-8859-1" | "iso8859-1" | "latin1" | "l1" | "iso_8859-1" | "us-ascii" | "ascii" => {
            Some(TextEncoding::Latin1)
        }
        _ => None,
    }
}

/// The text with its character references and the five predefined entity
/// references replaced by the characters they stand for; other references
/// are kept.
pub fn decode_references(text: &str) -> String {
    if !text.contains('&') {
        return text.to_owned();
    }
    let mut decoded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        decoded.push_str(&rest[..start]);
        rest = &rest[start..];
        let replacement = rest.find(';').and_then(|end| {
            let body = &rest[1..end];
            let character = match body {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => body
                    .strip_prefix("#x")
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| body.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                    .and_then(char::from_u32),
            };
            character.map(|character| (character, end + 1))
        });
        match replacement {
            Some((character, length)) => {
                decoded.push(character);
                rest = &rest[length..];
            }
            None => {
                decoded.push('&');
                rest = &rest[1..];
            }
        }
    }
    decoded.push_str(rest);
    decoded
}

#[cfg(test)]
mod tests;
