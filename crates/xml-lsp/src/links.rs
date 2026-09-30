//! `textDocument/documentLink` and navigation to referenced files, like
//! LemMinX.
//!
//! Recognized, by namespace (the prefix is resolved through `xmlns`
//! declarations; the conventional `xsi`, `xs`/`xsd`, `xi` and `xsl` prefixes
//! are accepted when undeclared):
//!
//! - each location (every other token) of `xsi:schemaLocation` and the
//!   value of `xsi:noNamespaceSchemaLocation`;
//! - `schemaLocation` of `xs:include`, `xs:import`, `xs:redefine` and
//!   `xs:override`;
//! - `href` of `xi:include` (XInclude);
//! - `href` of `xsl:import` and `xsl:include`;
//! - the `href` pseudo-attribute of the `<?xml-stylesheet ...?>` and
//!   `<?xml-model ...?>` instructions;
//! - the system identifier of `<!DOCTYPE ... SYSTEM|PUBLIC ...>`.
//!
//! Relative paths are resolved against the document (including in
//! percent-encoded form), `http(s)` URLs are kept as
//! is. Only existing local files and `http(s)` URLs
//! produce a link. `textDocument/definition` on the same values leads to
//! the start of the target file (0:0), which makes the links usable with
//! cmd-click even without `documentLink`.

use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use serde_json::{Value, json};
use xml_core::resource::is_local_file;
use xml_core::tags::{
    XmlAttribute, XmlMarkupKind, XmlTagTree, qualified_name_parts, resolve_namespace,
    scan_attributes, scan_markup,
};
use xsd_core::{model::XSD_NAMESPACE, resolve_path};

use crate::{
    catalog::{Catalogs, target_path},
    path_to_uri, percent_decode,
    selection::LineIndex,
    uri_to_path,
};

/// `xsi` namespace.
pub const XSI_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema-instance";
/// XInclude 1.0 namespace.
pub const XINCLUDE_NAMESPACE: &str = "http://www.w3.org/2001/XInclude";
/// Former XInclude namespace (2003 draft), still found in the wild.
const XINCLUDE_2003_NAMESPACE: &str = "http://www.w3.org/2003/XInclude";
/// XSLT namespace.
pub const XSLT_NAMESPACE: &str = "http://www.w3.org/1999/XSL/Transform";

/// Kind of a reference to another file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// Location of an `xsi:schemaLocation` pair.
    SchemaLocation,
    /// `xsi:noNamespaceSchemaLocation`.
    NoNamespaceSchemaLocation,
    /// `xs:include/@schemaLocation`.
    XsdInclude,
    /// `xs:import/@schemaLocation`.
    XsdImport,
    /// `xs:redefine/@schemaLocation`.
    XsdRedefine,
    /// `xs:override/@schemaLocation`.
    XsdOverride,
    /// `xi:include/@href`.
    XInclude,
    /// `<?xml-stylesheet href="..."?>`.
    Stylesheet,
    /// `<?xml-model href="..."?>`.
    XmlModel,
    /// System identifier of `<!DOCTYPE>`.
    Doctype,
    /// `xsl:import/@href`.
    XslImport,
    /// `xsl:include/@href`.
    XslInclude,
}

impl LinkKind {
    fn label(self) -> &'static str {
        match self {
            Self::SchemaLocation | Self::NoNamespaceSchemaLocation => "Open XSD schema",
            Self::XsdInclude => "Open included schema",
            Self::XsdImport => "Open imported schema",
            Self::XsdRedefine => "Open redefined schema",
            Self::XsdOverride => "Open overridden schema",
            Self::XInclude => "Open included document (XInclude)",
            Self::Stylesheet => "Open stylesheet",
            Self::XmlModel => "Open model (xml-model)",
            Self::Doctype => "Open DTD",
            Self::XslImport => "Open imported XSLT stylesheet",
            Self::XslInclude => "Open included XSLT stylesheet",
        }
    }
}

/// Lexical reference to another file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkReference {
    pub kind: LinkKind,
    /// Range of the value in the source (quotes and surrounding whitespace
    /// excluded).
    pub range: Range<usize>,
    /// Value, XML entities resolved.
    pub value: String,
    /// Associated identifier, used by XML catalogs: namespace
    /// (`xsi:schemaLocation`, `xs:import`) or public identifier
    /// (`<!DOCTYPE … PUBLIC>`).
    pub key: Option<String>,
}

/// Resolved target of a reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// Existing local file.
    File(PathBuf),
    /// `http` or `https` URL, kept as is.
    Url(String),
}

impl LinkTarget {
    /// URI of the target (`file://...` or the URL).
    pub fn uri(&self) -> String {
        match self {
            Self::File(path) => path_to_uri(path),
            Self::Url(url) => url.clone(),
        }
    }

    fn display(&self) -> String {
        match self {
            Self::File(path) => path.display().to_string(),
            Self::Url(url) => url.clone(),
        }
    }
}

/// Reference whose target has been resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentLink {
    pub reference: LinkReference,
    pub target: LinkTarget,
}

impl DocumentLink {
    /// Tooltip shown by the client.
    pub fn tooltip(&self) -> String {
        format!("{}: {}", self.reference.kind.label(), self.target.display())
    }
}

/// Lists the references to other files, in document order, without
/// resolution.
pub fn link_references(source: &str) -> Vec<LinkReference> {
    let tree = XmlTagTree::parse(source);
    let attributes: Vec<Vec<XmlAttribute>> = tree
        .elements()
        .iter()
        .map(|element| scan_attributes(source, &element.start_tag))
        .collect();
    let namespace = |element: usize, prefix: Option<&str>| -> Option<&str> {
        match resolve_namespace(source, &tree, &attributes, element, prefix) {
            Some(namespace) => namespace,
            None => conventional_namespace(prefix?),
        }
    };

    let mut references = Vec::new();
    for (index, element) in tree.elements().iter().enumerate() {
        let (prefix, local) = qualified_name_parts(source, element.start_tag.name.clone());
        let element_namespace = namespace(index, prefix.map(|range| &source[range]));
        let element_local = &source[local];
        let element_kind = match (element_namespace, element_local) {
            (Some(XSD_NAMESPACE), "include") => Some((LinkKind::XsdInclude, "schemaLocation")),
            (Some(XSD_NAMESPACE), "import") => Some((LinkKind::XsdImport, "schemaLocation")),
            (Some(XSD_NAMESPACE), "redefine") => Some((LinkKind::XsdRedefine, "schemaLocation")),
            (Some(XSD_NAMESPACE), "override") => Some((LinkKind::XsdOverride, "schemaLocation")),
            (Some(XINCLUDE_NAMESPACE | XINCLUDE_2003_NAMESPACE), "include") => {
                Some((LinkKind::XInclude, "href"))
            }
            (Some(XSLT_NAMESPACE), "import") => Some((LinkKind::XslImport, "href")),
            (Some(XSLT_NAMESPACE), "include") => Some((LinkKind::XslInclude, "href")),
            _ => None,
        };

        for attribute in &attributes[index] {
            let Some(value) = attribute.value.clone() else {
                continue;
            };
            let name = attribute.name(source);
            if let Some((kind, attribute_name)) = element_kind
                && name == attribute_name
            {
                let key = (kind == LinkKind::XsdImport)
                    .then(|| {
                        attributes[index]
                            .iter()
                            .find(|attribute| attribute.name(source) == "namespace")
                            .and_then(|attribute| attribute.value(source))
                            .map(|namespace| unescape(namespace.trim()))
                    })
                    .flatten();
                push_value(source, &mut references, kind, value.clone(), key);
                continue;
            }
            let Some((prefix, local)) = name.split_once(':') else {
                continue;
            };
            if prefix == "xmlns" || namespace(index, Some(prefix)) != Some(XSI_NAMESPACE) {
                continue;
            }
            match local {
                "schemaLocation" => {
                    let mut tokens = tokens(source, value);
                    while let (Some(namespace), Some(location)) = (tokens.next(), tokens.next()) {
                        let namespace = unescape(&source[namespace]);
                        push_value(
                            source,
                            &mut references,
                            LinkKind::SchemaLocation,
                            location,
                            Some(namespace),
                        );
                    }
                }
                "noNamespaceSchemaLocation" => push_value(
                    source,
                    &mut references,
                    LinkKind::NoNamespaceSchemaLocation,
                    value,
                    None,
                ),
                _ => {}
            }
        }
    }

    for markup in scan_markup(source) {
        match markup.kind {
            XmlMarkupKind::ProcessingInstruction => {
                let content = markup.content.clone();
                let target = scan_token(source, content.start, content.end);
                let kind = match &source[target.clone()] {
                    "xml-stylesheet" => LinkKind::Stylesheet,
                    "xml-model" => LinkKind::XmlModel,
                    _ => continue,
                };
                if let Some(value) = pseudo_attribute(source, target.end..content.end, "href") {
                    push_value(source, &mut references, kind, value, None);
                }
            }
            XmlMarkupKind::Declaration => {
                if let Some((public, system)) = doctype_external_id(source, markup.content.clone())
                {
                    let public = public.map(|range| source[range].to_owned());
                    push_value(source, &mut references, LinkKind::Doctype, system, public);
                }
            }
            XmlMarkupKind::Comment | XmlMarkupKind::CData => {}
        }
    }

    references.sort_by_key(|reference| reference.range.start);
    references
}

/// Resolves the references of the document `document_uri` (XML catalogs
/// first); keeps only existing local files and `http(s)`
/// URLs.
pub fn document_links(document_uri: &str, source: &str, catalogs: &Catalogs) -> Vec<DocumentLink> {
    link_references(source)
        .into_iter()
        .filter_map(|reference| {
            let target = resolve_reference(document_uri, &reference, catalogs)?;
            Some(DocumentLink { reference, target })
        })
        .collect()
}

/// JSON response of `textDocument/documentLink`.
pub fn document_links_json(document_uri: &str, source: &str, catalogs: &Catalogs) -> Value {
    let lines = LineIndex::new(source);
    Value::Array(
        document_links(document_uri, source, catalogs)
            .into_iter()
            .map(|link| {
                json!({
                    "range": lsp_range(&lines, source, &link.reference.range),
                    "target": link.target.uri(),
                    "tooltip": link.tooltip(),
                })
            })
            .collect(),
    )
}

/// Response of `textDocument/definition` if `offset` is on a reference: the
/// start of the local target file, or an empty list if the target is not an
/// existing local file (a URL stays opened by `documentLink`).
/// `None` if `offset` is on no reference. With `link_support`, the
/// response is a `LocationLink[]` whose origin is the whole value.
pub fn definition(
    document_uri: &str,
    source: &str,
    offset: usize,
    link_support: bool,
    catalogs: &Catalogs,
) -> Option<Value> {
    let reference = link_references(source)
        .into_iter()
        .find(|reference| reference.range.start <= offset && offset <= reference.range.end)?;
    let Some(LinkTarget::File(path)) = resolve_reference(document_uri, &reference, catalogs) else {
        return Some(json!([]));
    };
    let start = json!({"line": 0, "character": 0});
    let target_range = json!({"start": start, "end": start});
    let uri = path_to_uri(&path);
    Some(if link_support {
        let lines = LineIndex::new(source);
        json!([{
            "originSelectionRange": lsp_range(&lines, source, &reference.range),
            "targetUri": uri,
            "targetRange": target_range,
            "targetSelectionRange": target_range,
        }])
    } else {
        json!([{"uri": uri, "range": target_range}])
    })
}

/// Resolves `reference`: first through the XML catalogs (namespace of
/// `xsi:schemaLocation`/`xs:import` through `uri` entries, public or
/// system identifier of the DOCTYPE, then the value as a system identifier
/// or URI) to an existing local file, otherwise like [`resolve_target`].
pub fn resolve_reference(
    document_uri: &str,
    reference: &LinkReference,
    catalogs: &Catalogs,
) -> Option<LinkTarget> {
    if !catalogs.is_empty() {
        let value = reference.value.trim();
        let cataloged = match reference.kind {
            LinkKind::SchemaLocation | LinkKind::XsdImport => reference
                .key
                .as_deref()
                .and_then(|namespace| catalogs.resolve_uri(namespace))
                .and_then(|target| target_path(&target))
                .filter(|path| is_local_file(path)),
            LinkKind::Doctype => catalogs
                .resolve_external(reference.key.as_deref(), Some(value))
                .and_then(|target| target_path(&target))
                .filter(|path| is_local_file(path)),
            _ => None,
        }
        .or_else(|| {
            catalogs
                .resolve_location(value)
                .filter(|path| is_local_file(path))
        });
        if let Some(path) = cataloged {
            return Some(LinkTarget::File(path));
        }
    }
    resolve_target(document_uri, &reference.value)
}

/// Resolves `value` against the document `document_uri`. Returns `None`
/// for a URI scheme other than `http(s)`/`file`, a missing file or a
/// relative path in a document that is not a local file.
pub fn resolve_target(document_uri: &str, value: &str) -> Option<LinkTarget> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(scheme) = uri_scheme(value) {
        return match scheme.to_ascii_lowercase().as_str() {
            "http" | "https" => Some(LinkTarget::Url(value.to_owned())),
            "file" => {
                let path = uri_to_path(strip_query_and_fragment(value));
                is_local_file(&path).then_some(LinkTarget::File(path))
            }
            _ => None,
        };
    }
    let base = document_uri
        .get(..5)
        .filter(|scheme| scheme.eq_ignore_ascii_case("file:"))
        .and_then(|_| uri_to_path(document_uri).parent().map(Path::to_path_buf));
    let stripped = strip_query_and_fragment(value);
    let candidates = [
        value.to_owned(),
        percent_decode(value),
        percent_decode(stripped),
    ];
    candidates.iter().find_map(|candidate| {
        let path = Path::new(candidate);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            resolve_path(base.as_deref()?, candidate)
        };
        is_local_file(&path).then_some(LinkTarget::File(path))
    })
}

fn lsp_range(lines: &LineIndex, source: &str, range: &Range<usize>) -> Value {
    json!({
        "start": lines.position(source, range.start),
        "end": lines.position(source, range.end),
    })
}

/// Usual namespace of an undeclared prefix.
fn conventional_namespace(prefix: &str) -> Option<&'static str> {
    match prefix {
        "xsi" => Some(XSI_NAMESPACE),
        "xs" | "xsd" => Some(XSD_NAMESPACE),
        "xi" => Some(XINCLUDE_NAMESPACE),
        "xsl" => Some(XSLT_NAMESPACE),
        _ => None,
    }
}

/// Adds the reference with value `range` (surrounding whitespace removed),
/// if it is not empty.
fn push_value(
    source: &str,
    references: &mut Vec<LinkReference>,
    kind: LinkKind,
    range: Range<usize>,
    key: Option<String>,
) {
    let raw = &source[range.clone()];
    let start = range.start + (raw.len() - raw.trim_start().len());
    let end = range.end - (raw.len() - raw.trim_end().len());
    if start >= end {
        return;
    }
    references.push(LinkReference {
        kind,
        range: start..end,
        value: unescape(&source[start..end]),
        key,
    });
}

/// Space-separated tokens in `range`.
fn tokens(source: &str, range: Range<usize>) -> impl Iterator<Item = Range<usize>> + '_ {
    let bytes = source.as_bytes();
    let mut index = range.start;
    std::iter::from_fn(move || {
        while index < range.end && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index >= range.end {
            return None;
        }
        let start = index;
        while index < range.end && !bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        Some(start..index)
    })
}

/// First token (without space, `=` or quote) from `start`, leading
/// whitespace ignored.
fn scan_token(source: &str, start: usize, end: usize) -> Range<usize> {
    let bytes = source.as_bytes();
    let mut index = start;
    while index < end && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    let token_start = index;
    while index < end
        && !bytes[index].is_ascii_whitespace()
        && !matches!(bytes[index], b'=' | b'"' | b'\'')
    {
        index += 1;
    }
    token_start..index
}

/// Value (quotes excluded) of the `name` pseudo-attribute of a processing
/// instruction, searched in `range`.
fn pseudo_attribute(source: &str, range: Range<usize>, name: &str) -> Option<Range<usize>> {
    let bytes = source.as_bytes();
    let mut index = range.start;
    while index < range.end {
        let key = scan_token(source, index, range.end);
        index = key.end;
        while index < range.end && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if bytes.get(index) != Some(&b'=') || index >= range.end {
            // Isolated token (or unexpected character): skip to the next one.
            index = if key.is_empty() { index + 1 } else { index };
            continue;
        }
        index += 1;
        while index < range.end && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let quote = *bytes.get(index).filter(|_| index < range.end)?;
        if !matches!(quote, b'"' | b'\'') {
            continue;
        }
        let value_start = index + 1;
        let value_end = source[value_start..range.end]
            .find(quote as char)
            .map_or(range.end, |offset| value_start + offset);
        if &source[key] == name {
            return Some(value_start..value_end);
        }
        index = value_end + 1;
    }
    None
}

/// Public (optional) and system literals (quotes excluded) of a
/// `DOCTYPE name SYSTEM "..."` or `DOCTYPE name PUBLIC "..."
/// "..."` declaration; `content` is the content of the declaration (without `<!` or `>`).
pub(crate) fn doctype_external_id(
    source: &str,
    content: Range<usize>,
) -> Option<(Option<Range<usize>>, Range<usize>)> {
    let bytes = source.as_bytes();
    let keyword = scan_token(source, content.start, content.end);
    if &source[keyword.clone()] != "DOCTYPE" {
        return None;
    }
    let name = scan_token(source, keyword.end, content.end);
    let external = scan_token(source, name.end, content.end);
    let literals = match &source[external.clone()] {
        "SYSTEM" => 1,
        "PUBLIC" => 2,
        _ => return None,
    };
    let mut index = external.end;
    let mut public = None;
    let mut literal = None;
    for _ in 0..literals {
        public = literal.take();
        while index < content.end && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let quote = *bytes.get(index).filter(|_| index < content.end)?;
        if !matches!(quote, b'"' | b'\'') {
            return None;
        }
        let start = index + 1;
        let end = source[start..content.end]
            .find(quote as char)
            .map(|offset| start + offset)?;
        literal = Some(start..end);
        index = end + 1;
    }
    Some((public, literal?))
}

/// URI scheme of `value` (at least two characters, so that a Windows drive
/// letter `C:` is not mistaken for a scheme).
pub(crate) fn uri_scheme(value: &str) -> Option<&str> {
    let (scheme, _) = value.split_once(':')?;
    let mut chars = scheme.chars();
    (scheme.len() >= 2
        && chars.next()?.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
    .then_some(scheme)
}

fn strip_query_and_fragment(value: &str) -> &str {
    value.split(['?', '#']).next().unwrap_or(value)
}

/// Resolves predefined entities and character references.
pub(crate) fn unescape(value: &str) -> String {
    if !value.contains('&') {
        return value.to_owned();
    }
    let mut result = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(ampersand) = rest.find('&') {
        result.push_str(&rest[..ampersand]);
        rest = &rest[ampersand..];
        let replacement = rest.find(';').and_then(|semicolon| {
            let entity = &rest[1..semicolon];
            let character = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                    .map(|hex| u32::from_str_radix(hex, 16))
                    .or_else(|| entity.strip_prefix('#').map(str::parse::<u32>))
                    .and_then(Result::ok)
                    .and_then(char::from_u32),
            }?;
            Some((character, semicolon + 1))
        });
        match replacement {
            Some((character, length)) => {
                result.push(character);
                rest = &rest[length..];
            }
            None => {
                result.push('&');
                rest = &rest[1..];
            }
        }
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests;
