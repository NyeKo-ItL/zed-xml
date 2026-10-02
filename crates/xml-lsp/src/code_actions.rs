//! `textDocument/codeAction`: quick fixes and rewrites, like
//! LemMinX.
//!
//! Quick fixes (`quickfix`), computed from the source and offered when the
//! problem touches the requested range; the matching diagnostics of the
//! context (same `code`, same `data.kind`/`data.rule`, touching ranges) are
//! attached to the action:
//!
//! - well-formedness ([`xml_core::wellformed`]): end tag to rename (or
//!   start tag to rename), extra end tag to remove, element to close (or to
//!   make self-closing), missing `>`/`/>`, duplicate attribute to remove,
//!   value to quote, `&`/`<` to
//!   escape;
//! - XSD schema ([`xsd_core::model`]): missing required attributes added
//!   with a value (fixed, default, first enumerated value or empty), value
//!   outside the enumeration replaced by each of the allowed values, unknown
//!   element renamed to the closest expected name ("Did you mean...?", edit
//!   distance).
//!
//! Rewrites (`refactor.rewrite`): empty `<a></a>` to `<a/>` and back.
//!
//! Source actions (`source`): bind the document to an XSD schema
//! (`xmlns:xsi` + `xsi:noNamespaceSchemaLocation`, or `xsi:schemaLocation`
//! for a root in a namespace), to the `.xsd` files of the same directory or
//! a placeholder location.
//!
//! The `context.only` filter is honoured (kind prefixes). Each action
//! carries a `WorkspaceEdit` (`changes`): no deferred resolution.
//!
//! The module also publishes diagnostics for values outside an enumeration
//! ([`enumeration_diagnostics`]), which the simplified XSD validation does
//! not check.

use std::{fs, ops::Range, path::Path};

use serde_json::{Map, Value, json};
use xml_core::{
    tags::{XmlTag, XmlTagKind, qualified_name_parts, scan_tags},
    wellformed::{XmlProblemKind, check_well_formedness},
};
use xsd_core::model::{XsdModelSet, XsdTypeRef, XsdUse};

use crate::{
    encode_uri_path,
    hover::{self, Document, HoverContext},
    links::XSI_NAMESPACE,
    selection::LineIndex,
    uri_to_path,
};

/// Action kinds announced in `codeActionProvider.codeActionKinds`.
pub(crate) const CODE_ACTION_KINDS: [&str; 3] = ["quickfix", "refactor", "source"];
pub(crate) const QUICK_FIX: &str = "quickfix";
const REWRITE: &str = "refactor.rewrite";
const SOURCE: &str = "source";
/// Code of the XSD validation diagnostics.
const XSD_CODE: &str = "xsd-validation";
/// Maximum number of enumeration values offered as replacements.
const MAX_ENUMERATION_ACTIONS: usize = 20;
/// Maximum number of names offered for an unknown element.
const MAX_SUGGESTIONS: usize = 3;
/// Maximum number of neighbouring schemas offered for binding.
const MAX_BOUND_SCHEMAS: usize = 5;
/// Location offered when no neighbouring schema exists.
const PLACEHOLDER_SCHEMA: &str = "schema.xsd";

/// Answers `textDocument/codeAction` for the range `range` (UTF-8 offsets)
/// of the document `uri`; `request_context` is the `CodeActionContext`.
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

/// `xsd-validation` diagnostics (`data.rule` = `invalidEnumeration`) for
/// attribute values and text contents outside the enumeration of their type.
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
                    "value `{}` is not in the enumeration of {target} (expected: {listed})",
                    &source[problem.range.clone()]
                ),
            }))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Action construction
// ---------------------------------------------------------------------------

/// Actions being built for a request (shared with [`crate::dtd`]).
pub(crate) struct Actions<'a> {
    uri: &'a str,
    pub(crate) source: &'a str,
    range: Range<usize>,
    only: Option<Vec<String>>,
    diagnostics: Vec<(Range<usize>, &'a Value)>,
    /// Indexes of `diagnostics` sorted by start, and the length of the
    /// longest one: [`Actions::matching`] only visits the diagnostics that
    /// can touch a range (a document with thousands of problems stays
    /// linear).
    by_start: Vec<usize>,
    longest: usize,
    lines: LineIndex,
    pub(crate) actions: Vec<Value>,
}

impl<'a> Actions<'a> {
    pub(crate) fn new(
        uri: &'a str,
        source: &'a str,
        range: Range<usize>,
        context: &'a Value,
    ) -> Self {
        let only = context.get("only").and_then(Value::as_array).map(|kinds| {
            kinds
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        });
        let lines = LineIndex::new(source);
        let diagnostics = context
            .get("diagnostics")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|diagnostic| {
                Some((
                    lsp_range(&lines, source, diagnostic.get("range")?)?,
                    diagnostic,
                ))
            })
            .collect::<Vec<(Range<usize>, &Value)>>();
        let mut by_start = (0..diagnostics.len()).collect::<Vec<_>>();
        by_start.sort_by_key(|&index| diagnostics[index].0.start);
        let longest = diagnostics
            .iter()
            .map(|(range, _)| range.len())
            .max()
            .unwrap_or(0);
        Self {
            uri,
            source,
            range,
            only,
            diagnostics,
            by_start,
            longest,
            lines,
            actions: Vec::new(),
        }
    }

    /// Whether the kind `kind` passes the `context.only` filter.
    pub(crate) fn wants(&self, kind: &str) -> bool {
        self.only.as_ref().is_none_or(|only| {
            only.iter().any(|requested| {
                kind == requested
                    || kind
                        .strip_prefix(requested.as_str())
                        .is_some_and(|rest| rest.starts_with('.'))
            })
        })
    }

    /// Whether a problem located at `range` concerns the requested range.
    pub(crate) fn requested(&self, range: &Range<usize>) -> bool {
        touches(&self.range, range)
    }

    /// Context diagnostics matching the problem: same `code`, `data.<key>`
    /// equal to `id` (or missing) and touching ranges.
    pub(crate) fn matching(
        &self,
        code: &str,
        key: &str,
        id: &str,
        range: &Range<usize>,
    ) -> Vec<Value> {
        // Candidates start at most `longest` bytes before the range and not
        // after its end.
        let first = self.by_start.partition_point(|&index| {
            self.diagnostics[index].0.start.saturating_add(self.longest) < range.start
        });
        let last = self
            .by_start
            .partition_point(|&index| self.diagnostics[index].0.start <= range.end);
        let mut matching = self.by_start[first..last.max(first)]
            .iter()
            .copied()
            .filter(|&index| {
                let (diagnostic_range, diagnostic) = &self.diagnostics[index];
                diagnostic.get("code").and_then(Value::as_str) == Some(code)
                    && touches(diagnostic_range, range)
                    && diagnostic
                        .get("data")
                        .and_then(|data| data.get(key))
                        .and_then(Value::as_str)
                        .is_none_or(|value| value == id)
            })
            .collect::<Vec<_>>();
        // In the order of the request.
        matching.sort_unstable();
        matching
            .into_iter()
            .map(|index| self.diagnostics[index].1.clone())
            .collect()
    }

    pub(crate) fn push(
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

/// Closed ranges that overlap or touch.
fn touches(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start <= right.end && right.start <= left.end
}

fn lsp_range(lines: &LineIndex, source: &str, range: &Value) -> Option<Range<usize>> {
    let offset = |position: &Value| {
        let line = position.get("line")?.as_u64()? as usize;
        let character = position.get("character")?.as_u64()? as usize;
        Some(lines.offset(source, line, character))
    };
    let start = offset(range.get("start")?)?;
    let end = offset(range.get("end")?)?;
    Some(start.min(end)..start.max(end))
}

// ---------------------------------------------------------------------------
// Well-formedness
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
                    format!("Replace </{text}> with </{expected}>"),
                    QUICK_FIX,
                    vec![(range, expected.clone())],
                    diagnostics.clone(),
                    true,
                );
                actions.push(
                    format!("Rename <{expected}> to <{text}>"),
                    QUICK_FIX,
                    vec![(start_name.clone(), text.to_owned())],
                    diagnostics,
                    false,
                );
            }
            XmlProblemKind::UnmatchedEndTag { tag } => actions.push(
                format!("Remove end tag </{text}>"),
                QUICK_FIX,
                vec![(tag.clone(), String::new())],
                diagnostics,
                true,
            ),
            XmlProblemKind::UnclosedElement { insert_at, tag_end } => {
                actions.push(
                    format!("Close <{text}> with </{text}>"),
                    QUICK_FIX,
                    vec![(*insert_at..*insert_at, format!("</{text}>"))],
                    diagnostics.clone(),
                    true,
                );
                actions.push(
                    format!("Make <{text}> self-closing"),
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
                    "End the tag with `>`".to_owned(),
                    QUICK_FIX,
                    vec![(*insert_at..*insert_at, ">".to_owned())],
                    diagnostics.clone(),
                    true,
                );
                if *start_tag {
                    actions.push(
                        "End the tag with `/>`".to_owned(),
                        QUICK_FIX,
                        vec![(*insert_at..*insert_at, "/>".to_owned())],
                        diagnostics,
                        false,
                    );
                }
            }
            XmlProblemKind::DuplicateAttribute { removal } => actions.push(
                format!("Remove duplicate attribute {text}"),
                QUICK_FIX,
                vec![(removal.clone(), String::new())],
                diagnostics,
                true,
            ),
            XmlProblemKind::UnquotedAttributeValue => {
                let quote = if text.contains('"') { '\'' } else { '"' };
                actions.push(
                    "Quote the value".to_owned(),
                    QUICK_FIX,
                    vec![(range, format!("{quote}{text}{quote}"))],
                    diagnostics,
                    true,
                );
            }
            XmlProblemKind::UnescapedCharacter { character } => {
                let entity = if *character == '<' { "&lt;" } else { "&amp;" };
                actions.push(
                    format!("Replace `{character}` with `{entity}`"),
                    QUICK_FIX,
                    vec![(range, entity.to_owned())],
                    diagnostics,
                    true,
                );
            }
            XmlProblemKind::UndeclaredPrefix { prefix } => {
                let Some(namespace) = well_known_namespace(prefix) else {
                    continue;
                };
                let Some(root) = scan_tags(source)
                    .into_iter()
                    .find(|tag| tag.kind != XmlTagKind::End)
                else {
                    continue;
                };
                actions.push(
                    format!("Declare the prefix {prefix} on the root element"),
                    QUICK_FIX,
                    vec![(
                        root.name.end..root.name.end,
                        format!(" xmlns:{prefix}=\"{namespace}\""),
                    )],
                    diagnostics,
                    true,
                );
            }
            XmlProblemKind::InvalidQualifiedName | XmlProblemKind::InvalidNamespaceDeclaration => {}
        }
    }
}

/// Namespace conventionally bound to `prefix`.
fn well_known_namespace(prefix: &str) -> Option<&'static str> {
    Some(match prefix {
        "xsi" => "http://www.w3.org/2001/XMLSchema-instance",
        "xs" | "xsd" => "http://www.w3.org/2001/XMLSchema",
        "xsl" => "http://www.w3.org/1999/XSL/Transform",
        "xlink" => "http://www.w3.org/1999/xlink",
        "xi" => "http://www.w3.org/2001/XInclude",
        "xhtml" => "http://www.w3.org/1999/xhtml",
        "svg" => "http://www.w3.org/2000/svg",
        "rdf" => "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
        "rdfs" => "http://www.w3.org/2000/01/rdf-schema#",
        "dc" => "http://purl.org/dc/elements/1.1/",
        "dcterms" => "http://purl.org/dc/terms/",
        "soap" | "soapenv" => "http://schemas.xmlsoap.org/soap/envelope/",
        "wsdl" => "http://schemas.xmlsoap.org/wsdl/",
        "android" => "http://schemas.android.com/apk/res/android",
        "tools" => "http://schemas.android.com/tools",
        "app" => "http://schemas.android.com/apk/res-auto",
        "mc" => "http://schemas.openxmlformats.org/markup-compatibility/2006",
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// XSD schema
// ---------------------------------------------------------------------------

/// Located XSD validation problem, with the data needed to fix it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SchemaProblem {
    kind: SchemaProblemKind,
    /// Relevant range: start tag (missing attributes), value (enumeration)
    /// or start tag name (unknown element).
    range: Range<usize>,
    /// Rule (`data.rule`) of the matching diagnostics.
    rule: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SchemaProblemKind {
    /// Missing required attributes: `(qualified name, proposed value)`, to
    /// insert at `insert_at` (after the last attribute).
    MissingAttributes {
        insert_at: usize,
        attributes: Vec<(String, String)>,
    },
    /// Value outside the enumeration `values`; `target` describes the
    /// attribute or element for the message.
    InvalidEnumeration { values: Vec<String>, target: String },
    /// Element missing from the content model: local names to replace
    /// (start and end tags) and proposed close names.
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
                    format!("Add required attribute {names}")
                } else {
                    format!("Add required attributes {names}")
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
                        format!("Replace with `{value}`"),
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
                        format!("Did you mean <{suggestion}>?"),
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

/// Collects, for each element of the document (or only those touching
/// `within`, since code action requests are sent on every cursor
/// move), the missing required attributes, the values outside an
/// enumeration and the elements unknown to the model.
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
            let Some(value_type) = set.attribute_type(declaration) else {
                continue;
            };
            let Some(values) = enumeration_values(set, value_type) else {
                continue;
            };
            if !is_enumerated(set, value_type, &source[value.clone()], &values) {
                problems.push(SchemaProblem {
                    kind: SchemaProblemKind::InvalidEnumeration {
                        values,
                        target: format!("@{attribute_name} on <{name}>"),
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
                // No prefix declared for the namespace: insertion impossible.
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
    // Mixed content, CDATA or comments: not checked.
    if text.contains('<') || is_nil(document, index) {
        return None;
    }
    let values = enumeration_values(set, element_type)?;
    let trimmed = text.trim();
    if is_enumerated(set, element_type, trimmed, &values) {
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

/// Enumerated values of a simple type (facets accumulated along the
/// restrictions); `None` without an enumeration or for a list.
pub(crate) fn enumeration_values(
    set: &XsdModelSet,
    value_type: XsdTypeRef<'_>,
) -> Option<Vec<String>> {
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

/// Compares a raw value (predefined entities decoded) with the enumerated
/// values: as written, with whitespace collapsed, or in the value space of
/// its type (`1.0` is `1` for an `xs:decimal`).
fn is_enumerated(
    set: &XsdModelSet,
    value_type: XsdTypeRef<'_>,
    raw: &str,
    values: &[String],
) -> bool {
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
        || set
            .simple_type(value_type)
            .is_some_and(|simple_type| simple_type.validate(&value, None).is_ok())
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

/// Prefix declared (`xmlns:prefix`) for `namespace` in the scope of the
/// element.
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

/// Candidates close to `name` (edit distance with transpositions, case
/// insensitive), sorted from closest to farthest.
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

/// "Optimal string alignment" edit distance (insertion, deletion,
/// substitution, transposition of adjacent characters).
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
// Rewrites
// ---------------------------------------------------------------------------

/// `<a ...></a>` (empty or blank content) <-> `<a .../>` for the element one
/// of whose tags touches the start of the requested range.
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
                format!("Expand <{name}/> to <{name}></{name}>"),
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
                format!("Convert <{name}></{name}> to self-closing <{name}/>"),
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
// Schema binding
// ---------------------------------------------------------------------------

/// Offers to bind an unbound document to a neighbouring XSD schema (or to a
/// placeholder location).
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
            format!("Bind the document to an XSD schema (placeholder {file})")
        } else {
            format!("Bind the document to the XSD schema {file}")
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

/// `.xsd` files of the document's directory: the one with the same name
/// first, then in alphabetical order.
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
mod tests;
