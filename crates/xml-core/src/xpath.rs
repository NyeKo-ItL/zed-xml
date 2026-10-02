//! XPath of the node at a position of a document (`/root/item[2]/@id`), as
//! the "Get current XPath" command of other XML editors.
//!
//! Names are written as in the document (`prefix:local`), and a position
//! index counts the preceding siblings with the same qualified name. The
//! document may be malformed: the tolerant tag tree is used.

use std::collections::HashMap;

use crate::tags::{XmlTagTree, scan_attributes};

/// When a step of the path carries a position predicate (`item[2]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum XPathIndexes {
    /// Only when several siblings share the name of the step.
    #[default]
    Ambiguous,
    /// On every element step (`/root[1]/item[1]`).
    Always,
}

/// XPath of the element, or of the attribute, at byte `offset` of `source`;
/// `None` outside the root element.
pub fn xpath_at(source: &str, offset: usize, indexes: XPathIndexes) -> Option<String> {
    xpath_in_tree(source, &XmlTagTree::parse(source), offset, indexes)
}

/// Like [`xpath_at`], with the tag tree of `source` already parsed.
pub fn xpath_in_tree(
    source: &str,
    tree: &XmlTagTree,
    offset: usize,
    indexes: XPathIndexes,
) -> Option<String> {
    let target = tree.innermost_element_at(offset)?;
    let mut chain = tree.ancestors(target).collect::<Vec<_>>();
    chain.reverse();
    chain.push(target);

    // Position of every step among the siblings of the same name, found in
    // one pass over the elements (document order).
    let wanted: HashMap<Option<usize>, &str> = chain
        .iter()
        .map(|&index| {
            let element = &tree.elements()[index];
            (element.parent, element.name(source))
        })
        .collect();
    let mut totals: HashMap<Option<usize>, usize> = HashMap::new();
    let mut positions: HashMap<usize, usize> = HashMap::new();
    for (index, element) in tree.elements().iter().enumerate() {
        if wanted.get(&element.parent) != Some(&element.name(source)) {
            continue;
        }
        let total = totals.entry(element.parent).or_default();
        *total += 1;
        if chain.contains(&index) {
            positions.insert(index, *total);
        }
    }

    let mut path = String::new();
    for &index in &chain {
        let element = &tree.elements()[index];
        path.push('/');
        path.push_str(element.name(source));
        let total = totals.get(&element.parent).copied().unwrap_or(1);
        if total > 1 || indexes == XPathIndexes::Always {
            path.push_str(&format!(
                "[{}]",
                positions.get(&index).copied().unwrap_or(1)
            ));
        }
    }

    let tag = &tree.elements()[target].start_tag;
    if tag.range.start < offset && offset <= tag.range.end {
        // `name="value"` including the quotes, so the cursor after the
        // closing quote is still on the attribute.
        if let Some(attribute) = scan_attributes(source, tag).into_iter().find(|attribute| {
            let end = attribute
                .value
                .as_ref()
                .map_or(attribute.name.end, |value| {
                    (value.end + 1).min(tag.range.end)
                });
            attribute.name.start <= offset && offset <= end
        }) {
            path.push_str("/@");
            path.push_str(attribute.name(source));
        }
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(source: &str, marker: &str, indexes: XPathIndexes) -> Option<String> {
        let offset = source.find(marker).expect("marker") + 1;
        xpath_at(source, offset, indexes)
    }

    const DOCUMENT: &str = "<?xml version=\"1.0\"?>\n<root>\n  <item id=\"a\">one</item>\n  <item id=\"b\"><name>two</name><name>three</name></item>\n  <other/>\n</root>";

    #[test]
    fn indexes_only_ambiguous_siblings() {
        assert_eq!(
            at(DOCUMENT, "<root>", XPathIndexes::Ambiguous).as_deref(),
            Some("/root")
        );
        assert_eq!(
            at(DOCUMENT, "<item id=\"a\"", XPathIndexes::Ambiguous).as_deref(),
            Some("/root/item[1]")
        );
        assert_eq!(
            at(DOCUMENT, "<item id=\"b\"", XPathIndexes::Ambiguous).as_deref(),
            Some("/root/item[2]")
        );
        assert_eq!(
            at(DOCUMENT, "<other", XPathIndexes::Ambiguous).as_deref(),
            Some("/root/other")
        );
    }

    #[test]
    fn counts_siblings_of_each_ancestor() {
        assert_eq!(
            at(DOCUMENT, "<name>three", XPathIndexes::Ambiguous).as_deref(),
            Some("/root/item[2]/name[2]")
        );
        // The text of an element belongs to the element.
        assert_eq!(
            at(DOCUMENT, "three", XPathIndexes::Ambiguous).as_deref(),
            Some("/root/item[2]/name[2]")
        );
    }

    #[test]
    fn can_index_every_step() {
        assert_eq!(
            at(DOCUMENT, "<other", XPathIndexes::Always).as_deref(),
            Some("/root[1]/other[1]")
        );
    }

    #[test]
    fn names_the_attribute_under_the_cursor() {
        assert_eq!(
            at(DOCUMENT, "id=\"b\"", XPathIndexes::Ambiguous).as_deref(),
            Some("/root/item[2]/@id")
        );
        assert_eq!(
            at(DOCUMENT, "b\">", XPathIndexes::Ambiguous).as_deref(),
            Some("/root/item[2]/@id")
        );
        // The tag name is the element, not an attribute.
        assert_eq!(
            at(DOCUMENT, "item id=\"b\"", XPathIndexes::Ambiguous).as_deref(),
            Some("/root/item[2]")
        );
    }

    #[test]
    fn keeps_prefixes_and_ignores_siblings_of_other_names() {
        let source = "<a:r xmlns:a=\"u\"><a:x/><b/><a:x/><x/></a:r>";
        assert_eq!(
            xpath_at(
                source,
                source.rfind("<a:x").unwrap() + 1,
                XPathIndexes::Ambiguous
            )
            .as_deref(),
            Some("/a:r/a:x[2]")
        );
        assert_eq!(
            xpath_at(
                source,
                source.find("<x/>").unwrap() + 1,
                XPathIndexes::Ambiguous
            )
            .as_deref(),
            Some("/a:r/x")
        );
    }

    #[test]
    fn tolerates_malformed_documents() {
        let source = "<r><a/><a><b";
        assert_eq!(
            xpath_at(source, source.len(), XPathIndexes::Ambiguous).as_deref(),
            Some("/r/a[2]/b")
        );
        assert_eq!(xpath_at("", 0, XPathIndexes::Ambiguous), None);
        assert_eq!(xpath_at("  <r/>", 0, XPathIndexes::Ambiguous), None);
    }
}
