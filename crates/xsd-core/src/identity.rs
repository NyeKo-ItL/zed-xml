//! Identity constraints of XML Schema 1.0 (§3.11): `xs:unique`, `xs:key`
//! and `xs:keyref` with the restricted XPath of their `xs:selector` and
//! `xs:field`, and the document-wide `xs:ID`/`xs:IDREF(S)` checks.
//!
//! The schema side ([`XsdIdentityConstraint`], [`parse_xpath`]) is built by
//! the component model. The instance side works on the tree of elements
//! collected by the validator ([`InstanceNode`]): values are compared in
//! the value space of their simple types.

use std::{
    collections::{BTreeSet, HashMap},
    ops::Range,
};

use xml_core::names::{is_name_char, is_name_start_char};

use crate::{
    LocatedXsdDiagnostic, XsdDiagnosticKind,
    datatypes::{IdKind, Value},
    model::{XsdModelSet, XsdQName},
};

/// Kind of identity constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XsdIdentityKind {
    Unique,
    Key,
    KeyRef,
}

impl XsdIdentityKind {
    /// Schema element name (`unique`, `key`, `keyref`).
    pub fn element_name(self) -> &'static str {
        match self {
            Self::Unique => "unique",
            Self::Key => "key",
            Self::KeyRef => "keyref",
        }
    }

    fn describe(self, name: &str) -> String {
        match self {
            Self::Unique => format!("unique constraint '{name}'"),
            Self::Key => format!("key '{name}'"),
            Self::KeyRef => format!("keyref '{name}'"),
        }
    }
}

/// `xs:unique`, `xs:key` or `xs:keyref` of an element declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsdIdentityConstraint {
    pub kind: XsdIdentityKind,
    pub name: String,
    /// Target namespace of the schema (constraint names are qualified).
    pub namespace: Option<String>,
    /// Referenced key or unique constraint of a keyref.
    pub refer: Option<XsdQName>,
    pub selector: XsdXPath,
    pub fields: Vec<XsdXPath>,
}

/// Name test of a step (§3.11.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XsdNameTest {
    /// `*`
    Any,
    /// `prefix:*`
    Namespace(Option<String>),
    /// `name` (no namespace: the default namespace does not apply) or
    /// `prefix:name`.
    Name {
        namespace: Option<String>,
        local: String,
    },
}

impl XsdNameTest {
    fn matches(&self, namespace: Option<&str>, local: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Namespace(expected) => expected.as_deref() == namespace,
            Self::Name {
                namespace: expected,
                local: expected_local,
            } => expected_local == local && expected.as_deref() == namespace,
        }
    }
}

/// One alternative of a selector or field expression: `.//`? then child
/// steps (`None` for `.`), then an attribute test for a field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsdPath {
    pub descendants: bool,
    pub steps: Vec<Option<XsdNameTest>>,
    pub attribute: Option<XsdNameTest>,
}

/// Parsed `xpath` of a selector or field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsdXPath {
    /// The expression as written.
    pub expression: String,
    pub paths: Vec<XsdPath>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Dot,
    Slash,
    DoubleSlash,
    Bar,
    At,
    Star,
    Axis(String),
    Name(Option<String>, String),
    NamespaceWildcard(String),
}

fn tokenize(expression: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut characters = expression.char_indices().peekable();
    while let Some(&(index, character)) = characters.peek() {
        match character {
            ' ' | '\t' | '\n' | '\r' => {
                characters.next();
            }
            '.' => {
                characters.next();
                if matches!(characters.peek(), Some((_, '.'))) {
                    return Err("'..' is not allowed".to_owned());
                }
                tokens.push(Token::Dot);
            }
            '/' => {
                characters.next();
                if matches!(characters.peek(), Some((_, '/'))) {
                    characters.next();
                    tokens.push(Token::DoubleSlash);
                } else {
                    tokens.push(Token::Slash);
                }
            }
            '|' => {
                characters.next();
                tokens.push(Token::Bar);
            }
            '@' => {
                characters.next();
                tokens.push(Token::At);
            }
            '*' => {
                characters.next();
                tokens.push(Token::Star);
            }
            character if is_name_start_char(character) && character != ':' => {
                let end = scan_ncname(expression, index);
                let name = &expression[index..end];
                while matches!(characters.peek(), Some(&(next, _)) if next < end) {
                    characters.next();
                }
                let rest = &expression[end..];
                if let Some(after) = rest.trim_start().strip_prefix("::") {
                    let consumed = expression.len() - after.len();
                    while matches!(characters.peek(), Some(&(next, _)) if next < consumed) {
                        characters.next();
                    }
                    tokens.push(Token::Axis(name.to_owned()));
                } else if let Some(after) = rest.strip_prefix(':') {
                    if after.starts_with('*') {
                        characters.next();
                        characters.next();
                        tokens.push(Token::NamespaceWildcard(name.to_owned()));
                    } else if after
                        .chars()
                        .next()
                        .is_some_and(|next| is_name_start_char(next) && next != ':')
                    {
                        let local_start = end + 1;
                        let local_end = scan_ncname(expression, local_start);
                        while matches!(characters.peek(), Some(&(next, _)) if next < local_end) {
                            characters.next();
                        }
                        tokens.push(Token::Name(
                            Some(name.to_owned()),
                            expression[local_start..local_end].to_owned(),
                        ));
                    } else {
                        return Err(format!("invalid name '{name}:'"));
                    }
                } else {
                    tokens.push(Token::Name(None, name.to_owned()));
                }
            }
            other => return Err(format!("unexpected character '{other}'")),
        }
    }
    Ok(tokens)
}

/// End of the NCName starting at `start`.
fn scan_ncname(text: &str, start: usize) -> usize {
    text[start..]
        .char_indices()
        .find(|&(offset, character)| {
            character == ':'
                || if offset == 0 {
                    !is_name_start_char(character)
                } else {
                    !is_name_char(character)
                }
        })
        .map_or(text.len(), |(offset, _)| start + offset)
}

/// Parses the `xpath` of a selector (`field == false`) or of a field, in
/// the XPath subset of XML Schema 1.0 (§3.11.6); `resolve` gives the
/// namespace bound to a prefix in the schema.
pub fn parse_xpath(
    expression: &str,
    field: bool,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Result<XsdXPath, String> {
    let tokens = tokenize(expression)?;
    if tokens.is_empty() {
        return Err("empty XPath expression".to_owned());
    }
    let name_test = |token: &Token| -> Result<XsdNameTest, String> {
        match token {
            Token::Star => Ok(XsdNameTest::Any),
            Token::Name(None, local) => Ok(XsdNameTest::Name {
                namespace: None,
                local: local.clone(),
            }),
            Token::Name(Some(prefix), local) => Ok(XsdNameTest::Name {
                namespace: Some(
                    resolve(prefix)
                        .ok_or_else(|| format!("the prefix '{prefix}' is not declared"))?,
                ),
                local: local.clone(),
            }),
            Token::NamespaceWildcard(prefix) => Ok(XsdNameTest::Namespace(Some(
                resolve(prefix).ok_or_else(|| format!("the prefix '{prefix}' is not declared"))?,
            ))),
            _ => Err("a name test is expected".to_owned()),
        }
    };
    let mut paths = Vec::new();
    for alternative in tokens.split(|token| *token == Token::Bar) {
        let mut path = XsdPath {
            descendants: false,
            steps: Vec::new(),
            attribute: None,
        };
        let mut index = 0;
        if alternative.first() == Some(&Token::Dot)
            && alternative.get(1) == Some(&Token::DoubleSlash)
        {
            path.descendants = true;
            index = 2;
        }
        loop {
            let token = alternative
                .get(index)
                .ok_or_else(|| "a step is expected".to_owned())?;
            index += 1;
            match token {
                Token::Dot => path.steps.push(None),
                Token::At => {
                    let test = alternative
                        .get(index)
                        .ok_or_else(|| "a name test is expected after '@'".to_owned())?;
                    index += 1;
                    path.attribute = Some(name_test(test)?);
                }
                Token::Axis(axis) if axis == "attribute" => {
                    let test = alternative
                        .get(index)
                        .ok_or_else(|| "a name test is expected after 'attribute::'".to_owned())?;
                    index += 1;
                    path.attribute = Some(name_test(test)?);
                }
                Token::Axis(axis) if axis == "child" => {
                    let test = alternative
                        .get(index)
                        .ok_or_else(|| "a name test is expected after 'child::'".to_owned())?;
                    index += 1;
                    path.steps.push(Some(name_test(test)?));
                }
                Token::Axis(axis) => return Err(format!("the axis '{axis}::' is not allowed")),
                Token::Slash | Token::DoubleSlash => {
                    return Err(
                        "absolute paths and '//' are not allowed ('.//' only at the start)"
                            .to_owned(),
                    );
                }
                other => path.steps.push(Some(name_test(other)?)),
            }
            match alternative.get(index) {
                None => break,
                Some(Token::Slash) if path.attribute.is_none() => index += 1,
                Some(Token::DoubleSlash) => {
                    return Err("'//' is only allowed at the start, as './/'".to_owned());
                }
                Some(_) => return Err("'/' or '|' is expected between steps".to_owned()),
            }
        }
        if path.attribute.is_some() && !field {
            return Err("a selector cannot select attributes".to_owned());
        }
        paths.push(path);
    }
    Ok(XsdXPath {
        expression: expression.to_owned(),
        paths,
    })
}

impl XsdModelSet {
    /// Errors of the keyrefs across the schemas: `refer` naming no key or
    /// unique constraint (when its namespace is loaded), or a number of
    /// fields different from the referenced constraint.
    pub fn identity_problems(&self) -> Vec<String> {
        let constraints = self
            .models()
            .iter()
            .flat_map(|model| model.identity_constraints.iter())
            .collect::<Vec<_>>();
        let mut problems = Vec::new();
        for keyref in &constraints {
            let Some(refer) = keyref
                .refer
                .as_ref()
                .filter(|_| keyref.kind == XsdIdentityKind::KeyRef)
            else {
                continue;
            };
            let target = constraints.iter().find(|candidate| {
                candidate.kind != XsdIdentityKind::KeyRef
                    && candidate.name == refer.local
                    && candidate.namespace == refer.namespace
            });
            match target {
                Some(target) if target.fields.len() != keyref.fields.len() => {
                    problems.push(format!(
                        "the keyref '{}' has {} field(s) but the {} '{}' it refers to has {}",
                        keyref.name,
                        keyref.fields.len(),
                        target.kind.element_name(),
                        target.name,
                        target.fields.len()
                    ));
                }
                Some(_) => {}
                None => {
                    // The XML Schema namespace declares no constraint.
                    let loaded = refer.is_builtin()
                        || self
                            .models()
                            .iter()
                            .any(|model| model.target_namespace == refer.namespace);
                    if loaded {
                        problems.push(format!(
                            "the keyref '{}' refers to '{}', which is not a declared key or unique constraint",
                            keyref.name,
                            refer.display()
                        ));
                    }
                }
            }
        }
        problems
    }
}

/// Link from an ID reference or keyref value to the ID or key value it
/// designates (UTF-8 ranges of the instance document).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityLink {
    pub reference: Range<usize>,
    pub target: Range<usize>,
}

/// Simple value of an element or attribute of the instance.
#[derive(Debug, Clone)]
pub(crate) struct InstanceValue {
    pub(crate) value: Value,
    /// Value after whitespace processing (for messages and IDs).
    pub(crate) text: String,
    pub(crate) range: Range<usize>,
    /// The value is valid for its type (IDs are only checked then).
    pub(crate) valid: bool,
    pub(crate) id: Option<IdKind>,
}

#[derive(Debug, Clone)]
pub(crate) struct InstanceAttribute {
    pub(crate) namespace: Option<String>,
    pub(crate) local: String,
    pub(crate) value: InstanceValue,
}

/// Element of the instance, in document order (pre-order).
#[derive(Debug, Clone)]
pub(crate) struct InstanceNode<'s> {
    pub(crate) children: Vec<usize>,
    /// Index of the last descendant (the node itself without children).
    pub(crate) last: usize,
    pub(crate) namespace: Option<String>,
    pub(crate) local: String,
    /// Qualified name as written.
    pub(crate) name: String,
    pub(crate) location: Range<usize>,
    pub(crate) constraints: &'s [XsdIdentityConstraint],
    pub(crate) attributes: Vec<InstanceAttribute>,
    /// Simple content (`None` for element content).
    pub(crate) value: Option<InstanceValue>,
    pub(crate) nil: bool,
}

/// Key sequence of a selected node.
struct Entry {
    key: String,
    values: Vec<Value>,
    texts: Vec<String>,
    ranges: Vec<Range<usize>>,
}

impl Entry {
    fn display(&self) -> String {
        match self.texts.as_slice() {
            [text] => format!("'{text}'"),
            texts => format!(
                "({})",
                texts
                    .iter()
                    .map(|text| format!("'{text}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    fn equals(&self, other: &Entry) -> bool {
        self.key == other.key
            && self.values.len() == other.values.len()
            && self
                .values
                .iter()
                .zip(&other.values)
                .all(|(left, right)| left.equals(right))
    }
}

fn diagnostic(
    kind: XsdDiagnosticKind,
    range: &Range<usize>,
    message: String,
) -> LocatedXsdDiagnostic {
    LocatedXsdDiagnostic {
        kind,
        message,
        offset: range.start,
        end: range.end,
    }
}

/// Range of `token` in the value at `range`, or the whole value.
fn token_range(source: &str, range: &Range<usize>, token: &str) -> Range<usize> {
    source
        .get(range.clone())
        .and_then(|raw| raw.find(token))
        .map_or(range.clone(), |offset| {
            range.start + offset..range.start + offset + token.len()
        })
}

/// Checks the IDs, ID references and identity constraints of the instance
/// tree; returns the diagnostics and the links of the references.
pub(crate) fn check(
    source: &str,
    nodes: &[InstanceNode<'_>],
) -> (Vec<LocatedXsdDiagnostic>, Vec<IdentityLink>) {
    let mut diagnostics = Vec::new();
    let mut links = Vec::new();
    check_ids(source, nodes, &mut diagnostics, &mut links);
    if nodes.iter().any(|node| !node.constraints.is_empty()) {
        check_constraints(nodes, &mut diagnostics, &mut links);
    }
    (diagnostics, links)
}

fn check_ids(
    source: &str,
    nodes: &[InstanceNode<'_>],
    diagnostics: &mut Vec<LocatedXsdDiagnostic>,
    links: &mut Vec<IdentityLink>,
) {
    let mut ids: HashMap<&str, Range<usize>> = HashMap::new();
    let mut references: Vec<(&str, Range<usize>)> = Vec::new();
    let values = nodes.iter().flat_map(|node| {
        node.attributes
            .iter()
            .map(|attribute| &attribute.value)
            .chain(node.value.iter())
    });
    for value in values {
        if !value.valid {
            continue;
        }
        match value.id {
            Some(IdKind::Id) => {
                let id = value.text.as_str();
                let range = token_range(source, &value.range, id);
                if ids.contains_key(id) {
                    diagnostics.push(diagnostic(
                        XsdDiagnosticKind::DuplicateId,
                        &range,
                        format!("the ID '{id}' is already used in the document"),
                    ));
                } else {
                    ids.insert(id, range);
                }
            }
            Some(IdKind::IdRef | IdKind::IdRefs) => {
                for token in value.text.split_whitespace() {
                    references.push((token, token_range(source, &value.range, token)));
                }
            }
            None => {}
        }
    }
    for (reference, range) in references {
        match ids.get(reference) {
            Some(target) => links.push(IdentityLink {
                reference: range,
                target: target.clone(),
            }),
            None => diagnostics.push(diagnostic(
                XsdDiagnosticKind::UnknownIdref,
                &range,
                format!("no element has the ID '{reference}'"),
            )),
        }
    }
}

/// Nodes selected by `paths` from `context`, in document order.
fn select(nodes: &[InstanceNode<'_>], context: usize, path: &XsdPath) -> Vec<usize> {
    let mut current: Vec<usize> = if path.descendants {
        (context..=nodes[context].last.max(context)).collect()
    } else {
        vec![context]
    };
    for test in path.steps.iter().flatten() {
        current = current
            .iter()
            .flat_map(|&index| nodes[index].children.iter().copied())
            .filter(|&child| test.matches(nodes[child].namespace.as_deref(), &nodes[child].local))
            .collect();
    }
    current
}

fn select_all(nodes: &[InstanceNode<'_>], context: usize, xpath: &XsdXPath) -> Vec<usize> {
    let mut selected = BTreeSet::new();
    for path in &xpath.paths {
        selected.extend(select(nodes, context, path));
    }
    selected.into_iter().collect()
}

/// Node selected by a field.
enum FieldHit<'n> {
    Attribute(&'n InstanceValue),
    Element(usize),
}

fn field_hits<'n>(
    nodes: &'n [InstanceNode<'_>],
    context: usize,
    field: &XsdXPath,
) -> Vec<FieldHit<'n>> {
    let mut elements = BTreeSet::new();
    let mut attributes = Vec::new();
    for path in &field.paths {
        let selected = select(nodes, context, path);
        match &path.attribute {
            Some(test) => {
                for index in selected {
                    for attribute in &nodes[index].attributes {
                        if test.matches(attribute.namespace.as_deref(), &attribute.local)
                            && !attributes.iter().any(|existing: &&InstanceAttribute| {
                                std::ptr::eq(*existing, attribute)
                            })
                        {
                            attributes.push(attribute);
                        }
                    }
                }
            }
            None => elements.extend(selected),
        }
    }
    elements
        .into_iter()
        .map(FieldHit::Element)
        .chain(
            attributes
                .into_iter()
                .map(|attribute| FieldHit::Attribute(&attribute.value)),
        )
        .collect()
}

/// Key sequences of `constraint` evaluated at `context`.
fn evaluate(
    nodes: &[InstanceNode<'_>],
    context: usize,
    constraint: &XsdIdentityConstraint,
    diagnostics: &mut Vec<LocatedXsdDiagnostic>,
) -> Vec<Entry> {
    let described = constraint.kind.describe(&constraint.name);
    let mut entries = Vec::new();
    for selected in select_all(nodes, context, &constraint.selector) {
        let node = &nodes[selected];
        let mut entry = Entry {
            key: String::new(),
            values: Vec::new(),
            texts: Vec::new(),
            ranges: Vec::new(),
        };
        let mut complete = true;
        for field in &constraint.fields {
            let hits = field_hits(nodes, selected, field);
            let value = match hits.as_slice() {
                [] => None,
                [FieldHit::Attribute(value)] => Some(*value),
                [FieldHit::Element(index)] => {
                    let element = &nodes[*index];
                    if element.nil {
                        if constraint.kind == XsdIdentityKind::Key {
                            diagnostics.push(diagnostic(
                                XsdDiagnosticKind::MissingKeyField,
                                &element.location,
                                format!(
                                    "the field '{}' of the {described} selects <{}>, which is nil",
                                    field.expression, element.name
                                ),
                            ));
                        }
                        complete = false;
                        continue;
                    }
                    match &element.value {
                        Some(value) => Some(value),
                        None => {
                            diagnostics.push(diagnostic(
                                XsdDiagnosticKind::InvalidKeyField,
                                &element.location,
                                format!(
                                    "the field '{}' of the {described} selects <{}>, which has no simple value",
                                    field.expression, element.name
                                ),
                            ));
                            complete = false;
                            continue;
                        }
                    }
                }
                _ => {
                    diagnostics.push(diagnostic(
                        XsdDiagnosticKind::InvalidKeyField,
                        &node.location,
                        format!(
                            "the field '{}' of the {described} selects more than one node in <{}>",
                            field.expression, node.name
                        ),
                    ));
                    complete = false;
                    continue;
                }
            };
            match value {
                Some(value) => {
                    entry.key.push_str(&value.value.identity_key());
                    entry.key.push('\u{2}');
                    entry.values.push(value.value.clone());
                    entry.texts.push(value.text.clone());
                    entry.ranges.push(value.range.clone());
                }
                None => {
                    if constraint.kind == XsdIdentityKind::Key {
                        diagnostics.push(diagnostic(
                            XsdDiagnosticKind::MissingKeyField,
                            &node.location,
                            format!(
                                "the field '{}' of the {described} is missing in <{}>",
                                field.expression, node.name
                            ),
                        ));
                    }
                    complete = false;
                }
            }
        }
        if complete && !entry.values.is_empty() {
            entries.push(entry);
        }
    }
    if constraint.kind != XsdIdentityKind::KeyRef {
        let mut buckets: HashMap<&str, Vec<usize>> = HashMap::new();
        for (index, entry) in entries.iter().enumerate() {
            let bucket = buckets.entry(entry.key.as_str()).or_default();
            if bucket
                .iter()
                .any(|&previous| entries[previous].equals(entry))
            {
                diagnostics.push(diagnostic(
                    XsdDiagnosticKind::DuplicateKey,
                    &entry.ranges[0],
                    format!("duplicate value {} of the {described}", entry.display()),
                ));
            } else {
                bucket.push(index);
            }
        }
    }
    entries
}

fn check_constraints(
    nodes: &[InstanceNode<'_>],
    diagnostics: &mut Vec<LocatedXsdDiagnostic>,
    links: &mut Vec<IdentityLink>,
) {
    // Key and unique tables of every node declaring them.
    let mut tables: HashMap<(usize, usize), Vec<Entry>> = HashMap::new();
    for (index, node) in nodes.iter().enumerate() {
        for (position, constraint) in node.constraints.iter().enumerate() {
            if constraint.kind != XsdIdentityKind::KeyRef {
                let entries = evaluate(nodes, index, constraint, diagnostics);
                tables.insert((index, position), entries);
            }
        }
    }
    for (index, node) in nodes.iter().enumerate() {
        for constraint in node.constraints {
            let Some(refer) = constraint
                .refer
                .as_ref()
                .filter(|_| constraint.kind == XsdIdentityKind::KeyRef)
            else {
                continue;
            };
            let references = evaluate(nodes, index, constraint, diagnostics);
            if references.is_empty() {
                continue;
            }
            // Keys of the referenced constraint declared on the node or
            // its descendants.
            let mut targets: HashMap<&str, Vec<&Entry>> = HashMap::new();
            let end = node.last.max(index);
            for (scope, scoped) in nodes.iter().enumerate().take(end + 1).skip(index) {
                for (position, candidate) in scoped.constraints.iter().enumerate() {
                    if candidate.kind == XsdIdentityKind::KeyRef
                        || candidate.name != refer.local
                        || candidate.namespace != refer.namespace
                    {
                        continue;
                    }
                    for entry in tables.get(&(scope, position)).into_iter().flatten() {
                        targets.entry(entry.key.as_str()).or_default().push(entry);
                    }
                }
            }
            for reference in &references {
                let target = targets.get(reference.key.as_str()).and_then(|bucket| {
                    bucket
                        .iter()
                        .find(|candidate| candidate.equals(reference))
                        .copied()
                });
                match target {
                    Some(target) => {
                        for (reference, target) in reference.ranges.iter().zip(&target.ranges) {
                            links.push(IdentityLink {
                                reference: reference.clone(),
                                target: target.clone(),
                            });
                        }
                    }
                    None => diagnostics.push(diagnostic(
                        XsdDiagnosticKind::UnknownKeyref,
                        &reference.ranges[0],
                        format!(
                            "the value {} of the keyref '{}' matches no '{}' in scope",
                            reference.display(),
                            constraint.name,
                            refer.local
                        ),
                    )),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(prefix: &str) -> Option<String> {
        (prefix == "t").then(|| "urn:t".to_owned())
    }

    fn name(namespace: Option<&str>, local: &str) -> Option<XsdNameTest> {
        Some(XsdNameTest::Name {
            namespace: namespace.map(str::to_owned),
            local: local.to_owned(),
        })
    }

    #[test]
    fn parses_selector_and_field_expressions() {
        let selector = parse_xpath(".//t:item | child::a/ * ", false, &resolve).unwrap();
        assert_eq!(
            selector.paths,
            vec![
                XsdPath {
                    descendants: true,
                    steps: vec![name(Some("urn:t"), "item")],
                    attribute: None,
                },
                XsdPath {
                    descendants: false,
                    steps: vec![name(None, "a"), Some(XsdNameTest::Any)],
                    attribute: None,
                },
            ]
        );
        let field = parse_xpath("./t:*/@id|attribute::t:code", true, &resolve).unwrap();
        assert_eq!(
            field.paths[0].steps,
            vec![None, Some(XsdNameTest::Namespace(Some("urn:t".to_owned())))]
        );
        assert_eq!(field.paths[0].attribute, name(None, "id"));
        assert_eq!(field.paths[1].attribute, name(Some("urn:t"), "code"));
        assert_eq!(
            parse_xpath(".", true, &resolve).unwrap().paths[0].steps,
            vec![None]
        );
    }

    #[test]
    fn rejects_expressions_outside_the_subset() {
        for (expression, field) in [
            ("", false),
            ("/a", false),
            ("a//b", false),
            ("..", false),
            ("@id", false),
            ("a/@id/b", true),
            ("u:a", false),
            ("descendant::a", false),
            ("a[1]", false),
            ("a |", false),
            ("a b", false),
        ] {
            assert!(
                parse_xpath(expression, field, &resolve).is_err(),
                "{expression} should be rejected"
            );
        }
    }
}
