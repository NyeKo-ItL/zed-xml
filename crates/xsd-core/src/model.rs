//! XSD component model keeping the schema tree and the
//! `xs:annotation/xs:documentation` documentation.
//!
//! [`XsdSchema`](crate::XsdSchema) flattens the schema by local name for
//! validation; this model instead keeps the global and local declarations,
//! named and anonymous types, groups and facets, with resolved qualified
//! names. It serves hover (`textDocument/hover`): resolution of an element
//! declaration in its context (path of ancestors), of the attributes of a
//! type and of the facets of a simple type.
//!
//! Documentation is normalized: an `xs:documentation` block without
//! `xml:lang` or in English is preferred, multiple blocks are concatenated,
//! nested markup (XHTML...) is reduced to its text and whitespace is
//! grouped into paragraphs. `xs:appinfo` is ignored.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    sync::Arc,
};

use quick_xml::{Reader, events::Event};
use xml_core::names::is_ncname;

use crate::identity::{XsdIdentityConstraint, XsdIdentityKind, parse_xpath};

/// XML Schema namespace.
pub const XSD_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema";
/// Reserved namespace of the `xml` prefix.
pub const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";

/// Maximum depth followed through references (groups, derivations), to
/// guard against cyclic schemas.
pub(crate) const MAX_DEPTH: usize = 32;

/// Qualified name read from a schema attribute value (`type`, `ref`,
/// `base`...), with its resolved namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsdQName {
    pub prefix: Option<String>,
    pub namespace: Option<String>,
    pub local: String,
}

impl XsdQName {
    /// Name as written in the schema (`prefix:local`).
    pub fn display(&self) -> String {
        match &self.prefix {
            Some(prefix) => format!("{prefix}:{}", self.local),
            None => self.local.clone(),
        }
    }

    /// Whether the name designates an XML Schema component (`xs:string`...).
    pub fn is_builtin(&self) -> bool {
        self.namespace.as_deref() == Some(XSD_NAMESPACE)
    }
}

/// Attribute usage (`use`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum XsdUse {
    #[default]
    Optional,
    Required,
    Prohibited,
}

/// Element declaration, global or local (possibly a `ref`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsdElementDecl {
    /// Local name (that of the reference for a `ref`).
    pub name: String,
    pub reference: Option<XsdQName>,
    /// Effective namespace of the instances (`form`, `elementFormDefault`).
    pub namespace: Option<String>,
    pub type_name: Option<XsdQName>,
    pub anonymous_type: Option<Box<XsdTypeDef>>,
    pub min_occurs: usize,
    pub max_occurs: Option<usize>,
    pub default: Option<String>,
    pub fixed: Option<String>,
    pub nillable: bool,
    pub is_abstract: bool,
    /// `block` (or the schema's `blockDefault`) includes `substitution`:
    /// members of the substitution group cannot replace this element.
    pub blocks_substitution: bool,
    /// `block` (or `blockDefault`) includes `extension` / `restriction`:
    /// `xsi:type` cannot name a type derived that way from the declared one.
    pub blocks_extension: bool,
    pub blocks_restriction: bool,
    pub substitution_groups: Vec<XsdQName>,
    pub global: bool,
    pub documentation: Option<String>,
    /// `xs:unique`, `xs:key` and `xs:keyref` of the declaration.
    pub identity_constraints: Vec<XsdIdentityConstraint>,
}

/// Attribute declaration, global or local (possibly a `ref`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsdAttributeDecl {
    pub name: String,
    pub reference: Option<XsdQName>,
    /// Effective namespace (`form`, `attributeFormDefault`).
    pub namespace: Option<String>,
    pub type_name: Option<XsdQName>,
    pub anonymous_type: Option<Box<XsdTypeDef>>,
    pub usage: XsdUse,
    pub default: Option<String>,
    pub fixed: Option<String>,
    pub global: bool,
    pub documentation: Option<String>,
}

/// Derivation method of a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XsdDerivation {
    Restriction,
    Extension,
    List,
    Union,
}

/// Enumeration value and its documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsdEnumeration {
    pub value: String,
    pub documentation: Option<String>,
}

/// Facets of a simple type restriction (raw schema values).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XsdFacets {
    pub enumerations: Vec<XsdEnumeration>,
    pub patterns: Vec<String>,
    pub length: Option<String>,
    pub min_length: Option<String>,
    pub max_length: Option<String>,
    pub min_inclusive: Option<String>,
    pub max_inclusive: Option<String>,
    pub min_exclusive: Option<String>,
    pub max_exclusive: Option<String>,
    pub total_digits: Option<String>,
    pub fraction_digits: Option<String>,
    pub white_space: Option<String>,
}

impl XsdFacets {
    /// Whether no facet is defined.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Fills in missing facets with those of the base type (the facets of
    /// the derived type take precedence).
    fn inherit(&mut self, base: &XsdFacets) {
        if self.enumerations.is_empty() {
            self.enumerations = base.enumerations.clone();
        }
        for pattern in &base.patterns {
            if !self.patterns.contains(pattern) {
                self.patterns.push(pattern.clone());
            }
        }
        let fields: [(&mut Option<String>, &Option<String>); 10] = [
            (&mut self.length, &base.length),
            (&mut self.min_length, &base.min_length),
            (&mut self.max_length, &base.max_length),
            (&mut self.min_inclusive, &base.min_inclusive),
            (&mut self.max_inclusive, &base.max_inclusive),
            (&mut self.min_exclusive, &base.min_exclusive),
            (&mut self.max_exclusive, &base.max_exclusive),
            (&mut self.total_digits, &base.total_digits),
            (&mut self.fraction_digits, &base.fraction_digits),
            (&mut self.white_space, &base.white_space),
        ];
        for (field, inherited) in fields {
            if field.is_none() {
                field.clone_from(inherited);
            }
        }
    }
}

/// Compositor of a model group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XsdCompositor {
    Sequence,
    Choice,
    All,
}

/// Particle of a content model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XsdParticle {
    /// Element declaration or `ref`, with its own occurrence constraints.
    Element(Box<XsdElementDecl>),
    Group {
        compositor: XsdCompositor,
        particles: Vec<XsdParticle>,
        min_occurs: usize,
        max_occurs: Option<usize>,
    },
    /// `xs:group ref`.
    GroupRef {
        name: XsdQName,
        min_occurs: usize,
        max_occurs: Option<usize>,
    },
    /// `xs:any` wildcard.
    Any(XsdWildcard),
}

/// `xs:any` wildcard of a content model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsdWildcard {
    /// XSD 1.1 `notNamespace` or `notQName`: not modelled, so not compared.
    pub has_exclusions: bool,
    pub process_contents: XsdProcessContents,
    pub namespaces: XsdWildcardNamespaces,
    pub min_occurs: usize,
    pub max_occurs: Option<usize>,
}

/// `namespace` constraint of a wildcard, with `##targetNamespace` and
/// `##local` resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XsdWildcardNamespaces {
    /// `##any` (the default).
    Any,
    /// `##other`: any namespace but the target namespace of the schema
    /// (`None` when it has none) and the absent namespace.
    Other(Option<String>),
    /// An explicit list; `None` is the absent namespace (`##local`).
    Set(Vec<Option<String>>),
}

impl XsdWildcardNamespaces {
    /// Whether an element in `namespace` (`None` when it has none) is
    /// allowed.
    pub fn allows(&self, namespace: Option<&str>) -> bool {
        match self {
            Self::Any => true,
            Self::Other(target) => namespace.is_some() && namespace != target.as_deref(),
            Self::Set(namespaces) => namespaces
                .iter()
                .any(|allowed| allowed.as_deref() == namespace),
        }
    }
}

impl XsdParticle {
    /// `(minOccurs, maxOccurs)` of the particle (`None` is unbounded).
    pub fn occurs(&self) -> (usize, Option<usize>) {
        match self {
            Self::Element(declaration) => (declaration.min_occurs, declaration.max_occurs),
            Self::Group {
                min_occurs,
                max_occurs,
                ..
            }
            | Self::GroupRef {
                min_occurs,
                max_occurs,
                ..
            } => (*min_occurs, *max_occurs),
            Self::Any(wildcard) => (wildcard.min_occurs, wildcard.max_occurs),
        }
    }
}

/// `processContents` of a wildcard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum XsdProcessContents {
    #[default]
    Strict,
    Lax,
    Skip,
}

/// Simple or complex type definition, named or anonymous.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XsdTypeDef {
    pub name: Option<String>,
    pub namespace: Option<String>,
    pub complex: bool,
    pub mixed: bool,
    pub simple_content: bool,
    /// `abstract="true"` (complex types): instances need an `xsi:type`.
    pub is_abstract: bool,
    /// `block` (or `blockDefault`) of a complex type: types derived that way
    /// cannot replace it through `xsi:type`.
    pub blocks_extension: bool,
    pub blocks_restriction: bool,
    pub derivation: Option<XsdDerivation>,
    /// Base type (`restriction`/`extension`).
    pub base: Option<XsdQName>,
    /// Item type of a list (`itemType`).
    pub item_type: Option<XsdQName>,
    /// Member types of a union (`memberTypes`).
    pub member_types: Vec<XsdQName>,
    /// Anonymous simple types of the restriction, list or union.
    pub inline_types: Vec<XsdTypeDef>,
    pub content: Option<XsdParticle>,
    pub attributes: Vec<XsdAttributeDecl>,
    pub attribute_group_refs: Vec<XsdQName>,
    /// `xs:anyAttribute` (its `minOccurs`/`maxOccurs` are 0 and 1).
    pub any_attribute: Option<XsdWildcard>,
    pub facets: XsdFacets,
    pub documentation: Option<String>,
}

/// Named model group (`xs:group`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsdGroupDef {
    pub name: String,
    pub namespace: Option<String>,
    pub content: Option<XsdParticle>,
    pub documentation: Option<String>,
}

/// Named attribute group (`xs:attributeGroup`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsdAttributeGroupDef {
    pub name: String,
    pub namespace: Option<String>,
    pub attributes: Vec<XsdAttributeDecl>,
    pub attribute_group_refs: Vec<XsdQName>,
    pub any_attribute: Option<XsdWildcard>,
    pub documentation: Option<String>,
}

/// Components of an XSD document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XsdModel {
    pub target_namespace: Option<String>,
    pub element_form_qualified: bool,
    pub attribute_form_qualified: bool,
    pub documentation: Option<String>,
    pub elements: Vec<XsdElementDecl>,
    pub attributes: Vec<XsdAttributeDecl>,
    pub types: Vec<XsdTypeDef>,
    pub groups: Vec<XsdGroupDef>,
    pub attribute_groups: Vec<XsdAttributeGroupDef>,
    /// Every identity constraint of the document (global and local
    /// declarations).
    pub identity_constraints: Vec<XsdIdentityConstraint>,
    /// Components redefined or overridden by the document, as
    /// `(kind, name)` with kind `type`, `group` or `attributeGroup`: they
    /// legitimately exist twice in a schema set.
    pub redefined: Vec<(&'static str, String)>,
    /// Errors of the components (the schema is invalid but usable).
    pub problems: Vec<String>,
}

/// Parses an XSD document into a model of documented components.
pub fn parse_xsd_model(source: &str) -> Result<XsdModel, String> {
    if source.len() > crate::MAX_XSD_SOURCE_BYTES {
        return Err("XSD schema too large".to_owned());
    }
    let (root, scopes) = build_tree(source)?;
    if !is_xsd(&root, "schema") {
        return Err("the root is not an xs:schema".to_owned());
    }
    let target_namespace = root.attribute("targetNamespace").filter(|v| !v.is_empty());
    let context = Context {
        scopes: &scopes,
        element_form_qualified: root.attribute("elementFormDefault").as_deref()
            == Some("qualified"),
        attribute_form_qualified: root.attribute("attributeFormDefault").as_deref()
            == Some("qualified"),
        target_namespace,
        block_default: root.attribute("blockDefault"),
        identity_constraints: RefCell::default(),
        problems: RefCell::default(),
    };
    let mut model = XsdModel {
        target_namespace: context.target_namespace.clone(),
        element_form_qualified: context.element_form_qualified,
        attribute_form_qualified: context.attribute_form_qualified,
        documentation: documentation(&root),
        ..XsdModel::default()
    };
    context.top_level(&root, &mut model);
    model.identity_constraints = context.identity_constraints.take();
    let mut names = HashSet::new();
    for constraint in &model.identity_constraints {
        if !names.insert(constraint.name.as_str()) {
            context.problems.borrow_mut().push(format!(
                "the identity constraint name '{}' is declared twice",
                constraint.name
            ));
        }
    }
    model.problems = context.problems.take();
    Ok(model)
}

/// Element of the minimal XML tree built for interpretation.
struct Node {
    local: String,
    namespace: Option<String>,
    attributes: Vec<(String, String)>,
    children: Vec<Child>,
    scope: usize,
}

enum Child {
    Element(Node),
    Text(String),
}

impl Node {
    /// Unprefixed attribute.
    fn attribute(&self, name: &str) -> Option<String> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    }

    fn elements(&self) -> impl Iterator<Item = &Node> {
        self.children.iter().filter_map(|child| match child {
            Child::Element(node) => Some(node),
            Child::Text(_) => None,
        })
    }

    fn xsd_children<'a>(&'a self, local: &'a str) -> impl Iterator<Item = &'a Node> {
        self.elements().filter(move |node| is_xsd(node, local))
    }
}

/// XSD element: XML Schema namespace, or none (undeclared prefix,
/// tolerated as validation does).
fn is_xsd(node: &Node, local: &str) -> bool {
    node.local == local
        && node
            .namespace
            .as_deref()
            .is_none_or(|namespace| namespace == XSD_NAMESPACE)
}

/// Namespace declarations of an element (`xmlns`, `xmlns:p`), chained to
/// the scope of its parent: a scope is never copied, so a document declaring
/// prefixes on many elements stays linear in memory.
struct Scope {
    parent: Option<usize>,
    declarations: HashMap<String, String>,
}

/// Namespace bound to `prefix` (`""`: default namespace) in the scope
/// `index` or its ancestors.
fn scope_lookup<'s>(scopes: &'s [Scope], index: usize, prefix: &str) -> Option<&'s String> {
    let mut current = Some(index);
    while let Some(index) = current {
        let scope = scopes.get(index)?;
        if let Some(namespace) = scope.declarations.get(prefix) {
            return Some(namespace);
        }
        current = scope.parent;
    }
    None
}

/// Maximum nesting depth of a schema document: the component model is
/// built recursively, and real schemas nest a few dozen levels at most.
pub(crate) const MAX_SCHEMA_DEPTH: usize = 256;

fn build_tree(source: &str) -> Result<(Node, Vec<Scope>), String> {
    let mut reader = Reader::from_str(source);
    let mut scopes: Vec<Scope> = vec![Scope {
        parent: None,
        declarations: HashMap::new(),
    }];
    let mut stack: Vec<Node> = Vec::new();
    let mut root = None;
    loop {
        let event = reader
            .read_event()
            .map_err(|error| format!("XSD error: {error}"))?;
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(element) | Event::Empty(element) => {
                if stack.len() >= MAX_SCHEMA_DEPTH {
                    return Err(format!(
                        "XSD error: the schema is nested more than {MAX_SCHEMA_DEPTH} levels deep"
                    ));
                }
                let qname = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                let mut attributes = Vec::new();
                for attribute in element.attributes().flatten() {
                    let key = String::from_utf8_lossy(attribute.key.as_ref()).into_owned();
                    let value = normalized_attribute_value(&attribute).unwrap_or_default();
                    attributes.push((key, value));
                }
                let parent_scope = stack.last().map_or(0, |node| node.scope);
                let declarations = attributes
                    .iter()
                    .filter_map(|(key, value)| {
                        if key == "xmlns" {
                            Some((String::new(), value.clone()))
                        } else {
                            key.strip_prefix("xmlns:")
                                .map(|prefix| (prefix.to_owned(), value.clone()))
                        }
                    })
                    .collect::<Vec<_>>();
                let scope = if declarations.is_empty() {
                    parent_scope
                } else {
                    scopes.push(Scope {
                        parent: Some(parent_scope),
                        declarations: declarations.into_iter().collect(),
                    });
                    scopes.len() - 1
                };
                let (prefix, local) = match qname.split_once(':') {
                    Some((prefix, local)) => (prefix, local.to_owned()),
                    None => ("", qname.clone()),
                };
                let namespace = scope_lookup(&scopes, scope, prefix)
                    .filter(|namespace| !namespace.is_empty())
                    .cloned();
                let node = Node {
                    local,
                    namespace,
                    attributes,
                    children: Vec::new(),
                    scope,
                };
                stack.push(node);
                if empty {
                    close(&mut stack, &mut root);
                }
            }
            Event::End(_) => close(&mut stack, &mut root),
            Event::Text(text) => {
                if let Some(node) = stack.last_mut() {
                    let text = text.decode().map_err(|error| error.to_string())?;
                    node.children.push(Child::Text(text.into_owned()));
                }
            }
            Event::CData(data) => {
                if let Some(node) = stack.last_mut() {
                    let text = data.decode().map_err(|error| error.to_string())?;
                    node.children.push(Child::Text(text.into_owned()));
                }
            }
            Event::GeneralRef(reference) => {
                if let Some(node) = stack.last_mut() {
                    let resolved = match reference.resolve_char_ref() {
                        Ok(Some(character)) => character.to_string(),
                        _ => {
                            let name = reference.decode().map_err(|error| error.to_string())?;
                            match name.as_ref() {
                                "lt" => "<".to_owned(),
                                "gt" => ">".to_owned(),
                                "amp" => "&".to_owned(),
                                "apos" => "'".to_owned(),
                                "quot" => "\"".to_owned(),
                                other => format!("&{other};"),
                            }
                        }
                    };
                    node.children.push(Child::Text(resolved));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    while !stack.is_empty() {
        close(&mut stack, &mut root);
    }
    root.map(|root| (root, scopes))
        .ok_or_else(|| "empty XSD document".to_owned())
}

/// `xs:boolean` attribute of a schema component (`true` or `1`).
fn is_true(value: Option<String>) -> bool {
    matches!(value.as_deref().map(str::trim), Some("true" | "1"))
}

/// Value of an attribute after the attribute-value normalization of XML 1.0
/// (§2.11, §3.3.3): line breaks (`\r\n` counting as one) and tabs written
/// literally become spaces, character references are kept.
pub(crate) fn normalized_attribute_value(
    attribute: &quick_xml::events::attributes::Attribute<'_>,
) -> Option<String> {
    let raw = std::str::from_utf8(attribute.value.as_ref()).ok()?;
    let normalized = raw.replace("\r\n", " ").replace(['\r', '\n', '\t'], " ");
    quick_xml::escape::unescape(&normalized)
        .ok()
        .map(|value| value.into_owned())
}

fn close(stack: &mut Vec<Node>, root: &mut Option<Node>) {
    let Some(node) = stack.pop() else {
        return;
    };
    match stack.last_mut() {
        Some(parent) => parent.children.push(Child::Element(node)),
        None => {
            if root.is_none() {
                *root = Some(node);
            }
        }
    }
}

struct Context<'a> {
    scopes: &'a [Scope],
    target_namespace: Option<String>,
    element_form_qualified: bool,
    attribute_form_qualified: bool,
    /// `blockDefault` of the schema.
    block_default: Option<String>,
    identity_constraints: RefCell<Vec<XsdIdentityConstraint>>,
    problems: RefCell<Vec<String>>,
}

impl Context<'_> {
    fn qname(&self, node: &Node, value: &str) -> Option<XsdQName> {
        let value = value.trim();
        if value.is_empty() {
            return None;
        }
        let (prefix, local) = match value.split_once(':') {
            Some((prefix, local)) => (Some(prefix), local),
            None => (None, value),
        };
        let namespace = if prefix == Some("xml") {
            Some(XML_NAMESPACE.to_owned())
        } else {
            scope_lookup(self.scopes, node.scope, prefix.unwrap_or(""))
                .filter(|namespace| !namespace.is_empty())
                .cloned()
        };
        Some(XsdQName {
            prefix: prefix.map(str::to_owned),
            namespace,
            local: local.to_owned(),
        })
    }

    fn qname_attribute(&self, node: &Node, name: &str) -> Option<XsdQName> {
        self.qname(node, &node.attribute(name)?)
    }

    fn qname_list(&self, node: &Node, name: &str) -> Vec<XsdQName> {
        node.attribute(name)
            .unwrap_or_default()
            .split_whitespace()
            .filter_map(|value| self.qname(node, value))
            .collect()
    }

    fn top_level(&self, schema: &Node, model: &mut XsdModel) {
        for child in schema.elements() {
            if child
                .namespace
                .as_deref()
                .is_some_and(|namespace| namespace != XSD_NAMESPACE)
            {
                continue;
            }
            match child.local.as_str() {
                "element" => model.elements.extend(self.element(child, true)),
                "attribute" => model.attributes.extend(self.attribute(child, true)),
                "complexType" => model.types.push(self.complex_type(child)),
                "simpleType" => model.types.push(self.simple_type(child)),
                "group" => {
                    if let Some(name) = child.attribute("name") {
                        model.groups.push(XsdGroupDef {
                            name,
                            namespace: self.target_namespace.clone(),
                            content: self.content_of(child),
                            documentation: documentation(child),
                        });
                    }
                }
                "attributeGroup" => {
                    if let Some(name) = child.attribute("name") {
                        let mut group = XsdAttributeGroupDef {
                            name,
                            namespace: self.target_namespace.clone(),
                            attributes: Vec::new(),
                            attribute_group_refs: Vec::new(),
                            any_attribute: None,
                            documentation: documentation(child),
                        };
                        for item in child.elements() {
                            match item.local.as_str() {
                                "attribute" => group.attributes.extend(self.attribute(item, false)),
                                "attributeGroup" => group
                                    .attribute_group_refs
                                    .extend(self.qname_attribute(item, "ref")),
                                "anyAttribute" => {
                                    group.any_attribute = Some(self.attribute_wildcard(item))
                                }
                                _ => {}
                            }
                        }
                        model.attribute_groups.push(group);
                    }
                }
                "redefine" | "override" => {
                    let (types, groups, attribute_groups) = (
                        model.types.len(),
                        model.groups.len(),
                        model.attribute_groups.len(),
                    );
                    self.top_level(child, model);
                    for definition in &model.types[types..] {
                        if let Some(name) = &definition.name {
                            model.redefined.push(("type", name.clone()));
                        }
                    }
                    for group in &model.groups[groups..] {
                        model.redefined.push(("group", group.name.clone()));
                    }
                    for group in &model.attribute_groups[attribute_groups..] {
                        model.redefined.push(("attributeGroup", group.name.clone()));
                    }
                }
                _ => {}
            }
        }
    }

    fn element(&self, node: &Node, global: bool) -> Option<XsdElementDecl> {
        let reference = self.qname_attribute(node, "ref");
        let name = node
            .attribute("name")
            .or_else(|| reference.as_ref().map(|reference| reference.local.clone()))?;
        let namespace = match &reference {
            Some(reference) => reference.namespace.clone(),
            None if global => self.target_namespace.clone(),
            None => self.form_namespace(node, self.element_form_qualified),
        };
        let (min_occurs, max_occurs) = occurs(node);
        let anonymous_type = node
            .xsd_children("complexType")
            .next()
            .map(|child| self.complex_type(child))
            .or_else(|| {
                node.xsd_children("simpleType")
                    .next()
                    .map(|child| self.simple_type(child))
            })
            .map(Box::new);
        Some(XsdElementDecl {
            name,
            reference,
            namespace,
            type_name: self.qname_attribute(node, "type"),
            anonymous_type,
            min_occurs,
            max_occurs,
            default: node.attribute("default"),
            fixed: node.attribute("fixed"),
            nillable: is_true(node.attribute("nillable")),
            is_abstract: is_true(node.attribute("abstract")),
            blocks_substitution: self.blocks(node, "substitution"),
            blocks_extension: self.blocks(node, "extension"),
            blocks_restriction: self.blocks(node, "restriction"),
            substitution_groups: self.qname_list(node, "substitutionGroup"),
            global,
            documentation: documentation(node),
            identity_constraints: self.identity_constraints(node),
        })
    }

    /// Identity constraints of an element declaration; invalid ones are
    /// reported in `problems` and left out.
    fn identity_constraints(&self, node: &Node) -> Vec<XsdIdentityConstraint> {
        let mut constraints = Vec::new();
        let mut after_constraint = false;
        for child in node.elements() {
            let kind = match child.local.as_str() {
                "unique" => XsdIdentityKind::Unique,
                "key" => XsdIdentityKind::Key,
                "keyref" => XsdIdentityKind::KeyRef,
                "simpleType" | "complexType" if is_xsd(child, &child.local) => {
                    if after_constraint {
                        self.problems.borrow_mut().push(format!(
                            "in the declaration of '{}', xs:{} must come before the identity constraints",
                            node.attribute("name").unwrap_or_default(),
                            child.local
                        ));
                    }
                    continue;
                }
                _ => continue,
            };
            if !is_xsd(child, &child.local) {
                continue;
            }
            after_constraint = true;
            match self.identity_constraint(child, kind) {
                Ok(constraint) => {
                    self.identity_constraints
                        .borrow_mut()
                        .push(constraint.clone());
                    constraints.push(constraint);
                }
                Err(error) => self.problems.borrow_mut().push(format!(
                    "xs:{} '{}': {error}",
                    kind.element_name(),
                    child.attribute("name").unwrap_or_default()
                )),
            }
        }
        constraints
    }

    fn identity_constraint(
        &self,
        node: &Node,
        kind: XsdIdentityKind,
    ) -> Result<XsdIdentityConstraint, String> {
        let name = node
            .attribute("name")
            .ok_or_else(|| "the name attribute is missing".to_owned())?
            .trim()
            .to_owned();
        if !is_ncname(&name) {
            return Err(format!("'{name}' is not a valid name (NCName)"));
        }
        let refer = match kind {
            XsdIdentityKind::KeyRef => Some(
                self.qname_attribute(node, "refer")
                    .ok_or_else(|| "the refer attribute is missing".to_owned())?,
            ),
            _ => None,
        };
        let mut annotation = false;
        let mut selector = None;
        let mut fields = Vec::new();
        for child in node.elements() {
            let local = child.local.as_str();
            if !is_xsd(child, local) {
                return Err(format!("unexpected element <{local}>"));
            }
            match local {
                "annotation" if !annotation && selector.is_none() => annotation = true,
                "selector" if selector.is_none() => selector = Some(self.xpath(child, false)?),
                "field" if selector.is_some() => fields.push(self.xpath(child, true)?),
                _ => return Err(format!("unexpected xs:{local}")),
            }
        }
        let selector = selector.ok_or_else(|| "xs:selector is missing".to_owned())?;
        if fields.is_empty() {
            return Err("xs:field is missing".to_owned());
        }
        Ok(XsdIdentityConstraint {
            kind,
            name,
            namespace: self.target_namespace.clone(),
            refer,
            selector,
            fields,
        })
    }

    /// `xpath` of an `xs:selector` or `xs:field`.
    fn xpath(&self, node: &Node, field: bool) -> Result<crate::identity::XsdXPath, String> {
        let mut annotation = false;
        for child in node.elements() {
            if is_xsd(child, "annotation") && !annotation {
                annotation = true;
            } else {
                return Err(format!("unexpected <{}> in xs:{}", child.local, node.local));
            }
        }
        let expression = node
            .attribute("xpath")
            .ok_or_else(|| format!("the xpath attribute of xs:{} is missing", node.local))?;
        let scopes = self.scopes;
        let resolve = |prefix: &str| {
            if prefix == "xml" {
                return Some(XML_NAMESPACE.to_owned());
            }
            scope_lookup(scopes, node.scope, prefix)
                .filter(|namespace| !namespace.is_empty())
                .cloned()
        };
        parse_xpath(&expression, field, &resolve)
            .map_err(|error| format!("invalid xpath '{expression}': {error}"))
    }

    fn attribute(&self, node: &Node, global: bool) -> Option<XsdAttributeDecl> {
        let reference = self.qname_attribute(node, "ref");
        let name = node
            .attribute("name")
            .or_else(|| reference.as_ref().map(|reference| reference.local.clone()))?;
        let namespace = match &reference {
            Some(reference) => reference.namespace.clone(),
            None if global => self.target_namespace.clone(),
            None => self.form_namespace(node, self.attribute_form_qualified),
        };
        Some(XsdAttributeDecl {
            name,
            reference,
            namespace,
            type_name: self.qname_attribute(node, "type"),
            anonymous_type: node
                .xsd_children("simpleType")
                .next()
                .map(|child| Box::new(self.simple_type(child))),
            usage: match node.attribute("use").as_deref() {
                Some("required") => XsdUse::Required,
                Some("prohibited") => XsdUse::Prohibited,
                _ => XsdUse::Optional,
            },
            default: node.attribute("default"),
            fixed: node.attribute("fixed"),
            global,
            documentation: documentation(node),
        })
    }

    fn form_namespace(&self, node: &Node, qualified_by_default: bool) -> Option<String> {
        let qualified = match node.attribute("form").as_deref() {
            Some("qualified") => true,
            Some("unqualified") => false,
            _ => qualified_by_default,
        };
        qualified.then(|| self.target_namespace.clone()).flatten()
    }

    /// Whether the `block` of `node` (else the schema's `blockDefault`)
    /// contains `kind` or `#all`.
    fn blocks(&self, node: &Node, kind: &str) -> bool {
        node.attribute("block")
            .or_else(|| self.block_default.clone())
            .unwrap_or_default()
            .split_whitespace()
            .any(|token| token == "#all" || token == kind)
    }

    fn complex_type(&self, node: &Node) -> XsdTypeDef {
        let mut definition = XsdTypeDef {
            name: node.attribute("name"),
            namespace: self.target_namespace.clone(),
            complex: true,
            is_abstract: is_true(node.attribute("abstract")),
            blocks_extension: self.blocks(node, "extension"),
            blocks_restriction: self.blocks(node, "restriction"),
            mixed: is_true(node.attribute("mixed")),
            documentation: documentation(node),
            ..XsdTypeDef::default()
        };
        for child in node.elements() {
            match child.local.as_str() {
                "simpleContent" | "complexContent" => {
                    definition.simple_content = child.local == "simpleContent";
                    if is_true(child.attribute("mixed")) {
                        definition.mixed = true;
                    }
                    for derivation in child.elements() {
                        let kind = match derivation.local.as_str() {
                            "restriction" => XsdDerivation::Restriction,
                            "extension" => XsdDerivation::Extension,
                            _ => continue,
                        };
                        definition.derivation = Some(kind);
                        definition.base = self.qname_attribute(derivation, "base");
                        self.type_body(derivation, &mut definition);
                        self.facets(derivation, &mut definition);
                    }
                }
                _ => {}
            }
        }
        self.type_body(node, &mut definition);
        definition
    }

    /// Content model and attributes carried directly by `node`.
    fn type_body(&self, node: &Node, definition: &mut XsdTypeDef) {
        for child in node.elements() {
            match child.local.as_str() {
                "sequence" | "choice" | "all" | "group" if definition.content.is_none() => {
                    definition.content = self.particle(child);
                }
                "attribute" => definition.attributes.extend(self.attribute(child, false)),
                "attributeGroup" => definition
                    .attribute_group_refs
                    .extend(self.qname_attribute(child, "ref")),
                "anyAttribute" => definition.any_attribute = Some(self.attribute_wildcard(child)),
                _ => {}
            }
        }
    }

    fn simple_type(&self, node: &Node) -> XsdTypeDef {
        let mut definition = XsdTypeDef {
            name: node.attribute("name"),
            namespace: self.target_namespace.clone(),
            documentation: documentation(node),
            ..XsdTypeDef::default()
        };
        for child in node.elements() {
            match child.local.as_str() {
                "restriction" => {
                    definition.derivation = Some(XsdDerivation::Restriction);
                    definition.base = self.qname_attribute(child, "base");
                    self.facets(child, &mut definition);
                }
                "list" => {
                    definition.derivation = Some(XsdDerivation::List);
                    definition.item_type = self.qname_attribute(child, "itemType");
                    definition.inline_types.extend(
                        child
                            .xsd_children("simpleType")
                            .map(|item| self.simple_type(item)),
                    );
                }
                "union" => {
                    definition.derivation = Some(XsdDerivation::Union);
                    definition.member_types = self.qname_list(child, "memberTypes");
                    definition.inline_types.extend(
                        child
                            .xsd_children("simpleType")
                            .map(|member| self.simple_type(member)),
                    );
                }
                _ => {}
            }
        }
        definition
    }

    fn facets(&self, restriction: &Node, definition: &mut XsdTypeDef) {
        for facet in restriction.elements() {
            if facet.local == "simpleType" {
                definition.inline_types.push(self.simple_type(facet));
                continue;
            }
            let Some(value) = facet.attribute("value") else {
                continue;
            };
            let facets = &mut definition.facets;
            match facet.local.as_str() {
                "enumeration" => facets.enumerations.push(XsdEnumeration {
                    value,
                    documentation: documentation(facet),
                }),
                "pattern" => facets.patterns.push(value),
                "length" => facets.length = Some(value),
                "minLength" => facets.min_length = Some(value),
                "maxLength" => facets.max_length = Some(value),
                "minInclusive" => facets.min_inclusive = Some(value),
                "maxInclusive" => facets.max_inclusive = Some(value),
                "minExclusive" => facets.min_exclusive = Some(value),
                "maxExclusive" => facets.max_exclusive = Some(value),
                "totalDigits" => facets.total_digits = Some(value),
                "fractionDigits" => facets.fraction_digits = Some(value),
                "whiteSpace" => facets.white_space = Some(value),
                _ => {}
            }
        }
    }

    fn attribute_wildcard(&self, node: &Node) -> XsdWildcard {
        XsdWildcard {
            has_exclusions: node.attribute("notNamespace").is_some()
                || node.attribute("notQName").is_some(),
            process_contents: match node.attribute("processContents").as_deref().map(str::trim) {
                Some("skip") => XsdProcessContents::Skip,
                Some("lax") => XsdProcessContents::Lax,
                _ => XsdProcessContents::Strict,
            },
            namespaces: self.wildcard_namespaces(node),
            min_occurs: 0,
            max_occurs: Some(1),
        }
    }

    fn wildcard_namespaces(&self, node: &Node) -> XsdWildcardNamespaces {
        let Some(value) = node.attribute("namespace") else {
            return XsdWildcardNamespaces::Any;
        };
        let tokens = value.split_whitespace().collect::<Vec<_>>();
        match tokens.as_slice() {
            [] | ["##any"] => XsdWildcardNamespaces::Any,
            ["##other"] => XsdWildcardNamespaces::Other(self.target_namespace.clone()),
            _ => XsdWildcardNamespaces::Set(
                tokens
                    .into_iter()
                    .map(|token| match token {
                        "##local" => None,
                        "##targetNamespace" => self.target_namespace.clone(),
                        namespace => Some(namespace.to_owned()),
                    })
                    .collect(),
            ),
        }
    }

    /// First model group (`sequence`, `choice`, `all`) of `node`.
    fn content_of(&self, node: &Node) -> Option<XsdParticle> {
        node.elements()
            .filter(|child| matches!(child.local.as_str(), "sequence" | "choice" | "all"))
            .find_map(|child| self.particle(child))
    }

    fn particle(&self, node: &Node) -> Option<XsdParticle> {
        let compositor = match node.local.as_str() {
            "sequence" => XsdCompositor::Sequence,
            "choice" => XsdCompositor::Choice,
            "all" => XsdCompositor::All,
            "element" => {
                return self
                    .element(node, false)
                    .map(|declaration| XsdParticle::Element(Box::new(declaration)));
            }
            "group" => {
                let (min_occurs, max_occurs) = occurs(node);
                return self
                    .qname_attribute(node, "ref")
                    .map(|name| XsdParticle::GroupRef {
                        name,
                        min_occurs,
                        max_occurs,
                    });
            }
            "any" => {
                let (min_occurs, max_occurs) = occurs(node);
                return Some(XsdParticle::Any(XsdWildcard {
                    has_exclusions: node.attribute("notNamespace").is_some()
                        || node.attribute("notQName").is_some(),
                    process_contents: match node
                        .attribute("processContents")
                        .as_deref()
                        .map(str::trim)
                    {
                        Some("skip") => XsdProcessContents::Skip,
                        Some("lax") => XsdProcessContents::Lax,
                        _ => XsdProcessContents::Strict,
                    },
                    namespaces: self.wildcard_namespaces(node),
                    min_occurs,
                    max_occurs,
                }));
            }
            _ => return None,
        };
        let (min_occurs, max_occurs) = occurs(node);
        Some(XsdParticle::Group {
            compositor,
            min_occurs,
            max_occurs,
            particles: node
                .elements()
                .filter_map(|child| self.particle(child))
                .collect(),
        })
    }
}

/// `minOccurs` and `maxOccurs` of a particle (`None` is unbounded).
fn occurs(node: &Node) -> (usize, Option<usize>) {
    let min = node
        .attribute("minOccurs")
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(1);
    let max = match node.attribute("maxOccurs").as_deref().map(str::trim) {
        Some("unbounded") => None,
        Some(value) => Some(value.parse().unwrap_or(1)),
        None => Some(1),
    };
    (min, max)
}

/// XHTML layout blocks separated by a paragraph in the text.
const BLOCK_ELEMENTS: &[&str] = &[
    "p",
    "div",
    "br",
    "li",
    "ul",
    "ol",
    "pre",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "table",
    "tr",
    "dl",
    "dt",
    "dd",
    "blockquote",
];

/// `xs:annotation/xs:documentation` documentation carried directly by
/// `node`, normalized.
fn documentation(node: &Node) -> Option<String> {
    let blocks = node
        .xsd_children("annotation")
        .flat_map(|annotation| annotation.xsd_children("documentation"))
        .filter_map(|documentation| {
            let mut text = String::new();
            collect_text(documentation, &mut text);
            let text = normalize_documentation(&text);
            (!text.is_empty()).then(|| (documentation.attribute("xml:lang"), text))
        })
        .collect::<Vec<_>>();
    let preferred = blocks
        .iter()
        .filter(|(lang, _)| {
            lang.as_deref().is_none_or(|lang| {
                let lang = lang.trim().to_ascii_lowercase();
                lang.is_empty() || lang == "en" || lang.starts_with("en-")
            })
        })
        .map(|(_, text)| text.as_str())
        .collect::<Vec<_>>();
    let selected = if preferred.is_empty() {
        blocks.iter().map(|(_, text)| text.as_str()).collect()
    } else {
        preferred
    };
    (!selected.is_empty()).then(|| selected.join("\n\n"))
}

fn collect_text(node: &Node, text: &mut String) {
    for child in &node.children {
        match child {
            Child::Text(value) => text.push_str(value),
            Child::Element(element) => {
                let block = BLOCK_ELEMENTS.contains(&element.local.to_ascii_lowercase().as_str());
                if block {
                    text.push_str("\n\n");
                }
                collect_text(element, text);
                if block {
                    text.push_str("\n\n");
                }
            }
        }
    }
}

/// Groups consecutive non-empty lines into paragraphs separated by a blank
/// line, with normalized whitespace.
pub fn normalize_documentation(text: &str) -> String {
    let mut paragraphs = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for line in text.lines() {
        let words = line.split_whitespace().collect::<Vec<_>>();
        if words.is_empty() {
            if !current.is_empty() {
                paragraphs.push(current.join(" "));
                current.clear();
            }
        } else {
            current.push(words.join(" "));
        }
    }
    if !current.is_empty() {
        paragraphs.push(current.join(" "));
    }
    paragraphs.join("\n\n")
}

/// Model element and index of the schema declaring it.
#[derive(Debug, PartialEq, Eq)]
pub struct Located<'a, T> {
    pub schema: usize,
    pub item: &'a T,
}

impl<T> Clone for Located<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Located<'_, T> {}

/// Resolved type: definition (named or anonymous) or built-in type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XsdTypeRef<'a> {
    pub schema: usize,
    /// Type name as referenced (`None` for an anonymous type).
    pub name: Option<&'a XsdQName>,
    /// Definition (`None` for a built-in or missing type).
    pub definition: Option<&'a XsdTypeDef>,
}

/// Element declaration resolved in its context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedElement<'a> {
    /// Particle found in the content model (possibly a `ref`).
    pub particle: Located<'a, XsdElementDecl>,
    /// Effective declaration (global declaration for a `ref`).
    pub declaration: Located<'a, XsdElementDecl>,
    /// Effective type (`xsi:type` included).
    pub element_type: Option<XsdTypeRef<'a>>,
    /// `xsi:type` applied to the instance.
    pub xsi_type: Option<&'a XsdTypeDef>,
    /// The element, or one of its ancestors, is not declared by the content
    /// model of its parent but matched by a `processContents="skip"`
    /// wildcard: it must not be validated.
    pub skipped: bool,
}

/// Resolved attribute declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedAttribute<'a> {
    /// Local usage (carries `use`), possibly a `ref`.
    pub usage: Located<'a, XsdAttributeDecl>,
    /// Effective declaration (global declaration for a `ref`).
    pub declaration: Located<'a, XsdAttributeDecl>,
}

/// Problem of the `xsi:type` of an instance element (or of its absence).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XsiTypeProblem {
    /// `xsi:type` names a type no loaded schema defines.
    Unknown,
    /// The named type is not derived from the declared type.
    NotDerived,
    /// A derivation step is blocked by `block` of the element declaration or
    /// of a type ("extension" or "restriction").
    Blocked(&'static str),
    /// The type used by the instance is abstract.
    Abstract,
}

/// Step of the path of an instance element, from the root to the element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsdInstanceStep {
    pub namespace: Option<String>,
    pub local: String,
    /// `xsi:type` (namespace, local name) carried by the element.
    pub xsi_type: Option<(Option<String>, String)>,
}

/// Summary of a simple type: facets accumulated along the restrictions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XsdSimpleTypeInfo {
    pub facets: XsdFacets,
    /// Original built-in type (`xs:string`...).
    pub builtin: Option<XsdQName>,
    pub item_type: Option<String>,
    pub member_types: Vec<String>,
}

/// Set of schemas loaded together (includes and imports included).
#[derive(Debug, Clone, Default)]
pub struct XsdModelSet {
    models: Vec<Arc<XsdModel>>,
    /// Members of the substitution groups by head (namespace, local name),
    /// as `(schema, index of the global element)`; built on first use.
    substitutions: std::sync::OnceLock<SubstitutionIndex>,
}

type SubstitutionIndex = HashMap<(Option<String>, String), Vec<(usize, usize)>>;

/// Namespaces are compared strictly, then a component without namespace
/// matches any namespace as a fallback ("chameleon" schemas, which adopt the
/// namespace of the schema including them). A component in another
/// namespace never matches.
fn namespace_matches(strict: bool, expected: Option<&str>, actual: Option<&str>) -> bool {
    expected == actual || (!strict && actual.is_none())
}

impl XsdModelSet {
    pub fn new(models: Vec<Arc<XsdModel>>) -> Self {
        Self {
            models,
            substitutions: std::sync::OnceLock::new(),
        }
    }

    /// Non-abstract members of the substitution group headed by `head`,
    /// transitively (a member can head another group). The head itself is
    /// not included.
    pub(crate) fn substitution_members<'a>(
        &'a self,
        head: Located<'a, XsdElementDecl>,
    ) -> Vec<Located<'a, XsdElementDecl>> {
        let index = self.substitutions.get_or_init(|| {
            let mut index = SubstitutionIndex::new();
            for (schema, model) in self.models.iter().enumerate() {
                for (position, element) in model.elements.iter().enumerate() {
                    for group in &element.substitution_groups {
                        index
                            .entry((group.namespace.clone(), group.local.clone()))
                            .or_default()
                            .push((schema, position));
                    }
                }
            }
            index
        });
        let mut members = Vec::new();
        let mut seen = std::collections::HashSet::new();
        // A head that blocks substitution has no usable member.
        if head.item.blocks_substitution {
            return members;
        }
        let mut pending = vec![(head.item.namespace.clone(), head.item.name.clone())];
        while let Some(key) = pending.pop() {
            for &(schema, position) in index.get(&key).map(Vec::as_slice).unwrap_or_default() {
                if !seen.insert((schema, position)) {
                    continue;
                }
                let item = &self.models[schema].elements[position];
                if !item.blocks_substitution {
                    pending.push((item.namespace.clone(), item.name.clone()));
                }
                if !item.is_abstract {
                    members.push(Located { schema, item });
                }
            }
        }
        members
    }

    pub fn models(&self) -> &[Arc<XsdModel>] {
        &self.models
    }

    fn find<'a, T>(
        &'a self,
        namespace: Option<&str>,
        local: &str,
        items: impl Fn(&'a XsdModel) -> &'a [T],
        name_of: impl Fn(&T) -> (&str, Option<&str>),
    ) -> Option<Located<'a, T>> {
        self.find_where(namespace, local, items, name_of, |_| true)
    }

    fn find_where<'a, T>(
        &'a self,
        namespace: Option<&str>,
        local: &str,
        items: impl Fn(&'a XsdModel) -> &'a [T],
        name_of: impl Fn(&T) -> (&str, Option<&str>),
        accept: impl Fn(&T) -> bool,
    ) -> Option<Located<'a, T>> {
        [true, false].into_iter().find_map(|strict| {
            self.models.iter().enumerate().find_map(|(schema, model)| {
                items(model)
                    .iter()
                    .find(|item| {
                        let (name, item_namespace) = name_of(item);
                        name == local
                            && namespace_matches(strict, namespace, item_namespace)
                            && accept(item)
                    })
                    .map(|item| Located { schema, item })
            })
        })
    }

    /// The definition `local` a redefinition (`xs:redefine`) replaces: the
    /// first one in a schema after `after` (a document precedes the ones it
    /// includes), else any other than `exclude`.
    fn find_replaced<'a, T>(
        &'a self,
        namespace: Option<&str>,
        local: &str,
        after: usize,
        exclude: &T,
        items: impl Fn(&'a XsdModel) -> &'a [T],
        name_of: impl Fn(&T) -> (&str, Option<&str>),
    ) -> Option<Located<'a, T>> {
        let matching = |schema: usize, model: &'a XsdModel| {
            items(model)
                .iter()
                .find(|item| {
                    let (name, item_namespace) = name_of(item);
                    name == local
                        && namespace_matches(false, namespace, item_namespace)
                        && !std::ptr::eq(*item, exclude)
                })
                .map(|item| Located { schema, item })
        };
        self.models
            .iter()
            .enumerate()
            .skip(after + 1)
            .find_map(|(schema, model)| matching(schema, model))
            .or_else(|| {
                self.models
                    .iter()
                    .enumerate()
                    .find_map(|(schema, model)| matching(schema, model))
            })
    }

    /// The type a redefined type named `local` extends or restricts.
    pub(crate) fn replaced_type(
        &self,
        namespace: Option<&str>,
        local: &str,
        schema: usize,
        exclude: &XsdTypeDef,
    ) -> Option<Located<'_, XsdTypeDef>> {
        self.find_replaced(
            namespace,
            local,
            schema,
            exclude,
            |model| &model.types,
            |item| {
                (
                    item.name.as_deref().unwrap_or_default(),
                    item.namespace.as_deref(),
                )
            },
        )
    }

    /// The model group a redefined group refers to (see
    /// [`Self::replaced_type`]).
    pub(crate) fn replaced_group(
        &self,
        namespace: Option<&str>,
        local: &str,
        schema: usize,
        exclude: &XsdGroupDef,
    ) -> Option<Located<'_, XsdGroupDef>> {
        self.find_replaced(
            namespace,
            local,
            schema,
            exclude,
            |model| &model.groups,
            |item| (item.name.as_str(), item.namespace.as_deref()),
        )
    }

    /// The attribute group a redefined attribute group refers to (see
    /// [`Self::replaced_type`]).
    pub(crate) fn replaced_attribute_group(
        &self,
        namespace: Option<&str>,
        local: &str,
        schema: usize,
        exclude: &XsdAttributeGroupDef,
    ) -> Option<Located<'_, XsdAttributeGroupDef>> {
        self.find_replaced(
            namespace,
            local,
            schema,
            exclude,
            |model| &model.attribute_groups,
            |item| (item.name.as_str(), item.namespace.as_deref()),
        )
    }

    /// Global element declaration.
    pub fn global_element(
        &self,
        namespace: Option<&str>,
        local: &str,
    ) -> Option<Located<'_, XsdElementDecl>> {
        self.find(
            namespace,
            local,
            |model| &model.elements,
            |item| (item.name.as_str(), item.namespace.as_deref()),
        )
    }

    /// Global attribute declaration.
    pub fn global_attribute(
        &self,
        namespace: Option<&str>,
        local: &str,
    ) -> Option<Located<'_, XsdAttributeDecl>> {
        self.find(
            namespace,
            local,
            |model| &model.attributes,
            |item| (item.name.as_str(), item.namespace.as_deref()),
        )
    }

    /// Named global type.
    pub fn global_type(
        &self,
        namespace: Option<&str>,
        local: &str,
    ) -> Option<Located<'_, XsdTypeDef>> {
        self.find(
            namespace,
            local,
            |model| &model.types,
            |item| {
                (
                    item.name.as_deref().unwrap_or_default(),
                    item.namespace.as_deref(),
                )
            },
        )
    }

    /// Named model group.
    pub fn group(&self, namespace: Option<&str>, local: &str) -> Option<Located<'_, XsdGroupDef>> {
        self.find(
            namespace,
            local,
            |model| &model.groups,
            |item| (item.name.as_str(), item.namespace.as_deref()),
        )
    }

    /// Named attribute group.
    pub fn attribute_group(
        &self,
        namespace: Option<&str>,
        local: &str,
    ) -> Option<Located<'_, XsdAttributeGroupDef>> {
        self.find(
            namespace,
            local,
            |model| &model.attribute_groups,
            |item| (item.name.as_str(), item.namespace.as_deref()),
        )
    }

    /// Resolves a type name: built-in type or global type.
    pub fn resolve_type<'a>(&'a self, schema: usize, name: &'a XsdQName) -> XsdTypeRef<'a> {
        if name.is_builtin() {
            return XsdTypeRef {
                schema,
                name: Some(name),
                definition: None,
            };
        }
        match self.global_type(name.namespace.as_deref(), &name.local) {
            Some(found) => XsdTypeRef {
                schema: found.schema,
                name: Some(name),
                definition: Some(found.item),
            },
            None => XsdTypeRef {
                schema,
                name: Some(name),
                definition: None,
            },
        }
    }

    /// Effective declaration of a particle: follows a `ref` to the global
    /// declaration.
    pub fn element_target<'a>(
        &'a self,
        particle: Located<'a, XsdElementDecl>,
    ) -> Located<'a, XsdElementDecl> {
        particle
            .item
            .reference
            .as_ref()
            .and_then(|reference| {
                self.global_element(reference.namespace.as_deref(), &reference.local)
            })
            .unwrap_or(particle)
    }

    /// Type of an element declaration (`type`, anonymous type or the type of
    /// the substitution group head).
    pub fn element_type<'a>(
        &'a self,
        declaration: Located<'a, XsdElementDecl>,
    ) -> Option<XsdTypeRef<'a>> {
        self.element_type_at_depth(declaration, 0)
    }

    fn element_type_at_depth<'a>(
        &'a self,
        declaration: Located<'a, XsdElementDecl>,
        depth: usize,
    ) -> Option<XsdTypeRef<'a>> {
        let item = declaration.item;
        if let Some(name) = &item.type_name {
            return Some(self.resolve_type(declaration.schema, name));
        }
        if let Some(definition) = &item.anonymous_type {
            return Some(XsdTypeRef {
                schema: declaration.schema,
                name: None,
                definition: Some(definition),
            });
        }
        if depth < MAX_DEPTH
            && let Some(head) = item.substitution_groups.first()
            && let Some(head) = self.global_element(head.namespace.as_deref(), &head.local)
        {
            return self.element_type_at_depth(head, depth + 1);
        }
        None
    }

    /// Type of an attribute (`type` or anonymous simple type).
    pub fn attribute_type<'a>(
        &'a self,
        declaration: Located<'a, XsdAttributeDecl>,
    ) -> Option<XsdTypeRef<'a>> {
        let item = declaration.item;
        if let Some(name) = &item.type_name {
            return Some(self.resolve_type(declaration.schema, name));
        }
        item.anonymous_type.as_deref().map(|definition| XsdTypeRef {
            schema: declaration.schema,
            name: None,
            definition: Some(definition),
        })
    }

    /// Base type of a derived definition.
    pub fn base_type<'a>(&'a self, reference: XsdTypeRef<'a>) -> Option<XsdTypeRef<'a>> {
        let definition = reference.definition?;
        if let Some(base) = &definition.base {
            let resolved = self.resolve_type(reference.schema, base);
            // A redefinition derives from the definition it replaces.
            if definition.name.as_deref() == Some(base.local.as_str())
                && definition.namespace == base.namespace
                && let Some(original) = self.replaced_type(
                    base.namespace.as_deref(),
                    &base.local,
                    reference.schema,
                    definition,
                )
            {
                return Some(XsdTypeRef {
                    schema: original.schema,
                    name: Some(base),
                    definition: Some(original.item),
                });
            }
            return Some(resolved);
        }
        // Restriction of an anonymous simple type (`<xs:restriction><xs:simpleType>`).
        (definition.derivation == Some(XsdDerivation::Restriction))
            .then(|| definition.inline_types.first())
            .flatten()
            .map(|inline| XsdTypeRef {
                schema: reference.schema,
                name: None,
                definition: Some(inline),
            })
    }

    /// Resolves the declaration of an instance element from the path of its
    /// ancestors (root first): local declarations of content models,
    /// references, groups, extensions, substitution groups and
    /// `xsi:type`. Falls back to global declarations (`xs:any`, unknown
    /// content).
    pub fn resolve_element_path(&self, path: &[XsdInstanceStep]) -> Option<ResolvedElement<'_>> {
        let (first, rest) = path.split_first()?;
        let mut current = self.resolve_root_element(first)?;
        for step in rest {
            current = self.resolve_child_element(&current, step)?;
        }
        Some(current)
    }

    /// Global element declaration designated by the root element `step`.
    pub fn resolve_root_element(&self, step: &XsdInstanceStep) -> Option<ResolvedElement<'_>> {
        let particle = self.global_element(step.namespace.as_deref(), &step.local)?;
        Some(self.resolved(particle, step))
    }

    /// Declaration of the child `step` of `parent`: one step of
    /// [`Self::resolve_element_path`], for callers walking a document that
    /// keep the resolution of each open element (linear in the depth).
    pub fn resolve_child_element<'a>(
        &'a self,
        parent: &ResolvedElement<'a>,
        step: &XsdInstanceStep,
    ) -> Option<ResolvedElement<'a>> {
        let parent_type = parent.element_type;
        let child = parent_type.and_then(|parent_type| self.find_child(parent_type, step));
        let skipped = parent.skipped
            || (child.is_none()
                && parent_type.is_some_and(|parent| self.has_skip_wildcard(parent)));
        let particle =
            child.or_else(|| self.global_element(step.namespace.as_deref(), &step.local))?;
        let mut current = self.resolved(particle, step);
        current.skipped = skipped;
        Some(current)
    }

    fn resolved<'a>(
        &'a self,
        particle: Located<'a, XsdElementDecl>,
        step: &XsdInstanceStep,
    ) -> ResolvedElement<'a> {
        let declaration = self.element_target(particle);
        let xsi_type = step
            .xsi_type
            .as_ref()
            .and_then(|(namespace, local)| self.global_type(namespace.as_deref(), local));
        let element_type = match xsi_type {
            Some(found) => Some(XsdTypeRef {
                schema: found.schema,
                name: None,
                definition: Some(found.item),
            }),
            None => self.element_type(declaration),
        };
        ResolvedElement {
            particle,
            declaration,
            element_type,
            xsi_type: xsi_type.map(|found| found.item),
            skipped: false,
        }
    }

    /// Problem of the type of an instance element: abstract declared type
    /// without `xsi:type`, unknown `xsi:type`, type not derived from the
    /// declared one, blocked derivation, abstract `xsi:type`. `None` when the
    /// type is fine or cannot be judged (an unresolved or built-in side).
    pub fn xsi_type_problem(
        &self,
        resolved: &ResolvedElement<'_>,
        xsi_type: Option<&(Option<String>, String)>,
    ) -> Option<XsiTypeProblem> {
        let declared = self.element_type(resolved.declaration);
        let Some((namespace, local)) = xsi_type else {
            return declared
                .and_then(|declared| declared.definition)
                .filter(|definition| definition.is_abstract)
                .map(|_| XsiTypeProblem::Abstract);
        };
        let Some(used) = resolved.xsi_type else {
            let loaded = self
                .models
                .iter()
                .any(|model| model.target_namespace == *namespace);
            let builtin = namespace.as_deref() == Some(XSD_NAMESPACE);
            return (loaded && !builtin).then_some(XsiTypeProblem::Unknown);
        };
        if used.is_abstract {
            return Some(XsiTypeProblem::Abstract);
        }
        let declared = declared?;
        let declared_definition = declared.definition?;
        // Simple types also accept the members of a union (and their
        // restrictions): only complex types are compared.
        if !declared_definition.complex || std::ptr::eq(declared_definition, used) {
            return None;
        }
        let element = resolved.declaration.item;
        let mut current =
            self.global_type(namespace.as_deref(), local)
                .map(|found| XsdTypeRef {
                    schema: found.schema,
                    name: None,
                    definition: Some(found.item),
                })?;
        for _ in 0..MAX_DEPTH {
            let definition = current.definition?;
            let Some(base) = self.base_type(current) else {
                // The chain ends above the declared type.
                return Some(XsiTypeProblem::NotDerived);
            };
            let method = match definition.derivation {
                Some(XsdDerivation::Extension) => Some("extension"),
                Some(XsdDerivation::Restriction) => Some("restriction"),
                _ => None,
            };
            // Only the `block` of the element and of the declared type count,
            // not that of the types in between (cos-ct-derived-ok).
            if let Some(method) = method {
                let blocked = match method {
                    "extension" => element.blocks_extension || declared_definition.blocks_extension,
                    _ => element.blocks_restriction || declared_definition.blocks_restriction,
                };
                if blocked {
                    return Some(XsiTypeProblem::Blocked(method));
                }
            }
            match base.definition {
                Some(definition) if std::ptr::eq(definition, declared_definition) => return None,
                Some(_) => current = base,
                // A built-in base other than the declared type: not derived.
                None => return Some(XsiTypeProblem::NotDerived),
            }
        }
        None
    }

    /// Whether the content model of a type has a `processContents="skip"`
    /// wildcard.
    fn has_skip_wildcard(&self, reference: XsdTypeRef<'_>) -> bool {
        let mut particles = Vec::new();
        self.content_particles(reference, 0, &mut particles);
        particles
            .iter()
            .any(|(_, particle)| self.particle_has_skip_wildcard(particle, 0))
    }

    fn particle_has_skip_wildcard(&self, particle: &XsdParticle, depth: usize) -> bool {
        if depth > MAX_DEPTH {
            return false;
        }
        match particle {
            XsdParticle::Any(wildcard) => wildcard.process_contents == XsdProcessContents::Skip,
            XsdParticle::Group { particles, .. } => particles
                .iter()
                .any(|particle| self.particle_has_skip_wildcard(particle, depth + 1)),
            XsdParticle::GroupRef { name, .. } => self
                .group(name.namespace.as_deref(), &name.local)
                .and_then(|group| group.item.content.as_ref())
                .is_some_and(|content| self.particle_has_skip_wildcard(content, depth + 1)),
            XsdParticle::Element(_) => false,
        }
    }

    /// Content particles of a type, including content inherited by extension.
    pub(crate) fn content_particles<'a>(
        &'a self,
        reference: XsdTypeRef<'a>,
        depth: usize,
        out: &mut Vec<(usize, &'a XsdParticle)>,
    ) {
        let Some(definition) = reference.definition else {
            return;
        };
        if depth < MAX_DEPTH
            && definition.derivation == Some(XsdDerivation::Extension)
            && let Some(base) = self.base_type(reference)
        {
            self.content_particles(base, depth + 1, out);
        }
        if let Some(content) = &definition.content {
            out.push((reference.schema, content));
        }
    }

    fn find_child<'a>(
        &'a self,
        parent: XsdTypeRef<'a>,
        step: &XsdInstanceStep,
    ) -> Option<Located<'a, XsdElementDecl>> {
        let mut particles = Vec::new();
        self.content_particles(parent, 0, &mut particles);
        // Declarations by name first, substitution group members last (the
        // member search scans every global declaration).
        [(true, false), (false, false), (true, true), (false, true)]
            .into_iter()
            .find_map(|(strict, substitutions)| {
                particles.iter().find_map(|&(schema, particle)| {
                    self.find_in_particle(schema, particle, step, strict, substitutions, 0)
                })
            })
    }

    fn find_in_particle<'a>(
        &'a self,
        schema: usize,
        particle: &'a XsdParticle,
        step: &XsdInstanceStep,
        strict: bool,
        substitutions: bool,
        depth: usize,
    ) -> Option<Located<'a, XsdElementDecl>> {
        if depth > MAX_DEPTH {
            return None;
        }
        match particle {
            XsdParticle::Element(declaration) => {
                let declaration = declaration.as_ref();
                if declaration.name == step.local
                    && namespace_matches(
                        strict,
                        step.namespace.as_deref(),
                        declaration.namespace.as_deref(),
                    )
                {
                    return Some(Located {
                        schema,
                        item: declaration,
                    });
                }
                if !substitutions {
                    return None;
                }
                // Member of a substitution group whose head is the particle.
                let head = self.element_target(Located {
                    schema,
                    item: declaration,
                });
                self.models
                    .iter()
                    .enumerate()
                    .flat_map(|(schema, model)| {
                        model
                            .elements
                            .iter()
                            .map(move |item| Located { schema, item })
                    })
                    .find(|member| {
                        member.item.name == step.local
                            && namespace_matches(
                                strict,
                                step.namespace.as_deref(),
                                member.item.namespace.as_deref(),
                            )
                            && member.item.substitution_groups.iter().any(|group| {
                                group.local == head.item.name
                                    && namespace_matches(
                                        strict,
                                        group.namespace.as_deref(),
                                        head.item.namespace.as_deref(),
                                    )
                            })
                    })
            }
            XsdParticle::Group { particles, .. } => particles.iter().find_map(|particle| {
                self.find_in_particle(schema, particle, step, strict, substitutions, depth + 1)
            }),
            XsdParticle::GroupRef { name, .. } => {
                let group = self.group(name.namespace.as_deref(), &name.local)?;
                self.find_in_particle(
                    group.schema,
                    group.item.content.as_ref()?,
                    step,
                    strict,
                    substitutions,
                    depth + 1,
                )
            }
            XsdParticle::Any(_) => None,
        }
    }

    /// Global element declarations of all schemas.
    pub fn global_elements(&self) -> impl Iterator<Item = Located<'_, XsdElementDecl>> {
        self.models.iter().enumerate().flat_map(|(schema, model)| {
            model
                .elements
                .iter()
                .map(move |item| Located { schema, item })
        })
    }

    /// Elements allowed in the content of a type, in model order: particles
    /// (groups and group references expanded, content inherited by
    /// extension included, `ref`s resolved) then non-abstract members of
    /// substitution groups. Without duplicate qualified names.
    pub fn child_elements<'a>(
        &'a self,
        parent: XsdTypeRef<'a>,
    ) -> Vec<Located<'a, XsdElementDecl>> {
        let mut particles = Vec::new();
        self.content_particles(parent, 0, &mut particles);
        let mut children = Vec::new();
        for (schema, particle) in particles {
            self.collect_child_elements(schema, particle, 0, &mut children);
        }
        let heads = children.clone();
        for head in heads {
            children.extend(self.global_elements().filter(|member| {
                !member.item.is_abstract
                    && member.item.substitution_groups.iter().any(|group| {
                        group.local == head.item.name
                            && (group.namespace.is_none() || group.namespace == head.item.namespace)
                    })
            }));
        }
        let mut seen = std::collections::HashSet::new();
        children.retain(|child| {
            !child.item.is_abstract
                && seen.insert((child.item.namespace.clone(), child.item.name.clone()))
        });
        children
    }

    fn collect_child_elements<'a>(
        &'a self,
        schema: usize,
        particle: &'a XsdParticle,
        depth: usize,
        out: &mut Vec<Located<'a, XsdElementDecl>>,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        match particle {
            XsdParticle::Element(declaration) => out.push(self.element_target(Located {
                schema,
                item: declaration.as_ref(),
            })),
            XsdParticle::Group { particles, .. } => {
                for particle in particles {
                    self.collect_child_elements(schema, particle, depth + 1, out);
                }
            }
            XsdParticle::GroupRef { name, .. } => {
                if let Some(group) = self.group(name.namespace.as_deref(), &name.local)
                    && let Some(content) = &group.item.content
                {
                    self.collect_child_elements(group.schema, content, depth + 1, out);
                }
            }
            XsdParticle::Any(_) => {}
        }
    }

    /// Attributes usable on a type: own attributes, attribute groups and
    /// attributes inherited from the base type.
    pub fn attribute_uses<'a>(
        &'a self,
        reference: XsdTypeRef<'a>,
    ) -> Vec<Located<'a, XsdAttributeDecl>> {
        let mut uses = Vec::new();
        self.collect_attribute_uses(reference, 0, &mut uses);
        uses
    }

    fn collect_attribute_uses<'a>(
        &'a self,
        reference: XsdTypeRef<'a>,
        depth: usize,
        uses: &mut Vec<Located<'a, XsdAttributeDecl>>,
    ) {
        let Some(definition) = reference.definition else {
            return;
        };
        if depth > MAX_DEPTH {
            return;
        }
        let schema = reference.schema;
        uses.extend(
            definition
                .attributes
                .iter()
                .map(|item| Located { schema, item }),
        );
        for group in &definition.attribute_group_refs {
            self.collect_group_attributes(group, depth + 1, uses, None);
        }
        if definition.complex
            && let Some(base) = self.base_type(reference)
        {
            let own = uses.len();
            let mut inherited = Vec::new();
            self.collect_attribute_uses(base, depth + 1, &mut inherited);
            // A redeclaration (restriction) hides the inherited attribute.
            inherited.retain(|candidate| {
                !uses[..own]
                    .iter()
                    .any(|existing| existing.item.name == candidate.item.name)
            });
            uses.extend(inherited);
        }
    }

    /// Effective declaration of an attribute use: the global declaration for
    /// a `ref`.
    pub(crate) fn attribute_declaration<'a>(
        &'a self,
        usage: Located<'a, XsdAttributeDecl>,
    ) -> Located<'a, XsdAttributeDecl> {
        usage
            .item
            .reference
            .as_ref()
            .and_then(|reference| {
                self.global_attribute(reference.namespace.as_deref(), &reference.local)
            })
            .unwrap_or(usage)
    }

    /// The attribute wildcards of a type: its own, those of its attribute
    /// groups and, for an extension, those of its base type.
    pub fn attribute_wildcards<'a>(&'a self, reference: XsdTypeRef<'a>) -> Vec<&'a XsdWildcard> {
        let mut found = Vec::new();
        let mut current = Some(reference);
        for _ in 0..MAX_DEPTH {
            let Some(reference) = current else { break };
            let Some(definition) = reference.definition else {
                break;
            };
            found.extend(definition.any_attribute.iter());
            let mut groups: Vec<&XsdQName> = definition.attribute_group_refs.iter().collect();
            let mut visited = 0;
            while let Some(name) = groups.pop() {
                visited += 1;
                if visited > 256 {
                    break;
                }
                if let Some(group) = self.attribute_group(name.namespace.as_deref(), &name.local) {
                    found.extend(group.item.any_attribute.iter());
                    groups.extend(group.item.attribute_group_refs.iter());
                }
            }
            current = if definition.derivation == Some(XsdDerivation::Extension) {
                self.base_type(reference)
            } else {
                None
            };
        }
        found
    }

    fn collect_group_attributes<'a>(
        &'a self,
        name: &XsdQName,
        depth: usize,
        uses: &mut Vec<Located<'a, XsdAttributeDecl>>,
        parent: Option<Located<'a, XsdAttributeGroupDef>>,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        let Some(mut group) = self.attribute_group(name.namespace.as_deref(), &name.local) else {
            return;
        };
        // A redefined attribute group refers to itself for the original.
        if let Some(parent) = parent
            && parent.item.name == name.local
            && parent.item.namespace == name.namespace
            && let Some(original) = self.replaced_attribute_group(
                name.namespace.as_deref(),
                &name.local,
                parent.schema,
                parent.item,
            )
        {
            group = original;
        }
        let schema = group.schema;
        uses.extend(
            group
                .item
                .attributes
                .iter()
                .map(|item| Located { schema, item }),
        );
        for nested in &group.item.attribute_group_refs {
            self.collect_group_attributes(nested, depth + 1, uses, Some(group));
        }
    }

    /// Resolves an instance attribute on a resolved element, then among the
    /// global attributes (qualified attributes such as `xml:lang`).
    pub fn resolve_attribute<'a>(
        &'a self,
        element: Option<&ResolvedElement<'a>>,
        namespace: Option<&str>,
        local: &str,
    ) -> Option<ResolvedAttribute<'a>> {
        let uses = element
            .and_then(|element| element.element_type)
            .map(|reference| self.attribute_uses(reference))
            .unwrap_or_default();
        let usage = [true, false]
            .into_iter()
            .find_map(|strict| {
                uses.iter().copied().find(|usage| {
                    usage.item.name == local
                        && namespace_matches(strict, namespace, usage.item.namespace.as_deref())
                })
            })
            .or_else(|| {
                namespace.and_then(|namespace| self.global_attribute(Some(namespace), local))
            })?;
        let declaration = usage
            .item
            .reference
            .as_ref()
            .and_then(|reference| {
                self.global_attribute(reference.namespace.as_deref(), &reference.local)
            })
            .unwrap_or(usage);
        Some(ResolvedAttribute { usage, declaration })
    }

    /// Accumulated facets, original built-in type, list and union of a simple
    /// type (or of the simple content of a complex type).
    pub fn simple_type_info(&self, reference: XsdTypeRef<'_>) -> XsdSimpleTypeInfo {
        let mut info = XsdSimpleTypeInfo::default();
        let mut current = Some(reference);
        for _ in 0..MAX_DEPTH {
            let Some(reference) = current else {
                break;
            };
            let Some(definition) = reference.definition else {
                info.builtin = reference.name.filter(|name| name.is_builtin()).cloned();
                break;
            };
            info.facets.inherit(&definition.facets);
            match definition.derivation {
                Some(XsdDerivation::List) if info.item_type.is_none() => {
                    info.item_type = Some(
                        definition
                            .item_type
                            .as_ref()
                            .map(XsdQName::display)
                            .unwrap_or_else(|| "anonymous type".to_owned()),
                    );
                    break;
                }
                Some(XsdDerivation::Union) if info.member_types.is_empty() => {
                    info.member_types = definition
                        .member_types
                        .iter()
                        .map(XsdQName::display)
                        .chain(
                            definition
                                .inline_types
                                .iter()
                                .map(|_| "anonymous type".to_owned()),
                        )
                        .collect();
                    break;
                }
                _ => {}
            }
            current = self.base_type(reference);
        }
        info
    }

    /// Documentation of an enumeration value along the restrictions.
    pub fn enumeration<'a>(
        &'a self,
        reference: XsdTypeRef<'a>,
        value: &str,
    ) -> Option<&'a XsdEnumeration> {
        let mut current = Some(reference);
        for _ in 0..MAX_DEPTH {
            let definition = current?.definition?;
            if let Some(found) = definition
                .facets
                .enumerations
                .iter()
                .find(|enumeration| enumeration.value == value)
            {
                return Some(found);
            }
            current = self.base_type(current?);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(sources: &[&str]) -> XsdModelSet {
        XsdModelSet::new(
            sources
                .iter()
                .map(|source| Arc::new(parse_xsd_model(source).expect("model should parse")))
                .collect(),
        )
    }

    fn step(namespace: Option<&str>, local: &str) -> XsdInstanceStep {
        XsdInstanceStep {
            namespace: namespace.map(str::to_owned),
            local: local.to_owned(),
            xsi_type: None,
        }
    }

    #[test]
    fn lists_child_elements_through_groups_extensions_and_substitutions() {
        let models = set(&[
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:t="urn:t" targetNamespace="urn:t" elementFormDefault="qualified">
            <xs:element name="root" type="t:Derived"/>
            <xs:complexType name="Base"><xs:sequence><xs:element name="first"/></xs:sequence></xs:complexType>
            <xs:complexType name="Derived"><xs:complexContent><xs:extension base="t:Base">
                <xs:choice><xs:group ref="t:G"/><xs:element ref="t:head"/><xs:any/></xs:choice>
            </xs:extension></xs:complexContent></xs:complexType>
            <xs:group name="G"><xs:sequence><xs:element name="grouped"/><xs:element name="first"/></xs:sequence></xs:group>
            <xs:element name="head" abstract="true"/>
            <xs:element name="member" substitutionGroup="t:head"/>
        </xs:schema>"#,
        ]);
        let root = models
            .resolve_element_path(&[step(Some("urn:t"), "root")])
            .unwrap();
        let names = models
            .child_elements(root.element_type.unwrap())
            .iter()
            .map(|child| child.item.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["first", "grouped", "member"]);
        assert_eq!(
            models
                .global_elements()
                .map(|element| element.item.name.as_str())
                .collect::<Vec<_>>(),
            vec!["root", "head", "member"]
        );
    }

    #[test]
    fn parses_documentation_of_every_component_kind() {
        let model = parse_xsd_model(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
                <xs:annotation><xs:documentation>Schema doc</xs:documentation></xs:annotation>
                <xs:element name="root" type="Root">
                    <xs:annotation>
                        <xs:appinfo>ignored</xs:appinfo>
                        <xs:documentation>Root element</xs:documentation>
                    </xs:annotation>
                </xs:element>
                <xs:complexType name="Root">
                    <xs:annotation><xs:documentation>Root type</xs:documentation></xs:annotation>
                    <xs:attribute name="id">
                        <xs:annotation><xs:documentation>Identifier</xs:documentation></xs:annotation>
                    </xs:attribute>
                </xs:complexType>
                <xs:simpleType name="Color">
                    <xs:annotation><xs:documentation>A colour</xs:documentation></xs:annotation>
                    <xs:restriction base="xs:string">
                        <xs:enumeration value="red">
                            <xs:annotation><xs:documentation>Red value</xs:documentation></xs:annotation>
                        </xs:enumeration>
                        <xs:enumeration value="blue"/>
                    </xs:restriction>
                </xs:simpleType>
                <xs:group name="G"><xs:annotation><xs:documentation>Group</xs:documentation></xs:annotation><xs:sequence/></xs:group>
                <xs:attributeGroup name="AG"><xs:annotation><xs:documentation>Attributes</xs:documentation></xs:annotation></xs:attributeGroup>
            </xs:schema>"#,
        )
        .unwrap();

        assert_eq!(model.documentation.as_deref(), Some("Schema doc"));
        assert_eq!(
            model.elements[0].documentation.as_deref(),
            Some("Root element")
        );
        assert_eq!(model.types[0].documentation.as_deref(), Some("Root type"));
        assert_eq!(
            model.types[0].attributes[0].documentation.as_deref(),
            Some("Identifier")
        );
        assert_eq!(model.types[1].documentation.as_deref(), Some("A colour"));
        let enumerations = &model.types[1].facets.enumerations;
        assert_eq!(enumerations[0].documentation.as_deref(), Some("Red value"));
        assert_eq!(enumerations[1].documentation, None);
        assert_eq!(model.groups[0].documentation.as_deref(), Some("Group"));
        assert_eq!(
            model.attribute_groups[0].documentation.as_deref(),
            Some("Attributes")
        );
    }

    #[test]
    fn normalizes_documentation_whitespace_markup_and_entities() {
        let model = parse_xsd_model(
            "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:h=\"http://www.w3.org/1999/xhtml\">\r\n\
             <xs:element name=\"a\"><xs:annotation><xs:documentation>\r\n\
                 First   line\r\n\
                 continues &amp; ends.\r\n\
             \r\n\
                 Second <h:b>bold</h:b> paragraph &#x2014; <![CDATA[<raw>]]>\
                 <h:p>Block</h:p>tail\r\n\
             </xs:documentation></xs:annotation></xs:element></xs:schema>",
        )
        .unwrap();

        assert_eq!(
            model.elements[0].documentation.as_deref(),
            Some(
                "First line continues & ends.\n\nSecond bold paragraph \u{2014} <raw>\n\nBlock\n\ntail"
            )
        );
    }

    #[test]
    fn prefers_untagged_or_english_documentation_blocks() {
        let source = |blocks: &str| {
            format!(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="a"><xs:annotation>{blocks}</xs:annotation></xs:element></xs:schema>"#
            )
        };
        let doc = |blocks: &str| {
            parse_xsd_model(&source(blocks)).unwrap().elements[0]
                .documentation
                .clone()
        };

        assert_eq!(
            doc(r#"<xs:documentation xml:lang="fr">Bonjour</xs:documentation><xs:documentation xml:lang="en-GB">Hello</xs:documentation>"#)
                .as_deref(),
            Some("Hello")
        );
        assert_eq!(
            doc(r#"<xs:documentation>One</xs:documentation><xs:documentation xml:lang="de">Eins</xs:documentation><xs:documentation>Two</xs:documentation>"#)
                .as_deref(),
            Some("One\n\nTwo")
        );
        assert_eq!(
            doc(r#"<xs:documentation xml:lang="fr">Bonjour</xs:documentation><xs:documentation xml:lang="de">Hallo</xs:documentation>"#)
                .as_deref(),
            Some("Bonjour\n\nHallo")
        );
        assert_eq!(doc("<xs:documentation>  </xs:documentation>"), None);
        assert_eq!(doc("<xs:appinfo>tool data</xs:appinfo>"), None);
    }

    #[test]
    fn resolves_qnames_forms_and_references() {
        let model = parse_xsd_model(
            r#"<schema xmlns="http://www.w3.org/2001/XMLSchema" xmlns:t="urn:t" targetNamespace="urn:t" elementFormDefault="qualified">
                <element name="root"><complexType><sequence>
                    <element name="local" type="string"/>
                    <element name="unq" form="unqualified" type="t:Code"/>
                    <element ref="t:other" minOccurs="0" maxOccurs="unbounded"/>
                </sequence><attribute name="a" use="required" default="x"/></complexType></element>
                <element name="other"/>
            </schema>"#,
        )
        .unwrap();
        let root = &model.elements[0];
        let Some(XsdParticle::Group { particles, .. }) =
            &root.anonymous_type.as_ref().unwrap().content
        else {
            panic!("sequence expected");
        };
        let decls = particles
            .iter()
            .map(|particle| match particle {
                XsdParticle::Element(decl) => decl.as_ref(),
                _ => panic!("element expected"),
            })
            .collect::<Vec<_>>();

        assert_eq!(root.namespace.as_deref(), Some("urn:t"));
        assert!(decls[0].type_name.as_ref().unwrap().is_builtin());
        assert_eq!(decls[0].namespace.as_deref(), Some("urn:t"));
        assert_eq!(decls[1].namespace, None);
        assert_eq!(
            decls[1].type_name.as_ref().unwrap().namespace.as_deref(),
            Some("urn:t")
        );
        assert_eq!(decls[2].name, "other");
        assert_eq!(decls[2].reference.as_ref().unwrap().display(), "t:other");
        assert_eq!((decls[2].min_occurs, decls[2].max_occurs), (0, None));
        let attribute = &root.anonymous_type.as_ref().unwrap().attributes[0];
        assert_eq!(attribute.usage, XsdUse::Required);
        assert_eq!(attribute.default.as_deref(), Some("x"));
        assert_eq!(attribute.namespace, None);
    }

    #[test]
    fn resolves_local_declarations_through_the_instance_path() {
        let models = set(&[r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:element name="book"><xs:complexType><xs:sequence>
                <xs:element name="title"><xs:annotation><xs:documentation>Book title</xs:documentation></xs:annotation></xs:element>
                <xs:group ref="Meta"/>
            </xs:sequence></xs:complexType></xs:element>
            <xs:element name="title"><xs:annotation><xs:documentation>Global title</xs:documentation></xs:annotation></xs:element>
            <xs:element name="chapter" type="Chapter"/>
            <xs:complexType name="Base"><xs:sequence><xs:element name="title" type="xs:string"><xs:annotation><xs:documentation>Chapter title</xs:documentation></xs:annotation></xs:element></xs:sequence></xs:complexType>
            <xs:complexType name="Chapter"><xs:complexContent><xs:extension base="Base"><xs:sequence><xs:element name="para"/></xs:sequence></xs:extension></xs:complexContent></xs:complexType>
            <xs:group name="Meta"><xs:sequence><xs:element name="isbn"><xs:annotation><xs:documentation>ISBN</xs:documentation></xs:annotation></xs:element></xs:sequence></xs:group>
        </xs:schema>"#]);
        let doc = |path: &[&str]| {
            let steps = path.iter().map(|name| step(None, name)).collect::<Vec<_>>();
            models
                .resolve_element_path(&steps)
                .and_then(|resolved| resolved.declaration.item.documentation.clone())
        };

        assert_eq!(doc(&["book", "title"]).as_deref(), Some("Book title"));
        assert_eq!(doc(&["title"]).as_deref(), Some("Global title"));
        assert_eq!(doc(&["chapter", "title"]).as_deref(), Some("Chapter title"));
        assert_eq!(doc(&["book", "isbn"]).as_deref(), Some("ISBN"));
        let resolved = models
            .resolve_element_path(&[step(None, "chapter"), step(None, "para")])
            .unwrap();
        assert!(!resolved.declaration.item.global);
        assert_eq!(doc(&["book", "unknown"]), None);
    }

    #[test]
    fn resolves_references_substitution_groups_and_xsi_type_across_schemas() {
        let models = set(&[
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:a="urn:a" xmlns:b="urn:b" targetNamespace="urn:a" elementFormDefault="qualified">
                <xs:element name="root"><xs:complexType><xs:sequence>
                    <xs:element ref="b:shape"/>
                </xs:sequence></xs:complexType></xs:element>
                <xs:complexType name="Special"><xs:attribute name="extra"/></xs:complexType>
            </xs:schema>"#,
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:b="urn:b" targetNamespace="urn:b">
                <xs:element name="shape" abstract="true"><xs:annotation><xs:documentation>Shape</xs:documentation></xs:annotation></xs:element>
                <xs:element name="circle" substitutionGroup="b:shape" type="b:Circle"><xs:annotation><xs:documentation>Circle</xs:documentation></xs:annotation></xs:element>
                <xs:complexType name="Circle"><xs:attribute name="radius" type="xs:decimal"/></xs:complexType>
            </xs:schema>"#,
        ]);
        let shape = models
            .resolve_element_path(&[step(Some("urn:a"), "root"), step(Some("urn:b"), "shape")])
            .unwrap();
        assert_eq!(shape.declaration.schema, 1);
        assert_eq!(shape.particle.schema, 0);
        assert_eq!(
            shape.declaration.item.documentation.as_deref(),
            Some("Shape")
        );

        let circle = models
            .resolve_element_path(&[step(Some("urn:a"), "root"), step(Some("urn:b"), "circle")])
            .unwrap();
        assert_eq!(
            circle.declaration.item.documentation.as_deref(),
            Some("Circle")
        );
        let radius = models
            .resolve_attribute(Some(&circle), None, "radius")
            .unwrap();
        assert_eq!(
            radius.declaration.item.type_name.as_ref().unwrap().local,
            "decimal"
        );

        let mut typed = step(Some("urn:a"), "root");
        typed.xsi_type = Some((Some("urn:a".to_owned()), "Special".to_owned()));
        let root = models.resolve_element_path(&[typed]).unwrap();
        assert!(root.xsi_type.is_some());
        assert!(
            models
                .resolve_attribute(Some(&root), None, "extra")
                .is_some()
        );
    }

    #[test]
    fn collects_attributes_from_groups_references_and_base_types() {
        let models = set(&[
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:t="urn:t" targetNamespace="urn:t">
            <xs:attribute name="lang" type="xs:language"><xs:annotation><xs:documentation>Language</xs:documentation></xs:annotation></xs:attribute>
            <xs:attributeGroup name="Common"><xs:attribute name="id" type="xs:ID" use="required"/></xs:attributeGroup>
            <xs:complexType name="Base"><xs:attribute name="version" fixed="1"/></xs:complexType>
            <xs:complexType name="Item"><xs:complexContent><xs:extension base="t:Base">
                <xs:attributeGroup ref="t:Common"/>
                <xs:attribute ref="t:lang"/>
            </xs:extension></xs:complexContent></xs:complexType>
            <xs:element name="item" type="t:Item"/>
        </xs:schema>"#,
        ]);
        let item = models
            .resolve_element_path(&[step(Some("urn:t"), "item")])
            .unwrap();
        let names = models
            .attribute_uses(item.element_type.unwrap())
            .iter()
            .map(|usage| usage.item.name.clone())
            .collect::<Vec<_>>();
        assert_eq!(names, ["lang", "id", "version"]);

        let lang = models
            .resolve_attribute(Some(&item), Some("urn:t"), "lang")
            .unwrap();
        assert_eq!(
            lang.declaration.item.documentation.as_deref(),
            Some("Language")
        );
        assert!(lang.declaration.item.global);
        let id = models.resolve_attribute(Some(&item), None, "id").unwrap();
        assert_eq!(id.usage.item.usage, XsdUse::Required);
        assert!(
            models
                .resolve_attribute(Some(&item), None, "missing")
                .is_none()
        );
    }

    #[test]
    fn accumulates_simple_type_facets_along_restrictions() {
        let models = set(&[r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:simpleType name="Code"><xs:restriction base="xs:string">
                <xs:pattern value="[A-Z]+"/><xs:maxLength value="8"/>
                <xs:enumeration value="AB"><xs:annotation><xs:documentation>Code AB</xs:documentation></xs:annotation></xs:enumeration>
                <xs:enumeration value="CD"/>
            </xs:restriction></xs:simpleType>
            <xs:simpleType name="Short"><xs:restriction base="Code"><xs:minLength value="2"/><xs:maxLength value="4"/></xs:restriction></xs:simpleType>
            <xs:simpleType name="Codes"><xs:list itemType="Code"/></xs:simpleType>
            <xs:simpleType name="Mixed"><xs:union memberTypes="xs:int Code"><xs:simpleType><xs:restriction base="xs:string"/></xs:simpleType></xs:union></xs:simpleType>
            <xs:element name="a" type="Short"/>
        </xs:schema>"#]);
        let short = models.global_type(None, "Short").unwrap();
        let reference = XsdTypeRef {
            schema: 0,
            name: None,
            definition: Some(short.item),
        };
        let info = models.simple_type_info(reference);
        assert_eq!(info.facets.min_length.as_deref(), Some("2"));
        assert_eq!(info.facets.max_length.as_deref(), Some("4"));
        assert_eq!(info.facets.patterns, ["[A-Z]+"]);
        assert_eq!(info.facets.enumerations.len(), 2);
        assert_eq!(info.builtin.as_ref().unwrap().display(), "xs:string");
        assert_eq!(
            models
                .enumeration(reference, "AB")
                .and_then(|value| value.documentation.as_deref()),
            Some("Code AB")
        );

        let list = models.global_type(None, "Codes").unwrap();
        let info = models.simple_type_info(XsdTypeRef {
            schema: 0,
            name: None,
            definition: Some(list.item),
        });
        assert_eq!(info.item_type.as_deref(), Some("Code"));
        let union = models.global_type(None, "Mixed").unwrap();
        let info = models.simple_type_info(XsdTypeRef {
            schema: 0,
            name: None,
            definition: Some(union.item),
        });
        assert_eq!(info.member_types, ["xs:int", "Code", "anonymous type"]);
    }

    #[test]
    fn tolerates_cyclic_groups_and_types() {
        let models = set(&[r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
            <xs:group name="G"><xs:sequence><xs:group ref="G"/></xs:sequence></xs:group>
            <xs:complexType name="T"><xs:complexContent><xs:extension base="T"><xs:group ref="G"/></xs:extension></xs:complexContent></xs:complexType>
            <xs:simpleType name="S"><xs:restriction base="S"/></xs:simpleType>
            <xs:element name="a" type="T"/>
        </xs:schema>"#]);
        assert!(
            models
                .resolve_element_path(&[step(None, "a"), step(None, "b")])
                .is_none()
        );
        let s = models.global_type(None, "S").unwrap();
        let _ = models.simple_type_info(XsdTypeRef {
            schema: 0,
            name: None,
            definition: Some(s.item),
        });
    }

    #[test]
    fn rejects_non_schema_documents() {
        assert!(parse_xsd_model("<root/>").is_err());
        assert!(parse_xsd_model("<xs:schema").is_err());
    }

    #[test]
    fn bounds_the_depth_and_scopes_of_schemas() {
        let deep = format!(
            "<xs:schema xmlns:xs=\"{XSD_NAMESPACE}\">{}{}</xs:schema>",
            "<xs:complexType><xs:sequence>".repeat(50_000),
            "</xs:sequence></xs:complexType>".repeat(50_000)
        );
        let error = parse_xsd_model(&deep).unwrap_err();
        assert!(error.contains("nested more than"), "{error}");
        // Within the limit, nested prefixes still resolve.
        let nested = format!(
            "<xs:schema xmlns:xs=\"{XSD_NAMESPACE}\" xmlns:t=\"urn:t\" targetNamespace=\"urn:t\"><xs:element name=\"a\"><xs:complexType xmlns:u=\"urn:u\"><xs:sequence xmlns:t=\"urn:other\"><xs:element name=\"b\" type=\"t:B\"/></xs:sequence><xs:attribute name=\"c\" type=\"t:C\"/></xs:complexType></xs:element></xs:schema>"
        );
        let model = parse_xsd_model(&nested).unwrap();
        let element = &model.elements[0];
        let definition = element
            .anonymous_type
            .as_ref()
            .expect("anonymous type expected");
        let debug = format!("{definition:?}");
        assert!(debug.contains("urn:other"), "{debug}");
        assert!(debug.contains("urn:t"), "{debug}");
        // Many elements declaring prefixes deep in a schema stay linear
        // (scopes are chained, not copied).
        let prefixes = (0..200)
            .map(|index| format!("<xs:sequence xmlns:p{index}=\"urn:{index}\">"))
            .collect::<String>();
        let wide = format!(
            "<xs:schema xmlns:xs=\"{XSD_NAMESPACE}\"><xs:complexType name=\"T\">{prefixes}{}{}</xs:complexType></xs:schema>",
            "<xs:element name=\"e\" xmlns:q=\"urn:q\" type=\"q:E\"/>".repeat(50_000),
            "</xs:sequence>".repeat(200)
        );
        let start = std::time::Instant::now();
        assert!(parse_xsd_model(&wide).is_ok());
        assert!(start.elapsed() < std::time::Duration::from_secs(30));
    }
}
