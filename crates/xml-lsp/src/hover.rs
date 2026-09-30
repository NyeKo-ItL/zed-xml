//! `textDocument/hover`: XSD documentation on hover, like LemMinX.
//!
//! In an instance document bound to a schema (`xsi:schemaLocation`,
//! `xsi:noNamespaceSchemaLocation`):
//!
//! - element name (start or end tag): declaration resolved in its context
//!   (local declarations of the parent's content model, references,
//!   groups, extensions, substitution groups, `xsi:type`), namespace, type
//!   and base type, cardinality, default or fixed values, `xs:documentation`
//!   documentation and a link to the source schema;
//! - attribute name: declaration, type, usage (`use`), default or fixed
//!   value and documentation;
//! - attribute value or text content of a simple-typed element:
//!   documentation of the enumeration value and a summary of the facets.
//!
//! In an XSD schema, a `type`, `ref`, `base`, `itemType`, `memberTypes` or
//! `substitutionGroup` reference (or the `name` of a global component)
//! shows the documentation of the referenced component, including in
//! included or imported schemas.
//!
//! Without a schema, a minimal hover (name and namespace) is kept. The
//! content is Markdown and the response carries the hovered range.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Value, json};
use xml_core::{
    resource::{MAX_RESOURCE_SIZE, read_text_file},
    tags::{
        XmlAttribute, XmlTagKind, XmlTagTree, qualified_name_parts, resolve_namespace,
        scan_attributes,
    },
};
use xsd_core::model::{
    Located, XSD_NAMESPACE, XsdAttributeDecl, XsdDerivation, XsdElementDecl, XsdInstanceStep,
    XsdModel, XsdModelSet, XsdTypeRef, XsdUse, parse_xsd_model,
};
use xsd_core::{
    MAX_SCHEMA_DOCUMENTS, SchemaLocation, SchemaLocationKind, is_remote_location,
    resolve_schema_dependencies_with, resolve_schema_location, resolve_schema_locations_with,
};

use crate::{
    catalog::Catalogs, path_to_uri, schema_resolution_source, selection::LineIndex, uri_to_path,
};

const XSI_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema-instance";
/// Maximum number of listed enumeration values.
const MAX_LISTED_VALUES: usize = 20;
/// Maximum length of a value copied into the title.
const MAX_VALUE_CHARS: usize = 80;

/// XSD models read from disk, invalidated by modification time, with the
/// dependencies (`xs:include`, `xs:import`) of each schema.
pub type ModelCache = HashMap<PathBuf, (SystemTime, Arc<XsdModel>, Vec<PathBuf>)>;

/// Schemas loaded for a request: `paths[i]` is the source of
/// `set.models()[i]`.
pub(crate) struct LoadedModels {
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) set: XsdModelSet,
}

/// Context shared by hover requests.
pub struct HoverContext<'a> {
    /// Open documents (URI -> content), taking precedence over the disk.
    pub documents: &'a HashMap<String, String>,
    pub cache: &'a mut ModelCache,
    /// Schemas associated with the request's document by
    /// `xml.fileAssociations`, used when it declares none.
    pub associated_schemas: Vec<PathBuf>,
    /// XML catalogs (`xml.catalogs`) consulted to resolve schema
    /// locations.
    pub catalogs: &'a Catalogs,
}

/// Answers `textDocument/hover` for the document `uri` at the cursor `offset`.
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

/// Hovered construct.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    /// Name of a start or end tag of the element.
    ElementName { element: usize, name: Range<usize> },
    /// Name of the attribute `attribute` of the element.
    AttributeName { element: usize, attribute: usize },
    /// Value of the attribute `attribute` of the element.
    AttributeValue { element: usize, attribute: usize },
    /// Text content (surrounding whitespace excluded) of the element.
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
        // Text segment between the children surrounding the cursor.
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

    /// Namespace and local name of a qualified name read in `element`.
    pub(crate) fn split(&self, element: usize, name: &'a str) -> (Option<&'a str>, &'a str) {
        match name.split_once(':') {
            Some((prefix, local)) => (self.namespace(element, Some(prefix)), local),
            None => (self.namespace(element, None), name),
        }
    }

    fn attribute(&self, element: usize, attribute: usize) -> &XmlAttribute {
        &self.attributes[element][attribute]
    }

    /// Unprefixed attribute `name` of the element.
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

    /// Path of the element in the instance, root first.
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

    /// Schemas referenced by `xsi:schemaLocation` and
    /// `xsi:noNamespaceSchemaLocation`, read tolerantly (malformed document
    /// being typed), resolved through the catalogs.
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
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|[namespace, location]| SchemaLocation {
                            kind: SchemaLocationKind::SchemaLocation,
                            namespace: Some(*namespace),
                            location: Some(*location),
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
// Schema loading
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

/// Loads the schemas `roots` and their dependencies, breadth first so that
/// directly referenced schemas take precedence.
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
        if visited.len() >= MAX_SCHEMA_DOCUMENTS {
            break;
        }
        if !visited.insert(path.clone()) {
            continue;
        }
        let loaded = if let Some(source) = open_document(context.documents, &path) {
            parse_xsd_model(source).ok().map(|model| {
                let dependencies = dependency_paths(source, &path, context.catalogs);
                (Arc::new(model), dependencies)
            })
        } else if is_remote_location(&path) {
            None
        } else {
            let modified = fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(UNIX_EPOCH);
            match context.cache.get(&path) {
                Some((cached, model, dependencies)) if *cached == modified => {
                    Some((model.clone(), dependencies.clone()))
                }
                _ => read_text_file(&path, MAX_RESOURCE_SIZE)
                    .ok()
                    .and_then(|source| {
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
// Instance documents
// ---------------------------------------------------------------------------

/// Schemas of an instance document (`xsi:schemaLocation`,
/// `xsi:noNamespaceSchemaLocation`) and their dependencies.
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
        // Document being typed: tolerant reading of the xsi attributes.
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
                "**Element** {}",
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
                    "**Namespace declaration** {}\n\n{}",
                    code(name),
                    if value.is_empty() {
                        "No namespace".to_owned()
                    } else {
                        code(value)
                    }
                );
                return Some((markdown, attribute.name.clone()));
            }
            let title = format!("**Attribute** {}", code(name));
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
        // An unprefixed attribute belongs to no namespace.
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
        markdown.push_str(&format!("\n\nNamespace: {}", code(namespace)));
    }
    markdown
}

// ---------------------------------------------------------------------------
// XSD documents
// ---------------------------------------------------------------------------

/// Symbol space of an XSD reference.
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
            "**Built-in type** {}\n\nXML Schema built-in type ({}).",
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
                &title("Element"),
                declaration,
                declaration,
                set.element_type(declaration),
            )
        }
        ComponentKind::Attribute => {
            let declaration = set.global_attribute(namespace, local)?;
            render_attribute(&models, &title("Attribute"), declaration, declaration)
        }
        ComponentKind::Type => {
            let found = set.global_type(namespace, local)?;
            let label = if found.item.complex {
                "Complex type"
            } else {
                "Simple type"
            };
            render_type(&models, &title(label), found)
        }
        ComponentKind::Group => {
            let found = set.group(namespace, local)?;
            render_component(
                &models,
                &title("Group"),
                found.item.namespace.as_deref(),
                found.item.documentation.as_deref(),
                found.schema,
            )
        }
        ComponentKind::AttributeGroup => {
            let found = set.attribute_group(namespace, local)?;
            render_component(
                &models,
                &title("Attribute group"),
                found.item.namespace.as_deref(),
                found.item.documentation.as_deref(),
                found.schema,
            )
        }
    };
    Some((markdown, range))
}

/// Token (qualified name) containing `offset` in an attribute value possibly
/// made of several space-separated names.
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
// Markdown rendering
// ---------------------------------------------------------------------------

/// Escapes free text (documentation) for Markdown.
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

/// Markdown code span, robust to backticks.
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
        "Source: [{}]({})",
        escape_markdown(&name),
        path_to_uri(path)
    ))
}

/// Assembles title, properties, documentation and source.
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

/// Description of a type: name (or "anonymous") and derivation.
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
        return name.unwrap_or_else(|| "unknown".to_owned());
    };
    let name = name.unwrap_or_else(|| {
        if definition.complex {
            "anonymous complex".to_owned()
        } else {
            "anonymous simple".to_owned()
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
            .unwrap_or_else(|| "anonymous type".to_owned())
    };
    Some(match definition.derivation? {
        XsdDerivation::Restriction => format!("restriction of {}", base()),
        XsdDerivation::Extension => format!("extension of {}", base()),
        XsdDerivation::List => format!(
            "list of {}",
            definition
                .item_type
                .as_ref()
                .map(|item| code(&item.display()))
                .unwrap_or_else(|| "anonymous type".to_owned())
        ),
        XsdDerivation::Union => format!(
            "union of {}",
            definition
                .member_types
                .iter()
                .map(|member| code(&member.display()))
                .chain(
                    definition
                        .inline_types
                        .iter()
                        .map(|_| "anonymous type".to_owned())
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
        properties.push(format!("Namespace: {}", code(namespace)));
    }
    if let Some(element_type) = element_type {
        properties.push(format!("Type: {}", describe_type(element_type)));
    }
    if !particle.item.global {
        let max = particle
            .item
            .max_occurs
            .map_or_else(|| "*".to_owned(), |max| max.to_string());
        properties.push(format!(
            "Cardinality: {}",
            code(&format!("{}..{max}", particle.item.min_occurs))
        ));
    }
    if let Some(default) = &item.default {
        properties.push(format!("Default value: {}", code(default)));
    }
    if let Some(fixed) = &item.fixed {
        properties.push(format!("Fixed value: {}", code(fixed)));
    }
    if !item.substitution_groups.is_empty() {
        properties.push(format!(
            "Substitution group: {}",
            item.substitution_groups
                .iter()
                .map(|head| code(&head.display()))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if item.is_abstract {
        properties.push("Abstract".to_owned());
    }
    if item.nillable {
        properties.push("Accepts `xsi:nil`".to_owned());
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
        properties.push(format!("Namespace: {}", code(namespace)));
    }
    if let Some(attribute_type) = attribute_type {
        properties.push(format!("Type: {}", describe_type(attribute_type)));
    }
    if !usage.item.global || usage.item.reference.is_some() {
        properties.push(format!(
            "Use: {}",
            match usage.item.usage {
                XsdUse::Required => "required",
                XsdUse::Optional => "optional",
                XsdUse::Prohibited => "prohibited",
            }
        ));
    }
    if let Some(default) = usage.item.default.as_ref().or(item.default.as_ref()) {
        properties.push(format!("Default value: {}", code(default)));
    }
    if let Some(fixed) = usage.item.fixed.as_ref().or(item.fixed.as_ref()) {
        properties.push(format!("Fixed value: {}", code(fixed)));
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
        properties.push(format!("Namespace: {}", code(namespace)));
    }
    if let Some(derivation) = derivation(found.item) {
        properties.push(format!("Derivation: {derivation}"));
    }
    if found.item.mixed {
        properties.push("Mixed content".to_owned());
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
        .map(|namespace| vec![format!("Namespace: {}", code(namespace))])
        .unwrap_or_default();
    assemble(
        title,
        &properties,
        documentation,
        source_line(models, schema),
    )
}

/// Summary of the facets of a simple type (or a type with simple content).
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
            values.push(format!("… ({} values)", facets.enumerations.len()));
        }
        lines.push(format!("Allowed values: {}", values.join(", ")));
    }
    for pattern in &facets.patterns {
        lines.push(format!("Pattern: {}", code(pattern)));
    }
    let bounds = [
        ("Length", &facets.length),
        ("Minimum length", &facets.min_length),
        ("Maximum length", &facets.max_length),
        ("Minimum (inclusive)", &facets.min_inclusive),
        ("Minimum (exclusive)", &facets.min_exclusive),
        ("Maximum (inclusive)", &facets.max_inclusive),
        ("Maximum (exclusive)", &facets.max_exclusive),
        ("Total digits", &facets.total_digits),
        ("Fraction digits", &facets.fraction_digits),
        ("Whitespace", &facets.white_space),
    ];
    for (label, value) in bounds {
        if let Some(value) = value {
            lines.push(format!("{label}: {}", code(value)));
        }
    }
    if let Some(item_type) = &info.item_type {
        lines.push(format!("List of {}", code(item_type)));
    }
    if !info.member_types.is_empty() {
        lines.push(format!(
            "Union of {}",
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
        lines.push(format!("Built-in type: {}", code(&builtin.display())));
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
    let title = format!("**Value** {}", code(&shown));
    let mut properties = vec![format!("Type: {}", describe_type(value_type))];
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
mod tests;
