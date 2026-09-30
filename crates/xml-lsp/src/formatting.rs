//! `textDocument/formatting` and `textDocument/rangeFormatting`.
//!
//! LSP `FormattingOptions` (`tabSize`, `insertSpaces`,
//! `trimTrailingWhitespace`, `insertFinalNewline`, `trimFinalNewlines`) are
//! honoured; inserted line endings are the document's. The result is
//! returned as minimal edits (line-by-line difference) rather than a full
//! replacement, so that the editor keeps cursors outside the modified
//! lines.
//!
//! Range formatting follows LemMinX: the range is expanded to the enclosing
//! complete elements and only that region is reformatted, even when the
//! rest of the document is invalid.

use std::ops::Range;

use serde_json::{Value, json};
use xml_core::{FormatOptions, LineEnding, diff::diff_text, format_xml_range, format_xml_with};

use crate::{selection::LineIndex, settings::FormatSettings};

/// Formatting options: `xml.format.*` settings, then the request options
/// (which take precedence). Options missing from both keep the default
/// behaviour.
pub fn format_options(params: &Value, source: &str, settings: &FormatSettings) -> FormatOptions {
    let mut options = FormatOptions {
        line_ending: LineEnding::detect(source),
        ..FormatOptions::default()
    };
    settings.apply(&mut options);
    let Some(requested) = params.get("options") else {
        return options;
    };
    let flag = |name: &str| requested.get(name).and_then(Value::as_bool);
    if let Some(tab_size) = requested.get("tabSize").and_then(Value::as_u64) {
        options.tab_size = tab_size.min(16) as usize;
    }
    if let Some(insert_spaces) = flag("insertSpaces") {
        options.insert_spaces = insert_spaces;
    }
    if let Some(trim) = flag("trimTrailingWhitespace") {
        options.trim_trailing_whitespace = trim;
    }
    if let Some(insert) = flag("insertFinalNewline") {
        options.insert_final_newline = insert;
    }
    if let Some(trim) = flag("trimFinalNewlines") {
        options.trim_final_newlines = trim;
    }
    options
}

/// Edits (`TextEdit[]`) formatting the whole document, or `None` if the
/// document is invalid.
pub fn document_edits(source: &str, options: &FormatOptions) -> Option<Value> {
    let formatted = format_xml_with(source, options).ok()?;
    Some(text_edits(source, 0..source.len(), &formatted))
}

/// Edits (`TextEdit[]`) formatting the region enclosing `range`, or `None`
/// if it cannot be formatted safely.
pub fn range_edits(source: &str, range: Range<usize>, options: &FormatOptions) -> Option<Value> {
    let formatted = format_xml_range(source, range, options)?;
    Some(text_edits(source, formatted.range, &formatted.text))
}

/// Difference between `source[range]` and `replacement`, as `TextEdit[]`.
fn text_edits(source: &str, range: Range<usize>, replacement: &str) -> Value {
    let lines = LineIndex::new(source);
    let base = range.start;
    let edits = diff_text(&source[range], replacement)
        .into_iter()
        .map(|change| {
            json!({
                "range": {
                    "start": lines.position(source, base + change.range.start),
                    "end": lines.position(source, base + change.range.end),
                },
                "newText": change.text,
            })
        })
        .collect();
    Value::Array(edits)
}

/// Applies `TextEdit`s (UTF-16 positions) to `source`.
#[cfg(test)]
pub(crate) fn apply_edits(source: &str, edits: &Value) -> String {
    let offset = |position: &Value| {
        let line = position["line"].as_u64().unwrap() as usize;
        let character = position["character"].as_u64().unwrap() as usize;
        crate::offset_at(source, line, character)
    };
    let mut edits = edits
        .as_array()
        .unwrap()
        .iter()
        .map(|edit| {
            (
                offset(&edit["range"]["start"]),
                offset(&edit["range"]["end"]),
                edit["newText"].as_str().unwrap().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    edits.sort_by_key(|edit| edit.0);
    let mut result = source.to_owned();
    for (start, end, text) in edits.into_iter().rev() {
        result.replace_range(start..end, &text);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_lsp_formatting_options() {
        let options = format_options(
            &json!({"options": {
                "tabSize": 4,
                "insertSpaces": false,
                "trimTrailingWhitespace": true,
                "insertFinalNewline": false,
                "trimFinalNewlines": false,
            }}),
            "<a/>\r\n",
            &FormatSettings::default(),
        );
        assert_eq!(
            options,
            FormatOptions {
                tab_size: 4,
                insert_spaces: false,
                trim_trailing_whitespace: true,
                insert_final_newline: false,
                trim_final_newlines: false,
                line_ending: LineEnding::CrLf,
                ..FormatOptions::default()
            }
        );
        assert_eq!(
            format_options(&json!({}), "<a/>", &FormatSettings::default()),
            FormatOptions::default()
        );
        // Settings are the fallback for options missing from the request.
        let settings = crate::settings::Settings::from_value(&json!({"xml": {"format": {
            "tabSize": 8, "insertSpaces": false, "trimFinalNewlines": false,
            "emptyElements": "expand", "maxLineWidth": 40,
        }}}));
        let options = format_options(
            &json!({"options": {"tabSize": 3}}),
            "<a/>",
            &settings.format,
        );
        assert_eq!(
            options,
            FormatOptions {
                tab_size: 3,
                insert_spaces: false,
                trim_final_newlines: false,
                empty_elements: xml_core::EmptyElements::Expand,
                max_line_width: 40,
                ..FormatOptions::default()
            }
        );
    }

    #[test]
    fn document_edits_only_touch_changed_lines() {
        let source = "<root>\n  <a>é</a>\n<b/>\n  <c/>\n</root>\n";
        let edits = document_edits(source, &FormatOptions::default()).unwrap();
        assert_eq!(
            edits,
            json!([{
                "range": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 0}},
                "newText": "  ",
            }])
        );
        assert_eq!(
            apply_edits(source, &edits),
            "<root>\n  <a>é</a>\n  <b/>\n  <c/>\n</root>\n"
        );
        let formatted = apply_edits(source, &edits);
        assert_eq!(
            document_edits(&formatted, &FormatOptions::default()).unwrap(),
            json!([])
        );
    }

    #[test]
    fn range_edits_use_utf16_positions() {
        let source = "<root><😀>é</😀><b>x</b></root>";
        let start = source.find("<b>").unwrap();
        let edits = range_edits(source, start..start + 3, &FormatOptions::default()).unwrap();
        // `<b>` starts after 16 UTF-16 code units (😀 counts as two).
        assert_eq!(
            edits[0]["range"]["start"],
            json!({"line": 0, "character": 16})
        );
        assert_eq!(
            apply_edits(source, &edits),
            "<root><😀>é</😀>\n  <b>x</b>\n</root>"
        );
    }
}
