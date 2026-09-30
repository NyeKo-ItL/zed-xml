//! `textDocument/prepareRename` and `textDocument/rename`, like LemMinX and
//! IntelliJ:
//!
//! - element name: the start tag and the end tag of the pair;
//! - namespace prefix (`ns` in `<ns:a>`, `ns:attr`, `type="ns:T"` or
//!   `xmlns:ns`): the declaration and all the uses bound to it, honouring
//!   shadowing by a nested redeclaration;
//! - named global XSD component (`xs:element`, `xs:attribute`,
//!   `xs:complexType`, `xs:simpleType`, `xs:group`, `xs:attributeGroup`):
//!   the `name` attribute and the `ref`, `type`, `base`, `itemType`,
//!   `memberTypes` and `substitutionGroup` references of the schema. Open
//!   instance documents are updated by [`instance_ranges`].
//!
//! All offsets are UTF-8 byte offsets.

use std::ops::Range;

use serde_json::{Value, json};
use xml_core::tags::{
    XmlAttribute, XmlTagTree, namespace_declaration, qualified_name_parts, resolve_namespace,
    scan_attributes,
};

use crate::position_at;

const XSD_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema";
const XSI_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema-instance";

/// JSON-RPC `InvalidParams` error code.
pub const INVALID_PARAMS: i32 = -32602;
/// LSP `RequestFailed` error code.
pub const REQUEST_FAILED: i32 = -32803;

/// Rename error sent back to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameError {
    pub code: i32,
    pub message: String,
}

impl RenameError {
    fn invalid(message: String) -> Self {
        Self {
            code: INVALID_PARAMS,
            message,
        }
    }
}

/// Symbol space of a named XSD component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentKind {
    Element,
    Attribute,
    /// `xs:complexType` and `xs:simpleType` share the same space.
    Type,
    Group,
    AttributeGroup,
}

/// Renamed global XSD component, to propagate to instance documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenamedComponent {
    pub kind: ComponentKind,
    /// `targetNamespace` of the schema (`None` without a target namespace).
    pub namespace: Option<String>,
    pub old_name: String,
}

/// Result of a rename in the current document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenamePlan {
    /// Ranges to replace with the new name, sorted and without duplicates.
    pub ranges: Vec<Range<usize>>,
    /// Renamed global XSD component, if any.
    pub component: Option<RenamedComponent>,
}

/// Returns `{range, placeholder}` for the symbol under the cursor, or `None`
/// if no rename is possible there.
pub fn prepare_rename(source: &str, offset: usize) -> Option<Value> {
    let document = Document::parse(source);
    let target = document.target_at(offset)?;
    let range = target.range();
    Some(json!({
        "range": {
            "start": position_at(source, range.start),
            "end": position_at(source, range.end),
        },
        "placeholder": &source[range],
    }))
}

/// Computes the ranges to rename for the symbol under the cursor.
///
/// Returns `Ok(None)` if no rename is possible there, and an error if
/// `new_name` is not valid for this symbol.
pub fn rename(
    source: &str,
    offset: usize,
    new_name: &str,
) -> Result<Option<RenamePlan>, RenameError> {
    let document = Document::parse(source);
    let Some(target) = document.target_at(offset) else {
        return Ok(None);
    };
    let plan = match target {
        Target::Element { names, .. } => {
            if !is_qname(new_name) {
                return Err(RenameError::invalid(format!(
                    "'{new_name}' is not a valid XML element name."
                )));
            }
            RenamePlan {
                ranges: names,
                component: None,
            }
        }
        Target::Prefix { binding, range } => {
            document.validate_prefix(binding, &source[range.clone()], new_name)?;
            RenamePlan {
                ranges: document.prefix_ranges(binding, &source[range]),
                component: None,
            }
        }
        Target::Component(component) => {
            if !is_ncname(new_name) {
                return Err(RenameError::invalid(format!(
                    "'{new_name}' is not a valid XSD component name (NCName expected)."
                )));
            }
            document.component_plan(&component)
        }
    };
    Ok(Some(normalize(plan)))
}

/// Ranges to rename in an open instance document referencing the schema
/// where `component` was renamed: local names of elements and qualified
/// global attributes, and `xsi:type` values.
pub fn instance_ranges(source: &str, component: &RenamedComponent) -> Vec<Range<usize>> {
    let document = Document::parse(source);
    let namespace = component.namespace.as_deref();
    let name = component.old_name.as_str();
    let mut ranges = Vec::new();
    for (index, element) in document.tree.elements().iter().enumerate() {
        match component.kind {
            ComponentKind::Element => {
                let (prefix, local) = document.name_parts(element.start_tag.name.clone());
                if &source[local.clone()] != name
                    || document.namespace(index, prefix) != Some(namespace)
                {
                    continue;
                }
                ranges.push(local);
                if let Some(end_tag) = &element.end_tag {
                    ranges.push(qualified_name_parts(source, end_tag.name.clone()).1);
                }
            }
            ComponentKind::Attribute if namespace.is_some() => {
                for attribute in &document.attributes[index] {
                    let (prefix, local) = document.name_parts(attribute.name.clone());
                    if prefix.is_some_and(|prefix| prefix != "xmlns")
                        && &source[local.clone()] == name
                        && document.namespace(index, prefix) == Some(namespace)
                    {
                        ranges.push(local);
                    }
                }
            }
            ComponentKind::Type => {
                for token in document.qname_tokens(index) {
                    if token.kind == Some(ComponentKind::Type)
                        && document.token_matches(index, &token, namespace, name)
                    {
                        ranges.push(token.local.clone());
                    }
                }
            }
            _ => {}
        }
    }
    ranges.sort_by_key(|range| range.start);
    ranges.dedup();
    ranges
}

/// Converts ranges into LSP `TextEdit`s replaced by `new_text`.
pub fn text_edits(source: &str, ranges: &[Range<usize>], new_text: &str) -> Vec<Value> {
    let lines = crate::selection::LineIndex::new(source);
    ranges
        .iter()
        .map(|range| {
            json!({
                "range": {
                    "start": lines.position(source, range.start),
                    "end": lines.position(source, range.end),
                },
                "newText": new_text,
            })
        })
        .collect()
}

fn normalize(mut plan: RenamePlan) -> RenamePlan {
    plan.ranges.sort_by_key(|range| range.start);
    plan.ranges.dedup();
    plan
}

/// Renameable symbol under the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    /// Full qualified names of the tags of the pair (start tag first).
    Element {
        names: Vec<Range<usize>>,
        cursor: Range<usize>,
    },
    /// Prefix bound to the `xmlns:prefix` declaration of the `binding` element.
    Prefix { binding: usize, range: Range<usize> },
    /// Named XSD component.
    Component(Component),
}

impl Target {
    /// Range presented to the client (always contains the cursor).
    fn range(&self) -> Range<usize> {
        match self {
            Self::Element { cursor, .. } => cursor.clone(),
            Self::Prefix { range, .. } => range.clone(),
            Self::Component(component) => component.cursor.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Component {
    kind: ComponentKind,
    /// Declaration element (`xs:element`, `xs:complexType`...).
    declaration: usize,
    /// Value of the `name` attribute of the declaration.
    name: Range<usize>,
    /// Global declaration (direct child of `xs:schema`).
    global: bool,
    /// Range under the cursor (the declared name or the referenced local name).
    cursor: Range<usize>,
}

/// Prefix used in the document.
#[derive(Debug, Clone)]
struct PrefixUse {
    /// Element in whose context the prefix is resolved.
    element: usize,
    /// Range of the prefix (without `:`).
    range: Range<usize>,
    /// `true` for the local name of an `xmlns:prefix` declaration.
    declaration: bool,
}

/// Qualified name found in the value of a `QName`-typed attribute.
#[derive(Debug, Clone)]
struct QNameToken {
    prefix: Option<Range<usize>>,
    local: Range<usize>,
    /// Referenced symbol space (`None` for `xs:keyref/@refer`).
    kind: Option<ComponentKind>,
}

struct Document<'a> {
    source: &'a str,
    tree: XmlTagTree,
    /// Attributes of the start tag of each element.
    attributes: Vec<Vec<XmlAttribute>>,
}

impl<'a> Document<'a> {
    fn parse(source: &'a str) -> Self {
        let tree = XmlTagTree::parse(source);
        let attributes = tree
            .elements()
            .iter()
            .map(|element| scan_attributes(source, &element.start_tag))
            .collect();
        Self {
            source,
            tree,
            attributes,
        }
    }

    fn name_parts(&self, name: Range<usize>) -> (Option<&'a str>, Range<usize>) {
        let (prefix, local) = qualified_name_parts(self.source, name);
        (prefix.map(|range| &self.source[range]), local)
    }

    fn local_name(&self, element: usize) -> &'a str {
        let (_, local) = self.name_parts(self.tree.elements()[element].start_tag.name.clone());
        &self.source[local]
    }

    /// Nearest element (itself included) declaring `prefix`
    /// (`None`: default namespace).
    fn declaration(&self, element: usize, prefix: Option<&str>) -> Option<(usize, &XmlAttribute)> {
        namespace_declaration(self.source, &self.tree, &self.attributes, element, prefix)
    }

    /// Namespace of `prefix` in the context of `element`. Returns `None`
    /// for an undeclared prefix and `Some(None)` for no namespace.
    fn namespace(&self, element: usize, prefix: Option<&str>) -> Option<Option<&'a str>> {
        resolve_namespace(self.source, &self.tree, &self.attributes, element, prefix)
    }

    fn is_xsd(&self, element: usize, local: &str) -> bool {
        let (prefix, name) = self.name_parts(self.tree.elements()[element].start_tag.name.clone());
        &self.source[name] == local && self.namespace(element, prefix) == Some(Some(XSD_NAMESPACE))
    }

    /// Unprefixed attribute `name` of the element.
    fn attribute(&self, element: usize, name: &str) -> Option<&XmlAttribute> {
        self.attributes[element]
            .iter()
            .find(|attribute| attribute.name(self.source) == name)
    }

    /// Qualified names contained in the `QName`-typed attributes of the
    /// element: XSD references and `xsi:type`.
    fn qname_tokens(&self, element: usize) -> Vec<QNameToken> {
        let mut tokens = Vec::new();
        let xsd_local = self
            .is_xsd(element, self.local_name(element))
            .then(|| self.local_name(element));
        for attribute in &self.attributes[element] {
            let Some(value) = attribute.value.clone() else {
                continue;
            };
            let (prefix, local) = self.name_parts(attribute.name.clone());
            let local = &self.source[local];
            let kind = match (prefix, xsd_local) {
                (None, Some(element)) => match (element, local) {
                    ("element", "ref" | "substitutionGroup") => Some(ComponentKind::Element),
                    ("element" | "attribute", "type")
                    | ("restriction" | "extension", "base")
                    | ("list", "itemType")
                    | ("union", "memberTypes") => Some(ComponentKind::Type),
                    ("attribute", "ref") => Some(ComponentKind::Attribute),
                    ("group", "ref") => Some(ComponentKind::Group),
                    ("attributeGroup", "ref") => Some(ComponentKind::AttributeGroup),
                    ("keyref", "refer") => None,
                    _ => continue,
                },
                (Some(prefix), _)
                    if local == "type"
                        && self.namespace(element, Some(prefix)) == Some(Some(XSI_NAMESPACE)) =>
                {
                    Some(ComponentKind::Type)
                }
                _ => continue,
            };
            let text = &self.source[value.clone()];
            let mut position = 0;
            for token in text.split_ascii_whitespace() {
                let start = value.start + position + text[position..].find(token).unwrap_or(0);
                position = start + token.len() - value.start;
                let (prefix, local) = qualified_name_parts(self.source, start..start + token.len());
                if local.is_empty() || prefix.as_ref().is_some_and(Range::is_empty) {
                    continue;
                }
                tokens.push(QNameToken {
                    prefix,
                    local,
                    kind,
                });
            }
        }
        tokens
    }

    /// Whether `token` designates `name` in the namespace `namespace`.
    fn token_matches(
        &self,
        element: usize,
        token: &QNameToken,
        namespace: Option<&str>,
        name: &str,
    ) -> bool {
        let prefix = token.prefix.clone().map(|range| &self.source[range]);
        &self.source[token.local.clone()] == name
            && self.namespace(element, prefix) == Some(namespace)
    }

    /// All prefix uses of the document.
    fn prefix_uses(&self) -> Vec<PrefixUse> {
        let mut uses = Vec::new();
        let mut push = |element: usize, range: Option<Range<usize>>, declaration: bool| {
            if let Some(range) = range.filter(|range| !range.is_empty()) {
                uses.push(PrefixUse {
                    element,
                    range,
                    declaration,
                });
            }
        };
        for (index, element) in self.tree.elements().iter().enumerate() {
            push(
                index,
                qualified_name_parts(self.source, element.start_tag.name.clone()).0,
                false,
            );
            if let Some(end_tag) = &element.end_tag {
                push(
                    index,
                    qualified_name_parts(self.source, end_tag.name.clone()).0,
                    false,
                );
            }
            for attribute in &self.attributes[index] {
                let (prefix, local) = qualified_name_parts(self.source, attribute.name.clone());
                match prefix {
                    Some(prefix) if &self.source[prefix.clone()] == "xmlns" => {
                        push(index, Some(local), true);
                    }
                    Some(prefix) => push(index, Some(prefix), false),
                    None => {}
                }
            }
            for token in self.qname_tokens(index) {
                push(index, token.prefix, false);
            }
        }
        for tag in self.tree.orphan_end_tags() {
            if let Some(element) = self.tree.innermost_element_at(tag.range.start) {
                push(
                    element,
                    qualified_name_parts(self.source, tag.name.clone()).0,
                    false,
                );
            }
        }
        uses
    }

    /// Element whose `xmlns:prefix` declaration binds the use `use_`.
    fn binding(&self, use_: &PrefixUse) -> Option<usize> {
        if use_.declaration {
            return Some(use_.element);
        }
        let prefix = &self.source[use_.range.clone()];
        self.declaration(use_.element, Some(prefix))
            .map(|(index, _)| index)
    }

    fn prefix_ranges(&self, binding: usize, prefix: &str) -> Vec<Range<usize>> {
        self.prefix_uses()
            .into_iter()
            .filter(|use_| {
                &self.source[use_.range.clone()] == prefix && self.binding(use_) == Some(binding)
            })
            .map(|use_| use_.range)
            .collect()
    }

    fn validate_prefix(&self, binding: usize, old: &str, new: &str) -> Result<(), RenameError> {
        if !is_ncname(new) {
            return Err(RenameError::invalid(format!(
                "'{new}' is not a valid namespace prefix."
            )));
        }
        if new.eq_ignore_ascii_case("xml") || new.eq_ignore_ascii_case("xmlns") {
            return Err(RenameError::invalid(format!(
                "The prefix '{new}' is reserved."
            )));
        }
        let declared = self.attributes[binding]
            .iter()
            .any(|attribute| attribute.name(self.source).strip_prefix("xmlns:") == Some(new));
        if new != old && declared {
            return Err(RenameError {
                code: REQUEST_FAILED,
                message: format!(
                    "The prefix '{new}' is already declared on the element <{}>.",
                    self.tree.elements()[binding].name(self.source)
                ),
            });
        }
        Ok(())
    }

    fn target_at(&self, offset: usize) -> Option<Target> {
        if let Some(target) = self.prefix_target_at(offset) {
            return Some(target);
        }
        if let Some(pair) = self.tree.tag_pair_at(offset) {
            let names: Vec<_> = pair.name_ranges().collect();
            if names.iter().any(Range::is_empty) {
                return None;
            }
            let cursor = names
                .iter()
                .find(|range| range.start <= offset && offset <= range.end)
                .unwrap_or(&names[0])
                .clone();
            return Some(Target::Element { names, cursor });
        }
        self.component_target_at(offset).map(Target::Component)
    }

    fn prefix_target_at(&self, offset: usize) -> Option<Target> {
        let use_ = self
            .prefix_uses()
            .into_iter()
            .find(|use_| use_.range.start <= offset && offset <= use_.range.end)?;
        let prefix = &self.source[use_.range.clone()];
        if prefix == "xml" || prefix == "xmlns" {
            return None;
        }
        let binding = self.binding(&use_)?;
        Some(Target::Prefix {
            binding,
            range: use_.range,
        })
    }

    fn component_target_at(&self, offset: usize) -> Option<Component> {
        let contains = |range: &Range<usize>| range.start <= offset && offset <= range.end;
        for index in 0..self.tree.elements().len() {
            if self.tree.elements()[index].start_tag.range.start > offset {
                break;
            }
            if let Some(component) = self.component_declaration(index)
                && contains(&component.name)
            {
                return Some(component);
            }
            for token in self.qname_tokens(index) {
                let Some(kind) = token.kind else {
                    continue;
                };
                if !contains(&token.local) {
                    continue;
                }
                let prefix = token.prefix.clone().map(|range| &self.source[range]);
                let namespace = self.namespace(index, prefix)?;
                let name = &self.source[token.local.clone()];
                let component = (0..self.tree.elements().len())
                    .filter_map(|candidate| self.component_declaration(candidate))
                    .find(|component| {
                        component.global
                            && component.kind == kind
                            && &self.source[component.name.clone()] == name
                            && self.target_namespace(component.declaration) == namespace
                    })?;
                return Some(Component {
                    cursor: token.local,
                    ..component
                });
            }
        }
        None
    }

    /// Named XSD component declaration carried by the element `index`.
    fn component_declaration(&self, index: usize) -> Option<Component> {
        let local = self.local_name(index);
        let kind = match local {
            "element" => ComponentKind::Element,
            "attribute" => ComponentKind::Attribute,
            "complexType" | "simpleType" => ComponentKind::Type,
            "group" => ComponentKind::Group,
            "attributeGroup" => ComponentKind::AttributeGroup,
            _ => return None,
        };
        if !self.is_xsd(index, local) {
            return None;
        }
        let name = self.attribute(index, "name")?.value.clone()?;
        let global = self.tree.elements()[index]
            .parent
            .is_some_and(|parent| self.is_xsd(parent, "schema"));
        Some(Component {
            kind,
            declaration: index,
            name: name.clone(),
            global,
            cursor: name,
        })
    }

    /// `targetNamespace` of the enclosing `xs:schema`.
    fn target_namespace(&self, element: usize) -> Option<&'a str> {
        let schema = std::iter::once(element)
            .chain(self.tree.ancestors(element))
            .find(|&index| self.is_xsd(index, "schema"))?;
        self.attribute(schema, "targetNamespace")?
            .value
            .clone()
            .map(|range| &self.source[range])
            .filter(|value| !value.is_empty())
    }

    fn component_plan(&self, component: &Component) -> RenamePlan {
        let mut ranges = vec![component.name.clone()];
        if !component.global {
            return RenamePlan {
                ranges,
                component: None,
            };
        }
        let name = &self.source[component.name.clone()];
        let namespace = self.target_namespace(component.declaration);
        for index in 0..self.tree.elements().len() {
            for token in self.qname_tokens(index) {
                if token.kind == Some(component.kind)
                    && self.token_matches(index, &token, namespace, name)
                {
                    ranges.push(token.local);
                }
            }
        }
        RenamePlan {
            ranges,
            component: Some(RenamedComponent {
                kind: component.kind,
                namespace: namespace.map(str::to_owned),
                old_name: name.to_owned(),
            }),
        }
    }
}

/// `NameStartChar` from XML 1.0 5th edition.
fn is_name_start_char(character: char) -> bool {
    matches!(character,
        ':' | 'A'..='Z' | '_' | 'a'..='z'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

/// `NameChar` from XML 1.0 5th edition.
fn is_name_char(character: char) -> bool {
    is_name_start_char(character)
        || matches!(character,
            '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

/// Name without a colon (`NCName` from "Namespaces in XML").
pub fn is_ncname(name: &str) -> bool {
    let mut characters = name.chars();
    characters
        .next()
        .is_some_and(|first| first != ':' && is_name_start_char(first))
        && characters.all(|character| character != ':' && is_name_char(character))
}

/// Qualified name `prefix:local` or `local`.
pub fn is_qname(name: &str) -> bool {
    match name.split_once(':') {
        Some((prefix, local)) => is_ncname(prefix) && is_ncname(local),
        None => is_ncname(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applies the rename and returns the resulting document.
    fn renamed(source: &str, offset: usize, new_name: &str) -> Option<String> {
        let plan = rename(source, offset, new_name).expect("rename should succeed")?;
        Some(apply(source, &plan.ranges, new_name))
    }

    fn apply(source: &str, ranges: &[Range<usize>], new_text: &str) -> String {
        let mut result = source.to_owned();
        for range in ranges.iter().rev() {
            result.replace_range(range.clone(), new_text);
        }
        result
    }

    fn at(source: &str, needle: &str, nth: usize) -> usize {
        source
            .match_indices(needle)
            .nth(nth)
            .map(|(offset, _)| offset)
            .expect("needle should exist")
    }

    fn placeholder(source: &str, offset: usize) -> Option<String> {
        prepare_rename(source, offset)
            .map(|result| result["placeholder"].as_str().unwrap().to_owned())
    }

    #[test]
    fn renames_both_tags_of_an_element() {
        let source = "<root><item a=\"1\">x</item></root>";
        for offset in [7, 9, 11, 21, 25] {
            assert_eq!(
                renamed(source, offset, "entry").as_deref(),
                Some("<root><entry a=\"1\">x</entry></root>"),
                "{offset}"
            );
        }
        assert_eq!(placeholder(source, 9).as_deref(), Some("item"));
        // The returned range is the one of the name under the cursor.
        let result = prepare_rename(source, 21).unwrap();
        assert_eq!(result["range"]["start"]["character"], 21);
        assert_eq!(result["range"]["end"]["character"], 25);
        let result = prepare_rename(source, 9).unwrap();
        assert_eq!(result["range"]["start"]["character"], 7);
        assert_eq!(result["range"]["end"]["character"], 11);
    }

    #[test]
    fn renames_the_right_pair_among_nested_same_name_elements() {
        let source = "<a><a></a></a>";
        assert_eq!(renamed(source, 1, "b").as_deref(), Some("<b><a></a></b>"));
        assert_eq!(renamed(source, 8, "b").as_deref(), Some("<a><b></b></a>"));
        assert_eq!(renamed(source, 13, "b").as_deref(), Some("<b><a></a></b>"));
    }

    #[test]
    fn renames_self_closing_unclosed_and_orphan_tags() {
        assert_eq!(
            renamed("<root><item/></root>", 8, "x").as_deref(),
            Some("<root><x/></root>")
        );
        assert_eq!(
            renamed("<root><item></root>", 8, "x").as_deref(),
            Some("<root><x></root>")
        );
        assert_eq!(
            renamed("<root></item></root>", 9, "x").as_deref(),
            Some("<root></x></root>")
        );
    }

    #[test]
    fn renames_the_whole_qualified_name_from_the_local_part() {
        let source = "<p:root xmlns:p=\"urn:p\"><p:item/></p:root>";
        let offset = at(source, "item", 0) + 1;
        assert_eq!(placeholder(source, offset).as_deref(), Some("p:item"));
        assert_eq!(
            renamed(source, offset, "p:entry").as_deref(),
            Some("<p:root xmlns:p=\"urn:p\"><p:entry/></p:root>")
        );
    }

    #[test]
    fn rejects_invalid_element_names() {
        let source = "<root></root>";
        for name in ["", "1a", "a b", "a>", ":a", "a:", "a:b:c", "-a", "a/"] {
            let error = rename(source, 1, name).expect_err(name);
            assert_eq!(error.code, INVALID_PARAMS);
            assert!(error.message.contains("valid XML element name"), "{name}");
        }
        for name in ["a", "_a", "a-b.c", "p:a", "élément", "😀"] {
            assert!(rename(source, 1, name).is_ok(), "{name}");
        }
    }

    #[test]
    fn returns_nothing_outside_renameable_symbols() {
        let source = "<?xml version=\"1.0\"?><root attr=\"v\" xml:lang=\"fr\" xmlns=\"urn:d\">text<!-- <root> --><![CDATA[<a>]]></root>";
        for needle in [
            "version",
            "attr",
            "\"v\"",
            "lang",
            "xml:",
            "xmlns=",
            "urn:d",
            "text",
            "<!-- <root",
            "<a>",
        ] {
            let offset = at(source, needle, 0) + 1;
            assert_eq!(prepare_rename(source, offset), None, "{needle}");
            assert_eq!(rename(source, offset, "x"), Ok(None), "{needle}");
        }
        assert_eq!(prepare_rename("<></>", 1), None);
        assert_eq!(prepare_rename("", 0), None);
    }

    #[test]
    fn renames_a_prefix_from_every_kind_of_use() {
        let source = "<ns:root xmlns:ns=\"urn:x\" ns:a=\"1\"><ns:item ns:b=\"2\">x</ns:item><other/></ns:root>";
        let expected =
            "<p:root xmlns:p=\"urn:x\" p:a=\"1\"><p:item p:b=\"2\">x</p:item><other/></p:root>";
        for (needle, nth) in [
            ("ns:root", 0),
            ("ns=", 0),
            ("ns:a", 0),
            ("ns:item", 0),
            ("ns:b", 0),
            ("ns:item", 1),
            ("ns:root", 1),
        ] {
            let offset = at(source, needle, nth);
            assert_eq!(
                placeholder(source, offset).as_deref(),
                Some("ns"),
                "{needle}"
            );
            assert_eq!(
                renamed(source, offset, "p").as_deref(),
                Some(expected),
                "{needle}"
            );
            // Right before `:`.
            assert_eq!(
                renamed(source, offset + 2, "p").as_deref(),
                Some(expected),
                "{needle}"
            );
        }
    }

    #[test]
    fn respects_prefix_shadowing() {
        let source = concat!(
            "<a:root xmlns:a=\"urn:1\">",
            "<a:x/>",
            "<a:inner xmlns:a=\"urn:2\"><a:y a:z=\"\"/></a:inner>",
            "<a:w/>",
            "</a:root>"
        );
        let outer = renamed(source, 1, "o").unwrap();
        assert_eq!(
            outer,
            concat!(
                "<o:root xmlns:o=\"urn:1\">",
                "<o:x/>",
                "<a:inner xmlns:a=\"urn:2\"><a:y a:z=\"\"/></a:inner>",
                "<o:w/>",
                "</o:root>"
            )
        );
        let inner = renamed(source, at(source, "a:y", 0), "i").unwrap();
        assert_eq!(
            inner,
            concat!(
                "<a:root xmlns:a=\"urn:1\">",
                "<a:x/>",
                "<i:inner xmlns:i=\"urn:2\"><i:y i:z=\"\"/></i:inner>",
                "<a:w/>",
                "</a:root>"
            )
        );
    }

    #[test]
    fn default_namespace_is_not_affected_by_prefix_rename() {
        let source = "<root xmlns=\"urn:d\" xmlns:p=\"urn:p\"><child p:a=\"\"/><p:child/></root>";
        let offset = at(source, "p:child", 0);
        assert_eq!(
            renamed(source, offset, "q").as_deref(),
            Some("<root xmlns=\"urn:d\" xmlns:q=\"urn:p\"><child q:a=\"\"/><q:child/></root>")
        );
        // Unprefixed element: element rename.
        assert_eq!(
            placeholder(source, at(source, "child", 0)).as_deref(),
            Some("child")
        );
    }

    #[test]
    fn undeclared_prefix_falls_back_to_element_rename() {
        let source = "<u:item></u:item>";
        assert_eq!(placeholder(source, 1).as_deref(), Some("u:item"));
        assert_eq!(
            renamed(source, 1, "v:item").as_deref(),
            Some("<v:item></v:item>")
        );
        // Attribute with an undeclared prefix: nothing to rename.
        assert_eq!(prepare_rename("<a u:b=\"\"/>", 3), None);
    }

    #[test]
    fn validates_new_prefixes() {
        let source = "<p:a xmlns:p=\"urn:p\" xmlns:q=\"urn:q\"/>";
        for name in ["", "a:b", "1a", "xml", "XMLNS", "a b"] {
            assert_eq!(
                rename(source, 1, name).unwrap_err().code,
                INVALID_PARAMS,
                "{name}"
            );
        }
        let conflict = rename(source, 1, "q").unwrap_err();
        assert_eq!(conflict.code, REQUEST_FAILED);
        assert!(conflict.message.contains("already declared"));
        assert!(rename(source, 1, "p").is_ok());
    }

    #[test]
    fn renames_prefixes_inside_xsd_qname_values() {
        let source = concat!(
            "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:t=\"urn:t\" targetNamespace=\"urn:t\">",
            "<xs:element name=\"a\" type=\"t:T\"/>",
            "<xs:simpleType name=\"L\"><xs:union memberTypes=\"t:T  xs:int\"/></xs:simpleType>",
            "<xs:complexType name=\"T\"><xs:attribute name=\"n\" type=\"xs:string\" default=\"xs:x\"/></xs:complexType>",
            "</xs:schema>"
        );
        let result = renamed(source, at(source, "xs:element", 0), "xsd").unwrap();
        assert!(result.contains("xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\""));
        assert!(result.contains("memberTypes=\"t:T  xsd:int\""));
        assert!(result.contains("type=\"xsd:string\" default=\"xs:x\""));
        assert!(!result.contains("<xs:"));
        let from_value = renamed(source, at(source, "t:T", 1), "tns").unwrap();
        assert!(from_value.contains("xmlns:tns=\"urn:t\""));
        assert!(from_value.contains("type=\"tns:T\""));
        assert!(from_value.contains("memberTypes=\"tns:T  xs:int\""));
    }

    #[test]
    fn renames_xsi_type_prefixes_in_instances() {
        let source = "<r xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xmlns:m=\"urn:m\" xsi:type=\"m:T\"/>";
        assert_eq!(
            renamed(source, at(source, "m:T", 0), "n").as_deref(),
            Some(
                "<r xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xmlns:n=\"urn:m\" xsi:type=\"n:T\"/>"
            )
        );
    }

    const SCHEMA: &str = concat!(
        "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:t=\"urn:t\" targetNamespace=\"urn:t\">\r\n",
        "  <xs:element name=\"item\" type=\"t:ItemType\" substitutionGroup=\"t:base\"/>\r\n",
        "  <xs:element name=\"base\"/>\r\n",
        "  <xs:complexType name=\"ItemType\"><xs:sequence><xs:element ref=\"t:item\"/><xs:element name=\"item\" type=\"xs:string\"/></xs:sequence></xs:complexType>\r\n",
        "  <xs:simpleType name=\"Sub\"><xs:restriction base=\"t:ItemType\"/></xs:simpleType>\r\n",
        "  <xs:simpleType name=\"L\"><xs:list itemType=\"ItemType\"/></xs:simpleType>\r\n",
        "</xs:schema>"
    );

    #[test]
    fn renames_global_xsd_types_and_their_references() {
        let offset = at(SCHEMA, "ItemType\"><", 0) + 2;
        assert_eq!(placeholder(SCHEMA, offset).as_deref(), Some("ItemType"));
        let plan = rename(SCHEMA, offset, "Entry").unwrap().unwrap();
        let result = apply(SCHEMA, &plan.ranges, "Entry");
        assert!(result.contains("type=\"t:Entry\""));
        assert!(result.contains("complexType name=\"Entry\""));
        assert!(result.contains("base=\"t:Entry\""));
        // Without a default namespace, the unprefixed `ItemType` is not in
        // `urn:t`.
        assert!(result.contains("itemType=\"ItemType\""));
        assert_eq!(
            plan.component,
            Some(RenamedComponent {
                kind: ComponentKind::Type,
                namespace: Some("urn:t".into()),
                old_name: "ItemType".into(),
            })
        );
        // From a reference.
        let from_reference = rename(SCHEMA, at(SCHEMA, "t:ItemType", 1) + 4, "Entry")
            .unwrap()
            .unwrap();
        assert_eq!(from_reference.ranges, plan.ranges);
    }

    #[test]
    fn renames_global_xsd_elements_but_not_local_homonyms() {
        let offset = at(SCHEMA, "\"item\"", 0) + 1;
        let plan = rename(SCHEMA, offset, "entry").unwrap().unwrap();
        let result = apply(SCHEMA, &plan.ranges, "entry");
        assert!(result.contains("<xs:element name=\"entry\" type=\"t:ItemType\""));
        assert!(result.contains("ref=\"t:entry\""));
        assert!(result.contains("<xs:element name=\"item\" type=\"xs:string\""));
        // Local declaration: only its name is renamed.
        let local = at(SCHEMA, "\"item\"", 1) + 1;
        let plan = rename(SCHEMA, local, "x").unwrap().unwrap();
        assert_eq!(plan.ranges.len(), 1);
        assert_eq!(plan.component, None);
        // `substitutionGroup` references a global element.
        let base = rename(SCHEMA, at(SCHEMA, "t:base", 0) + 3, "root")
            .unwrap()
            .unwrap();
        assert!(apply(SCHEMA, &base.ranges, "root").contains("name=\"root\"/>"));
        assert_eq!(base.ranges.len(), 2);
        // Built-in types and invalid names.
        assert_eq!(prepare_rename(SCHEMA, at(SCHEMA, "xs:string", 0) + 4), None);
        assert_eq!(
            rename(SCHEMA, offset, "a:b").unwrap_err().code,
            INVALID_PARAMS
        );
    }

    #[test]
    fn computes_instance_ranges_for_renamed_components() {
        let instance = "<t:item xmlns:t=\"urn:t\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:type=\"t:ItemType\"><item/><t:item></t:item></t:item>";
        let element = RenamedComponent {
            kind: ComponentKind::Element,
            namespace: Some("urn:t".into()),
            old_name: "item".into(),
        };
        let ranges = instance_ranges(instance, &element);
        assert_eq!(
            apply(instance, &ranges, "entry"),
            "<t:entry xmlns:t=\"urn:t\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:type=\"t:ItemType\"><item/><t:entry></t:entry></t:entry>"
        );
        let r#type = RenamedComponent {
            kind: ComponentKind::Type,
            namespace: Some("urn:t".into()),
            old_name: "ItemType".into(),
        };
        let ranges = instance_ranges(instance, &r#type);
        assert!(apply(instance, &ranges, "Entry").contains("xsi:type=\"t:Entry\""));
        let no_namespace = RenamedComponent {
            kind: ComponentKind::Element,
            namespace: None,
            old_name: "item".into(),
        };
        assert_eq!(instance_ranges(instance, &no_namespace).len(), 1);
    }

    #[test]
    fn uses_utf16_positions_with_crlf_line_endings() {
        let source = "<a>\r\n  <😀x>\r\n  </😀x>\r\n</a>";
        let offset = at(source, "😀x", 1);
        let result = prepare_rename(source, offset).unwrap();
        assert_eq!(
            result["range"],
            json!({"start": {"line": 2, "character": 4}, "end": {"line": 2, "character": 7}})
        );
        let plan = rename(source, offset, "y").unwrap().unwrap();
        assert_eq!(
            text_edits(source, &plan.ranges, "y"),
            vec![
                json!({"range": {"start": {"line": 1, "character": 3}, "end": {"line": 1, "character": 6}}, "newText": "y"}),
                json!({"range": {"start": {"line": 2, "character": 4}, "end": {"line": 2, "character": 7}}, "newText": "y"}),
            ]
        );
    }

    #[test]
    fn tolerates_malformed_documents() {
        for source in [
            "<",
            "<a",
            "<a b=\"",
            "<p:a xmlns:p=",
            "<p:a xmlns:=\"x\"><p:",
            "</p:a>",
            "<a xmlns:p=\"u\"></p:b>",
            "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"><xs:element name=\"",
            "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"><xs:element type=\" :  a: \"/>",
        ] {
            for offset in 0..=source.len() {
                if !source.is_char_boundary(offset) {
                    continue;
                }
                let _ = prepare_rename(source, offset);
                let _ = rename(source, offset, "n");
            }
        }
        // Orphan end tag in the scope of a declaration.
        let source = "<a xmlns:p=\"u\"><p:b/></p:c></a>";
        assert_eq!(
            renamed(source, at(source, "p:c", 0), "q").as_deref(),
            Some("<a xmlns:q=\"u\"><q:b/></q:c></a>")
        );
    }
}
