//! `textDocument/hover` : documentation XSD au survol, comme LemMinX.
//!
//! Dans un document d'instance lié à un schéma (`xsi:schemaLocation`,
//! `xsi:noNamespaceSchemaLocation`) :
//!
//! - nom d'élément (balise ouvrante ou fermante) : déclaration résolue dans
//!   son contexte (déclarations locales du modèle de contenu du parent,
//!   références, groupes, extensions, groupes de substitution, `xsi:type`),
//!   espace de noms, type et type de base, cardinalité, valeurs par défaut ou
//!   fixe, documentation `xs:documentation` et lien vers le schéma source ;
//! - nom d'attribut : déclaration, type, utilisation (`use`), valeur par
//!   défaut ou fixe et documentation ;
//! - valeur d'attribut ou contenu texte d'un élément de type simple :
//!   documentation de la valeur d'énumération et résumé des facettes.
//!
//! Dans un schéma XSD, une référence `type`, `ref`, `base`, `itemType`,
//! `memberTypes` ou `substitutionGroup` (ou le `name` d'un composant global)
//! affiche la documentation du composant référencé, y compris dans les
//! schémas inclus ou importés.
//!
//! Sans schéma, un survol minimal (nom et espace de noms) est conservé. Le
//! contenu est du Markdown et la réponse porte l'étendue survolée.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};
use xml_core::tags::{
    XmlAttribute, XmlTagKind, XmlTagTree, qualified_name_parts, resolve_namespace, scan_attributes,
};
use xsd_core::model::{
    Located, XSD_NAMESPACE, XsdAttributeDecl, XsdDerivation, XsdElementDecl, XsdInstanceStep,
    XsdModel, XsdModelSet, XsdTypeRef, XsdUse, parse_xsd_model,
};
use xsd_core::{
    SchemaLocation, SchemaLocationKind, resolve_schema_dependencies_with, resolve_schema_location,
    resolve_schema_locations_with,
};

use crate::{
    catalog::Catalogs, path_to_uri, schema_resolution_source, selection::LineIndex, uri_to_path,
};

const XSI_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema-instance";
/// Nombre maximal de valeurs d'énumération listées.
const MAX_LISTED_VALUES: usize = 20;
/// Longueur maximale d'une valeur recopiée dans le titre.
const MAX_VALUE_CHARS: usize = 80;

/// Modèles XSD lus sur disque, invalidés par date de modification, avec les
/// dépendances (`xs:include`, `xs:import`) de chaque schéma.
pub type ModelCache = HashMap<PathBuf, (SystemTime, Arc<XsdModel>, Vec<PathBuf>)>;

/// Schémas chargés pour une requête : `paths[i]` est la source de
/// `set.models()[i]`.
pub(crate) struct LoadedModels {
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) set: XsdModelSet,
}

/// Contexte partagé par les requêtes de survol.
pub struct HoverContext<'a> {
    /// Documents ouverts (URI -> contenu), prioritaires sur le disque.
    pub documents: &'a HashMap<String, String>,
    pub cache: &'a mut ModelCache,
    /// Schémas associés au document de la requête par
    /// `xml.fileAssociations`, utilisés lorsqu'il n'en déclare aucun.
    pub associated_schemas: Vec<PathBuf>,
    /// Catalogues XML (`xml.catalogs`) consultés pour résoudre les
    /// emplacements de schémas.
    pub catalogs: &'a Catalogs,
}

/// Répond à `textDocument/hover` pour le document `uri` au curseur `offset`.
pub fn hover(
    context: &mut HoverContext<'_>,
    uri: &str,
    source: &str,
    offset: usize,
) -> Option<Value> {
    let document = Document::parse(source);
    let target = document.target_at(offset)?;
    let (markdown, range) = if document.is_schema() {
        schema_hover(context, uri, &document, &target, offset)
            .or_else(|| instance_hover(context, uri, &document, &target))?
    } else {
        instance_hover(context, uri, &document, &target)?
    };
    let lines = LineIndex::new(source);
    Some(json!({
        "contents": {"kind": "markdown", "value": markdown},
        "range": {
            "start": lines.position(source, range.start),
            "end": lines.position(source, range.end),
        },
    }))
}

/// Construction survolée.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    /// Nom d'une balise ouvrante ou fermante de l'élément.
    ElementName { element: usize, name: Range<usize> },
    /// Nom de l'attribut `attribute` de l'élément.
    AttributeName { element: usize, attribute: usize },
    /// Valeur de l'attribut `attribute` de l'élément.
    AttributeValue { element: usize, attribute: usize },
    /// Contenu texte (espaces de bord exclus) de l'élément.
    Text { element: usize, range: Range<usize> },
}

pub(crate) struct Document<'a> {
    pub(crate) source: &'a str,
    pub(crate) tree: XmlTagTree,
    pub(crate) attributes: Vec<Vec<XmlAttribute>>,
}

impl<'a> Document<'a> {
    pub(crate) fn parse(source: &'a str) -> Self {
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

    fn target_at(&self, offset: usize) -> Option<Target> {
        let elements = self.tree.elements();
        for (index, element) in elements.iter().enumerate() {
            let start = &element.start_tag;
            if start.range.start > offset {
                break;
            }
            if start.name_contains(offset) {
                return Some(Target::ElementName {
                    element: index,
                    name: start.name.clone(),
                });
            }
            if let Some(end) = &element.end_tag
                && end.kind == XmlTagKind::End
                && end.name_contains(offset)
            {
                return Some(Target::ElementName {
                    element: index,
                    name: end.name.clone(),
                });
            }
            if start.range.start < offset && offset < start.range.end.max(start.name.end + 1) {
                for (attribute_index, attribute) in self.attributes[index].iter().enumerate() {
                    if attribute.name.start <= offset && offset <= attribute.name.end {
                        return Some(Target::AttributeName {
                            element: index,
                            attribute: attribute_index,
                        });
                    }
                    if let Some(value) = &attribute.value
                        && value.start <= offset
                        && offset <= value.end
                    {
                        return Some(Target::AttributeValue {
                            element: index,
                            attribute: attribute_index,
                        });
                    }
                }
                return None;
            }
        }
        let element = self.tree.innermost_element_at(offset)?;
        let content = elements[element].content_range()?;
        if offset < content.start || offset > content.end {
            return None;
        }
        // Segment de texte entre les enfants qui entourent le curseur.
        let mut segment = content.clone();
        for child in elements
            .iter()
            .filter(|child| child.parent == Some(element))
        {
            let range = child.range();
            if range.end <= offset {
                segment.start = segment.start.max(range.end);
            } else if range.start >= offset {
                segment.end = segment.end.min(range.start);
            }
        }
        let text = &self.source[segment.clone()];
        let leading = text.len() - text.trim_start().len();
        let trimmed = text.trim();
        let range = segment.start + leading..segment.start + leading + trimmed.len();
        (!trimmed.is_empty()
            && !trimmed.starts_with('<')
            && range.start <= offset
            && offset <= range.end)
            .then_some(Target::Text { element, range })
    }

    pub(crate) fn element_name(&self, element: usize) -> &'a str {
        self.tree.elements()[element].name(self.source)
    }

    pub(crate) fn namespace(&self, element: usize, prefix: Option<&str>) -> Option<&'a str> {
        resolve_namespace(self.source, &self.tree, &self.attributes, element, prefix).flatten()
    }

    /// Espace de noms et nom local d'un nom qualifié lu dans `element`.
    pub(crate) fn split(&self, element: usize, name: &'a str) -> (Option<&'a str>, &'a str) {
        match name.split_once(':') {
            Some((prefix, local)) => (self.namespace(element, Some(prefix)), local),
            None => (self.namespace(element, None), name),
        }
    }

    fn attribute(&self, element: usize, attribute: usize) -> &XmlAttribute {
        &self.attributes[element][attribute]
    }

    /// Attribut non préfixé `name` de l'élément.
    fn attribute_value(&self, element: usize, name: &str) -> Option<&'a str> {
        self.attributes[element]
            .iter()
            .find(|attribute| attribute.name(self.source) == name)
            .and_then(|attribute| attribute.value(self.source))
    }

    fn is_xsd(&self, element: usize, local: &str) -> bool {
        let (namespace, name) = self.split(element, self.element_name(element));
        name == local && namespace == Some(XSD_NAMESPACE)
    }

    pub(crate) fn is_schema(&self) -> bool {
        self.tree
            .elements()
            .iter()
            .position(|element| element.parent.is_none())
            .is_some_and(|root| self.is_xsd(root, "schema"))
    }

    /// Chemin de l'élément dans l'instance, racine en premier.
    pub(crate) fn instance_path(&self, element: usize) -> Vec<XsdInstanceStep> {
        let mut indices = self.tree.ancestors(element).collect::<Vec<_>>();
        indices.reverse();
        indices.push(element);
        indices
            .into_iter()
            .map(|index| {
                let (namespace, local) = self.split(index, self.element_name(index));
                XsdInstanceStep {
                    namespace: namespace.map(str::to_owned),
                    local: local.to_owned(),
                    xsi_type: self.xsi_type(index),
                }
            })
            .collect()
    }

    /// Schémas référencés par `xsi:schemaLocation` et
    /// `xsi:noNamespaceSchemaLocation`, lus de façon tolérante (document mal
    /// formé en cours de saisie), résolus via les catalogues.
    fn schema_locations(&self, document_path: &Path, catalogs: &Catalogs) -> Vec<PathBuf> {
        let base = document_path.parent().unwrap_or_else(|| Path::new(""));
        let mut paths = Vec::new();
        for element in 0..self.tree.elements().len() {
            for attribute in &self.attributes[element] {
                let (prefix, local) = qualified_name_parts(self.source, attribute.name.clone());
                let Some(prefix) = prefix.map(|range| &self.source[range]) else {
                    continue;
                };
                if self.namespace(element, Some(prefix)) != Some(XSI_NAMESPACE) {
                    continue;
                }
                let value = attribute.value(self.source).unwrap_or_default();
                let tokens = value.split_whitespace().collect::<Vec<_>>();
                let requests = match &self.source[local] {
                    "schemaLocation" => tokens
                        .chunks_exact(2)
                        .map(|pair| SchemaLocation {
                            kind: SchemaLocationKind::SchemaLocation,
                            namespace: Some(pair[0]),
                            location: Some(pair[1]),
                            base_directory: base,
                        })
                        .collect::<Vec<_>>(),
                    "noNamespaceSchemaLocation" => tokens
                        .first()
                        .map(|location| SchemaLocation {
                            kind: SchemaLocationKind::NoNamespaceSchemaLocation,
                            namespace: None,
                            location: Some(location),
                            base_directory: base,
                        })
                        .into_iter()
                        .collect(),
                    _ => continue,
                };
                paths.extend(requests.iter().filter_map(|request| {
                    resolve_schema_location(request, &|request| catalogs.resolve_schema(request))
                }));
            }
        }
        paths
    }

    fn xsi_type(&self, element: usize) -> Option<(Option<String>, String)> {
        self.attributes[element].iter().find_map(|attribute| {
            let (prefix, local) = qualified_name_parts(self.source, attribute.name.clone());
            let prefix = prefix.map(|range| &self.source[range])?;
            if &self.source[local] != "type"
                || self.namespace(element, Some(prefix)) != Some(XSI_NAMESPACE)
            {
                return None;
            }
            let value = attribute.value(self.source)?.trim();
            let (namespace, local) = self.split(element, value);
            Some((namespace.map(str::to_owned), local.to_owned()))
        })
    }
}

// ---------------------------------------------------------------------------
// Chargement des schémas
// ---------------------------------------------------------------------------

fn open_document<'d>(documents: &'d HashMap<String, String>, path: &Path) -> Option<&'d String> {
    documents
        .iter()
        .find(|(uri, _)| uri_to_path(uri) == path)
        .map(|(_, source)| source)
}

fn dependency_paths(source: &str, path: &Path, catalogs: &Catalogs) -> Vec<PathBuf> {
    resolve_schema_dependencies_with(source, path, &|request| catalogs.resolve_schema(request))
        .map(|references| {
            references
                .into_iter()
                .map(|reference| reference.path)
                .collect()
        })
        .unwrap_or_default()
}

/// Charge les schémas `roots` et leurs dépendances, en largeur d'abord pour
/// que les schémas référencés directement soient prioritaires.
pub(crate) fn load_models(
    context: &mut HoverContext<'_>,
    loaded: Vec<(PathBuf, Arc<XsdModel>)>,
    roots: Vec<PathBuf>,
) -> LoadedModels {
    let mut visited = loaded
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<HashSet<_>>();
    let (mut paths, mut models): (Vec<_>, Vec<_>) = loaded.into_iter().unzip();
    let mut queue = VecDeque::from(roots);
    while let Some(path) = queue.pop_front() {
        if !visited.insert(path.clone()) {
            continue;
        }
        let loaded = if let Some(source) = open_document(context.documents, &path) {
            parse_xsd_model(source).ok().map(|model| {
                let dependencies = dependency_paths(source, &path, context.catalogs);
                (Arc::new(model), dependencies)
            })
        } else {
            let modified = fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(UNIX_EPOCH);
            match context.cache.get(&path) {
                Some((cached, model, dependencies)) if *cached == modified => {
                    Some((model.clone(), dependencies.clone()))
                }
                _ => fs::read_to_string(&path).ok().and_then(|source| {
                    let model = Arc::new(parse_xsd_model(&source).ok()?);
                    let dependencies = dependency_paths(&source, &path, context.catalogs);
                    context.cache.insert(
                        path.clone(),
                        (modified, model.clone(), dependencies.clone()),
                    );
                    Some((model, dependencies))
                }),
            }
        };
        if let Some((model, dependencies)) = loaded {
            paths.push(path);
            models.push(model);
            queue.extend(dependencies);
        }
    }
    LoadedModels {
        paths,
        set: XsdModelSet::new(models),
    }
}

// ---------------------------------------------------------------------------
// Documents d'instance
// ---------------------------------------------------------------------------

/// Schémas d'un document d'instance (`xsi:schemaLocation`,
/// `xsi:noNamespaceSchemaLocation`) et leurs dépendances.
pub(crate) fn instance_models(
    context: &mut HoverContext<'_>,
    uri: &str,
    document: &Document<'_>,
) -> LoadedModels {
    let catalogs = context.catalogs;
    let roots: Vec<PathBuf> = match resolve_schema_locations_with(
        schema_resolution_source(document.source),
        uri_to_path(uri),
        &|request| catalogs.resolve_schema(request),
    ) {
        Ok(references) => references
            .into_iter()
            .map(|reference| reference.path)
            .collect(),
        // Document en cours de saisie : lecture tolérante des attributs xsi.
        Err(_) => document.schema_locations(&uri_to_path(uri), catalogs),
    };
    let roots = if roots.is_empty() {
        context.associated_schemas.clone()
    } else {
        roots
    };
    load_models(context, Vec::new(), roots)
}

fn instance_hover(
    context: &mut HoverContext<'_>,
    uri: &str,
    document: &Document<'_>,
    target: &Target,
) -> Option<(String, Range<usize>)> {
    let models = instance_models(context, uri, document);
    let source = document.source;
    match target {
        Target::ElementName { element, name } => {
            let path = document.instance_path(*element);
            let title = format!(
                "**Élément** {}",
                code(&format!("<{}>", &source[name.clone()]))
            );
            let markdown = match models.set.resolve_element_path(&path) {
                Some(resolved) => {
                    let element_type = resolved.element_type;
                    render_element(
                        &models,
                        &title,
                        resolved.particle,
                        resolved.declaration,
                        element_type,
                    )
                }
                None => fallback_element(&title, path.last()?.namespace.as_deref()),
            };
            Some((markdown, name.clone()))
        }
        Target::AttributeName { element, attribute } => {
            let attribute = document.attribute(*element, *attribute);
            let name = attribute.name(source);
            if name == "xmlns" || name.starts_with("xmlns:") {
                let value = attribute.value(source).unwrap_or_default();
                let markdown = format!(
                    "**Déclaration d'espace de noms** {}\n\n{}",
                    code(name),
                    if value.is_empty() {
                        "Aucun espace de noms".to_owned()
                    } else {
                        code(value)
                    }
                );
                return Some((markdown, attribute.name.clone()));
            }
            let title = format!("**Attribut** {}", code(name));
            let markdown = match resolve_instance_attribute(&models.set, document, *element, name) {
                Some(resolved) => render_attribute(&models, &title, resolved.0, resolved.1),
                None => {
                    let (namespace, _) = attribute_namespace(document, *element, name);
                    fallback_element(&title, namespace)
                }
            };
            Some((markdown, attribute.name.clone()))
        }
        Target::AttributeValue { element, attribute } => {
            let attribute = document.attribute(*element, *attribute);
            let name = attribute.name(source);
            let (_, declaration) =
                resolve_instance_attribute(&models.set, document, *element, name)?;
            let value_type = models.set.attribute_type(declaration)?;
            let range = attribute.value.clone()?;
            let markdown = render_value(&models, &source[range.clone()], value_type);
            Some((markdown, range))
        }
        Target::Text { element, range } => {
            let resolved = models
                .set
                .resolve_element_path(&document.instance_path(*element))?;
            let element_type = resolved.element_type?;
            if element_type
                .definition
                .is_some_and(|definition| definition.complex && !definition.simple_content)
            {
                return None;
            }
            let markdown = render_value(&models, &source[range.clone()], element_type);
            Some((markdown, range.clone()))
        }
    }
}

pub(crate) fn attribute_namespace<'a>(
    document: &Document<'a>,
    element: usize,
    name: &'a str,
) -> (Option<&'a str>, &'a str) {
    match name.split_once(':') {
        Some((prefix, local)) => (document.namespace(element, Some(prefix)), local),
        // Un attribut non préfixé n'appartient à aucun espace de noms.
        None => (None, name),
    }
}

fn resolve_instance_attribute<'m>(
    set: &'m XsdModelSet,
    document: &Document<'_>,
    element: usize,
    name: &str,
) -> Option<(Located<'m, XsdAttributeDecl>, Located<'m, XsdAttributeDecl>)> {
    let (namespace, local) = attribute_namespace(document, element, name);
    let resolved = set.resolve_element_path(&document.instance_path(element));
    let attribute = set.resolve_attribute(resolved.as_ref(), namespace, local)?;
    Some((attribute.usage, attribute.declaration))
}

fn fallback_element(title: &str, namespace: Option<&str>) -> String {
    let mut markdown = title.to_owned();
    if let Some(namespace) = namespace {
        markdown.push_str(&format!("\n\nEspace de noms : {}", code(namespace)));
    }
    markdown
}

// ---------------------------------------------------------------------------
// Documents XSD
// ---------------------------------------------------------------------------

/// Espace de symboles d'une référence XSD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComponentKind {
    Element,
    Attribute,
    Type,
    Group,
    AttributeGroup,
}

fn schema_hover(
    context: &mut HoverContext<'_>,
    uri: &str,
    document: &Document<'_>,
    target: &Target,
    offset: usize,
) -> Option<(String, Range<usize>)> {
    let Target::AttributeValue { element, attribute } = target else {
        return None;
    };
    let (element, attribute) = (*element, document.attribute(*element, *attribute));
    let source = document.source;
    let (_, element_local) = document.split(element, document.element_name(element));
    if !document.is_xsd(element, element_local) {
        return None;
    }
    let is_global = document.tree.elements()[element]
        .parent
        .is_some_and(|parent| document.is_xsd(parent, "schema"));
    let kind = match (element_local, attribute.name(source)) {
        ("element", "ref" | "substitutionGroup") => ComponentKind::Element,
        ("element" | "attribute", "type")
        | ("restriction" | "extension", "base")
        | ("list", "itemType")
        | ("union", "memberTypes") => ComponentKind::Type,
        ("attribute", "ref") => ComponentKind::Attribute,
        ("group", "ref") => ComponentKind::Group,
        ("attributeGroup", "ref") => ComponentKind::AttributeGroup,
        ("element", "name") if is_global => ComponentKind::Element,
        ("attribute", "name") if is_global => ComponentKind::Attribute,
        ("complexType" | "simpleType", "name") if is_global => ComponentKind::Type,
        ("group", "name") if is_global => ComponentKind::Group,
        ("attributeGroup", "name") if is_global => ComponentKind::AttributeGroup,
        _ => return None,
    };
    let value = attribute.value.clone()?;
    let range = token_at(source, value, offset)?;
    let token = &source[range.clone()];
    let (namespace, local) = if attribute.name(source) == "name" {
        let root = document
            .tree
            .elements()
            .iter()
            .position(|element| element.parent.is_none())?;
        (
            document
                .attribute_value(root, "targetNamespace")
                .filter(|namespace| !namespace.is_empty()),
            token,
        )
    } else {
        document.split(element, token)
    };

    if kind == ComponentKind::Type && namespace == Some(XSD_NAMESPACE) {
        let markdown = format!(
            "**Type prédéfini** {}\n\nType prédéfini de XML Schema ({}).",
            code(token),
            code(XSD_NAMESPACE)
        );
        return Some((markdown, range));
    }

    let path = uri_to_path(uri);
    let model = Arc::new(parse_xsd_model(source).ok()?);
    let dependencies = dependency_paths(source, &path, context.catalogs);
    let models = load_models(context, vec![(path, model)], dependencies);
    let set = &models.set;
    let title = |label: &str| format!("**{label}** {}", code(token));
    let markdown = match kind {
        ComponentKind::Element => {
            let declaration = set.global_element(namespace, local)?;
            render_element(
                &models,
                &title("Élément"),
                declaration,
                declaration,
                set.element_type(declaration),
            )
        }
        ComponentKind::Attribute => {
            let declaration = set.global_attribute(namespace, local)?;
            render_attribute(&models, &title("Attribut"), declaration, declaration)
        }
        ComponentKind::Type => {
            let found = set.global_type(namespace, local)?;
            let label = if found.item.complex {
                "Type complexe"
            } else {
                "Type simple"
            };
            render_type(&models, &title(label), found)
        }
        ComponentKind::Group => {
            let found = set.group(namespace, local)?;
            render_component(
                &models,
                &title("Groupe"),
                found.item.namespace.as_deref(),
                found.item.documentation.as_deref(),
                found.schema,
            )
        }
        ComponentKind::AttributeGroup => {
            let found = set.attribute_group(namespace, local)?;
            render_component(
                &models,
                &title("Groupe d'attributs"),
                found.item.namespace.as_deref(),
                found.item.documentation.as_deref(),
                found.schema,
            )
        }
    };
    Some((markdown, range))
}

/// Jeton (nom qualifié) contenant `offset` dans une valeur d'attribut
/// éventuellement composée de plusieurs noms séparés par des espaces.
fn token_at(source: &str, value: Range<usize>, offset: usize) -> Option<Range<usize>> {
    let text = &source[value.clone()];
    let mut start = None;
    for (index, character) in text
        .char_indices()
        .chain(std::iter::once((text.len(), ' ')))
    {
        if character.is_ascii_whitespace() {
            if let Some(token_start) = start.take() {
                let range = value.start + token_start..value.start + index;
                if range.start <= offset && offset <= range.end {
                    return Some(range);
                }
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Rendu Markdown
// ---------------------------------------------------------------------------

/// Échappe le texte libre (documentation) pour le Markdown.
fn escape_markdown(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if matches!(
            character,
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|' | '~'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Span de code Markdown, robuste aux accents graves.
fn code(text: &str) -> String {
    if text.contains('`') {
        format!("`` {text} ``")
    } else {
        format!("`{text}`")
    }
}

fn source_line(models: &LoadedModels, schema: usize) -> Option<String> {
    let path = models.paths.get(schema)?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    Some(format!(
        "Source : [{}]({})",
        escape_markdown(&name),
        path_to_uri(path)
    ))
}

/// Assemble titre, propriétés, documentation et source.
fn assemble(
    title: &str,
    properties: &[String],
    documentation: Option<&str>,
    source: Option<String>,
) -> String {
    let mut sections = vec![title.to_owned()];
    if !properties.is_empty() {
        sections.push(
            properties
                .iter()
                .map(|property| format!("- {property}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    if let Some(documentation) = documentation.filter(|text| !text.is_empty()) {
        sections.push(escape_markdown(documentation));
    }
    sections.extend(source);
    sections.join("\n\n")
}

/// Description d'un type : nom (ou « anonyme ») et dérivation.
fn describe_type(reference: XsdTypeRef<'_>) -> String {
    let name = reference
        .name
        .map(|name| code(&name.display()))
        .or_else(|| {
            reference
                .definition
                .and_then(|definition| definition.name.as_deref())
                .map(code)
        });
    let Some(definition) = reference.definition else {
        return name.unwrap_or_else(|| "inconnu".to_owned());
    };
    let name = name.unwrap_or_else(|| {
        if definition.complex {
            "complexe anonyme".to_owned()
        } else {
            "simple anonyme".to_owned()
        }
    });
    match derivation(definition) {
        Some(derivation) => format!("{name} ({derivation})"),
        None => name,
    }
}

fn derivation(definition: &xsd_core::model::XsdTypeDef) -> Option<String> {
    let base = || {
        definition
            .base
            .as_ref()
            .map(|base| code(&base.display()))
            .unwrap_or_else(|| "type anonyme".to_owned())
    };
    Some(match definition.derivation? {
        XsdDerivation::Restriction => format!("restriction de {}", base()),
        XsdDerivation::Extension => format!("extension de {}", base()),
        XsdDerivation::List => format!(
            "liste de {}",
            definition
                .item_type
                .as_ref()
                .map(|item| code(&item.display()))
                .unwrap_or_else(|| "type anonyme".to_owned())
        ),
        XsdDerivation::Union => format!(
            "union de {}",
            definition
                .member_types
                .iter()
                .map(|member| code(&member.display()))
                .chain(
                    definition
                        .inline_types
                        .iter()
                        .map(|_| "type anonyme".to_owned())
                )
                .collect::<Vec<_>>()
                .join(", ")
        ),
    })
}

fn render_element(
    models: &LoadedModels,
    title: &str,
    particle: Located<'_, XsdElementDecl>,
    declaration: Located<'_, XsdElementDecl>,
    element_type: Option<XsdTypeRef<'_>>,
) -> String {
    let item = declaration.item;
    let mut properties = Vec::new();
    if let Some(namespace) = &item.namespace {
        properties.push(format!("Espace de noms : {}", code(namespace)));
    }
    if let Some(element_type) = element_type {
        properties.push(format!("Type : {}", describe_type(element_type)));
    }
    if !particle.item.global {
        let max = particle
            .item
            .max_occurs
            .map_or_else(|| "*".to_owned(), |max| max.to_string());
        properties.push(format!(
            "Cardinalité : {}",
            code(&format!("{}..{max}", particle.item.min_occurs))
        ));
    }
    if let Some(default) = &item.default {
        properties.push(format!("Valeur par défaut : {}", code(default)));
    }
    if let Some(fixed) = &item.fixed {
        properties.push(format!("Valeur fixe : {}", code(fixed)));
    }
    if !item.substitution_groups.is_empty() {
        properties.push(format!(
            "Groupe de substitution : {}",
            item.substitution_groups
                .iter()
                .map(|head| code(&head.display()))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if item.is_abstract {
        properties.push("Abstrait".to_owned());
    }
    if item.nillable {
        properties.push("Accepte `xsi:nil`".to_owned());
    }
    let documentation = particle
        .item
        .documentation
        .as_deref()
        .or(item.documentation.as_deref())
        .or_else(|| {
            element_type
                .and_then(|reference| reference.definition)
                .and_then(|definition| definition.documentation.as_deref())
        });
    assemble(
        title,
        &properties,
        documentation,
        source_line(models, declaration.schema),
    )
}

fn render_attribute(
    models: &LoadedModels,
    title: &str,
    usage: Located<'_, XsdAttributeDecl>,
    declaration: Located<'_, XsdAttributeDecl>,
) -> String {
    let item = declaration.item;
    let attribute_type = models.set.attribute_type(declaration);
    let mut properties = Vec::new();
    if let Some(namespace) = &item.namespace {
        properties.push(format!("Espace de noms : {}", code(namespace)));
    }
    if let Some(attribute_type) = attribute_type {
        properties.push(format!("Type : {}", describe_type(attribute_type)));
    }
    if !usage.item.global || usage.item.reference.is_some() {
        properties.push(format!(
            "Utilisation : {}",
            match usage.item.usage {
                XsdUse::Required => "obligatoire",
                XsdUse::Optional => "facultative",
                XsdUse::Prohibited => "interdite",
            }
        ));
    }
    if let Some(default) = usage.item.default.as_ref().or(item.default.as_ref()) {
        properties.push(format!("Valeur par défaut : {}", code(default)));
    }
    if let Some(fixed) = usage.item.fixed.as_ref().or(item.fixed.as_ref()) {
        properties.push(format!("Valeur fixe : {}", code(fixed)));
    }
    if let Some(attribute_type) = attribute_type {
        properties.extend(facet_lines(models, attribute_type));
    }
    let documentation = usage
        .item
        .documentation
        .as_deref()
        .or(item.documentation.as_deref())
        .or_else(|| {
            attribute_type
                .and_then(|reference| reference.definition)
                .and_then(|definition| definition.documentation.as_deref())
        });
    assemble(
        title,
        &properties,
        documentation,
        source_line(models, declaration.schema),
    )
}

fn render_type(
    models: &LoadedModels,
    title: &str,
    found: Located<'_, xsd_core::model::XsdTypeDef>,
) -> String {
    let reference = XsdTypeRef {
        schema: found.schema,
        name: None,
        definition: Some(found.item),
    };
    let mut properties = Vec::new();
    if let Some(namespace) = &found.item.namespace {
        properties.push(format!("Espace de noms : {}", code(namespace)));
    }
    if let Some(derivation) = derivation(found.item) {
        properties.push(format!("Dérivation : {derivation}"));
    }
    if found.item.mixed {
        properties.push("Contenu mixte".to_owned());
    }
    properties.extend(facet_lines(models, reference));
    assemble(
        title,
        &properties,
        found.item.documentation.as_deref(),
        source_line(models, found.schema),
    )
}

fn render_component(
    models: &LoadedModels,
    title: &str,
    namespace: Option<&str>,
    documentation: Option<&str>,
    schema: usize,
) -> String {
    let properties = namespace
        .map(|namespace| vec![format!("Espace de noms : {}", code(namespace))])
        .unwrap_or_default();
    assemble(
        title,
        &properties,
        documentation,
        source_line(models, schema),
    )
}

/// Résumé des facettes d'un type simple (ou à contenu simple).
fn facet_lines(models: &LoadedModels, reference: XsdTypeRef<'_>) -> Vec<String> {
    if reference
        .definition
        .is_some_and(|definition| definition.complex && !definition.simple_content)
    {
        return Vec::new();
    }
    let info = models.set.simple_type_info(reference);
    let facets = &info.facets;
    let mut lines = Vec::new();
    if !facets.enumerations.is_empty() {
        let mut values = facets
            .enumerations
            .iter()
            .take(MAX_LISTED_VALUES)
            .map(|enumeration| code(&enumeration.value))
            .collect::<Vec<_>>();
        if facets.enumerations.len() > MAX_LISTED_VALUES {
            values.push(format!("… ({} valeurs)", facets.enumerations.len()));
        }
        lines.push(format!("Valeurs autorisées : {}", values.join(", ")));
    }
    for pattern in &facets.patterns {
        lines.push(format!("Motif : {}", code(pattern)));
    }
    let bounds = [
        ("Longueur", &facets.length),
        ("Longueur minimale", &facets.min_length),
        ("Longueur maximale", &facets.max_length),
        ("Minimum (inclus)", &facets.min_inclusive),
        ("Minimum (exclu)", &facets.min_exclusive),
        ("Maximum (inclus)", &facets.max_inclusive),
        ("Maximum (exclu)", &facets.max_exclusive),
        ("Nombre total de chiffres", &facets.total_digits),
        ("Chiffres après la virgule", &facets.fraction_digits),
        ("Espaces", &facets.white_space),
    ];
    for (label, value) in bounds {
        if let Some(value) = value {
            lines.push(format!("{label} : {}", code(value)));
        }
    }
    if let Some(item_type) = &info.item_type {
        lines.push(format!("Liste de {}", code(item_type)));
    }
    if !info.member_types.is_empty() {
        lines.push(format!(
            "Union de {}",
            info.member_types
                .iter()
                .map(|member| code(member))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let displayed = reference.name.map(|name| name.display());
    if let Some(builtin) = &info.builtin
        && displayed.as_deref() != Some(builtin.display().as_str())
    {
        lines.push(format!("Type prédéfini : {}", code(&builtin.display())));
    }
    lines
}

fn render_value(models: &LoadedModels, value: &str, value_type: XsdTypeRef<'_>) -> String {
    let trimmed = value.trim();
    let shown = if trimmed.chars().count() > MAX_VALUE_CHARS {
        format!(
            "{}…",
            trimmed
                .chars()
                .take(MAX_VALUE_CHARS - 1)
                .collect::<String>()
        )
    } else {
        trimmed.to_owned()
    };
    let title = format!("**Valeur** {}", code(&shown));
    let mut properties = vec![format!("Type : {}", describe_type(value_type))];
    properties.extend(facet_lines(models, value_type));
    let documentation = models
        .set
        .enumeration(value_type, trimmed)
        .and_then(|enumeration| enumeration.documentation.as_deref());
    let schema = value_type.definition.map(|_| value_type.schema);
    assemble(
        &title,
        &properties,
        documentation,
        schema.and_then(|schema| source_line(models, schema)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:lib="urn:lib" targetNamespace="urn:lib" elementFormDefault="qualified">
  <xs:include schemaLocation="types.xsd"/>
  <xs:element name="library">
    <xs:annotation><xs:documentation>A library of *books*.</xs:documentation></xs:annotation>
    <xs:complexType><xs:sequence>
      <xs:element name="title" type="xs:string"><xs:annotation><xs:documentation>Library name.</xs:documentation></xs:annotation></xs:element>
      <xs:element name="book" type="lib:Book" minOccurs="0" maxOccurs="unbounded"/>
    </xs:sequence></xs:complexType>
  </xs:element>
  <xs:element name="title"><xs:annotation><xs:documentation>Global title.</xs:documentation></xs:annotation></xs:element>
</xs:schema>"#;

    const TYPES_XSD: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:lib="urn:lib" targetNamespace="urn:lib" elementFormDefault="qualified">
  <xs:complexType name="Item">
    <xs:annotation><xs:documentation>Base item.</xs:documentation></xs:annotation>
    <xs:attribute name="id" type="xs:ID" use="required"><xs:annotation><xs:documentation>Unique identifier.</xs:documentation></xs:annotation></xs:attribute>
  </xs:complexType>
  <xs:complexType name="Book">
    <xs:annotation><xs:documentation xml:lang="fr">Un livre.</xs:documentation><xs:documentation xml:lang="en">A book.</xs:documentation></xs:annotation>
    <xs:complexContent><xs:extension base="lib:Item">
      <xs:sequence>
        <xs:element name="title" type="lib:Title"><xs:annotation><xs:documentation>Book title.</xs:documentation></xs:annotation></xs:element>
        <xs:element name="isbn" type="lib:Isbn"/>
      </xs:sequence>
      <xs:attribute name="format" type="lib:Format" default="paper"/>
    </xs:extension></xs:complexContent>
  </xs:complexType>
  <xs:simpleType name="Format">
    <xs:annotation><xs:documentation>Publication format.</xs:documentation></xs:annotation>
    <xs:restriction base="xs:string">
      <xs:enumeration value="paper"><xs:annotation><xs:documentation>Printed edition.</xs:documentation></xs:annotation></xs:enumeration>
      <xs:enumeration value="ebook"/>
    </xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="Isbn"><xs:restriction base="xs:string"><xs:pattern value="[0-9]{13}"/><xs:length value="13"/></xs:restriction></xs:simpleType>
  <xs:simpleType name="Title"><xs:restriction base="xs:string"><xs:minLength value="1"/><xs:maxLength value="200"/></xs:restriction></xs:simpleType>
</xs:schema>"#;

    const INSTANCE: &str = "<l:library xmlns:l=\"urn:lib\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:schemaLocation=\"urn:lib main.xsd\">\r\n  <l:title>Città 📚</l:title>\r\n  <l:book id=\"b1\" format=\"paper\">\r\n    <l:title>Dune</l:title>\r\n    <l:isbn>9780441013593</l:isbn>\r\n  </l:book>\r\n</l:library>";

    struct Fixture {
        directory: PathBuf,
        documents: HashMap<String, String>,
        cache: ModelCache,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let directory =
                std::env::temp_dir().join(format!("xml-lsp-hover-{}-{name}", std::process::id()));
            fs::create_dir_all(&directory).expect("directory should be created");
            fs::write(directory.join("main.xsd"), MAIN_XSD).expect("schema should be written");
            fs::write(directory.join("types.xsd"), TYPES_XSD).expect("schema should be written");
            Self {
                directory,
                documents: HashMap::new(),
                cache: ModelCache::new(),
            }
        }

        fn uri(&self, file: &str) -> String {
            path_to_uri(&self.directory.join(file))
        }

        fn open(&mut self, file: &str, source: &str) -> String {
            let uri = self.uri(file);
            self.documents.insert(uri.clone(), source.to_owned());
            uri
        }

        fn hover(&mut self, uri: &str, offset: usize) -> Option<Value> {
            let source = self.documents[uri].clone();
            let mut context = HoverContext {
                documents: &self.documents,
                cache: &mut self.cache,
                associated_schemas: Vec::new(),
                catalogs: &crate::catalog::Catalogs::default(),
            };
            hover(&mut context, uri, &source, offset)
        }

        fn markdown(&mut self, uri: &str, offset: usize) -> String {
            let hover = self.hover(uri, offset).expect("hover expected");
            assert_eq!(hover["contents"]["kind"], "markdown");
            hover["contents"]["value"].as_str().unwrap().to_owned()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    fn at(source: &str, needle: &str, nth: usize) -> usize {
        source
            .match_indices(needle)
            .nth(nth)
            .map(|(index, _)| index)
            .expect("needle should exist")
    }

    #[test]
    fn documents_elements_resolved_in_their_context() {
        let mut fixture = Fixture::new("elements");
        let uri = fixture.open("doc.xml", INSTANCE);
        let main_uri = fixture.uri("main.xsd");
        let types_uri = fixture.uri("types.xsd");

        let hover = fixture.hover(&uri, at(INSTANCE, "library", 0) + 2).unwrap();
        assert_eq!(
            hover["range"],
            json!({"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 10}})
        );
        assert_eq!(
            hover["contents"]["value"],
            format!(
                "**Élément** `<l:library>`\n\n- Espace de noms : `urn:lib`\n- Type : complexe anonyme\n\nA library of \\*books\\*.\n\nSource : [main.xsd]({main_uri})"
            )
        );

        // Déclaration locale de <library>, pas la déclaration globale homonyme.
        let local = fixture.markdown(&uri, at(INSTANCE, "l:title", 0));
        assert!(local.contains("- Type : `xs:string`"), "{local}");
        assert!(local.contains("- Cardinalité : `1..1`"), "{local}");
        assert!(local.contains("Library name."), "{local}");
        assert!(!local.contains("Global title."), "{local}");

        // Déclaration locale du type hérité, dans le schéma inclus.
        let nested = fixture.markdown(&uri, at(INSTANCE, "l:title", 2) + 4);
        assert!(
            nested.contains("- Type : `lib:Title` (restriction de `xs:string`)"),
            "{nested}"
        );
        assert!(nested.contains("Book title."), "{nested}");
        assert!(nested.contains(&format!("Source : [types.xsd]({types_uri})")));

        // Balise fermante : documentation du type à défaut de celle de l'élément.
        let book = fixture
            .hover(&uri, at(INSTANCE, "</l:book", 0) + 3)
            .unwrap();
        assert_eq!(
            book["range"],
            json!({"start": {"line": 5, "character": 4}, "end": {"line": 5, "character": 10}})
        );
        let book = book["contents"]["value"].as_str().unwrap();
        assert!(
            book.contains("- Type : `lib:Book` (extension de `lib:Item`)"),
            "{book}"
        );
        assert!(book.contains("- Cardinalité : `0..*`"), "{book}");
        assert!(book.contains("\n\nA book.\n\n"), "{book}");
        assert!(!book.contains("Un livre"), "{book}");
    }

    #[test]
    fn documents_attributes_and_namespace_declarations() {
        let mut fixture = Fixture::new("attributes");
        let uri = fixture.open("doc.xml", INSTANCE);
        let types_uri = fixture.uri("types.xsd");

        let id = fixture.hover(&uri, at(INSTANCE, "id=", 0) + 1).unwrap();
        assert_eq!(
            id["range"],
            json!({"start": {"line": 2, "character": 10}, "end": {"line": 2, "character": 12}})
        );
        assert_eq!(
            id["contents"]["value"],
            format!(
                "**Attribut** `id`\n\n- Type : `xs:ID`\n- Utilisation : obligatoire\n\nUnique identifier.\n\nSource : [types.xsd]({types_uri})"
            )
        );

        let format = fixture.markdown(&uri, at(INSTANCE, "format=", 0));
        assert!(format.contains("- Type : `lib:Format` (restriction de `xs:string`)"));
        assert!(format.contains("- Utilisation : facultative"), "{format}");
        assert!(format.contains("- Valeur par défaut : `paper`"), "{format}");
        assert!(format.contains("- Valeurs autorisées : `paper`, `ebook`"));
        assert!(format.contains("Publication format."), "{format}");

        let namespace = fixture.markdown(&uri, at(INSTANCE, "xmlns:l", 0) + 6);
        assert_eq!(
            namespace,
            "**Déclaration d'espace de noms** `xmlns:l`\n\n`urn:lib`"
        );
    }

    #[test]
    fn documents_enumeration_values_and_simple_content_facets() {
        let mut fixture = Fixture::new("values");
        let uri = fixture.open("doc.xml", INSTANCE);

        let value = fixture.hover(&uri, at(INSTANCE, "paper", 0) + 1).unwrap();
        assert_eq!(
            value["range"],
            json!({"start": {"line": 2, "character": 26}, "end": {"line": 2, "character": 31}})
        );
        let value = value["contents"]["value"].as_str().unwrap();
        assert!(value.starts_with("**Valeur** `paper`\n\n"), "{value}");
        assert!(value.contains("- Valeurs autorisées : `paper`, `ebook`"));
        assert!(value.contains("Printed edition."), "{value}");

        let isbn = fixture.markdown(&uri, at(INSTANCE, "9780", 0) + 5);
        assert!(isbn.contains("- Type : `lib:Isbn` (restriction de `xs:string`)"));
        assert!(isbn.contains("- Motif : `[0-9]{13}`"), "{isbn}");
        assert!(isbn.contains("- Longueur : `13`"), "{isbn}");
        assert!(isbn.contains("- Type prédéfini : `xs:string`"), "{isbn}");

        let title = fixture.markdown(&uri, at(INSTANCE, "Dune", 0));
        assert!(title.contains("- Longueur minimale : `1`"), "{title}");
        assert!(title.contains("- Longueur maximale : `200`"), "{title}");

        // Positions UTF-16 (emoji hors plan multilingue de base) et CRLF.
        let text = fixture.hover(&uri, at(INSTANCE, "Città", 0) + 1).unwrap();
        assert_eq!(
            text["range"],
            json!({"start": {"line": 1, "character": 11}, "end": {"line": 1, "character": 19}})
        );
        assert!(
            text["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("- Type : `xs:string`")
        );

        // Espaces entre éléments, contenu complexe : pas de survol.
        assert!(
            fixture
                .hover(&uri, at(INSTANCE, "\r\n  <l:book", 0) + 2)
                .is_none()
        );
    }

    #[test]
    fn documents_xsd_references_across_included_schemas() {
        let mut fixture = Fixture::new("schemas");
        let main_uri = fixture.open("main.xsd", MAIN_XSD);
        let types_uri = fixture.uri("types.xsd");

        let book = fixture
            .hover(&main_uri, at(MAIN_XSD, "lib:Book", 0) + 5)
            .unwrap();
        assert_eq!(
            book["contents"]["value"],
            format!(
                "**Type complexe** `lib:Book`\n\n- Espace de noms : `urn:lib`\n- Dérivation : extension de `lib:Item`\n\nA book.\n\nSource : [types.xsd]({types_uri})"
            )
        );
        assert_eq!(book["range"]["start"], json!({"line": 6, "character": 36}));

        let builtin = fixture.markdown(&main_uri, at(MAIN_XSD, "xs:string", 0));
        assert!(
            builtin.starts_with("**Type prédéfini** `xs:string`"),
            "{builtin}"
        );

        let global = fixture.markdown(&main_uri, at(MAIN_XSD, "\"title\"", 1) + 1);
        assert!(global.contains("Global title."), "{global}");
        // Déclaration locale : pas de composant global à documenter.
        assert!(
            fixture
                .hover(&main_uri, at(MAIN_XSD, "\"title\"", 0) + 1)
                .is_none_or(|hover| !hover.to_string().contains("Library name"))
        );

        // Un schéma ouvert non enregistré est prioritaire sur le disque.
        let edited = TYPES_XSD.replace(">A book.<", ">An edited book.<");
        let types_uri = fixture.open("types.xsd", &edited);
        let book = fixture.markdown(&main_uri, at(MAIN_XSD, "lib:Book", 0));
        assert!(book.contains("An edited book."), "{book}");

        let item = fixture.markdown(&types_uri, at(&edited, "lib:Item", 0) + 4);
        assert!(item.contains("**Type complexe** `lib:Item`"), "{item}");
        assert!(item.contains("Base item."), "{item}");
        let format = fixture.markdown(&types_uri, at(&edited, "\"Format\"", 0) + 2);
        assert!(format.starts_with("**Type simple** `Format`"), "{format}");
        assert!(format.contains("- Valeurs autorisées : `paper`, `ebook`"));
        assert!(format.contains("Publication format."), "{format}");
    }

    #[test]
    fn resolves_xsd_reference_lists_prefixes_and_groups() {
        let mut fixture = Fixture::new("lists");
        let source = r#"<schema xmlns="http://www.w3.org/2001/XMLSchema" xmlns:t="urn:t" targetNamespace="urn:t">
  <simpleType name="A"><annotation><documentation>Type A</documentation></annotation><restriction base="string"/></simpleType>
  <simpleType name="B"><annotation><documentation>Type B</documentation></annotation><restriction base="int"/></simpleType>
  <simpleType name="AB"><union memberTypes="t:A  t:B"/></simpleType>
  <group name="G"><annotation><documentation>Group G</documentation></annotation><sequence/></group>
  <attributeGroup name="AG"><annotation><documentation>Group AG</documentation></annotation></attributeGroup>
  <attribute name="lang"><annotation><documentation>Lang attribute</documentation></annotation></attribute>
  <complexType name="C"><sequence><group ref="t:G"/></sequence><attributeGroup ref="t:AG"/><attribute ref="t:lang" use="required"/></complexType>
</schema>"#;
        let uri = fixture.open("lists.xsd", source);

        let b = fixture.hover(&uri, at(source, "t:B", 0) + 2).unwrap();
        assert!(b["contents"]["value"].as_str().unwrap().contains("Type B"));
        assert_eq!(
            b["range"],
            json!({"start": {"line": 3, "character": 49}, "end": {"line": 3, "character": 52}})
        );
        assert!(
            fixture
                .markdown(&uri, at(source, "t:A", 0))
                .contains("Type A")
        );
        assert!(
            fixture
                .markdown(&uri, at(source, "\"string\"", 0) + 1)
                .starts_with("**Type prédéfini** `string`")
        );
        let union = fixture.markdown(&uri, at(source, "\"AB\"", 0) + 1);
        assert!(union.contains("- Union de `t:A`, `t:B`"), "{union}");
        assert!(
            fixture
                .markdown(&uri, at(source, "t:G", 0))
                .contains("**Groupe** `t:G`")
        );
        assert!(
            fixture
                .markdown(&uri, at(source, "t:AG", 0))
                .contains("Group AG")
        );
        let lang = fixture.markdown(&uri, at(source, "t:lang", 0));
        assert!(lang.contains("**Attribut** `t:lang`"), "{lang}");
        assert!(lang.contains("Lang attribute"), "{lang}");
        // Hors d'une référence : survol XML minimal.
        assert_eq!(
            fixture.markdown(&uri, at(source, "sequence", 0)),
            "**Élément** `<sequence>`\n\nEspace de noms : `http://www.w3.org/2001/XMLSchema`"
        );
    }

    #[test]
    fn falls_back_without_schema() {
        let mut fixture = Fixture::new("fallback");
        let source = "<a:root xmlns:a=\"urn:x\" b=\"1\" a:c=\"2\">text<child/></a:root>";
        let uri = fixture.open("plain.xml", source);

        assert_eq!(
            fixture.markdown(&uri, 3),
            "**Élément** `<a:root>`\n\nEspace de noms : `urn:x`"
        );
        assert_eq!(
            fixture.markdown(&uri, at(source, "child", 0)),
            "**Élément** `<child>`"
        );
        assert_eq!(
            fixture.markdown(&uri, at(source, "b=", 0)),
            "**Attribut** `b`"
        );
        assert_eq!(
            fixture.markdown(&uri, at(source, "a:c", 0)),
            "**Attribut** `a:c`\n\nEspace de noms : `urn:x`"
        );
        assert!(fixture.hover(&uri, at(source, "\"1\"", 0) + 1).is_none());
        assert!(fixture.hover(&uri, at(source, "text", 0) + 1).is_none());
        assert!(fixture.hover(&uri, 0).is_none());
    }

    #[test]
    fn resolves_default_namespace_xsi_type_and_malformed_documents() {
        let mut fixture = Fixture::new("xsi");
        let schema = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:t="urn:t" targetNamespace="urn:t" elementFormDefault="qualified">
  <xs:element name="shape" type="t:Shape"/>
  <xs:complexType name="Shape"><xs:attribute name="name"/></xs:complexType>
  <xs:complexType name="Circle"><xs:annotation><xs:documentation>A circle.</xs:documentation></xs:annotation><xs:complexContent><xs:extension base="t:Shape">
    <xs:attribute name="radius" type="xs:decimal"><xs:annotation><xs:documentation>Radius &lt;cm&gt;.</xs:documentation></xs:annotation></xs:attribute>
  </xs:extension></xs:complexContent></xs:complexType>
</xs:schema>"#;
        fs::write(fixture.directory.join("shape.xsd"), schema).unwrap();
        let source = "<shape xmlns=\"urn:t\" xmlns:i=\"http://www.w3.org/2001/XMLSchema-instance\" i:schemaLocation=\"urn:t shape.xsd\" i:type=\"Circle\" radius=\"2\"";
        let uri = fixture.open("shape.xml", source);

        let shape = fixture.markdown(&uri, 2);
        assert!(
            shape.contains("- Type : `Circle` (extension de `t:Shape`)"),
            "{shape}"
        );
        assert!(shape.contains("A circle."), "{shape}");
        let radius = fixture.markdown(&uri, at(source, "radius", 0));
        assert!(radius.contains("Radius \\<cm\\>."), "{radius}");
        let value = fixture.markdown(&uri, at(source, "\"2\"", 0) + 1);
        assert!(value.contains("- Type : `xs:decimal`"), "{value}");
    }
}
