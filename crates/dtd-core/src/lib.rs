//! DTD grammars, like LemMinX (Xerces): parsing of the internal
//! (`<!DOCTYPE r [ ... ]>`) and external (`SYSTEM`/`PUBLIC`) subsets, content
//! models and validation of an instance document.
//!
//! - [`parser`]: `ELEMENT` declarations (EMPTY, ANY, mixed, `children` with
//!   `, | ? * +`), `ATTLIST` (CDATA, ID, IDREF(S), NMTOKEN(S), ENTITY/
//!   ENTITIES, enumerations, NOTATION; `#REQUIRED`, `#IMPLIED`, `#FIXED`,
//!   default value), `ENTITY` (general and parameter, internal and
//!   external, `NDATA`), `NOTATION`, comments (documentation of the
//!   following declaration), parameter entity expansion (between
//!   declarations and inside them) and `INCLUDE`/`IGNORE` conditional
//!   sections of the external subset. External resources
//!   are read by an [`ExternalLoader`] provided by the caller.
//! - [`content`]: recognition automaton of `children` models.
//! - [`validate`]: entity references of the document and validation
//!   (declarations, content models, attributes, ID/IDREF).
//!
//! Security: entity expansion is bounded
//! ([`MAX_ENTITY_EXPANSION`], [`MAX_PARAMETER_EXPANSION`],
//! [`MAX_ENTITY_DEPTH`]); general entities are never expanded in memory,
//! only their size is computed ("billion laughs" attack).

pub mod content;
use xml_core::names;
pub mod parser;
pub mod validate;

use std::{
    collections::HashMap,
    fmt,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

pub use content::{ContentAutomaton, ContentMatcher, ContentParticle, ContentSpec, Occurrence};
pub use parser::{
    Doctype, DtdBuilder, ExternalLoader, LoadError, NoLoader, find_doctype, load_document_dtd,
    parse_dtd,
};
pub use validate::{
    EntityReference, InstanceProblem, InstanceProblemKind, check_entity_references,
    entity_reference_at, general_entity_references, id_links, validate_instance,
};
pub use xml_core::names::{is_name, is_name_char, is_nmtoken};

/// Predefined entities, always available.
pub const PREDEFINED_ENTITIES: [(&str, &str); 5] = [
    ("amp", "&"),
    ("lt", "<"),
    ("gt", ">"),
    ("quot", "\""),
    ("apos", "'"),
];

/// Maximum size (bytes) of the full expansion of a general entity.
pub const MAX_ENTITY_EXPANSION: usize = 1 << 20;
/// Maximum cumulative size (bytes) of the parameter entity replacement
/// texts expanded for a grammar.
pub const MAX_PARAMETER_EXPANSION: usize = 4 << 20;
/// Maximum nesting depth of entities and conditional
/// sections.
pub const MAX_ENTITY_DEPTH: usize = 32;
/// Maximum number of sources (files, replacement texts) of a
/// grammar.
pub const MAX_SOURCES: usize = 256;

pub type SourceId = usize;

/// Origin of a parsed text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceKind {
    /// Directly parsed document: instance document (internal subset) or
    /// open `.dtd` file; its path when local.
    Document(Option<PathBuf>),
    /// External subset or external parameter entity read from disk.
    External(PathBuf),
    /// Replacement text of an internal parameter entity expanded at
    /// `reference`.
    Replacement { entity: String, reference: Location },
}

#[derive(Debug, Clone)]
pub struct DtdSource {
    pub kind: SourceKind,
    pub text: Arc<str>,
    /// Reference that introduced the source (system identifier of the
    /// external subset, `%name;`), `None` for a document.
    pub reference: Option<Location>,
}

/// Range (UTF-8 bytes) in a source of the grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub source: SourceId,
    pub range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementDecl {
    pub name: String,
    /// Name in the declaration.
    pub location: Location,
    /// Whole declaration (`<!ELEMENT ... >`).
    pub declaration: Location,
    pub content: ContentSpec,
    /// Comment preceding the declaration.
    pub documentation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributeType {
    CData,
    Id,
    IdRef,
    IdRefs,
    Entity,
    Entities,
    NmToken,
    NmTokens,
    Notation(Vec<String>),
    Enumeration(Vec<String>),
}

impl AttributeType {
    /// Allowed values of an enumeration or a `NOTATION` type.
    pub fn values(&self) -> Option<&[String]> {
        match self {
            AttributeType::Notation(values) | AttributeType::Enumeration(values) => Some(values),
            _ => None,
        }
    }
}

impl fmt::Display for AttributeType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AttributeType::CData => formatter.write_str("CDATA"),
            AttributeType::Id => formatter.write_str("ID"),
            AttributeType::IdRef => formatter.write_str("IDREF"),
            AttributeType::IdRefs => formatter.write_str("IDREFS"),
            AttributeType::Entity => formatter.write_str("ENTITY"),
            AttributeType::Entities => formatter.write_str("ENTITIES"),
            AttributeType::NmToken => formatter.write_str("NMTOKEN"),
            AttributeType::NmTokens => formatter.write_str("NMTOKENS"),
            AttributeType::Notation(values) => {
                write!(formatter, "NOTATION ({})", values.join(" | "))
            }
            AttributeType::Enumeration(values) => write!(formatter, "({})", values.join(" | ")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultDecl {
    Required,
    Implied,
    /// `#FIXED "value"` (normalized value).
    Fixed(String),
    /// Default value (normalized).
    Default(String),
}

impl DefaultDecl {
    /// Fixed or default value.
    pub fn value(&self) -> Option<&str> {
        match self {
            DefaultDecl::Fixed(value) | DefaultDecl::Default(value) => Some(value),
            DefaultDecl::Required | DefaultDecl::Implied => None,
        }
    }
}

impl fmt::Display for DefaultDecl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DefaultDecl::Required => formatter.write_str("#REQUIRED"),
            DefaultDecl::Implied => formatter.write_str("#IMPLIED"),
            DefaultDecl::Fixed(value) => write!(formatter, "#FIXED {}", quote(value)),
            DefaultDecl::Default(value) => formatter.write_str(&quote(value)),
        }
    }
}

fn quote(value: &str) -> String {
    if value.contains('"') {
        format!("'{value}'")
    } else {
        format!("\"{value}\"")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributeDecl {
    pub element: String,
    pub name: String,
    pub location: Location,
    pub declaration: Location,
    pub attribute_type: AttributeType,
    pub default: DefaultDecl,
    pub documentation: Option<String>,
}

impl AttributeDecl {
    /// Definition in the `<!ATTLIST element name TYPE DEFAULT>` format.
    pub fn display(&self) -> String {
        format!(
            "<!ATTLIST {} {} {} {}>",
            self.element, self.name, self.attribute_type, self.default
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityValue {
    /// Replacement text (character references and parameter entity
    /// references expanded, general entity references kept).
    Internal(String),
    External {
        public: Option<String>,
        system: String,
        /// `NDATA` notation of an unparsed entity.
        notation: Option<String>,
        /// Path of the declaring source, base of relative paths.
        base: Option<PathBuf>,
    },
}

/// Result of the expansion (computed, never materialized) of a general
/// entity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntityExpansion {
    /// Size of the full expansion, in bytes (saturated).
    pub length: usize,
    /// The expansion only contains whitespace.
    pub blank: bool,
    /// The expansion contains markup (`<`).
    pub markup: bool,
    /// The entity, or an entity it references, is external.
    pub external: bool,
    /// Unparsed entity (`NDATA`).
    pub unparsed: bool,
    /// Expansion impossible: recursion or limit exceeded.
    pub error: Option<ExpansionError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpansionError {
    Recursive,
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityDecl {
    pub name: String,
    pub parameter: bool,
    pub location: Location,
    pub declaration: Location,
    pub value: EntityValue,
    pub documentation: Option<String>,
    /// Expansion of general entities (default for parameter
    /// entities).
    pub expansion: EntityExpansion,
}

impl EntityDecl {
    /// Declaration in the `<!ENTITY ...>` format.
    pub fn display(&self) -> String {
        let percent = if self.parameter { "% " } else { "" };
        let value = match &self.value {
            EntityValue::Internal(text) => quote(text),
            EntityValue::External {
                public,
                system,
                notation,
                ..
            } => {
                let mut value = match public {
                    Some(public) => format!("PUBLIC {} {}", quote(public), quote(system)),
                    None => format!("SYSTEM {}", quote(system)),
                };
                if let Some(notation) = notation {
                    value.push_str(&format!(" NDATA {notation}"));
                }
                value
            }
        };
        format!("<!ENTITY {percent}{} {value}>", self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotationDecl {
    pub name: String,
    pub location: Location,
    pub declaration: Location,
    pub public: Option<String>,
    pub system: Option<String>,
    pub documentation: Option<String>,
}

/// Problem of the grammar itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DtdProblemKind {
    /// Syntax error in a declaration.
    Syntax,
    DuplicateElement,
    DuplicateNotation,
    /// More than one ID attribute for an element.
    MultipleIdAttributes,
    /// ID attribute with a default or fixed value.
    IdAttributeDefault,
    /// Default value outside the enumeration.
    InvalidDefaultValue,
    UndeclaredParameterEntity,
    UndeclaredNotation,
    /// Entity referencing itself.
    EntityRecursion,
    /// Expansion limit exceeded.
    EntityExpansionLimit,
    /// Conditional section in the internal subset.
    ConditionalSection,
    /// External resource not loaded; `remote` for a remote URL.
    ExternalLoad {
        remote: bool,
    },
}

impl DtdProblemKind {
    /// Stable identifier, published in `data.kind`.
    pub fn id(&self) -> &'static str {
        match self {
            DtdProblemKind::Syntax => "dtdSyntax",
            DtdProblemKind::DuplicateElement => "duplicateElement",
            DtdProblemKind::DuplicateNotation => "duplicateNotation",
            DtdProblemKind::MultipleIdAttributes => "multipleIdAttributes",
            DtdProblemKind::IdAttributeDefault => "idAttributeDefault",
            DtdProblemKind::InvalidDefaultValue => "invalidDefaultValue",
            DtdProblemKind::UndeclaredParameterEntity => "undeclaredParameterEntity",
            DtdProblemKind::UndeclaredNotation => "undeclaredNotation",
            DtdProblemKind::EntityRecursion => "entityRecursion",
            DtdProblemKind::EntityExpansionLimit => "entityExpansionLimit",
            DtdProblemKind::ConditionalSection => "conditionalSection",
            DtdProblemKind::ExternalLoad { .. } => "externalLoad",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DtdProblem {
    pub kind: DtdProblemKind,
    pub location: Location,
    pub message: String,
}

/// DTD grammar: parsed sources, declarations (the first declaration of an
/// entity or attribute wins, as in XML) and problems.
#[derive(Debug, Clone, Default)]
pub struct Dtd {
    pub sources: Vec<DtdSource>,
    /// Name of the root element announced by `<!DOCTYPE>`.
    pub doctype_name: Option<String>,
    pub elements: Vec<ElementDecl>,
    pub attributes: Vec<AttributeDecl>,
    pub general_entities: Vec<EntityDecl>,
    pub parameter_entities: Vec<EntityDecl>,
    pub notations: Vec<NotationDecl>,
    pub problems: Vec<DtdProblem>,
    /// Declarations may be missing (external subset or external parameter
    /// entity not read).
    pub incomplete: bool,
    element_index: HashMap<String, usize>,
    attribute_index: HashMap<String, Vec<usize>>,
    general_index: HashMap<String, usize>,
    parameter_index: HashMap<String, usize>,
    notation_index: HashMap<String, usize>,
}

impl Dtd {
    pub fn element(&self, name: &str) -> Option<&ElementDecl> {
        self.element_index
            .get(name)
            .map(|&index| &self.elements[index])
    }

    /// Attributes declared for `element`, in declaration order.
    pub fn attributes_of<'a>(&'a self, element: &str) -> impl Iterator<Item = &'a AttributeDecl> {
        self.attribute_index
            .get(element)
            .into_iter()
            .flatten()
            .map(|&index| &self.attributes[index])
    }

    pub fn attribute(&self, element: &str, name: &str) -> Option<&AttributeDecl> {
        self.attributes_of(element)
            .find(|attribute| attribute.name == name)
    }

    pub fn general_entity(&self, name: &str) -> Option<&EntityDecl> {
        self.general_index
            .get(name)
            .map(|&index| &self.general_entities[index])
    }

    pub fn parameter_entity(&self, name: &str) -> Option<&EntityDecl> {
        self.parameter_index
            .get(name)
            .map(|&index| &self.parameter_entities[index])
    }

    pub fn notation(&self, name: &str) -> Option<&NotationDecl> {
        self.notation_index
            .get(name)
            .map(|&index| &self.notations[index])
    }

    /// The grammar declares at least one element: structural validation of
    /// the document applies (a DTD declaring only entities only serves to
    /// define them).
    pub fn declares_elements(&self) -> bool {
        !self.elements.is_empty()
    }

    pub fn source_text(&self, source: SourceId) -> &str {
        self.sources
            .get(source)
            .map_or("", |source| source.text.as_ref())
    }

    /// Local path of the source (document or external file).
    pub fn source_path(&self, source: SourceId) -> Option<&Path> {
        match &self.sources.get(source)?.kind {
            SourceKind::Document(path) => path.as_deref(),
            SourceKind::External(path) => Some(path),
            SourceKind::Replacement { .. } => None,
        }
    }

    /// Maps `location` back into a real text: a range of a parameter entity
    /// replacement text becomes the `%name;` reference.
    pub fn anchor(&self, location: &Location) -> Location {
        let mut location = location.clone();
        for _ in 0..=MAX_SOURCES {
            match self.sources.get(location.source).map(|source| &source.kind) {
                Some(SourceKind::Replacement { reference, .. }) => location = reference.clone(),
                _ => break,
            }
        }
        location
    }

    /// Maps `location` back into the source `root` by walking up the
    /// references that introduced each source (replacement texts, external
    /// files); `None` if the source does not descend from `root`.
    pub fn origin_in(&self, location: &Location, root: SourceId) -> Option<Location> {
        let mut location = location.clone();
        for _ in 0..=self.sources.len() {
            if location.source == root {
                return Some(location);
            }
            location = self.sources.get(location.source)?.reference.clone()?;
        }
        None
    }

    /// Children allowed in `parent` (`None`: root element) after the
    /// `preceding` children, for completion. If `preceding` already breaks
    /// the model, all names of the model are proposed.
    pub fn allowed_children(&self, parent: Option<&str>, preceding: &[&str]) -> Vec<String> {
        let all = || {
            let mut names = self
                .elements
                .iter()
                .map(|element| element.name.clone())
                .collect::<Vec<_>>();
            names.sort();
            names
        };
        let Some(parent) = parent else {
            return match &self.doctype_name {
                Some(name) if self.element(name).is_some() => vec![name.clone()],
                _ => all(),
            };
        };
        let Some(declaration) = self.element(parent) else {
            return all();
        };
        match &declaration.content {
            ContentSpec::Empty => Vec::new(),
            ContentSpec::Any => all(),
            ContentSpec::Mixed(names) => names.clone(),
            ContentSpec::Children(particle) => {
                let automaton = ContentAutomaton::new(particle);
                let mut matcher = automaton.matcher();
                if preceding.iter().all(|name| matcher.feed(name)) {
                    matcher.expected()
                } else {
                    particle.names().into_iter().map(str::to_owned).collect()
                }
            }
        }
    }

    /// Normalizes an attribute value (XML 1.0 §3.3.3): character and
    /// internal entity references expanded (bounded), whitespace replaced
    /// by spaces; for a type other than CDATA, leading and trailing spaces
    /// removed and runs of spaces collapsed. `None` if the value references
    /// an external, unparsed, unknown or too large entity, or one
    /// containing `<`.
    pub fn normalize_attribute_value(&self, raw: &str, cdata: bool) -> Option<String> {
        let mut value = String::new();
        let mut budget = MAX_ENTITY_EXPANSION;
        self.expand_attribute_text(raw, &mut value, &mut budget, 0)?;
        if cdata {
            return Some(value);
        }
        Some(
            value
                .split(' ')
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
        )
    }

    fn expand_attribute_text(
        &self,
        raw: &str,
        value: &mut String,
        budget: &mut usize,
        depth: usize,
    ) -> Option<()> {
        if depth > MAX_ENTITY_DEPTH {
            return None;
        }
        let mut rest = raw;
        while let Some(position) = rest.find(['&', '\t', '\n', '\r']) {
            value.push_str(&rest[..position]);
            let tail = &rest[position..];
            if !tail.starts_with('&') {
                // `\r\n` is a single line break.
                let skip = if tail.starts_with("\r\n") { 2 } else { 1 };
                value.push(' ');
                rest = &tail[skip..];
                continue;
            }
            let end = tail.find(';')?;
            let reference = &tail[1..end];
            rest = &tail[end + 1..];
            if let Some(character) = parser::decode_char_reference(reference) {
                value.push(character);
            } else if let Some((_, text)) = PREDEFINED_ENTITIES
                .iter()
                .find(|(name, _)| *name == reference)
            {
                value.push_str(text);
            } else {
                let entity = self.general_entity(reference)?;
                let EntityValue::Internal(text) = &entity.value else {
                    return None;
                };
                if entity.expansion.error.is_some() || entity.expansion.markup {
                    return None;
                }
                *budget = budget.checked_sub(text.len())?;
                self.expand_attribute_text(text, value, budget, depth + 1)?;
            }
        }
        value.push_str(rest);
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_attribute_values_with_entities() {
        let dtd = parse_dtd(
            "<!ENTITY co \"ACME &amp; Co\">\n<!ENTITY tag \"<b/>\">\n<!ENTITY ext SYSTEM \"x.xml\">",
            None,
            &mut NoLoader,
        );
        assert_eq!(
            dtd.normalize_attribute_value("  a\n&co;\tb ", true)
                .as_deref(),
            Some("  a ACME & Co b ")
        );
        assert_eq!(
            dtd.normalize_attribute_value("  a\r\n  b  &#x41;", false)
                .as_deref(),
            Some("a b A")
        );
        assert_eq!(dtd.normalize_attribute_value("&tag;", true), None);
        assert_eq!(dtd.normalize_attribute_value("&ext;", true), None);
        assert_eq!(dtd.normalize_attribute_value("&unknown;", true), None);
    }

    #[test]
    fn computes_allowed_children_for_completion() {
        let dtd = parse_dtd(
            "<!ELEMENT book (title, chapter+, appendix?)>\n<!ELEMENT title (#PCDATA | em)*>\n<!ELEMENT chapter ANY>\n<!ELEMENT appendix EMPTY>\n<!ELEMENT em (#PCDATA)>",
            None,
            &mut NoLoader,
        );
        assert_eq!(dtd.allowed_children(Some("book"), &[]), vec!["title"]);
        assert_eq!(
            dtd.allowed_children(Some("book"), &["title", "chapter"]),
            vec!["appendix", "chapter"]
        );
        // Content already invalid: all names of the model.
        assert_eq!(
            dtd.allowed_children(Some("book"), &["chapter"]),
            vec!["title", "chapter", "appendix"]
        );
        assert_eq!(dtd.allowed_children(Some("title"), &[]), vec!["em"]);
        assert!(dtd.allowed_children(Some("appendix"), &[]).is_empty());
        assert_eq!(dtd.allowed_children(Some("chapter"), &[]).len(), 5);
        assert_eq!(dtd.allowed_children(None, &[]).len(), 5);
    }
}
