//! `textDocument/documentHighlight` : mise en évidence de la balise ouvrante
//! et de la balise fermante correspondante, comme LemMinX.

use serde_json::{Value, json};
use xml_core::tags::XmlTagTree;

use crate::position_at;

/// `DocumentHighlightKind.Read`, utilisé par LemMinX pour les noms de balises.
const HIGHLIGHT_KIND_READ: u8 = 2;

/// Retourne les mises en évidence LSP pour le curseur situé à `offset`.
///
/// Si le curseur est sur le nom d'une balise ouvrante ou fermante, les noms
/// des deux balises de la paire sont retournés (un seul pour un élément
/// auto-fermant, non fermé ou une balise fermante orpheline). Ailleurs, la
/// liste est vide.
pub fn document_highlights(source: &str, offset: usize) -> Vec<Value> {
    let Some(pair) = XmlTagTree::parse(source).tag_pair_at(offset) else {
        return Vec::new();
    };
    pair.name_ranges()
        .map(|range| {
            json!({
                "range": {
                    "start": position_at(source, range.start),
                    "end": position_at(source, range.end),
                },
                "kind": HIGHLIGHT_KIND_READ,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(start: (u32, u32), end: (u32, u32)) -> Value {
        json!({
            "start": {"line": start.0, "character": start.1},
            "end": {"line": end.0, "character": end.1},
        })
    }

    fn ranges(highlights: &[Value]) -> Vec<Value> {
        highlights
            .iter()
            .map(|highlight| highlight["range"].clone())
            .collect()
    }

    #[test]
    fn highlights_both_tag_names_from_either_side() {
        let source = "<root><ns:item a=\"1\">x</ns:item></root>";
        let from_start = document_highlights(source, 9);
        assert_eq!(
            ranges(&from_start),
            vec![range((0, 7), (0, 14)), range((0, 24), (0, 31))]
        );
        assert_eq!(from_start[0]["kind"], 2);
        assert_eq!(document_highlights(source, 31), from_start);
    }

    #[test]
    fn returns_nothing_outside_tag_names() {
        let source = "<root attr=\"v\">text<!-- <root> --></root>";
        assert!(document_highlights(source, 8).is_empty());
        assert!(document_highlights(source, 17).is_empty());
        assert!(document_highlights(source, 26).is_empty());
    }

    #[test]
    fn highlights_a_single_name_for_self_closing_and_unclosed_elements() {
        let source = "<root><item/>";
        assert_eq!(
            ranges(&document_highlights(source, 8)),
            vec![range((0, 7), (0, 11))]
        );
        assert_eq!(
            ranges(&document_highlights(source, 2)),
            vec![range((0, 1), (0, 5))]
        );
    }

    #[test]
    fn uses_utf16_positions_with_crlf_line_endings() {
        let source = "<a>\r\n  <😀x>\r\n  </😀x>\r\n</a>";
        let offset = source.find("😀x>").unwrap();
        assert_eq!(
            ranges(&document_highlights(source, offset)),
            vec![range((1, 3), (1, 6)), range((2, 4), (2, 7))]
        );
    }
}
