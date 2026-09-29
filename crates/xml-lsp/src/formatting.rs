//! `textDocument/formatting` et `textDocument/rangeFormatting`.
//!
//! Les `FormattingOptions` LSP (`tabSize`, `insertSpaces`,
//! `trimTrailingWhitespace`, `insertFinalNewline`, `trimFinalNewlines`) sont
//! respectées ; la fin de ligne insérée est celle du document. Le résultat
//! est renvoyé sous forme de modifications minimales (différence ligne à
//! ligne) plutôt que d'un remplacement complet, pour que l'éditeur conserve
//! les curseurs hors des lignes modifiées.
//!
//! Le formatage de plage suit LemMinX : la plage est étendue aux éléments
//! complets qui l'englobent et seule cette région est reformatée, même si le
//! reste du document est invalide.

use std::ops::Range;

use serde_json::{Value, json};
use xml_core::{FormatOptions, LineEnding, diff::diff_text, format_xml_range, format_xml_with};

use crate::{selection::LineIndex, settings::FormatSettings};

/// Options de formatage : réglages `xml.format.*`, puis options de la
/// requête (prioritaires). Les options absentes des deux conservent le
/// comportement par défaut.
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

/// Modifications (`TextEdit[]`) formatant tout le document, ou `None` si le
/// document est invalide.
pub fn document_edits(source: &str, options: &FormatOptions) -> Option<Value> {
    let formatted = format_xml_with(source, options).ok()?;
    Some(text_edits(source, 0..source.len(), &formatted))
}

/// Modifications (`TextEdit[]`) formatant la région englobant `range`, ou
/// `None` si elle ne peut pas être formatée sans risque.
pub fn range_edits(source: &str, range: Range<usize>, options: &FormatOptions) -> Option<Value> {
    let formatted = format_xml_range(source, range, options)?;
    Some(text_edits(source, formatted.range, &formatted.text))
}

/// Différence entre `source[range]` et `replacement`, en `TextEdit[]`.
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

/// Applique des `TextEdit` (positions UTF-16) à `source`.
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
        // Les réglages servent de repli aux options absentes de la requête.
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
        // `<b>` commence après 16 unités UTF-16 (😀 en compte deux).
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
