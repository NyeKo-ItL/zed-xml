//! Navigation between ID references and IDs: go to definition
//! (`textDocument/definition`) from an `xs:IDREF(S)`, DTD `IDREF(S)` or
//! `xs:keyref` value to the `ID` or key value it designates, and references
//! (`textDocument/references`) from an ID or key value (or one of its
//! references) to every reference, like IntelliJ.
//!
//! The links come from `xsd_core::identity_links` and
//! `dtd_core::id_links`: (reference range, target range) in UTF-8 offsets.

use std::ops::Range;

use serde_json::{Value, json};

use crate::selection::LineIndex;

/// (reference, target) ranges of the document.
pub(crate) type Links = Vec<(Range<usize>, Range<usize>)>;

/// The cursor is in the range (its end included, for a cursor right after
/// the value).
fn contains(range: &Range<usize>, offset: usize) -> bool {
    range.start <= offset && offset <= range.end
}

/// Whether some reference or target of `links` contains `offset`: cheap
/// check made before computing locations.
pub(crate) fn applies(links: &Links, offset: usize) -> bool {
    links
        .iter()
        .any(|(reference, target)| contains(reference, offset) || contains(target, offset))
}

fn location(uri: &str, source: &str, lines: &LineIndex, range: &Range<usize>) -> Value {
    json!({
        "uri": uri,
        "range": {
            "start": lines.position(source, range.start),
            "end": lines.position(source, range.end),
        },
    })
}

/// Location of the ID or key designated by the reference at `offset`.
pub(crate) fn definition(links: &Links, uri: &str, source: &str, offset: usize) -> Option<Value> {
    let (_, target) = links
        .iter()
        .find(|(reference, _)| contains(reference, offset))?;
    let lines = LineIndex::new(source);
    Some(json!([location(uri, source, &lines, target)]))
}

/// References of the ID or key at `offset` (or designated by the reference
/// at `offset`), with the ID itself when `include_declaration` is set.
pub(crate) fn references(
    links: &Links,
    uri: &str,
    source: &str,
    offset: usize,
    include_declaration: bool,
) -> Option<Value> {
    let target = links
        .iter()
        .find(|(reference, target)| contains(reference, offset) || contains(target, offset))
        .map(|(_, target)| target.clone())?;
    let lines = LineIndex::new(source);
    let mut ranges = Vec::new();
    if include_declaration {
        ranges.push(target.clone());
    }
    for (reference, candidate) in links {
        if *candidate == target && !ranges.contains(reference) {
            ranges.push(reference.clone());
        }
    }
    ranges.sort_by_key(|range| range.start);
    Some(Value::Array(
        ranges
            .iter()
            .map(|range| location(uri, source, &lines, range))
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigates_between_references_and_targets() {
        let source = "<a id=\"x\"/>\r\n<b ref=\"x\"/><c refs=\"é x\"/>";
        let target = 7..8;
        let first = source.find("ref=\"x\"").unwrap() + 5;
        let second = source.rfind('x').unwrap();
        let links: Links = vec![
            (first..first + 1, target.clone()),
            (second..second + 1, target),
        ];
        let definition = definition(&links, "file:///a.xml", source, first + 1).unwrap();
        assert_eq!(
            definition[0]["range"],
            json!({"start": {"line": 0, "character": 7}, "end": {"line": 0, "character": 8}})
        );
        assert!(super::definition(&links, "file:///a.xml", source, 7).is_none());
        let references = references(&links, "file:///a.xml", source, 7, true).unwrap();
        let ranges = references
            .as_array()
            .unwrap()
            .iter()
            .map(|location| location["range"]["start"].clone())
            .collect::<Vec<_>>();
        assert_eq!(
            ranges,
            [
                json!({"line": 0, "character": 7}),
                json!({"line": 1, "character": 8}),
                json!({"line": 1, "character": 23}),
            ]
        );
        assert!(applies(&links, second));
        assert!(!applies(&links, 0));
    }
}
