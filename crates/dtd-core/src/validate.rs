//! Entity references and validation of an instance document against a
//! DTD, like Xerces.
//!
//! - [`check_entity_references`]: undeclared `&name;` references in content
//!   and attribute values (the five predefined entities are always
//!   allowed), unparsed, recursive or too large entities, external entities
//!   or entities containing `<` in an attribute value. Also applies
//!   without a DTD.
//! - [`validate_instance`]: root element matching `<!DOCTYPE>`, declared
//!   elements and attributes, content models (EMPTY, ANY, mixed,
//!   `children`), required attributes, fixed and enumerated values, (unique)
//!   IDs, IDREF(S) (existing targets), NMTOKEN(S), ENTITY/ENTITIES. A DTD
//!   declaring no element only serves for entities: only its attributes are
//!   checked. Undeclared `xmlns`, `xmlns:*`, `xml:*` and `xsi:*` attributes
//!   are tolerated (documents also validated by XSD). The content of an
//!   element referencing an entity that contains markup (or is external) is
//!   not checked against its model.

use std::{collections::HashMap, ops::Range};

use xml_core::tags::{
    XmlElement, XmlMarkup, XmlMarkupKind, XmlTagKind, XmlTagTree, scan_attributes, scan_markup,
    scan_tags,
};

use crate::{
    AttributeDecl, AttributeType, ContentAutomaton, ContentSpec, DefaultDecl, Dtd, ExpansionError,
    PREDEFINED_ENTITIES, is_name, is_nmtoken, names::scan_name_chars,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstanceProblemKind {
    UndefinedEntity {
        name: String,
    },
    /// Reference to a recursive or too large entity.
    EntityExpansion,
    UnparsedEntityReference,
    ExternalEntityInAttribute,
    RootMismatch {
        expected: String,
    },
    UndeclaredElement,
    UndeclaredAttribute,
    MissingAttribute {
        element: String,
        attribute: String,
        /// Proposed value (first enumerated value, otherwise empty).
        value: String,
        /// Insertion offset of ` attribute="value"` in the tag.
        insert_at: usize,
    },
    InvalidAttributeValue,
    InvalidEnumeration {
        values: Vec<String>,
    },
    FixedValue {
        expected: String,
    },
    DuplicateId,
    UnknownIdRef,
    InvalidEntityAttribute,
    EmptyContent,
    UnexpectedElement {
        expected: Vec<String>,
    },
    IncompleteContent {
        expected: Vec<String>,
    },
    TextNotAllowed,
    /// The entity expansion budget of the document
    /// ([`crate::MAX_DOCUMENT_EXPANSION`]) is exhausted: the following
    /// attribute values referencing entities are not checked.
    ExpansionBudget,
}

impl InstanceProblemKind {
    /// Stable identifier, published in `data.kind`.
    pub fn id(&self) -> &'static str {
        match self {
            InstanceProblemKind::UndefinedEntity { .. } => "undefinedEntity",
            InstanceProblemKind::EntityExpansion => "entityExpansion",
            InstanceProblemKind::UnparsedEntityReference => "unparsedEntityReference",
            InstanceProblemKind::ExternalEntityInAttribute => "externalEntityInAttribute",
            InstanceProblemKind::RootMismatch { .. } => "rootMismatch",
            InstanceProblemKind::UndeclaredElement => "undeclaredElement",
            InstanceProblemKind::UndeclaredAttribute => "undeclaredAttribute",
            InstanceProblemKind::MissingAttribute { .. } => "missingAttribute",
            InstanceProblemKind::InvalidAttributeValue => "invalidAttributeValue",
            InstanceProblemKind::InvalidEnumeration { .. } => "invalidEnumeration",
            InstanceProblemKind::FixedValue { .. } => "fixedValue",
            InstanceProblemKind::DuplicateId => "duplicateId",
            InstanceProblemKind::UnknownIdRef => "unknownIdref",
            InstanceProblemKind::InvalidEntityAttribute => "invalidEntityAttribute",
            InstanceProblemKind::EmptyContent => "emptyContent",
            InstanceProblemKind::UnexpectedElement { .. } => "unexpectedElement",
            InstanceProblemKind::IncompleteContent { .. } => "incompleteContent",
            InstanceProblemKind::TextNotAllowed => "textNotAllowed",
            InstanceProblemKind::ExpansionBudget => "expansionBudget",
        }
    }
}

/// Located problem (UTF-8 offsets) in the instance document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceProblem {
    pub kind: InstanceProblemKind,
    pub range: Range<usize>,
    pub message: String,
}

fn is_predefined(name: &str) -> bool {
    PREDEFINED_ENTITIES
        .iter()
        .any(|(predefined, _)| *predefined == name)
}

/// `&name;` references (excluding character references) of `text`: range
/// of the reference and of the name, shifted by `base`.
fn entity_references(text: &str, base: usize) -> Vec<(Range<usize>, Range<usize>)> {
    let bytes = text.as_bytes();
    let mut references = Vec::new();
    let mut index = 0;
    while let Some(offset) = text[index..].find('&') {
        let start = index + offset;
        let name_end = scan_name_chars(text, start + 1, text.len());
        if name_end > start + 1
            && bytes.get(name_end) == Some(&b';')
            && is_name(&text[start + 1..name_end])
        {
            references.push((
                base + start..base + name_end + 1,
                base + start + 1..base + name_end,
            ));
            index = name_end + 1;
        } else {
            index = start + 1;
        }
    }
    references
}

/// Name of the `&name;` reference (or `%name;` with `parameter`) containing
/// `offset` in `source` (bounds included).
pub fn entity_reference_at(source: &str, offset: usize, parameter: bool) -> Option<Range<usize>> {
    let offset = offset.min(source.len());
    let sigil = if parameter { '%' } else { '&' };
    // A reference does not extend beyond a reasonable line.
    let window = offset.saturating_sub(256);
    let window = (window..=offset)
        .find(|&index| source.is_char_boundary(index))
        .unwrap_or(offset);
    // The cursor may be on the `&` itself.
    let until = (offset + 1..=source.len())
        .find(|&index| source.is_char_boundary(index))
        .unwrap_or(source.len());
    let start = window + source[window..until].rfind(sigil)?;
    let name_end = scan_name_chars(source, start + 1, source.len());
    let name = start + 1..name_end;
    (name_end > start + 1
        && source.as_bytes().get(name_end) == Some(&b';')
        && offset <= name_end + 1
        && is_name(&source[name.clone()]))
    .then_some(name)
}

/// Text segments of the document outside tags and markup.
fn text_segments(source: &str) -> Vec<Range<usize>> {
    let mut blocked = scan_tags(source)
        .into_iter()
        .map(|tag| tag.range)
        .chain(scan_markup(source).into_iter().map(|markup| markup.range))
        .collect::<Vec<_>>();
    blocked.sort_by_key(|range| range.start);
    let mut segments = Vec::new();
    let mut cursor = 0;
    for range in blocked {
        if range.start > cursor {
            segments.push(cursor..range.start);
        }
        cursor = cursor.max(range.end);
    }
    if cursor < source.len() {
        segments.push(cursor..source.len());
    }
    segments
}

/// General entity reference of the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityReference {
    /// Whole `&name;`.
    pub range: Range<usize>,
    /// Name only.
    pub name: Range<usize>,
    /// In an attribute value (otherwise in content).
    pub in_attribute: bool,
}

/// `&name;` references of the content and attribute values of the document
/// (excluding comments, CDATA, processing instructions and DOCTYPE), in
/// document order.
pub fn general_entity_references(source: &str) -> Vec<EntityReference> {
    let mut references = Vec::new();
    for segment in text_segments(source) {
        for (range, name) in entity_references(&source[segment.clone()], segment.start) {
            references.push(EntityReference {
                range,
                name,
                in_attribute: false,
            });
        }
    }
    for tag in scan_tags(source) {
        if tag.kind == XmlTagKind::End {
            continue;
        }
        for attribute in scan_attributes(source, &tag) {
            let Some(value) = attribute.value else {
                continue;
            };
            for (range, name) in entity_references(&source[value.clone()], value.start) {
                references.push(EntityReference {
                    range,
                    name,
                    in_attribute: true,
                });
            }
        }
    }
    references.sort_by_key(|reference| reference.range.start);
    references
}

/// Checks the general entity references of the document (see the module).
pub fn check_entity_references(source: &str, dtd: Option<&Dtd>) -> Vec<InstanceProblem> {
    let mut problems = Vec::new();
    for reference in general_entity_references(source) {
        check_reference(
            &mut problems,
            source,
            dtd,
            reference.range,
            reference.name,
            reference.in_attribute,
        );
    }
    problems
}

fn check_reference(
    problems: &mut Vec<InstanceProblem>,
    source: &str,
    dtd: Option<&Dtd>,
    reference: Range<usize>,
    name: Range<usize>,
    in_attribute: bool,
) {
    let name = &source[name];
    if is_predefined(name) {
        return;
    }
    let Some(entity) = dtd.and_then(|dtd| dtd.general_entity(name)) else {
        problems.push(InstanceProblem {
            kind: InstanceProblemKind::UndefinedEntity {
                name: name.to_owned(),
            },
            range: reference,
            message: format!("the entity '{name}' is referenced but not declared"),
        });
        return;
    };
    let expansion = &entity.expansion;
    let (kind, message) = if expansion.unparsed {
        (
            InstanceProblemKind::UnparsedEntityReference,
            format!("the unparsed entity '{name}' (NDATA) cannot be referenced"),
        )
    } else if let Some(error) = expansion.error {
        (
            InstanceProblemKind::EntityExpansion,
            match error {
                ExpansionError::Recursive => format!("the entity '{name}' is recursive"),
                ExpansionError::TooLarge => format!(
                    "the expansion of the entity '{name}' exceeds the limit of {} bytes",
                    crate::MAX_ENTITY_EXPANSION
                ),
            },
        )
    } else if in_attribute && expansion.external {
        (
            InstanceProblemKind::ExternalEntityInAttribute,
            format!("an attribute value cannot reference the external entity '{name}'"),
        )
    } else if in_attribute && expansion.markup {
        (
            InstanceProblemKind::InvalidAttributeValue,
            format!(
                "the expansion of the entity '{name}' contains '<', which is not allowed in an attribute value"
            ),
        )
    } else {
        return;
    };
    problems.push(InstanceProblem {
        kind,
        range: reference,
        message,
    });
}

/// Validates the document `source` against `dtd` (see the module).
pub fn validate_instance(source: &str, dtd: &Dtd) -> Vec<InstanceProblem> {
    run_validator(source, dtd).0
}

/// Links of the `IDREF(S)` values of the document to the `ID` values they
/// designate: (reference range, target range), UTF-8 offsets.
pub fn id_links(source: &str, dtd: &Dtd) -> Vec<(Range<usize>, Range<usize>)> {
    run_validator(source, dtd).1
}

#[allow(clippy::type_complexity)]
fn run_validator(
    source: &str,
    dtd: &Dtd,
) -> (Vec<InstanceProblem>, Vec<(Range<usize>, Range<usize>)>) {
    if !dtd.declares_elements() && dtd.attributes.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let mut validator = Validator {
        source,
        dtd,
        problems: Vec::new(),
        automata: HashMap::new(),
        ids: HashMap::new(),
        references: Vec::new(),
        links: Vec::new(),
        expansion_budget: crate::MAX_DOCUMENT_EXPANSION,
        content_budget: MAX_CONTENT_STEPS,
    };
    validator.run();
    validator
        .problems
        .sort_by_key(|problem| problem.range.start);
    (validator.problems, validator.links)
}

/// Maximum number of automaton state visits (states × children) spent on
/// the content models of one document.
const MAX_CONTENT_STEPS: usize = 1 << 28;

/// Maximum number of characters of a value quoted in a message.
const MAX_QUOTED_CHARS: usize = 80;

/// `value`, shortened with `…` when it is too long to be quoted in a
/// message (a value can come from a large entity expansion).
fn excerpt(value: &str) -> std::borrow::Cow<'_, str> {
    match value.char_indices().nth(MAX_QUOTED_CHARS) {
        Some((end, _)) => format!("{}…", &value[..end]).into(),
        None => value.into(),
    }
}

/// Maximum number of names listed in a message.
const MAX_LISTED_NAMES: usize = 50;

fn list(names: &[String]) -> String {
    if names.is_empty() {
        return "no element".to_owned();
    }
    let mut listed = names
        .iter()
        .take(MAX_LISTED_NAMES)
        .map(|name| format!("'{}'", excerpt(name)))
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > MAX_LISTED_NAMES {
        listed.push_str(&format!(" and {} more", names.len() - MAX_LISTED_NAMES));
    }
    listed
}

/// Direct content of an element, excluding child elements.
#[derive(Debug, Default)]
struct ContentSummary {
    /// First non-whitespace text (or CDATA section).
    text: Option<Range<usize>>,
    /// Whether there is whitespace.
    whitespace: bool,
    /// Reference to an entity containing markup, external or unknown: the
    /// actual content is not known.
    opaque: bool,
}

struct Validator<'a> {
    source: &'a str,
    dtd: &'a Dtd,
    problems: Vec<InstanceProblem>,
    automata: HashMap<&'a str, ContentAutomaton>,
    /// IDs and the range of their first occurrence.
    ids: HashMap<String, Range<usize>>,
    references: Vec<(String, Range<usize>)>,
    /// (IDREF, ID) ranges.
    links: Vec<(Range<usize>, Range<usize>)>,
    /// Remaining bytes of entity replacement texts for attribute values.
    expansion_budget: usize,
    /// Remaining automaton steps (states × children) for content models.
    content_budget: usize,
}

impl<'a> Validator<'a> {
    fn push(&mut self, kind: InstanceProblemKind, range: Range<usize>, message: String) {
        self.problems.push(InstanceProblem {
            kind,
            range,
            message,
        });
    }

    fn run(&mut self) {
        let source = self.source;
        let tree = XmlTagTree::parse(source);
        let elements = tree.elements();
        let markups = scan_markup(source);
        let mut children = vec![Vec::new(); elements.len()];
        let mut roots = Vec::new();
        for (index, element) in elements.iter().enumerate() {
            match element.parent {
                Some(parent) => children[parent].push(index),
                None => roots.push(index),
            }
        }
        let structural = self.dtd.declares_elements();
        if structural && let (Some(expected), Some(&root)) = (&self.dtd.doctype_name, roots.first())
        {
            let root = &elements[root];
            let name = root.name(source);
            if name != expected {
                self.push(
                    InstanceProblemKind::RootMismatch {
                        expected: expected.clone(),
                    },
                    root.start_tag.name.clone(),
                    format!(
                        "the root element '{name}' does not match the name '{expected}' of the DOCTYPE declaration"
                    ),
                );
            }
        }
        for (index, element) in elements.iter().enumerate() {
            let name = element.name(source);
            let declaration = self.dtd.element(name);
            if structural && declaration.is_none() {
                self.push(
                    InstanceProblemKind::UndeclaredElement,
                    element.start_tag.name.clone(),
                    format!("the element '{name}' is not declared in the DTD"),
                );
            }
            self.check_attributes(element, declaration.is_some());
            if let Some(declaration) = declaration
                && element.is_closed()
                && children[index]
                    .iter()
                    .all(|&child| elements[child].is_closed())
            {
                self.check_content(
                    element,
                    &declaration.content,
                    &children[index],
                    elements,
                    &markups,
                );
            }
        }
        for (value, range) in std::mem::take(&mut self.references) {
            if let Some(target) = self.ids.get(&value) {
                self.links.push((range, target.clone()));
            } else {
                self.push(
                    InstanceProblemKind::UnknownIdRef,
                    range,
                    format!("no element has the ID '{}'", excerpt(&value)),
                );
            }
        }
    }

    fn check_attributes(&mut self, element: &XmlElement, declared: bool) {
        let source = self.source;
        let name = element.name(source);
        let attributes = scan_attributes(source, &element.start_tag);
        let mut present = Vec::new();
        for attribute in &attributes {
            let attribute_name = attribute.name(source);
            present.push(attribute_name);
            let Some(declaration) = self.dtd.attribute(name, attribute_name) else {
                let namespace = attribute_name == "xmlns"
                    || ["xmlns:", "xml:", "xsi:"]
                        .iter()
                        .any(|prefix| attribute_name.starts_with(prefix));
                if declared && !namespace {
                    self.push(
                        InstanceProblemKind::UndeclaredAttribute,
                        attribute.name.clone(),
                        format!(
                            "the attribute '{attribute_name}' is not declared for the element '{name}'"
                        ),
                    );
                }
                continue;
            };
            if let Some(value) = attribute.value.clone() {
                self.check_value(declaration, value);
            }
        }
        for declaration in self.dtd.attributes_of(name) {
            if declaration.default != DefaultDecl::Required
                || present.contains(&declaration.name.as_str())
            {
                continue;
            }
            let tag = &element.start_tag;
            let text = &source[tag.range.clone()];
            let mut insert_at = if !tag.closed {
                tag.range.end
            } else if text.ends_with("/>") {
                tag.range.end - 2
            } else {
                tag.range.end - 1
            };
            while insert_at > tag.name.end && source.as_bytes()[insert_at - 1].is_ascii_whitespace()
            {
                insert_at -= 1;
            }
            let value = declaration
                .attribute_type
                .values()
                .and_then(|values| values.first().cloned())
                .unwrap_or_default();
            self.push(
                InstanceProblemKind::MissingAttribute {
                    element: name.to_owned(),
                    attribute: declaration.name.clone(),
                    value,
                    insert_at,
                },
                tag.name.clone(),
                format!(
                    "the required attribute '{}' is missing from the element '{name}'",
                    declaration.name
                ),
            );
        }
    }

    /// Range of the token `token` in the raw value, or the whole value.
    fn token_range(&self, value: &Range<usize>, token: &str) -> Range<usize> {
        self.source[value.clone()]
            .find(token)
            .map_or(value.clone(), |offset| {
                value.start + offset..value.start + offset + token.len()
            })
    }

    fn check_value(&mut self, declaration: &AttributeDecl, range: Range<usize>) {
        let raw = &self.source[range.clone()];
        let cdata = declaration.attribute_type == AttributeType::CData;
        // Invalid references: reported by `check_entity_references`.
        let Some(value) = self.normalize(raw, cdata, &range) else {
            return;
        };
        let attribute = &declaration.name;
        if let DefaultDecl::Fixed(expected) = &declaration.default {
            if value != *expected {
                self.push(
                    InstanceProblemKind::FixedValue {
                        expected: expected.clone(),
                    },
                    range,
                    format!(
                        "the attribute '{attribute}' has the fixed value '{}' (#FIXED), found '{}'",
                        excerpt(expected),
                        excerpt(&value)
                    ),
                );
            }
            return;
        }
        let tokens = value
            .split(' ')
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>();
        match &declaration.attribute_type {
            AttributeType::CData => {}
            AttributeType::Id => {
                if !is_name(&value) {
                    self.invalid(range, &value, attribute, "an XML name (ID)");
                } else if self.ids.contains_key(&value) {
                    self.push(
                        InstanceProblemKind::DuplicateId,
                        range,
                        format!(
                            "the ID '{}' is already used in the document",
                            excerpt(&value)
                        ),
                    );
                } else {
                    let target = self.token_range(&range, &value);
                    self.ids.insert(value.clone(), target);
                }
            }
            AttributeType::IdRef | AttributeType::IdRefs => {
                let multiple = declaration.attribute_type == AttributeType::IdRefs;
                if tokens.is_empty() || (!multiple && tokens.len() > 1) {
                    self.invalid(range, &value, attribute, "an XML name (IDREF)");
                    return;
                }
                for token in tokens {
                    let token_range = self.token_range(&range, token);
                    if is_name(token) {
                        self.references.push((token.to_owned(), token_range));
                    } else {
                        self.invalid(token_range, token, attribute, "an XML name (IDREF)");
                    }
                }
            }
            AttributeType::Entity | AttributeType::Entities => {
                let multiple = declaration.attribute_type == AttributeType::Entities;
                if tokens.is_empty() || (!multiple && tokens.len() > 1) {
                    self.invalid(range, &value, attribute, "an unparsed entity name");
                    return;
                }
                for token in tokens {
                    let unparsed = self
                        .dtd
                        .general_entity(token)
                        .is_some_and(|entity| entity.expansion.unparsed);
                    if !unparsed {
                        let token_range = self.token_range(&range, token);
                        self.push(
                            InstanceProblemKind::InvalidEntityAttribute,
                            token_range,
                            format!("'{token}' is not a declared unparsed entity (NDATA)"),
                        );
                    }
                }
            }
            AttributeType::NmToken => {
                if !is_nmtoken(&value) {
                    self.invalid(range, &value, attribute, "an NMTOKEN");
                }
            }
            AttributeType::NmTokens => {
                if tokens.is_empty() || !tokens.iter().all(|token| is_nmtoken(token)) {
                    self.invalid(range, &value, attribute, "a list of NMTOKENs");
                }
            }
            AttributeType::Notation(values) | AttributeType::Enumeration(values) => {
                if !values.contains(&value) {
                    self.push(
                        InstanceProblemKind::InvalidEnumeration {
                            values: values.clone(),
                        },
                        range,
                        format!(
                            "the value '{}' is not allowed for the attribute '{attribute}' (expected: {})",
                            excerpt(&value),
                            values.join(", ")
                        ),
                    );
                }
            }
        }
    }

    /// Normalized attribute value, the entity replacement texts being taken
    /// from the budget of the document (reported once when exhausted).
    fn normalize(&mut self, raw: &str, cdata: bool, range: &Range<usize>) -> Option<String> {
        if !raw.contains('&') {
            return self.dtd.normalize_attribute_value(raw, cdata);
        }
        if self.expansion_budget == 0 {
            return None;
        }
        let mut budget = self.expansion_budget.min(crate::MAX_ENTITY_EXPANSION);
        let before = budget;
        let value = self
            .dtd
            .normalize_attribute_value_within(raw, cdata, &mut budget);
        self.expansion_budget -= before - budget;
        if self.expansion_budget == 0 {
            self.push(
                InstanceProblemKind::ExpansionBudget,
                range.clone(),
                format!(
                    "the entity references of the attribute values expand to more than {} bytes in total: the following values referencing entities are not checked",
                    crate::MAX_DOCUMENT_EXPANSION
                ),
            );
        }
        value
    }

    fn invalid(&mut self, range: Range<usize>, value: &str, attribute: &str, expected: &str) {
        let value = excerpt(value);
        self.push(
            InstanceProblemKind::InvalidAttributeValue,
            range,
            format!("the value '{value}' of the attribute '{attribute}' must be {expected}"),
        );
    }

    /// Text, whitespace and entity references of the direct content.
    fn summarize(
        &self,
        content: Range<usize>,
        children: &[Range<usize>],
        markups: &[XmlMarkup],
    ) -> ContentSummary {
        let source = self.source;
        let first = markups.partition_point(|markup| markup.range.start < content.start);
        let mut blocked = children
            .iter()
            .map(|range| (range.clone(), false))
            .chain(
                markups[first..]
                    .iter()
                    .take_while(|markup| markup.range.start < content.end)
                    .map(|markup| (markup.range.clone(), markup.kind == XmlMarkupKind::CData)),
            )
            .collect::<Vec<_>>();
        blocked.sort_by_key(|(range, _)| range.start);
        let mut summary = ContentSummary::default();
        let mut gaps = Vec::new();
        let mut cursor = content.start;
        for (range, cdata) in blocked {
            if range.start < cursor {
                // Markup inside a child.
                continue;
            }
            gaps.push(cursor..range.start);
            if cdata && summary.text.is_none() {
                summary.text = Some(range.clone());
            }
            cursor = range.end;
        }
        gaps.push(cursor..content.end.max(cursor));
        for gap in gaps {
            let text = &source[gap.clone()];
            let mut plain_start = 0;
            let mut pieces = Vec::new();
            for (reference, name) in entity_references(text, gap.start) {
                pieces.push((plain_start + gap.start..reference.start, None));
                pieces.push((reference.clone(), Some(name)));
                plain_start = reference.end - gap.start;
            }
            pieces.push((plain_start + gap.start..gap.end, None));
            for (range, name) in pieces {
                match name {
                    None => {
                        let piece = &source[range.clone()];
                        summary.whitespace |= piece.bytes().any(|byte| byte.is_ascii_whitespace());
                        if summary.text.is_none() {
                            let trimmed = piece.trim_start();
                            if !trimmed.is_empty() {
                                let start = range.start + piece.len() - trimmed.len();
                                summary.text = Some(start..start + trimmed.trim_end().len());
                            }
                        }
                    }
                    Some(name) => {
                        let name = &source[name];
                        let blank = if is_predefined(name) {
                            false
                        } else {
                            match self.dtd.general_entity(name) {
                                Some(entity)
                                    if !entity.expansion.markup
                                        && !entity.expansion.external
                                        && entity.expansion.error.is_none() =>
                                {
                                    entity.expansion.blank
                                }
                                _ => {
                                    summary.opaque = true;
                                    continue;
                                }
                            }
                        };
                        if blank {
                            summary.whitespace = true;
                        } else if summary.text.is_none() {
                            summary.text = Some(range);
                        }
                    }
                }
            }
            // Character references: text.
            if summary.text.is_none()
                && let Some(offset) = text.find("&#")
            {
                summary.text = Some(gap.start + offset..gap.start + offset + 2);
            }
        }
        summary
    }

    fn check_content(
        &mut self,
        element: &XmlElement,
        content: &ContentSpec,
        children: &[usize],
        elements: &[XmlElement],
        markups: &[XmlMarkup],
    ) {
        let source = self.source;
        let name = element.name(source);
        let name_range = element.start_tag.name.clone();
        let summary = match element.content_range() {
            Some(range) => {
                let child_ranges = children
                    .iter()
                    .map(|&child| elements[child].range())
                    .collect::<Vec<_>>();
                self.summarize(range, &child_ranges, markups)
            }
            None => ContentSummary::default(),
        };
        if summary.opaque {
            return;
        }
        match content {
            ContentSpec::Any => {}
            ContentSpec::Empty => {
                if !children.is_empty() || summary.text.is_some() || summary.whitespace {
                    self.push(
                        InstanceProblemKind::EmptyContent,
                        name_range,
                        format!(
                            "the element '{name}' is declared EMPTY and must not have any content"
                        ),
                    );
                }
            }
            ContentSpec::Mixed(names) => {
                for &child in children {
                    let child_name = elements[child].name(source);
                    if !names.iter().any(|allowed| allowed == child_name) {
                        let message = if names.is_empty() {
                            format!(
                                "the element '{child_name}' is not allowed: '{name}' only contains text"
                            )
                        } else {
                            format!(
                                "the element '{child_name}' is not allowed in '{name}' (expected: {})",
                                list(names)
                            )
                        };
                        self.push(
                            InstanceProblemKind::UnexpectedElement {
                                expected: names.clone(),
                            },
                            elements[child].start_tag.name.clone(),
                            message,
                        );
                    }
                }
            }
            ContentSpec::Children(particle) => {
                if let Some(range) = summary.text {
                    self.push(
                        InstanceProblemKind::TextNotAllowed,
                        range,
                        format!(
                            "text is not allowed in the element '{name}' (content {})",
                            excerpt(&content.to_string())
                        ),
                    );
                }
                let automaton = self
                    .automata
                    .entry(name)
                    .or_insert_with(|| ContentAutomaton::new(particle));
                // Recognition costs `states × children`: a huge model
                // matched against many children is skipped once the budget
                // of the document is spent.
                let cost = automaton
                    .state_count()
                    .saturating_mul(children.len().saturating_add(1));
                let Some(rest) = self.content_budget.checked_sub(cost) else {
                    return;
                };
                self.content_budget = rest;
                let mut matcher = automaton.matcher();
                for &child in children {
                    let child_name = elements[child].name(source);
                    if !matcher.feed(child_name) {
                        let expected = matcher.expected();
                        let message = if expected.is_empty() {
                            format!(
                                "the element '{child_name}' is not expected here: the content of '{name}' is complete ({})",
                                excerpt(&content.to_string())
                            )
                        } else {
                            format!(
                                "the element '{child_name}' is not expected here in '{name}' (expected: {})",
                                list(&expected)
                            )
                        };
                        self.problems.push(InstanceProblem {
                            kind: InstanceProblemKind::UnexpectedElement { expected },
                            range: elements[child].start_tag.name.clone(),
                            message,
                        });
                        return;
                    }
                }
                if !matcher.accepts() {
                    let expected = matcher.expected();
                    let message = format!(
                        "the content of the element '{name}' is incomplete (expected: {}); model {}",
                        list(&expected),
                        excerpt(&content.to_string())
                    );
                    self.problems.push(InstanceProblem {
                        kind: InstanceProblemKind::IncompleteContent { expected },
                        range: name_range,
                        message,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NoLoader, load_document_dtd};

    fn validate(document: &str) -> Vec<(&'static str, String)> {
        let (_, dtd) = load_document_dtd(document, None, &mut NoLoader).expect("DOCTYPE");
        assert!(dtd.problems.is_empty(), "{:?}", dtd.problems);
        validate_instance(document, &dtd)
            .into_iter()
            .map(|problem| (problem.kind.id(), document[problem.range].to_owned()))
            .collect()
    }

    fn entities(document: &str) -> Vec<(&'static str, String)> {
        let dtd = load_document_dtd(document, None, &mut NoLoader).map(|(_, dtd)| dtd);
        check_entity_references(document, dtd.as_ref())
            .into_iter()
            .map(|problem| (problem.kind.id(), document[problem.range].to_owned()))
            .collect()
    }

    const BOOK: &str = "<!DOCTYPE book [\n<!ELEMENT book (title, chapter+)>\n<!ELEMENT title (#PCDATA | em)*>\n<!ELEMENT em (#PCDATA)>\n<!ELEMENT chapter (para | note)*>\n<!ELEMENT para (#PCDATA)>\n<!ELEMENT note EMPTY>\n<!ATTLIST chapter id ID #REQUIRED kind (intro | body) \"body\" see IDREFS #IMPLIED>\n<!ATTLIST book version CDATA #FIXED \"1.0\" lang NMTOKEN #IMPLIED>\n<!ENTITY ws \"  \">\n<!ENTITY frag \"<para>x</para>\">\n]>\n";

    #[test]
    fn accepts_valid_documents() {
        let document = format!(
            "{BOOK}<book version=\"1.0\" lang=\"fr\" xmlns:x=\"urn:x\">\n  <title>A <em>b</em> &amp; c</title>\n  <chapter id=\"c1\" see=\"c2 c1\"><!-- c --><para>p</para><note/></chapter>\n  &ws;<chapter id=\"c2\" kind=\"intro\"></chapter>\n</book>"
        );
        assert_eq!(validate(&document), Vec::new());
    }

    #[test]
    fn reports_structure_problems() {
        let document = format!(
            "{BOOK}<book>\n  <chapter id=\"c1\"><para>p</para>text<note>x</note><unknown/></chapter>\n  <title/>\n</book>"
        );
        assert_eq!(
            validate(&document),
            vec![
                ("unexpectedElement", "chapter".to_owned()),
                ("textNotAllowed", "text".to_owned()),
                ("emptyContent", "note".to_owned()),
                // Like Xerces: element not declared and not allowed here.
                ("unexpectedElement", "unknown".to_owned()),
                ("undeclaredElement", "unknown".to_owned()),
            ]
        );

        let document = format!("{BOOK}<book><title>t</title></book>");
        assert_eq!(
            validate(&document),
            vec![("incompleteContent", "book".to_owned())]
        );

        let document = format!("{BOOK}<book/>");
        assert_eq!(
            validate(&document),
            vec![("incompleteContent", "book".to_owned())]
        );

        let document = format!("{BOOK}<chapter id=\"a\"/>");
        assert_eq!(
            validate(&document),
            vec![("rootMismatch", "chapter".to_owned())]
        );

        let document = format!(
            "{BOOK}<book><title>t<para/></title><chapter id=\"a\"><para><em/></para></chapter></book>"
        );
        assert_eq!(
            validate(&document),
            vec![
                ("unexpectedElement", "para".to_owned()),
                ("unexpectedElement", "em".to_owned()),
            ]
        );
    }

    #[test]
    fn reports_attribute_problems() {
        let document = format!(
            "{BOOK}<book version=\"2.0\" lang=\"a b\" extra=\"1\" xml:lang=\"fr\" xsi:type=\"t\">\n  <title/>\n  <chapter kind=\"outro\" see=\"nowhere c1\"/>\n  <chapter id=\"c1\"/>\n  <chapter id=\"c1\" see=\"\"/>\n  <chapter id=\"1st\"/>\n</book>"
        );
        let problems = validate(&document);
        assert_eq!(
            problems,
            vec![
                ("fixedValue", "2.0".to_owned()),
                ("invalidAttributeValue", "a b".to_owned()),
                ("undeclaredAttribute", "extra".to_owned()),
                ("missingAttribute", "chapter".to_owned()),
                ("invalidEnumeration", "outro".to_owned()),
                ("unknownIdref", "nowhere".to_owned()),
                ("duplicateId", "c1".to_owned()),
                ("invalidAttributeValue", "".to_owned()),
                ("invalidAttributeValue", "1st".to_owned()),
            ]
        );
        let (_, dtd) = load_document_dtd(&document, None, &mut NoLoader).unwrap();
        let missing = validate_instance(&document, &dtd)
            .into_iter()
            .find(|problem| problem.kind.id() == "missingAttribute")
            .unwrap();
        let InstanceProblemKind::MissingAttribute {
            element,
            attribute,
            value,
            insert_at,
        } = missing.kind
        else {
            unreachable!()
        };
        assert_eq!(
            (element.as_str(), attribute.as_str(), value.as_str()),
            ("chapter", "id", "")
        );
        assert!(document[..insert_at].ends_with("see=\"nowhere c1\""));
        assert_eq!(&document[insert_at..insert_at + 2], "/>");

        // IDREF(S) tokens link to the first occurrence of their ID.
        let links = id_links(&document, &dtd)
            .into_iter()
            .map(|(reference, target)| (&document[reference], target))
            .collect::<Vec<_>>();
        let target = document.find("id=\"c1\"").unwrap() + 4;
        assert_eq!(links, [("c1", target..target + 2)]);
    }

    #[test]
    fn validates_entity_and_notation_attributes() {
        let document = "<!DOCTYPE doc [\n<!NOTATION gif SYSTEM \"image/gif\">\n<!ENTITY logo SYSTEM \"logo.gif\" NDATA gif>\n<!ENTITY text \"t\">\n<!ELEMENT doc EMPTY>\n<!ATTLIST doc img ENTITY #IMPLIED imgs ENTITIES #IMPLIED type NOTATION (gif) #IMPLIED>\n]>\n<doc img=\"logo\" imgs=\"logo text\" type=\"png\"/>";
        assert_eq!(
            validate(document),
            vec![
                ("invalidEntityAttribute", "text".to_owned()),
                ("invalidEnumeration", "png".to_owned()),
            ]
        );
    }

    #[test]
    fn checks_entity_references() {
        let document = "<!DOCTYPE r [\n<!ENTITY known \"k\">\n<!ENTITY ext SYSTEM \"e.xml\">\n<!ENTITY tag \"<b/>\">\n<!NOTATION gif SYSTEM \"g\">\n<!ENTITY img SYSTEM \"i.gif\" NDATA gif>\n<!ENTITY loop \"&loop;\">\n]>\n<r a=\"&known;&ext;&tag;&lt;\">&known; &unknown; &amp; &#10; &ext; &img; &loop;<!-- &c; --><![CDATA[&d;]]><?pi &e;?></r>";
        assert_eq!(
            entities(document),
            vec![
                ("externalEntityInAttribute", "&ext;".to_owned()),
                ("invalidAttributeValue", "&tag;".to_owned()),
                ("undefinedEntity", "&unknown;".to_owned()),
                ("unparsedEntityReference", "&img;".to_owned()),
                ("entityExpansion", "&loop;".to_owned()),
            ]
        );
        // Without a DTD, only the predefined entities are known.
        assert_eq!(
            entities("<r a='&x;'>&quot;&apos;&gt;&y;</r>"),
            vec![
                ("undefinedEntity", "&x;".to_owned()),
                ("undefinedEntity", "&y;".to_owned()),
            ]
        );
    }

    #[test]
    fn skips_content_models_behind_markup_entities() {
        let document =
            format!("{BOOK}<book><title>t</title><chapter id=\"a\">&frag;</chapter>&frag;</book>");
        // `&frag;` inserts elements: the content is not checked.
        assert_eq!(validate(&document), Vec::new());
    }

    #[test]
    fn dtd_without_elements_only_defines_entities() {
        let document =
            "<!DOCTYPE html [ <!ENTITY nbsp \"&#160;\"> ]><html><body>&nbsp;</body></html>";
        assert_eq!(validate(document), Vec::new());
        assert_eq!(entities(document), Vec::new());
    }

    #[test]
    fn finds_entity_references_at_offsets() {
        let source = "a &name; b %pe; c";
        assert_eq!(entity_reference_at(source, 3, false), Some(3..7));
        assert_eq!(entity_reference_at(source, 2, false), Some(3..7));
        assert_eq!(entity_reference_at(source, 8, false), Some(3..7));
        assert_eq!(entity_reference_at(source, 10, false), None);
        assert_eq!(entity_reference_at(source, 13, true), Some(12..14));
        assert_eq!(entity_reference_at("&é;", 1, false), Some(1..3));
        assert_eq!(entity_reference_at("& b;", 3, false), None);
    }

    #[test]
    fn bounds_attribute_normalization_over_the_document() {
        // "Quadratic blowup": a 512 KiB entity referenced by 2000
        // attribute values would copy 1 GiB without a document budget.
        let big = "x".repeat(512 * 1024);
        let mut document = format!(
            "<!DOCTYPE r [\n<!ELEMENT r (e*)>\n<!ELEMENT e EMPTY>\n<!ATTLIST e a CDATA #FIXED \"v\">\n<!ENTITY big \"{big}\">\n]>\n<r>"
        );
        for _ in 0..2000 {
            document.push_str("<e a=\"&big;\"/>");
        }
        document.push_str("</r>");
        let start = std::time::Instant::now();
        let problems = validate(&document);
        assert!(start.elapsed() < std::time::Duration::from_secs(20));
        let fixed = problems
            .iter()
            .filter(|(kind, _)| *kind == "fixedValue")
            .count();
        let budget = problems
            .iter()
            .filter(|(kind, _)| *kind == "expansionBudget")
            .count();
        // 8 MiB / 512 KiB values are checked, then the budget is reported
        // once and the other values are skipped.
        assert_eq!(budget, 1, "{problems:?}");
        assert_eq!(
            fixed,
            crate::MAX_DOCUMENT_EXPANSION / big.len(),
            "{problems:?}"
        );
        // Messages quote a bounded excerpt of the value.
        let (_, dtd) = load_document_dtd(&document, None, &mut NoLoader).unwrap();
        let problem = validate_instance(&document, &dtd)
            .into_iter()
            .find(|problem| problem.kind.id() == "fixedValue")
            .unwrap();
        assert!(problem.message.len() < 300, "{}", problem.message);
        assert!(problem.message.contains("xxx…"));
    }

    #[test]
    fn never_reads_external_general_entities() {
        struct Recording(Vec<String>);
        impl crate::ExternalLoader for Recording {
            fn load(
                &mut self,
                _: Option<&str>,
                system: &str,
                _: Option<&std::path::Path>,
            ) -> Result<(std::path::PathBuf, String), crate::LoadError> {
                self.0.push(system.to_owned());
                Err(crate::LoadError {
                    message: "not found".to_owned(),
                    remote: false,
                })
            }
        }
        let document = "<!DOCTYPE r [\n<!ELEMENT r ANY>\n<!ATTLIST r a CDATA #IMPLIED>\n<!ENTITY secret SYSTEM \"file:///etc/passwd\">\n<!ENTITY remote SYSTEM \"http://example.com/x.xml\">\n]>\n<r a=\"&secret;\">&secret;&remote;</r>";
        let mut loader = Recording(Vec::new());
        let (_, dtd) = load_document_dtd(document, None, &mut loader).unwrap();
        validate_instance(document, &dtd);
        let problems = check_entity_references(document, Some(&dtd));
        assert_eq!(loader.0, Vec::<String>::new());
        assert_eq!(
            problems
                .iter()
                .map(|problem| problem.kind.id())
                .collect::<Vec<_>>(),
            vec!["externalEntityInAttribute"]
        );
    }

    #[test]
    fn bounds_content_model_recognition() {
        // A model of 20 000 optional particles against 20 000 children
        // would cost 8·10⁸ state visits: skipped once over the budget.
        let model = vec!["a?"; 20_000].join(",");
        let children = "<a/>".repeat(20_000);
        let document = format!(
            "<!DOCTYPE r [<!ELEMENT r ({model})><!ELEMENT a EMPTY><!ELEMENT s ({model})>]><r>{children}</r>"
        );
        let start = std::time::Instant::now();
        assert_eq!(validate(&document), Vec::new());
        assert!(start.elapsed() < std::time::Duration::from_secs(20));
        // Small models are still checked, and messages quote a bounded
        // excerpt of a large model.
        let document = format!(
            "<!DOCTYPE r [<!ELEMENT r ({model}, b)><!ELEMENT a EMPTY><!ELEMENT b EMPTY>]><r><a/></r>"
        );
        let (_, dtd) = load_document_dtd(&document, None, &mut NoLoader).unwrap();
        let problems = validate_instance(&document, &dtd);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].message.len() < 400, "{}", problems[0].message);
    }

    #[test]
    fn an_undeclared_entity_is_only_a_validity_error_with_an_external_subset_or_parameter_references()
     {
        let load = |source: &str| {
            load_document_dtd(source, None, &mut crate::NoLoader)
                .map(|(_, dtd)| dtd)
                .unwrap()
        };
        let plain = load("<!DOCTYPE a [<!ELEMENT a ANY>]><a>&e;</a>");
        assert!(!plain.optional_declarations);
        let parameter = load("<!DOCTYPE a [<!ENTITY % p \"<!ENTITY x 'y'>\">%p;]><a>&e;</a>");
        assert!(parameter.optional_declarations);
        let external = load("<!DOCTYPE a SYSTEM \"a.dtd\"><a>&e;</a>");
        assert!(external.optional_declarations);
    }
}
