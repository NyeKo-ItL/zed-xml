use std::fs;

use super::*;
use crate::resource::{ResourceError, read_text_file};

fn utf16(text: &str, little_endian: bool, bom: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    let units = bom
        .then_some(0xFEFF)
        .into_iter()
        .chain(text.encode_utf16())
        .collect::<Vec<u16>>();
    for unit in units {
        if little_endian {
            bytes.extend(unit.to_le_bytes());
        } else {
            bytes.extend(unit.to_be_bytes());
        }
    }
    bytes
}

#[test]
fn strips_a_utf8_byte_order_mark() {
    assert_eq!(strip_bom("\u{FEFF}<a/>"), ("<a/>", 3));
    assert_eq!(strip_bom("<a/>"), ("<a/>", 0));
    assert_eq!(
        decode_bytes(b"\xEF\xBB\xBF<a>\xC3\xA9</a>").as_deref(),
        Some("<a>é</a>")
    );
    assert_eq!(decode_bytes(b"<a/>").as_deref(), Some("<a/>"));
}

#[test]
fn decodes_utf16_with_and_without_byte_order_mark() {
    let text = "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\n<a>é 𝄞</a>";
    for little_endian in [true, false] {
        for bom in [true, false] {
            assert_eq!(
                decode_bytes(&utf16(text, little_endian, bom)).as_deref(),
                Some(text),
                "little endian {little_endian}, bom {bom}"
            );
        }
    }
    // Unpaired surrogate or odd length: not UTF-16.
    assert_eq!(decode_bytes(&[0xFF, 0xFE, 0x00, 0xD8]), None);
    assert_eq!(decode_bytes(&[0xFF, 0xFE, b'<']), None);
}

#[test]
fn decodes_declared_latin1_and_rejects_invalid_utf8() {
    assert_eq!(
        decode_bytes(b"<?xml version='1.0' encoding='ISO-8859-1'?><a>\xE9</a>").as_deref(),
        Some("<?xml version='1.0' encoding='ISO-8859-1'?><a>é</a>")
    );
    assert_eq!(decode_bytes(b"<a>\xE9</a>"), None);
    assert_eq!(decode_bytes(b""), Some(String::new()));
}

#[test]
fn reads_files_and_reports_undecodable_content() {
    let directory = std::env::temp_dir().join(format!("xml-core-text-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("utf16.xsd");
    fs::write(&path, utf16("<schema/>", true, true)).unwrap();
    assert_eq!(read_text_file(&path, 100).as_deref(), Ok("<schema/>"));
    fs::write(&path, b"<a>\xFF</a>").unwrap();
    assert_eq!(read_text_file(&path, 100), Err(ResourceError::NotUtf8));
    let _ = fs::remove_dir_all(&directory);
}
