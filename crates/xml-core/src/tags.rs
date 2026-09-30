//! Tolerant location of tags and start/end tag pairs.
//!
//! This module provides a deliberately permissive lexer: it never stops on
//! a syntax error, so that it stays usable while typing (malformed
//! documents, unclosed tags, missing quotes...). It is the common
//! foundation of the LSP features that need to link a start tag to its end
//! tag:
//!
//! - `textDocument/documentHighlight`: [`XmlTagTree::tag_pair_at`];
//! - `textDocument/linkedEditingRange`: [`XmlTagTree::tag_pair_at`] then
//!   [`XmlTagPair::name_ranges`];
//! - `textDocument/rename`: [`XmlTagTree::tag_pair_at`],
//!   [`qualified_name_parts`] to tell prefix and local name apart, and
//!   [`scan_attributes`] for `xmlns:prefix` declarations;
//! - `textDocument/foldingRange`: [`XmlTagTree::elements`],
//!   [`XmlElement::end_tag`] and [`scan_markup`] (comments, CDATA,
//!   processing instructions, `<!DOCTYPE ...>`);
//! - `textDocument/selectionRange`: [`XmlTagTree::innermost_element_at`] and
//!   [`XmlTagTree::ancestors`].
//!
//! All offsets are UTF-8 byte offsets into the source and always fall on a
//! character boundary. Comments, CDATA sections, processing instructions
//! and `<!DOCTYPE ...>` declarations are ignored by [`scan_tags`] and
//! listed separately by [`scan_markup`].

use std::{collections::HashMap, ops::Range};

/// Kind of a tag found in the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmlTagKind {
    /// Start tag `<name ...>` (possibly unterminated).
    Start,
    /// End tag `</name>`.
    End,
    /// Self-closing tag `<name ... />`.
    SelfClosing,
}

/// Lexically located tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlTag {
    pub kind: XmlTagKind,
    /// Range of the tag, from `<` to after `>`. For an unterminated tag, the
    /// range stops at the next `<` or at the end of the source.
    pub range: Range<usize>,
    /// Range of the qualified name (`prefix:local`) of the tag.
    pub name: Range<usize>,
    /// Whether the tag is properly terminated by `>` or `/>`.
    pub closed: bool,
}

impl XmlTag {
    /// Qualified name of the tag.
    pub fn name<'a>(&self, source: &'a str) -> &'a str {
        &source[self.name.clone()]
    }

    /// Whether `offset` is on the tag name, bounds included (the cursor
    /// right after the last character of the name counts).
    pub fn name_contains(&self, offset: usize) -> bool {
        self.name.start <= offset && offset <= self.name.end
    }
}

/// Element rebuilt from the tags: start tag and, when it exists, the
/// matching end tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlElement {
    /// Start or self-closing tag.
    pub start_tag: XmlTag,
    /// Matching end tag (`None` for a self-closing or unclosed
    /// element).
    pub end_tag: Option<XmlTag>,
    /// Index of the parent element in [`XmlTagTree::elements`].
    pub parent: Option<usize>,
    /// Depth (0 for a root element).
    pub depth: usize,
}

impl XmlElement {
    /// Qualified name of the element.
    pub fn name<'a>(&self, source: &'a str) -> &'a str {
        self.start_tag.name(source)
    }

    /// Whether the element has the `<name />` form.
    pub fn is_self_closing(&self) -> bool {
        self.start_tag.kind == XmlTagKind::SelfClosing
    }

    /// Whether the element is self-closing or has its end tag.
    pub fn is_closed(&self) -> bool {
        self.is_self_closing() || self.end_tag.is_some()
    }

    /// Full range of the element, from the opening `<` to after the `>` of
    /// the end tag. For an unclosed element, only the start tag is
    /// covered.
    pub fn range(&self) -> Range<usize> {
        let end = self
            .end_tag
            .as_ref()
            .map_or(self.start_tag.range.end, |tag| tag.range.end);
        self.start_tag.range.start..end
    }

    /// Range of the content between the start tag and the end tag.
    pub fn content_range(&self) -> Option<Range<usize>> {
        let end_tag = self.end_tag.as_ref()?;
        Some(self.start_tag.range.end..end_tag.range.start)
    }
}

/// Pair of linked tag names, found from a cursor position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlTagPair {
    /// Side of the pair the cursor is on.
    pub cursor_on: XmlTagKind,
    /// Name of the start or self-closing tag (`None` for an orphan end
    /// tag).
    pub start_name: Option<Range<usize>>,
    /// Name of the end tag (`None` for a self-closing or unclosed
    /// element).
    pub end_name: Option<Range<usize>>,
    /// Index of the element in [`XmlTagTree::elements`] (`None` for an
    /// orphan end tag).
    pub element: Option<usize>,
}

impl XmlTagPair {
    /// Ranges of the existing names, start tag first.
    pub fn name_ranges(&self) -> impl Iterator<Item = Range<usize>> + '_ {
        self.start_name.iter().chain(self.end_name.iter()).cloned()
    }

    /// Whether both sides of the pair exist.
    pub fn is_complete(&self) -> bool {
        self.start_name.is_some() && self.end_name.is_some()
    }
}

/// Element tree rebuilt tolerantly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XmlTagTree {
    elements: Vec<XmlElement>,
    orphan_end_tags: Vec<XmlTag>,
    /// For each element, the nearest strict ancestor whose start tag may
    /// declare a namespace (contains `xmlns`): namespace resolution only
    /// visits these, so it stays constant time in a deeply nested document
    /// without declarations.
    namespace_parents: Vec<Option<usize>>,
}

/// Maximum number of declaring ancestors visited to resolve a prefix (a
/// document declaring namespaces on thousands of nested elements is
/// hostile: beyond, the prefix is considered undeclared).
const MAX_NAMESPACE_SCOPES: usize = 4096;

impl XmlTagTree {
    /// Parses the source and pairs the tags.
    ///
    /// An end tag is associated with the nearest open element with the same
    /// name; intermediate open elements stay unclosed. An end tag without a
    /// matching open element is kept in
    /// [`XmlTagTree::orphan_end_tags`].
    pub fn parse(source: &str) -> Self {
        let mut elements: Vec<XmlElement> = Vec::new();
        let mut orphan_end_tags = Vec::new();
        let mut namespace_parents: Vec<Option<usize>> = Vec::new();
        let mut declares: Vec<bool> = Vec::new();
        let mut open: Vec<usize> = Vec::new();
        // Positions in `open` of the open elements, by name: an end tag
        // finds its element without scanning the whole stack (linear even
        // for many unmatched end tags deep in a document).
        let mut open_by_name: HashMap<&str, Vec<usize>> = HashMap::new();

        for tag in scan_tags(source) {
            match tag.kind {
                XmlTagKind::Start | XmlTagKind::SelfClosing => {
                    let is_start = tag.kind == XmlTagKind::Start;
                    let parent = open.last().copied();
                    namespace_parents.push(parent.and_then(|parent| {
                        if declares[parent] {
                            Some(parent)
                        } else {
                            namespace_parents[parent]
                        }
                    }));
                    declares.push(source[tag.range.clone()].contains("xmlns"));
                    let name = tag.name(source);
                    elements.push(XmlElement {
                        start_tag: tag,
                        end_tag: None,
                        parent,
                        depth: open.len(),
                    });
                    if is_start {
                        open_by_name.entry(name).or_default().push(open.len());
                        open.push(elements.len() - 1);
                    }
                }
                XmlTagKind::End => {
                    let name = tag.name(source);
                    let matching = open_by_name
                        .get(name)
                        .and_then(|positions| positions.last())
                        .copied();
                    match matching {
                        Some(position) => {
                            let index = open[position];
                            for closed in open.drain(position..).rev() {
                                if let Some(positions) =
                                    open_by_name.get_mut(elements[closed].name(source))
                                {
                                    positions.pop();
                                }
                            }
                            elements[index].end_tag = Some(tag);
                        }
                        None => orphan_end_tags.push(tag),
                    }
                }
            }
        }

        Self {
            elements,
            orphan_end_tags,
            namespace_parents,
        }
    }

    /// Ancestors of `index` (nearest first) whose start tag may declare a
    /// namespace, bounded by [`MAX_NAMESPACE_SCOPES`].
    fn namespace_ancestors(&self, index: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(
            self.namespace_parents.get(index).copied().flatten(),
            |&parent| self.namespace_parents.get(parent).copied().flatten(),
        )
        .take(MAX_NAMESPACE_SCOPES)
    }

    /// Elements in the order of their start tag (a parent always precedes
    /// its children).
    pub fn elements(&self) -> &[XmlElement] {
        &self.elements
    }

    /// End tags without a matching start tag.
    pub fn orphan_end_tags(&self) -> &[XmlTag] {
        &self.orphan_end_tags
    }

    /// Returns the pair of names linked to the tag name under the cursor.
    ///
    /// Returns `None` if the cursor is not on a tag name (content,
    /// attributes, comments...).
    pub fn tag_pair_at(&self, offset: usize) -> Option<XmlTagPair> {
        for (index, element) in self.elements.iter().enumerate() {
            if element.start_tag.range.start > offset {
                break;
            }
            let on_start = element.start_tag.name_contains(offset);
            let on_end = element
                .end_tag
                .as_ref()
                .is_some_and(|tag| tag.name_contains(offset));
            if on_start || on_end {
                return Some(XmlTagPair {
                    cursor_on: if on_start {
                        element.start_tag.kind
                    } else {
                        XmlTagKind::End
                    },
                    start_name: Some(element.start_tag.name.clone()),
                    end_name: element.end_tag.as_ref().map(|tag| tag.name.clone()),
                    element: Some(index),
                });
            }
        }
        self.orphan_end_tags
            .iter()
            .find(|tag| tag.name_contains(offset))
            .map(|tag| XmlTagPair {
                cursor_on: XmlTagKind::End,
                start_name: None,
                end_name: Some(tag.name.clone()),
                element: None,
            })
    }

    /// Index of the deepest element whose [`XmlElement::range`] contains
    /// `offset` (bounds included).
    pub fn innermost_element_at(&self, offset: usize) -> Option<usize> {
        let mut found = None;
        for (index, element) in self.elements.iter().enumerate() {
            if element.start_tag.range.start > offset {
                break;
            }
            let range = element.range();
            if range.start <= offset && offset <= range.end {
                found = Some(index);
            }
        }
        found
    }

    /// Ancestors of the element `index`, from the direct parent to the root.
    pub fn ancestors(&self, index: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(
            self.elements.get(index).and_then(|element| element.parent),
            |&parent| self.elements[parent].parent,
        )
    }
}

/// Splits a qualified name into a prefix range (without `:`) and a local
/// name range. `name` must be a range of `source`.
pub fn qualified_name_parts(
    source: &str,
    name: Range<usize>,
) -> (Option<Range<usize>>, Range<usize>) {
    match source[name.clone()].find(':') {
        Some(colon) => (
            Some(name.start..name.start + colon),
            name.start + colon + 1..name.end,
        ),
        None => (None, name),
    }
}

/// Reserved namespace of the `xml` prefix.
pub const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";

/// Nearest element (`element` itself included) declaring `prefix`
/// (`None`: default namespace `xmlns`), with the declaring attribute.
/// `attributes[i]` are the attributes of the start tag of element `i` of
/// `tree` (see [`scan_attributes`]).
pub fn namespace_declaration<'t>(
    source: &str,
    tree: &XmlTagTree,
    attributes: &'t [Vec<XmlAttribute>],
    element: usize,
    prefix: Option<&str>,
) -> Option<(usize, &'t XmlAttribute)> {
    std::iter::once(element)
        .chain(tree.namespace_ancestors(element))
        .find_map(|index| {
            attributes
                .get(index)?
                .iter()
                .find(|attribute| {
                    let name = attribute.name(source);
                    match prefix {
                        Some(prefix) => name.strip_prefix("xmlns:") == Some(prefix),
                        None => name == "xmlns",
                    }
                })
                .map(|attribute| (index, attribute))
        })
}

/// Namespace of `prefix` in the context of the element `element`, walking
/// up the `xmlns` declarations of the ancestors. Returns `None` for an
/// undeclared prefix and `Some(None)` for no namespace (no prefix without a
/// default `xmlns`, or `xmlns=""`). The `xml` prefix is predefined.
pub fn resolve_namespace<'s>(
    source: &'s str,
    tree: &XmlTagTree,
    attributes: &[Vec<XmlAttribute>],
    element: usize,
    prefix: Option<&str>,
) -> Option<Option<&'s str>> {
    if prefix == Some("xml") {
        return Some(Some(XML_NAMESPACE));
    }
    match namespace_declaration(source, tree, attributes, element, prefix) {
        Some((_, attribute)) => Some(
            attribute
                .value
                .clone()
                .map(|range| &source[range])
                .filter(|value| !value.is_empty()),
        ),
        None if prefix.is_none() => Some(None),
        None => None,
    }
}

/// Lexically located attribute in a start or self-closing tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlAttribute {
    /// Range of the qualified name of the attribute.
    pub name: Range<usize>,
    /// Range of the value, quotes excluded (`None` without `=` or without a
    /// value). A value missing its closing quote stops before the end of the
    /// tag.
    pub value: Option<Range<usize>>,
}

impl XmlAttribute {
    /// Qualified name of the attribute.
    pub fn name<'a>(&self, source: &'a str) -> &'a str {
        &source[self.name.clone()]
    }

    /// Raw value of the attribute (entities not resolved).
    pub fn value<'a>(&self, source: &'a str) -> Option<&'a str> {
        self.value.clone().map(|range| &source[range])
    }
}

/// Lists the attributes of a start or self-closing tag, tolerantly.
/// Returns an empty list for an end tag.
pub fn scan_attributes(source: &str, tag: &XmlTag) -> Vec<XmlAttribute> {
    let mut attributes = Vec::new();
    if tag.kind == XmlTagKind::End {
        return attributes;
    }
    let bytes = &source.as_bytes()[..tag.range.end.min(source.len())];
    let mut index = tag.name.end;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>' | b'=') {
            index += 1;
            continue;
        }
        if matches!(byte, b'"' | b'\'') {
            index = find_byte(bytes, index + 1, byte).map_or(bytes.len(), |end| end + 1);
            continue;
        }
        let name = scan_name(bytes, index);
        if name.is_empty() {
            index += 1;
            continue;
        }
        index = skip_whitespace(bytes, name.end);
        let mut value = None;
        if bytes.get(index) == Some(&b'=') {
            index = skip_whitespace(bytes, index + 1);
            match bytes.get(index) {
                Some(&quote @ (b'"' | b'\'')) => {
                    let end = find_byte(bytes, index + 1, quote);
                    let value_end = end.unwrap_or_else(|| unterminated_value_end(bytes));
                    value = Some(index + 1..value_end.max(index + 1));
                    index = end.map_or(bytes.len(), |end| end + 1);
                }
                Some(_) => {
                    let unquoted = scan_name(bytes, index);
                    index = unquoted.end.max(index + 1);
                    if !unquoted.is_empty() {
                        value = Some(unquoted);
                    }
                }
                None => {}
            }
        } else {
            index = name.end;
        }
        attributes.push(XmlAttribute { name, value });
    }
    attributes
}

fn skip_whitespace(bytes: &[u8], mut index: usize) -> usize {
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }
    index
}

/// End of an unterminated value: before the final `>` or `/>` of the tag.
fn unterminated_value_end(bytes: &[u8]) -> usize {
    if bytes.ends_with(b"/>") {
        bytes.len() - 2
    } else if bytes.ends_with(b">") {
        bytes.len() - 1
    } else {
        bytes.len()
    }
}

/// Kind of a construct that is not an element tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmlMarkupKind {
    /// Comment `<!-- ... -->`.
    Comment,
    /// Section `<![CDATA[ ... ]]>`.
    CData,
    /// Processing instruction `<? ... ?>`, including the `<?xml ...?>`
    /// prolog.
    ProcessingInstruction,
    /// Declaration `<! ... >` (typically `<!DOCTYPE ...>` with its
    /// internal subset `[...]`).
    Declaration,
}

/// Comment, CDATA section, processing instruction or declaration located
/// lexically outside element tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlMarkup {
    pub kind: XmlMarkupKind,
    /// Full range, delimiters included. An unterminated construct extends
    /// to the end of the source.
    pub range: Range<usize>,
    /// Range of the content, delimiters excluded (`<!--`/`-->`, `<![CDATA[`/
    /// `]]>`, `<?`/`?>`, `<!`/`>`).
    pub content: Range<usize>,
    /// Whether the closing delimiter is present.
    pub closed: bool,
}

impl XmlMarkup {
    /// Raw content, delimiters excluded.
    pub fn content<'a>(&self, source: &'a str) -> &'a str {
        &source[self.content.clone()]
    }
}

/// Lists the element tags of the source in document order.
pub fn scan_tags(source: &str) -> Vec<XmlTag> {
    scan(source).0
}

/// Lists the top-level comments, CDATA sections, processing instructions
/// and declarations in document order. Comments of a DTD internal subset
/// are part of the declaration.
pub fn scan_markup(source: &str) -> Vec<XmlMarkup> {
    scan(source).1
}

/// Builds an [`XmlMarkup`] starting at `start`, whose opening delimiter is
/// `open` bytes long and whose scan (by [`skip_past`] with the terminator
/// `close`) stopped at `end`.
fn markup(
    source: &[u8],
    kind: XmlMarkupKind,
    start: usize,
    end: usize,
    open: usize,
    close: &[u8],
) -> XmlMarkup {
    let content_start = (start + open).min(end);
    let closed = end >= content_start + close.len() && source[..end].ends_with(close);
    let content_end = if closed { end - close.len() } else { end };
    XmlMarkup {
        kind,
        range: start..end,
        content: content_start..content_end.max(content_start),
        closed,
    }
}

fn scan(source: &str) -> (Vec<XmlTag>, Vec<XmlMarkup>) {
    let bytes = source.as_bytes();
    let mut tags = Vec::new();
    let mut markups = Vec::new();
    let mut index = 0;

    while let Some(relative) = find_byte(bytes, index, b'<') {
        let start = relative;
        let rest = &bytes[start..];
        if rest.starts_with(b"<!--") {
            index = skip_past(bytes, start + 4, b"-->");
            markups.push(markup(
                bytes,
                XmlMarkupKind::Comment,
                start,
                index,
                4,
                b"-->",
            ));
        } else if rest.starts_with(b"<![CDATA[") {
            index = skip_past(bytes, start + 9, b"]]>");
            markups.push(markup(bytes, XmlMarkupKind::CData, start, index, 9, b"]]>"));
        } else if rest.starts_with(b"<?") {
            index = skip_past(bytes, start + 2, b"?>");
            markups.push(markup(
                bytes,
                XmlMarkupKind::ProcessingInstruction,
                start,
                index,
                2,
                b"?>",
            ));
        } else if rest.starts_with(b"<!") {
            let (end, closed) = skip_declaration(bytes, start + 2);
            index = end;
            markups.push(XmlMarkup {
                kind: XmlMarkupKind::Declaration,
                range: start..end,
                content: start + 2..if closed { end - 1 } else { end },
                closed,
            });
        } else if rest.starts_with(b"</") {
            let name = scan_name(bytes, start + 2);
            if name.is_empty() {
                index = start + 2;
                continue;
            }
            let (end, closed, _) = scan_tag_end(bytes, name.end);
            tags.push(XmlTag {
                kind: XmlTagKind::End,
                range: start..end,
                name,
                closed,
            });
            index = end;
        } else {
            let name = scan_name(bytes, start + 1);
            if name.is_empty() {
                index = start + 1;
                continue;
            }
            let (end, closed, self_closing) = scan_tag_end(bytes, name.end);
            tags.push(XmlTag {
                kind: if self_closing {
                    XmlTagKind::SelfClosing
                } else {
                    XmlTagKind::Start
                },
                range: start..end,
                name,
                closed,
            });
            index = end;
        }
    }

    (tags, markups)
}

fn find_byte(bytes: &[u8], from: usize, needle: u8) -> Option<usize> {
    bytes
        .get(from..)?
        .iter()
        .position(|&byte| byte == needle)
        .map(|position| from + position)
}

fn skip_past(bytes: &[u8], from: usize, terminator: &[u8]) -> usize {
    bytes
        .get(from..)
        .and_then(|rest| {
            rest.windows(terminator.len())
                .position(|window| window == terminator)
        })
        .map_or(bytes.len(), |position| from + position + terminator.len())
}

/// Skips a `<!DOCTYPE ...>` declaration including its internal subset.
///
/// Returns `(end, closed)`.
fn skip_declaration(bytes: &[u8], mut index: usize) -> (usize, bool) {
    let mut depth = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'"' | b'\'' => {
                let quote = bytes[index];
                index = find_byte(bytes, index + 1, quote).map_or(bytes.len(), |end| end + 1);
                continue;
            }
            b'<' if bytes[index..].starts_with(b"<!--") => {
                index = skip_past(bytes, index + 4, b"-->");
                continue;
            }
            b'[' => depth += 1,
            b']' => depth = depth.saturating_sub(1),
            b'>' if depth == 0 => return (index + 1, true),
            _ => {}
        }
        index += 1;
    }
    (bytes.len(), false)
}

fn is_name_byte(byte: u8) -> bool {
    !matches!(
        byte,
        b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>' | b'<' | b'=' | b'"' | b'\'' | b'!' | b'?'
    )
}

fn scan_name(bytes: &[u8], start: usize) -> Range<usize> {
    let length = bytes.get(start..).map_or(0, |rest| {
        rest.iter().take_while(|&&byte| is_name_byte(byte)).count()
    });
    start..start + length
}

/// Walks the attributes up to the end of the tag.
///
/// Returns `(end, closed, self_closing)`. An unterminated tag stops before
/// the next `<` outside quotes or at the end of the source.
fn scan_tag_end(bytes: &[u8], mut index: usize) -> (usize, bool, bool) {
    while index < bytes.len() {
        match bytes[index] {
            b'>' => return (index + 1, true, false),
            b'/' if bytes.get(index + 1) == Some(&b'>') => return (index + 2, true, true),
            b'<' => return (index, false, false),
            b'"' | b'\'' => {
                let quote = bytes[index];
                match find_byte(bytes, index + 1, quote) {
                    Some(end) => index = end + 1,
                    None => return (bytes.len(), false, false),
                }
            }
            _ => index += 1,
        }
    }
    (bytes.len(), false, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names<'a>(source: &'a str, pair: &XmlTagPair) -> Vec<&'a str> {
        pair.name_ranges().map(|range| &source[range]).collect()
    }

    fn offset_of(source: &str, needle: &str, nth: usize) -> usize {
        source
            .match_indices(needle)
            .nth(nth)
            .map(|(offset, _)| offset)
            .expect("needle should exist")
    }

    #[test]
    fn scans_start_end_and_self_closing_tags() {
        let source = "<?xml version=\"1.0\"?><root a=\"1\"><item/><x:y b='>'></x:y></root>";
        let tags = scan_tags(source);
        let summary: Vec<_> = tags
            .iter()
            .map(|tag| (tag.kind, tag.name(source), tag.closed))
            .collect();
        assert_eq!(
            summary,
            vec![
                (XmlTagKind::Start, "root", true),
                (XmlTagKind::SelfClosing, "item", true),
                (XmlTagKind::Start, "x:y", true),
                (XmlTagKind::End, "x:y", true),
                (XmlTagKind::End, "root", true),
            ]
        );
        assert_eq!(&source[tags[2].range.clone()], "<x:y b='>'>");
    }

    #[test]
    fn ignores_comments_cdata_processing_instructions_and_doctype() {
        let source = "<!DOCTYPE r [<!ELEMENT r ANY><!-- <x> -->]><r><!-- <a> --><![CDATA[<b>]]><?pi <c>?></r>";
        let tags = scan_tags(source);
        let names: Vec<_> = tags.iter().map(|tag| tag.name(source)).collect();
        assert_eq!(names, vec!["r", "r"]);
    }

    #[test]
    fn pairs_nested_elements_with_the_same_name() {
        let source = "<a><a></a></a>";
        let tree = XmlTagTree::parse(source);
        let outer = tree.tag_pair_at(1).unwrap();
        assert_eq!(outer.start_name, Some(1..2));
        assert_eq!(outer.end_name, Some(12..13));
        let inner = tree.tag_pair_at(8).unwrap();
        assert_eq!(inner.start_name, Some(4..5));
        assert_eq!(inner.end_name, Some(8..9));
        assert_eq!(inner.cursor_on, XmlTagKind::End);
        assert_eq!(tree.elements()[1].parent, Some(0));
        assert_eq!(tree.elements()[1].depth, 1);
    }

    #[test]
    fn cursor_outside_tag_names_finds_nothing() {
        let source = "<root attr=\"value\">text</root>";
        let tree = XmlTagTree::parse(source);
        assert!(tree.tag_pair_at(0).is_none());
        assert!(tree.tag_pair_at(offset_of(source, "attr", 0) + 1).is_none());
        assert!(tree.tag_pair_at(offset_of(source, "text", 0) + 1).is_none());
        assert!(tree.tag_pair_at(offset_of(source, "</", 0) + 1).is_none());
        let pair = tree.tag_pair_at(5).expect("end of the name is included");
        assert!(pair.is_complete());
    }

    #[test]
    fn self_closing_elements_have_a_single_name() {
        let source = "<root><ns:item /></root>";
        let tree = XmlTagTree::parse(source);
        let pair = tree
            .tag_pair_at(offset_of(source, "ns:item", 0) + 3)
            .unwrap();
        assert_eq!(pair.cursor_on, XmlTagKind::SelfClosing);
        assert_eq!(names(source, &pair), vec!["ns:item"]);
    }

    #[test]
    fn tolerates_unclosed_and_mismatched_documents() {
        let source = "<root><open><child></child></root><stray></oops>";
        let tree = XmlTagTree::parse(source);
        let open = tree.tag_pair_at(offset_of(source, "open", 0)).unwrap();
        assert_eq!(names(source, &open), vec!["open"]);
        assert!(!open.is_complete());
        let root = tree.tag_pair_at(offset_of(source, "root", 1)).unwrap();
        assert_eq!(names(source, &root), vec!["root", "root"]);
        let orphan = tree.tag_pair_at(offset_of(source, "oops", 0)).unwrap();
        assert_eq!(orphan.start_name, None);
        assert_eq!(names(source, &orphan), vec!["oops"]);

        for source in [
            "<",
            "<a",
            "<a b=\"",
            "</",
            "<a <b>",
            "< a>",
            "<a></",
            "<!--",
            "<![CDATA[",
        ] {
            let tree = XmlTagTree::parse(source);
            for offset in 0..=source.len() {
                let _ = tree.tag_pair_at(offset);
                let _ = tree.innermost_element_at(offset);
            }
        }
        let tags = scan_tags("<a <b>");
        assert_eq!(tags[0].range, 0..3);
        assert!(!tags[0].closed);
    }

    #[test]
    fn finds_innermost_element_and_ancestors() {
        let source = "<a><b><c/></b></a>";
        let tree = XmlTagTree::parse(source);
        let c = tree
            .innermost_element_at(offset_of(source, "c", 0))
            .unwrap();
        assert_eq!(tree.elements()[c].name(source), "c");
        let ancestors: Vec<_> = tree
            .ancestors(c)
            .map(|index| tree.elements()[index].name(source))
            .collect();
        assert_eq!(ancestors, vec!["b", "a"]);
        let b = &tree.elements()[1];
        assert_eq!(&source[b.content_range().unwrap()], "<c/>");
    }

    #[test]
    fn splits_qualified_names() {
        let source = "<xs:element>";
        let (prefix, local) = qualified_name_parts(source, 1..11);
        assert_eq!(prefix.map(|range| &source[range]), Some("xs"));
        assert_eq!(&source[local], "element");
        let (prefix, local) = qualified_name_parts(source, 4..11);
        assert_eq!(prefix, None);
        assert_eq!(local, 4..11);
    }

    #[test]
    fn scans_attributes_of_start_and_self_closing_tags() {
        let source = "<x:a xmlns:x=\"urn:x\"  b = 'v>1' c d=e\r\n\tx:f=\"\"/><b g=\"é\"></b>";
        let tags = scan_tags(source);
        let summary: Vec<_> = scan_attributes(source, &tags[0])
            .iter()
            .map(|attribute| (attribute.name(source), attribute.value(source)))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("xmlns:x", Some("urn:x")),
                ("b", Some("v>1")),
                ("c", None),
                ("d", Some("e")),
                ("x:f", Some("")),
            ]
        );
        let b = scan_attributes(source, &tags[1]);
        assert_eq!(b[0].value(source), Some("é"));
        assert!(scan_attributes(source, &tags[2]).is_empty());
    }

    #[test]
    fn scans_attributes_of_malformed_tags() {
        for source in [
            "<a b=\"",
            "<a b=",
            "<a b=>",
            "<a \"x\" c='1'>",
            "<a b=\"1\"c=\"2\">",
            "<a b=\"x/>",
            "<a b=\">",
            "<a =>",
        ] {
            let tags = scan_tags(source);
            for attribute in scan_attributes(source, &tags[0]) {
                let _ = attribute.name(source);
                let _ = attribute.value(source);
            }
        }
        let names = |source: &str| -> Vec<(String, Option<String>)> {
            let tags = scan_tags(source);
            scan_attributes(source, &tags[0])
                .iter()
                .map(|attribute| {
                    (
                        attribute.name(source).to_owned(),
                        attribute.value(source).map(str::to_owned),
                    )
                })
                .collect()
        };
        assert_eq!(
            names("<a \"x\" c='1'>"),
            vec![("c".into(), Some("1".into()))]
        );
        assert_eq!(names("<a b=\"x/>"), vec![("b".into(), Some("x".into()))]);
        assert_eq!(
            names("<a b=\"1\"c=\"2\">"),
            vec![
                ("b".into(), Some("1".into())),
                ("c".into(), Some("2".into()))
            ]
        );
        assert_eq!(names("<a b=>"), vec![("b".into(), None)]);
    }

    #[test]
    fn scans_comments_cdata_processing_instructions_and_declarations() {
        let source = "<?xml version=\"1.0\"?>\n<!DOCTYPE r [\n<!-- ]> -->\n<!ELEMENT r ANY>\n]>\n<r><!-- #region --><![CDATA[<x>]]><?pi a?></r>";
        let summary: Vec<_> = scan_markup(source)
            .iter()
            .map(|markup| (markup.kind, markup.content(source), markup.closed))
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    XmlMarkupKind::ProcessingInstruction,
                    "xml version=\"1.0\"",
                    true
                ),
                (
                    XmlMarkupKind::Declaration,
                    "DOCTYPE r [\n<!-- ]> -->\n<!ELEMENT r ANY>\n]",
                    true
                ),
                (XmlMarkupKind::Comment, " #region ", true),
                (XmlMarkupKind::CData, "<x>", true),
                (XmlMarkupKind::ProcessingInstruction, "pi a", true),
            ]
        );
        let markups = scan_markup(source);
        assert_eq!(&source[markups[2].range.clone()], "<!-- #region -->");
    }

    #[test]
    fn scans_unterminated_markup_to_the_end_of_the_source() {
        for (source, kind, content) in [
            ("<!-- a", XmlMarkupKind::Comment, " a"),
            ("<!-->", XmlMarkupKind::Comment, ">"),
            ("<!--", XmlMarkupKind::Comment, ""),
            ("<![CDATA[x]]", XmlMarkupKind::CData, "x]]"),
            ("<?pi", XmlMarkupKind::ProcessingInstruction, "pi"),
            ("<?>", XmlMarkupKind::ProcessingInstruction, ">"),
            (
                "<!DOCTYPE r [<!ELEMENT r ANY>",
                XmlMarkupKind::Declaration,
                "DOCTYPE r [<!ELEMENT r ANY>",
            ),
        ] {
            let markups = scan_markup(source);
            assert_eq!(markups.len(), 1, "{source}");
            assert_eq!(markups[0].kind, kind, "{source}");
            assert!(!markups[0].closed, "{source}");
            assert_eq!(markups[0].content(source), content, "{source}");
            assert_eq!(markups[0].range, 0..source.len(), "{source}");
        }
        let empty = scan_markup("<!---->");
        assert!(empty[0].closed);
        assert_eq!(empty[0].content("<!---->"), "");
    }

    #[test]
    fn keeps_offsets_on_character_boundaries() {
        let source = "<élément attr=\"é\">texte</élément>";
        let tree = XmlTagTree::parse(source);
        let pair = tree.tag_pair_at(1).unwrap();
        assert_eq!(names(source, &pair), vec!["élément", "élément"]);
    }
}
