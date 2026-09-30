//! Tolerant, precisely located well-formedness check.
//!
//! `quick-xml` stops at the first error and checks neither duplicate
//! attributes, nor unquoted values, nor unescaped `&`/`<` characters. This
//! module relies on the [`crate::tags`] lexer to report every problem of
//! the document with the exact range of the faulty construct and the
//! information needed by quick fixes (`textDocument/codeAction`):
//!
//! - end tag that does not match the open element
//!   ([`XmlProblemKind::MismatchedEndTag`]) or without an open element
//!   ([`XmlProblemKind::UnmatchedEndTag`]);
//! - element without an end tag ([`XmlProblemKind::UnclosedElement`]);
//! - tag without a final `>` ([`XmlProblemKind::UnclosedTag`]);
//! - duplicate attribute ([`XmlProblemKind::DuplicateAttribute`]) or
//!   unquoted value ([`XmlProblemKind::UnquotedAttributeValue`]);
//! - unescaped `&` or `<` in text or an attribute value
//!   ([`XmlProblemKind::UnescapedCharacter`]);
//! - Namespaces in XML 1.0 constraints: undeclared prefixes
//!   ([`XmlProblemKind::UndeclaredPrefix`]), names that are not qualified
//!   names ([`XmlProblemKind::InvalidQualifiedName`]), invalid `xmlns`
//!   declarations and reserved prefixes
//!   ([`XmlProblemKind::InvalidNamespaceDeclaration`]) and attributes with
//!   the same expanded name (reported as
//!   [`XmlProblemKind::DuplicateAttribute`]).
//!
//! Offsets are UTF-8 byte offsets into the source.

use std::collections::HashMap;
use std::ops::Range;

use crate::{
    names::{is_name, is_ncname, is_qname},
    tags::{
        XML_NAMESPACE, XmlAttribute, XmlTag, XmlTagKind, scan_attributes, scan_markup, scan_tags,
    },
};

/// Kind of a well-formedness problem and the data needed to fix it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlProblemKind {
    /// End tag (reported range: its name) that closes the open element
    /// `expected`, whose name is at `start_name`.
    MismatchedEndTag {
        expected: String,
        start_name: Range<usize>,
    },
    /// End tag (reported range: its name) without a matching open element;
    /// `tag` covers the whole tag.
    UnmatchedEndTag { tag: Range<usize> },
    /// Element (reported range: the start tag name) without an end tag.
    /// `insert_at` is where the end tag is expected and `tag_end` the
    /// position of the start tag's `>`.
    UnclosedElement { insert_at: usize, tag_end: usize },
    /// Tag (reported range: its name) without a final `>`; `insert_at`
    /// follows the last significant character of the tag. `start_tag` marks a
    /// start tag (which may also become self-closing).
    UnclosedTag { insert_at: usize, start_tag: bool },
    /// Attribute (reported range: its name) already present on the tag;
    /// `removal` covers the attribute and the whitespace before it.
    DuplicateAttribute { removal: Range<usize> },
    /// Attribute value (reported range) without quotes.
    UnquotedAttributeValue,
    /// `&` or `<` character (reported range) to replace with `&amp;` or
    /// `&lt;`.
    UnescapedCharacter { character: char },
    /// Namespace prefix (reported range: the prefix) of an element or
    /// attribute name that no `xmlns:prefix` declaration in scope binds.
    UndeclaredPrefix { prefix: String },
    /// Name (reported range) that is a well-formed name but not a
    /// qualified name (`a:b:c`, `a:`, `:a`).
    InvalidQualifiedName,
    /// `xmlns` attribute (reported range: its name) breaking a constraint
    /// of Namespaces in XML: reserved prefix or namespace, empty namespace
    /// bound to a prefix.
    InvalidNamespaceDeclaration,
}

impl XmlProblemKind {
    /// Stable identifier of the problem, published in `data.kind` of LSP
    /// diagnostics.
    pub fn id(&self) -> &'static str {
        match self {
            Self::MismatchedEndTag { .. } => "mismatchedEndTag",
            Self::UnmatchedEndTag { .. } => "unmatchedEndTag",
            Self::UnclosedElement { .. } => "unclosedElement",
            Self::UnclosedTag { .. } => "unclosedTag",
            Self::DuplicateAttribute { .. } => "duplicateAttribute",
            Self::UnquotedAttributeValue => "unquotedAttributeValue",
            Self::UnescapedCharacter { .. } => "unescapedCharacter",
            Self::UndeclaredPrefix { .. } => "undeclaredPrefix",
            Self::InvalidQualifiedName => "invalidQualifiedName",
            Self::InvalidNamespaceDeclaration => "invalidNamespaceDeclaration",
        }
    }

    /// Whether this is a structure problem (tag matching) rather than a
    /// syntax problem.
    pub fn is_structural(&self) -> bool {
        matches!(
            self,
            Self::MismatchedEndTag { .. }
                | Self::UnmatchedEndTag { .. }
                | Self::UnclosedElement { .. }
        )
    }
}

/// Located well-formedness problem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlProblem {
    pub kind: XmlProblemKind,
    /// Reported range (see [`XmlProblemKind`]).
    pub range: Range<usize>,
    pub message: String,
}

/// Reports the well-formedness problems of the source, in document
/// order.
pub fn check_well_formedness(source: &str) -> Vec<XmlProblem> {
    // The DOCTYPE is lexed by the strict check and the DTD parser: its
    // literals and comments must not look like tags.
    let dtd_may_declare_prefixes = dtd_may_declare_prefixes(source);
    let masked = crate::strict::mask_doctype(source);
    let source = masked.as_ref();
    let tags = scan_tags(source);
    let mut problems = Vec::new();
    check_tags(source, &tags, &mut problems);
    for tag in &tags {
        check_attributes(source, tag, &mut problems);
    }
    check_text(source, &tags, &mut problems);
    check_namespaces(source, &tags, dtd_may_declare_prefixes, &mut problems);
    problems.sort_by_key(|problem| (problem.range.start, problem.range.end));
    problems
}

fn check_tags(source: &str, tags: &[XmlTag], problems: &mut Vec<XmlProblem>) {
    let mut open: Vec<usize> = Vec::new();
    let unclosed = |index: usize, insert_at: usize, problems: &mut Vec<XmlProblem>| {
        let tag: &XmlTag = &tags[index];
        // A start tag without `>` is already reported.
        if tag.closed {
            problems.push(XmlProblem {
                kind: XmlProblemKind::UnclosedElement {
                    insert_at,
                    tag_end: tag.range.end - 1,
                },
                range: tag.name.clone(),
                message: format!("unclosed element <{}>", tag.name(source)),
            });
        }
    };

    for (index, tag) in tags.iter().enumerate() {
        let name = tag.name(source);
        if !tag.closed {
            let insert_at = source[..tag.range.end.min(source.len())].trim_end().len();
            let (start_tag, label) = match tag.kind {
                XmlTagKind::End => (false, format!("end tag </{name}>")),
                _ => (true, format!("tag <{name}>")),
            };
            problems.push(XmlProblem {
                kind: XmlProblemKind::UnclosedTag {
                    insert_at: insert_at.max(tag.name.end),
                    start_tag,
                },
                range: tag.name.clone(),
                message: format!("{label} is not terminated: missing `>`"),
            });
        }
        match tag.kind {
            XmlTagKind::Start => open.push(index),
            XmlTagKind::SelfClosing => {}
            XmlTagKind::End => {
                if let Some(position) = open
                    .iter()
                    .rposition(|&candidate| tags[candidate].name(source) == name)
                {
                    for &inner in open[position + 1..].iter().rev() {
                        unclosed(inner, tag.range.start, problems);
                    }
                    open.truncate(position);
                    continue;
                }
                let Some(&top) = open.last() else {
                    problems.push(unmatched(source, tag));
                    continue;
                };
                // If the next end tag closes the open element, this one is
                // extra; otherwise it was meant for the open element.
                let expected = tags[top].name(source);
                let next_closes_top = tags[index + 1..]
                    .iter()
                    .find(|next| next.kind == XmlTagKind::End)
                    .is_some_and(|next| next.name(source) == expected);
                if next_closes_top {
                    problems.push(unmatched(source, tag));
                } else {
                    problems.push(XmlProblem {
                        kind: XmlProblemKind::MismatchedEndTag {
                            expected: expected.to_owned(),
                            start_name: tags[top].name.clone(),
                        },
                        range: tag.name.clone(),
                        message: format!("end tag </{name}> does not match </{expected}>"),
                    });
                    open.pop();
                }
            }
        }
    }
    for &index in open.iter().rev() {
        unclosed(index, source.len(), problems);
    }
}

fn unmatched(source: &str, tag: &XmlTag) -> XmlProblem {
    XmlProblem {
        kind: XmlProblemKind::UnmatchedEndTag {
            tag: tag.range.clone(),
        },
        range: tag.name.clone(),
        message: format!("unexpected end tag </{}>", tag.name(source)),
    }
}

fn check_attributes(source: &str, tag: &XmlTag, problems: &mut Vec<XmlProblem>) {
    let attributes = scan_attributes(source, tag);
    let bytes = source.as_bytes();
    for (index, attribute) in attributes.iter().enumerate() {
        let name = attribute.name(source);
        if attributes[..index]
            .iter()
            .any(|previous| previous.name(source) == name)
        {
            problems.push(duplicate_attribute(
                source,
                attribute,
                format!("duplicate attribute {name} on <{}>", tag.name(source)),
            ));
        }
        let Some(value) = &attribute.value else {
            continue;
        };
        if !is_quote(value.start.checked_sub(1).and_then(|at| bytes.get(at))) {
            problems.push(XmlProblem {
                kind: XmlProblemKind::UnquotedAttributeValue,
                range: value.clone(),
                message: format!("unquoted value for attribute {name}"),
            });
            continue;
        }
        check_characters(source, value.clone(), problems);
    }
}

fn duplicate_attribute(source: &str, attribute: &XmlAttribute, message: String) -> XmlProblem {
    let bytes = source.as_bytes();
    let start = source[..attribute.name.start].trim_end().len();
    let end = match &attribute.value {
        Some(value) if is_quote(bytes.get(value.end)) => value.end + 1,
        Some(value) => value.end,
        None => attribute.name.end,
    };
    XmlProblem {
        kind: XmlProblemKind::DuplicateAttribute {
            removal: start..end,
        },
        range: attribute.name.clone(),
        message,
    }
}

fn is_quote(byte: Option<&u8>) -> bool {
    matches!(byte, Some(b'"' | b'\''))
}

/// Text outside tags, comments, CDATA, processing instructions and
/// declarations.
fn check_text(source: &str, tags: &[XmlTag], problems: &mut Vec<XmlProblem>) {
    let mut constructs = tags
        .iter()
        .map(|tag| tag.range.clone())
        .chain(scan_markup(source).into_iter().map(|markup| markup.range))
        .collect::<Vec<_>>();
    constructs.sort_by_key(|range| range.start);
    let mut position = 0;
    for construct in constructs {
        if construct.start > position {
            check_characters(source, position..construct.start, problems);
        }
        position = position.max(construct.end);
    }
    if position < source.len() {
        check_characters(source, position..source.len(), problems);
    }
}

fn check_characters(source: &str, range: Range<usize>, problems: &mut Vec<XmlProblem>) {
    let text = &source[range.clone()];
    for (index, character) in text.char_indices() {
        let escaped = match character {
            '<' => "&lt;",
            '&' if !starts_with_reference(&text[index..]) => "&amp;",
            _ => continue,
        };
        let start = range.start + index;
        problems.push(XmlProblem {
            kind: XmlProblemKind::UnescapedCharacter { character },
            range: start..start + 1,
            message: format!("unescaped `{character}` character (use `{escaped}`)"),
        });
    }
}

/// Whether `text` (starting with `&`) begins with a complete entity or
/// character reference (`&name;`, `&#10;`, `&#x1F;`).
fn starts_with_reference(text: &str) -> bool {
    let Some(end) = text.find(';') else {
        return false;
    };
    let body = &text[1..end];
    if let Some(hex) = body.strip_prefix("#x") {
        return !hex.is_empty() && hex.chars().all(|character| character.is_ascii_hexdigit());
    }
    if let Some(decimal) = body.strip_prefix('#') {
        return !decimal.is_empty() && decimal.chars().all(|character| character.is_ascii_digit());
    }
    let mut characters = body.chars();
    characters.next().is_some_and(|first| {
        first.is_alphabetic() || first == '_' || first == ':' || !first.is_ascii()
    }) && characters.all(|character| {
        character.is_alphanumeric()
            || matches!(character, '_' | ':' | '.' | '-')
            || !character.is_ascii()
    })
}

/// Namespace of the `xmlns` prefix, which cannot be bound or declared.
const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";

/// A DTD can declare default `xmlns:*` attributes that the document does not
/// show: prefixes are then not checked.
fn dtd_may_declare_prefixes(source: &str) -> bool {
    source.find("<!DOCTYPE").is_some_and(|start| {
        let doctype = &source[start..];
        doctype[..doctype.find("]>").unwrap_or(doctype.len())].contains("xmlns:")
    })
}

/// Checks the constraints of Namespaces in XML 1.0 on element and attribute
/// names and on `xmlns` declarations, walking the tags once with a stack of
/// the bindings in scope.
fn check_namespaces(
    source: &str,
    tags: &[XmlTag],
    dtd_may_declare_prefixes: bool,
    problems: &mut Vec<XmlProblem>,
) {
    // `prefix -> stack of namespaces` ("" is the default namespace).
    let mut bindings: HashMap<&str, Vec<String>> = HashMap::new();
    let mut open: Vec<Vec<&str>> = Vec::new();

    for tag in tags {
        if tag.kind == XmlTagKind::End {
            for prefix in open.pop().unwrap_or_default() {
                if let Some(stack) = bindings.get_mut(prefix) {
                    stack.pop();
                }
            }
            continue;
        }
        let attributes = scan_attributes(source, tag);
        let mut declared = Vec::new();
        for attribute in &attributes {
            let name = attribute.name(source);
            let prefix = if name == "xmlns" {
                ""
            } else if let Some(prefix) = name.strip_prefix("xmlns:") {
                prefix
            } else {
                continue;
            };
            let raw_value = attribute.value.clone().map_or("", |range| &source[range]);
            // Character and predefined entity references are resolved before
            // namespaces are compared.
            let value = decode_references(raw_value);
            if !prefix.is_empty() && !is_ncname(prefix) && is_name(name) {
                problems.push(XmlProblem {
                    kind: XmlProblemKind::InvalidQualifiedName,
                    range: attribute.name.clone(),
                    message: format!("{name} is not a valid namespace declaration (xmlns:NCName)"),
                });
            } else if let Some(message) = declaration_problem(prefix, &value) {
                problems.push(XmlProblem {
                    kind: XmlProblemKind::InvalidNamespaceDeclaration,
                    range: attribute.name.clone(),
                    message,
                });
            }
            bindings.entry(prefix).or_default().push(value);
            declared.push(prefix);
        }

        let in_scope = |prefix: &str| {
            prefix == "xml"
                || dtd_may_declare_prefixes
                || bindings
                    .get(prefix)
                    .is_some_and(|stack| stack.last().is_some_and(|value| !value.is_empty()))
        };
        check_name(source, tag.name.clone(), false, &in_scope, problems);
        // `(namespace, local name, name as written)` of the prefixed
        // attributes seen so far.
        let mut expanded: Vec<(&str, &str, &str)> = Vec::new();
        for attribute in &attributes {
            let name = attribute.name(source);
            if name == "xmlns" || name.starts_with("xmlns:") {
                continue;
            }
            check_name(source, attribute.name.clone(), true, &in_scope, problems);
            let Some((prefix, local)) = name.split_once(':') else {
                continue;
            };
            let namespace = if prefix == "xml" {
                Some(XML_NAMESPACE)
            } else {
                bindings
                    .get(prefix)
                    .and_then(|stack| stack.last().map(String::as_str))
            };
            let Some(namespace) = namespace else {
                continue;
            };
            match expanded
                .iter()
                .find(|(other, other_local, _)| *other == namespace && *other_local == local)
            {
                Some((_, _, first)) => problems.push(duplicate_attribute(
                    source,
                    attribute,
                    format!(
                        "duplicate attribute {name} on <{}> (same expanded name as {first})",
                        tag.name(source)
                    ),
                )),
                None => expanded.push((namespace, local, name)),
            }
        }

        if tag.kind == XmlTagKind::Start {
            open.push(declared);
        } else {
            for prefix in declared {
                if let Some(stack) = bindings.get_mut(prefix) {
                    stack.pop();
                }
            }
        }
    }
}

/// The text with its character references and the five predefined entity
/// references replaced by the characters they stand for; other references
/// are kept.
fn decode_references(text: &str) -> String {
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

/// Constraint broken by the declaration of `prefix` (`""` for the default
/// namespace) as `value`, if any (Namespaces in XML 1.0 §3, §6.2).
fn declaration_problem(prefix: &str, value: &str) -> Option<String> {
    if prefix == "xmlns" {
        return Some("the prefix xmlns is reserved and cannot be declared".to_owned());
    }
    if value == XMLNS_NAMESPACE {
        return Some(format!(
            "the namespace {XMLNS_NAMESPACE} is reserved and cannot be declared"
        ));
    }
    if prefix == "xml" {
        return (value != XML_NAMESPACE)
            .then(|| format!("the prefix xml can only be bound to {XML_NAMESPACE}"));
    }
    if value == XML_NAMESPACE {
        return Some(format!(
            "the namespace {XML_NAMESPACE} can only be bound to the prefix xml"
        ));
    }
    (!prefix.is_empty() && value.is_empty())
        .then(|| format!("the prefix {prefix} cannot be bound to an empty namespace"))
}

/// Checks a tag or attribute name: qualified name syntax, reserved and
/// undeclared prefixes.
fn check_name(
    source: &str,
    name: Range<usize>,
    attribute: bool,
    in_scope: &dyn Fn(&str) -> bool,
    problems: &mut Vec<XmlProblem>,
) {
    let text = &source[name.clone()];
    if !is_name(text) {
        return;
    }
    if !is_qname(text) {
        problems.push(XmlProblem {
            kind: XmlProblemKind::InvalidQualifiedName,
            range: name,
            message: format!("{text} is not a valid qualified name (NCName:NCName)"),
        });
        return;
    }
    let Some((prefix, _)) = text.split_once(':') else {
        return;
    };
    if !is_ncname(prefix) {
        return;
    }
    let range = name.start..name.start + prefix.len();
    if prefix == "xmlns" && !attribute {
        problems.push(XmlProblem {
            kind: XmlProblemKind::InvalidNamespaceDeclaration,
            range,
            message: "the prefix xmlns is reserved and cannot be used on an element".to_owned(),
        });
    } else if !in_scope(prefix) {
        problems.push(XmlProblem {
            kind: XmlProblemKind::UndeclaredPrefix {
                prefix: prefix.to_owned(),
            },
            range,
            message: format!("namespace prefix {prefix} is not declared"),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<(&'static str, &str)> {
        check_well_formedness(source)
            .into_iter()
            .map(|problem| (problem.kind.id(), &source[problem.range]))
            .collect()
    }

    #[test]
    fn accepts_well_formed_documents() {
        let source = "<?xml version=\"1.0\"?>\n<!DOCTYPE r [<!ENTITY e \"&#38;\">]>\n<r a=\"1 &amp; 2\" b='&#x3C;'><!-- a < b & c --><![CDATA[<&>]]><x/>&e; &#10;</r>\n";
        assert_eq!(kinds(source), vec![]);
    }

    #[test]
    fn reports_mismatched_and_unmatched_end_tags() {
        let source = "<root><child></chidl></root>";
        let problems = check_well_formedness(source);
        assert_eq!(problems.len(), 1);
        assert_eq!(&source[problems[0].range.clone()], "chidl");
        assert_eq!(
            problems[0].kind,
            XmlProblemKind::MismatchedEndTag {
                expected: "child".to_owned(),
                start_name: 7..12,
            }
        );
        assert_eq!(
            problems[0].message,
            "end tag </chidl> does not match </child>"
        );

        // The next tag closes the open element: this one is extra.
        let source = "<root></extra></root>";
        assert_eq!(kinds(source), vec![("unmatchedEndTag", "extra")]);
        assert_eq!(kinds("<a/></b>"), vec![("unmatchedEndTag", "b")]);
        assert_eq!(kinds("<a></b>"), vec![("mismatchedEndTag", "b")]);
    }

    #[test]
    fn reports_unclosed_elements_with_their_insertion_point() {
        let source = "<root>\n  <item>\n  <other/>\n</root>";
        let problems = check_well_formedness(source);
        assert_eq!(problems.len(), 1);
        assert_eq!(&source[problems[0].range.clone()], "item");
        assert_eq!(
            problems[0].kind,
            XmlProblemKind::UnclosedElement {
                insert_at: source.find("</root>").unwrap(),
                tag_end: source.find("<item>").unwrap() + 5,
            }
        );

        let source = "<root><child>";
        assert_eq!(
            kinds(source),
            vec![("unclosedElement", "root"), ("unclosedElement", "child")]
        );
        assert!(check_well_formedness(source).iter().all(|problem| matches!(
            problem.kind,
            XmlProblemKind::UnclosedElement { insert_at, .. } if insert_at == source.len()
        )));
    }

    #[test]
    fn reports_tags_missing_their_closing_bracket() {
        let source = "<a><b x=\"1\"  \n</a>";
        let problems = check_well_formedness(source);
        assert_eq!(problems.len(), 1);
        assert_eq!(
            problems[0].kind,
            XmlProblemKind::UnclosedTag {
                insert_at: source.find("  \n").unwrap(),
                start_tag: true,
            }
        );
        assert_eq!(kinds("<a></a"), vec![("unclosedTag", "a")]);
    }

    #[test]
    fn reports_duplicate_attributes_and_unquoted_values() {
        let source = "<a x=\"1\" y=2 x='3'/>";
        let problems = check_well_formedness(source);
        assert_eq!(
            problems
                .iter()
                .map(|problem| (problem.kind.id(), &source[problem.range.clone()]))
                .collect::<Vec<_>>(),
            vec![("unquotedAttributeValue", "2"), ("duplicateAttribute", "x")]
        );
        let XmlProblemKind::DuplicateAttribute { removal } = &problems[1].kind else {
            panic!("expected a duplicate attribute");
        };
        assert_eq!(&source[removal.clone()], " x='3'");
    }

    #[test]
    fn reports_unescaped_characters_in_text_and_attribute_values() {
        let source = "<a t=\"x < y &\">1 < 2 & 3 &amp; &bad &#xZ;</a>";
        assert_eq!(
            kinds(source),
            vec![
                ("unescapedCharacter", "<"),
                ("unescapedCharacter", "&"),
                ("unescapedCharacter", "<"),
                ("unescapedCharacter", "&"),
                ("unescapedCharacter", "&"),
                ("unescapedCharacter", "&"),
            ]
        );
        // Unicode and CRLF: offsets stay on character boundaries.
        let source = "<é>\r\n😀 & ü\r\n</é>";
        let problems = check_well_formedness(source);
        assert_eq!(problems.len(), 1);
        assert_eq!(&source[problems[0].range.clone()], "&");
    }

    #[test]
    fn handles_namespaced_names() {
        assert_eq!(
            kinds("<p:a xmlns:p=\"urn:p\"><p:b></p:c></p:a>"),
            vec![("mismatchedEndTag", "p:c")]
        );
    }

    #[test]
    fn reports_undeclared_prefixes_in_scope_order() {
        assert_eq!(kinds("<p:a/>"), vec![("undeclaredPrefix", "p")]);
        assert_eq!(kinds("<a q:x=\"1\"/>"), vec![("undeclaredPrefix", "q")]);
        assert!(kinds("<p:a xmlns:p=\"urn:p\" p:x=\"1\"><p:b/></p:a>").is_empty());
        assert!(kinds("<a xml:lang=\"en\" xmlns:p=\"urn:p\"/>").is_empty());
        // The declaration goes out of scope with its element.
        assert_eq!(
            kinds("<a><b xmlns:p=\"urn:p\"/><p:c/></a>"),
            vec![("undeclaredPrefix", "p")]
        );
        // Prefixes a DTD may declare by default attributes are not checked.
        assert!(
            kinds("<!DOCTYPE a [<!ATTLIST a xmlns:p CDATA #FIXED \"urn:p\">]><a><p:b/></a>")
                .is_empty()
        );
    }

    #[test]
    fn reports_reserved_prefixes_and_namespaces() {
        for source in [
            "<a xmlns:xmlns=\"urn:x\"/>",
            "<a xmlns:xml=\"urn:other\"/>",
            "<a xmlns:p=\"http://www.w3.org/XML/1998/namespace\"/>",
            "<a xmlns:p=\"http://www.w3.org/2000/xmlns/\"/>",
            "<a xmlns:p=\"\"/>",
        ] {
            assert_eq!(
                kinds(source).iter().map(|kind| kind.0).collect::<Vec<_>>(),
                vec!["invalidNamespaceDeclaration"],
                "{source}"
            );
        }
        assert!(kinds("<a xmlns=\"urn:a\"><b xmlns=\"\"/></a>").is_empty());
        assert!(kinds("<a xmlns:xml=\"http://www.w3.org/XML/1998/namespace\"/>").is_empty());
        assert_eq!(kinds("<xmlns:a/>")[0].0, "invalidNamespaceDeclaration");
    }

    #[test]
    fn reports_names_that_are_not_qualified_names() {
        assert_eq!(
            kinds("<a:b:c xmlns:a=\"urn:a\"/>")[0].0,
            "invalidQualifiedName"
        );
        assert_eq!(
            kinds("<a: xmlns:a=\"urn:a\"/>")[0].0,
            "invalidQualifiedName"
        );
    }

    #[test]
    fn reports_attributes_with_the_same_expanded_name() {
        let source = "<a xmlns:p=\"urn:u\" xmlns:q=\"urn:u\" p:x=\"1\" q:x=\"2\"/>";
        let problems = check_well_formedness(source);
        assert_eq!(problems.len(), 1);
        assert!(matches!(
            problems[0].kind,
            XmlProblemKind::DuplicateAttribute { .. }
        ));
        assert!(problems[0].message.contains("same expanded name as p:x"));
        assert!(
            kinds("<a xmlns:p=\"urn:p\" xmlns:q=\"urn:q\" p:x=\"1\" q:x=\"2\" x=\"3\"/>")
                .is_empty()
        );
    }
}
