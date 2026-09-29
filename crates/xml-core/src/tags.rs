//! Localisation tolérante des balises et des paires ouvrante/fermante.
//!
//! Ce module fournit un analyseur lexical volontairement permissif : il ne
//! s'arrête jamais sur une erreur de syntaxe, afin de rester utilisable
//! pendant la saisie (documents mal formés, balises non fermées, guillemets
//! manquants...). Il sert de socle commun aux fonctionnalités LSP qui ont
//! besoin de relier une balise ouvrante à sa balise fermante :
//!
//! - `textDocument/documentHighlight` : [`XmlTagTree::tag_pair_at`] ;
//! - `textDocument/linkedEditingRange` : [`XmlTagTree::tag_pair_at`] puis
//!   [`XmlTagPair::name_ranges`] ;
//! - `textDocument/rename` : [`XmlTagTree::tag_pair_at`],
//!   [`qualified_name_parts`] pour distinguer préfixe et nom local, et
//!   [`scan_attributes`] pour les déclarations `xmlns:prefix` ;
//! - `textDocument/foldingRange` : [`XmlTagTree::elements`],
//!   [`XmlElement::end_tag`] et [`scan_markup`] (commentaires, CDATA,
//!   instructions de traitement, `<!DOCTYPE ...>`) ;
//! - `textDocument/selectionRange` : [`XmlTagTree::innermost_element_at`] et
//!   [`XmlTagTree::ancestors`].
//!
//! Tous les offsets sont des offsets d'octets UTF-8 dans la source et tombent
//! toujours sur une frontière de caractère. Les commentaires, sections CDATA,
//! instructions de traitement et déclarations `<!DOCTYPE ...>` sont ignorés
//! par [`scan_tags`] et listés à part par [`scan_markup`].

use std::ops::Range;

/// Nature d'une balise rencontrée dans la source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmlTagKind {
    /// Balise ouvrante `<name ...>` (éventuellement non terminée).
    Start,
    /// Balise fermante `</name>`.
    End,
    /// Balise auto-fermante `<name ... />`.
    SelfClosing,
}

/// Balise repérée lexicalement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlTag {
    pub kind: XmlTagKind,
    /// Étendue de la balise, du `<` jusqu'après le `>`. Pour une balise non
    /// terminée, l'étendue s'arrête au prochain `<` ou à la fin de la source.
    pub range: Range<usize>,
    /// Étendue du nom qualifié (`prefix:local`) de la balise.
    pub name: Range<usize>,
    /// Indique si la balise se termine bien par `>` ou `/>`.
    pub closed: bool,
}

impl XmlTag {
    /// Nom qualifié de la balise.
    pub fn name<'a>(&self, source: &'a str) -> &'a str {
        &source[self.name.clone()]
    }

    /// Indique si `offset` est sur le nom de la balise, bornes incluses
    /// (le curseur juste après le dernier caractère du nom compte).
    pub fn name_contains(&self, offset: usize) -> bool {
        self.name.start <= offset && offset <= self.name.end
    }
}

/// Élément reconstruit à partir des balises : balise ouvrante et, si elle
/// existe, balise fermante correspondante.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlElement {
    /// Balise ouvrante ou auto-fermante.
    pub start_tag: XmlTag,
    /// Balise fermante correspondante (`None` pour un élément auto-fermant
    /// ou non fermé).
    pub end_tag: Option<XmlTag>,
    /// Index de l'élément parent dans [`XmlTagTree::elements`].
    pub parent: Option<usize>,
    /// Profondeur (0 pour un élément racine).
    pub depth: usize,
}

impl XmlElement {
    /// Nom qualifié de l'élément.
    pub fn name<'a>(&self, source: &'a str) -> &'a str {
        self.start_tag.name(source)
    }

    /// Indique si l'élément est de la forme `<name />`.
    pub fn is_self_closing(&self) -> bool {
        self.start_tag.kind == XmlTagKind::SelfClosing
    }

    /// Indique si l'élément est auto-fermant ou possède sa balise fermante.
    pub fn is_closed(&self) -> bool {
        self.is_self_closing() || self.end_tag.is_some()
    }

    /// Étendue complète de l'élément, du `<` ouvrant jusqu'après le `>` de la
    /// balise fermante. Pour un élément non fermé, seule la balise ouvrante
    /// est couverte.
    pub fn range(&self) -> Range<usize> {
        let end = self
            .end_tag
            .as_ref()
            .map_or(self.start_tag.range.end, |tag| tag.range.end);
        self.start_tag.range.start..end
    }

    /// Étendue du contenu entre la balise ouvrante et la balise fermante.
    pub fn content_range(&self) -> Option<Range<usize>> {
        let end_tag = self.end_tag.as_ref()?;
        Some(self.start_tag.range.end..end_tag.range.start)
    }
}

/// Paire de noms de balises liés, trouvée depuis une position du curseur.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlTagPair {
    /// Côté de la paire sur lequel se trouve le curseur.
    pub cursor_on: XmlTagKind,
    /// Nom de la balise ouvrante ou auto-fermante (`None` pour une balise
    /// fermante orpheline).
    pub start_name: Option<Range<usize>>,
    /// Nom de la balise fermante (`None` pour un élément auto-fermant ou non
    /// fermé).
    pub end_name: Option<Range<usize>>,
    /// Index de l'élément dans [`XmlTagTree::elements`] (`None` pour une
    /// balise fermante orpheline).
    pub element: Option<usize>,
}

impl XmlTagPair {
    /// Étendues des noms existants, balise ouvrante en premier.
    pub fn name_ranges(&self) -> impl Iterator<Item = Range<usize>> + '_ {
        self.start_name.iter().chain(self.end_name.iter()).cloned()
    }

    /// Indique si les deux côtés de la paire existent.
    pub fn is_complete(&self) -> bool {
        self.start_name.is_some() && self.end_name.is_some()
    }
}

/// Arbre d'éléments reconstruit de façon tolérante.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XmlTagTree {
    elements: Vec<XmlElement>,
    orphan_end_tags: Vec<XmlTag>,
}

impl XmlTagTree {
    /// Analyse la source et apparie les balises.
    ///
    /// Une balise fermante est associée à l'élément ouvert le plus proche
    /// portant le même nom ; les éléments ouverts intermédiaires restent non
    /// fermés. Une balise fermante sans élément ouvert correspondant est
    /// conservée dans [`XmlTagTree::orphan_end_tags`].
    pub fn parse(source: &str) -> Self {
        let mut elements: Vec<XmlElement> = Vec::new();
        let mut orphan_end_tags = Vec::new();
        let mut open: Vec<usize> = Vec::new();

        for tag in scan_tags(source) {
            match tag.kind {
                XmlTagKind::Start | XmlTagKind::SelfClosing => {
                    let is_start = tag.kind == XmlTagKind::Start;
                    elements.push(XmlElement {
                        start_tag: tag,
                        end_tag: None,
                        parent: open.last().copied(),
                        depth: open.len(),
                    });
                    if is_start {
                        open.push(elements.len() - 1);
                    }
                }
                XmlTagKind::End => {
                    let name = tag.name(source);
                    let matching = open
                        .iter()
                        .rposition(|&index| elements[index].name(source) == name);
                    match matching {
                        Some(position) => {
                            let index = open[position];
                            open.truncate(position);
                            elements[index].end_tag = Some(tag);
                        }
                        None => orphan_end_tags.push(tag),
                    }
                }
            }
        }

        Self {
            elements,
            orphan_end_tags,
        }
    }

    /// Éléments dans l'ordre de leur balise ouvrante (un parent précède
    /// toujours ses enfants).
    pub fn elements(&self) -> &[XmlElement] {
        &self.elements
    }

    /// Balises fermantes sans balise ouvrante correspondante.
    pub fn orphan_end_tags(&self) -> &[XmlTag] {
        &self.orphan_end_tags
    }

    /// Retourne la paire de noms liée au nom de balise sous le curseur.
    ///
    /// Retourne `None` si le curseur n'est pas sur un nom de balise (contenu,
    /// attributs, commentaires...).
    pub fn tag_pair_at(&self, offset: usize) -> Option<XmlTagPair> {
        for (index, element) in self.elements.iter().enumerate() {
            if element.start_tag.range.start > offset {
                break;
            }
            let on_start = element.start_tag.name_contains(offset);
            let on_end = element
                .end_tag
                .as_ref()
                .is_some_and(|tag| tag.name_contains(offset));
            if on_start || on_end {
                return Some(XmlTagPair {
                    cursor_on: if on_start {
                        element.start_tag.kind
                    } else {
                        XmlTagKind::End
                    },
                    start_name: Some(element.start_tag.name.clone()),
                    end_name: element.end_tag.as_ref().map(|tag| tag.name.clone()),
                    element: Some(index),
                });
            }
        }
        self.orphan_end_tags
            .iter()
            .find(|tag| tag.name_contains(offset))
            .map(|tag| XmlTagPair {
                cursor_on: XmlTagKind::End,
                start_name: None,
                end_name: Some(tag.name.clone()),
                element: None,
            })
    }

    /// Index de l'élément le plus profond dont [`XmlElement::range`] contient
    /// `offset` (bornes incluses).
    pub fn innermost_element_at(&self, offset: usize) -> Option<usize> {
        let mut found = None;
        for (index, element) in self.elements.iter().enumerate() {
            if element.start_tag.range.start > offset {
                break;
            }
            let range = element.range();
            if range.start <= offset && offset <= range.end {
                found = Some(index);
            }
        }
        found
    }

    /// Ancêtres de l'élément `index`, du parent direct vers la racine.
    pub fn ancestors(&self, index: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(
            self.elements.get(index).and_then(|element| element.parent),
            |&parent| self.elements[parent].parent,
        )
    }
}

/// Sépare un nom qualifié en étendue de préfixe (sans `:`) et étendue de nom
/// local. `name` doit être une étendue de `source`.
pub fn qualified_name_parts(
    source: &str,
    name: Range<usize>,
) -> (Option<Range<usize>>, Range<usize>) {
    match source[name.clone()].find(':') {
        Some(colon) => (
            Some(name.start..name.start + colon),
            name.start + colon + 1..name.end,
        ),
        None => (None, name),
    }
}

/// Espace de noms réservé du préfixe `xml`.
pub const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";

/// Élément le plus proche (`element` lui-même inclus) qui déclare `prefix`
/// (`None` : espace de noms par défaut `xmlns`), avec l'attribut déclarant.
/// `attributes[i]` sont les attributs de la balise ouvrante de l'élément `i`
/// de `tree` (voir [`scan_attributes`]).
pub fn namespace_declaration<'t>(
    source: &str,
    tree: &XmlTagTree,
    attributes: &'t [Vec<XmlAttribute>],
    element: usize,
    prefix: Option<&str>,
) -> Option<(usize, &'t XmlAttribute)> {
    std::iter::once(element)
        .chain(tree.ancestors(element))
        .find_map(|index| {
            attributes
                .get(index)?
                .iter()
                .find(|attribute| {
                    let name = attribute.name(source);
                    match prefix {
                        Some(prefix) => name.strip_prefix("xmlns:") == Some(prefix),
                        None => name == "xmlns",
                    }
                })
                .map(|attribute| (index, attribute))
        })
}

/// Espace de noms de `prefix` dans le contexte de l'élément `element`, en
/// remontant les déclarations `xmlns` des ancêtres. Retourne `None` pour un
/// préfixe non déclaré et `Some(None)` sans espace de noms (préfixe absent
/// sans `xmlns` par défaut, ou `xmlns=""`). Le préfixe `xml` est prédéfini.
pub fn resolve_namespace<'s>(
    source: &'s str,
    tree: &XmlTagTree,
    attributes: &[Vec<XmlAttribute>],
    element: usize,
    prefix: Option<&str>,
) -> Option<Option<&'s str>> {
    if prefix == Some("xml") {
        return Some(Some(XML_NAMESPACE));
    }
    match namespace_declaration(source, tree, attributes, element, prefix) {
        Some((_, attribute)) => Some(
            attribute
                .value
                .clone()
                .map(|range| &source[range])
                .filter(|value| !value.is_empty()),
        ),
        None if prefix.is_none() => Some(None),
        None => None,
    }
}

/// Attribut repéré lexicalement dans une balise ouvrante ou auto-fermante.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlAttribute {
    /// Étendue du nom qualifié de l'attribut.
    pub name: Range<usize>,
    /// Étendue de la valeur, guillemets exclus (`None` sans `=` ou sans
    /// valeur). Une valeur dont le guillemet fermant manque s'arrête avant la
    /// fin de la balise.
    pub value: Option<Range<usize>>,
}

impl XmlAttribute {
    /// Nom qualifié de l'attribut.
    pub fn name<'a>(&self, source: &'a str) -> &'a str {
        &source[self.name.clone()]
    }

    /// Valeur brute de l'attribut (entités non résolues).
    pub fn value<'a>(&self, source: &'a str) -> Option<&'a str> {
        self.value.clone().map(|range| &source[range])
    }
}

/// Liste les attributs d'une balise ouvrante ou auto-fermante, de façon
/// tolérante. Retourne une liste vide pour une balise fermante.
pub fn scan_attributes(source: &str, tag: &XmlTag) -> Vec<XmlAttribute> {
    let mut attributes = Vec::new();
    if tag.kind == XmlTagKind::End {
        return attributes;
    }
    let bytes = &source.as_bytes()[..tag.range.end.min(source.len())];
    let mut index = tag.name.end;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>' | b'=') {
            index += 1;
            continue;
        }
        if matches!(byte, b'"' | b'\'') {
            index = find_byte(bytes, index + 1, byte).map_or(bytes.len(), |end| end + 1);
            continue;
        }
        let name = scan_name(bytes, index);
        if name.is_empty() {
            index += 1;
            continue;
        }
        index = skip_whitespace(bytes, name.end);
        let mut value = None;
        if bytes.get(index) == Some(&b'=') {
            index = skip_whitespace(bytes, index + 1);
            match bytes.get(index) {
                Some(&quote @ (b'"' | b'\'')) => {
                    let end = find_byte(bytes, index + 1, quote);
                    let value_end = end.unwrap_or_else(|| unterminated_value_end(bytes));
                    value = Some(index + 1..value_end.max(index + 1));
                    index = end.map_or(bytes.len(), |end| end + 1);
                }
                Some(_) => {
                    let unquoted = scan_name(bytes, index);
                    index = unquoted.end.max(index + 1);
                    if !unquoted.is_empty() {
                        value = Some(unquoted);
                    }
                }
                None => {}
            }
        } else {
            index = name.end;
        }
        attributes.push(XmlAttribute { name, value });
    }
    attributes
}

fn skip_whitespace(bytes: &[u8], mut index: usize) -> usize {
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }
    index
}

/// Fin d'une valeur non terminée : avant le `>` ou `/>` final de la balise.
fn unterminated_value_end(bytes: &[u8]) -> usize {
    if bytes.ends_with(b"/>") {
        bytes.len() - 2
    } else if bytes.ends_with(b">") {
        bytes.len() - 1
    } else {
        bytes.len()
    }
}

/// Nature d'une construction qui n'est pas une balise d'élément.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XmlMarkupKind {
    /// Commentaire `<!-- ... -->`.
    Comment,
    /// Section `<![CDATA[ ... ]]>`.
    CData,
    /// Instruction de traitement `<? ... ?>`, y compris le prologue
    /// `<?xml ...?>`.
    ProcessingInstruction,
    /// Déclaration `<! ... >` (typiquement `<!DOCTYPE ...>` avec son
    /// sous-ensemble interne `[...]`).
    Declaration,
}

/// Commentaire, section CDATA, instruction de traitement ou déclaration
/// repéré lexicalement hors des balises d'éléments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlMarkup {
    pub kind: XmlMarkupKind,
    /// Étendue complète, délimiteurs inclus. Une construction non terminée
    /// s'étend jusqu'à la fin de la source.
    pub range: Range<usize>,
    /// Étendue du contenu, délimiteurs exclus (`<!--`/`-->`, `<![CDATA[`/
    /// `]]>`, `<?`/`?>`, `<!`/`>`).
    pub content: Range<usize>,
    /// Indique si le délimiteur fermant est présent.
    pub closed: bool,
}

impl XmlMarkup {
    /// Contenu brut, délimiteurs exclus.
    pub fn content<'a>(&self, source: &'a str) -> &'a str {
        &source[self.content.clone()]
    }
}

/// Liste les balises d'éléments de la source dans l'ordre du document.
pub fn scan_tags(source: &str) -> Vec<XmlTag> {
    scan(source).0
}

/// Liste les commentaires, sections CDATA, instructions de traitement et
/// déclarations de premier niveau dans l'ordre du document. Les commentaires
/// du sous-ensemble interne d'une DTD font partie de la déclaration.
pub fn scan_markup(source: &str) -> Vec<XmlMarkup> {
    scan(source).1
}

/// Construit un [`XmlMarkup`] démarrant à `start`, dont le délimiteur ouvrant
/// mesure `open` octets et dont le parcours (par [`skip_past`] avec le
/// terminateur `close`) s'est arrêté à `end`.
fn markup(
    source: &[u8],
    kind: XmlMarkupKind,
    start: usize,
    end: usize,
    open: usize,
    close: &[u8],
) -> XmlMarkup {
    let content_start = (start + open).min(end);
    let closed = end >= content_start + close.len() && source[..end].ends_with(close);
    let content_end = if closed { end - close.len() } else { end };
    XmlMarkup {
        kind,
        range: start..end,
        content: content_start..content_end.max(content_start),
        closed,
    }
}

fn scan(source: &str) -> (Vec<XmlTag>, Vec<XmlMarkup>) {
    let bytes = source.as_bytes();
    let mut tags = Vec::new();
    let mut markups = Vec::new();
    let mut index = 0;

    while let Some(relative) = find_byte(bytes, index, b'<') {
        let start = relative;
        let rest = &bytes[start..];
        if rest.starts_with(b"<!--") {
            index = skip_past(bytes, start + 4, b"-->");
            markups.push(markup(
                bytes,
                XmlMarkupKind::Comment,
                start,
                index,
                4,
                b"-->",
            ));
        } else if rest.starts_with(b"<![CDATA[") {
            index = skip_past(bytes, start + 9, b"]]>");
            markups.push(markup(bytes, XmlMarkupKind::CData, start, index, 9, b"]]>"));
        } else if rest.starts_with(b"<?") {
            index = skip_past(bytes, start + 2, b"?>");
            markups.push(markup(
                bytes,
                XmlMarkupKind::ProcessingInstruction,
                start,
                index,
                2,
                b"?>",
            ));
        } else if rest.starts_with(b"<!") {
            let (end, closed) = skip_declaration(bytes, start + 2);
            index = end;
            markups.push(XmlMarkup {
                kind: XmlMarkupKind::Declaration,
                range: start..end,
                content: start + 2..if closed { end - 1 } else { end },
                closed,
            });
        } else if rest.starts_with(b"</") {
            let name = scan_name(bytes, start + 2);
            if name.is_empty() {
                index = start + 2;
                continue;
            }
            let (end, closed, _) = scan_tag_end(bytes, name.end);
            tags.push(XmlTag {
                kind: XmlTagKind::End,
                range: start..end,
                name,
                closed,
            });
            index = end;
        } else {
            let name = scan_name(bytes, start + 1);
            if name.is_empty() {
                index = start + 1;
                continue;
            }
            let (end, closed, self_closing) = scan_tag_end(bytes, name.end);
            tags.push(XmlTag {
                kind: if self_closing {
                    XmlTagKind::SelfClosing
                } else {
                    XmlTagKind::Start
                },
                range: start..end,
                name,
                closed,
            });
            index = end;
        }
    }

    (tags, markups)
}

fn find_byte(bytes: &[u8], from: usize, needle: u8) -> Option<usize> {
    bytes
        .get(from..)?
        .iter()
        .position(|&byte| byte == needle)
        .map(|position| from + position)
}

fn skip_past(bytes: &[u8], from: usize, terminator: &[u8]) -> usize {
    bytes
        .get(from..)
        .and_then(|rest| {
            rest.windows(terminator.len())
                .position(|window| window == terminator)
        })
        .map_or(bytes.len(), |position| from + position + terminator.len())
}

/// Ignore une déclaration `<!DOCTYPE ...>` y compris son sous-ensemble interne.
///
/// Retourne `(fin, fermée)`.
fn skip_declaration(bytes: &[u8], mut index: usize) -> (usize, bool) {
    let mut depth = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'"' | b'\'' => {
                let quote = bytes[index];
                index = find_byte(bytes, index + 1, quote).map_or(bytes.len(), |end| end + 1);
                continue;
            }
            b'<' if bytes[index..].starts_with(b"<!--") => {
                index = skip_past(bytes, index + 4, b"-->");
                continue;
            }
            b'[' => depth += 1,
            b']' => depth = depth.saturating_sub(1),
            b'>' if depth == 0 => return (index + 1, true),
            _ => {}
        }
        index += 1;
    }
    (bytes.len(), false)
}

fn is_name_byte(byte: u8) -> bool {
    !matches!(
        byte,
        b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>' | b'<' | b'=' | b'"' | b'\'' | b'!' | b'?'
    )
}

fn scan_name(bytes: &[u8], start: usize) -> Range<usize> {
    let length = bytes.get(start..).map_or(0, |rest| {
        rest.iter().take_while(|&&byte| is_name_byte(byte)).count()
    });
    start..start + length
}

/// Parcourt les attributs jusqu'à la fin de la balise.
///
/// Retourne `(fin, fermée, auto-fermante)`. Une balise non terminée s'arrête
/// avant le prochain `<` hors guillemets ou à la fin de la source.
fn scan_tag_end(bytes: &[u8], mut index: usize) -> (usize, bool, bool) {
    while index < bytes.len() {
        match bytes[index] {
            b'>' => return (index + 1, true, false),
            b'/' if bytes.get(index + 1) == Some(&b'>') => return (index + 2, true, true),
            b'<' => return (index, false, false),
            b'"' | b'\'' => {
                let quote = bytes[index];
                match find_byte(bytes, index + 1, quote) {
                    Some(end) => index = end + 1,
                    None => return (bytes.len(), false, false),
                }
            }
            _ => index += 1,
        }
    }
    (bytes.len(), false, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names<'a>(source: &'a str, pair: &XmlTagPair) -> Vec<&'a str> {
        pair.name_ranges().map(|range| &source[range]).collect()
    }

    fn offset_of(source: &str, needle: &str, nth: usize) -> usize {
        source
            .match_indices(needle)
            .nth(nth)
            .map(|(offset, _)| offset)
            .expect("needle should exist")
    }

    #[test]
    fn scans_start_end_and_self_closing_tags() {
        let source = "<?xml version=\"1.0\"?><root a=\"1\"><item/><x:y b='>'></x:y></root>";
        let tags = scan_tags(source);
        let summary: Vec<_> = tags
            .iter()
            .map(|tag| (tag.kind, tag.name(source), tag.closed))
            .collect();
        assert_eq!(
            summary,
            vec![
                (XmlTagKind::Start, "root", true),
                (XmlTagKind::SelfClosing, "item", true),
                (XmlTagKind::Start, "x:y", true),
                (XmlTagKind::End, "x:y", true),
                (XmlTagKind::End, "root", true),
            ]
        );
        assert_eq!(&source[tags[2].range.clone()], "<x:y b='>'>");
    }

    #[test]
    fn ignores_comments_cdata_processing_instructions_and_doctype() {
        let source = "<!DOCTYPE r [<!ELEMENT r ANY><!-- <x> -->]><r><!-- <a> --><![CDATA[<b>]]><?pi <c>?></r>";
        let tags = scan_tags(source);
        let names: Vec<_> = tags.iter().map(|tag| tag.name(source)).collect();
        assert_eq!(names, vec!["r", "r"]);
    }

    #[test]
    fn pairs_nested_elements_with_the_same_name() {
        let source = "<a><a></a></a>";
        let tree = XmlTagTree::parse(source);
        let outer = tree.tag_pair_at(1).unwrap();
        assert_eq!(outer.start_name, Some(1..2));
        assert_eq!(outer.end_name, Some(12..13));
        let inner = tree.tag_pair_at(8).unwrap();
        assert_eq!(inner.start_name, Some(4..5));
        assert_eq!(inner.end_name, Some(8..9));
        assert_eq!(inner.cursor_on, XmlTagKind::End);
        assert_eq!(tree.elements()[1].parent, Some(0));
        assert_eq!(tree.elements()[1].depth, 1);
    }

    #[test]
    fn cursor_outside_tag_names_finds_nothing() {
        let source = "<root attr=\"value\">text</root>";
        let tree = XmlTagTree::parse(source);
        assert!(tree.tag_pair_at(0).is_none());
        assert!(tree.tag_pair_at(offset_of(source, "attr", 0) + 1).is_none());
        assert!(tree.tag_pair_at(offset_of(source, "text", 0) + 1).is_none());
        assert!(tree.tag_pair_at(offset_of(source, "</", 0) + 1).is_none());
        let pair = tree.tag_pair_at(5).expect("end of the name is included");
        assert!(pair.is_complete());
    }

    #[test]
    fn self_closing_elements_have_a_single_name() {
        let source = "<root><ns:item /></root>";
        let tree = XmlTagTree::parse(source);
        let pair = tree
            .tag_pair_at(offset_of(source, "ns:item", 0) + 3)
            .unwrap();
        assert_eq!(pair.cursor_on, XmlTagKind::SelfClosing);
        assert_eq!(names(source, &pair), vec!["ns:item"]);
    }

    #[test]
    fn tolerates_unclosed_and_mismatched_documents() {
        let source = "<root><open><child></child></root><stray></oops>";
        let tree = XmlTagTree::parse(source);
        let open = tree.tag_pair_at(offset_of(source, "open", 0)).unwrap();
        assert_eq!(names(source, &open), vec!["open"]);
        assert!(!open.is_complete());
        let root = tree.tag_pair_at(offset_of(source, "root", 1)).unwrap();
        assert_eq!(names(source, &root), vec!["root", "root"]);
        let orphan = tree.tag_pair_at(offset_of(source, "oops", 0)).unwrap();
        assert_eq!(orphan.start_name, None);
        assert_eq!(names(source, &orphan), vec!["oops"]);

        for source in [
            "<",
            "<a",
            "<a b=\"",
            "</",
            "<a <b>",
            "< a>",
            "<a></",
            "<!--",
            "<![CDATA[",
        ] {
            let tree = XmlTagTree::parse(source);
            for offset in 0..=source.len() {
                let _ = tree.tag_pair_at(offset);
                let _ = tree.innermost_element_at(offset);
            }
        }
        let tags = scan_tags("<a <b>");
        assert_eq!(tags[0].range, 0..3);
        assert!(!tags[0].closed);
    }

    #[test]
    fn finds_innermost_element_and_ancestors() {
        let source = "<a><b><c/></b></a>";
        let tree = XmlTagTree::parse(source);
        let c = tree
            .innermost_element_at(offset_of(source, "c", 0))
            .unwrap();
        assert_eq!(tree.elements()[c].name(source), "c");
        let ancestors: Vec<_> = tree
            .ancestors(c)
            .map(|index| tree.elements()[index].name(source))
            .collect();
        assert_eq!(ancestors, vec!["b", "a"]);
        let b = &tree.elements()[1];
        assert_eq!(&source[b.content_range().unwrap()], "<c/>");
    }

    #[test]
    fn splits_qualified_names() {
        let source = "<xs:element>";
        let (prefix, local) = qualified_name_parts(source, 1..11);
        assert_eq!(prefix.map(|range| &source[range]), Some("xs"));
        assert_eq!(&source[local], "element");
        let (prefix, local) = qualified_name_parts(source, 4..11);
        assert_eq!(prefix, None);
        assert_eq!(local, 4..11);
    }

    #[test]
    fn scans_attributes_of_start_and_self_closing_tags() {
        let source = "<x:a xmlns:x=\"urn:x\"  b = 'v>1' c d=e\r\n\tx:f=\"\"/><b g=\"é\"></b>";
        let tags = scan_tags(source);
        let summary: Vec<_> = scan_attributes(source, &tags[0])
            .iter()
            .map(|attribute| (attribute.name(source), attribute.value(source)))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("xmlns:x", Some("urn:x")),
                ("b", Some("v>1")),
                ("c", None),
                ("d", Some("e")),
                ("x:f", Some("")),
            ]
        );
        let b = scan_attributes(source, &tags[1]);
        assert_eq!(b[0].value(source), Some("é"));
        assert!(scan_attributes(source, &tags[2]).is_empty());
    }

    #[test]
    fn scans_attributes_of_malformed_tags() {
        for source in [
            "<a b=\"",
            "<a b=",
            "<a b=>",
            "<a \"x\" c='1'>",
            "<a b=\"1\"c=\"2\">",
            "<a b=\"x/>",
            "<a b=\">",
            "<a =>",
        ] {
            let tags = scan_tags(source);
            for attribute in scan_attributes(source, &tags[0]) {
                let _ = attribute.name(source);
                let _ = attribute.value(source);
            }
        }
        let names = |source: &str| -> Vec<(String, Option<String>)> {
            let tags = scan_tags(source);
            scan_attributes(source, &tags[0])
                .iter()
                .map(|attribute| {
                    (
                        attribute.name(source).to_owned(),
                        attribute.value(source).map(str::to_owned),
                    )
                })
                .collect()
        };
        assert_eq!(
            names("<a \"x\" c='1'>"),
            vec![("c".into(), Some("1".into()))]
        );
        assert_eq!(names("<a b=\"x/>"), vec![("b".into(), Some("x".into()))]);
        assert_eq!(
            names("<a b=\"1\"c=\"2\">"),
            vec![
                ("b".into(), Some("1".into())),
                ("c".into(), Some("2".into()))
            ]
        );
        assert_eq!(names("<a b=>"), vec![("b".into(), None)]);
    }

    #[test]
    fn scans_comments_cdata_processing_instructions_and_declarations() {
        let source = "<?xml version=\"1.0\"?>\n<!DOCTYPE r [\n<!-- ]> -->\n<!ELEMENT r ANY>\n]>\n<r><!-- #region --><![CDATA[<x>]]><?pi a?></r>";
        let summary: Vec<_> = scan_markup(source)
            .iter()
            .map(|markup| (markup.kind, markup.content(source), markup.closed))
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    XmlMarkupKind::ProcessingInstruction,
                    "xml version=\"1.0\"",
                    true
                ),
                (
                    XmlMarkupKind::Declaration,
                    "DOCTYPE r [\n<!-- ]> -->\n<!ELEMENT r ANY>\n]",
                    true
                ),
                (XmlMarkupKind::Comment, " #region ", true),
                (XmlMarkupKind::CData, "<x>", true),
                (XmlMarkupKind::ProcessingInstruction, "pi a", true),
            ]
        );
        let markups = scan_markup(source);
        assert_eq!(&source[markups[2].range.clone()], "<!-- #region -->");
    }

    #[test]
    fn scans_unterminated_markup_to_the_end_of_the_source() {
        for (source, kind, content) in [
            ("<!-- a", XmlMarkupKind::Comment, " a"),
            ("<!-->", XmlMarkupKind::Comment, ">"),
            ("<!--", XmlMarkupKind::Comment, ""),
            ("<![CDATA[x]]", XmlMarkupKind::CData, "x]]"),
            ("<?pi", XmlMarkupKind::ProcessingInstruction, "pi"),
            ("<?>", XmlMarkupKind::ProcessingInstruction, ">"),
            (
                "<!DOCTYPE r [<!ELEMENT r ANY>",
                XmlMarkupKind::Declaration,
                "DOCTYPE r [<!ELEMENT r ANY>",
            ),
        ] {
            let markups = scan_markup(source);
            assert_eq!(markups.len(), 1, "{source}");
            assert_eq!(markups[0].kind, kind, "{source}");
            assert!(!markups[0].closed, "{source}");
            assert_eq!(markups[0].content(source), content, "{source}");
            assert_eq!(markups[0].range, 0..source.len(), "{source}");
        }
        let empty = scan_markup("<!---->");
        assert!(empty[0].closed);
        assert_eq!(empty[0].content("<!---->"), "");
    }

    #[test]
    fn keeps_offsets_on_character_boundaries() {
        let source = "<élément attr=\"é\">texte</élément>";
        let tree = XmlTagTree::parse(source);
        let pair = tree.tag_pair_at(1).unwrap();
        assert_eq!(names(source, &pair), vec!["élément", "élément"]);
    }
}
