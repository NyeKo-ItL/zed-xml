//! Diagnostics of a document: `XmlLanguageServer::diagnostics` is the single
//! place computing them (called by the diagnostics worker), and the helpers
//! building the LSP `Diagnostic` values of the non-XML sources (schema and
//! catalog problems, DOCTYPE, large files, missing grammar).

use super::*;

impl XmlLanguageServer {
    /// `publishDiagnostics` parameters of `uri` according to `xml.validation.*`.
    pub(crate) fn diagnostics(&mut self, uri: &str, source: &str) -> Value {
        let validation = self.settings.validation.clone();
        if !validation.enabled {
            return json!({"uri": uri, "diagnostics": []});
        }
        if self.settings.is_large(source) {
            // Beyond `xml.maxFileSize`: well-formedness only (linear).
            let diagnostics = if dtd::is_dtd_uri(uri) {
                Vec::new()
            } else {
                parse_xml(source).diagnostics
            };
            let notice = large_file_diagnostic(source.len(), self.settings.max_file_size);
            return diagnostics_params(uri, source, &diagnostics, &[notice]);
        }
        if dtd::is_dtd_uri(uri) {
            // DTD file: neither XML well-formedness nor schema.
            let diagnostics = self.dtd_diagnostics(uri, source);
            return json!({"uri": uri, "diagnostics": diagnostics});
        }
        let diagnostics = parse_xml(source).diagnostics;
        let mut extra = Vec::new();
        if validation.disallow_doc_type_decl {
            extra.extend(doctype_diagnostics(source));
        }
        extra.extend(catalog_diagnostics(uri, source));
        extra.extend(xslt::diagnostics(uri, source));
        extra.extend(schema_document_diagnostics(source));
        extra.extend(self.dtd_diagnostics(uri, source));
        if validation.schema != settings::SchemaValidation::Never {
            extra.extend(self.schema_diagnostics(uri, source));
        }
        if let Some(severity) = validation.no_grammar.severity()
            && !self.has_grammar(uri, source)
        {
            extra.extend(no_grammar_diagnostic(source, severity));
        }
        diagnostics_params(uri, source, &diagnostics, &extra)
    }

    pub(crate) fn dtd_diagnostics(&mut self, uri: &str, source: &str) -> Vec<Value> {
        let validation = self.settings.validation.clone();
        dtd::diagnostics(&mut self.dtd_context(), uri, source, &validation)
    }

    pub(crate) fn schema_diagnostics(&mut self, uri: &str, source: &str) -> Vec<Value> {
        let references = match self.schema_references(uri, source) {
            Ok(references) => references,
            Err(error) => return vec![xsd_error_diagnostic(error)],
        };
        let schemas::LoadedSchemas { merged, errors } =
            self.schemas.load(references, &self.catalogs);
        let schema_errors = !errors.is_empty();
        let mut diagnostics = errors
            .into_iter()
            .map(xsd_schema_error_diagnostic)
            .collect::<Vec<_>>();
        if schema_errors
            && self.settings.validation.schema == settings::SchemaValidation::OnValidSchema
        {
            return diagnostics;
        }
        if let Some(schema) = merged {
            let lines = selection::LineIndex::new(source);
            // Values outside an enumeration are published by
            // `code_actions::enumeration_diagnostics`, whose ranges and
            // messages the enumeration quick fixes match.
            diagnostics.extend(
                validate_document_located(source, &schema)
                    .iter()
                    .filter(|diagnostic| diagnostic.kind != XsdDiagnosticKind::InvalidEnumeration)
                    .map(|diagnostic| xsd_error_diagnostic_at(diagnostic, source, &lines)),
            );
            let mut context = self.hover_context(uri);
            diagnostics.extend(code_actions::enumeration_diagnostics(
                &mut context,
                uri,
                source,
            ));
        }

        diagnostics
    }
}

pub(crate) fn xsd_error_diagnostic_at(
    diagnostic: &LocatedXsdDiagnostic,
    source: &str,
    lines: &selection::LineIndex,
) -> Value {
    json!({
        "range": {
            "start": lines.position(source, diagnostic.offset),
            "end": lines.position(source, diagnostic.end),
        },
        "severity": 1,
        "source": "xml-lsp",
        "code": "xsd-validation",
        "data": {"category": "xsd", "kind": "validation", "rule": diagnostic.kind.id()},
        "message": diagnostic.message,
    })
}

/// Problems of a schema document itself (the document is an `xs:schema`).
pub(crate) fn schema_document_diagnostics(source: &str) -> Vec<Value> {
    let problems = xsd_core::schema_check::check_schema_document(source);
    if problems.is_empty() {
        return Vec::new();
    }
    let lines = selection::LineIndex::new(source);
    problems
        .into_iter()
        .map(|problem| {
            json!({
                "range": {
                    "start": lines.position(source, problem.range.start),
                    "end": lines.position(source, problem.range.end),
                },
                "severity": 1,
                "source": "xml-lsp",
                "code": "xsd-schema",
                "data": {"category": "xsd", "kind": problem.rule},
                "message": problem.message,
            })
        })
        .collect()
}

pub(crate) fn xsd_schema_error_diagnostic(error: schemas::SchemaLoadError) -> Value {
    json!({
        "range": {
            "start": {"line": 0, "character": 0},
            "end": {"line": 0, "character": 0},
        },
        "severity": if error.remote { 2 } else { 1 },
        "source": "xml-lsp",
        "code": "xsd-validation",
        "data": {
            "category": "xsd",
            "kind": "loading",
            "schemaUri": path_to_uri(&error.path),
            "schemaOffset": error.offset,
        },
        "message": error.message,
    })
}

/// Warnings of an open XML catalog: local targets not found.
pub(crate) fn catalog_diagnostics(uri: &str, source: &str) -> Vec<Value> {
    catalog::catalog_problems(uri, source)
        .into_iter()
        .map(|problem| {
            json!({
                "range": {
                    "start": position_at(source, problem.range.start),
                    "end": position_at(source, problem.range.end),
                },
                "severity": 2,
                "source": "xml-lsp",
                "code": "catalog-target-missing",
                "data": {"category": "catalog", "kind": "missingTarget"},
                "message": problem.message,
            })
        })
        .collect()
}

/// `xml.validation.disallowDocTypeDecl` error on each `<!DOCTYPE>`.
pub(crate) fn doctype_diagnostics(source: &str) -> Vec<Value> {
    xml_core::tags::scan_markup(source)
        .into_iter()
        .filter(|markup| {
            markup.kind == xml_core::tags::XmlMarkupKind::Declaration
                && source[markup.content.clone()].starts_with("DOCTYPE")
        })
        .map(|markup| {
            json!({
                "range": {
                    "start": position_at(source, markup.range.start),
                    "end": position_at(source, markup.range.end),
                },
                "severity": 1,
                "source": "xml-lsp",
                "code": "doctype-disallowed",
                "data": {"category": "xml", "kind": "doctypeDisallowed"},
                "message": "DOCTYPE declarations are not allowed (xml.validation.disallowDocTypeDecl).",
            })
        })
        .collect()
}

/// Information on a document larger than `xml.maxFileSize`: what is
/// disabled and how to change the limit.
pub(crate) fn large_file_diagnostic(size: usize, limit: Option<usize>) -> Value {
    let megabytes = |bytes: usize| bytes as f64 / (1024.0 * 1024.0);
    json!({
        "range": {
            "start": {"line": 0, "character": 0},
            "end": {"line": 0, "character": 0},
        },
        "severity": 3,
        "source": "xml-lsp",
        "code": "large-file",
        "data": {"category": "xml", "kind": "largeFile", "size": size, "limit": limit},
        "message": format!(
            "Large document ({:.1} MiB, xml.maxFileSize is {:.1} MiB): only well-formedness is checked; schema and DTD validation, document symbols, folding, colors, links, selection ranges and code actions are disabled for this file.",
            megabytes(size),
            megabytes(limit.unwrap_or(0)),
        ),
    })
}

/// `xml.validation.noGrammar` diagnostic on the name of the root element.
pub(crate) fn no_grammar_diagnostic(source: &str, severity: u8) -> Option<Value> {
    let tree = xml_core::tags::XmlTagTree::parse(source);
    let root = tree.elements().first()?;
    let name = root.start_tag.name.clone();
    Some(json!({
        "range": {
            "start": position_at(source, name.start),
            "end": position_at(source, name.end),
        },
        "severity": severity,
        "source": "xml-lsp",
        "code": "no-grammar",
        "data": {"category": "xml", "kind": "noGrammar"},
        "message": "No grammar (XSD, DTD) is associated with this document.",
    }))
}

pub(crate) fn xsd_error_diagnostic(diagnostic: impl Into<String>) -> Value {
    json!({
        "range": {
            "start": {"line": 0, "character": 0},
            "end": {"line": 0, "character": 0},
        },
        "severity": 1,
        "source": "xml-lsp",
        "code": "xsd-validation",
        "data": {"category": "xsd", "kind": "loading"},
        "message": diagnostic.into(),
    })
}

pub(crate) fn diagnostics_params(
    uri: &str,
    source: &str,
    diagnostics: &[XmlDiagnostic],
    schema_diagnostics: &[Value],
) -> Value {
    let lines = selection::LineIndex::new(source);
    let diagnostics = diagnostics.iter().map(|diagnostic| {
        let mut value = json!({
            "range": {
                "start": lines.position(source, diagnostic.offset),
                "end": lines.position(source, diagnostic.end),
            },
            "severity": 1,
            "source": "xml-lsp",
            "code": diagnostic.code(),
            "message": diagnostic.message,
        });
        if let Some(rule) = diagnostic.rule {
            value["data"] = json!({"category": "xml", "kind": rule});
        }
        value
    });

    let mut diagnostics = diagnostics.collect::<Vec<_>>();
    diagnostics.extend(schema_diagnostics.iter().cloned());
    json!({
        "uri": uri,
        "diagnostics": diagnostics,
    })
}
