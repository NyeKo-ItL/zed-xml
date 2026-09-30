//! Modèle de composants XSD conservant l'arborescence du schéma et la
//! documentation `xs:annotation/xs:documentation`.
//!
//! [`XsdSchema`](crate::XsdSchema) aplatit le schéma par nom local pour la
//! validation ; ce modèle garde au contraire les déclarations globales et
//! locales, les types nommés et anonymes, les groupes et les facettes, avec
//! les noms qualifiés résolus. Il sert au survol (`textDocument/hover`) :
//! résolution d'une déclaration d'élément dans son contexte (chemin des
//! ancêtres), des attributs d'un type et des facettes d'un type simple.
//!
//! La documentation est normalisée : un bloc `xs:documentation` sans
//! `xml:lang` ou en anglais est préféré, les blocs multiples sont concaténés,
//! le balisage imbriqué (XHTML...) est réduit à son texte et les espaces sont
//! regroupés en paragraphes. `xs:appinfo` est ignoré.

use std::{collections::HashMap, sync::Arc};

use quick_xml::{Reader, events::Event};

/// Espace de noms de XML Schema.
pub const XSD_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema";
/// Espace de noms réservé du préfixe `xml`.
pub const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";

/// Profondeur maximale suivie dans les références (groupes, dérivations),
/// pour se protéger des schémas cycliques.
const MAX_DEPTH: usize = 32;

/// Nom qualifié lu dans une valeur d'attribut du schéma (`type`, `ref`,
/// `base`...), avec son espace de noms résolu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsdQName {
    pub prefix: Option<String>,
    pub namespace: Option<String>,
    pub local: String,
}

impl XsdQName {
    /// Nom tel qu'écrit dans le schéma (`prefix:local`).
    pub fn display(&self) -> String {
        match &self.prefix {
            Some(prefix) => format!("{prefix}:{}", self.local),
            None => self.local.clone(),
        }
    }

    /// Indique si le nom désigne un composant de XML Schema (`xs:string`...).
    pub fn is_builtin(&self) -> bool {
        self.namespace.as_deref() == Some(XSD_NAMESPACE)
    }
}

/// Utilisation d'un attribut (`use`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum XsdUse {
    #[default]
    Optional,
    Required,
    Prohibited,
}

/// Déclaration d'élément, globale ou locale (éventuellement `ref`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsdElementDecl {
    /// Nom local (celui de la référence pour un `ref`).
    pub name: String,
    pub reference: Option<XsdQName>,
    /// Espace de noms effectif des instances (`form`, `elementFormDefault`).
    pub namespace: Option<String>,
    pub type_name: Option<XsdQName>,
    pub anonymous_type: Option<Box<XsdTypeDef>>,
    pub min_occurs: usize,
    pub max_occurs: Option<usize>,
    pub default: Option<String>,
    pub fixed: Option<String>,
    pub nillable: bool,
    pub is_abstract: bool,
    pub substitution_groups: Vec<XsdQName>,
    pub global: bool,
    pub documentation: Option<String>,
}

/// Déclaration d'attribut, globale ou locale (éventuellement `ref`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsdAttributeDecl {
    pub name: String,
    pub reference: Option<XsdQName>,
    /// Espace de noms effectif (`form`, `attributeFormDefault`).
    pub namespace: Option<String>,
    pub type_name: Option<XsdQName>,
    pub anonymous_type: Option<Box<XsdTypeDef>>,
    pub usage: XsdUse,
    pub default: Option<String>,
    pub fixed: Option<String>,
    pub global: bool,
    pub documentation: Option<String>,
}

/// Mode de dérivation d'un type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XsdDerivation {
    Restriction,
    Extension,
    List,
    Union,
}

/// Valeur d'énumération et sa documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsdEnumeration {
    pub value: String,
    pub documentation: Option<String>,
}

/// Facettes d'une restriction de type simple (valeurs brutes du schéma).
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
    /// Indique si aucune facette n'est définie.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Complète les facettes absentes avec celles du type de base (les
    /// facettes du type dérivé restent prioritaires).
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

/// Compositeur d'un groupe de modèle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XsdCompositor {
    Sequence,
    Choice,
    All,
}

/// Particule d'un modèle de contenu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XsdParticle {
    Element(Box<XsdElementDecl>),
    Group {
        compositor: XsdCompositor,
        particles: Vec<XsdParticle>,
    },
    GroupRef(XsdQName),
    Any,
}

/// Définition de type simple ou complexe, nommée ou anonyme.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XsdTypeDef {
    pub name: Option<String>,
    pub namespace: Option<String>,
    pub complex: bool,
    pub mixed: bool,
    pub simple_content: bool,
    pub derivation: Option<XsdDerivation>,
    /// Type de base (`restriction`/`extension`).
    pub base: Option<XsdQName>,
    /// Type des items d'une liste (`itemType`).
    pub item_type: Option<XsdQName>,
    /// Types membres d'une union (`memberTypes`).
    pub member_types: Vec<XsdQName>,
    /// Types simples anonymes de la restriction, de la liste ou de l'union.
    pub inline_types: Vec<XsdTypeDef>,
    pub content: Option<XsdParticle>,
    pub attributes: Vec<XsdAttributeDecl>,
    pub attribute_group_refs: Vec<XsdQName>,
    pub any_attribute: bool,
    pub facets: XsdFacets,
    pub documentation: Option<String>,
}

/// Groupe de modèle nommé (`xs:group`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsdGroupDef {
    pub name: String,
    pub namespace: Option<String>,
    pub content: Option<XsdParticle>,
    pub documentation: Option<String>,
}

/// Groupe d'attributs nommé (`xs:attributeGroup`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsdAttributeGroupDef {
    pub name: String,
    pub namespace: Option<String>,
    pub attributes: Vec<XsdAttributeDecl>,
    pub attribute_group_refs: Vec<XsdQName>,
    pub any_attribute: bool,
    pub documentation: Option<String>,
}

/// Composants d'un document XSD.
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
}

/// Analyse un document XSD en modèle de composants documentés.
pub fn parse_xsd_model(source: &str) -> Result<XsdModel, String> {
    if source.len() > crate::MAX_XSD_SOURCE_BYTES {
        return Err("schéma XSD trop volumineux".to_owned());
    }
    let (root, scopes) = build_tree(source)?;
    if !is_xsd(&root, "schema") {
        return Err("la racine n'est pas un xs:schema".to_owned());
    }
    let target_namespace = root.attribute("targetNamespace").filter(|v| !v.is_empty());
    let context = Context {
        scopes: &scopes,
        element_form_qualified: root.attribute("elementFormDefault").as_deref()
            == Some("qualified"),
        attribute_form_qualified: root.attribute("attributeFormDefault").as_deref()
            == Some("qualified"),
        target_namespace,
    };
    let mut model = XsdModel {
        target_namespace: context.target_namespace.clone(),
        element_form_qualified: context.element_form_qualified,
        attribute_form_qualified: context.attribute_form_qualified,
        documentation: documentation(&root),
        ..XsdModel::default()
    };
    context.top_level(&root, &mut model);
    Ok(model)
}

/// Élément de l'arbre XML minimal construit pour l'interprétation.
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
    /// Attribut non préfixé.
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

/// Élément XSD : espace de noms XML Schema, ou aucun (préfixe non déclaré,
/// toléré comme le fait la validation).
fn is_xsd(node: &Node, local: &str) -> bool {
    node.local == local
        && node
            .namespace
            .as_deref()
            .is_none_or(|namespace| namespace == XSD_NAMESPACE)
}

type Scope = HashMap<String, String>;

fn build_tree(source: &str) -> Result<(Node, Vec<Scope>), String> {
    let mut reader = Reader::from_str(source);
    let mut scopes: Vec<Scope> = vec![Scope::new()];
    let mut stack: Vec<Node> = Vec::new();
    let mut root = None;
    loop {
        let event = reader
            .read_event()
            .map_err(|error| format!("erreur XSD : {error}"))?;
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(element) | Event::Empty(element) => {
                let qname = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                let mut attributes = Vec::new();
                for attribute in element.attributes().flatten() {
                    let key = String::from_utf8_lossy(attribute.key.as_ref()).into_owned();
                    let value = attribute
                        .unescape_value()
                        .map(|value| value.into_owned())
                        .unwrap_or_default();
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
                    let mut scope = scopes[parent_scope].clone();
                    scope.extend(declarations);
                    scopes.push(scope);
                    scopes.len() - 1
                };
                let (prefix, local) = match qname.split_once(':') {
                    Some((prefix, local)) => (prefix, local.to_owned()),
                    None => ("", qname.clone()),
                };
                let namespace = scopes[scope]
                    .get(prefix)
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
        .ok_or_else(|| "document XSD vide".to_owned())
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
            self.scopes[node.scope]
                .get(prefix.unwrap_or(""))
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
                            any_attribute: false,
                            documentation: documentation(child),
                        };
                        for item in child.elements() {
                            match item.local.as_str() {
                                "attribute" => group.attributes.extend(self.attribute(item, false)),
                                "attributeGroup" => group
                                    .attribute_group_refs
                                    .extend(self.qname_attribute(item, "ref")),
                                "anyAttribute" => group.any_attribute = true,
                                _ => {}
                            }
                        }
                        model.attribute_groups.push(group);
                    }
                }
                "redefine" | "override" => self.top_level(child, model),
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
            min_occurs: node
                .attribute("minOccurs")
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(1),
            max_occurs: match node.attribute("maxOccurs").as_deref().map(str::trim) {
                Some("unbounded") => None,
                Some(value) => Some(value.parse().unwrap_or(1)),
                None => Some(1),
            },
            default: node.attribute("default"),
            fixed: node.attribute("fixed"),
            nillable: node.attribute("nillable").as_deref() == Some("true"),
            is_abstract: node.attribute("abstract").as_deref() == Some("true"),
            substitution_groups: self.qname_list(node, "substitutionGroup"),
            global,
            documentation: documentation(node),
        })
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

    fn complex_type(&self, node: &Node) -> XsdTypeDef {
        let mut definition = XsdTypeDef {
            name: node.attribute("name"),
            namespace: self.target_namespace.clone(),
            complex: true,
            mixed: node.attribute("mixed").as_deref() == Some("true"),
            documentation: documentation(node),
            ..XsdTypeDef::default()
        };
        for child in node.elements() {
            match child.local.as_str() {
                "simpleContent" | "complexContent" => {
                    definition.simple_content = child.local == "simpleContent";
                    if child.attribute("mixed").as_deref() == Some("true") {
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

    /// Modèle de contenu et attributs portés directement par `node`.
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
                "anyAttribute" => definition.any_attribute = true,
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

    /// Premier groupe de modèle (`sequence`, `choice`, `all`) de `node`.
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
            "group" => return self.qname_attribute(node, "ref").map(XsdParticle::GroupRef),
            "any" => return Some(XsdParticle::Any),
            _ => return None,
        };
        Some(XsdParticle::Group {
            compositor,
            particles: node
                .elements()
                .filter_map(|child| self.particle(child))
                .collect(),
        })
    }
}

/// Blocs de mise en page XHTML séparés par un paragraphe dans le texte.
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

/// Documentation `xs:annotation/xs:documentation` directement portée par
/// `node`, normalisée.
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

/// Regroupe les lignes non vides consécutives en paragraphes séparés par
/// une ligne vide, avec des espaces normalisés.
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

/// Élément d'un modèle et index du schéma qui le déclare.
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

/// Type résolu : définition (nommée ou anonyme) ou type prédéfini.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XsdTypeRef<'a> {
    pub schema: usize,
    /// Nom du type tel que référencé (`None` pour un type anonyme).
    pub name: Option<&'a XsdQName>,
    /// Définition (`None` pour un type prédéfini ou introuvable).
    pub definition: Option<&'a XsdTypeDef>,
}

/// Déclaration d'élément résolue dans son contexte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedElement<'a> {
    /// Particule trouvée dans le modèle de contenu (éventuellement `ref`).
    pub particle: Located<'a, XsdElementDecl>,
    /// Déclaration effective (déclaration globale pour un `ref`).
    pub declaration: Located<'a, XsdElementDecl>,
    /// Type effectif (`xsi:type` compris).
    pub element_type: Option<XsdTypeRef<'a>>,
    /// `xsi:type` appliqué à l'instance.
    pub xsi_type: Option<&'a XsdTypeDef>,
}

/// Déclaration d'attribut résolue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedAttribute<'a> {
    /// Utilisation locale (porte `use`), éventuellement `ref`.
    pub usage: Located<'a, XsdAttributeDecl>,
    /// Déclaration effective (déclaration globale pour un `ref`).
    pub declaration: Located<'a, XsdAttributeDecl>,
}

/// Étape du chemin d'un élément d'instance, de la racine vers l'élément.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XsdInstanceStep {
    pub namespace: Option<String>,
    pub local: String,
    /// `xsi:type` (espace de noms, nom local) porté par l'élément.
    pub xsi_type: Option<(Option<String>, String)>,
}

/// Synthèse d'un type simple : facettes cumulées le long des restrictions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XsdSimpleTypeInfo {
    pub facets: XsdFacets,
    /// Type prédéfini d'origine (`xs:string`...).
    pub builtin: Option<XsdQName>,
    pub item_type: Option<String>,
    pub member_types: Vec<String>,
}

/// Ensemble de schémas chargés ensemble (inclusions et imports compris).
#[derive(Debug, Clone, Default)]
pub struct XsdModelSet {
    models: Vec<Arc<XsdModel>>,
}

/// Espaces de noms comparés strictement, puis sur le seul nom local en
/// repli (schémas « caméléon », documents sans espace de noms...).
fn namespace_matches(strict: bool, expected: Option<&str>, actual: Option<&str>) -> bool {
    !strict || expected == actual
}

impl XsdModelSet {
    pub fn new(models: Vec<Arc<XsdModel>>) -> Self {
        Self { models }
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
        [true, false].into_iter().find_map(|strict| {
            self.models.iter().enumerate().find_map(|(schema, model)| {
                items(model)
                    .iter()
                    .find(|item| {
                        let (name, item_namespace) = name_of(item);
                        name == local && namespace_matches(strict, namespace, item_namespace)
                    })
                    .map(|item| Located { schema, item })
            })
        })
    }

    /// Déclaration d'élément globale.
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

    /// Déclaration d'attribut globale.
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

    /// Type global nommé.
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

    /// Groupe de modèle nommé.
    pub fn group(&self, namespace: Option<&str>, local: &str) -> Option<Located<'_, XsdGroupDef>> {
        self.find(
            namespace,
            local,
            |model| &model.groups,
            |item| (item.name.as_str(), item.namespace.as_deref()),
        )
    }

    /// Groupe d'attributs nommé.
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

    /// Résout un nom de type : type prédéfini ou type global.
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

    /// Déclaration effective d'une particule : suit un `ref` vers la
    /// déclaration globale.
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

    /// Type d'une déclaration d'élément (`type`, type anonyme ou type de la
    /// tête de substitution).
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

    /// Type d'un attribut (`type` ou type simple anonyme).
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

    /// Type de base d'une définition dérivée.
    pub fn base_type<'a>(&'a self, reference: XsdTypeRef<'a>) -> Option<XsdTypeRef<'a>> {
        let definition = reference.definition?;
        if let Some(base) = &definition.base {
            return Some(self.resolve_type(reference.schema, base));
        }
        // Restriction d'un type simple anonyme (`<xs:restriction><xs:simpleType>`).
        (definition.derivation == Some(XsdDerivation::Restriction))
            .then(|| definition.inline_types.first())
            .flatten()
            .map(|inline| XsdTypeRef {
                schema: reference.schema,
                name: None,
                definition: Some(inline),
            })
    }

    /// Résout la déclaration d'un élément d'instance à partir du chemin de
    /// ses ancêtres (racine en premier) : déclarations locales des modèles de
    /// contenu, références, groupes, extensions, groupes de substitution et
    /// `xsi:type`. Repli sur les déclarations globales (`xs:any`, contenu
    /// inconnu).
    pub fn resolve_element_path(&self, path: &[XsdInstanceStep]) -> Option<ResolvedElement<'_>> {
        let (first, rest) = path.split_first()?;
        let particle = self.global_element(first.namespace.as_deref(), &first.local)?;
        let mut current = self.resolved(particle, first);
        for step in rest {
            let particle = current
                .element_type
                .and_then(|parent| self.find_child(parent, step))
                .or_else(|| self.global_element(step.namespace.as_deref(), &step.local))?;
            current = self.resolved(particle, step);
        }
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
        }
    }

    /// Particules de contenu d'un type, contenu hérité par extension compris.
    fn content_particles<'a>(
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
        [true, false].into_iter().find_map(|strict| {
            particles.iter().find_map(|&(schema, particle)| {
                self.find_in_particle(schema, particle, step, strict, 0)
            })
        })
    }

    fn find_in_particle<'a>(
        &'a self,
        schema: usize,
        particle: &'a XsdParticle,
        step: &XsdInstanceStep,
        strict: bool,
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
                // Membre d'un groupe de substitution dont la particule est la tête.
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
                self.find_in_particle(schema, particle, step, strict, depth + 1)
            }),
            XsdParticle::GroupRef(name) => {
                let group = self.group(name.namespace.as_deref(), &name.local)?;
                self.find_in_particle(
                    group.schema,
                    group.item.content.as_ref()?,
                    step,
                    strict,
                    depth + 1,
                )
            }
            XsdParticle::Any => None,
        }
    }

    /// Attributs utilisables sur un type : attributs propres, groupes
    /// d'attributs et attributs hérités du type de base.
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
            self.collect_group_attributes(group, depth + 1, uses);
        }
        if definition.complex
            && let Some(base) = self.base_type(reference)
        {
            let own = uses.len();
            let mut inherited = Vec::new();
            self.collect_attribute_uses(base, depth + 1, &mut inherited);
            // Une redéclaration (restriction) masque l'attribut hérité.
            inherited.retain(|candidate| {
                !uses[..own]
                    .iter()
                    .any(|existing| existing.item.name == candidate.item.name)
            });
            uses.extend(inherited);
        }
    }

    fn collect_group_attributes<'a>(
        &'a self,
        name: &XsdQName,
        depth: usize,
        uses: &mut Vec<Located<'a, XsdAttributeDecl>>,
    ) {
        if depth > MAX_DEPTH {
            return;
        }
        let Some(group) = self.attribute_group(name.namespace.as_deref(), &name.local) else {
            return;
        };
        let schema = group.schema;
        uses.extend(
            group
                .item
                .attributes
                .iter()
                .map(|item| Located { schema, item }),
        );
        for nested in &group.item.attribute_group_refs {
            self.collect_group_attributes(nested, depth + 1, uses);
        }
    }

    /// Résout un attribut d'instance sur un élément résolu, puis parmi les
    /// attributs globaux (attributs qualifiés comme `xml:lang`).
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

    /// Facettes cumulées, type prédéfini d'origine, liste et union d'un type
    /// simple (ou du contenu simple d'un type complexe).
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
                            .unwrap_or_else(|| "type anonyme".to_owned()),
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
                                .map(|_| "type anonyme".to_owned()),
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

    /// Documentation d'une valeur d'énumération le long des restrictions.
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
        assert_eq!(info.member_types, ["xs:int", "Code", "type anonyme"]);
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
}
