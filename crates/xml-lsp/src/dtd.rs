//! Support DTD, comme LemMinX : grammaire du `<!DOCTYPE>` (sous-ensemble
//! interne et DTD externe `SYSTEM`/`PUBLIC`) et fichiers `.dtd`/`.ent`.
//!
//! - Chargement ([`load`]) : la DTD externe et les entités paramètres
//!   externes sont résolues par les catalogues XML (`xml.catalogs`), puis
//!   relativement à la source déclarante ; seuls des fichiers locaux sont lus
//!   (tampons ouverts d'abord, puis disque avec un cache par date de
//!   modification), jamais une URL `http(s)` (avertissement qui renvoie vers
//!   `xml.catalogs`). Le chargement de la DTD externe est actif par défaut,
//!   comme dans LemMinX.
//! - Diagnostics ([`diagnostics`]) : erreurs de la DTD (`dtd-grammar`),
//!   références d'entités non déclarées ou invalides (`xml-entity`, aussi
//!   sans DTD), validation du document (`dtd-validation`), en plus de la
//!   validation XSD. `xml.validation.disallowDocTypeDecl` désactive le tout
//!   pour un document qui a un `<!DOCTYPE>` ; avec
//!   `xml.validation.resolveExternalEntities`, les entités générales externes
//!   référencées doivent être résolubles (sans quoi elles ne sont jamais
//!   lues).
//! - Complétion ([`completions`]) : éléments permis par le modèle de contenu
//!   du parent, attributs déclarés et valeurs énumérées (ID existants pour
//!   IDREF), entités après `&` ; dans une DTD : mots-clés après `<!`, `#…`,
//!   entités paramètres après `%`, noms d'éléments dans les déclarations.
//! - Survol ([`hover`]) et définition ([`definition`]) : déclaration DTD d'un
//!   élément, d'un attribut, d'une entité (`&nom;`, `%nom;`) ou d'une
//!   notation, avec le commentaire qui la précède comme documentation.
//! - Correctifs ([`code_actions`]) : déclarer une entité manquante, ajouter
//!   un attribut requis, remplacer une valeur hors énumération ou fixe.
//! - Symboles ([`document_symbols`]) d'un fichier `.dtd`.

use std::{
    collections::HashMap,
    fs,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use dtd_core::{
    AttributeType, Doctype, Dtd, DtdProblemKind, EntityValue, ExternalLoader, InstanceProblemKind,
    LoadError, Location, check_entity_references, entity_reference_at, find_doctype,
    general_entity_references, is_name_char, load_document_dtd, parse_dtd, validate_instance,
};
use serde_json::{Value, json};
use xml_core::tags::{XmlTagKind, XmlTagTree, scan_attributes, scan_markup, scan_tags};
use xsd_core::{file_uri_to_path, percent_decode};

use crate::{
    catalog::{Catalogs, target_path},
    code_actions::{Actions, QUICK_FIX},
    links::uri_scheme,
    path_to_uri,
    selection::LineIndex,
    settings::ValidationSettings,
    uri_to_path,
};

/// Taille maximale d'un fichier DTD lu sur disque.
const MAX_DTD_SIZE: u64 = 4 * 1024 * 1024;
/// Code des diagnostics d'une DTD (syntaxe, chargement).
pub(crate) const GRAMMAR_CODE: &str = "dtd-grammar";
/// Code des diagnostics de validation du document contre la DTD.
pub(crate) const VALIDATION_CODE: &str = "dtd-validation";
/// Code des diagnostics de références d'entités.
pub(crate) const ENTITY_CODE: &str = "xml-entity";
/// Nombre maximal de valeurs proposées en remplacement.
const MAX_VALUE_ACTIONS: usize = 20;

/// Textes des DTD lues sur disque, invalidés par date de modification et
/// taille.
pub(crate) type DtdCache = HashMap<PathBuf, (SystemTime, u64, Arc<str>)>;

/// Fichier DTD (`.dtd`, `.ent`).
pub(crate) fn is_dtd_uri(uri: &str) -> bool {
    uri_to_path(uri).extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("dtd") || extension.eq_ignore_ascii_case("ent")
    })
}

/// Contexte de chargement des DTD.
pub(crate) struct DtdContext<'a> {
    /// Documents ouverts (URI -> contenu), prioritaires sur le disque.
    pub(crate) documents: &'a HashMap<String, String>,
    pub(crate) catalogs: &'a Catalogs,
    pub(crate) cache: &'a mut DtdCache,
}

/// Grammaire d'un document : `doctype` pour un document d'instance, `None`
/// pour un fichier DTD (source 0 dans les deux cas).
pub(crate) struct Grammar {
    pub(crate) doctype: Option<Doctype>,
    pub(crate) dtd: Dtd,
}

fn document_path(uri: &str) -> Option<PathBuf> {
    uri.starts_with("file:").then(|| uri_to_path(uri))
}

struct Loader<'c, 'a> {
    context: &'c mut DtdContext<'a>,
}

impl ExternalLoader for Loader<'_, '_> {
    fn load(
        &mut self,
        public: Option<&str>,
        system: &str,
        base: Option<&Path>,
    ) -> Result<(PathBuf, String), LoadError> {
        let path = resolve_external(self.context, public, system, base)?;
        read_text(self.context, &path).map(|text| (path, text))
    }
}

fn not_found(path: &Path) -> LoadError {
    LoadError {
        message: format!("DTD « {} » introuvable", path.display()),
        remote: false,
    }
}

fn is_available(context: &DtdContext<'_>, path: &Path) -> bool {
    path.is_file() || context.documents.contains_key(&path_to_uri(path))
}

/// Résout un identifiant externe : catalogues XML, puis URI `file:`, chemin
/// absolu ou relatif à `base`. Une ressource distante n'est jamais
/// téléchargée.
fn resolve_external(
    context: &DtdContext<'_>,
    public: Option<&str>,
    system: &str,
    base: Option<&Path>,
) -> Result<PathBuf, LoadError> {
    let system = system.trim();
    if let Some(target) = context.catalogs.resolve_external(public, Some(system)) {
        return match target_path(&target) {
            Some(path) if is_available(context, &path) => Ok(path),
            Some(path) => Err(not_found(&path)),
            None => Err(LoadError {
                message: format!(
                    "la cible de catalogue « {target} » est distante et n'est jamais téléchargée"
                ),
                remote: true,
            }),
        };
    }
    if let Some(scheme) = uri_scheme(system) {
        if scheme.eq_ignore_ascii_case("file") {
            let path = file_uri_to_path(system);
            return if is_available(context, &path) {
                Ok(path)
            } else {
                Err(not_found(&path))
            };
        }
        return Err(LoadError {
            message: format!(
                "DTD « {system} » non chargée : les ressources distantes ne sont jamais téléchargées, associez-la à un fichier local avec xml.catalogs"
            ),
            remote: true,
        });
    }
    let decoded = percent_decode(system);
    let candidate = Path::new(&decoded);
    let path = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        match base.and_then(Path::parent) {
            Some(directory) => directory.join(candidate),
            None => {
                return Err(LoadError {
                    message: format!(
                        "identifiant système relatif « {system} » non résolu : le document n'est pas un fichier local"
                    ),
                    remote: false,
                });
            }
        }
    };
    if is_available(context, &path) {
        Ok(path)
    } else {
        Err(not_found(&path))
    }
}

fn read_text(context: &mut DtdContext<'_>, path: &Path) -> Result<String, LoadError> {
    if let Some(text) = context.documents.get(&path_to_uri(path)) {
        return Ok(text.clone());
    }
    let error = |message: String| LoadError {
        message,
        remote: false,
    };
    let metadata = fs::metadata(path)
        .map_err(|cause| error(format!("DTD « {} » illisible : {cause}", path.display())))?;
    if metadata.len() > MAX_DTD_SIZE {
        return Err(error(format!(
            "DTD « {} » trop volumineuse (plus de {MAX_DTD_SIZE} octets)",
            path.display()
        )));
    }
    let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
    if let Some((time, length, text)) = context.cache.get(path)
        && *time == modified
        && *length == metadata.len()
    {
        return Ok(text.to_string());
    }
    let text = fs::read_to_string(path)
        .map_err(|cause| error(format!("DTD « {} » illisible : {cause}", path.display())))?;
    context.cache.insert(
        path.to_path_buf(),
        (modified, metadata.len(), Arc::from(text.as_str())),
    );
    Ok(text)
}

/// Grammaire du document `uri` : DTD de son `<!DOCTYPE>` (`None` sans
/// DOCTYPE) ou, pour un fichier `.dtd`, le fichier lui-même.
pub(crate) fn load(context: &mut DtdContext<'_>, uri: &str, source: &str) -> Option<Grammar> {
    let path = document_path(uri);
    let mut loader = Loader { context };
    if is_dtd_uri(uri) {
        return Some(Grammar {
            doctype: None,
            dtd: parse_dtd(source, path, &mut loader),
        });
    }
    let (doctype, dtd) = load_document_dtd(source, path, &mut loader)?;
    Some(Grammar {
        doctype: Some(doctype),
        dtd,
    })
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

fn diagnostic(
    lines: &LineIndex,
    source: &str,
    range: &Range<usize>,
    severity: u8,
    (code, category, kind): (&str, &str, &str),
    message: String,
) -> Value {
    json!({
        "range": {
            "start": lines.position(source, range.start),
            "end": lines.position(source, range.end),
        },
        "severity": severity,
        "source": "xml-lsp",
        "code": code,
        "data": {"category": category, "kind": kind},
        "message": message,
    })
}

/// Diagnostics DTD du document `uri` (voir le module).
pub(crate) fn diagnostics(
    context: &mut DtdContext<'_>,
    uri: &str,
    source: &str,
    validation: &ValidationSettings,
) -> Vec<Value> {
    let lines = LineIndex::new(source);
    let mut diagnostics = Vec::new();
    if is_dtd_uri(uri) {
        if let Some(grammar) = load(context, uri, source) {
            grammar_diagnostics(&grammar.dtd, source, &lines, &mut diagnostics);
        }
        return diagnostics;
    }
    if validation.disallow_doc_type_decl && find_doctype(source).is_some() {
        // Le DOCTYPE lui-même est signalé ; sa grammaire est ignorée.
        return diagnostics;
    }
    let grammar = load(context, uri, source);
    let dtd = grammar.as_ref().map(|grammar| &grammar.dtd);
    if let Some(dtd) = dtd {
        grammar_diagnostics(dtd, source, &lines, &mut diagnostics);
    }
    let incomplete = dtd.is_some_and(|dtd| dtd.incomplete);
    for problem in check_entity_references(source, dtd) {
        // Déclaration peut-être dans une DTD non chargée : avertissement.
        let severity =
            if incomplete && matches!(problem.kind, InstanceProblemKind::UndefinedEntity { .. }) {
                2
            } else {
                1
            };
        diagnostics.push(diagnostic(
            &lines,
            source,
            &problem.range,
            severity,
            (ENTITY_CODE, "xml", problem.kind.id()),
            problem.message,
        ));
    }
    if let Some(dtd) = dtd {
        if validation.resolve_external_entities {
            external_entity_diagnostics(context, dtd, source, &lines, &mut diagnostics);
        }
        // Sans la DTD complète, toute déclaration peut manquer : pas de
        // validation (l'échec de chargement est déjà signalé).
        if !dtd.incomplete {
            for problem in validate_instance(source, dtd) {
                diagnostics.push(diagnostic(
                    &lines,
                    source,
                    &problem.range,
                    1,
                    (VALIDATION_CODE, "dtd", problem.kind.id()),
                    problem.message,
                ));
            }
        }
    }
    diagnostics
}

/// Problèmes de la grammaire : ceux du document (sous-ensemble interne,
/// fichier `.dtd`) à leur place, ceux d'un fichier externe résumés sur la
/// référence qui l'a chargé.
fn grammar_diagnostics(dtd: &Dtd, source: &str, lines: &LineIndex, out: &mut Vec<Value>) {
    let mut external: Vec<(Range<usize>, u8, Vec<String>)> = Vec::new();
    for problem in &dtd.problems {
        let severity = match problem.kind {
            DtdProblemKind::ExternalLoad { remote: true } => 2,
            _ => 1,
        };
        let anchored = dtd.anchor(&problem.location);
        if anchored.source == 0 {
            out.push(diagnostic(
                lines,
                source,
                &anchored.range,
                severity,
                (GRAMMAR_CODE, "dtd", problem.kind.id()),
                problem.message.clone(),
            ));
            continue;
        }
        let Some(origin) = dtd.origin_in(&problem.location, 0) else {
            continue;
        };
        let file = dtd
            .source_path(anchored.source)
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let line = dtd.source_text(anchored.source)[..anchored
            .range
            .start
            .min(dtd.source_text(anchored.source).len())]
            .matches('\n')
            .count()
            + 1;
        let detail = format!("{} ({file}:{line})", problem.message);
        match external
            .iter_mut()
            .find(|(range, _, _)| *range == origin.range)
        {
            Some((_, existing, details)) => {
                *existing = (*existing).min(severity);
                details.push(detail);
            }
            None => external.push((origin.range, severity, vec![detail])),
        }
    }
    for (range, severity, details) in external {
        let message = if details.len() == 1 {
            format!("erreur dans la DTD : {}", details[0])
        } else {
            format!(
                "{} erreurs dans la DTD, dont : {}",
                details.len(),
                details[0]
            )
        };
        out.push(diagnostic(
            lines,
            source,
            &range,
            severity,
            (GRAMMAR_CODE, "dtd", "externalGrammar"),
            message,
        ));
    }
}

/// `xml.validation.resolveExternalEntities` : références à des entités
/// générales externes introuvables.
fn external_entity_diagnostics(
    context: &mut DtdContext<'_>,
    dtd: &Dtd,
    source: &str,
    lines: &LineIndex,
    out: &mut Vec<Value>,
) {
    for reference in general_entity_references(source) {
        let name = &source[reference.name.clone()];
        let Some(entity) = dtd.general_entity(name) else {
            continue;
        };
        let EntityValue::External {
            public,
            system,
            notation: None,
            base,
        } = &entity.value
        else {
            continue;
        };
        if let Err(error) = resolve_external(context, public.as_deref(), system, base.as_deref()) {
            out.push(diagnostic(
                lines,
                source,
                &reference.range,
                2,
                (ENTITY_CODE, "xml", "externalEntity"),
                format!("entité externe « {name} » non résolue : {}", error.message),
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Contextes
// ---------------------------------------------------------------------------

/// Étendue du texte DTD du document : fichier entier ou sous-ensemble
/// interne.
fn dtd_region(grammar: Option<&Grammar>, uri: &str, source: &str) -> Option<Range<usize>> {
    if is_dtd_uri(uri) {
        return Some(0..source.len());
    }
    grammar?.doctype.as_ref()?.internal_subset.clone()
}

/// Le curseur est dans du texte DTD (fichier `.dtd` ou sous-ensemble
/// interne du DOCTYPE).
pub(crate) fn in_dtd_text(
    grammar: Option<&Grammar>,
    uri: &str,
    source: &str,
    offset: usize,
) -> bool {
    dtd_region(grammar, uri, source)
        .is_some_and(|region| region.start <= offset && offset <= region.end)
}

/// Début du nom en cours de saisie avant `offset`.
fn typed_start(source: &str, offset: usize) -> usize {
    source[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, character)| is_name_char(*character))
        .last()
        .map_or(offset, |(index, _)| index)
}

/// Nom (suite de `NameChar`) qui contient `offset`.
fn word_at(source: &str, offset: usize) -> Option<Range<usize>> {
    let start = typed_start(source, offset);
    let end = source[offset..]
        .char_indices()
        .find(|(_, character)| !is_name_char(*character))
        .map_or(source.len(), |(index, _)| offset + index);
    (start < end).then_some(start..end)
}

fn floor_boundary(source: &str, offset: usize) -> usize {
    let mut offset = offset.min(source.len());
    while !source.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Le curseur est à l'intérieur d'un commentaire, d'une section CDATA,
/// d'une instruction de traitement ou d'une déclaration.
fn in_markup(source: &str, offset: usize) -> bool {
    scan_markup(source)
        .iter()
        .any(|markup| markup.range.start < offset && (offset < markup.range.end || !markup.closed))
}

/// Contexte dans une balise ouvrante.
#[derive(Debug, PartialEq, Eq)]
enum TagContext<'s> {
    ElementName,
    AttributeName {
        element: &'s str,
        present: Vec<&'s str>,
    },
    AttributeValue {
        element: &'s str,
        attribute: &'s str,
    },
}

/// Analyse le début de balise `fragment` (après `<`, jusqu'au curseur).
fn tag_context(fragment: &str) -> Option<TagContext<'_>> {
    if fragment.starts_with(['/', '!', '?']) {
        return None;
    }
    let element_end = fragment
        .find(|character: char| character.is_whitespace())
        .unwrap_or(fragment.len());
    let element = &fragment[..element_end];
    if !element.chars().all(is_name_char) {
        return None;
    }
    if element_end == fragment.len() {
        return Some(TagContext::ElementName);
    }
    let mut present = Vec::new();
    let mut token_start: Option<usize> = None;
    let mut last_name: Option<&str> = None;
    let mut pending: Option<&str> = None;
    let mut quote: Option<(char, &str)> = None;
    for (index, character) in fragment
        .char_indices()
        .skip_while(|(index, _)| *index < element_end)
    {
        if let Some((open, _)) = quote {
            if character == open {
                quote = None;
            }
            continue;
        }
        match character {
            '"' | '\'' => {
                quote = Some((character, pending.take().unwrap_or_default()));
                token_start = None;
            }
            '=' => {
                if let Some(start) = token_start.take() {
                    last_name = Some(&fragment[start..index]);
                }
                pending = last_name.take();
                present.extend(pending);
            }
            character if character.is_whitespace() => {
                if let Some(start) = token_start.take() {
                    last_name = Some(&fragment[start..index]);
                }
            }
            _ => {
                if token_start.is_none() {
                    token_start = Some(index);
                    last_name = None;
                }
            }
        }
    }
    if let Some((_, attribute)) = quote {
        return Some(TagContext::AttributeValue { element, attribute });
    }
    if pending.is_some() {
        return None;
    }
    Some(TagContext::AttributeName { element, present })
}

// ---------------------------------------------------------------------------
// Complétion
// ---------------------------------------------------------------------------

fn item(
    label: &str,
    kind: u8,
    insert_text: String,
    snippet: bool,
    detail: Option<String>,
    documentation: Option<&str>,
) -> Value {
    let mut item = json!({
        "label": label,
        "kind": kind,
        "insertText": insert_text,
    });
    if snippet {
        item["insertTextFormat"] = json!(2);
    }
    if let Some(detail) = detail {
        item["detail"] = json!(detail);
    }
    if let Some(documentation) = documentation {
        item["documentation"] =
            json!({"kind": "markdown", "value": escape_markdown(documentation)});
    }
    item
}

const ELEMENT_KIND: u8 = 10;
const ATTRIBUTE_KIND: u8 = 5;
const VALUE_KIND: u8 = 12;
const ENTITY_KIND: u8 = 21;
const KEYWORD_KIND: u8 = 14;

fn element_declaration_text(dtd: &Dtd, name: &str) -> Option<String> {
    let declaration = dtd.element(name)?;
    Some(format!(
        "<!ELEMENT {} {}>",
        declaration.name, declaration.content
    ))
}

fn element_item(dtd: &Dtd, name: &str) -> Value {
    let documentation = dtd
        .element(name)
        .and_then(|declaration| declaration.documentation.as_deref());
    item(
        name,
        ELEMENT_KIND,
        name.to_owned(),
        false,
        element_declaration_text(dtd, name),
        documentation,
    )
}

/// Entités générales (et prédéfinies) après `&`.
fn entity_items(dtd: Option<&Dtd>) -> Vec<Value> {
    let mut items = dtd_core::PREDEFINED_ENTITIES
        .iter()
        .map(|(name, value)| {
            item(
                name,
                ENTITY_KIND,
                format!("{name};"),
                false,
                Some(format!("&{name}; → {value}")),
                None,
            )
        })
        .collect::<Vec<_>>();
    for entity in dtd.into_iter().flat_map(|dtd| &dtd.general_entities) {
        if entity.expansion.unparsed
            || dtd_core::PREDEFINED_ENTITIES
                .iter()
                .any(|(name, _)| *name == entity.name)
        {
            continue;
        }
        items.push(item(
            &entity.name,
            ENTITY_KIND,
            format!("{};", entity.name),
            false,
            Some(entity.display()),
            entity.documentation.as_deref(),
        ));
    }
    items
}

/// Propositions DTD au curseur `offset` du document `uri`.
pub(crate) fn completions(
    grammar: Option<&Grammar>,
    uri: &str,
    source: &str,
    offset: usize,
) -> Vec<Value> {
    let offset = floor_boundary(source, offset);
    let dtd = grammar.map(|grammar| &grammar.dtd);
    if let Some(region) = dtd_region(grammar, uri, source)
        && region.start <= offset
        && offset <= region.end
    {
        return dtd_text_completions(dtd, source, region.start, offset);
    }
    if in_markup(source, offset) {
        return Vec::new();
    }
    let start = typed_start(source, offset);
    let prefix = &source[..start];
    if prefix.ends_with('&') {
        return entity_items(dtd);
    }
    let Some(grammar) = grammar else {
        return Vec::new();
    };
    let dtd = &grammar.dtd;
    let Some(opening) = prefix.rfind('<') else {
        return Vec::new();
    };
    if source[opening..offset].contains('>') {
        return Vec::new();
    }
    match tag_context(&source[opening + 1..offset]) {
        Some(TagContext::ElementName) => {
            let tree = XmlTagTree::parse(&source[..opening]);
            let elements = tree.elements();
            let parent = elements
                .iter()
                .enumerate()
                .rev()
                .find(|(_, element)| {
                    element.start_tag.kind == XmlTagKind::Start
                        && element.start_tag.closed
                        && element.end_tag.is_none()
                })
                .map(|(index, _)| index);
            let preceding = elements
                .iter()
                .filter(|element| element.parent == parent)
                .map(|element| element.name(source))
                .collect::<Vec<_>>();
            let parent_name = parent.map(|index| elements[index].name(source));
            dtd.allowed_children(parent_name, &preceding)
                .iter()
                .map(|name| element_item(dtd, name))
                .collect()
        }
        Some(TagContext::AttributeName { element, present }) => dtd
            .attributes_of(element)
            .filter(|attribute| !present.contains(&attribute.name.as_str()))
            .map(|attribute| {
                item(
                    &attribute.name,
                    ATTRIBUTE_KIND,
                    format!("{}=\"$1\"", attribute.name),
                    true,
                    Some(attribute.display()),
                    attribute.documentation.as_deref(),
                )
            })
            .collect(),
        Some(TagContext::AttributeValue { element, attribute }) => {
            let Some(declaration) = dtd.attribute(element, attribute) else {
                return Vec::new();
            };
            let mut values = declaration
                .attribute_type
                .values()
                .map(<[String]>::to_vec)
                .unwrap_or_default();
            if matches!(
                declaration.attribute_type,
                AttributeType::IdRef | AttributeType::IdRefs
            ) {
                values.extend(document_ids(dtd, source));
            }
            if let Some(value) = declaration.default.value()
                && !values.iter().any(|existing| existing == value)
            {
                values.push(value.to_owned());
            }
            values
                .iter()
                .map(|value| {
                    item(
                        value,
                        VALUE_KIND,
                        value.clone(),
                        false,
                        Some(declaration.display()),
                        None,
                    )
                })
                .collect()
        }
        None => Vec::new(),
    }
}

/// Valeurs des attributs de type ID du document.
fn document_ids(dtd: &Dtd, source: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for tag in scan_tags(source) {
        if tag.kind == XmlTagKind::End {
            continue;
        }
        let element = tag.name(source);
        for attribute in scan_attributes(source, &tag) {
            let is_id = dtd
                .attribute(element, attribute.name(source))
                .is_some_and(|declaration| declaration.attribute_type == AttributeType::Id);
            if is_id
                && let Some(value) = attribute.value(source)
                && !value.is_empty()
                && !ids.iter().any(|id| id == value)
            {
                ids.push(value.to_owned());
            }
        }
    }
    ids
}

/// Propositions dans du texte DTD commençant en `region_start`.
fn dtd_text_completions(
    dtd: Option<&Dtd>,
    source: &str,
    region_start: usize,
    offset: usize,
) -> Vec<Value> {
    let before = &source[region_start..offset];
    // Dans un commentaire ou un littéral : rien.
    if before
        .rfind("<!--")
        .is_some_and(|start| !before[start..].contains("-->"))
    {
        return Vec::new();
    }
    let start = typed_start(source, offset).max(region_start);
    let preceding = &source[region_start..start];
    if preceding.ends_with('%') {
        return dtd
            .into_iter()
            .flat_map(|dtd| &dtd.parameter_entities)
            .map(|entity| {
                item(
                    &entity.name,
                    ENTITY_KIND,
                    format!("{};", entity.name),
                    false,
                    Some(entity.display()),
                    entity.documentation.as_deref(),
                )
            })
            .collect();
    }
    let declaration = preceding
        .rfind("<!")
        .filter(|&index| !preceding[index..].contains('>'))
        .map(|index| &preceding[index + 2..]);
    if declaration.is_some_and(|declaration| {
        let quotes = declaration.matches('"').count() + declaration.matches('\'').count();
        quotes % 2 == 1
    }) {
        if preceding.ends_with('&') {
            return entity_items(dtd);
        }
        return Vec::new();
    }
    if preceding.ends_with("<!") {
        return [
            ("ELEMENT", "ELEMENT ${1:name} ${2:(#PCDATA)}>"),
            (
                "ATTLIST",
                "ATTLIST ${1:element} ${2:attribute} ${3:CDATA} ${4:#IMPLIED}>",
            ),
            ("ENTITY", "ENTITY ${1:name} \"${2:value}\">"),
            ("NOTATION", "NOTATION ${1:name} SYSTEM \"${2:uri}\">"),
        ]
        .iter()
        .map(|(label, snippet)| item(label, KEYWORD_KIND, (*snippet).to_owned(), true, None, None))
        .collect();
    }
    let keywords = |words: &[&str]| {
        words
            .iter()
            .map(|word| item(word, KEYWORD_KIND, (*word).to_owned(), false, None, None))
            .collect::<Vec<_>>()
    };
    if preceding.ends_with('#') {
        return keywords(&["PCDATA", "REQUIRED", "IMPLIED", "FIXED"]);
    }
    let Some(declaration) = declaration else {
        return Vec::new();
    };
    let element_names = || {
        dtd.into_iter()
            .flat_map(|dtd| {
                dtd.elements
                    .iter()
                    .map(|element| element_item(dtd, &element.name))
            })
            .collect::<Vec<_>>()
    };
    let tokens = declaration.split_whitespace().collect::<Vec<_>>();
    let complete_tokens = if declaration.ends_with(char::is_whitespace) {
        tokens.len()
    } else {
        tokens.len().saturating_sub(1)
    };
    match tokens.first().copied() {
        Some("ELEMENT") if declaration.contains('(') => {
            let trimmed = preceding.trim_end();
            if trimmed.ends_with(['(', '|', ',']) {
                element_names()
            } else {
                Vec::new()
            }
        }
        Some("ELEMENT") if complete_tokens == 2 => keywords(&["EMPTY", "ANY"]),
        Some("ATTLIST") if complete_tokens == 1 => element_names(),
        Some("ATTLIST") if complete_tokens >= 3 && complete_tokens % 3 == 0 => keywords(&[
            "CDATA", "ID", "IDREF", "IDREFS", "ENTITY", "ENTITIES", "NMTOKEN", "NMTOKENS",
            "NOTATION",
        ]),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Survol et définition
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum Target {
    Element(String),
    Attribute { element: String, name: String },
    Entity(String),
    ParameterEntity(String),
    Notation(String),
}

/// Construction DTD sous le curseur et son étendue.
fn target_at(
    grammar: &Grammar,
    uri: &str,
    source: &str,
    offset: usize,
) -> Option<(Target, Range<usize>)> {
    let offset = floor_boundary(source, offset);
    let dtd = &grammar.dtd;
    if let Some(region) = dtd_region(Some(grammar), uri, source)
        && region.start <= offset
        && offset <= region.end
    {
        if let Some(name) = entity_reference_at(source, offset, true) {
            return Some((
                Target::ParameterEntity(source[name.clone()].to_owned()),
                name,
            ));
        }
        if let Some(name) = entity_reference_at(source, offset, false) {
            return Some((Target::Entity(source[name.clone()].to_owned()), name));
        }
        let word = word_at(source, offset)?;
        let name = &source[word.clone()];
        let before = &source[region.start..word.start];
        let declaration = before
            .rfind("<!")
            .filter(|&index| !before[index..].contains('>'))
            .map(|index| &before[index + 2..]);
        let mut tokens = declaration.unwrap_or_default().split_whitespace();
        let keyword = tokens.next();
        if keyword == Some("ATTLIST")
            && let Some(element) = tokens.next()
            && dtd.attribute(element, name).is_some()
        {
            return Some((
                Target::Attribute {
                    element: element.to_owned(),
                    name: name.to_owned(),
                },
                word,
            ));
        }
        if keyword == Some("ENTITY") {
            let parameter = declaration.is_some_and(|text| {
                text.trim_start()["ENTITY".len()..]
                    .trim_start()
                    .starts_with('%')
            });
            if parameter && dtd.parameter_entity(name).is_some() {
                return Some((Target::ParameterEntity(name.to_owned()), word));
            }
            if !parameter && dtd.general_entity(name).is_some() {
                return Some((Target::Entity(name.to_owned()), word));
            }
        }
        if dtd.element(name).is_some() {
            return Some((Target::Element(name.to_owned()), word));
        }
        if dtd.notation(name).is_some() {
            return Some((Target::Notation(name.to_owned()), word));
        }
        return None;
    }
    if in_markup(source, offset) {
        return None;
    }
    if let Some(name) = entity_reference_at(source, offset, false)
        && general_entity_references(source)
            .iter()
            .any(|reference| reference.name == name)
    {
        return Some((Target::Entity(source[name.clone()].to_owned()), name));
    }
    let tag = scan_tags(source)
        .into_iter()
        .find(|tag| tag.range.start < offset && offset < tag.range.end.max(tag.range.start + 1))?;
    if tag.name_contains(offset) {
        let name = tag.name(source).to_owned();
        return dtd
            .element(&name)
            .is_some()
            .then(|| (Target::Element(name), tag.name.clone()));
    }
    if tag.kind == XmlTagKind::End {
        return None;
    }
    let element = tag.name(source);
    let attribute = scan_attributes(source, &tag)
        .into_iter()
        .find(|attribute| attribute.name.start <= offset && offset <= attribute.name.end)?;
    let name = attribute.name(source);
    dtd.attribute(element, name).is_some().then(|| {
        (
            Target::Attribute {
                element: element.to_owned(),
                name: name.to_owned(),
            },
            attribute.name.clone(),
        )
    })
}

/// Déclaration de la cible : texte, documentation et emplacement.
fn declaration_of(dtd: &Dtd, target: &Target) -> Option<(String, Option<String>, Location)> {
    Some(match target {
        Target::Element(name) => {
            let declaration = dtd.element(name)?;
            let mut text = format!("<!ELEMENT {} {}>", declaration.name, declaration.content);
            for attribute in dtd.attributes_of(name) {
                text.push('\n');
                text.push_str(&attribute.display());
            }
            (
                text,
                declaration.documentation.clone(),
                declaration.location.clone(),
            )
        }
        Target::Attribute { element, name } => {
            let declaration = dtd.attribute(element, name)?;
            (
                declaration.display(),
                declaration.documentation.clone(),
                declaration.location.clone(),
            )
        }
        Target::Entity(name) => {
            let declaration = dtd.general_entity(name)?;
            (
                declaration.display(),
                declaration.documentation.clone(),
                declaration.location.clone(),
            )
        }
        Target::ParameterEntity(name) => {
            let declaration = dtd.parameter_entity(name)?;
            (
                declaration.display(),
                declaration.documentation.clone(),
                declaration.location.clone(),
            )
        }
        Target::Notation(name) => {
            let declaration = dtd.notation(name)?;
            let external = match (&declaration.public, &declaration.system) {
                (Some(public), Some(system)) => format!("PUBLIC \"{public}\" \"{system}\""),
                (Some(public), None) => format!("PUBLIC \"{public}\""),
                (None, Some(system)) => format!("SYSTEM \"{system}\""),
                (None, None) => String::new(),
            };
            (
                format!("<!NOTATION {} {external}>", declaration.name),
                declaration.documentation.clone(),
                declaration.location.clone(),
            )
        }
    })
}

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

/// Emplacement réel (URI, texte, étendue) d'une déclaration.
fn resolve_location<'g>(
    grammar: &'g Grammar,
    uri: &str,
    source: &'g str,
    location: &Location,
) -> Option<(String, &'g str, Range<usize>)> {
    let anchored = grammar.dtd.anchor(location);
    if anchored.source == 0 {
        return Some((uri.to_owned(), source, anchored.range));
    }
    let path = grammar.dtd.source_path(anchored.source)?;
    Some((
        path_to_uri(path),
        grammar.dtd.source_text(anchored.source),
        anchored.range,
    ))
}

/// Répond à `textDocument/hover` sur une construction déclarée par la DTD.
pub(crate) fn hover(grammar: &Grammar, uri: &str, source: &str, offset: usize) -> Option<Value> {
    let (target, range) = target_at(grammar, uri, source, offset)?;
    let (text, documentation, location) = declaration_of(&grammar.dtd, &target)?;
    let mut sections = vec![format!("```xml\n{text}\n```")];
    if let Some(documentation) = documentation {
        sections.push(escape_markdown(&documentation));
    }
    if let Some((target_uri, _, _)) = resolve_location(grammar, uri, source, &location)
        && target_uri != uri
        && let Some(name) = uri_to_path(&target_uri).file_name()
    {
        sections.push(format!(
            "Source : [{}]({target_uri})",
            escape_markdown(&name.to_string_lossy())
        ));
    }
    let lines = LineIndex::new(source);
    Some(json!({
        "contents": {"kind": "markdown", "value": sections.join("\n\n")},
        "range": {
            "start": lines.position(source, range.start),
            "end": lines.position(source, range.end),
        },
    }))
}

/// Répond à `textDocument/definition` : déclaration DTD de l'élément, de
/// l'attribut, de l'entité ou de la notation sous le curseur.
pub(crate) fn definition(
    grammar: &Grammar,
    uri: &str,
    source: &str,
    offset: usize,
) -> Option<Value> {
    let (target, _) = target_at(grammar, uri, source, offset)?;
    let (_, _, location) = declaration_of(&grammar.dtd, &target)?;
    let (target_uri, text, range) = resolve_location(grammar, uri, source, &location)?;
    let lines = LineIndex::new(text);
    Some(json!([{
        "uri": target_uri,
        "range": {
            "start": lines.position(text, range.start),
            "end": lines.position(text, range.end),
        },
    }]))
}

// ---------------------------------------------------------------------------
// Correctifs
// ---------------------------------------------------------------------------

fn escape_value(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

/// Insertion de `<!ENTITY name "">` dans le DOCTYPE (créé au besoin).
fn declare_entity_edit(
    grammar: Option<&Grammar>,
    source: &str,
    name: &str,
) -> Option<(Range<usize>, String)> {
    let declaration = format!("<!ENTITY {name} \"\">");
    let newline = if source.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let doctype = grammar
        .and_then(|grammar| grammar.doctype.clone())
        .or_else(|| find_doctype(source));
    match doctype {
        Some(doctype) => match doctype.internal_subset {
            Some(subset) => {
                let text = if source[..subset.end].ends_with('\n') {
                    format!("  {declaration}{newline}")
                } else {
                    format!("{newline}  {declaration}{newline}")
                };
                Some((subset.end..subset.end, text))
            }
            None => {
                let end = doctype.range.end;
                if !source[..end].ends_with('>') {
                    return None;
                }
                Some((
                    end - 1..end - 1,
                    format!(" [{newline}  {declaration}{newline}]"),
                ))
            }
        },
        None => {
            let tree = XmlTagTree::parse(source);
            let root = tree.elements().first()?;
            let start = root.start_tag.range.start;
            Some((
                start..start,
                format!(
                    "<!DOCTYPE {} [{newline}  {declaration}{newline}]>{newline}",
                    root.name(source)
                ),
            ))
        }
    }
}

/// Correctifs DTD de l'étendue demandée.
pub(crate) fn code_actions(actions: &mut Actions<'_>, grammar: Option<&Grammar>) {
    if !actions.wants(QUICK_FIX) {
        return;
    }
    let source = actions.source;
    let dtd = grammar.map(|grammar| &grammar.dtd);
    for problem in check_entity_references(source, dtd) {
        let InstanceProblemKind::UndefinedEntity { name } = &problem.kind else {
            continue;
        };
        if !actions.requested(&problem.range) {
            continue;
        }
        let Some(edit) = declare_entity_edit(grammar, source, name) else {
            continue;
        };
        let diagnostics = actions.matching(ENTITY_CODE, "kind", problem.kind.id(), &problem.range);
        actions.push(
            format!("Déclarer l'entité « &{name}; » dans le DOCTYPE"),
            QUICK_FIX,
            vec![edit],
            diagnostics,
            true,
        );
    }
    let Some(dtd) = dtd.filter(|dtd| !dtd.incomplete) else {
        return;
    };
    for problem in validate_instance(source, dtd) {
        if !actions.requested(&problem.range) {
            continue;
        }
        let diagnostics =
            actions.matching(VALIDATION_CODE, "kind", problem.kind.id(), &problem.range);
        match &problem.kind {
            InstanceProblemKind::MissingAttribute {
                attribute,
                value,
                insert_at,
                ..
            } => actions.push(
                format!("Ajouter l'attribut requis « {attribute} »"),
                QUICK_FIX,
                vec![(
                    *insert_at..*insert_at,
                    format!(" {attribute}=\"{}\"", escape_value(value)),
                )],
                diagnostics,
                true,
            ),
            InstanceProblemKind::InvalidEnumeration { values } => {
                for value in values.iter().take(MAX_VALUE_ACTIONS) {
                    actions.push(
                        format!("Remplacer par « {value} »"),
                        QUICK_FIX,
                        vec![(problem.range.clone(), escape_value(value))],
                        diagnostics.clone(),
                        values.len() == 1,
                    );
                }
            }
            InstanceProblemKind::FixedValue { expected } => actions.push(
                format!("Remplacer par la valeur fixe « {expected} »"),
                QUICK_FIX,
                vec![(problem.range.clone(), escape_value(expected))],
                diagnostics,
                true,
            ),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Symboles
// ---------------------------------------------------------------------------

mod symbol_kind {
    pub(super) const CLASS: u8 = 5;
    pub(super) const PROPERTY: u8 = 7;
    pub(super) const CONSTANT: u8 = 14;
    pub(super) const TYPE_PARAMETER: u8 = 26;
}

/// Symboles d'un fichier DTD : éléments (avec leurs attributs), entités et
/// notations déclarés dans le fichier lui-même.
pub(crate) fn document_symbols(
    grammar: &Grammar,
    uri: &str,
    source: &str,
    hierarchical: bool,
) -> Vec<Value> {
    let dtd = &grammar.dtd;
    let lines = LineIndex::new(source);
    let range = |range: &Range<usize>| {
        json!({
            "start": lines.position(source, range.start),
            "end": lines.position(source, range.end),
        })
    };
    // Déclaration et nom ancrés dans le document, s'ils y sont.
    let local = |declaration: &Location, name: &Location| {
        let declaration = dtd.anchor(declaration);
        let name = dtd.anchor(name);
        (declaration.source == 0 && name.source == 0).then_some((declaration.range, name.range))
    };
    let symbol = |name: &str,
                  kind: u8,
                  detail: String,
                  spans: (Range<usize>, Range<usize>),
                  children: Vec<Value>,
                  container: Option<&str>| {
        if hierarchical {
            let mut symbol = json!({
                "name": name,
                "detail": detail,
                "kind": kind,
                "range": range(&spans.0),
                "selectionRange": range(&spans.1),
            });
            if !children.is_empty() {
                symbol["children"] = Value::Array(children);
            }
            symbol
        } else {
            let mut symbol = json!({
                "name": name,
                "kind": kind,
                "location": {"uri": uri, "range": range(&spans.0)},
            });
            if let Some(container) = container {
                symbol["containerName"] = json!(container);
            }
            symbol
        }
    };
    let mut symbols = Vec::new();
    for element in &dtd.elements {
        let Some(spans) = local(&element.declaration, &element.location) else {
            continue;
        };
        let attributes = dtd
            .attributes_of(&element.name)
            .filter_map(|attribute| {
                let spans = local(&attribute.declaration, &attribute.location)?;
                Some(symbol(
                    &attribute.name,
                    symbol_kind::PROPERTY,
                    format!("{} {}", attribute.attribute_type, attribute.default),
                    spans,
                    Vec::new(),
                    Some(&element.name),
                ))
            })
            .collect::<Vec<_>>();
        let flat_attributes = if hierarchical {
            Vec::new()
        } else {
            attributes.clone()
        };
        symbols.push(symbol(
            &element.name,
            symbol_kind::CLASS,
            element.content.to_string(),
            spans,
            if hierarchical { attributes } else { Vec::new() },
            None,
        ));
        symbols.extend(flat_attributes);
    }
    for entity in dtd.parameter_entities.iter().chain(&dtd.general_entities) {
        let Some(spans) = local(&entity.declaration, &entity.location) else {
            continue;
        };
        let name = if entity.parameter {
            format!("%{}", entity.name)
        } else {
            format!("&{}", entity.name)
        };
        symbols.push(symbol(
            &name,
            symbol_kind::CONSTANT,
            entity.display(),
            spans,
            Vec::new(),
            None,
        ));
    }
    for notation in &dtd.notations {
        let Some(spans) = local(&notation.declaration, &notation.location) else {
            continue;
        };
        symbols.push(symbol(
            &notation.name,
            symbol_kind::TYPE_PARAMETER,
            String::new(),
            spans,
            Vec::new(),
            None,
        ));
    }
    if hierarchical {
        symbols.sort_by_key(|symbol| {
            (
                symbol["range"]["start"]["line"].as_u64(),
                symbol["range"]["start"]["character"].as_u64(),
            )
        });
    }
    symbols
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dossier temporaire propre au test.
    fn directory(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("xml-lsp-dtd {name} {}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).expect("directory should be created");
        directory
    }

    struct Fixture {
        documents: HashMap<String, String>,
        catalogs: Catalogs,
        cache: DtdCache,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                documents: HashMap::new(),
                catalogs: Catalogs::default(),
                cache: DtdCache::new(),
            }
        }

        fn context(&mut self) -> DtdContext<'_> {
            DtdContext {
                documents: &self.documents,
                catalogs: &self.catalogs,
                cache: &mut self.cache,
            }
        }

        fn grammar(&mut self, uri: &str, source: &str) -> Option<Grammar> {
            load(&mut self.context(), uri, source)
        }

        fn diagnostics(&mut self, uri: &str, source: &str) -> Vec<Value> {
            self.diagnostics_with(uri, source, &ValidationSettings::default())
        }

        fn diagnostics_with(
            &mut self,
            uri: &str,
            source: &str,
            validation: &ValidationSettings,
        ) -> Vec<Value> {
            diagnostics(&mut self.context(), uri, source, validation)
        }
    }

    fn kinds(diagnostics: &[Value]) -> Vec<(String, String, u64)> {
        diagnostics
            .iter()
            .map(|diagnostic| {
                (
                    diagnostic["code"].as_str().unwrap().to_owned(),
                    diagnostic["data"]["kind"].as_str().unwrap().to_owned(),
                    diagnostic["severity"].as_u64().unwrap(),
                )
            })
            .collect()
    }

    fn kind(code: &str, kind: &str, severity: u64) -> (String, String, u64) {
        (code.to_owned(), kind.to_owned(), severity)
    }

    fn labels(items: &[Value]) -> Vec<&str> {
        items
            .iter()
            .map(|item| item["label"].as_str().unwrap())
            .collect()
    }

    fn at(source: &str, marker: &str) -> usize {
        source.find(marker).expect("marker should exist")
    }

    const MEMO: &str = "<?xml version=\"1.0\"?>\n<!DOCTYPE memo [\n  <!-- Un mémo. -->\n  <!ELEMENT memo (to+, from, body)>\n  <!ELEMENT to (#PCDATA)>\n  <!ELEMENT from (#PCDATA)>\n  <!ELEMENT body (#PCDATA | ref)*>\n  <!ELEMENT ref EMPTY>\n  <!-- Priorité du mémo. -->\n  <!ATTLIST memo priority (low | normal | high) \"normal\" id ID #REQUIRED>\n  <!ATTLIST ref target IDREF #REQUIRED>\n  <!ENTITY company \"ACME\">\n  <!ENTITY % shared \"x\">\n]>\n";

    #[test]
    fn parses_tag_contexts() {
        assert_eq!(tag_context("me"), Some(TagContext::ElementName));
        assert_eq!(tag_context(""), Some(TagContext::ElementName));
        assert_eq!(
            tag_context("memo id=\"a\" pri"),
            Some(TagContext::AttributeName {
                element: "memo",
                present: vec!["id"],
            })
        );
        assert_eq!(
            tag_context("memo id='a b' priority=\"lo"),
            Some(TagContext::AttributeValue {
                element: "memo",
                attribute: "priority",
            })
        );
        assert_eq!(tag_context("memo id="), None);
        assert_eq!(tag_context("/memo"), None);
        assert_eq!(tag_context("!DOCTYPE"), None);
    }

    #[test]
    fn completes_from_the_document_type_declaration() {
        let mut fixture = Fixture::new();
        let uri = "file:///tmp/memo.xml";
        let complete = |fixture: &mut Fixture, source: &str| {
            let grammar = fixture.grammar(uri, source);
            completions(grammar.as_ref(), uri, source, source.len())
        };

        // Racine : nom du DOCTYPE.
        let source = format!("{MEMO}<");
        assert_eq!(labels(&complete(&mut fixture, &source)), vec!["memo"]);
        // Enfants permis après ceux déjà présents.
        let source = format!("{MEMO}<memo id=\"m1\"><to>a</to><");
        let items = complete(&mut fixture, &source);
        assert_eq!(labels(&items), vec!["from", "to"]);
        assert_eq!(items[0]["detail"], "<!ELEMENT from (#PCDATA)>");
        let source = format!("{MEMO}<memo id=\"m1\"><body>text <");
        assert_eq!(labels(&complete(&mut fixture, &source)), vec!["ref"]);
        // Attributs non présents, avec documentation.
        let source = format!("{MEMO}<memo id=\"m1\" ");
        let items = complete(&mut fixture, &source);
        assert_eq!(labels(&items), vec!["priority"]);
        assert_eq!(items[0]["insertText"], "priority=\"$1\"");
        assert_eq!(items[0]["insertTextFormat"], 2);
        assert_eq!(items[0]["documentation"]["value"], "Priorité du mémo.");
        // Valeurs énumérées et ID existants pour IDREF.
        let source = format!("{MEMO}<memo id=\"m1\" priority=\"");
        assert_eq!(
            labels(&complete(&mut fixture, &source)),
            vec!["low", "normal", "high"]
        );
        let source = format!("{MEMO}<memo id=\"m1\"><body><ref target=\"");
        assert_eq!(labels(&complete(&mut fixture, &source)), vec!["m1"]);
        // Entités après `&`, dans le texte et les valeurs d'attributs.
        let source = format!("{MEMO}<memo id=\"m1\"><to>&co");
        let items = complete(&mut fixture, &source);
        assert_eq!(
            labels(&items),
            vec!["amp", "lt", "gt", "quot", "apos", "company"]
        );
        assert_eq!(items[5]["insertText"], "company;");
        assert_eq!(items[5]["detail"], "<!ENTITY company \"ACME\">");
        // Sans DTD : entités prédéfinies seulement.
        let source = "<a b=\"&";
        assert_eq!(complete(&mut fixture, source).len(), 5);
        // Rien dans un commentaire.
        let source = format!("{MEMO}<memo id=\"m1\"><!-- &");
        assert!(complete(&mut fixture, &source).is_empty());
    }

    #[test]
    fn completes_inside_dtd_text() {
        let mut fixture = Fixture::new();
        let uri = "file:///tmp/grammar.dtd";
        let complete = |fixture: &mut Fixture, source: &str| {
            let grammar = fixture.grammar(uri, source);
            completions(grammar.as_ref(), uri, source, source.len())
        };
        let base = "<!ENTITY % inline \"b | i\">\n<!ELEMENT p (#PCDATA)>\n<!ELEMENT b (#PCDATA)>\n";
        let items = complete(&mut fixture, &format!("{base}<!"));
        assert_eq!(
            labels(&items),
            vec!["ELEMENT", "ATTLIST", "ENTITY", "NOTATION"]
        );
        assert_eq!(items[0]["insertTextFormat"], 2);
        assert_eq!(
            labels(&complete(&mut fixture, &format!("{base}<!ELEMENT q (#"))),
            vec!["PCDATA", "REQUIRED", "IMPLIED", "FIXED"]
        );
        assert_eq!(
            labels(&complete(
                &mut fixture,
                &format!("{base}<!ELEMENT q (#PCDATA | %")
            )),
            vec!["inline"]
        );
        assert_eq!(
            labels(&complete(&mut fixture, &format!("{base}<!ELEMENT q (p, "))),
            // `q` est déjà déclaré par la déclaration en cours (récursion permise).
            vec!["p", "b", "q"]
        );
        assert_eq!(
            labels(&complete(&mut fixture, &format!("{base}<!ELEMENT q "))),
            vec!["EMPTY", "ANY"]
        );
        assert_eq!(
            labels(&complete(&mut fixture, &format!("{base}<!ATTLIST "))),
            vec!["p", "b"]
        );
        assert!(
            labels(&complete(&mut fixture, &format!("{base}<!ATTLIST p a "))).contains(&"CDATA")
        );
        assert!(complete(&mut fixture, &format!("{base}<!ENTITY x \"<")).is_empty());

        // Sous-ensemble interne d'un document d'instance.
        let uri = "file:///tmp/doc.xml";
        let source = "<!DOCTYPE r [\n  <!ELEMENT r EMPTY>\n  <!";
        let grammar = fixture.grammar(uri, source);
        assert!(in_dtd_text(grammar.as_ref(), uri, source, source.len()));
        assert_eq!(
            completions(grammar.as_ref(), uri, source, source.len()).len(),
            4
        );
    }

    #[test]
    fn hovers_and_navigates_to_external_declarations() {
        let directory = directory("hover");
        let dtd_path = directory.join("memo.dtd");
        let dtd_text = "<!-- Destinataire\n     du mémo. -->\n<!ELEMENT to (#PCDATA)>\n<!ELEMENT memo (to)>\n<!-- Niveau. -->\n<!ATTLIST memo level NMTOKEN #IMPLIED>\n<!ENTITY sign \"— ACME\">\n";
        fs::write(&dtd_path, dtd_text).expect("dtd should be written");
        let uri = path_to_uri(&directory.join("memo.xml"));
        let source =
            "<!DOCTYPE memo SYSTEM \"memo.dtd\">\n<memo level=\"1\"><to>Bob &sign;</to></memo>";
        let mut fixture = Fixture::new();
        let grammar = fixture.grammar(&uri, source).expect("grammar");
        assert!(
            grammar.dtd.problems.is_empty(),
            "{:?}",
            grammar.dtd.problems
        );

        let hover_on = |marker: &str, delta: usize| {
            hover(&grammar, &uri, source, at(source, marker) + delta).expect("hover")
        };
        let element = hover_on("<to>", 1);
        let markdown = element["contents"]["value"].as_str().unwrap();
        assert!(
            markdown.starts_with("```xml\n<!ELEMENT to (#PCDATA)>\n```"),
            "{markdown}"
        );
        assert!(markdown.contains("Destinataire\ndu mémo."), "{markdown}");
        assert!(
            markdown.contains("Source : [memo.dtd](file://"),
            "{markdown}"
        );
        assert_eq!(
            element["range"]["start"],
            json!({"line": 1, "character": 17})
        );
        let memo = hover_on("<memo", 2);
        assert!(
            memo["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("<!ATTLIST memo level NMTOKEN #IMPLIED>")
        );
        let attribute = hover_on("level=", 1);
        assert!(
            attribute["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("Niveau.")
        );
        let entity = hover_on("&sign;", 2);
        assert!(
            entity["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("<!ENTITY sign \"— ACME\">")
        );
        assert!(hover(&grammar, &uri, source, at(source, "Bob")).is_none());

        let definition_at = |marker: &str, delta: usize| {
            definition(&grammar, &uri, source, at(source, marker) + delta).expect("definition")
        };
        let target = definition_at("</to>", 3);
        assert_eq!(target[0]["uri"], path_to_uri(&dtd_path));
        assert_eq!(
            target[0]["range"],
            json!({"start": {"line": 2, "character": 10}, "end": {"line": 2, "character": 12}})
        );
        let target = definition_at("level", 0);
        assert_eq!(
            target[0]["range"]["start"],
            json!({"line": 5, "character": 15})
        );
        let target = definition_at("&sign;", 1);
        assert_eq!(
            target[0]["range"]["start"],
            json!({"line": 6, "character": 9})
        );

        // Dans le fichier DTD lui-même : nom cité dans un modèle.
        let dtd_uri = path_to_uri(&dtd_path);
        let dtd_grammar = fixture.grammar(&dtd_uri, dtd_text).expect("grammar");
        let offset = at(dtd_text, "(to)") + 1;
        let target = definition(&dtd_grammar, &dtd_uri, dtd_text, offset).unwrap();
        assert_eq!(target[0]["uri"], dtd_uri);
        assert_eq!(
            target[0]["range"]["start"],
            json!({"line": 2, "character": 10})
        );
        let offset = at(dtd_text, "level");
        let hovered = hover(&dtd_grammar, &dtd_uri, dtd_text, offset).unwrap();
        let markdown = hovered["contents"]["value"].as_str().unwrap();
        assert!(markdown.contains("Niveau."), "{markdown}");
        // Pas de lien « Source » vers le fichier lui-même.
        assert!(!markdown.contains("Source"), "{markdown}");
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn publishes_grammar_entity_and_validation_diagnostics() {
        let directory = directory("diagnostics");
        fs::write(
            directory.join("broken.dtd"),
            "<!ELEMENT r (a)>\n<!ELEMENT a (#PCDATA)>\n<!ELEMENT a EMPTY>\n<!BOGUS>",
        )
        .expect("dtd should be written");
        fs::write(
            directory.join("ok.dtd"),
            "<!ELEMENT r (a)>\n<!ELEMENT a (#PCDATA)>",
        )
        .expect("dtd should be written");
        let uri = path_to_uri(&directory.join("doc.xml"));
        let mut fixture = Fixture::new();

        // Erreurs d'un fichier externe résumées sur l'identifiant système.
        let source = "<!DOCTYPE r SYSTEM \"broken.dtd\"><r><a/><b/></r>";
        let diagnostics = fixture.diagnostics(&uri, source);
        assert_eq!(
            kinds(&diagnostics),
            vec![
                kind("dtd-grammar", "externalGrammar", 1),
                kind("dtd-validation", "unexpectedElement", 1),
                kind("dtd-validation", "undeclaredElement", 1),
            ]
        );
        let summary = diagnostics[0]["message"].as_str().unwrap();
        assert!(summary.starts_with("2 erreurs dans la DTD"), "{summary}");
        assert!(summary.contains("(broken.dtd:3)"), "{summary}");
        assert_eq!(
            diagnostics[0]["range"],
            json!({"start": {"line": 0, "character": 20}, "end": {"line": 0, "character": 30}})
        );

        // Document valide sauf une entité inconnue.
        let source = "<!DOCTYPE r SYSTEM \"ok.dtd\"><r><a>&x; &amp;</a></r>";
        assert_eq!(
            kinds(&fixture.diagnostics(&uri, source)),
            vec![kind("xml-entity", "undefinedEntity", 1)]
        );
        // DTD distante : avertissement, pas de validation, entités inconnues
        // en avertissement.
        let source = "<!DOCTYPE r SYSTEM \"http://example.com/r.dtd\"><r><zzz>&x;</zzz></r>";
        let diagnostics = fixture.diagnostics(&uri, source);
        assert_eq!(
            kinds(&diagnostics),
            vec![
                kind("dtd-grammar", "externalLoad", 2),
                kind("xml-entity", "undefinedEntity", 2),
            ]
        );
        assert!(
            diagnostics[0]["message"]
                .as_str()
                .unwrap()
                .contains("xml.catalogs")
        );
        // Fichier absent : erreur.
        let source = "<!DOCTYPE r SYSTEM \"absent.dtd\"><r/>";
        assert_eq!(
            kinds(&fixture.diagnostics(&uri, source)),
            vec![kind("dtd-grammar", "externalLoad", 1)]
        );
        // Sans DOCTYPE : entités inconnues seulement.
        assert_eq!(
            kinds(&fixture.diagnostics(&uri, "<r>&nbsp;</r>")),
            vec![kind("xml-entity", "undefinedEntity", 1)]
        );
        // DOCTYPE interdit : aucune analyse DTD.
        let disallowed = ValidationSettings {
            disallow_doc_type_decl: true,
            ..ValidationSettings::default()
        };
        let source = "<!DOCTYPE r [<!ELEMENT r EMPTY>]><r>&x;<b/></r>";
        assert!(
            fixture
                .diagnostics_with(&uri, source, &disallowed)
                .is_empty()
        );
        // `&x;` inconnue rend le contenu de `r` opaque : pas d'erreur EMPTY.
        assert_eq!(
            kinds(&fixture.diagnostics(&uri, source)),
            vec![
                kind("xml-entity", "undefinedEntity", 1),
                kind("dtd-validation", "undeclaredElement", 1),
            ]
        );

        // Entités externes : vérifiées seulement avec resolveExternalEntities.
        let source = "<!DOCTYPE r [<!ELEMENT r ANY><!ENTITY chap SYSTEM \"chap.xml\"><!ENTITY here SYSTEM \"ok.dtd\">]><r>&chap;&here;</r>";
        assert!(fixture.diagnostics(&uri, source).is_empty());
        let resolving = ValidationSettings {
            resolve_external_entities: true,
            ..ValidationSettings::default()
        };
        assert_eq!(
            kinds(&fixture.diagnostics_with(&uri, source, &resolving)),
            vec![kind("xml-entity", "externalEntity", 2)]
        );

        // Fichier DTD ouvert : erreurs à leur place.
        let dtd_uri = path_to_uri(&directory.join("edit.dtd"));
        let diagnostics = fixture.diagnostics(
            &dtd_uri,
            "<!ELEMENT a (b,)>\n<!ENTITY % m SYSTEM \"https://example.com/m.ent\">\n%m;",
        );
        assert_eq!(
            kinds(&diagnostics),
            vec![
                kind("dtd-grammar", "dtdSyntax", 1),
                kind("dtd-grammar", "externalLoad", 2),
            ]
        );
        assert_eq!(
            diagnostics[0]["range"]["start"],
            json!({"line": 0, "character": 15})
        );
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn resolves_external_subsets_through_catalogs_and_open_buffers() {
        let directory = directory("catalog");
        fs::create_dir_all(directory.join("dtds")).unwrap();
        fs::write(directory.join("dtds/note.dtd"), "<!ELEMENT note (#PCDATA)>").unwrap();
        let catalog = directory.join("catalog.xml");
        fs::write(
            &catalog,
            "<catalog xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n  <public publicId=\"-//ACME//DTD Note//EN\" uri=\"dtds/note.dtd\"/>\n</catalog>",
        )
        .unwrap();
        let mut fixture = Fixture::new();
        fixture.catalogs.set_roots(vec![catalog]);
        let uri = path_to_uri(&directory.join("note.xml"));
        let source = "<!DOCTYPE note PUBLIC \"-//ACME//DTD Note//EN\" \"http://example.com/note.dtd\"><note><x/></note>";
        assert_eq!(
            kinds(&fixture.diagnostics(&uri, source)),
            vec![
                kind("dtd-validation", "unexpectedElement", 1),
                kind("dtd-validation", "undeclaredElement", 1),
            ]
        );
        let grammar = fixture.grammar(&uri, source).unwrap();
        assert_eq!(
            grammar.dtd.source_path(1),
            Some(directory.join("dtds/note.dtd").as_path())
        );

        // Tampon ouvert prioritaire sur le disque (même non enregistré).
        let buffer = directory.join("buffer.dtd");
        fixture.documents.insert(
            path_to_uri(&buffer),
            "<!ELEMENT note (x)><!ELEMENT x EMPTY>".to_owned(),
        );
        let source = "<!DOCTYPE note SYSTEM \"buffer.dtd\"><note><x/></note>";
        assert!(fixture.diagnostics(&uri, source).is_empty());
        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn offers_quick_fixes_for_dtd_problems() {
        let fixes = |source: &str, range: Range<usize>| {
            let mut fixture = Fixture::new();
            let uri = "file:///tmp/fix.xml";
            let grammar = fixture.grammar(uri, source);
            let context = json!({});
            let mut actions = Actions::new(uri, source, range, &context);
            code_actions(&mut actions, grammar.as_ref());
            actions
                .actions
                .iter()
                .map(|action| {
                    let edits = action["edit"]["changes"][uri].clone();
                    (
                        action["title"].as_str().unwrap().to_owned(),
                        crate::formatting::apply_edits(source, &edits),
                    )
                })
                .collect::<Vec<_>>()
        };

        let source = "<!DOCTYPE r [\n  <!ELEMENT r ANY>\n]>\n<r>&x;</r>";
        let offset = at(source, "&x;");
        assert_eq!(
            fixes(source, offset..offset),
            vec![(
                "Déclarer l'entité « &x; » dans le DOCTYPE".to_owned(),
                "<!DOCTYPE r [\n  <!ELEMENT r ANY>\n  <!ENTITY x \"\">\n]>\n<r>&x;</r>".to_owned()
            )]
        );
        let source = "<!DOCTYPE r SYSTEM \"r.dtd\"><r>&x;</r>";
        let offset = at(source, "&x;");
        assert_eq!(
            fixes(source, offset..offset)[0].1,
            "<!DOCTYPE r SYSTEM \"r.dtd\" [\n  <!ENTITY x \"\">\n]><r>&x;</r>"
        );
        let source = "<?xml version=\"1.0\"?>\n<r a=\"&x;\"/>";
        let offset = at(source, "&x;");
        assert_eq!(
            fixes(source, offset..offset)[0].1,
            "<?xml version=\"1.0\"?>\n<!DOCTYPE r [\n  <!ENTITY x \"\">\n]>\n<r a=\"&x;\"/>"
        );

        let source = "<!DOCTYPE r [<!ELEMENT r EMPTY><!ATTLIST r kind (a|b) #REQUIRED mode (x) #FIXED \"x\">]><r mode=\"y\"  />";
        assert_eq!(
            fixes(source, 0..source.len()),
            vec![
                (
                    "Ajouter l'attribut requis « kind »".to_owned(),
                    source.replace("mode=\"y\"  />", "mode=\"y\" kind=\"a\"  />")
                ),
                (
                    "Remplacer par la valeur fixe « x »".to_owned(),
                    source.replace("mode=\"y\"", "mode=\"x\"")
                ),
            ]
        );
    }

    #[test]
    fn lists_dtd_declarations_as_symbols() {
        let mut fixture = Fixture::new();
        let uri = "file:///tmp/symbols.dtd";
        let source = "<!ENTITY % common \"id ID #IMPLIED\">\n<!ELEMENT book (title)>\n<!ATTLIST book %common; lang CDATA #IMPLIED>\n<!ELEMENT title (#PCDATA)>\n<!ENTITY c \"©\">\n<!NOTATION gif SYSTEM \"g\">";
        let grammar = fixture.grammar(uri, source).unwrap();
        let symbols = document_symbols(&grammar, uri, source, true);
        let names = symbols
            .iter()
            .map(|symbol| symbol["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["%common", "book", "title", "&c", "gif"]);
        let children = symbols[1]["children"].as_array().unwrap();
        assert_eq!(children.len(), 2);
        assert_eq!(children[0]["name"], "id");
        // Attribut issu de `%common;` : sélection sur la référence.
        assert_eq!(
            children[0]["selectionRange"]["start"],
            json!({"line": 2, "character": 15})
        );
        assert_eq!(children[1]["detail"], "CDATA #IMPLIED");
        let flat = document_symbols(&grammar, uri, source, false);
        assert_eq!(flat.len(), 7);
        assert_eq!(flat[1]["containerName"], "book");
        assert_eq!(flat[1]["location"]["uri"], uri);
    }
}
