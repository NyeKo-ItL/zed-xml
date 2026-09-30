//! XSLT awareness for documents in the XSLT namespace (IntelliJ/Eclipse
//! XSL tools style, which LemMinX lacks beyond the bundled XSD):
//!
//! - `xpath-syntax` diagnostics for the XPath expressions (`select`,
//!   `test`, `use`, `group-by`, ...), patterns (`match`, `count`, ...),
//!   `use-when`, attribute value templates and XSLT 3.0 text value
//!   templates, located on the offending token;
//! - completion of `xsl:` elements allowed in context, their attributes
//!   and enumerated values, component names (templates, modes, attribute
//!   sets, ...), variables after `$` and key names in `key('...')`;
//! - go to definition, references and rename of named templates,
//!   variables and parameters (with XSLT scoping and XPath range
//!   variables), stylesheet functions, modes, keys, attribute sets,
//!   decimal formats, accumulators, character maps and output
//!   definitions, across `xsl:include`/`xsl:import` (and open stylesheets
//!   including the current one);
//! - hover on XSLT elements, attributes and references.

mod completion;
mod navigation;
pub(crate) mod stylesheet;
pub(crate) mod vocabulary;

use std::{collections::HashMap, path::PathBuf};

use serde_json::{Value, json};

pub(crate) use completion::completions;
pub(crate) use navigation::{definition, hover, prepare_rename, references, rename};
use stylesheet::{Module, SiteKind, may_be_stylesheet};

use crate::{
    catalog::Catalogs,
    links::{self, LinkKind, LinkTarget},
    path_to_uri,
    selection::LineIndex,
    uri_to_path,
};

/// Stable diagnostic code of XPath syntax errors.
pub(crate) const XPATH_SYNTAX_CODE: &str = "xpath-syntax";

/// Maximum number of stylesheet modules loaded for one request.
const MAX_MODULES: usize = 64;
/// Maximum size of a stylesheet module read from disk.
const MAX_MODULE_SIZE: u64 = 8 * 1024 * 1024;

/// What XSLT features need from the server.
pub(crate) struct XsltContext<'a> {
    /// Open documents (URI → text), preferred over the disk.
    pub(crate) documents: &'a HashMap<String, String>,
    pub(crate) catalogs: &'a Catalogs,
}

/// `xpath-syntax` diagnostics of the stylesheet `source` (empty for other
/// documents).
pub(crate) fn diagnostics(uri: &str, source: &str) -> Vec<Value> {
    let Some(module) = Module::parse(uri, source) else {
        return Vec::new();
    };
    let lines = LineIndex::new(source);
    let mut diagnostics = Vec::new();
    for site in &module.sites {
        for error in &site.analysis.errors {
            let mut range = error.range.clone();
            if range.is_empty() && site.value.is_empty() && site.attribute.is_some() {
                // Empty attribute value: underline the quotes.
                range = site.value.start.saturating_sub(1)..(site.value.end + 1).min(source.len());
            }
            let (what, kind) = match site.kind {
                SiteKind::Expression => ("XPath expression", "expression"),
                SiteKind::Pattern => ("pattern", "pattern"),
                SiteKind::ValueTemplate => ("value template", "value-template"),
            };
            let location = site
                .attribute
                .as_deref()
                .map(|name| format!(" in '{name}'"))
                .unwrap_or_default();
            diagnostics.push(json!({
                "range": {
                    "start": lines.position(source, range.start),
                    "end": lines.position(source, range.end),
                },
                "severity": 1,
                "source": "xml-lsp",
                "code": XPATH_SYNTAX_CODE,
                "message": format!("Invalid {what}{location}: {}", error.message),
                "data": {"category": "xpath", "kind": kind},
            }));
        }
    }
    diagnostics
}

/// Texts of the stylesheet modules of a request: the document itself
/// first, then the modules it includes or imports (transitively), then
/// the open stylesheets including it and their modules.
pub(crate) struct Texts(Vec<(String, String)>);

impl Texts {
    /// The document alone, when it may be a stylesheet.
    pub(crate) fn single(uri: &str, source: &str) -> Option<Self> {
        may_be_stylesheet(source).then(|| Self(vec![(uri.to_owned(), source.to_owned())]))
    }

    pub(crate) fn load(context: &XsltContext<'_>, uri: &str, source: &str) -> Option<Self> {
        if !may_be_stylesheet(source) {
            return None;
        }
        let mut texts = Vec::new();
        closure(context, uri, source, &mut texts);
        let current = uri_to_path(uri);
        let mut others = context
            .documents
            .iter()
            .filter(|(other, text)| {
                other.as_str() != uri
                    && may_be_stylesheet(text)
                    && (text.contains("include") || text.contains("import"))
            })
            .collect::<Vec<_>>();
        others.sort_by(|left, right| left.0.cmp(right.0));
        for (other, text) in others {
            if texts.len() >= MAX_MODULES {
                break;
            }
            let mut including = Vec::new();
            closure(context, other, text, &mut including);
            if including
                .iter()
                .skip(1)
                .any(|(module, _)| uri_to_path(module) == current)
            {
                for (module, text) in including {
                    if texts.len() < MAX_MODULES
                        && !texts
                            .iter()
                            .any(|(known, _)| uri_to_path(known) == uri_to_path(&module))
                    {
                        texts.push((module, text));
                    }
                }
            }
        }
        Some(Self(texts))
    }

    /// Analyzed modules; `None` if the document is not a stylesheet.
    pub(crate) fn modules(&self) -> Option<Vec<Module<'_>>> {
        let mut modules = Vec::with_capacity(self.0.len());
        for (index, (uri, text)) in self.0.iter().enumerate() {
            match Module::parse(uri, text) {
                Some(module) => modules.push(module),
                None if index == 0 => return None,
                None => {}
            }
        }
        Some(modules)
    }
}

/// Appends `uri` and the modules it includes or imports (breadth first,
/// bounded).
fn closure(context: &XsltContext<'_>, uri: &str, source: &str, texts: &mut Vec<(String, String)>) {
    let mut seen: Vec<PathBuf> = texts.iter().map(|(known, _)| uri_to_path(known)).collect();
    seen.push(uri_to_path(uri));
    let start = texts.len();
    texts.push((uri.to_owned(), source.to_owned()));
    let mut next = start;
    while next < texts.len() && texts.len() < MAX_MODULES {
        let (module_uri, module_source) = texts[next].clone();
        next += 1;
        for reference in links::link_references(&module_source) {
            if !matches!(reference.kind, LinkKind::XslInclude | LinkKind::XslImport) {
                continue;
            }
            let Some(LinkTarget::File(path)) =
                links::resolve_reference(&module_uri, &reference, context.catalogs)
            else {
                continue;
            };
            if seen.contains(&path) || texts.len() >= MAX_MODULES {
                continue;
            }
            seen.push(path.clone());
            if let Some((uri, text)) = read_module(context, &path) {
                texts.push((uri, text));
            }
        }
    }
}

/// Open buffer of `path`, otherwise the file on disk (bounded size).
fn read_module(context: &XsltContext<'_>, path: &PathBuf) -> Option<(String, String)> {
    if let Some((uri, text)) = context
        .documents
        .iter()
        .find(|(uri, _)| uri.starts_with("file:") && &uri_to_path(uri) == path)
    {
        return Some((uri.clone(), text.clone()));
    }
    let text = xml_core::resource::read_text_file(path, MAX_MODULE_SIZE).ok()?;
    Some((path_to_uri(path), text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_xpath_syntax_errors_with_precise_ranges() {
        let source = "<xsl:stylesheet version=\"2.0\" xmlns:xsl=\"http://www.w3.org/1999/XSL/Transform\">\r\n  <xsl:template match=\"a[\">\r\n    <é title=\"{@x +}\"><xsl:value-of select=\"\"/></é>\r\n    <xsl:if test=\"count(//a) &gt; 1 and\"/>\r\n  </xsl:template>\r\n</xsl:stylesheet>";
        let diagnostics = diagnostics("file:///s.xsl", source);
        let summary = diagnostics
            .iter()
            .map(|diagnostic| {
                (
                    diagnostic["code"].as_str().unwrap_or_default().to_owned(),
                    diagnostic["range"]["start"]["line"].as_u64().unwrap_or(99),
                    diagnostic["range"]["start"]["character"]
                        .as_u64()
                        .unwrap_or(99),
                    diagnostic["range"]["end"]["character"]
                        .as_u64()
                        .unwrap_or(99),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            summary,
            vec![
                ("xpath-syntax".to_owned(), 1, 24, 25),
                ("xpath-syntax".to_owned(), 2, 18, 19),
                ("xpath-syntax".to_owned(), 2, 43, 45),
                ("xpath-syntax".to_owned(), 3, 36, 39),
            ],
            "{diagnostics:#?}"
        );
        assert_eq!(
            diagnostics[0]["message"],
            "Invalid pattern in 'match': Expected an expression but reached the end of the expression."
        );
        assert!(super::diagnostics("file:///plain.xml", "<a b=\"{\"/>").is_empty());
    }

    #[test]
    fn real_world_stylesheets_have_no_false_positives() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/real-world");
        for name in ["xslt/docbook-admon.xsl", "specs/xslt3-identity.xsl"] {
            let source =
                std::fs::read_to_string(root.join(name)).expect("fixture should be readable");
            let module = Module::parse("file:///fixture.xsl", &source).expect("stylesheet");
            assert!(!module.sites.is_empty(), "{name}");
            assert_eq!(
                super::diagnostics("file:///fixture.xsl", &source),
                Vec::<Value>::new(),
                "{name}"
            );
        }
    }
}
