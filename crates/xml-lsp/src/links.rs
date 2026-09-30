//! `textDocument/documentLink` et navigation vers les fichiers référencés,
//! comme LemMinX.
//!
//! Sont reconnus, par espace de noms (le préfixe est résolu via les
//! déclarations `xmlns`, les préfixes conventionnels `xsi`, `xs`/`xsd`, `xi`
//! et `xsl` sont acceptés s'ils ne sont pas déclarés) :
//!
//! - chaque emplacement (un jeton sur deux) de `xsi:schemaLocation` et la
//!   valeur de `xsi:noNamespaceSchemaLocation` ;
//! - `schemaLocation` de `xs:include`, `xs:import`, `xs:redefine` et
//!   `xs:override` ;
//! - `href` de `xi:include` (XInclude) ;
//! - `href` de `xsl:import` et `xsl:include` ;
//! - le pseudo-attribut `href` des instructions `<?xml-stylesheet ...?>` et
//!   `<?xml-model ...?>` ;
//! - l'identifiant système de `<!DOCTYPE ... SYSTEM|PUBLIC ...>`.
//!
//! Les chemins relatifs sont résolus par rapport au document (y compris sous
//! forme encodée en pourcentage), les URL `http(s)` sont conservées telles
//! quelles. Seuls les fichiers locaux existants et les URL `http(s)`
//! produisent un lien. `textDocument/definition` sur ces mêmes valeurs mène
//! au début du fichier cible (0:0), ce qui rend les liens utilisables par
//! cmd-clic même sans `documentLink`.

use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use serde_json::{Value, json};
use xml_core::tags::{
    XmlAttribute, XmlMarkupKind, XmlTagTree, qualified_name_parts, resolve_namespace,
    scan_attributes, scan_markup,
};
use xsd_core::{model::XSD_NAMESPACE, resolve_path};

use crate::{path_to_uri, percent_decode, selection::LineIndex, uri_to_path};

/// Espace de noms `xsi`.
pub const XSI_NAMESPACE: &str = "http://www.w3.org/2001/XMLSchema-instance";
/// Espace de noms XInclude 1.0.
pub const XINCLUDE_NAMESPACE: &str = "http://www.w3.org/2001/XInclude";
/// Ancien espace de noms XInclude (brouillon 2003), encore rencontré.
const XINCLUDE_2003_NAMESPACE: &str = "http://www.w3.org/2003/XInclude";
/// Espace de noms XSLT.
pub const XSLT_NAMESPACE: &str = "http://www.w3.org/1999/XSL/Transform";

/// Nature d'une référence vers un autre fichier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    /// Emplacement d'une paire de `xsi:schemaLocation`.
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
    /// Identifiant système de `<!DOCTYPE>`.
    Doctype,
    /// `xsl:import/@href`.
    XslImport,
    /// `xsl:include/@href`.
    XslInclude,
}

impl LinkKind {
    fn label(self) -> &'static str {
        match self {
            Self::SchemaLocation | Self::NoNamespaceSchemaLocation => "Ouvrir le schéma XSD",
            Self::XsdInclude => "Ouvrir le schéma inclus",
            Self::XsdImport => "Ouvrir le schéma importé",
            Self::XsdRedefine => "Ouvrir le schéma redéfini",
            Self::XsdOverride => "Ouvrir le schéma surchargé",
            Self::XInclude => "Ouvrir le document inclus (XInclude)",
            Self::Stylesheet => "Ouvrir la feuille de style",
            Self::XmlModel => "Ouvrir le modèle (xml-model)",
            Self::Doctype => "Ouvrir la DTD",
            Self::XslImport => "Ouvrir la feuille XSLT importée",
            Self::XslInclude => "Ouvrir la feuille XSLT incluse",
        }
    }
}

/// Référence lexicale vers un autre fichier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkReference {
    pub kind: LinkKind,
    /// Étendue de la valeur dans la source (guillemets et espaces de bord
    /// exclus).
    pub range: Range<usize>,
    /// Valeur, entités XML résolues.
    pub value: String,
}

/// Cible résolue d'une référence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// Fichier local existant.
    File(PathBuf),
    /// URL `http` ou `https`, conservée telle quelle.
    Url(String),
}

impl LinkTarget {
    /// URI de la cible (`file://...` ou l'URL).
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

/// Référence dont la cible a été résolue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentLink {
    pub reference: LinkReference,
    pub target: LinkTarget,
}

impl DocumentLink {
    /// Info-bulle affichée par le client.
    pub fn tooltip(&self) -> String {
        format!(
            "{} : {}",
            self.reference.kind.label(),
            self.target.display()
        )
    }
}

/// Liste les références vers d'autres fichiers, dans l'ordre du document,
/// sans résolution.
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
                push_value(source, &mut references, kind, value.clone());
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
                    for token in tokens(source, value).skip(1).step_by(2) {
                        push_value(source, &mut references, LinkKind::SchemaLocation, token);
                    }
                }
                "noNamespaceSchemaLocation" => push_value(
                    source,
                    &mut references,
                    LinkKind::NoNamespaceSchemaLocation,
                    value,
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
                    push_value(source, &mut references, kind, value);
                }
            }
            XmlMarkupKind::Declaration => {
                if let Some(value) = doctype_system_literal(source, markup.content.clone()) {
                    push_value(source, &mut references, LinkKind::Doctype, value);
                }
            }
            XmlMarkupKind::Comment | XmlMarkupKind::CData => {}
        }
    }

    references.sort_by_key(|reference| reference.range.start);
    references
}

/// Résout les références du document `document_uri` ; ne garde que les
/// fichiers locaux existants et les URL `http(s)`.
pub fn document_links(document_uri: &str, source: &str) -> Vec<DocumentLink> {
    link_references(source)
        .into_iter()
        .filter_map(|reference| {
            let target = resolve_target(document_uri, &reference.value)?;
            Some(DocumentLink { reference, target })
        })
        .collect()
}

/// Réponse JSON de `textDocument/documentLink`.
pub fn document_links_json(document_uri: &str, source: &str) -> Value {
    let lines = LineIndex::new(source);
    Value::Array(
        document_links(document_uri, source)
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

/// Réponse de `textDocument/definition` si `offset` est sur une référence :
/// le début du fichier local cible, ou une liste vide si la cible n'est pas
/// un fichier local existant (une URL reste ouverte par `documentLink`).
/// `None` si `offset` n'est sur aucune référence. Avec `link_support`, la
/// réponse est un `LocationLink[]` dont l'origine est la valeur entière.
pub fn definition(
    document_uri: &str,
    source: &str,
    offset: usize,
    link_support: bool,
) -> Option<Value> {
    let reference = link_references(source)
        .into_iter()
        .find(|reference| reference.range.start <= offset && offset <= reference.range.end)?;
    let Some(LinkTarget::File(path)) = resolve_target(document_uri, &reference.value) else {
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

/// Résout `value` par rapport au document `document_uri`. Retourne `None`
/// pour un autre schéma d'URI qu'`http(s)`/`file`, un fichier absent ou un
/// chemin relatif dans un document qui n'est pas un fichier local.
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
                path.is_file().then_some(LinkTarget::File(path))
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
        path.is_file().then_some(LinkTarget::File(path))
    })
}

fn lsp_range(lines: &LineIndex, source: &str, range: &Range<usize>) -> Value {
    json!({
        "start": lines.position(source, range.start),
        "end": lines.position(source, range.end),
    })
}

/// Espace de noms usuel d'un préfixe non déclaré.
fn conventional_namespace(prefix: &str) -> Option<&'static str> {
    match prefix {
        "xsi" => Some(XSI_NAMESPACE),
        "xs" | "xsd" => Some(XSD_NAMESPACE),
        "xi" => Some(XINCLUDE_NAMESPACE),
        "xsl" => Some(XSLT_NAMESPACE),
        _ => None,
    }
}

/// Ajoute la référence de valeur `range` (espaces de bord retirés), si elle
/// n'est pas vide.
fn push_value(
    source: &str,
    references: &mut Vec<LinkReference>,
    kind: LinkKind,
    range: Range<usize>,
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
    });
}

/// Jetons séparés par des espaces dans `range`.
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

/// Premier jeton (sans espace ni `=`/guillemet) à partir de `start`, espaces
/// de tête ignorés.
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

/// Valeur (guillemets exclus) du pseudo-attribut `name` d'une instruction
/// de traitement, cherché dans `range`.
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
            // Jeton isolé (ou caractère inattendu) : on passe au suivant.
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

/// Littéral système (guillemets exclus) d'une déclaration
/// `DOCTYPE nom SYSTEM "..."` ou `DOCTYPE nom PUBLIC "..." "..."`.
fn doctype_system_literal(source: &str, content: Range<usize>) -> Option<Range<usize>> {
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
    let mut literal = None;
    for _ in 0..literals {
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
    literal
}

/// Schéma d'URI de `value` (au moins deux caractères, pour ne pas confondre
/// une lettre de lecteur Windows `C:` avec un schéma).
fn uri_scheme(value: &str) -> Option<&str> {
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

/// Résout les entités prédéfinies et les références de caractères.
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
mod tests {
    use super::*;
    use std::fs;

    /// Répertoire temporaire propre au test, supprimé à la fin.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("xml-lsp-links-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("temp dir should be created");
            Self(path)
        }

        fn file(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).expect("parent should be created");
            fs::write(&path, "<root/>").expect("file should be written");
            path
        }

        fn document_uri(&self, name: &str) -> String {
            path_to_uri(&self.0.join(name))
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn references(source: &str) -> Vec<(LinkKind, &str, String)> {
        link_references(source)
            .into_iter()
            .map(|reference| {
                (
                    reference.kind,
                    &source[reference.range.clone()],
                    reference.value,
                )
            })
            .collect()
    }

    #[test]
    fn finds_schema_locations_by_namespace_with_any_prefix() {
        let source = r#"<r:root xmlns:r="urn:r" xmlns:inst="http://www.w3.org/2001/XMLSchema-instance"
  inst:schemaLocation="  urn:a  a.xsd
     urn:b	sub/b.xsd urn:odd "
  inst:noNamespaceSchemaLocation=" none.xsd "
  xsi:schemaLocation="urn:c c.xsd"
  other:schemaLocation="urn:d d.xsd" schemaLocation="urn:e e.xsd"/>"#;
        assert_eq!(
            references(source),
            vec![
                (LinkKind::SchemaLocation, "a.xsd", "a.xsd".to_owned()),
                (
                    LinkKind::SchemaLocation,
                    "sub/b.xsd",
                    "sub/b.xsd".to_owned()
                ),
                (
                    LinkKind::NoNamespaceSchemaLocation,
                    "none.xsd",
                    "none.xsd".to_owned()
                ),
                // Préfixe `xsi` non déclaré : espace de noms conventionnel.
                (LinkKind::SchemaLocation, "c.xsd", "c.xsd".to_owned()),
            ]
        );
    }

    #[test]
    fn a_redeclared_xsi_prefix_is_not_the_instance_namespace() {
        let source = r#"<root xmlns:xsi="urn:not-xsi" xsi:noNamespaceSchemaLocation="a.xsd"/>"#;
        assert!(references(source).is_empty());
    }

    #[test]
    fn finds_xsd_xinclude_and_xslt_references() {
        let source = r#"<schema xmlns="http://www.w3.org/2001/XMLSchema" xmlns:x="http://www.w3.org/2001/XInclude">
  <include schemaLocation="inc.xsd"/>
  <import namespace="urn:i" schemaLocation="imp.xsd"></import>
  <import namespace="urn:no-location"/>
  <redefine schemaLocation="red.xsd"/>
  <override schemaLocation="ovr.xsd"/>
  <x:include href="part.xml" parse="xml"/>
  <x:include href=""/>
  <element name="include" type="string"/>
  <t:stylesheet xmlns:t="http://www.w3.org/1999/XSL/Transform">
    <t:import href="base.xsl"/><t:include href='common.xsl'/>
  </t:stylesheet>
  <xsl:include href="undeclared.xsl"/>
</schema>"#;
        assert_eq!(
            references(source)
                .into_iter()
                .map(|(kind, text, _)| (kind, text))
                .collect::<Vec<_>>(),
            vec![
                (LinkKind::XsdInclude, "inc.xsd"),
                (LinkKind::XsdImport, "imp.xsd"),
                (LinkKind::XsdRedefine, "red.xsd"),
                (LinkKind::XsdOverride, "ovr.xsd"),
                (LinkKind::XInclude, "part.xml"),
                (LinkKind::XslImport, "base.xsl"),
                (LinkKind::XslInclude, "common.xsl"),
                (LinkKind::XslInclude, "undeclared.xsl"),
            ]
        );
    }

    #[test]
    fn elements_outside_the_expected_namespace_are_ignored() {
        let source = r#"<root xmlns:xs="urn:other"><xs:include schemaLocation="a.xsd"/><include href="b.xml"/></root>"#;
        assert!(references(source).is_empty());
    }

    #[test]
    fn finds_processing_instruction_and_doctype_references() {
        let source = "<?xml version=\"1.0\"?>\r\n\
<?xml-stylesheet type=\"text/xsl\" href=\"style.xsl\"?>\r\n\
<?xml-model href='model.rng' schematypens=\"http://relaxng.org/ns/structure/1.0\"?>\r\n\
<?xml-stylesheet xhref=\"no.css\"?>\r\n\
<?other href=\"no.css\"?>\r\n\
<!DOCTYPE note PUBLIC \"-//W3C//DTD//EN\" \"note.dtd\" [\r\n<!ENTITY e \"v\">\r\n]>\r\n\
<note/>";
        assert_eq!(
            references(source)
                .into_iter()
                .map(|(kind, text, _)| (kind, text))
                .collect::<Vec<_>>(),
            vec![
                (LinkKind::Stylesheet, "style.xsl"),
                (LinkKind::XmlModel, "model.rng"),
                (LinkKind::Doctype, "note.dtd"),
            ]
        );
        assert_eq!(
            references("<!DOCTYPE note SYSTEM 'sys.dtd'><note/>")[0].1,
            "sys.dtd"
        );
        assert!(references("<!DOCTYPE note [<!ELEMENT note ANY>]><note/>").is_empty());
    }

    #[test]
    fn values_are_unescaped_but_ranges_stay_raw() {
        let source = r#"<root xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:noNamespaceSchemaLocation="a&amp;b&#x20;c&#46;xsd&unknown;"/>"#;
        assert_eq!(
            references(source),
            vec![(
                LinkKind::NoNamespaceSchemaLocation,
                "a&amp;b&#x20;c&#46;xsd&unknown;",
                "a&b c.xsd&unknown;".to_owned()
            )]
        );
    }

    #[test]
    fn malformed_documents_do_not_panic() {
        for source in [
            "<root xsi:schemaLocation=\"urn:a a.xsd",
            "<?xml-stylesheet href=\"a.css",
            "<?xml-stylesheet href=",
            "<?xml-stylesheet = href",
            "<!DOCTYPE",
            "<!DOCTYPE a SYSTEM",
            "<!DOCTYPE a PUBLIC \"p\"",
            "<xs:include schemaLocation=",
            "é<xi:include href=\"😀.xml\"",
        ] {
            let _ = link_references(source);
        }
    }

    #[test]
    fn resolves_relative_encoded_absolute_and_remote_targets() {
        let dir = TempDir::new("resolve");
        let spaced = dir.file("my schemas/a b.xsd");
        let nested = dir.file("sub/c.xsd");
        let document = dir.document_uri("docs dir/doc.xml");

        assert_eq!(
            resolve_target(&document, "../my schemas/a b.xsd"),
            Some(LinkTarget::File(spaced.clone()))
        );
        assert_eq!(
            resolve_target(&document, "../my%20schemas/a%20b.xsd"),
            Some(LinkTarget::File(spaced.clone()))
        );
        assert_eq!(
            resolve_target(&document, "../sub/./c.xsd#fragment"),
            Some(LinkTarget::File(nested.clone()))
        );
        assert_eq!(
            resolve_target(&document, &nested.to_string_lossy()),
            Some(LinkTarget::File(nested.clone()))
        );
        assert_eq!(
            resolve_target(&document, &path_to_uri(&spaced)),
            Some(LinkTarget::File(spaced))
        );
        assert_eq!(
            resolve_target(&document, "https://example.com/s.xsd?v=1&x=2"),
            Some(LinkTarget::Url(
                "https://example.com/s.xsd?v=1&x=2".to_owned()
            ))
        );
        assert_eq!(
            resolve_target(&document, "HTTP://example.com/s.xsd"),
            Some(LinkTarget::Url("HTTP://example.com/s.xsd".to_owned()))
        );
        for missing in [
            "../missing.xsd",
            "urn:example:schema",
            "ftp://example.com/a.xsd",
            "C:\\schemas\\a.xsd",
            "",
            "  ",
            "../sub",
        ] {
            assert_eq!(resolve_target(&document, missing), None, "{missing}");
        }
        // Document non enregistré : pas de base pour un chemin relatif.
        assert_eq!(resolve_target("untitled:Untitled-1", "c.xsd"), None);
    }

    #[test]
    fn document_links_keep_existing_files_and_urls_with_utf16_ranges() {
        let dir = TempDir::new("document");
        dir.file("a.xsd");
        let uri = dir.document_uri("doc.xml");
        let source = "<?xml-stylesheet href=\"missing.css\"?>\r\n<😀 xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\r\n xsi:schemaLocation=\"urn:a a.xsd urn:b http://example.com/b.xsd\"/>";
        let links = document_links_json(&uri, source);
        let a_start = source.find(" a.xsd").unwrap() + 1 - source.rfind('\n').unwrap() - 1;
        assert_eq!(
            links,
            json!([
                {
                    "range": {
                        "start": {"line": 2, "character": a_start},
                        "end": {"line": 2, "character": a_start + 5},
                    },
                    "target": path_to_uri(&dir.0.join("a.xsd")),
                    "tooltip": format!("Ouvrir le schéma XSD : {}", dir.0.join("a.xsd").display()),
                },
                {
                    "range": {
                        "start": {"line": 2, "character": a_start + 12},
                        "end": {"line": 2, "character": a_start + 36},
                    },
                    "target": "http://example.com/b.xsd",
                    "tooltip": "Ouvrir le schéma XSD : http://example.com/b.xsd",
                },
            ])
        );
        // UTF-16 : l'emoji compte pour deux unités.
        let emoji = "<a xmlns:xi=\"http://www.w3.org/2001/XInclude\"><xi:include href=\"😀/../a.xsd\"/></a>";
        let emoji_uri = dir.document_uri("emoji.xml");
        let range = &document_links_json(&emoji_uri, emoji)[0]["range"];
        let start = emoji.find("😀").unwrap();
        assert_eq!(range["start"]["character"], start);
        assert_eq!(range["end"]["character"], start + 2 + "/../a.xsd".len());
    }

    #[test]
    fn definition_targets_the_start_of_the_linked_file() {
        let dir = TempDir::new("definition");
        let schema = dir.file("a.xsd");
        let uri = dir.document_uri("doc.xml");
        let source = "<root xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"a.xsd\" xsi:schemaLocation=\"urn:x http://example.com/x.xsd urn:y missing.xsd\"/>";
        let value = source.find("a.xsd").unwrap();
        let zero =
            json!({"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}});

        for offset in [value, value + 2, value + 5] {
            assert_eq!(
                definition(&uri, source, offset, false),
                Some(json!([{"uri": path_to_uri(&schema), "range": zero}]))
            );
        }
        assert_eq!(
            definition(&uri, source, value, true),
            Some(json!([{
                "originSelectionRange": {
                    "start": {"line": 0, "character": value},
                    "end": {"line": 0, "character": value + 5},
                },
                "targetUri": path_to_uri(&schema),
                "targetRange": zero,
                "targetSelectionRange": zero,
            }]))
        );
        let url = source.find("http://example").unwrap();
        assert_eq!(definition(&uri, source, url + 3, false), Some(json!([])));
        let missing = source.find("missing").unwrap();
        assert_eq!(definition(&uri, source, missing, false), Some(json!([])));
        // Hors valeur : laissé à la définition d'élément.
        assert_eq!(definition(&uri, source, 2, false), None);
        assert_eq!(definition(&uri, source, value - 1, false), None);
        let namespace = source.find("urn:x").unwrap();
        assert_eq!(definition(&uri, source, namespace + 1, false), None);
    }
}
