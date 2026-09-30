//! XML formatting: whole document ([`format_xml_with`]) and range
//! ([`format_xml_range`]), configured by [`FormatOptions`].

use std::ops::Range;

use quick_xml::{
    Reader, Writer,
    events::{BytesStart, BytesText, Event},
};

use crate::{
    MAX_XML_SOURCE_BYTES, parse_xml,
    tags::{XmlElement, XmlTagTree},
};

/// Line ending used for the line breaks inserted by the formatter
/// (existing text content is never converted).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LineEnding {
    /// `\n`.
    #[default]
    Lf,
    /// `\r\n`.
    CrLf,
}

impl LineEnding {
    /// Line ending of the first line of `source` (`\n` by default). Leading
    /// whitespace is ignored, and so are the line breaks inside comments,
    /// CDATA sections, processing instructions and the DOCTYPE: the
    /// formatter removes the former and keeps the latter as written, so
    /// none of them may decide the line ending of the result.
    pub fn detect(source: &str) -> Self {
        let source = source.trim_start();
        let markup = crate::tags::scan_markup(source);
        let mut next_markup = 0;
        for (index, _) in source.match_indices('\n') {
            while next_markup < markup.len() && markup[next_markup].range.end <= index {
                next_markup += 1;
            }
            if markup
                .get(next_markup)
                .is_some_and(|span| span.range.start <= index)
            {
                continue;
            }
            return if source[..index].ends_with('\r') {
                Self::CrLf
            } else {
                Self::Lf
            };
        }
        Self::Lf
    }

    /// Textual representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
        }
    }
}

/// Layout of the attributes of a start tag
/// (LemMinX `xml.format.splitAttributes`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SplitAttributes {
    /// Attributes stay on the tag line (or keep their line breaks, see
    /// [`FormatOptions::preserve_attribute_line_breaks`]).
    #[default]
    Preserve,
    /// Each attribute on its own line, indented one level deeper than the
    /// element (when the tag has at least two attributes).
    SplitNewLine,
    /// First attribute on the tag line, the following ones aligned with it
    /// (when the tag has at least two attributes).
    AlignWithFirstAttr,
}

/// Handling of empty elements (LemMinX `xml.format.emptyElements`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EmptyElements {
    /// `<a/>` and `<a></a>` are left as they are.
    #[default]
    Ignore,
    /// `<a/>` becomes `<a></a>`.
    Expand,
    /// `<a></a>` (or containing only whitespace) becomes `<a/>`.
    Collapse,
}

/// Quotes of attribute values (LemMinX `xml.format.enforceQuoteStyle` with
/// `xml.preferences.quoteStyle`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QuoteStyle {
    /// Quotes are kept as written.
    #[default]
    Preserve,
    /// Values are delimited by `"` (a `"` inside becomes `&quot;`).
    Double,
    /// Values are delimited by `'` (a `'` inside becomes `&apos;`).
    Single,
}

/// Formatting options.
///
/// The defaults reproduce the historical formatting: two spaces per level,
/// `\n` line endings, exactly one final newline, no blank line kept between
/// elements, text left intact and start tags copied as they are (attributes
/// included).
///
/// The first five fields match the LSP `FormattingOptions`; the following
/// ones are LemMinX-style settings (`xml.format.*`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatOptions {
    /// Width of an indentation level when `insert_spaces` is true.
    pub tab_size: usize,
    /// Indents with spaces (otherwise one tab per level).
    pub insert_spaces: bool,
    /// Removes trailing whitespace in text and comments (never in CDATA
    /// sections or attribute values).
    pub trim_trailing_whitespace: bool,
    /// Guarantees at least one newline at the end of the document.
    pub insert_final_newline: bool,
    /// Keeps only one newline at the end of the document.
    pub trim_final_newlines: bool,
    /// Line ending of the inserted line breaks.
    pub line_ending: LineEnding,
    /// Maximum number of blank lines kept between two constructs
    /// (LemMinX `xml.format.preservedNewlines`; 0 removes them all).
    pub preserved_newlines: usize,
    /// Attribute layout (`xml.format.splitAttributes`).
    pub split_attributes: SplitAttributes,
    /// Maximum width of a start tag line (`xml.format.maxLineWidth`):
    /// attributes that would exceed it move to the next line. 0 disables
    /// wrapping; only attribute placement is affected, never
    /// text.
    pub max_line_width: usize,
    /// Puts `>` or `/>` on its own line when the attributes are split over
    /// several lines by [`SplitAttributes::SplitNewLine`] or
    /// [`SplitAttributes::AlignWithFirstAttr`] (`xml.format.closingBracketNewLine`).
    pub closing_bracket_new_line: bool,
    /// Handling of empty elements (`xml.format.emptyElements`). Ignored by
    /// range formatting, which only changes whitespace.
    pub empty_elements: EmptyElements,
    /// Keeps existing line breaks before attributes
    /// (`xml.format.preserveAttributeLineBreaks`). With
    /// [`SplitAttributes::Preserve`], `true` and no `max_line_width`, the
    /// start tag is copied verbatim (historical behaviour);
    /// `false` puts all attributes on the tag line, separated by a
    /// space.
    pub preserve_attribute_line_breaks: bool,
    /// Writes `<a />` instead of `<a/>` (`xml.format.spaceBeforeEmptyCloseTag`).
    pub space_before_empty_close_tag: bool,
    /// Quote style of attribute values (`xml.format.enforceQuoteStyle`).
    pub quote_style: QuoteStyle,
    /// Keeps the whitespace of an element whose content is only whitespace
    /// (`<a>  </a>` stays on one line as written;
    /// `xml.format.preserveEmptyContent`). Ignored when `empty_elements`
    /// expands or collapses them.
    pub preserve_empty_content: bool,
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
            split_attributes: SplitAttributes::Preserve,
            max_line_width: 0,
            closing_bracket_new_line: false,
            empty_elements: EmptyElements::Ignore,
            preserve_attribute_line_breaks: true,
            space_before_empty_close_tag: false,
            quote_style: QuoteStyle::Preserve,
            preserve_empty_content: false,
        }
    }
}

impl FormatOptions {
    /// One level of indentation.
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

    /// Start tags are copied without being rebuilt (an empty-element tag
    /// with `space_before_empty_close_tag` is always rebuilt).
    fn keeps_raw_tags(&self, self_closing: bool) -> bool {
        self.split_attributes == SplitAttributes::Preserve
            && self.preserve_attribute_line_breaks
            && self.max_line_width == 0
            && self.quote_style == QuoteStyle::Preserve
            && !(self_closing && self.space_before_empty_close_tag)
    }

    /// Displayed width of an indentation (a tab counts as `tab_size`).
    fn display_width(&self, text: &str) -> usize {
        text.chars()
            .map(|character| {
                if character == '\t' {
                    self.tab_size.max(1)
                } else {
                    1
                }
            })
            .sum()
    }
}

/// Formats a valid XML document with two spaces per level.
pub fn format_xml(source: &str) -> Result<String, String> {
    format_xml_with(source, &FormatOptions::default())
}

/// Formats a valid XML document according to `options`.
pub fn format_xml_with(source: &str, options: &FormatOptions) -> Result<String, String> {
    if parse_xml(source)
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.blocks_formatting())
    {
        return Err("the XML document is invalid".to_owned());
    }

    let mut formatter = Formatter::new(options, 0, false, false, source.len());
    formatter.run(source)?;
    if !formatter.has_root {
        return Err("the XML document has no root element".to_owned());
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

/// Replacement computed by [`format_xml_range`]: `text` replaces
/// `source[range]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormattedRange {
    pub range: Range<usize>,
    pub text: String,
}

/// Formats the range `range` (UTF-8 offsets) of `source`, like LemMinX
/// `rangeFormatting`.
///
/// The range is expanded to the enclosing complete elements: the run of
/// sibling elements covering the range, or the element containing it when
/// it touches its tags. Only that region (and the whitespace around it on
/// the same line) is reformatted, with the indentation matching its
/// depth. The rest of the document may be invalid: only the region must
/// be well-formed. Returns `None` when the region cannot be formatted
/// safely.
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

    // Context: the parent element, its content and any mixed text
    // preceding the region (which disables indentation, as for whole
    // document formatting).
    let parent_content = match region.parent {
        Some(parent) => elements[parent].content_range()?,
        None => 0..source.len(),
    };
    let text_before = match region.parent {
        Some(_) => has_text_at_top_level(&source[parent_content.start..region.range.start])?,
        None => false,
    };

    // Range formatting only changes whitespace (see the safeguard
    // below): empty elements are left as they are.
    let options = &FormatOptions {
        empty_elements: EmptyElements::Ignore,
        ..options.clone()
    };
    let mut formatter = Formatter::new(options, region.depth, text_before, true, source.len());
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
            // Trailing whitespace after the region.
            Some(line_break) => replaced.end += line_break,
            // Next construct on the same line: line break.
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

    // Safeguard: formatting must only change whitespace.
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

/// Region to reformat: run of complete sibling elements.
struct Region {
    range: Range<usize>,
    /// Common complete parent (`None` at document level).
    parent: Option<usize>,
    /// Indentation depth of the elements of the region.
    depth: usize,
}

impl Region {
    fn find(tree: &XmlTagTree, start: usize, end: usize) -> Option<Self> {
        let elements = tree.elements();
        // Only complete elements form a well-nested tree. Parents precede
        // their children: linear even for deeply nested unclosed elements.
        let mut closed_parent: Vec<Option<usize>> = Vec::with_capacity(elements.len());
        for element in elements {
            let parent = element.parent.and_then(|parent| {
                if elements[parent].is_closed() {
                    Some(parent)
                } else {
                    closed_parent.get(parent).copied().flatten()
                }
            });
            closed_parent.push(parent);
        }
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
                let last_chain = chain(last).collect::<std::collections::HashSet<_>>();
                chain(first).find(|ancestor| last_chain.contains(ancestor))
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

/// Whether text (other than whitespace) or a CDATA section appears at the
/// top level of `content`, which disables indentation in whole document
/// formatting. `None` if `content` cannot be parsed.
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

/// Removes the spaces and tabs preceding each line ending.
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
    /// Indentation depth of the top level.
    base_depth: usize,
    /// Depth relative to the top level.
    depth: usize,
    /// Whether each open level contains text; the first item represents the
    /// enclosing level.
    stack: Vec<bool>,
    /// Fragment (range formatting): several roots and text allowed at the
    /// top level.
    fragment: bool,
    output_started: bool,
    /// Pending start tag and the blank lines preceding it.
    pending_start: Option<(BytesStart<'static>, usize)>,
    blank_lines: usize,
    has_root: bool,
    /// Whitespace-only text dropped right before the current event: it is
    /// character data after all when a CDATA section or a reference follows.
    skipped_text: Option<String>,
    /// Maximum size of the output (see [`max_formatted_size`]).
    max_output: usize,
}

/// Minimum of [`max_formatted_size`].
const MIN_MAX_FORMATTED_SIZE: usize = 64 * 1024 * 1024;

/// Maximum size of the formatted text of a `source_len`-byte input. The
/// indentation grows with the depth, so a small, deeply nested document
/// (or many siblings deep in one) would otherwise format into gigabytes:
/// formatting is refused beyond this size.
fn max_formatted_size(source_len: usize) -> usize {
    source_len.saturating_mul(8).max(MIN_MAX_FORMATTED_SIZE)
}

impl<'a> Formatter<'a> {
    fn new(
        options: &'a FormatOptions,
        base_depth: usize,
        outer_has_text: bool,
        fragment: bool,
        source_len: usize,
    ) -> Self {
        Self {
            max_output: max_formatted_size(source_len),
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
            skipped_text: None,
        }
    }

    fn run(&mut self, source: &str) -> Result<(), String> {
        // `quick-xml` cannot lex the quoted literals and comments of a DTD
        // (`<!ENTITY e '<a/>'>`): the declaration is emitted as one piece
        // and the markup around it is read normally.
        match crate::strict::doctype_range(source) {
            Some(range) => {
                self.run_part(&source[..range.start])?;
                let declaration = &source[range.clone()];
                let content = declaration
                    .strip_prefix("<!DOCTYPE")
                    .and_then(|rest| rest.strip_suffix('>'))
                    .map(str::trim_start)
                    .unwrap_or_default();
                self.flush_pending_start()?;
                self.write_indent()?;
                self.emit(Event::DocType(BytesText::from_escaped(content)))?;
                self.run_part(&source[range.end..])?;
            }
            None => self.run_part(source)?,
        }
        if self.depth > 0 || self.pending_start.is_some() {
            return Err("unclosed element".to_owned());
        }
        Ok(())
    }

    fn run_part(&mut self, source: &str) -> Result<(), String> {
        let mut reader = Reader::from_str(source);
        loop {
            let event = reader
                .read_event()
                .map_err(|error| format!("XML error: {error}"))?;
            let skipped = self.skipped_text.take();
            match event {
                Event::Eof => break,
                Event::Decl(_) | Event::DocType(_) | Event::PI(_) => {
                    self.flush_pending_start()?;
                    // In mixed content a processing instruction is inline
                    // (like a comment): adding a line break would add text.
                    if !(matches!(event, Event::PI(_)) && self.has_text()) {
                        self.write_indent()?;
                    }
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
                    if self.options.empty_elements == EmptyElements::Expand {
                        self.write_start_tag(element.clone(), false)?;
                        self.write_end_tag(&element)?;
                    } else {
                        self.write_start_tag(element, true)?;
                    }
                }
                Event::End(element) => {
                    // Whitespace-only content kept as written.
                    if self.options.preserve_empty_content
                        && self.options.empty_elements == EmptyElements::Ignore
                        && let Some(whitespace) = skipped.as_ref().filter(|text| !text.is_empty())
                        && let Some((start, blank_lines)) = self.pending_start.take()
                    {
                        self.blank_lines = blank_lines;
                        if !self.has_text() {
                            self.write_indent()?;
                        }
                        self.write_start_tag(start.clone(), false)?;
                        self.emit(Event::Text(BytesText::from_escaped(whitespace.as_str())))?;
                        self.write_end_tag(&start)?;
                        continue;
                    }
                    // Element without content (or only whitespace).
                    if self.options.empty_elements != EmptyElements::Ignore
                        && let Some((start, blank_lines)) = self.pending_start.take()
                    {
                        self.blank_lines = blank_lines;
                        if !self.has_text() {
                            self.write_indent()?;
                        }
                        if self.options.empty_elements == EmptyElements::Collapse {
                            self.write_start_tag(start, true)?;
                        } else {
                            self.write_start_tag(start.clone(), false)?;
                            self.write_end_tag(&start)?;
                        }
                        continue;
                    }
                    self.flush_pending_start()?;
                    if self.depth == 0 {
                        return Err("unexpected end tag".to_owned());
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
                    // Whitespace between the children of an element that has
                    // text (mixed content) is content: it is kept as written.
                    let significant = self.pending_start.is_none() && self.has_text();
                    if raw.trim().is_empty() && !significant {
                        let blank_lines = raw.matches('\n').count().saturating_sub(1);
                        self.blank_lines = blank_lines.min(self.options.preserved_newlines);
                        self.skipped_text = Some(raw);
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
                    self.emit_skipped(skipped)?;
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
                    self.mark_text();
                    self.emit_skipped(skipped)?;
                    self.emit(Event::GeneralRef(reference.into_owned()))?;
                }
            }
        }
        Ok(())
    }

    /// Formatted text and whether there is text at the top level.
    fn finish(self) -> Result<(String, bool), String> {
        let has_text = self.stack.first().copied().unwrap_or(false);
        let output =
            String::from_utf8(self.writer.into_inner()).map_err(|error| error.to_string())?;
        Ok((output, has_text))
    }

    fn has_text(&self) -> bool {
        self.stack.last().copied().unwrap_or(false)
    }

    /// Text at the top level of a whole document does not affect
    /// indentation (historical behaviour).
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

    fn emit_skipped(&mut self, skipped: Option<String>) -> Result<(), String> {
        match skipped {
            Some(text) => self.emit(Event::Text(BytesText::from_escaped(text))),
            None => Ok(()),
        }
    }

    fn flush_pending_start(&mut self) -> Result<(), String> {
        let Some((start, blank_lines)) = self.pending_start.take() else {
            return Ok(());
        };
        // Blank lines read after the start tag belong to its first
        // child.
        let blank_lines_after = std::mem::replace(&mut self.blank_lines, blank_lines);
        if !self.has_text() {
            self.write_indent()?;
        }
        self.stack.push(false);
        self.write_start_tag(start, false)?;
        self.depth += 1;
        self.blank_lines = blank_lines_after;
        Ok(())
    }

    /// Writes a start (or empty) tag, copied or rebuilt according to the
    /// attribute layout options.
    fn write_start_tag(&mut self, start: BytesStart<'_>, self_closing: bool) -> Result<(), String> {
        let rebuilt = if self.options.keeps_raw_tags(self_closing) {
            None
        } else {
            let source = std::str::from_utf8(&start).map_err(|error| error.to_string())?;
            let name_length = start.name().as_ref().len();
            layout_start_tag(
                self.options,
                self.base_depth + self.depth,
                source,
                name_length,
                self_closing,
            )
        };
        match rebuilt {
            Some(tag) => self.emit(Event::Text(BytesText::from_escaped(tag))),
            None if self_closing => self.emit(Event::Empty(start.into_owned())),
            None => self.emit(Event::Start(start.into_owned())),
        }
    }

    /// Writes the end tag matching `start` on the same line.
    fn write_end_tag(&mut self, start: &BytesStart<'_>) -> Result<(), String> {
        let end = start.to_end().into_owned();
        self.emit(Event::End(end))
    }

    fn write_indent(&mut self) -> Result<(), String> {
        if self.writer.get_ref().len() > self.max_output {
            return Err("the formatted document would be too large".to_owned());
        }
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

/// Attribute of a start tag, normalized (`name="value"` without whitespace
/// around `=`, original quotes kept).
struct TagAttribute<'a> {
    name: &'a str,
    quote: char,
    value: &'a str,
    /// The attribute is preceded by a line break in the source.
    after_line_break: bool,
}

impl TagAttribute<'_> {
    fn width(&self) -> usize {
        self.name.chars().count() + self.value.chars().count() + 3
    }

    fn push_to(&self, output: &mut String, style: QuoteStyle) {
        let quote = match style {
            QuoteStyle::Preserve => self.quote,
            QuoteStyle::Double => '"',
            QuoteStyle::Single => '\'',
        };
        output.push_str(self.name);
        output.push('=');
        output.push(quote);
        if quote == self.quote {
            output.push_str(self.value);
        } else {
            // The other quote character may be used freely in the value;
            // `quote` itself must be escaped.
            let entity = if quote == '"' { "&quot;" } else { "&apos;" };
            output.push_str(&self.value.replace(quote, entity));
        }
        output.push(quote);
    }
}

/// Splits the attributes of `source` (content of a start tag, without
/// `<` or `>`/`/>`) after the name, or `None` if the tag cannot be parsed.
fn tag_attributes(source: &str, name_length: usize) -> Option<Vec<TagAttribute<'_>>> {
    let mut attributes = Vec::new();
    let mut rest = source.get(name_length..)?;
    loop {
        let trimmed = rest.trim_start_matches(XML_WHITESPACE);
        let after_line_break = rest[..rest.len() - trimmed.len()].contains('\n');
        if trimmed.is_empty() {
            return Some(attributes);
        }
        if trimmed.len() == rest.len() {
            // The name and each attribute must be followed by whitespace.
            return None;
        }
        let name_end = trimmed
            .find(|character: char| character == '=' || XML_WHITESPACE.contains(&character))?;
        let name = &trimmed[..name_end];
        if name.is_empty() {
            return None;
        }
        let after_name = trimmed[name_end..].trim_start_matches(XML_WHITESPACE);
        let after_equals = after_name
            .strip_prefix('=')?
            .trim_start_matches(XML_WHITESPACE);
        let quote = after_equals
            .chars()
            .next()
            .filter(|character| *character == '"' || *character == '\'')?;
        let value_source = &after_equals[1..];
        let value_end = value_source.find(quote)?;
        attributes.push(TagAttribute {
            name,
            quote,
            value: &value_source[..value_end],
            after_line_break,
        });
        rest = &value_source[value_end + 1..];
    }
}

/// Rebuilds a start tag according to `splitAttributes`,
/// `preserveAttributeLineBreaks`, `maxLineWidth` and `closingBracketNewLine`.
/// `depth` is the indentation depth of the element.
fn layout_start_tag(
    options: &FormatOptions,
    depth: usize,
    source: &str,
    name_length: usize,
    self_closing: bool,
) -> Option<String> {
    let attributes = tag_attributes(source, name_length)?;
    let name = &source[..name_length];
    let newline = options.line_ending.as_str();
    let element_indent = options.indent(depth);
    let split = attributes.len() > 1;
    let continuation_indent = match options.split_attributes {
        SplitAttributes::AlignWithFirstAttr if split => {
            format!("{element_indent}{}", " ".repeat(name.chars().count() + 2))
        }
        _ => options.indent(depth + 1),
    };
    let continuation_width = options.display_width(&continuation_indent);

    let mut tag = format!("<{name}");
    let mut width = options.display_width(&element_indent) + 1 + name.chars().count();
    let mut multiline = false;
    for (index, attribute) in attributes.iter().enumerate() {
        let mut line_break = match options.split_attributes {
            SplitAttributes::SplitNewLine => split,
            SplitAttributes::AlignWithFirstAttr => split && index > 0,
            SplitAttributes::Preserve => {
                options.preserve_attribute_line_breaks && attribute.after_line_break
            }
        };
        if !line_break
            && options.max_line_width > 0
            && width + 1 + attribute.width() > options.max_line_width
        {
            line_break = true;
        }
        if line_break {
            tag.push_str(newline);
            tag.push_str(&continuation_indent);
            width = continuation_width;
            multiline = true;
        } else {
            tag.push(' ');
            width += 1;
        }
        attribute.push_to(&mut tag, options.quote_style);
        width += attribute.width();
    }
    let bracket_on_own_line = multiline
        && options.closing_bracket_new_line
        && options.split_attributes != SplitAttributes::Preserve;
    if bracket_on_own_line {
        tag.push_str(newline);
        tag.push_str(&element_indent);
    }
    if self_closing && options.space_before_empty_close_tag && !bracket_on_own_line {
        tag.push(' ');
    }
    tag.push_str(if self_closing { "/>" } else { ">" });
    Some(tag)
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
        // Without the option, the text stays intact.
        assert!(format_xml(source).unwrap().contains("line  \n  next\t\n"));
    }

    #[test]
    fn keeps_mixed_content_stable_and_idempotent() {
        let format = |source: &str| {
            let options = FormatOptions {
                line_ending: LineEnding::detect(source),
                ..FormatOptions::default()
            };
            format_xml_with(source, &options).unwrap()
        };
        for source in [
            // A processing instruction after text is inline.
            "<root>\n  x  <?t v?>\n  <item/>\n</root>\n",
            // A reference is text.
            "<p>&b;</p>\n",
            // Whitespace before CDATA is character data.
            "<p>\n\n    <![CDATA[x]]>\n</p>\n",
            // Quoted literals of the DOCTYPE may contain markup.
            "<!DOCTYPE t [\n    <!ENTITY b 'a/>'>\n]>\n<p>&b;</p>\n",
            // Line breaks of the DOCTYPE or of CDATA do not decide the line ending.
            "<!DOCTYPE\r\n t [\n<!ENTITY b 'x'>\n]>\n<p/>\n",
            "<doc>\n<![CDATA[a\r\nb]]>\n</doc>\n",
        ] {
            let once = format(source);
            assert_eq!(format(&once), once, "{source:?}");
        }
        assert_eq!(format("<p>&b;</p>\n"), "<p>&b;</p>\n");
        assert_eq!(
            format("<p>\n  <![CDATA[x]]></p>\n"),
            "<p>\n  <![CDATA[x]]></p>\n"
        );
    }

    #[test]
    fn writes_a_space_before_the_empty_close_tag_and_enforces_quotes() {
        let source = "<r a='1' b=\"x'y\"><e/><f c='&quot;q'></f><g/></r>\n";
        let space = FormatOptions {
            space_before_empty_close_tag: true,
            ..FormatOptions::default()
        };
        assert_eq!(
            format_xml_with(source, &space).unwrap(),
            "<r a='1' b=\"x'y\">\n  <e />\n  <f c='&quot;q'>\n  </f>\n  <g />\n</r>\n"
        );
        let collapse = FormatOptions {
            empty_elements: EmptyElements::Collapse,
            ..space.clone()
        };
        assert!(
            format_xml_with(source, &collapse)
                .unwrap()
                .contains("<f c='&quot;q' />")
        );
        let double = FormatOptions {
            quote_style: QuoteStyle::Double,
            ..FormatOptions::default()
        };
        assert_eq!(
            format_xml_with(source, &double).unwrap(),
            "<r a=\"1\" b=\"x'y\">\n  <e/>\n  <f c=\"&quot;q\">\n  </f>\n  <g/>\n</r>\n"
        );
        let single = FormatOptions {
            quote_style: QuoteStyle::Single,
            ..FormatOptions::default()
        };
        let formatted = format_xml_with(source, &single).unwrap();
        assert!(formatted.contains("<r a='1' b='x&apos;y'>"), "{formatted}");
        // Idempotent.
        assert_eq!(format_xml_with(&formatted, &single).unwrap(), formatted);
        let preserve = FormatOptions {
            preserve_empty_content: true,
            ..FormatOptions::default()
        };
        assert_eq!(
            format_xml_with("<r><a>  </a><b>\n</b><c></c></r>\n", &preserve).unwrap(),
            "<r>\n  <a>  </a>\n  <b>\n</b>\n  <c>\n  </c>\n</r>\n"
        );
        assert_eq!(
            format_xml_with("<r><a> </a></r>\n", &FormatOptions::default()).unwrap(),
            "<r>\n  <a>\n  </a>\n</r>\n"
        );
        // The closing bracket on its own line takes no space.
        let own_line = FormatOptions {
            split_attributes: SplitAttributes::SplitNewLine,
            closing_bracket_new_line: true,
            space_before_empty_close_tag: true,
            ..FormatOptions::default()
        };
        assert_eq!(
            format_xml_with("<a x=\"1\" y=\"2\"/>", &own_line).unwrap(),
            "<a\n  x=\"1\"\n  y=\"2\"\n/>\n"
        );
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
        assert_eq!(LineEnding::detect("\r\n<a>\n</a>\r\n"), LineEnding::Lf);
        assert_eq!(
            LineEnding::detect("<!DOCTYPE a [\n<!ENTITY e 'x'>\n]>\r\n<a/>"),
            LineEnding::CrLf
        );
        assert_eq!(
            LineEnding::detect("<a><![CDATA[\r\n]]>\n<!--\r\n--></a>"),
            LineEnding::Lf
        );
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
                        ..FormatOptions::default()
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
        // From inside <a> to the middle of <b>: a and b are formatted.
        let start = source.find("x/>").unwrap();
        let end = source.find("<y").unwrap() + 2;
        let formatted = format_xml_range(source, start..end, &FormatOptions::default()).unwrap();
        assert_eq!(
            apply(source, &formatted),
            "<root>\n  <a>\n    <x/>\n  </a>\n  <b>\n    <y/>\n  </b>\n  <c/></root>"
        );
        // Range touching a tag of the element: the whole element.
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
        // Formatting the whole document by range gives the whole formatting.
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
        // No complete element.
        assert_eq!(
            format_xml_range("<root><a>", 0..9, &FormatOptions::default()),
            None
        );
        // Unclosed element between two complete siblings.
        let source = "<root><a/><open><b/></root>";
        assert_eq!(
            format_range_of(source, "<a/><open><b/>", &FormatOptions::default()),
            None
        );
        // Malformed entity in the region.
        assert_eq!(
            format_range_of("<root><a>&</a></root>", "<a>", &FormatOptions::default()),
            None
        );
    }

    #[test]
    fn keeps_the_whitespace_between_inline_elements_of_mixed_content() {
        let source = "<p>for example <code>jar</code> <code>war</code>\n  <code>ear</code>.</p>\n";
        assert_eq!(
            format_xml_with(source, &FormatOptions::default()).unwrap(),
            source
        );
        let nested = "<root>\n  <p>a <b>x</b> <i>y</i> b</p>\n</root>\n";
        assert_eq!(
            format_xml_with(nested, &FormatOptions::default()).unwrap(),
            nested
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

    fn assert_stable(source: &str, options: &FormatOptions) -> String {
        let formatted = format_xml_with(source, options).unwrap();
        assert_eq!(
            format_xml_with(&formatted, options).unwrap(),
            formatted,
            "{source:?} with {options:?}"
        );
        formatted
    }

    #[test]
    fn default_options_keep_start_tags_verbatim() {
        let source = "<root  a='1'\n      b = \"2\" ><c x=\"1\"   y=\"2\" /></root>";
        assert_eq!(
            format_xml(source).unwrap(),
            "<root  a='1'\n      b = \"2\" >\n  <c x=\"1\"   y=\"2\" />\n</root>\n"
        );
    }

    #[test]
    fn splits_attributes_on_new_lines() {
        let source = "<root><item id=\"1\" name='x &amp; y' kind=\"a\"/><one only=\"1\"/></root>";
        let options = FormatOptions {
            split_attributes: SplitAttributes::SplitNewLine,
            ..FormatOptions::default()
        };
        assert_eq!(
            assert_stable(source, &options),
            "<root>\n  <item\n    id=\"1\"\n    name='x &amp; y'\n    kind=\"a\"/>\n  <one only=\"1\"/>\n</root>\n"
        );
        let options = FormatOptions {
            closing_bracket_new_line: true,
            ..options
        };
        assert_eq!(
            assert_stable(source, &options),
            "<root>\n  <item\n    id=\"1\"\n    name='x &amp; y'\n    kind=\"a\"\n  />\n  <one only=\"1\"/>\n</root>\n"
        );
    }

    #[test]
    fn aligns_attributes_with_the_first_one() {
        let source =
            "<root><ns:item id=\"1\"\n name=\"x\"><b c=\"1\" d=\"2\"></b></ns:item></root>";
        let options = FormatOptions {
            split_attributes: SplitAttributes::AlignWithFirstAttr,
            insert_spaces: false,
            ..FormatOptions::default()
        };
        assert_eq!(
            assert_stable(source, &options),
            "<root>\n\t<ns:item id=\"1\"\n\t         name=\"x\">\n\t\t<b c=\"1\"\n\t\t   d=\"2\">\n\t\t</b>\n\t</ns:item>\n</root>\n"
        );
    }

    #[test]
    fn joins_or_preserves_attribute_line_breaks() {
        let source = "<root a=\"1\"\n    b=\"2\"   c=\"3\"/>";
        let joined = FormatOptions {
            preserve_attribute_line_breaks: false,
            ..FormatOptions::default()
        };
        assert_eq!(
            assert_stable(source, &joined),
            "<root a=\"1\" b=\"2\" c=\"3\"/>\n"
        );
        // With a maximum width, existing line breaks are kept and spaces
        // normalized.
        let preserved = FormatOptions {
            max_line_width: 200,
            ..FormatOptions::default()
        };
        assert_eq!(
            assert_stable(source, &preserved),
            "<root a=\"1\"\n  b=\"2\" c=\"3\"/>\n"
        );
    }

    #[test]
    fn wraps_attributes_beyond_the_maximum_line_width() {
        let source = "<root><item first=\"aaaa\" second=\"bbbb\" third=\"cccc\" fourth=\"dddd\">t</item></root>";
        for preserve in [true, false] {
            let options = FormatOptions {
                max_line_width: 30,
                preserve_attribute_line_breaks: preserve,
                ..FormatOptions::default()
            };
            assert_eq!(
                assert_stable(source, &options),
                "<root>\n  <item first=\"aaaa\"\n    second=\"bbbb\" third=\"cccc\"\n    fourth=\"dddd\">t</item>\n</root>\n"
            );
        }
        // Text is never wrapped.
        let options = FormatOptions {
            max_line_width: 5,
            ..FormatOptions::default()
        };
        assert_eq!(
            assert_stable("<p>a long text line</p>", &options),
            "<p>a long text line</p>\n"
        );
    }

    #[test]
    fn expands_and_collapses_empty_elements() {
        let source = "<root><a/><b x=\"1\"></b><c>\n\n</c><d> t </d><e><!-- c --></e></root>";
        let expand = FormatOptions {
            empty_elements: EmptyElements::Expand,
            ..FormatOptions::default()
        };
        assert_eq!(
            assert_stable(source, &expand),
            "<root>\n  <a></a>\n  <b x=\"1\"></b>\n  <c></c>\n  <d> t </d>\n  <e>\n    <!-- c -->\n  </e>\n</root>\n"
        );
        let collapse = FormatOptions {
            empty_elements: EmptyElements::Collapse,
            ..FormatOptions::default()
        };
        assert_eq!(
            assert_stable(source, &collapse),
            "<root>\n  <a/>\n  <b x=\"1\"/>\n  <c/>\n  <d> t </d>\n  <e>\n    <!-- c -->\n  </e>\n</root>\n"
        );
        assert_eq!(assert_stable("<r></r>", &collapse), "<r/>\n");
        // Range formatting only touches whitespace.
        let range = format_range_of(source, "<a/>", &expand).unwrap();
        assert!(range.contains("<a/>"));
    }

    #[test]
    fn attribute_layouts_are_idempotent_and_whitespace_only() {
        let documents = [
            "<root xmlns:x=\"urn:x\"><x:a p=\"1\" q='2' r=\"&lt;\"/><b\n  s=\"é\"\tt=\"😀\">text <i k=\"v\" l=\"w\">it</i></b></root>",
            "<?xml version=\"1.0\"?>\r\n<root a=\"1\" b=\"2\">\r\n<c d = \"3\" e= '4'/>\r\n</root>\r\n",
        ];
        let significant = |value: &str| {
            value
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect::<String>()
        };
        for document in documents {
            for split in [
                SplitAttributes::Preserve,
                SplitAttributes::SplitNewLine,
                SplitAttributes::AlignWithFirstAttr,
            ] {
                for flags in 0..8u8 {
                    let options = FormatOptions {
                        split_attributes: split,
                        closing_bracket_new_line: flags & 1 != 0,
                        preserve_attribute_line_breaks: flags & 2 != 0,
                        max_line_width: if flags & 4 != 0 { 20 } else { 0 },
                        insert_spaces: flags & 2 == 0,
                        line_ending: LineEnding::detect(document),
                        ..FormatOptions::default()
                    };
                    let formatted = assert_stable(document, &options);
                    assert_eq!(significant(&formatted), significant(document));
                }
            }
        }
    }
}
