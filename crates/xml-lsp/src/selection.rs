//! `textDocument/selectionRange` : extension progressive de la sélection,
//! comme « Expand Selection » de LemMinX ou « Extend Selection »
//! d'IntelliJ.
//!
//! Pour chaque position, la chaîne va du plus petit au plus grand :
//!
//! - nom de balise : préfixe ou nom local, nom qualifié, balise, élément ;
//! - attribut : mot, jeton de la valeur, valeur sans guillemets, valeur avec
//!   guillemets, attribut complet, balise ouvrante, élément ;
//! - texte : mot, nœud texte sans les blancs, nœud texte, contenu de
//!   l'élément sans les blancs, contenu, élément ;
//! - commentaire, CDATA, instruction de traitement, déclaration : mot (ou
//!   cible de l'instruction), contenu sans les blancs, contenu, construction
//!   complète, puis le contenu de l'élément englobant ;
//!
//! puis, pour chaque ancêtre : contenu sans les blancs, contenu, élément, et
//! enfin le document entier. Chaque étendue contient strictement la
//! précédente (les doublons sont éliminés).

use std::ops::Range;

use serde_json::{Value, json};
use xml_core::tags::{
    XmlMarkup, XmlMarkupKind, XmlTag, XmlTagTree, qualified_name_parts, scan_attributes,
    scan_markup, scan_tags,
};

/// Retourne la réponse LSP (`SelectionRange[]`) pour les offsets demandés,
/// dans le même ordre.
pub fn selection_ranges(source: &str, offsets: &[usize]) -> Vec<Value> {
    let document = Document::new(source);
    let lines = LineIndex::new(source);
    offsets
        .iter()
        .map(|&offset| {
            let offset = floor_char_boundary(source, offset);
            let mut chain = document.chain(offset);
            if chain.is_empty() {
                chain.push(offset..offset);
            }
            chain.iter().rev().fold(Value::Null, |parent, range| {
                let mut value = json!({
                    "range": {
                        "start": lines.position(source, range.start),
                        "end": lines.position(source, range.end),
                    },
                });
                if !parent.is_null() {
                    value["parent"] = parent;
                }
                value
            })
        })
        .collect()
}

/// Analyse lexicale partagée par toutes les positions d'une requête.
struct Document<'a> {
    source: &'a str,
    tree: XmlTagTree,
    markups: Vec<XmlMarkup>,
    orphans: Vec<XmlTag>,
    /// Étendues de toutes les constructions (balises et markup), triées.
    constructs: Vec<Range<usize>>,
}

impl<'a> Document<'a> {
    fn new(source: &'a str) -> Self {
        let tree = XmlTagTree::parse(source);
        let markups = scan_markup(source);
        let orphans = tree.orphan_end_tags().to_vec();
        let mut constructs: Vec<Range<usize>> = scan_tags(source)
            .into_iter()
            .map(|tag| tag.range)
            .chain(markups.iter().map(|markup| markup.range.clone()))
            .collect();
        constructs.sort_by_key(|range| range.start);
        Self {
            source,
            tree,
            markups,
            orphans,
            constructs,
        }
    }

    /// Chaîne d'étendues, de la plus petite à la plus grande.
    fn chain(&self, offset: usize) -> Vec<Range<usize>> {
        let mut chain = Chain::new(offset);
        let elements = self.tree.elements();

        let on_tag = elements.iter().enumerate().find_map(|(index, element)| {
            if self.tag_contains(&element.start_tag, offset) {
                Some((index, &element.start_tag))
            } else {
                element
                    .end_tag
                    .as_ref()
                    .filter(|tag| self.tag_contains(tag, offset))
                    .map(|tag| (index, tag))
            }
        });

        if let Some((index, tag)) = on_tag {
            self.push_tag(&mut chain, tag, offset);
            self.push_element(&mut chain, index, false);
        } else if let Some(tag) = self
            .orphans
            .iter()
            .find(|tag| self.tag_contains(tag, offset))
        {
            self.push_tag(&mut chain, tag, offset);
            self.push_enclosing(&mut chain, tag.range.clone());
        } else if let Some(markup) = self
            .markups
            .iter()
            .find(|markup| self.construct_contains(&markup.range, markup.closed, offset))
        {
            self.push_markup(&mut chain, markup, offset);
            self.push_enclosing(&mut chain, markup.range.clone());
        } else {
            let text = self.text_node_at(offset);
            chain.push(word_at(self.source, &text, offset, is_word_char));
            chain.push(trim(self.source, text.clone()));
            chain.push(text);
            self.push_enclosing(&mut chain, offset..offset);
        }

        chain.push(0..self.source.len());
        chain.ranges
    }

    fn tag_contains(&self, tag: &XmlTag, offset: usize) -> bool {
        self.construct_contains(&tag.range, tag.closed, offset)
    }

    /// Une construction fermée contient `[début, fin)` ; une construction
    /// non terminée contient aussi sa fin (curseur en fin de saisie).
    fn construct_contains(&self, range: &Range<usize>, closed: bool, offset: usize) -> bool {
        range.start <= offset && (offset < range.end || (!closed && offset == range.end))
    }

    /// Nom (préfixe ou nom local, puis nom qualifié), attribut, balise.
    fn push_tag(&self, chain: &mut Chain, tag: &XmlTag, offset: usize) {
        if tag.name_contains(offset) {
            self.push_name(chain, tag.name.clone(), offset);
        } else if let Some(attribute) =
            scan_attributes(self.source, tag)
                .into_iter()
                .find(|attribute| {
                    attribute.name.start <= offset
                        && offset <= self.attribute_range(&attribute.name, &attribute.value).end
                })
        {
            if offset <= attribute.name.end {
                self.push_name(chain, attribute.name.clone(), offset);
            } else if let Some(value) = attribute
                .value
                .clone()
                .filter(|value| value.start <= offset && offset <= value.end)
            {
                chain.push(word_at(self.source, &value, offset, is_word_char));
                chain.push(word_at(self.source, &value, offset, |c| !c.is_whitespace()));
                chain.push(value.clone());
                chain.push(self.quoted_value(&value));
            }
            chain.push(self.attribute_range(&attribute.name, &attribute.value));
        }
        chain.push(tag.range.clone());
    }

    fn push_name(&self, chain: &mut Chain, name: Range<usize>, offset: usize) {
        let (prefix, local) = qualified_name_parts(self.source, name.clone());
        match prefix {
            Some(prefix) if offset <= prefix.end => chain.push(prefix),
            _ => chain.push(local),
        }
        chain.push(name);
    }

    /// Mot (ou cible d'instruction), contenu sans les blancs, contenu,
    /// construction complète.
    fn push_markup(&self, chain: &mut Chain, markup: &XmlMarkup, offset: usize) {
        let content = markup.content.clone();
        if markup.kind == XmlMarkupKind::ProcessingInstruction {
            let target_end = self.source[content.clone()]
                .find(char::is_whitespace)
                .map_or(content.end, |end| content.start + end);
            let target = content.start..target_end;
            if offset <= target.end {
                chain.push(target);
            }
        }
        if content.start <= offset && offset <= content.end {
            chain.push(word_at(self.source, &content, offset, is_word_char));
            chain.push(trim(self.source, content.clone()));
            chain.push(content);
        }
        chain.push(markup.range.clone());
    }

    /// Contenu puis élément de l'élément le plus profond dont le contenu
    /// contient `range`, puis ses ancêtres.
    fn push_enclosing(&self, chain: &mut Chain, range: Range<usize>) {
        let enclosing = self.tree.elements().iter().rposition(|element| {
            element
                .content_range()
                .is_some_and(|content| content.start <= range.start && range.end <= content.end)
        });
        if let Some(index) = enclosing {
            self.push_element(chain, index, true);
        }
    }

    fn push_element(&self, chain: &mut Chain, index: usize, inside_content: bool) {
        let elements = self.tree.elements();
        if inside_content {
            self.push_content(chain, index);
        }
        chain.push(elements[index].range());
        for ancestor in self.tree.ancestors(index) {
            self.push_content(chain, ancestor);
            chain.push(elements[ancestor].range());
        }
    }

    fn push_content(&self, chain: &mut Chain, index: usize) {
        if let Some(content) = self.tree.elements()[index].content_range() {
            chain.push(trim(self.source, content.clone()));
            chain.push(content);
        }
    }

    /// Nœud texte entre les deux constructions qui entourent `offset`.
    fn text_node_at(&self, offset: usize) -> Range<usize> {
        let start = self
            .constructs
            .iter()
            .map(|range| range.end)
            .filter(|&end| end <= offset)
            .max()
            .unwrap_or(0);
        let end = self
            .constructs
            .iter()
            .map(|range| range.start)
            .find(|&start| start >= offset)
            .unwrap_or(self.source.len());
        start..end
    }

    /// Attribut complet : du nom jusqu'après le guillemet fermant.
    fn attribute_range(&self, name: &Range<usize>, value: &Option<Range<usize>>) -> Range<usize> {
        match value {
            Some(value) => name.start..self.quoted_value(value).end,
            None => name.clone(),
        }
    }

    /// Valeur avec ses guillemets (le guillemet fermant peut manquer).
    fn quoted_value(&self, value: &Range<usize>) -> Range<usize> {
        let bytes = self.source.as_bytes();
        let Some(quote) = value
            .start
            .checked_sub(1)
            .map(|index| bytes[index])
            .filter(|&byte| matches!(byte, b'"' | b'\''))
        else {
            return value.clone();
        };
        let end = if bytes.get(value.end) == Some(&quote) {
            value.end + 1
        } else {
            value.end
        };
        value.start - 1..end
    }
}

/// Chaîne d'étendues imbriquées : une étendue n'est ajoutée que si elle est
/// non vide et contient strictement la précédente (ou, pour la première, la
/// position demandée).
struct Chain {
    offset: usize,
    ranges: Vec<Range<usize>>,
}

impl Chain {
    fn new(offset: usize) -> Self {
        Self {
            offset,
            ranges: Vec::new(),
        }
    }

    fn push(&mut self, range: Range<usize>) {
        if range.is_empty() {
            return;
        }
        let accepted = match self.ranges.last() {
            Some(last) => range.start <= last.start && last.end <= range.end && range != *last,
            None => range.start <= self.offset && self.offset <= range.end,
        };
        if accepted {
            self.ranges.push(range);
        }
    }
}

fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '-')
}

/// Plus longue suite de caractères `is_word` autour de `offset`, limitée à
/// `bounds` (vide si le curseur n'est pas au contact d'un mot).
fn word_at(
    source: &str,
    bounds: &Range<usize>,
    offset: usize,
    is_word: impl Fn(char) -> bool,
) -> Range<usize> {
    if offset < bounds.start || offset > bounds.end {
        return offset..offset;
    }
    let before = &source[bounds.start..offset];
    let after = &source[offset..bounds.end];
    let start = before
        .char_indices()
        .rev()
        .take_while(|&(_, character)| is_word(character))
        .last()
        .map_or(offset, |(index, _)| bounds.start + index);
    let end = offset
        + after
            .char_indices()
            .find(|&(_, character)| !is_word(character))
            .map_or(after.len(), |(index, _)| index);
    start..end
}

/// Retire les blancs en début et en fin d'étendue.
fn trim(source: &str, range: Range<usize>) -> Range<usize> {
    let text = &source[range.clone()];
    let start = range.start + (text.len() - text.trim_start().len());
    let end = range.end - (text.len() - text.trim_end().len());
    start..end.max(start)
}

fn floor_char_boundary(source: &str, mut offset: usize) -> usize {
    offset = offset.min(source.len());
    while !source.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// Conversion offset UTF-8 -> position LSP (ligne, unités UTF-16) en
/// temps logarithmique pour la ligne.
struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(source: &str) -> Self {
        let starts = std::iter::once(0)
            .chain(source.match_indices('\n').map(|(index, _)| index + 1))
            .collect();
        Self { starts }
    }

    fn position(&self, source: &str, offset: usize) -> Value {
        let offset = floor_char_boundary(source, offset);
        let line = self.starts.partition_point(|&start| start <= offset) - 1;
        let character: usize = source[self.starts[line]..offset]
            .chars()
            .map(char::len_utf16)
            .sum();
        json!({"line": line, "character": character})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Chaîne sous forme de sous-chaînes, de la plus petite à la plus grande.
    fn chain(source: &str, offset: usize) -> Vec<&str> {
        Document::new(source)
            .chain(offset)
            .into_iter()
            .map(|range| &source[range])
            .collect()
    }

    fn at(source: &str, needle: &str) -> usize {
        source.find(needle).expect("needle should exist")
    }

    fn assert_strictly_nested(source: &str) {
        let document = Document::new(source);
        for offset in (0..=source.len()).filter(|&offset| source.is_char_boundary(offset)) {
            let ranges = document.chain(offset);
            assert!(!ranges.is_empty() || source.is_empty(), "offset {offset}");
            if let Some(first) = ranges.first() {
                assert!(
                    first.start <= offset && offset <= first.end,
                    "offset {offset}"
                );
            }
            for pair in ranges.windows(2) {
                assert!(
                    pair[1].start <= pair[0].start
                        && pair[0].end <= pair[1].end
                        && pair[0] != pair[1],
                    "offset {offset}: {pair:?}"
                );
            }
        }
    }

    #[test]
    fn expands_from_a_prefixed_tag_name_to_the_document() {
        let source = "<root>\n  <ns:item a=\"1\">x</ns:item>\n</root>\n";
        assert_eq!(
            chain(source, at(source, "item a") + 2),
            vec![
                "item",
                "ns:item",
                "<ns:item a=\"1\">",
                "<ns:item a=\"1\">x</ns:item>",
                "\n  <ns:item a=\"1\">x</ns:item>\n",
                "<root>\n  <ns:item a=\"1\">x</ns:item>\n</root>",
                source,
            ]
        );
        assert_eq!(chain(source, at(source, "ns:item") + 1)[0], "ns");
        assert_eq!(
            chain(source, at(source, "</ns:item>") + 6)[..3],
            ["item", "ns:item", "</ns:item>"]
        );
    }

    #[test]
    fn expands_attribute_values_and_names() {
        let source = "<root xsi:schemaLocation=\"urn:a a.xsd\" id='v'/>";
        assert_eq!(
            chain(source, at(source, "a.xsd")),
            vec![
                "a",
                "a.xsd",
                "urn:a a.xsd",
                "\"urn:a a.xsd\"",
                "xsi:schemaLocation=\"urn:a a.xsd\"",
                "<root xsi:schemaLocation=\"urn:a a.xsd\" id='v'/>",
            ]
        );
        assert_eq!(
            chain(source, at(source, "schemaLocation") + 3)[..3],
            [
                "schemaLocation",
                "xsi:schemaLocation",
                "xsi:schemaLocation=\"urn:a a.xsd\""
            ]
        );
        assert_eq!(
            chain(source, at(source, "'v'") + 1)[..3],
            ["v", "'v'", "id='v'"]
        );
    }

    #[test]
    fn expands_text_content_through_parents() {
        let source = "<a>\n  <b>\n    hello world\n  </b>\n</a>";
        assert_eq!(
            chain(source, at(source, "world") + 2),
            vec![
                "world",
                "hello world",
                "\n    hello world\n  ",
                "<b>\n    hello world\n  </b>",
                "\n  <b>\n    hello world\n  </b>\n",
                source,
            ]
        );
    }

    #[test]
    fn handles_whitespace_only_content_and_empty_elements() {
        let source = "<a>\n  <b>   </b><c></c>\n</a>";
        assert_eq!(
            chain(source, at(source, "   </b>") + 1),
            vec![
                "   ",
                "<b>   </b>",
                "<b>   </b><c></c>",
                "\n  <b>   </b><c></c>\n",
                source
            ]
        );
        assert_eq!(
            chain(source, at(source, "</c>")),
            vec![
                "</c>",
                "<c></c>",
                "<b>   </b><c></c>",
                "\n  <b>   </b><c></c>\n",
                source
            ]
        );
    }

    #[test]
    fn expands_comments_cdata_and_processing_instructions() {
        let source = "<?xml version=\"1.0\"?>\n<r><!-- a note --><![CDATA[x < y]]><?pi data?></r>";
        assert_eq!(
            chain(source, at(source, "note")),
            vec![
                "note",
                "a note",
                " a note ",
                "<!-- a note -->",
                "<!-- a note --><![CDATA[x < y]]><?pi data?>",
                "<r><!-- a note --><![CDATA[x < y]]><?pi data?></r>",
                source,
            ]
        );
        assert_eq!(
            chain(source, at(source, "< y"))[..3],
            [
                "x < y",
                "<![CDATA[x < y]]>",
                "<!-- a note --><![CDATA[x < y]]><?pi data?>"
            ]
        );
        assert_eq!(
            chain(source, at(source, "pi data") + 1)[..3],
            ["pi", "pi data", "<?pi data?>"]
        );
        assert_eq!(
            chain(source, at(source, "version") + 2)[..4],
            [
                "version",
                "xml version=\"1.0\"",
                "<?xml version=\"1.0\"?>",
                source
            ]
        );
    }

    #[test]
    fn handles_self_closing_malformed_and_orphan_tags() {
        assert_strictly_nested("<a><b x=\"1/></a></z>");
        let source = "<a></z></a>";
        assert_strictly_nested(source);
        assert_eq!(
            chain(source, at(source, "</z>") + 2),
            vec!["z", "</z>", source]
        );

        let source = "<a><b/>text";
        assert_strictly_nested(source);
        assert_eq!(chain(source, at(source, "b/>")), vec!["b", "<b/>", source]);
        assert_eq!(chain(source, source.len()), vec!["text", source]);

        assert_strictly_nested("<a attr= ><b attr=\"x\"\n</a>");
        assert_strictly_nested("<!DOCTYPE r [<!-- c -->]><r>&amp;<![CDATA[");
        assert_strictly_nested("<a>\r\n  <b c='😀 é'>t😀t</b>\r\n</a>\r\n");
        assert!(Document::new("").chain(0).is_empty());
    }

    #[test]
    fn builds_nested_lsp_values_with_utf16_positions_and_crlf() {
        let source = "<a>\r\n  <😀b c=\"1\"/>\r\n</a>";
        let result = selection_ranges(source, &[at(source, "c=") + 1, 0, 999]);
        assert_eq!(result.len(), 3);
        let first = &result[0];
        assert_eq!(
            first["range"],
            json!({"start": {"line": 1, "character": 7}, "end": {"line": 1, "character": 8}})
        );
        assert_eq!(
            first["parent"]["range"],
            json!({"start": {"line": 1, "character": 7}, "end": {"line": 1, "character": 12}})
        );
        assert_eq!(
            first["parent"]["parent"]["range"],
            json!({"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 14}})
        );
        let mut depth = 0;
        let mut current = first;
        while let Some(parent) = current.get("parent") {
            current = parent;
            depth += 1;
        }
        assert_eq!(depth, 4);
        assert_eq!(
            current["range"],
            json!({"start": {"line": 0, "character": 0}, "end": {"line": 2, "character": 4}})
        );
        assert_eq!(
            result[2]["range"]["end"],
            json!({"line": 2, "character": 4})
        );
        assert!(result[2].get("parent").is_none());
        assert_eq!(
            selection_ranges("", &[0]),
            vec![json!({"range": {
                "start": {"line": 0, "character": 0},
                "end": {"line": 0, "character": 0},
            }})]
        );
    }
}
