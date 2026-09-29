//! Formatage XML : document complet ([`format_xml_with`]) et plage
//! ([`format_xml_range`]), paramétrés par [`FormatOptions`].

use std::ops::Range;

use quick_xml::{
    Reader, Writer,
    events::{BytesStart, BytesText, Event},
};

use crate::{
    MAX_XML_SOURCE_BYTES, parse_xml,
    tags::{XmlElement, XmlTagTree},
};

/// Fin de ligne utilisée pour les retours à la ligne insérés par le
/// formateur (le contenu texte existant n'est jamais converti).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LineEnding {
    /// `\n`.
    #[default]
    Lf,
    /// `\r\n`.
    CrLf,
}

impl LineEnding {
    /// Fin de ligne de la première ligne de `source` (`\n` par défaut).
    pub fn detect(source: &str) -> Self {
        match source.find('\n') {
            Some(index) if source[..index].ends_with('\r') => Self::CrLf,
            _ => Self::Lf,
        }
    }

    /// Représentation textuelle.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
        }
    }
}

/// Options de formatage.
///
/// Les valeurs par défaut reproduisent le formatage historique : deux espaces
/// par niveau, fins de ligne `\n`, exactement un saut de ligne final, aucune
/// ligne vide conservée entre les éléments et texte laissé intact.
///
/// Les cinq premiers champs correspondent aux `FormattingOptions` LSP ; les
/// suivants préparent les réglages de type LemMinX (`xml.format.*`), qui
/// pourront être complétés (largeur maximale, attributs sur plusieurs lignes,
/// éléments vides, etc.) sans modifier le comportement par défaut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatOptions {
    /// Largeur d'un niveau d'indentation lorsque `insert_spaces` est vrai.
    pub tab_size: usize,
    /// Indente avec des espaces (sinon une tabulation par niveau).
    pub insert_spaces: bool,
    /// Supprime les blancs en fin de ligne dans le texte et les commentaires
    /// (jamais dans les sections CDATA ni les valeurs d'attributs).
    pub trim_trailing_whitespace: bool,
    /// Garantit au moins un saut de ligne en fin de document.
    pub insert_final_newline: bool,
    /// Ne conserve qu'un seul saut de ligne en fin de document.
    pub trim_final_newlines: bool,
    /// Fin de ligne des retours à la ligne insérés.
    pub line_ending: LineEnding,
    /// Nombre maximal de lignes vides conservées entre deux constructions
    /// (`xml.format.preservedNewlines` de LemMinX ; 0 les supprime toutes).
    pub preserved_newlines: usize,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            tab_size: 2,
            insert_spaces: true,
            trim_trailing_whitespace: false,
            insert_final_newline: true,
            trim_final_newlines: true,
            line_ending: LineEnding::Lf,
            preserved_newlines: 0,
        }
    }
}

impl FormatOptions {
    /// Indentation d'un niveau.
    pub fn indent_unit(&self) -> String {
        if self.insert_spaces {
            " ".repeat(self.tab_size)
        } else {
            "\t".to_owned()
        }
    }

    fn indent(&self, depth: usize) -> String {
        self.indent_unit().repeat(depth)
    }
}

/// Formate un document XML valide avec deux espaces par niveau.
pub fn format_xml(source: &str) -> Result<String, String> {
    format_xml_with(source, &FormatOptions::default())
}

/// Formate un document XML valide selon `options`.
pub fn format_xml_with(source: &str, options: &FormatOptions) -> Result<String, String> {
    if !parse_xml(source).diagnostics.is_empty() {
        return Err("le document XML est invalide".to_owned());
    }

    let mut formatter = Formatter::new(options, 0, false, false);
    formatter.run(source)?;
    if !formatter.has_root {
        return Err("le document XML ne contient aucun élément racine".to_owned());
    }
    let (mut result, _) = formatter.finish()?;

    while result.ends_with('\n') {
        result.pop();
        if result.ends_with('\r') {
            result.pop();
        }
    }
    let mut final_newlines = source
        .chars()
        .rev()
        .take_while(|character| character.is_whitespace())
        .filter(|&character| character == '\n')
        .count();
    if options.trim_final_newlines {
        final_newlines = final_newlines.min(1);
    }
    if options.insert_final_newline {
        final_newlines = final_newlines.max(1);
    }
    for _ in 0..final_newlines {
        result.push_str(options.line_ending.as_str());
    }
    Ok(result)
}

/// Remplacement calculé par [`format_xml_range`] : `text` remplace
/// `source[range]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormattedRange {
    pub range: Range<usize>,
    pub text: String,
}

/// Formate la plage `range` (offsets UTF-8) de `source`, comme le
/// `rangeFormatting` de LemMinX.
///
/// La plage est étendue aux éléments complets qui l'englobent : la suite
/// d'éléments frères couvrant la plage, ou l'élément qui la contient
/// lorsqu'elle touche ses balises. Seule cette région (et les blancs qui
/// l'entourent sur la même ligne) est reformatée, avec l'indentation
/// correspondant à sa profondeur. Le reste du document peut être invalide :
/// seule la région doit être bien formée. Retourne `None` lorsque la région
/// ne peut pas être formatée sans risque.
pub fn format_xml_range(
    source: &str,
    range: Range<usize>,
    options: &FormatOptions,
) -> Option<FormattedRange> {
    if source.len() > MAX_XML_SOURCE_BYTES {
        return None;
    }
    let start = range.start.min(source.len());
    let end = range.end.clamp(start, source.len());
    let tree = XmlTagTree::parse(source);
    let region = Region::find(&tree, start, end)?;
    let elements = tree.elements();

    // Contexte : l'élément parent, son contenu et l'éventuel texte mixte
    // précédant la région (qui désactive l'indentation, comme pour le
    // formatage complet).
    let parent_content = match region.parent {
        Some(parent) => elements[parent].content_range()?,
        None => 0..source.len(),
    };
    let text_before = match region.parent {
        Some(_) => has_text_at_top_level(&source[parent_content.start..region.range.start])?,
        None => false,
    };

    let mut formatter = Formatter::new(options, region.depth, text_before, true);
    formatter.run(&source[region.range.clone()]).ok()?;
    let (body, text_after) = formatter.finish().ok()?;

    let newline = options.line_ending.as_str();
    let mut replaced = region.range.clone();
    let mut text = String::new();

    if !text_before {
        let before = &source[parent_content.start..region.range.start];
        let whitespace = before.len() - before.trim_end_matches(XML_WHITESPACE).len();
        replaced.start -= whitespace;
        if replaced.start > 0 {
            let line_breaks = source[replaced.start..region.range.start]
                .matches('\n')
                .count();
            let blank_lines = line_breaks
                .saturating_sub(1)
                .min(options.preserved_newlines);
            for _ in 0..=blank_lines {
                text.push_str(newline);
            }
        }
    }
    text.push_str(&body);

    if !text_after {
        let after = &source[region.range.end..];
        let whitespace = &after[..after.len() - after.trim_start_matches(XML_WHITESPACE).len()];
        let next = &after[whitespace.len()..];
        match whitespace.find(['\r', '\n']) {
            // Blancs en fin de ligne après la région.
            Some(line_break) => replaced.end += line_break,
            // Construction suivante sur la même ligne : passage à la ligne.
            None if next.starts_with('<') => {
                replaced.end += whitespace.len();
                let depth = if next.starts_with("</") {
                    region.depth.saturating_sub(1)
                } else {
                    region.depth
                };
                text.push_str(newline);
                text.push_str(&options.indent(depth));
            }
            None => {}
        }
    }

    // Garde-fou : le formatage ne doit modifier que des blancs.
    let significant = |value: &str| {
        value
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>()
    };
    if significant(&source[replaced.clone()]) != significant(&text) {
        return None;
    }

    Some(FormattedRange {
        range: replaced,
        text,
    })
}

const XML_WHITESPACE: [char; 4] = [' ', '\t', '\r', '\n'];

/// Région à reformater : suite d'éléments frères complets.
struct Region {
    range: Range<usize>,
    /// Parent complet commun (`None` au niveau du document).
    parent: Option<usize>,
    /// Profondeur d'indentation des éléments de la région.
    depth: usize,
}

impl Region {
    fn find(tree: &XmlTagTree, start: usize, end: usize) -> Option<Self> {
        let elements = tree.elements();
        // Seuls les éléments complets forment un arbre bien imbriqué.
        let closed_parent = (0..elements.len())
            .map(|index| {
                tree.ancestors(index)
                    .find(|&ancestor| elements[ancestor].is_closed())
            })
            .collect::<Vec<_>>();
        let deepest = |contains: &dyn Fn(Range<usize>) -> bool| {
            elements
                .iter()
                .enumerate()
                .filter(|(_, element)| element.is_closed() && contains(element.range()))
                .map(|(index, _)| index)
                .next_back()
        };
        let contains_start = |range: Range<usize>| range.start <= start && start < range.end;
        let contains_end = |range: Range<usize>| range.start < end && end <= range.end;

        let at_start = deepest(&contains_start);
        let at_end = if end > start {
            deepest(&contains_end)
        } else {
            at_start
        };
        let chain =
            |index: usize| std::iter::successors(Some(index), |&current| closed_parent[current]);
        let common = match (at_start, at_end) {
            (Some(first), Some(last)) => {
                chain(first).find(|&ancestor| chain(last).any(|other| other == ancestor))
            }
            _ => None,
        };

        let whole = |index: usize| {
            let element: &XmlElement = &elements[index];
            Region {
                range: element.range(),
                parent: closed_parent[index],
                depth: chain(index).count() - 1,
            }
        };

        if let Some(common) = common {
            let inside_content = elements[common]
                .content_range()
                .is_some_and(|content| content.start <= start && end <= content.end);
            if !inside_content {
                return Some(whole(common));
            }
        }

        let children = (0..elements.len())
            .filter(|&index| elements[index].is_closed() && closed_parent[index] == common)
            .collect::<Vec<_>>();
        let first = children
            .iter()
            .copied()
            .find(|&index| contains_start(elements[index].range()))
            .or_else(|| {
                children
                    .iter()
                    .copied()
                    .find(|&index| elements[index].range().start >= start)
            });
        let last = if end == start {
            first.filter(|&index| contains_start(elements[index].range()))
        } else {
            children
                .iter()
                .copied()
                .rfind(|&index| contains_end(elements[index].range()))
                .or_else(|| {
                    children
                        .iter()
                        .copied()
                        .rfind(|&index| elements[index].range().end <= end)
                })
        };

        match (first, last) {
            (Some(first), Some(last)) if first <= last => Some(Region {
                range: elements[first].range().start..elements[last].range().end,
                parent: common,
                depth: common.map_or(0, |common| chain(common).count()),
            }),
            _ => common.map(whole),
        }
    }
}

/// Indique si du texte (hors blancs) ou une section CDATA apparaît au premier
/// niveau de `content`, ce qui désactive l'indentation dans le formatage
/// complet. `None` si `content` n'est pas analysable.
fn has_text_at_top_level(content: &str) -> Option<bool> {
    let mut reader = Reader::from_str(content);
    let mut depth = 0usize;
    loop {
        match reader.read_event().ok()? {
            Event::Start(_) => depth += 1,
            Event::End(_) => depth = depth.checked_sub(1)?,
            Event::Text(text)
                if depth == 0 && !String::from_utf8_lossy(text.as_ref()).trim().is_empty() =>
            {
                return Some(true);
            }
            Event::CData(_) if depth == 0 => return Some(true),
            Event::Eof => return Some(false),
            _ => {}
        }
    }
}

/// Supprime les espaces et tabulations précédant chaque fin de ligne.
fn trim_trailing_whitespace(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| {
            let Some(body) = line.strip_suffix('\n') else {
                return line.to_owned();
            };
            let (body, line_ending) = match body.strip_suffix('\r') {
                Some(body) => (body, "\r\n"),
                None => (body, "\n"),
            };
            format!("{}{line_ending}", body.trim_end_matches([' ', '\t']))
        })
        .collect()
}

struct Formatter<'a> {
    options: &'a FormatOptions,
    writer: Writer<Vec<u8>>,
    /// Profondeur d'indentation du premier niveau.
    base_depth: usize,
    /// Profondeur relative au premier niveau.
    depth: usize,
    /// Présence de texte dans chaque niveau ouvert ; le premier élément
    /// représente le niveau englobant.
    stack: Vec<bool>,
    /// Fragment (formatage de plage) : plusieurs racines et texte permis au
    /// premier niveau.
    fragment: bool,
    output_started: bool,
    /// Balise ouvrante en attente et lignes vides qui la précèdent.
    pending_start: Option<(BytesStart<'static>, usize)>,
    blank_lines: usize,
    has_root: bool,
}

impl<'a> Formatter<'a> {
    fn new(
        options: &'a FormatOptions,
        base_depth: usize,
        outer_has_text: bool,
        fragment: bool,
    ) -> Self {
        Self {
            options,
            writer: Writer::new(Vec::new()),
            base_depth,
            depth: 0,
            stack: vec![outer_has_text],
            fragment,
            output_started: false,
            pending_start: None,
            blank_lines: 0,
            has_root: false,
        }
    }

    fn run(&mut self, source: &str) -> Result<(), String> {
        let mut reader = Reader::from_str(source);
        loop {
            let event = reader
                .read_event()
                .map_err(|error| format!("erreur XML : {error}"))?;
            match event {
                Event::Eof => break,
                Event::Decl(_) | Event::DocType(_) | Event::PI(_) => {
                    self.flush_pending_start()?;
                    self.write_indent()?;
                    self.emit(event.into_owned())?;
                }
                Event::Start(element) => {
                    self.flush_pending_start()?;
                    let blank_lines = std::mem::take(&mut self.blank_lines);
                    self.pending_start = Some((element.into_owned(), blank_lines));
                    if self.depth == 0 {
                        self.has_root = true;
                    }
                }
                Event::Empty(element) => {
                    self.flush_pending_start()?;
                    if !self.has_text() {
                        self.write_indent()?;
                    }
                    if self.depth == 0 {
                        self.has_root = true;
                    }
                    self.emit(Event::Empty(element.into_owned()))?;
                }
                Event::End(element) => {
                    self.flush_pending_start()?;
                    if self.depth == 0 {
                        return Err("balise fermante inattendue".to_owned());
                    }
                    self.depth -= 1;
                    let has_text = self.stack.pop().unwrap_or(false);
                    if !has_text {
                        self.write_indent()?;
                    }
                    self.emit(Event::End(element.into_owned()))?;
                }
                Event::Text(text) => {
                    let raw = String::from_utf8_lossy(text.as_ref()).into_owned();
                    if raw.trim().is_empty() {
                        let blank_lines = raw.matches('\n').count().saturating_sub(1);
                        self.blank_lines = blank_lines.min(self.options.preserved_newlines);
                        continue;
                    }
                    self.flush_pending_start()?;
                    self.mark_text();
                    if self.options.trim_trailing_whitespace {
                        let trimmed = trim_trailing_whitespace(&raw);
                        self.emit(Event::Text(BytesText::from_escaped(trimmed)))?;
                    } else {
                        self.emit(Event::Text(text.into_owned()))?;
                    }
                }
                Event::CData(data) => {
                    self.flush_pending_start()?;
                    self.mark_text();
                    self.emit(Event::CData(data.into_owned()))?;
                }
                Event::Comment(comment) => {
                    self.flush_pending_start()?;
                    if !self.has_text() {
                        self.write_indent()?;
                    }
                    if self.options.trim_trailing_whitespace {
                        let raw = String::from_utf8_lossy(comment.as_ref());
                        let trimmed = trim_trailing_whitespace(&raw);
                        self.emit(Event::Comment(BytesText::from_escaped(trimmed)))?;
                    } else {
                        self.emit(Event::Comment(comment.into_owned()))?;
                    }
                }
                Event::GeneralRef(reference) => {
                    self.flush_pending_start()?;
                    self.emit(Event::GeneralRef(reference.into_owned()))?;
                }
            }
        }
        if self.depth > 0 || self.pending_start.is_some() {
            return Err("balise non fermée".to_owned());
        }
        Ok(())
    }

    /// Texte formaté et présence de texte au premier niveau.
    fn finish(self) -> Result<(String, bool), String> {
        let has_text = self.stack.first().copied().unwrap_or(false);
        let output =
            String::from_utf8(self.writer.into_inner()).map_err(|error| error.to_string())?;
        Ok((output, has_text))
    }

    fn has_text(&self) -> bool {
        self.stack.last().copied().unwrap_or(false)
    }

    /// Le texte au premier niveau d'un document complet n'influence pas
    /// l'indentation (comportement historique).
    fn mark_text(&mut self) {
        if (self.depth > 0 || self.fragment)
            && let Some(has_text) = self.stack.last_mut()
        {
            *has_text = true;
        }
    }

    fn emit(&mut self, event: Event<'_>) -> Result<(), String> {
        self.writer
            .write_event(event)
            .map_err(|error| error.to_string())?;
        self.output_started = true;
        self.blank_lines = 0;
        Ok(())
    }

    fn flush_pending_start(&mut self) -> Result<(), String> {
        let Some((start, blank_lines)) = self.pending_start.take() else {
            return Ok(());
        };
        // Les lignes vides lues après la balise ouvrante concernent son
        // premier enfant.
        let blank_lines_after = std::mem::replace(&mut self.blank_lines, blank_lines);
        if !self.has_text() {
            self.write_indent()?;
        }
        self.stack.push(false);
        self.emit(Event::Start(start))?;
        self.depth += 1;
        self.blank_lines = blank_lines_after;
        Ok(())
    }

    fn write_indent(&mut self) -> Result<(), String> {
        let mut indent = String::new();
        if self.output_started {
            let newline = self.options.line_ending.as_str();
            for _ in 0..=self.blank_lines {
                indent.push_str(newline);
            }
        }
        self.blank_lines = 0;
        indent.push_str(&self.options.indent(self.base_depth + self.depth));
        if !indent.is_empty() {
            self.writer
                .write_event(Event::Text(BytesText::from_escaped(indent)))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(source: &str, formatted: &FormattedRange) -> String {
        let mut result = source.to_owned();
        result.replace_range(formatted.range.clone(), &formatted.text);
        result
    }

    fn format_range_of(source: &str, needle: &str, options: &FormatOptions) -> Option<String> {
        let start = source.find(needle).expect("needle should exist");
        format_xml_range(source, start..start + needle.len(), options)
            .map(|formatted| apply(source, &formatted))
    }

    #[test]
    fn default_options_match_historical_output() {
        let source = "<root><a x=\"1\"><b/></a><c>text</c><!-- n --></root>";
        assert_eq!(
            format_xml_with(source, &FormatOptions::default()).unwrap(),
            format_xml(source).unwrap()
        );
        assert_eq!(
            format_xml(source).unwrap(),
            "<root>\n  <a x=\"1\">\n    <b/>\n  </a>\n  <c>text</c>\n  <!-- n -->\n</root>\n"
        );
    }

    #[test]
    fn honours_tab_size_and_tabs() {
        let source = "<root><a><b/></a></root>";
        let four = FormatOptions {
            tab_size: 4,
            ..FormatOptions::default()
        };
        assert_eq!(
            format_xml_with(source, &four).unwrap(),
            "<root>\n    <a>\n        <b/>\n    </a>\n</root>\n"
        );
        let tabs = FormatOptions {
            insert_spaces: false,
            ..FormatOptions::default()
        };
        let formatted = format_xml_with(source, &tabs).unwrap();
        assert_eq!(formatted, "<root>\n\t<a>\n\t\t<b/>\n\t</a>\n</root>\n");
        assert_eq!(format_xml_with(&formatted, &tabs).unwrap(), formatted);
    }

    #[test]
    fn honours_final_newline_options() {
        let source = "<root/>\n\n\n";
        let keep_all = FormatOptions {
            trim_final_newlines: false,
            ..FormatOptions::default()
        };
        assert_eq!(format_xml_with(source, &keep_all).unwrap(), "<root/>\n\n\n");
        let no_insert = FormatOptions {
            insert_final_newline: false,
            ..FormatOptions::default()
        };
        assert_eq!(format_xml_with("<root/>", &no_insert).unwrap(), "<root/>");
        assert_eq!(format_xml_with(source, &no_insert).unwrap(), "<root/>\n");
        assert_eq!(format_xml(source).unwrap(), "<root/>\n");
    }

    #[test]
    fn trims_trailing_whitespace_outside_cdata() {
        let source = "<root><p>line  \n  next\t\n</p><!-- c  \n --><![CDATA[keep  \n]]></root>";
        let options = FormatOptions {
            trim_trailing_whitespace: true,
            ..FormatOptions::default()
        };
        let formatted = format_xml_with(source, &options).unwrap();
        assert_eq!(
            formatted,
            "<root>\n  <p>line\n  next\n</p>\n  <!-- c\n --><![CDATA[keep  \n]]></root>\n"
        );
        assert_eq!(format_xml_with(&formatted, &options).unwrap(), formatted);
        // Sans l'option, le texte reste intact.
        assert!(format_xml(source).unwrap().contains("line  \n  next\t\n"));
    }

    #[test]
    fn uses_crlf_line_endings() {
        let source = "<root>\r\n<a>x</a>\r\n<b/>\r\n</root>\r\n";
        let options = FormatOptions {
            line_ending: LineEnding::detect(source),
            ..FormatOptions::default()
        };
        assert_eq!(options.line_ending, LineEnding::CrLf);
        let formatted = format_xml_with(source, &options).unwrap();
        assert_eq!(formatted, "<root>\r\n  <a>x</a>\r\n  <b/>\r\n</root>\r\n");
        assert_eq!(format_xml_with(&formatted, &options).unwrap(), formatted);
        assert_eq!(LineEnding::detect("<a/>\n"), LineEnding::Lf);
        assert_eq!(LineEnding::detect("<a/>"), LineEnding::Lf);
    }

    #[test]
    fn preserves_blank_lines_up_to_the_limit() {
        let source = "<root>\n\n\n\n  <a/>\n  <b/>\n\n</root>";
        let options = FormatOptions {
            preserved_newlines: 2,
            ..FormatOptions::default()
        };
        let formatted = format_xml_with(source, &options).unwrap();
        assert_eq!(formatted, "<root>\n\n\n  <a/>\n  <b/>\n\n</root>\n");
        assert_eq!(format_xml_with(&formatted, &options).unwrap(), formatted);
        assert_eq!(
            format_xml(source).unwrap(),
            "<root>\n  <a/>\n  <b/>\n</root>\n"
        );
    }

    #[test]
    fn formatting_with_options_is_idempotent() {
        let documents = [
            "<root />",
            "<root><item>value</item><empty></empty></root>",
            "<root>\r\n<item id=\"1\">one &amp; two  \r\n</item><!-- note  \r\n--></root>\r\n\r\n",
            "<?xml version=\"1.0\"?><root>\n\n<item><![CDATA[a < b  \n]]></item>\n\n\n<b/></root>",
            "<p>Hello <b>big</b>\n\n <i>world</i></p>",
        ];
        for document in documents {
            for tab_size in [0, 2, 4] {
                for flags in 0..16u8 {
                    let options = FormatOptions {
                        tab_size,
                        insert_spaces: flags & 1 == 0,
                        trim_trailing_whitespace: flags & 2 != 0,
                        insert_final_newline: flags & 4 != 0,
                        trim_final_newlines: flags & 8 != 0,
                        line_ending: LineEnding::detect(document),
                        preserved_newlines: usize::from(flags % 3),
                    };
                    let formatted = format_xml_with(document, &options).unwrap();
                    assert_eq!(
                        format_xml_with(&formatted, &options).unwrap(),
                        formatted,
                        "{document:?} with {options:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn range_formatting_of_a_nested_element_uses_its_depth() {
        let source = "<root>\n  <outer>\n  <inner><a/><b>t</b></inner>\n  </outer>\n</root>\n";
        let result = format_range_of(source, "<a/>", &FormatOptions::default()).unwrap();
        assert_eq!(
            result,
            "<root>\n  <outer>\n  <inner>\n      <a/>\n      <b>t</b></inner>\n  </outer>\n</root>\n"
        );
        let result = format_range_of(source, "<inner><a/>", &FormatOptions::default()).unwrap();
        assert_eq!(
            result,
            "<root>\n  <outer>\n    <inner>\n      <a/>\n      <b>t</b>\n    </inner>\n  </outer>\n</root>\n"
        );
    }

    #[test]
    fn range_formatting_expands_partial_tags_to_sibling_elements() {
        let source = "<root><a><x/></a><b><y/></b><c/></root>";
        // De l'intérieur de <a> jusqu'au milieu de <b> : a et b sont formatés.
        let start = source.find("x/>").unwrap();
        let end = source.find("<y").unwrap() + 2;
        let formatted = format_xml_range(source, start..end, &FormatOptions::default()).unwrap();
        assert_eq!(
            apply(source, &formatted),
            "<root>\n  <a>\n    <x/>\n  </a>\n  <b>\n    <y/>\n  </b>\n  <c/></root>"
        );
        // Plage touchant une balise de l'élément : l'élément entier.
        let result = format_range_of(source, "<root><a>", &FormatOptions::default()).unwrap();
        assert_eq!(result, format_xml(source).unwrap().trim_end());
    }

    #[test]
    fn range_formatting_is_consistent_with_document_formatting() {
        let source = "<?xml version=\"1.0\"?>\n<root><a><b>x</b><c/></a><!-- n --><d>mixed <e>t</e></d></root>\n";
        let formatted = format_xml(source).unwrap();
        for options in [
            FormatOptions::default(),
            FormatOptions {
                insert_spaces: false,
                ..FormatOptions::default()
            },
        ] {
            let formatted = format_xml_with(source, &options).unwrap();
            for start in 0..formatted.len() {
                for end in start..formatted.len() {
                    if !formatted.is_char_boundary(start) || !formatted.is_char_boundary(end) {
                        continue;
                    }
                    if let Some(range) = format_xml_range(&formatted, start..end, &options) {
                        assert_eq!(
                            apply(&formatted, &range),
                            formatted,
                            "range {start}..{end} should be stable"
                        );
                    }
                }
            }
        }
        // Formater tout le document par plage donne le formatage complet.
        let whole = format_xml_range(source, 0..source.len(), &FormatOptions::default()).unwrap();
        assert_eq!(apply(source, &whole), formatted);
    }

    #[test]
    fn range_formatting_works_when_the_rest_of_the_document_is_malformed() {
        let source = "<root>\n<broken attr=\"x\">\n<item><a/><b/></item>\n<oops></root>";
        let result = format_range_of(source, "<a/>", &FormatOptions::default()).unwrap();
        assert_eq!(
            result,
            "<root>\n<broken attr=\"x\">\n<item>\n    <a/>\n    <b/></item>\n<oops></root>"
        );
        let result =
            format_range_of(source, "<item><a/><b/></item>", &FormatOptions::default()).unwrap();
        assert_eq!(
            result,
            "<root>\n<broken attr=\"x\">\n  <item>\n    <a/>\n    <b/>\n  </item>\n<oops></root>"
        );
    }

    #[test]
    fn range_formatting_refuses_unformattable_regions() {
        // Aucun élément complet.
        assert_eq!(
            format_xml_range("<root><a>", 0..9, &FormatOptions::default()),
            None
        );
        // Élément non fermé entre deux frères complets.
        let source = "<root><a/><open><b/></root>";
        assert_eq!(
            format_range_of(source, "<a/><open><b/>", &FormatOptions::default()),
            None
        );
        // Entité mal formée dans la région.
        assert_eq!(
            format_range_of("<root><a>&</a></root>", "<a>", &FormatOptions::default()),
            None
        );
    }

    #[test]
    fn range_formatting_respects_mixed_content() {
        let source = "<p>Hello <b>big</b> <i>world</i></p>";
        let result = format_range_of(source, "<i>world</i>", &FormatOptions::default()).unwrap();
        assert_eq!(result, source);
    }

    #[test]
    fn range_formatting_uses_tabs_crlf_and_blank_lines() {
        let source = "<root>\r\n\r\n\r\n<a><b/></a>   \r\n</root>\r\n";
        let options = FormatOptions {
            insert_spaces: false,
            line_ending: LineEnding::detect(source),
            preserved_newlines: 1,
            ..FormatOptions::default()
        };
        let result = format_range_of(source, "<b/></a>", &options).unwrap();
        assert_eq!(
            result,
            "<root>\r\n\r\n\t<a>\r\n\t\t<b/>\r\n\t</a>\r\n</root>\r\n"
        );
    }
}
