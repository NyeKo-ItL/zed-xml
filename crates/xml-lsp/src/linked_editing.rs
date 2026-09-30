//! `textDocument/linkedEditingRange` : édition simultanée du nom de la balise
//! ouvrante et de la balise fermante, comme LemMinX.

use serde_json::{Value, json};
use xml_core::tags::XmlTagTree;

use crate::position_at;

/// Motif ECMAScript d'un nom XML (`Name` de XML 1.0 5e édition).
///
/// Il permet au client d'arrêter l'édition liée dès qu'un caractère invalide
/// (espace, `>`, `=`...) est saisi. Les caractères hors du plan multilingue
/// de base sont décrits par leurs paires de substitution UTF-16, car le motif
/// est évalué sans le drapeau `u`.
pub const XML_NAME_WORD_PATTERN: &str = concat!(
    r"(?:[:A-Z_a-zÀ-ÖØ-öø-˿Ͱ-ͽͿ-῿",
    r"‌-‍⁰-↏Ⰰ-⿯、-퟿豈-﷏ﷰ-�]",
    r"|[\uD800-\uDB7F][\uDC00-\uDFFF])",
    r"(?:[-.0-9:A-Z_a-z·À-ÖØ-öø-ͽͿ-῿",
    r"‌-‍‿-⁀⁰-↏Ⰰ-⿯、-퟿豈-﷏",
    r"ﷰ-�]|[\uD800-\uDB7F][\uDC00-\uDFFF])*",
);

/// Retourne les plages d'édition liée pour le curseur situé à `offset`.
///
/// Le curseur doit se trouver sur le nom de la balise ouvrante ou fermante
/// d'un élément complet (`<a>...</a>`). Retourne `None` pour un élément
/// auto-fermant, non fermé, une balise fermante orpheline, des noms
/// différents, ou un curseur hors d'un nom de balise.
pub fn linked_editing_ranges(source: &str, offset: usize) -> Option<Value> {
    let pair = XmlTagTree::parse(source).tag_pair_at(offset)?;
    if !pair.is_complete() {
        return None;
    }
    let (start, end) = (pair.start_name.as_ref()?, pair.end_name.as_ref()?);
    if start.is_empty() || source[start.clone()] != source[end.clone()] {
        return None;
    }
    let ranges: Vec<Value> = pair
        .name_ranges()
        .map(|range| {
            json!({
                "start": position_at(source, range.start),
                "end": position_at(source, range.end),
            })
        })
        .collect();
    Some(json!({
        "ranges": ranges,
        "wordPattern": XML_NAME_WORD_PATTERN,
    }))
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

    fn ranges(source: &str, offset: usize) -> Option<Value> {
        linked_editing_ranges(source, offset).map(|result| result["ranges"].clone())
    }

    #[test]
    fn links_prefixed_names_from_either_side() {
        let source = "<root><ns:item a=\"1\">x</ns:item></root>";
        let expected = json!([range((0, 7), (0, 14)), range((0, 24), (0, 31))]);
        for offset in [7, 9, 14, 24, 27, 31] {
            assert_eq!(ranges(source, offset), Some(expected.clone()), "{offset}");
        }
        let result = linked_editing_ranges(source, 9).unwrap();
        assert_eq!(result["wordPattern"], XML_NAME_WORD_PATTERN);
    }

    #[test]
    fn links_the_right_pair_among_nested_same_name_elements() {
        let source = "<a><a></a></a>";
        let outer = json!([range((0, 1), (0, 2)), range((0, 12), (0, 13))]);
        let inner = json!([range((0, 4), (0, 5)), range((0, 8), (0, 9))]);
        assert_eq!(ranges(source, 1), Some(outer.clone()));
        assert_eq!(ranges(source, 13), Some(outer));
        assert_eq!(ranges(source, 5), Some(inner.clone()));
        assert_eq!(ranges(source, 8), Some(inner));
    }

    #[test]
    fn returns_null_for_incomplete_pairs() {
        // Auto-fermant, non fermé, balise fermante orpheline.
        assert_eq!(linked_editing_ranges("<root><item/></root>", 8), None);
        assert_eq!(linked_editing_ranges("<root><item></root>", 8), None);
        assert_eq!(linked_editing_ranges("<root></item></root>", 9), None);
        // Noms différents : aucune paire ne se forme.
        assert_eq!(linked_editing_ranges("<item></items>", 2), None);
        assert_eq!(linked_editing_ranges("<item></items>", 10), None);
    }

    #[test]
    fn returns_null_outside_tag_names() {
        let source = "<root attr=\"v\">text<!-- <root> --></root>";
        assert_eq!(linked_editing_ranges(source, 0), None);
        assert_eq!(linked_editing_ranges(source, 8), None);
        assert_eq!(linked_editing_ranges(source, 12), None);
        assert_eq!(linked_editing_ranges(source, 17), None);
        assert_eq!(linked_editing_ranges(source, 26), None);
    }

    #[test]
    fn returns_null_for_empty_names() {
        assert_eq!(linked_editing_ranges("<></>", 1), None);
        assert_eq!(linked_editing_ranges("<></>", 3), None);
        assert_eq!(linked_editing_ranges("<", 1), None);
        assert_eq!(linked_editing_ranges("", 0), None);
        // Curseur juste après `<` d'une balise vide suivie d'une paire valide.
        assert_eq!(linked_editing_ranges("<root><></root>", 7), None);
    }

    #[test]
    fn uses_utf16_positions_with_crlf_line_endings() {
        let source = "<a>\r\n  <😀x>\r\n  </😀x>\r\n</a>";
        let offset = source.rfind("😀x>").unwrap();
        assert_eq!(
            ranges(source, offset),
            Some(json!([range((1, 3), (1, 6)), range((2, 4), (2, 7))]))
        );
    }
}
