//! `textDocument/codeAction` : correctifs rapides et réécritures, comme
//! LemMinX.
//!
//! Correctifs rapides (`quickfix`), calculés à partir de la source et
//! proposés lorsque le problème touche l'étendue demandée ; les diagnostics
//! correspondants du contexte (même `code`, même `data.kind`/`data.rule`,
//! étendues qui se touchent) sont rattachés à l'action :
//!
//! - bonne formation ([`xml_core::wellformed`]) : balise fermante à renommer
//!   (ou balise ouvrante à renommer), balise fermante en trop à supprimer,
//!   élément à fermer (ou à rendre auto-fermant), `>`/`/>` manquant, attribut
//!   dupliqué à supprimer, valeur à entourer de guillemets, `&`/`<` à
//!   échapper ;
//! - schéma XSD ([`xsd_core::model`]) : attributs requis manquants ajoutés
//!   avec une valeur (fixe, par défaut, première valeur énumérée ou vide),
//!   valeur hors énumération remplacée par chacune des valeurs permises,
//!   élément inconnu renommé vers le nom attendu le plus proche
//!   (« Vous vouliez dire... ? », distance d'édition).
//!
//! Réécritures (`refactor.rewrite`) : `<a></a>` vide en `<a/>` et l'inverse.
//!
//! Actions de source (`source`) : lier le document à un schéma XSD
//! (`xmlns:xsi` + `xsi:noNamespaceSchemaLocation`, ou `xsi:schemaLocation`
//! pour une racine dans un espace de noms), vers les `.xsd` du même dossier
//! ou un emplacement à compléter.
//!
//! Le filtre `context.only` est respecté (préfixes de genres). Chaque action
//! porte un `WorkspaceEdit` (`changes`) : aucune résolution différée.
//!
//! Le module publie aussi les diagnostics de valeurs hors énumération
//! ([`enumeration_diagnostics`]), que la validation XSD simplifiée ne
//! vérifie pas.

use std::{fs, ops::Range, path::Path};

use serde_json::{Map, Value, json};
use xml_core::{
    tags::{XmlTag, XmlTagKind, qualified_name_parts},
    wellformed::{XmlProblemKind, check_well_formedness},
};
use xsd_core::model::{XsdModelSet, XsdTypeRef, XsdUse};

use crate::{
    encode_uri_path,
    hover::{self, Document, HoverContext},
    links::XSI_NAMESPACE,
    offset_at,
    selection::LineIndex,
    uri_to_path,
};

/// Genres d'actions annoncés dans `codeActionProvider.codeActionKinds`.
pub(crate) const CODE_ACTION_KINDS: [&str; 3] = ["quickfix", "refactor", "source"];
const QUICK_FIX: &str = "quickfix";
const REWRITE: &str = "refactor.rewrite";
const SOURCE: &str = "source";
/// Code des diagnostics de validation XSD.
const XSD_CODE: &str = "xsd-validation";
/// Nombre maximal de valeurs d'énumération proposées en remplacement.
const MAX_ENUMERATION_ACTIONS: usize = 20;
/// Nombre maximal de noms proposés pour un élément inconnu.
const MAX_SUGGESTIONS: usize = 3;
/// Nombre maximal de schémas voisins proposés pour la liaison.
const MAX_BOUND_SCHEMAS: usize = 5;
/// Emplacement proposé lorsqu'aucun schéma voisin n'existe.
const PLACEHOLDER_SCHEMA: &str = "schema.xsd";

/// Répond à `textDocument/codeAction` pour l'étendue `range` (offsets UTF-8)
/// du document `uri` ; `request_context` est le `CodeActionContext`.
pub(crate) fn code_actions(
    context: &mut HoverContext<'_>,
    uri: &str,
    source: &str,
    range: Range<usize>,
    request_context: &Value,
) -> Vec<Value> {
    let mut actions = Actions::new(uri, source, range, request_context);
    let document = Document::parse(source);
    if actions.wants(QUICK_FIX) {
        well_formedness_fixes(&mut actions);
        let models = hover::instance_models(context, uri, &document);
        if !models.set.models().is_empty() {
            schema_fixes(&mut actions, &document, &models.set);
        }
    }
    if actions.wants(REWRITE) {
        element_rewrites(&mut actions, &document);
    }
    if actions.wants(SOURCE) {
        bind_schema_actions(&mut actions, &document);
    }
    actions.actions
}

/// Diagnostics `xsd-validation` (`data.rule` = `invalidEnumeration`) des
/// valeurs d'attribut et contenus texte hors de l'énumération de leur type.
pub(crate) fn enumeration_diagnostics(
    context: &mut HoverContext<'_>,
    uri: &str,
    source: &str,
) -> Vec<Value> {
    let document = Document::parse(source);
    let models = hover::instance_models(context, uri, &document);
    if models.set.models().is_empty() {
        return Vec::new();
    }
    let lines = LineIndex::new(source);
    schema_problems(&document, &models.set, None)
        .into_iter()
        .filter_map(|problem| {
            let SchemaProblemKind::InvalidEnumeration { values, target } = problem.kind else {
                return None;
            };
            let mut listed = values
                .iter()
                .take(10)
                .map(|value| format!("`{value}`"))
                .collect::<Vec<_>>()
                .join(", ");
            if values.len() > 10 {
                listed.push_str(", …");
            }
            Some(json!({
                "range": {
                    "start": lines.position(source, problem.range.start),
                    "end": lines.position(source, problem.range.end),
                },
                "severity": 1,
                "source": "xml-lsp",
                "code": XSD_CODE,
                "data": {"category": "xsd", "kind": "validation", "rule": problem.rule},
                "message": format!(
                    "valeur `{}` hors énumération pour {target} (attendu : {listed})",
                    &source[problem.range.clone()]
                ),
            }))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Construction des actions
// ---------------------------------------------------------------------------

struct Actions<'a> {
    uri: &'a str,
    source: &'a str,
    range: Range<usize>,
    only: Option<Vec<String>>,
    diagnostics: Vec<(Range<usize>, &'a Value)>,
    lines: LineIndex,
    actions: Vec<Value>,
}

impl<'a> Actions<'a> {
    fn new(uri: &'a str, source: &'a str, range: Range<usize>, context: &'a Value) -> Self {
        let only = context.get("only").and_then(Value::as_array).map(|kinds| {
            kinds
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        });
        let diagnostics = context
            .get("diagnostics")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|diagnostic| {
                Some((lsp_range(source, diagnostic.get("range")?)?, diagnostic))
            })
            .collect();
        Self {
            uri,
            source,
            range,
            only,
            diagnostics,
            lines: LineIndex::new(source),
            actions: Vec::new(),
        }
    }

    /// Indique si le genre `kind` passe le filtre `context.only`.
    fn wants(&self, kind: &str) -> bool {
        self.only.as_ref().is_none_or(|only| {
            only.iter().any(|requested| {
                kind == requested
                    || kind
                        .strip_prefix(requested.as_str())
                        .is_some_and(|rest| rest.starts_with('.'))
            })
        })
    }

    /// Indique si un problème situé en `range` concerne l'étendue demandée.
    fn requested(&self, range: &Range<usize>) -> bool {
        touches(&self.range, range)
    }

    /// Diagnostics du contexte correspondant au problème : même `code`,
    /// `data.<key>` égal à `id` (ou absent) et étendues qui se touchent.
    fn matching(&self, code: &str, key: &str, id: &str, range: &Range<usize>) -> Vec<Value> {
        self.diagnostics
            .iter()
            .filter(|(diagnostic_range, diagnostic)| {
                diagnostic.get("code").and_then(Value::as_str) == Some(code)
                    && touches(diagnostic_range, range)
                    && diagnostic
                        .get("data")
                        .and_then(|data| data.get(key))
                        .and_then(Value::as_str)
                        .is_none_or(|value| value == id)
            })
            .map(|(_, diagnostic)| (*diagnostic).clone())
            .collect()
    }

    fn push(
        &mut self,
        title: String,
        kind: &str,
        edits: Vec<(Range<usize>, String)>,
        diagnostics: Vec<Value>,
        preferred: bool,
    ) {
        let edits = edits
            .into_iter()
            .map(|(range, text)| {
                json!({
                    "range": {
                        "start": self.lines.position(self.source, range.start),
                        "end": self.lines.position(self.source, range.end),
                    },
                    "newText": text,
                })
            })
            .collect::<Vec<_>>();
        let mut changes = Map::new();
        changes.insert(self.uri.to_owned(), Value::Array(edits));
        let mut action = json!({
            "title": title,
            "kind": kind,
            "edit": {"changes": changes},
        });
        if !diagnostics.is_empty() {
            action["diagnostics"] = Value::Array(diagnostics);
        }
        if preferred {
            action["isPreferred"] = Value::Bool(true);
        }
        self.actions.push(action);
    }
}

/// Étendues fermées qui se chevauchent ou se touchent.
fn touches(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start <= right.end && right.start <= left.end
}

fn lsp_range(source: &str, range: &Value) -> Option<Range<usize>> {
    let offset = |position: &Value| {
        let line = position.get("line")?.as_u64()? as usize;
        let character = position.get("character")?.as_u64()? as usize;
        Some(offset_at(source, line, character))
    };
    let start = offset(range.get("start")?)?;
    let end = offset(range.get("end")?)?;
    Some(start.min(end)..start.max(end))
}

// ---------------------------------------------------------------------------
// Bonne formation
// ---------------------------------------------------------------------------

fn well_formedness_fixes(actions: &mut Actions<'_>) {
    let source = actions.source;
    for problem in check_well_formedness(source) {
        let range = problem.range.clone();
        if !actions.requested(&range) {
            continue;
        }
        let code = if problem.kind.is_structural() {
            "xml-structure"
        } else {
            "xml-syntax"
        };
        let diagnostics = actions.matching(code, "kind", problem.kind.id(), &range);
        let text = &source[range.clone()];
        match &problem.kind {
            XmlProblemKind::MismatchedEndTag {
                expected,
                start_name,
            } => {
                actions.push(
                    format!("Remplacer </{text}> par </{expected}>"),
                    QUICK_FIX,
                    vec![(range, expected.clone())],
                    diagnostics.clone(),
                    true,
                );
                actions.push(
                    format!("Renommer <{expected}> en <{text}>"),
                    QUICK_FIX,
                    vec![(start_name.clone(), text.to_owned())],
                    diagnostics,
                    false,
                );
            }
            XmlProblemKind::UnmatchedEndTag { tag } => actions.push(
                format!("Supprimer la balise fermante </{text}>"),
                QUICK_FIX,
                vec![(tag.clone(), String::new())],
                diagnostics,
                true,
            ),
            XmlProblemKind::UnclosedElement { insert_at, tag_end } => {
                actions.push(
                    format!("Fermer <{text}> avec </{text}>"),
                    QUICK_FIX,
                    vec![(*insert_at..*insert_at, format!("</{text}>"))],
                    diagnostics.clone(),
                    true,
                );
                actions.push(
                    format!("Transformer <{text}> en élément auto-fermant"),
                    QUICK_FIX,
                    vec![(*tag_end..*tag_end + 1, "/>".to_owned())],
                    diagnostics,
                    false,
                );
            }
            XmlProblemKind::UnclosedTag {
                insert_at,
                start_tag,
            } => {
                actions.push(
                    "Terminer la balise par `>`".to_owned(),
                    QUICK_FIX,
                    vec![(*insert_at..*insert_at, ">".to_owned())],
                    diagnostics.clone(),
                    true,
                );
                if *start_tag {
                    actions.push(
                        "Terminer la balise par `/>`".to_owned(),
                        QUICK_FIX,
                        vec![(*insert_at..*insert_at, "/>".to_owned())],
                        diagnostics,
                        false,
                    );
                }
            }
            XmlProblemKind::DuplicateAttribute { removal } => actions.push(
                format!("Supprimer l'attribut dupliqué {text}"),
                QUICK_FIX,
                vec![(removal.clone(), String::new())],
                diagnostics,
                true,
            ),
            XmlProblemKind::UnquotedAttributeValue => {
                let quote = if text.contains('"') { '\'' } else { '"' };
                actions.push(
                    "Entourer la valeur de guillemets".to_owned(),
                    QUICK_FIX,
                    vec![(range, format!("{quote}{text}{quote}"))],
                    diagnostics,
                    true,
                );
            }
            XmlProblemKind::UnescapedCharacter { character } => {
                let entity = if *character == '<' { "&lt;" } else { "&amp;" };
                actions.push(
                    format!("Remplacer `{character}` par `{entity}`"),
                    QUICK_FIX,
                    vec![(range, entity.to_owned())],
                    diagnostics,
                    true,
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Schéma XSD
// ---------------------------------------------------------------------------

/// Problème de validation XSD localisé, avec les données de correction.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SchemaProblem {
    kind: SchemaProblemKind,
    /// Étendue concernée : balise ouvrante (attributs manquants), valeur
    /// (énumération) ou nom de la balise ouvrante (élément inconnu).
    range: Range<usize>,
    /// Règle (`data.rule`) des diagnostics correspondants.
    rule: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SchemaProblemKind {
    /// Attributs requis absents : `(nom qualifié, valeur proposée)`, à
    /// insérer en `insert_at` (après le dernier attribut).
    MissingAttributes {
        insert_at: usize,
        attributes: Vec<(String, String)>,
    },
    /// Valeur hors de l'énumération `values` ; `target` décrit l'attribut ou
    /// l'élément pour le message.
    InvalidEnumeration { values: Vec<String>, target: String },
    /// Élément absent du modèle de contenu : noms locaux à remplacer
    /// (balises ouvrante et fermante) et noms proches proposés.
    UnknownElement {
        names: Vec<Range<usize>>,
        suggestions: Vec<String>,
    },
}

fn schema_fixes(actions: &mut Actions<'_>, document: &Document<'_>, set: &XsdModelSet) {
    let source = actions.source;
    let range = actions.range.clone();
    for problem in schema_problems(document, set, Some(&range)) {
        if !actions.requested(&problem.range) {
            continue;
        }
        let diagnostics = actions.matching(XSD_CODE, "rule", problem.rule, &problem.range);
        match problem.kind {
            SchemaProblemKind::MissingAttributes {
                insert_at,
                attributes,
            } => {
                let names = attributes
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let title = if attributes.len() == 1 {
                    format!("Ajouter l'attribut requis {names}")
                } else {
                    format!("Ajouter les attributs requis {names}")
                };
                let text = attributes
                    .iter()
                    .map(|(name, value)| format!(" {name}=\"{}\"", escape(value)))
                    .collect::<String>();
                actions.push(
                    title,
                    QUICK_FIX,
                    vec![(insert_at..insert_at, text)],
                    diagnostics,
                    true,
                );
            }
            SchemaProblemKind::InvalidEnumeration { values, .. } => {
                let current = &source[problem.range.clone()];
                let preferred = closest(current, values.iter().map(String::as_str))
                    .first()
                    .map(|(value, _)| *value);
                for value in values.iter().take(MAX_ENUMERATION_ACTIONS) {
                    actions.push(
                        format!("Remplacer par `{value}`"),
                        QUICK_FIX,
                        vec![(problem.range.clone(), escape(value))],
                        diagnostics.clone(),
                        values.len() == 1 || preferred == Some(value.as_str()),
                    );
                }
            }
            SchemaProblemKind::UnknownElement { names, suggestions } => {
                for (index, suggestion) in suggestions.iter().enumerate() {
                    actions.push(
                        format!("Vous vouliez dire <{suggestion}> ?"),
                        QUICK_FIX,
                        names
                            .iter()
                            .map(|range| (range.clone(), suggestion.clone()))
                            .collect(),
                        diagnostics.clone(),
                        index == 0,
                    );
                }
            }
        }
    }
}

/// Relève, pour chaque élément du document (ou seulement ceux qui touchent
/// `within`, les requêtes de code étant envoyées à chaque déplacement du
/// curseur), les attributs requis absents, les valeurs hors énumération et
/// les éléments inconnus du modèle.
fn schema_problems(
    document: &Document<'_>,
    set: &XsdModelSet,
    within: Option<&Range<usize>>,
) -> Vec<SchemaProblem> {
    let source = document.source;
    let mut problems = Vec::new();
    for (index, element) in document.tree.elements().iter().enumerate() {
        if within.is_some_and(|range| !touches(range, &element.range())) {
            continue;
        }
        let name = document.element_name(index);
        let (namespace, local) = document.split(index, name);
        if let Some(problem) = unknown_element(document, set, index, namespace, local) {
            problems.push(problem);
            continue;
        }
        let Some(resolved) = set.resolve_element_path(&document.instance_path(index)) else {
            continue;
        };
        let Some(element_type) = resolved.element_type else {
            continue;
        };
        if element.start_tag.closed
            && let Some(problem) = missing_attributes(document, set, index, element_type)
        {
            problems.push(problem);
        }
        for attribute in &document.attributes[index] {
            let attribute_name = attribute.name(source);
            let Some(value) = attribute.value.clone() else {
                continue;
            };
            let (attribute_namespace, attribute_local) =
                hover::attribute_namespace(document, index, attribute_name);
            if attribute_name == "xmlns"
                || attribute_name.starts_with("xmlns:")
                || attribute_namespace == Some(XSI_NAMESPACE)
            {
                continue;
            }
            let Some(declaration) = set
                .resolve_attribute(Some(&resolved), attribute_namespace, attribute_local)
                .map(|attribute| attribute.declaration)
            else {
                continue;
            };
            let Some(values) = set
                .attribute_type(declaration)
                .and_then(|value_type| enumeration_values(set, value_type))
            else {
                continue;
            };
            if !is_enumerated(&source[value.clone()], &values) {
                problems.push(SchemaProblem {
                    kind: SchemaProblemKind::InvalidEnumeration {
                        values,
                        target: format!("@{attribute_name} sur <{name}>"),
                    },
                    range: value,
                    rule: "invalidEnumeration",
                });
            }
        }
        if let Some(problem) = text_enumeration(document, set, index, element_type) {
            problems.push(problem);
        }
    }
    problems
}

fn unknown_element(
    document: &Document<'_>,
    set: &XsdModelSet,
    index: usize,
    namespace: Option<&str>,
    local: &str,
) -> Option<SchemaProblem> {
    let element = &document.tree.elements()[index];
    let (candidates, rule) = match element.parent {
        None => {
            if set.global_element(namespace, local).is_some() {
                return None;
            }
            let names = set
                .global_elements()
                .filter(|candidate| !candidate.item.is_abstract)
                .map(|candidate| candidate.item.name.clone())
                .collect::<Vec<_>>();
            (names, "unknownRoot")
        }
        Some(parent) => {
            let parent_type = set
                .resolve_element_path(&document.instance_path(parent))?
                .element_type?;
            let names = set
                .child_elements(parent_type)
                .into_iter()
                .map(|candidate| candidate.item.name.clone())
                .collect::<Vec<_>>();
            if names.iter().any(|candidate| candidate == local) {
                return None;
            }
            (names, "unexpectedElement")
        }
    };
    let suggestions = closest(local, candidates.iter().map(String::as_str))
        .into_iter()
        .take(MAX_SUGGESTIONS)
        .map(|(name, _)| name.to_owned())
        .collect::<Vec<_>>();
    if suggestions.is_empty() {
        return None;
    }
    let source = document.source;
    let names = std::iter::once(&element.start_tag)
        .chain(element.end_tag.as_ref())
        .map(|tag: &XmlTag| qualified_name_parts(source, tag.name.clone()).1)
        .collect();
    Some(SchemaProblem {
        kind: SchemaProblemKind::UnknownElement { names, suggestions },
        range: element.start_tag.name.clone(),
        rule,
    })
}

fn missing_attributes(
    document: &Document<'_>,
    set: &XsdModelSet,
    index: usize,
    element_type: XsdTypeRef<'_>,
) -> Option<SchemaProblem> {
    let source = document.source;
    let present = document.attributes[index]
        .iter()
        .map(|attribute| {
            let name = attribute.name(source);
            name.rsplit_once(':').map_or(name, |(_, local)| local)
        })
        .collect::<Vec<_>>();
    let mut attributes = Vec::new();
    for usage in set.attribute_uses(element_type) {
        if usage.item.usage != XsdUse::Required {
            continue;
        }
        let declaration = usage
            .item
            .reference
            .as_ref()
            .and_then(|reference| {
                set.global_attribute(reference.namespace.as_deref(), &reference.local)
            })
            .unwrap_or(usage);
        let local = declaration.item.name.as_str();
        if present.contains(&local) || attributes.iter().any(|(name, _)| name == local) {
            continue;
        }
        let name = match &declaration.item.namespace {
            None => local.to_owned(),
            Some(namespace) => match prefix_for(document, index, namespace) {
                Some(prefix) => format!("{prefix}:{local}"),
                // Pas de préfixe déclaré pour l'espace de noms : insertion impossible.
                None => continue,
            },
        };
        let value = usage
            .item
            .fixed
            .clone()
            .or_else(|| declaration.item.fixed.clone())
            .or_else(|| usage.item.default.clone())
            .or_else(|| declaration.item.default.clone())
            .or_else(|| {
                set.attribute_type(declaration)
                    .and_then(|value_type| enumeration_values(set, value_type))
                    .and_then(|values| values.into_iter().next())
            })
            .unwrap_or_default();
        attributes.push((name, value));
    }
    if attributes.is_empty() {
        return None;
    }
    let tag = &document.tree.elements()[index].start_tag;
    let close = if tag.kind == XmlTagKind::SelfClosing {
        2
    } else {
        1
    };
    let insert_at = source[..tag.range.end - close]
        .trim_end()
        .len()
        .max(tag.name.end);
    Some(SchemaProblem {
        kind: SchemaProblemKind::MissingAttributes {
            insert_at,
            attributes,
        },
        range: tag.range.clone(),
        rule: "missingAttribute",
    })
}

fn text_enumeration(
    document: &Document<'_>,
    set: &XsdModelSet,
    index: usize,
    element_type: XsdTypeRef<'_>,
) -> Option<SchemaProblem> {
    let source = document.source;
    let element = &document.tree.elements()[index];
    if element_type
        .definition
        .is_some_and(|definition| definition.complex && !definition.simple_content)
    {
        return None;
    }
    let content = element.content_range()?;
    let text = &source[content.clone()];
    // Contenu mixte, CDATA ou commentaires : pas de contrôle.
    if text.contains('<') || is_nil(document, index) {
        return None;
    }
    let values = enumeration_values(set, element_type)?;
    let trimmed = text.trim();
    if is_enumerated(trimmed, &values) {
        return None;
    }
    let start = content.start + (text.len() - text.trim_start().len());
    Some(SchemaProblem {
        kind: SchemaProblemKind::InvalidEnumeration {
            values,
            target: format!("<{}>", document.element_name(index)),
        },
        range: start..start + trimmed.len(),
        rule: "invalidEnumeration",
    })
}

fn is_nil(document: &Document<'_>, index: usize) -> bool {
    let source = document.source;
    document.attributes[index].iter().any(|attribute| {
        let name = attribute.name(source);
        let (namespace, local) = hover::attribute_namespace(document, index, name);
        namespace == Some(XSI_NAMESPACE)
            && local == "nil"
            && matches!(attribute.value(source).map(str::trim), Some("true" | "1"))
    })
}

/// Valeurs énumérées d'un type simple (facettes cumulées le long des
/// restrictions) ; `None` sans énumération ou pour une liste.
fn enumeration_values(set: &XsdModelSet, value_type: XsdTypeRef<'_>) -> Option<Vec<String>> {
    let info = set.simple_type_info(value_type);
    if info.item_type.is_some() || info.facets.enumerations.is_empty() {
        return None;
    }
    Some(
        info.facets
            .enumerations
            .into_iter()
            .map(|enumeration| enumeration.value)
            .collect(),
    )
}

/// Compare une valeur brute (entités prédéfinies décodées, espaces réduits
/// en repli) aux valeurs énumérées.
fn is_enumerated(raw: &str, values: &[String]) -> bool {
    let value = raw
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    values
        .iter()
        .any(|candidate| *candidate == value || *candidate == collapsed)
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

/// Préfixe déclaré (`xmlns:prefix`) pour `namespace` dans la portée de
/// l'élément.
fn prefix_for<'a>(document: &Document<'a>, index: usize, namespace: &str) -> Option<&'a str> {
    let source = document.source;
    std::iter::once(index)
        .chain(document.tree.ancestors(index))
        .find_map(|element| {
            document.attributes[element].iter().find_map(|attribute| {
                let prefix = attribute.name(source).strip_prefix("xmlns:")?;
                (attribute.value(source) == Some(namespace)).then_some(prefix)
            })
        })
}

/// Candidats proches de `name` (distance d'édition avec transpositions, sans
/// tenir compte de la casse), triés du plus proche au plus lointain.
fn closest<'c>(name: &str, candidates: impl Iterator<Item = &'c str>) -> Vec<(&'c str, usize)> {
    let mut found = candidates
        .filter(|candidate| *candidate != name)
        .filter_map(|candidate| {
            let distance = edit_distance(&name.to_lowercase(), &candidate.to_lowercase());
            let limit = (name.chars().count().max(candidate.chars().count()) / 3).max(1);
            (distance <= limit).then_some((candidate, distance))
        })
        .collect::<Vec<_>>();
    found.sort_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(right.0)));
    found.dedup();
    found
}

/// Distance d'édition « optimal string alignment » (insertion, suppression,
/// substitution, transposition de caractères adjacents).
pub(crate) fn edit_distance(left: &str, right: &str) -> usize {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    let width = right.len() + 1;
    let mut rows = vec![0usize; (left.len() + 1) * width];
    for (row, value) in rows.iter_mut().step_by(width).enumerate() {
        *value = row;
    }
    for (column, value) in rows.iter_mut().take(width).enumerate() {
        *value = column;
    }
    for i in 1..=left.len() {
        for j in 1..=right.len() {
            let cost = usize::from(left[i - 1] != right[j - 1]);
            let mut best = (rows[(i - 1) * width + j] + 1)
                .min(rows[i * width + j - 1] + 1)
                .min(rows[(i - 1) * width + j - 1] + cost);
            if i > 1 && j > 1 && left[i - 1] == right[j - 2] && left[i - 2] == right[j - 1] {
                best = best.min(rows[(i - 2) * width + j - 2] + 1);
            }
            rows[i * width + j] = best;
        }
    }
    rows[left.len() * width + right.len()]
}

// ---------------------------------------------------------------------------
// Réécritures
// ---------------------------------------------------------------------------

/// `<a ...></a>` (contenu vide ou blanc) <-> `<a .../>` pour l'élément dont
/// une balise touche le début de l'étendue demandée.
fn element_rewrites(actions: &mut Actions<'_>, document: &Document<'_>) {
    let source = actions.source;
    let Some(index) = document.tree.innermost_element_at(actions.range.start) else {
        return;
    };
    let element = &document.tree.elements()[index];
    let start_tag = &element.start_tag;
    let on_tag = actions.requested(&start_tag.range)
        || element
            .end_tag
            .as_ref()
            .is_some_and(|tag| actions.requested(&tag.range));
    if !on_tag || !start_tag.closed {
        return;
    }
    let name = element.name(source);
    match (&start_tag.kind, &element.end_tag) {
        (XmlTagKind::SelfClosing, _) => {
            let slash = start_tag.range.end - 2;
            let from = source[..slash].trim_end().len().max(start_tag.name.end);
            actions.push(
                format!("Développer <{name}/> en <{name}></{name}>"),
                REWRITE,
                vec![(from..start_tag.range.end, format!("></{name}>"))],
                Vec::new(),
                false,
            );
        }
        (XmlTagKind::Start, Some(end_tag))
            if end_tag.closed
                && source[start_tag.range.end..end_tag.range.start]
                    .trim()
                    .is_empty() =>
        {
            actions.push(
                format!("Convertir <{name}></{name}> en élément auto-fermant <{name}/>"),
                REWRITE,
                vec![(start_tag.range.end - 1..end_tag.range.end, "/>".to_owned())],
                Vec::new(),
                false,
            );
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Liaison à un schéma
// ---------------------------------------------------------------------------

/// Propose de lier un document non lié à un schéma XSD voisin (ou à un
/// emplacement à compléter).
fn bind_schema_actions(actions: &mut Actions<'_>, document: &Document<'_>) {
    let source = actions.source;
    let Some(root) = document
        .tree
        .elements()
        .iter()
        .position(|element| element.parent.is_none())
    else {
        return;
    };
    let tag = &document.tree.elements()[root].start_tag;
    if !tag.closed || document.is_schema() {
        return;
    }
    let mut xsi_prefix = None;
    for attribute in &document.attributes[root] {
        let name = attribute.name(source);
        if let Some(prefix) = name.strip_prefix("xmlns:")
            && attribute.value(source) == Some(XSI_NAMESPACE)
        {
            xsi_prefix = Some(prefix);
        }
        let (namespace, local) = hover::attribute_namespace(document, root, name);
        if namespace == Some(XSI_NAMESPACE)
            && matches!(local, "schemaLocation" | "noNamespaceSchemaLocation")
        {
            return;
        }
    }
    let (namespace, _) = document.split(root, document.element_name(root));
    let close = if tag.kind == XmlTagKind::SelfClosing {
        2
    } else {
        1
    };
    let insert_at = source[..tag.range.end - close]
        .trim_end()
        .len()
        .max(tag.name.end);
    let schemas = sibling_schemas(&uri_to_path(actions.uri));
    let placeholder = schemas.is_empty();
    let locations = if placeholder {
        vec![PLACEHOLDER_SCHEMA.to_owned()]
    } else {
        schemas
    };
    for file in locations {
        let prefix = xsi_prefix.unwrap_or("xsi");
        let mut text = String::new();
        if xsi_prefix.is_none() {
            text.push_str(&format!(" xmlns:xsi=\"{XSI_NAMESPACE}\""));
        }
        let location = encode_uri_path(&file);
        match namespace {
            Some(namespace) => text.push_str(&format!(
                " {prefix}:schemaLocation=\"{} {location}\"",
                escape(namespace)
            )),
            None => text.push_str(&format!(
                " {prefix}:noNamespaceSchemaLocation=\"{location}\""
            )),
        }
        let title = if placeholder {
            format!("Lier le document à un schéma XSD ({file} à compléter)")
        } else {
            format!("Lier le document au schéma XSD {file}")
        };
        actions.push(
            title,
            SOURCE,
            vec![(insert_at..insert_at, text)],
            Vec::new(),
            false,
        );
    }
}

/// Fichiers `.xsd` du dossier du document : celui qui porte le même nom en
/// premier, puis par ordre alphabétique.
fn sibling_schemas(document_path: &Path) -> Vec<String> {
    let Some(directory) = document_path.parent() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let stem = document_path.file_stem();
    let mut schemas = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("xsd"))
        })
        .filter_map(|path| {
            let same_stem = path.file_stem() == stem;
            Some((!same_stem, path.file_name()?.to_str()?.to_owned()))
        })
        .collect::<Vec<_>>();
    schemas.sort();
    schemas
        .into_iter()
        .take(MAX_BOUND_SCHEMAS)
        .map(|(_, name)| name)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{formatting::apply_edits, hover::ModelCache, path_to_uri};

    const SCHEMA: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="catalog">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="book" maxOccurs="unbounded">
          <xs:complexType>
            <xs:sequence>
              <xs:element name="title" type="xs:string"/>
              <xs:element name="format" type="Format" minOccurs="0"/>
            </xs:sequence>
            <xs:attribute name="isbn" type="xs:string" use="required"/>
            <xs:attribute name="lang" type="xs:language" use="required" default="fr"/>
            <xs:attribute name="status" type="Status" use="required"/>
            <xs:attribute name="color" type="Color"/>
          </xs:complexType>
        </xs:element>
      </xs:sequence>
    </xs:complexType>
  </xs:element>
  <xs:simpleType name="Format">
    <xs:restriction base="xs:token">
      <xs:enumeration value="hardcover"/>
      <xs:enumeration value="paperback"/>
    </xs:restriction>
  </xs:simpleType>
  <xs:simpleType name="Status"><xs:restriction base="xs:string"><xs:enumeration value="draft"/><xs:enumeration value="published"/></xs:restriction></xs:simpleType>
  <xs:simpleType name="Color"><xs:restriction base="xs:string"><xs:enumeration value="red"/><xs:enumeration value="green"/><xs:enumeration value="blue"/></xs:restriction></xs:simpleType>
</xs:schema>"#;

    const BOUND: &str = "<catalog xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"catalog.xsd\">";

    struct Fixture {
        directory: std::path::PathBuf,
        documents: HashMap<String, String>,
        cache: ModelCache,
    }

    impl Fixture {
        fn new(name: &str, schema: Option<&str>) -> Self {
            let directory = std::env::temp_dir().join(format!(
                "xml-lsp-code-actions-{name}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&directory);
            fs::create_dir_all(&directory).expect("directory should be created");
            if let Some(schema) = schema {
                fs::write(directory.join("catalog.xsd"), schema).expect("schema should be written");
            }
            Self {
                directory,
                documents: HashMap::new(),
                cache: ModelCache::new(),
            }
        }

        fn uri(&self) -> String {
            path_to_uri(&self.directory.join("catalog.xml"))
        }

        fn actions(&mut self, source: &str, range: Range<usize>, context: Value) -> Vec<Value> {
            let uri = self.uri();
            self.documents.insert(uri.clone(), source.to_owned());
            let mut hover = HoverContext {
                documents: &self.documents,
                cache: &mut self.cache,
                associated_schemas: Vec::new(),
                catalogs: &crate::catalog::Catalogs::default(),
            };
            code_actions(&mut hover, &uri, source, range, &context)
        }

        fn diagnostics(&mut self, source: &str) -> Vec<Value> {
            let uri = self.uri();
            let mut hover = HoverContext {
                documents: &self.documents,
                cache: &mut self.cache,
                associated_schemas: Vec::new(),
                catalogs: &crate::catalog::Catalogs::default(),
            };
            enumeration_diagnostics(&mut hover, &uri, source)
        }

        /// Applique l'action intitulée `title`.
        fn apply(&self, source: &str, actions: &[Value], title: &str) -> String {
            let action = actions
                .iter()
                .find(|action| action["title"] == title)
                .unwrap_or_else(|| panic!("action {title:?} missing in {actions:#?}"));
            apply_edits(source, &action["edit"]["changes"][self.uri()])
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    fn at(source: &str, needle: &str) -> Range<usize> {
        let start = source
            .find(needle)
            .unwrap_or_else(|| panic!("{needle:?} missing"));
        start..start
    }

    fn titles(actions: &[Value]) -> Vec<&str> {
        actions
            .iter()
            .map(|action| action["title"].as_str().unwrap())
            .collect()
    }

    fn quick_fixes() -> Value {
        json!({"only": ["quickfix"]})
    }

    #[test]
    fn fixes_mismatched_and_unmatched_end_tags() {
        let mut fixture = Fixture::new("tags", None);
        let source = "<root>\n  <child></chidl>\n</root>";
        let actions = fixture.actions(source, at(source, "hidl"), quick_fixes());
        assert_eq!(
            titles(&actions),
            vec![
                "Remplacer </chidl> par </child>",
                "Renommer <child> en <chidl>"
            ]
        );
        assert_eq!(actions[0]["isPreferred"], true);
        assert_eq!(actions[0]["kind"], "quickfix");
        assert_eq!(
            fixture.apply(source, &actions, "Remplacer </chidl> par </child>"),
            "<root>\n  <child></child>\n</root>"
        );
        assert_eq!(
            fixture.apply(source, &actions, "Renommer <child> en <chidl>"),
            "<root>\n  <chidl></chidl>\n</root>"
        );
        // Hors de la balise fautive : aucun correctif.
        assert!(fixture.actions(source, 0..0, quick_fixes()).is_empty());

        let source = "<root></extra></root>";
        let actions = fixture.actions(source, at(source, "extra"), quick_fixes());
        assert_eq!(
            fixture.apply(source, &actions, "Supprimer la balise fermante </extra>"),
            "<root></root>"
        );
    }

    #[test]
    fn closes_unclosed_elements_and_tags() {
        let mut fixture = Fixture::new("close", None);
        let source = "<root>\n  <item>\n  <other/>\n</root>";
        let actions = fixture.actions(source, at(source, "item"), quick_fixes());
        assert_eq!(
            fixture.apply(source, &actions, "Fermer <item> avec </item>"),
            "<root>\n  <item>\n  <other/>\n</item></root>"
        );
        assert_eq!(
            fixture.apply(
                source,
                &actions,
                "Transformer <item> en élément auto-fermant"
            ),
            "<root>\n  <item/>\n  <other/>\n</root>"
        );

        let source = "<root><a x=\"1\" \n</root>";
        let actions = fixture.actions(source, at(source, "a x"), quick_fixes());
        assert_eq!(
            fixture.apply(source, &actions, "Terminer la balise par `/>`"),
            "<root><a x=\"1\"/> \n</root>"
        );
        assert_eq!(
            fixture.apply(source, &actions, "Terminer la balise par `>`"),
            "<root><a x=\"1\"> \n</root>"
        );
    }

    #[test]
    fn fixes_attributes_and_unescaped_characters() {
        let mut fixture = Fixture::new("syntax", None);
        let source = "<a x=\"1\" y=2 x='3'>1 < 2 & 3</a>";
        let actions = fixture.actions(source, 0..source.len(), quick_fixes());
        assert_eq!(
            titles(&actions),
            vec![
                "Entourer la valeur de guillemets",
                "Supprimer l'attribut dupliqué x",
                "Remplacer `<` par `&lt;`",
                "Remplacer `&` par `&amp;`",
            ]
        );
        let mut fixed = source.to_owned();
        for title in titles(&actions) {
            let actions = fixture.actions(&fixed, 0..fixed.len(), quick_fixes());
            fixed = fixture.apply(&fixed, &actions, title);
        }
        assert_eq!(fixed, "<a x=\"1\" y=\"2\">1 &lt; 2 &amp; 3</a>");
        assert!(check_well_formedness(&fixed).is_empty());
    }

    #[test]
    fn attaches_matching_context_diagnostics_only() {
        let mut fixture = Fixture::new("diagnostics", None);
        let source = "<a>é & b</a>";
        let range = |start: u32, end: u32| json!({"start": {"line": 0, "character": start}, "end": {"line": 0, "character": end}});
        let matching = json!({
            "range": range(5, 6),
            "code": "xml-syntax",
            "data": {"category": "xml", "kind": "unescapedCharacter"},
            "message": "caractère `&` non échappé",
        });
        let other = json!({"range": range(5, 6), "code": "xsd-validation", "message": "autre"});
        // `&` : colonne UTF-16 5, octet 6 (é occupe deux octets).
        let actions = fixture.actions(
            source,
            6..6,
            json!({"only": ["quickfix"], "diagnostics": [matching.clone(), other]}),
        );
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0]["diagnostics"], json!([matching]));
    }

    #[test]
    fn filters_actions_by_requested_kinds() {
        let mut fixture = Fixture::new("only", None);
        let source = "<root><empty></empty> & </root>";
        let everything = fixture.actions(source, at(source, "empty>"), json!({}));
        let kinds = everything
            .iter()
            .map(|action| action["kind"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(kinds, vec!["refactor.rewrite", "source"]);
        let refactors =
            fixture.actions(source, at(source, "empty>"), json!({"only": ["refactor"]}));
        assert_eq!(refactors.len(), 1);
        assert!(
            fixture
                .actions(
                    source,
                    at(source, "empty>"),
                    json!({"only": ["refactor.extract"]})
                )
                .is_empty()
        );
        assert!(
            fixture
                .actions(source, at(source, "empty>"), json!({"only": ["sourceX"]}))
                .is_empty()
        );
    }

    #[test]
    fn converts_between_empty_and_self_closing_elements() {
        let mut fixture = Fixture::new("rewrite", None);
        let rewrite = json!({"only": ["refactor.rewrite"]});
        let source = "<root>\n  <item id=\"1\">\n  </item>\n</root>";
        let actions = fixture.actions(source, at(source, "/item"), rewrite.clone());
        let converted = fixture.apply(
            source,
            &actions,
            "Convertir <item></item> en élément auto-fermant <item/>",
        );
        assert_eq!(converted, "<root>\n  <item id=\"1\"/>\n</root>");
        let actions = fixture.actions(&converted, at(&converted, "item"), rewrite.clone());
        assert_eq!(
            fixture.apply(&converted, &actions, "Développer <item/> en <item></item>"),
            "<root>\n  <item id=\"1\"></item>\n</root>"
        );
        let source = "<p:a xmlns:p=\"urn:p\" />";
        let actions = fixture.actions(source, 1..1, rewrite.clone());
        assert_eq!(
            fixture.apply(source, &actions, "Développer <p:a/> en <p:a></p:a>"),
            "<p:a xmlns:p=\"urn:p\"></p:a>"
        );
        // Élément non vide ou curseur dans le contenu : pas de réécriture.
        let source = "<a>text</a>";
        assert!(fixture.actions(source, 1..1, rewrite.clone()).is_empty());
        assert!(fixture.actions(source, 5..5, rewrite).is_empty());
    }

    #[test]
    fn adds_missing_required_attributes() {
        let mut fixture = Fixture::new("required", Some(SCHEMA));
        let source =
            format!("{BOUND}\n  <book isbn=\"1\"><title>T</title></book>\n  <book />\n</catalog>");
        let first = source.find("<book").unwrap();
        let actions = fixture.actions(&source, first + 3..first + 3, quick_fixes());
        assert_eq!(
            titles(&actions),
            vec!["Ajouter les attributs requis lang, status"]
        );
        let fixed = fixture.apply(
            &source,
            &actions,
            "Ajouter les attributs requis lang, status",
        );
        assert!(fixed.contains("<book isbn=\"1\" lang=\"fr\" status=\"draft\">"));

        let second = source.rfind("<book").unwrap();
        let actions = fixture.actions(&source, second + 7..second + 7, quick_fixes());
        let fixed = fixture.apply(
            &source,
            &actions,
            "Ajouter les attributs requis isbn, lang, status",
        );
        assert!(fixed.contains("<book isbn=\"\" lang=\"fr\" status=\"draft\" />"));
    }

    #[test]
    fn replaces_values_outside_enumerations() {
        let mut fixture = Fixture::new("enumeration", Some(SCHEMA));
        let source = format!(
            "{BOUND}\n  <book isbn=\"1\" lang=\"fr\" status=\"draft\" color=\"gren\"><title>T</title><format> hardcovers </format></book>\n</catalog>"
        );
        let diagnostics = fixture.diagnostics(&source);
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0]["code"], "xsd-validation");
        assert_eq!(diagnostics[0]["data"]["rule"], "invalidEnumeration");
        assert_eq!(
            diagnostics[0]["message"],
            "valeur `gren` hors énumération pour @color sur <book> (attendu : `red`, `green`, `blue`)"
        );
        assert_eq!(
            diagnostics[1]["message"],
            "valeur `hardcovers` hors énumération pour <format> (attendu : `hardcover`, `paperback`)"
        );

        let color = source.find("gren").unwrap();
        let actions = fixture.actions(
            &source,
            color..color,
            json!({"only": ["quickfix"], "diagnostics": [diagnostics[0]]}),
        );
        assert_eq!(
            titles(&actions),
            vec![
                "Remplacer par `red`",
                "Remplacer par `green`",
                "Remplacer par `blue`"
            ]
        );
        assert_eq!(actions[1]["isPreferred"], true);
        assert!(actions[0].get("isPreferred").is_none());
        assert_eq!(actions[1]["diagnostics"], json!([diagnostics[0]]));
        let fixed = fixture.apply(&source, &actions, "Remplacer par `green`");
        assert!(fixed.contains("color=\"green\""));

        let format = source.find("hardcovers").unwrap();
        let actions = fixture.actions(&source, format + 2..format + 2, quick_fixes());
        let fixed = fixture.apply(&source, &actions, "Remplacer par `hardcover`");
        assert!(fixed.contains("<format> hardcover </format>"));
        assert!(fixture.diagnostics(&fixed).len() == 1);
    }

    #[test]
    fn suggests_close_element_names() {
        let mut fixture = Fixture::new("unknown", Some(SCHEMA));
        let source = format!(
            "{BOUND}\n  <book isbn=\"1\" lang=\"fr\" status=\"draft\"><titel>T</titel></book>\n</catalog>"
        );
        let unknown = source.find("titel").unwrap();
        let actions = fixture.actions(&source, unknown..unknown, quick_fixes());
        assert_eq!(titles(&actions), vec!["Vous vouliez dire <title> ?"]);
        assert_eq!(actions[0]["isPreferred"], true);
        let fixed = fixture.apply(&source, &actions, "Vous vouliez dire <title> ?");
        assert!(fixed.contains("<title>T</title>"));

        let source = source
            .replace("<catalog xmlns", "<catalgo xmlns")
            .replace("</catalog>", "</catalgo>");
        let actions = fixture.actions(&source, 3..3, quick_fixes());
        assert_eq!(titles(&actions), vec!["Vous vouliez dire <catalog> ?"]);
        // Aucun nom proche : pas de suggestion.
        let source = source.replace("catalgo", "zzz");
        assert!(fixture.actions(&source, 2..2, quick_fixes()).is_empty());
    }

    #[test]
    fn binds_documents_to_sibling_or_placeholder_schemas() {
        let source_actions = json!({"only": ["source"]});
        let mut fixture = Fixture::new("bind", Some(SCHEMA));
        fs::write(fixture.directory.join("a other.xsd"), "<xs:schema/>").unwrap();
        let source = "<?xml version=\"1.0\"?>\n<catalog>\n  <book/>\n</catalog>";
        let actions = fixture.actions(source, 0..0, source_actions.clone());
        assert_eq!(
            titles(&actions),
            vec![
                "Lier le document au schéma XSD catalog.xsd",
                "Lier le document au schéma XSD a other.xsd",
            ]
        );
        assert_eq!(actions[0]["kind"], "source");
        assert_eq!(
            fixture.apply(
                source,
                &actions,
                "Lier le document au schéma XSD a other.xsd"
            ),
            "<?xml version=\"1.0\"?>\n<catalog xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"a%20other.xsd\">\n  <book/>\n</catalog>"
        );
        // Déjà lié : rien à proposer.
        assert!(
            fixture
                .actions(&format!("{BOUND}</catalog>"), 0..0, source_actions.clone())
                .is_empty()
        );

        let mut empty = Fixture::new("bind-placeholder", None);
        let source =
            "<t:root xmlns:t=\"urn:t\" xmlns:i=\"http://www.w3.org/2001/XMLSchema-instance\"/>";
        let actions = empty.actions(source, 0..0, source_actions);
        assert_eq!(
            titles(&actions),
            vec!["Lier le document à un schéma XSD (schema.xsd à compléter)"]
        );
        assert_eq!(
            empty.apply(
                source,
                &actions,
                "Lier le document à un schéma XSD (schema.xsd à compléter)"
            ),
            "<t:root xmlns:t=\"urn:t\" xmlns:i=\"http://www.w3.org/2001/XMLSchema-instance\" i:schemaLocation=\"urn:t schema.xsd\"/>"
        );
    }

    #[test]
    fn computes_edit_distances_with_transpositions() {
        assert_eq!(edit_distance("chidl", "child"), 1);
        assert_eq!(edit_distance("titel", "title"), 1);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("été", "ete"), 2);
        assert_eq!(
            closest("Titel", ["title", "format", "titles"].into_iter()),
            vec![("title", 1), ("titles", 2)]
        );
    }
}
