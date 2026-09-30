//! Grammaires DTD, comme LemMinX (Xerces) : analyse des sous-ensembles
//! interne (`<!DOCTYPE r [ ... ]>`) et externe (`SYSTEM`/`PUBLIC`), modèles
//! de contenu et validation d'un document d'instance.
//!
//! - [`parser`] : déclarations `ELEMENT` (EMPTY, ANY, mixte, `children` avec
//!   `, | ? * +`), `ATTLIST` (CDATA, ID, IDREF(S), NMTOKEN(S), ENTITY/
//!   ENTITIES, énumérations, NOTATION ; `#REQUIRED`, `#IMPLIED`, `#FIXED`,
//!   valeur par défaut), `ENTITY` (générales et paramètres, internes et
//!   externes, `NDATA`), `NOTATION`, commentaires (documentation de la
//!   déclaration suivante), développement des entités paramètres (entre les
//!   déclarations et à l'intérieur de celles-ci) et sections conditionnelles
//!   `INCLUDE`/`IGNORE` du sous-ensemble externe. Les ressources externes
//!   sont lues par un [`ExternalLoader`] fourni par l'appelant.
//! - [`content`] : automate de reconnaissance des modèles `children`.
//! - [`validate`] : références d'entités du document et validation
//!   (déclarations, modèles de contenu, attributs, ID/IDREF).
//!
//! Sécurité : le développement des entités est borné
//! ([`MAX_ENTITY_EXPANSION`], [`MAX_PARAMETER_EXPANSION`],
//! [`MAX_ENTITY_DEPTH`]) ; les entités générales ne sont jamais développées
//! en mémoire, seule leur taille est calculée (attaque « billion laughs »).

pub mod content;
mod names;
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
pub use names::{is_name, is_name_char, is_nmtoken};
pub use parser::{
    Doctype, DtdBuilder, ExternalLoader, LoadError, NoLoader, find_doctype, load_document_dtd,
    parse_dtd,
};
pub use validate::{
    EntityReference, InstanceProblem, InstanceProblemKind, check_entity_references,
    entity_reference_at, general_entity_references, validate_instance,
};

/// Entités prédéfinies, toujours disponibles.
pub const PREDEFINED_ENTITIES: [(&str, &str); 5] = [
    ("amp", "&"),
    ("lt", "<"),
    ("gt", ">"),
    ("quot", "\""),
    ("apos", "'"),
];

/// Taille maximale (octets) du développement complet d'une entité générale.
pub const MAX_ENTITY_EXPANSION: usize = 1 << 20;
/// Taille cumulée maximale (octets) des textes de remplacement d'entités
/// paramètres développés pour une grammaire.
pub const MAX_PARAMETER_EXPANSION: usize = 4 << 20;
/// Profondeur maximale d'imbrication des entités et sections
/// conditionnelles.
pub const MAX_ENTITY_DEPTH: usize = 32;
/// Nombre maximal de sources (fichiers, textes de remplacement) d'une
/// grammaire.
pub const MAX_SOURCES: usize = 256;

pub type SourceId = usize;

/// Origine d'un texte analysé.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceKind {
    /// Document analysé directement : document d'instance (sous-ensemble
    /// interne) ou fichier `.dtd` ouvert ; chemin s'il est local.
    Document(Option<PathBuf>),
    /// Sous-ensemble externe ou entité paramètre externe lue sur disque.
    External(PathBuf),
    /// Texte de remplacement d'une entité paramètre interne développée en
    /// `reference`.
    Replacement { entity: String, reference: Location },
}

#[derive(Debug, Clone)]
pub struct DtdSource {
    pub kind: SourceKind,
    pub text: Arc<str>,
    /// Référence qui a introduit la source (identifiant système du
    /// sous-ensemble externe, `%nom;`), `None` pour un document.
    pub reference: Option<Location>,
}

/// Étendue (octets UTF-8) dans une source de la grammaire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub source: SourceId,
    pub range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElementDecl {
    pub name: String,
    /// Nom dans la déclaration.
    pub location: Location,
    /// Déclaration entière (`<!ELEMENT ... >`).
    pub declaration: Location,
    pub content: ContentSpec,
    /// Commentaire qui précède la déclaration.
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
    /// Valeurs permises d'une énumération ou d'un type `NOTATION`.
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
    /// `#FIXED "valeur"` (valeur normalisée).
    Fixed(String),
    /// Valeur par défaut (normalisée).
    Default(String),
}

impl DefaultDecl {
    /// Valeur fixe ou par défaut.
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
    /// Définition au format `<!ATTLIST element nom TYPE DÉFAUT>`.
    pub fn display(&self) -> String {
        format!(
            "<!ATTLIST {} {} {} {}>",
            self.element, self.name, self.attribute_type, self.default
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityValue {
    /// Texte de remplacement (références de caractères et d'entités
    /// paramètres développées, références d'entités générales conservées).
    Internal(String),
    External {
        public: Option<String>,
        system: String,
        /// Notation `NDATA` d'une entité non analysée.
        notation: Option<String>,
        /// Chemin de la source déclarante, base des chemins relatifs.
        base: Option<PathBuf>,
    },
}

/// Résultat du développement (calculé, jamais matérialisé) d'une entité
/// générale.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntityExpansion {
    /// Taille du développement complet, en octets (saturée).
    pub length: usize,
    /// Le développement ne contient que des espaces blancs.
    pub blank: bool,
    /// Le développement contient du balisage (`<`).
    pub markup: bool,
    /// L'entité, ou une entité qu'elle référence, est externe.
    pub external: bool,
    /// Entité non analysée (`NDATA`).
    pub unparsed: bool,
    /// Développement impossible : récursion ou limite dépassée.
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
    /// Développement des entités générales (défaut pour les entités
    /// paramètres).
    pub expansion: EntityExpansion,
}

impl EntityDecl {
    /// Déclaration au format `<!ENTITY ...>`.
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

/// Problème de la grammaire elle-même.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DtdProblemKind {
    /// Erreur de syntaxe d'une déclaration.
    Syntax,
    DuplicateElement,
    DuplicateNotation,
    /// Plus d'un attribut de type ID pour un élément.
    MultipleIdAttributes,
    /// Attribut ID avec une valeur par défaut ou fixe.
    IdAttributeDefault,
    /// Valeur par défaut hors énumération.
    InvalidDefaultValue,
    UndeclaredParameterEntity,
    UndeclaredNotation,
    /// Entité qui se référence elle-même.
    EntityRecursion,
    /// Limite de développement dépassée.
    EntityExpansionLimit,
    /// Section conditionnelle dans le sous-ensemble interne.
    ConditionalSection,
    /// Ressource externe non chargée ; `remote` pour une URL distante.
    ExternalLoad {
        remote: bool,
    },
}

impl DtdProblemKind {
    /// Identifiant stable, publié dans `data.kind`.
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

/// Grammaire DTD : sources analysées, déclarations (la première déclaration
/// d'une entité ou d'un attribut l'emporte, comme en XML) et problèmes.
#[derive(Debug, Clone, Default)]
pub struct Dtd {
    pub sources: Vec<DtdSource>,
    /// Nom de l'élément racine annoncé par `<!DOCTYPE>`.
    pub doctype_name: Option<String>,
    pub elements: Vec<ElementDecl>,
    pub attributes: Vec<AttributeDecl>,
    pub general_entities: Vec<EntityDecl>,
    pub parameter_entities: Vec<EntityDecl>,
    pub notations: Vec<NotationDecl>,
    pub problems: Vec<DtdProblem>,
    /// Des déclarations peuvent manquer (sous-ensemble externe ou entité
    /// paramètre externe non lu).
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

    /// Attributs déclarés pour `element`, dans l'ordre des déclarations.
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

    /// La grammaire déclare au moins un élément : la validation structurelle
    /// du document s'applique (une DTD qui ne déclare que des entités sert
    /// seulement à les définir).
    pub fn declares_elements(&self) -> bool {
        !self.elements.is_empty()
    }

    pub fn source_text(&self, source: SourceId) -> &str {
        self.sources
            .get(source)
            .map_or("", |source| source.text.as_ref())
    }

    /// Chemin local de la source (document ou fichier externe).
    pub fn source_path(&self, source: SourceId) -> Option<&Path> {
        match &self.sources.get(source)?.kind {
            SourceKind::Document(path) => path.as_deref(),
            SourceKind::External(path) => Some(path),
            SourceKind::Replacement { .. } => None,
        }
    }

    /// Ramène `location` dans un texte réel : une étendue d'un texte de
    /// remplacement d'entité paramètre devient la référence `%nom;`.
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

    /// Ramène `location` dans la source `root` en remontant les références
    /// qui ont introduit chaque source (textes de remplacement, fichiers
    /// externes) ; `None` si la source ne descend pas de `root`.
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

    /// Enfants permis dans `parent` (`None` : élément racine) après les
    /// enfants `preceding`, pour la complétion. Si `preceding` ne respecte
    /// déjà pas le modèle, tous les noms du modèle sont proposés.
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

    /// Normalise une valeur d'attribut (XML 1.0 §3.3.3) : références de
    /// caractères et d'entités internes développées (bornées), blancs
    /// remplacés par des espaces ; pour un type autre que CDATA, espaces de
    /// bord supprimés et suites d'espaces réduites. `None` si la valeur
    /// référence une entité externe, non analysée, inconnue, trop grande ou
    /// contenant `<`.
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
                // `\r\n` est un seul saut de ligne.
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
        // Contenu déjà invalide : tous les noms du modèle.
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
