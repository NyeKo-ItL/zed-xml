//! Parsing of DTD declarations and of the `<!DOCTYPE>` declaration.
//!
//! Parsing is tolerant: each invalid declaration produces a located
//! [`DtdProblem`] and parsing resumes at the next declaration.
//! Parameter entity references are expanded between declarations (the
//! replacement text becomes a [`SourceKind::Replacement`] or
//! [`SourceKind::External`] source) and inside declarations (outside
//! literals, with a space on each side as required by XML 1.0 §4.4.8); the
//! ranges of an expanded text are mapped back to the `%name;`
//! reference.

use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

use xml_core::tags::{XmlMarkupKind, scan_markup, scan_tags};

use crate::{
    AttributeDecl, AttributeType, ContentParticle, ContentSpec, DefaultDecl, Dtd, DtdProblem,
    DtdProblemKind, DtdSource, ElementDecl, EntityDecl, EntityExpansion, EntityValue,
    ExpansionError, Location, MAX_DOCUMENT_EXPANSION, MAX_ENTITY_DEPTH, MAX_ENTITY_EXPANSION,
    MAX_PARAMETER_EXPANSION, MAX_SOURCES, NotationDecl, Occurrence, PREDEFINED_ENTITIES, SourceId,
    SourceKind,
    content::ParticleKind,
    names::{is_name, scan_name_chars},
};

/// Maximum depth of nested groups in a content model.
const MAX_GROUP_DEPTH: usize = 64;

/// Failure to read an external resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    pub message: String,
    /// Remote resource (`http(s)`), never downloaded.
    pub remote: bool,
}

/// Reading of external resources (external subset, external parameter
/// entities); `base` is the path of the declaring source.
pub trait ExternalLoader {
    fn load(
        &mut self,
        public: Option<&str>,
        system: &str,
        base: Option<&Path>,
    ) -> Result<(PathBuf, String), LoadError>;
}

/// Loader refusing every external resource.
pub struct NoLoader;

impl ExternalLoader for NoLoader {
    fn load(
        &mut self,
        _public: Option<&str>,
        system: &str,
        _base: Option<&Path>,
    ) -> Result<(PathBuf, String), LoadError> {
        Err(LoadError {
            message: format!("external resource '{system}' not loaded"),
            remote: false,
        })
    }
}

/// `<!DOCTYPE>` declaration of a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doctype {
    /// Whole declaration.
    pub range: Range<usize>,
    pub name: String,
    pub name_range: Range<usize>,
    /// Public identifier and its range (quotes excluded).
    pub public_id: Option<(String, Range<usize>)>,
    /// System identifier and its range (quotes excluded).
    pub system_id: Option<(String, Range<usize>)>,
    /// Content of the internal subset, brackets excluded.
    pub internal_subset: Option<Range<usize>>,
}

/// `<!DOCTYPE>` declaration preceding the root element.
pub fn find_doctype(source: &str) -> Option<Doctype> {
    let first_tag = scan_tags(source)
        .first()
        .map_or(source.len(), |tag| tag.range.start);
    let markup = scan_markup(source)
        .into_iter()
        .take_while(|markup| markup.range.start < first_tag)
        .find(|markup| {
            markup.kind == XmlMarkupKind::Declaration
                && source[markup.content.clone()].starts_with("DOCTYPE")
        })?;
    parse_doctype(source, markup.range, markup.content)
}

fn parse_doctype(source: &str, range: Range<usize>, content: Range<usize>) -> Option<Doctype> {
    let bytes = source.as_bytes();
    let end = content.end;
    let start = skip_spaces(bytes, content.start + "DOCTYPE".len(), end);
    let name_range = start..scan_name_chars(source, start, end);
    let mut index = skip_spaces(bytes, name_range.end, end);
    let keyword_end = scan_name_chars(source, index, end);
    let mut public_id = None;
    let mut system_id = None;
    match &source[index..keyword_end] {
        "SYSTEM" => {
            index = keyword_end;
            if let Some((value, range, next)) = quoted(source, index, end) {
                system_id = Some((value, range));
                index = next;
            }
        }
        "PUBLIC" => {
            index = keyword_end;
            if let Some((value, range, next)) = quoted(source, index, end) {
                public_id = Some((value, range));
                index = next;
                if let Some((value, range, next)) = quoted(source, index, end) {
                    system_id = Some((value, range));
                    index = next;
                }
            }
        }
        _ => {}
    }
    index = skip_spaces(bytes, index, end);
    let internal_subset = (index < end && bytes[index] == b'[').then(|| {
        let start = index + 1;
        start..subset_end(bytes, start, end)
    });
    Some(Doctype {
        range,
        name: source[name_range.clone()].to_owned(),
        name_range,
        public_id,
        system_id,
        internal_subset,
    })
}

fn skip_spaces(bytes: &[u8], mut index: usize, end: usize) -> usize {
    while index < end && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    index
}

/// Quoted literal after whitespace: `(value, range, rest)`.
fn quoted(source: &str, from: usize, end: usize) -> Option<(String, Range<usize>, usize)> {
    let bytes = source.as_bytes();
    let index = skip_spaces(bytes, from, end);
    let quote = *bytes.get(index).filter(|_| index < end)?;
    if !matches!(quote, b'"' | b'\'') {
        return None;
    }
    let close = index
        + 1
        + bytes[index + 1..end]
            .iter()
            .position(|&byte| byte == quote)?;
    Some((
        source[index + 1..close].to_owned(),
        index + 1..close,
        close + 1,
    ))
}

/// Position of the `]` closing the internal subset (outside literals,
/// comments and processing instructions), or `end`.
fn subset_end(bytes: &[u8], mut index: usize, end: usize) -> usize {
    while index < end {
        match bytes[index] {
            b'"' | b'\'' => {
                let quote = bytes[index];
                index = bytes[index + 1..end]
                    .iter()
                    .position(|&byte| byte == quote)
                    .map_or(end, |offset| index + offset + 2);
            }
            b'<' if bytes[index..end].starts_with(b"<!--") => {
                index = find_after(bytes, index + 4, end, b"-->");
            }
            b'<' if bytes[index..end].starts_with(b"<?") => {
                index = find_after(bytes, index + 2, end, b"?>");
            }
            // Conditional section (not allowed here, but its brackets do not
            // close the subset).
            b'<' if bytes[index..end].starts_with(b"<![") => {
                index = section_end(bytes, index + 3, end).map_or(end, |close| close + 3);
            }
            b']' => return index,
            _ => index += 1,
        }
    }
    end
}

fn find_after(bytes: &[u8], from: usize, end: usize, needle: &[u8]) -> usize {
    bytes[from.min(end)..end]
        .windows(needle.len())
        .position(|window| window == needle)
        .map_or(end, |offset| from + offset + needle.len())
}

/// Parses a whole DTD file (external subset).
pub fn parse_dtd(text: &str, path: Option<PathBuf>, loader: &mut dyn ExternalLoader) -> Dtd {
    let mut builder = DtdBuilder::new(loader);
    let source = builder.add_document(text, path);
    builder.parse_external_text(source);
    builder.finish()
}

/// Grammar of an instance document: internal subset (source 0 = the
/// document), then external subset. `None` without `<!DOCTYPE>`.
pub fn load_document_dtd(
    document: &str,
    path: Option<PathBuf>,
    loader: &mut dyn ExternalLoader,
) -> Option<(Doctype, Dtd)> {
    let doctype = find_doctype(document)?;
    let mut builder = DtdBuilder::new(loader);
    if !doctype.name.is_empty() {
        builder.dtd.doctype_name = Some(doctype.name.clone());
    }
    let source = builder.add_document(document, path);
    if let Some(subset) = &doctype.internal_subset {
        builder.dtd.optional_declarations = has_parameter_reference(&document[subset.clone()]);
        builder.parse_internal_subset(source, subset.clone());
    }
    if doctype.system_id.is_some() {
        builder.dtd.optional_declarations = true;
    }
    if let Some((system, range)) = &doctype.system_id {
        builder.parse_external_subset(
            doctype
                .public_id
                .as_ref()
                .map(|(public, _)| public.as_str()),
            system,
            Location {
                source,
                range: range.clone(),
            },
        );
    }
    Some((doctype, builder.finish()))
}

/// Whether `text` contains a parameter entity reference (`%name;`).
fn has_parameter_reference(text: &str) -> bool {
    text.match_indices('%').any(|(index, _)| {
        let rest = &text[index + 1..];
        rest.find(';').is_some_and(|end| is_name(&rest[..end]))
    })
}

/// Decodes the content of a character reference (`#10`, `#x1F`).
pub(crate) fn decode_char_reference(reference: &str) -> Option<char> {
    let digits = reference.strip_prefix('#')?;
    let code = match digits.strip_prefix('x') {
        Some(hex) if !hex.is_empty() && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) => {
            u32::from_str_radix(hex, 16).ok()?
        }
        None if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) => {
            digits.parse().ok()?
        }
        _ => return None,
    };
    char::from_u32(code).filter(|&character| {
        matches!(character, '\t' | '\n' | '\r')
            || ('\u{20}'..='\u{D7FF}').contains(&character)
            || ('\u{E000}'..='\u{FFFD}').contains(&character)
            || character >= '\u{10000}'
    })
}

/// Comment normalized into documentation (trimmed lines).
fn normalize_comment(text: &str) -> Option<String> {
    let text = text.lines().map(str::trim).collect::<Vec<_>>().join("\n");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// End of a `<!...>` declaration: `(rest, end of body, closed)`. A `<`
/// outside a literal interrupts an unclosed declaration.
fn declaration_end(bytes: &[u8], mut index: usize, end: usize) -> (usize, usize, bool) {
    while index < end {
        match bytes[index] {
            b'"' | b'\'' => {
                let quote = bytes[index];
                match bytes[index + 1..end].iter().position(|&byte| byte == quote) {
                    Some(offset) => index += offset + 2,
                    None => return (end, end, false),
                }
            }
            b'>' => return (index + 1, index, true),
            b'<' => return (index, index, false),
            _ => index += 1,
        }
    }
    (end, end, false)
}

/// Position of the `]]>` closing a conditional section (nested sections
/// included).
fn section_end(bytes: &[u8], mut index: usize, end: usize) -> Option<usize> {
    let mut depth = 0usize;
    while index < end {
        let rest = &bytes[index..end];
        if rest.starts_with(b"<!--") {
            index = find_after(bytes, index + 4, end, b"-->");
        } else if rest.starts_with(b"<![") {
            depth += 1;
            index += 3;
        } else if rest.starts_with(b"]]>") {
            if depth == 0 {
                return Some(index);
            }
            depth -= 1;
            index += 3;
        } else {
            index += 1;
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Expanded text
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Origin {
    /// Copied from the source starting at this offset.
    Verbatim(usize),
    /// Produced by the expansion of the `%name;` reference at this range.
    Reference(Range<usize>),
}

#[derive(Debug, Clone)]
struct Segment {
    start: usize,
    end: usize,
    origin: Origin,
}

/// Body of a declaration after parameter entity expansion.
#[derive(Debug, Default)]
struct Expanded {
    text: String,
    segments: Vec<Segment>,
    fallback: Range<usize>,
}

impl Expanded {
    fn push(&mut self, text: &str, origin: Option<&Range<usize>>, source_start: usize) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        self.segments.push(Segment {
            start,
            end: self.text.len(),
            origin: match origin {
                Some(reference) => Origin::Reference(reference.clone()),
                None => Origin::Verbatim(source_start),
            },
        });
    }

    /// Source range matching `range` of the expanded text.
    fn locate(&self, range: &Range<usize>) -> Range<usize> {
        let segment = self
            .segments
            .iter()
            .find(|segment| segment.start <= range.start && range.start < segment.end)
            .or_else(|| self.segments.last());
        match segment {
            Some(Segment {
                start,
                end,
                origin: Origin::Verbatim(offset),
            }) => {
                let from = offset + range.start.clamp(*start, *end) - start;
                let to = offset + range.end.clamp(*start, *end) - start;
                from..to.max(from)
            }
            Some(Segment {
                origin: Origin::Reference(reference),
                ..
            }) => reference.clone(),
            None => self.fallback.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Declaration tokens
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Name,
    /// `#PCDATA`, `#REQUIRED`...
    Hash,
    Percent,
    Literal,
    UnclosedLiteral,
    Punct(u8),
    Other,
    End,
}

struct Lexer<'t> {
    text: &'t str,
    position: usize,
}

impl<'t> Lexer<'t> {
    fn new(text: &'t str) -> Self {
        Self { text, position: 0 }
    }

    fn next(&mut self) -> (Token, Range<usize>) {
        let bytes = self.text.as_bytes();
        while self.position < bytes.len() && bytes[self.position].is_ascii_whitespace() {
            self.position += 1;
        }
        let start = self.position;
        let Some(&byte) = bytes.get(start) else {
            return (Token::End, start..start);
        };
        let (token, end) = match byte {
            b'"' | b'\'' => match self.text[start + 1..].find(byte as char) {
                Some(offset) => (Token::Literal, start + offset + 2),
                None => (Token::UnclosedLiteral, bytes.len()),
            },
            b'#' => (
                Token::Hash,
                scan_name_chars(self.text, start + 1, bytes.len()),
            ),
            b'%' => (Token::Percent, start + 1),
            b'(' | b')' | b'|' | b',' | b'?' | b'*' | b'+' => (Token::Punct(byte), start + 1),
            _ => {
                let end = scan_name_chars(self.text, start, bytes.len());
                if end > start {
                    (Token::Name, end)
                } else {
                    let length = self.text[start..].chars().next().map_or(1, char::len_utf8);
                    (Token::Other, start + length)
                }
            }
        };
        self.position = end;
        (token, start..end)
    }

    fn peek(&self) -> (Token, Range<usize>) {
        Lexer {
            text: self.text,
            position: self.position,
        }
        .next()
    }
}

#[derive(Debug)]
struct ParseError {
    range: Range<usize>,
    message: String,
}

impl ParseError {
    fn new(range: Range<usize>, message: impl Into<String>) -> Self {
        Self {
            range,
            message: message.into(),
        }
    }
}

fn literal_content<'t>(
    token: Token,
    text: &'t str,
    range: &Range<usize>,
) -> Result<&'t str, ParseError> {
    match token {
        Token::Literal => Ok(&text[range.start + 1..range.end - 1]),
        Token::UnclosedLiteral => Err(ParseError::new(
            range.clone(),
            "unclosed literal: closing quote expected",
        )),
        _ => Err(ParseError::new(range.clone(), "quoted value expected")),
    }
}

fn parse_content_spec(lexer: &mut Lexer<'_>, text: &str) -> Result<ContentSpec, ParseError> {
    let (token, range) = lexer.next();
    match token {
        Token::Name if &text[range.clone()] == "EMPTY" => Ok(ContentSpec::Empty),
        Token::Name if &text[range.clone()] == "ANY" => Ok(ContentSpec::Any),
        Token::Punct(b'(') => {
            let (next, next_range) = lexer.peek();
            if next == Token::Hash {
                lexer.next();
                if &text[next_range.clone()] != "#PCDATA" {
                    return Err(ParseError::new(next_range, "'#PCDATA' expected"));
                }
                parse_mixed(lexer, text)
            } else {
                parse_group(lexer, text, 0).map(ContentSpec::Children)
            }
        }
        _ => Err(ParseError::new(
            range,
            "content model expected: EMPTY, ANY or '('",
        )),
    }
}

/// Rest of a mixed model after `( #PCDATA`.
fn parse_mixed(lexer: &mut Lexer<'_>, text: &str) -> Result<ContentSpec, ParseError> {
    let mut names = Vec::new();
    loop {
        let (token, range) = lexer.next();
        match token {
            Token::Punct(b'|') => {
                let (token, range) = lexer.next();
                if token != Token::Name {
                    return Err(ParseError::new(range, "element name expected after '|'"));
                }
                names.push(text[range].to_owned());
            }
            Token::Punct(b')') => break,
            _ => {
                return Err(ParseError::new(
                    range,
                    "'|' or ')' expected in a mixed model",
                ));
            }
        }
    }
    let (token, range) = lexer.peek();
    if token == Token::Punct(b'*') {
        lexer.next();
    } else if !names.is_empty() {
        return Err(ParseError::new(
            range,
            "'*' expected after a mixed model listing elements",
        ));
    }
    Ok(ContentSpec::Mixed(names))
}

/// Group after `(`, up to `)` and its cardinality.
fn parse_group(
    lexer: &mut Lexer<'_>,
    text: &str,
    depth: usize,
) -> Result<ContentParticle, ParseError> {
    if depth >= MAX_GROUP_DEPTH {
        let (_, range) = lexer.peek();
        return Err(ParseError::new(range, "content model nested too deeply"));
    }
    let mut items = vec![parse_particle(lexer, text, depth)?];
    let mut separator = None;
    loop {
        let (token, range) = lexer.next();
        match token {
            Token::Punct(character @ (b',' | b'|')) => {
                if separator.is_some_and(|separator| separator != character) {
                    return Err(ParseError::new(
                        range,
                        "',' and '|' cannot be mixed in the same group",
                    ));
                }
                separator = Some(character);
                items.push(parse_particle(lexer, text, depth)?);
            }
            Token::Punct(b')') => break,
            _ => return Err(ParseError::new(range, "',', '|' or ')' expected")),
        }
    }
    let occurrence = parse_occurrence(lexer);
    Ok(ContentParticle {
        kind: if separator == Some(b'|') {
            ParticleKind::Choice(items)
        } else {
            ParticleKind::Sequence(items)
        },
        occurrence,
    })
}

fn parse_particle(
    lexer: &mut Lexer<'_>,
    text: &str,
    depth: usize,
) -> Result<ContentParticle, ParseError> {
    let (token, range) = lexer.next();
    match token {
        Token::Name => Ok(ContentParticle {
            kind: ParticleKind::Name(text[range].to_owned()),
            occurrence: parse_occurrence(lexer),
        }),
        Token::Punct(b'(') => parse_group(lexer, text, depth + 1),
        Token::Hash => Err(ParseError::new(
            range,
            "'#PCDATA' must be the first item of the group",
        )),
        _ => Err(ParseError::new(range, "element name or '(' expected")),
    }
}

fn parse_occurrence(lexer: &mut Lexer<'_>) -> Occurrence {
    let occurrence = match lexer.peek().0 {
        Token::Punct(b'?') => Occurrence::Optional,
        Token::Punct(b'*') => Occurrence::ZeroOrMore,
        Token::Punct(b'+') => Occurrence::OneOrMore,
        _ => return Occurrence::Once,
    };
    lexer.next();
    occurrence
}

/// Values of an enumeration after `(`, up to `)`.
fn parse_enumeration(
    lexer: &mut Lexer<'_>,
    text: &str,
    notation: bool,
) -> Result<Vec<String>, ParseError> {
    let mut values = Vec::new();
    loop {
        let (token, range) = lexer.next();
        if token != Token::Name {
            return Err(ParseError::new(
                range,
                if notation {
                    "notation name expected"
                } else {
                    "enumeration value (NMTOKEN) expected"
                },
            ));
        }
        let value = &text[range.clone()];
        if notation && !is_name(value) {
            return Err(ParseError::new(
                range,
                format!("'{value}' is not a valid notation name"),
            ));
        }
        values.push(value.to_owned());
        let (token, range) = lexer.next();
        match token {
            Token::Punct(b'|') => {}
            Token::Punct(b')') => return Ok(values),
            _ => return Err(ParseError::new(range, "'|' or ')' expected")),
        }
    }
}

/// `SYSTEM "…"`, `PUBLIC "…" "…"` (or `PUBLIC "…"` alone for a notation)
/// after the `keyword` keyword.
fn external_id(
    lexer: &mut Lexer<'_>,
    text: &str,
    keyword: Range<usize>,
    public_only: bool,
) -> Result<(Option<String>, Option<String>), ParseError> {
    match &text[keyword.clone()] {
        "SYSTEM" => {
            let (token, range) = lexer.next();
            let system = literal_content(token, text, &range)?;
            Ok((None, Some(system.to_owned())))
        }
        "PUBLIC" => {
            let (token, range) = lexer.next();
            let public = literal_content(token, text, &range)?.to_owned();
            let (token, range) = lexer.peek();
            if matches!(token, Token::Literal | Token::UnclosedLiteral) {
                lexer.next();
                let system = literal_content(token, text, &range)?;
                Ok((Some(public), Some(system.to_owned())))
            } else if public_only {
                Ok((Some(public), None))
            } else {
                Err(ParseError::new(
                    range,
                    "system identifier expected after the public identifier",
                ))
            }
        }
        other => Err(ParseError::new(
            keyword,
            format!("SYSTEM or PUBLIC expected, found '{other}'"),
        )),
    }
}

fn expect_end(lexer: &mut Lexer<'_>) -> Result<(), ParseError> {
    let (token, range) = lexer.next();
    if token == Token::End {
        Ok(())
    } else {
        Err(ParseError::new(
            range,
            "unexpected content: end of declaration '>' expected",
        ))
    }
}

// ---------------------------------------------------------------------------
// Grammar construction
// ---------------------------------------------------------------------------

/// Builds a [`Dtd`] from one or more texts.
pub struct DtdBuilder<'l> {
    dtd: Dtd,
    loader: &'l mut dyn ExternalLoader,
    /// Parameter entities being expanded.
    active: Vec<String>,
    /// Depth of conditional sections.
    depth: usize,
    /// Bytes of expanded replacement texts.
    expanded: usize,
    /// Remaining bytes of entity replacement texts for the normalization
    /// of attribute defaults ([`MAX_DOCUMENT_EXPANSION`]).
    default_budget: usize,
}

impl<'l> DtdBuilder<'l> {
    pub fn new(loader: &'l mut dyn ExternalLoader) -> Self {
        Self {
            dtd: Dtd::default(),
            loader,
            active: Vec::new(),
            depth: 0,
            expanded: 0,
            default_budget: MAX_DOCUMENT_EXPANSION,
        }
    }

    /// Adds the text of a directly parsed document.
    pub fn add_document(&mut self, text: &str, path: Option<PathBuf>) -> SourceId {
        self.push_source(SourceKind::Document(path), Arc::from(text), None)
    }

    /// Parses the internal subset `range` of `source`.
    pub fn parse_internal_subset(&mut self, source: SourceId, range: Range<usize>) {
        self.parse_declarations(source, range, false);
    }

    /// Parses the whole `source` as an external subset.
    pub fn parse_external_text(&mut self, source: SourceId) {
        let length = self.dtd.source_text(source).len();
        self.parse_declarations(source, 0..length, true);
    }

    /// Loads and parses the external subset referenced at `reference`.
    pub fn parse_external_subset(
        &mut self,
        public: Option<&str>,
        system: &str,
        reference: Location,
    ) {
        let base = self.base_path(reference.source);
        if let Some(source) = self.load_external(public, system, base.as_deref(), &reference) {
            self.parse_external_text(source);
        }
    }

    pub fn finish(mut self) -> Dtd {
        self.check_references();
        self.compute_expansions();
        self.dtd
    }

    fn push_source(
        &mut self,
        kind: SourceKind,
        text: Arc<str>,
        reference: Option<Location>,
    ) -> SourceId {
        self.dtd.sources.push(DtdSource {
            kind,
            text,
            reference,
        });
        self.dtd.sources.len() - 1
    }

    fn problem(&mut self, kind: DtdProblemKind, location: Location, message: impl Into<String>) {
        self.dtd.problems.push(DtdProblem {
            kind,
            location,
            message: message.into(),
        });
    }

    fn at(source: SourceId, range: Range<usize>) -> Location {
        Location { source, range }
    }

    /// Path used as the base of relative system identifiers.
    fn base_path(&self, source: SourceId) -> Option<PathBuf> {
        let anchored = self.dtd.anchor(&Self::at(source, 0..0));
        self.dtd.source_path(anchored.source).map(Path::to_path_buf)
    }

    fn sources_exhausted(&mut self, reference: &Location) -> bool {
        if self.dtd.sources.len() < MAX_SOURCES {
            return false;
        }
        self.dtd.incomplete = true;
        self.problem(
            DtdProblemKind::EntityExpansionLimit,
            reference.clone(),
            format!("too many DTD resources expanded (limit {MAX_SOURCES})"),
        );
        true
    }

    fn load_external(
        &mut self,
        public: Option<&str>,
        system: &str,
        base: Option<&Path>,
        reference: &Location,
    ) -> Option<SourceId> {
        if self.sources_exhausted(reference) {
            return None;
        }
        match self.loader.load(public, system, base) {
            Ok((path, text)) => Some(self.push_source(
                SourceKind::External(path),
                Arc::from(text),
                Some(reference.clone()),
            )),
            Err(error) => {
                self.dtd.incomplete = true;
                self.problem(
                    DtdProblemKind::ExternalLoad {
                        remote: error.remote,
                    },
                    reference.clone(),
                    error.message,
                );
                None
            }
        }
    }

    /// Replacement text of the parameter entity `name` referenced at `at`,
    /// and the file path for an external entity. Checks the declaration,
    /// recursion, depth and expansion
    /// budget.
    fn parameter_text(&mut self, name: &str, at: &Location) -> Option<(Arc<str>, Option<PathBuf>)> {
        let Some(value) = self
            .dtd
            .parameter_entity(name)
            .map(|entity| entity.value.clone())
        else {
            if !self.dtd.incomplete {
                self.problem(
                    DtdProblemKind::UndeclaredParameterEntity,
                    at.clone(),
                    format!("the parameter entity '%{name};' is not declared"),
                );
            }
            return None;
        };
        if self.active.iter().any(|active| active == name) {
            self.problem(
                DtdProblemKind::EntityRecursion,
                at.clone(),
                format!("recursive reference to the parameter entity '%{name};'"),
            );
            return None;
        }
        if self.active.len() >= MAX_ENTITY_DEPTH {
            self.problem(
                DtdProblemKind::EntityExpansionLimit,
                at.clone(),
                format!("parameter entities nested too deeply (limit {MAX_ENTITY_DEPTH})"),
            );
            return None;
        }
        let (text, path): (Arc<str>, Option<PathBuf>) = match value {
            EntityValue::Internal(text) => (Arc::from(text.as_str()), None),
            EntityValue::External {
                public,
                system,
                base,
                ..
            } => {
                if self.sources_exhausted(at) {
                    return None;
                }
                match self
                    .loader
                    .load(public.as_deref(), &system, base.as_deref())
                {
                    Ok((path, text)) => (Arc::from(text), Some(path)),
                    Err(error) => {
                        self.dtd.incomplete = true;
                        self.problem(
                            DtdProblemKind::ExternalLoad {
                                remote: error.remote,
                            },
                            at.clone(),
                            error.message,
                        );
                        return None;
                    }
                }
            }
        };
        self.expanded = self.expanded.saturating_add(text.len());
        if self.expanded > MAX_PARAMETER_EXPANSION {
            self.problem(
                DtdProblemKind::EntityExpansionLimit,
                at.clone(),
                format!(
                    "parameter entity expansion too large (limit {MAX_PARAMETER_EXPANSION} bytes)"
                ),
            );
            return None;
        }
        Some((text, path))
    }

    /// Declarations, comments, processing instructions, conditional
    /// sections and `%name;` references of `range` in `source`.
    fn parse_declarations(&mut self, source: SourceId, range: Range<usize>, external: bool) {
        let text = self.dtd.sources[source].text.clone();
        let bytes = text.as_bytes();
        let end = range.end.min(text.len());
        let mut index = range.start;
        let mut documentation: Option<String> = None;
        while index < end {
            let byte = bytes[index];
            if byte.is_ascii_whitespace() {
                index += 1;
                continue;
            }
            let rest = &text[index..end];
            if let Some(comment) = rest.strip_prefix("<!--") {
                match comment.find("-->") {
                    Some(offset) => {
                        documentation = normalize_comment(&comment[..offset]);
                        index += offset + 7;
                    }
                    None => {
                        self.problem(
                            DtdProblemKind::Syntax,
                            Self::at(source, index..end),
                            "unclosed comment: '-->' expected",
                        );
                        return;
                    }
                }
                continue;
            }
            if rest.starts_with("<?") {
                match rest.find("?>") {
                    Some(offset) => index += offset + 2,
                    None => {
                        self.problem(
                            DtdProblemKind::Syntax,
                            Self::at(source, index..end),
                            "unclosed processing instruction: '?>' expected",
                        );
                        return;
                    }
                }
                documentation = None;
                continue;
            }
            if rest.starts_with("<![") {
                index = self.conditional_section(source, &text, index, end, external);
                documentation = None;
                continue;
            }
            if rest.starts_with("<!") {
                let keyword_end = scan_name_chars(&text, index + 2, end);
                let keyword = &text[index + 2..keyword_end];
                let (declaration_end, body_end, closed) = declaration_end(bytes, keyword_end, end);
                match keyword {
                    "ELEMENT" | "ATTLIST" | "ENTITY" | "NOTATION" => self.declaration(
                        source,
                        keyword,
                        index..declaration_end,
                        keyword_end..body_end,
                        documentation.take(),
                    ),
                    _ => self.problem(
                        DtdProblemKind::Syntax,
                        Self::at(source, index..keyword_end.max(index + 2)),
                        format!(
                            "unknown declaration '<!{keyword}': ELEMENT, ATTLIST, ENTITY or NOTATION expected"
                        ),
                    ),
                }
                if !closed {
                    self.problem(
                        DtdProblemKind::Syntax,
                        Self::at(source, index..keyword_end.max(index + 2)),
                        "unclosed declaration: '>' expected",
                    );
                }
                documentation = None;
                index = declaration_end;
                continue;
            }
            if byte == b'%' {
                let name_end = scan_name_chars(&text, index + 1, end);
                if name_end > index + 1 && name_end < end && bytes[name_end] == b';' {
                    let name = text[index + 1..name_end].to_owned();
                    self.include_parameter_entity(source, &name, index..name_end + 1, external);
                    index = name_end + 1;
                } else {
                    self.problem(
                        DtdProblemKind::Syntax,
                        Self::at(source, index..name_end.max(index + 1)),
                        "invalid parameter entity reference: '%name;' expected",
                    );
                    index = name_end.max(index + 1);
                }
                documentation = None;
                continue;
            }
            // The unexpected character may be longer than one byte.
            let next = index + text[index..].chars().next().map_or(1, char::len_utf8);
            let stop = text[next.min(end)..end]
                .find(['<', '%'])
                .map_or(end, |offset| next + offset);
            let unexpected_end = index + text[index..stop].trim_end().len();
            self.problem(
                DtdProblemKind::Syntax,
                Self::at(source, index..unexpected_end.max(next.min(end))),
                "unexpected content in the DTD: '<!…>' declaration expected",
            );
            documentation = None;
            index = stop;
        }
    }

    /// `%name;` reference between two declarations: its replacement text is
    /// parsed as a sequence of declarations.
    fn include_parameter_entity(
        &mut self,
        source: SourceId,
        name: &str,
        reference: Range<usize>,
        external: bool,
    ) {
        let at = Self::at(source, reference);
        let Some((text, path)) = self.parameter_text(name, &at) else {
            return;
        };
        if self.sources_exhausted(&at) {
            return;
        }
        let from_file = path.is_some();
        let kind = match path {
            Some(path) => SourceKind::External(path),
            None => SourceKind::Replacement {
                entity: name.to_owned(),
                reference: at.clone(),
            },
        };
        let length = text.len();
        let id = self.push_source(kind, text, Some(at));
        self.active.push(name.to_owned());
        self.parse_declarations(id, 0..length, external || from_file);
        self.active.pop();
    }

    fn conditional_section(
        &mut self,
        source: SourceId,
        text: &str,
        start: usize,
        end: usize,
        external: bool,
    ) -> usize {
        let bytes = text.as_bytes();
        let Some(open) = text[start + 3..end]
            .find('[')
            .map(|offset| start + 3 + offset)
        else {
            self.problem(
                DtdProblemKind::Syntax,
                Self::at(source, start..end),
                "invalid conditional section: '[' expected",
            );
            return end;
        };
        let keyword_range = start + 3..open;
        let keyword = self.expand(source, keyword_range.clone()).text;
        let keyword = keyword.trim();
        let close = section_end(bytes, open + 1, end);
        let next = close.map_or(end, |close| close + 3);
        let opening = Self::at(source, start..open + 1);
        if close.is_none() {
            self.problem(
                DtdProblemKind::Syntax,
                opening.clone(),
                "unclosed conditional section: ']]>' expected",
            );
        }
        if !external {
            self.problem(
                DtdProblemKind::ConditionalSection,
                opening,
                "conditional sections are only allowed in the external subset",
            );
            return next;
        }
        match keyword {
            "INCLUDE" => {
                if self.depth >= MAX_ENTITY_DEPTH {
                    self.problem(
                        DtdProblemKind::EntityExpansionLimit,
                        opening,
                        "conditional sections nested too deeply",
                    );
                    return next;
                }
                self.depth += 1;
                self.parse_declarations(source, open + 1..close.unwrap_or(end), external);
                self.depth -= 1;
            }
            "IGNORE" => {}
            other => self.problem(
                DtdProblemKind::Syntax,
                Self::at(source, keyword_range),
                format!("INCLUDE or IGNORE expected, found '{other}'"),
            ),
        }
        next
    }

    /// Expands the `%name;` references (outside literals) of `range`.
    fn expand(&mut self, source: SourceId, range: Range<usize>) -> Expanded {
        let text = self.dtd.sources[source].text.clone();
        let mut expanded = Expanded {
            fallback: range.clone(),
            ..Expanded::default()
        };
        self.expand_into(
            &text[range.clone()],
            range.start,
            source,
            None,
            &mut expanded,
        );
        expanded
    }

    /// `origin`: `None` for a text copied from the source starting at
    /// `base`, otherwise the reference the text comes from.
    fn expand_into(
        &mut self,
        text: &str,
        base: usize,
        source: SourceId,
        origin: Option<&Range<usize>>,
        out: &mut Expanded,
    ) {
        let bytes = text.as_bytes();
        let mut index = 0;
        let mut copied = 0;
        while index < bytes.len() {
            match bytes[index] {
                quote @ (b'"' | b'\'') => {
                    index = bytes[index + 1..]
                        .iter()
                        .position(|&byte| byte == quote)
                        .map_or(bytes.len(), |offset| index + offset + 2);
                }
                b'%' => {
                    let name_end = scan_name_chars(text, index + 1, bytes.len());
                    if name_end == index + 1 || bytes.get(name_end) != Some(&b';') {
                        index += 1;
                        continue;
                    }
                    out.push(&text[copied..index], origin, base + copied);
                    let reference = origin.cloned().unwrap_or(base + index..base + name_end + 1);
                    let name = &text[index + 1..name_end];
                    let at = Self::at(source, reference.clone());
                    if let Some((replacement, _)) = self.parameter_text(name, &at) {
                        out.push(" ", Some(&reference), 0);
                        self.active.push(name.to_owned());
                        self.expand_into(&replacement, 0, source, Some(&reference), out);
                        self.active.pop();
                        out.push(" ", Some(&reference), 0);
                    }
                    index = name_end + 1;
                    copied = index;
                }
                _ => index += 1,
            }
        }
        out.push(&text[copied..], origin, base + copied);
    }

    /// Replacement text of an internal entity: parameter entity and
    /// character references expanded.
    fn entity_value(&mut self, raw: &str, at: &Location) -> String {
        let bytes = raw.as_bytes();
        let mut value = String::with_capacity(raw.len());
        let mut index = 0;
        while index < bytes.len() {
            match bytes[index] {
                b'%' => {
                    let name_end = scan_name_chars(raw, index + 1, raw.len());
                    if name_end > index + 1 && bytes.get(name_end) == Some(&b';') {
                        let name = &raw[index + 1..name_end];
                        if let Some((replacement, _)) = self.parameter_text(name, at) {
                            self.active.push(name.to_owned());
                            let expanded = self.entity_value(&replacement, at);
                            self.active.pop();
                            value.push_str(&expanded);
                        }
                        index = name_end + 1;
                    } else {
                        value.push('%');
                        index += 1;
                    }
                }
                b'&' if bytes.get(index + 1) == Some(&b'#') => {
                    let decoded = raw[index..].find(';').and_then(|end| {
                        decode_char_reference(&raw[index + 1..index + end])
                            .map(|character| (character, end))
                    });
                    match decoded {
                        Some((character, end)) => {
                            value.push(character);
                            index += end + 1;
                        }
                        None => {
                            self.problem(
                                DtdProblemKind::Syntax,
                                at.clone(),
                                "invalid character reference in the entity value",
                            );
                            value.push('&');
                            index += 1;
                        }
                    }
                }
                _ => {
                    let character = raw[index..].chars().next().unwrap_or_default();
                    value.push(character);
                    index += character.len_utf8().max(1);
                }
            }
        }
        value
    }

    fn declaration(
        &mut self,
        source: SourceId,
        keyword: &str,
        whole: Range<usize>,
        body: Range<usize>,
        documentation: Option<String>,
    ) {
        let expanded = self.expand(source, body.clone());
        let declaration = Self::at(source, whole.clone());
        let result = match keyword {
            "ELEMENT" => self.element_declaration(&expanded, &declaration, documentation),
            "ATTLIST" => self.attlist_declaration(&expanded, &declaration, documentation),
            "ENTITY" => self.entity_declaration(&expanded, &declaration, documentation),
            _ => self.notation_declaration(&expanded, &declaration, documentation),
        };
        if let Err(error) = result {
            let mut range = expanded.locate(&error.range);
            if range.is_empty() {
                // Error at the end of the declaration: the keyword is reported.
                range = whole.start..body.start;
            }
            self.problem(
                DtdProblemKind::Syntax,
                Self::at(source, range),
                error.message,
            );
        }
    }

    fn located(expanded: &Expanded, declaration: &Location, range: &Range<usize>) -> Location {
        Self::at(declaration.source, expanded.locate(range))
    }

    fn check_name(&mut self, name: &str, location: &Location) {
        if !is_name(name) {
            self.problem(
                DtdProblemKind::Syntax,
                location.clone(),
                format!("'{name}' is not a valid XML name"),
            );
        }
    }

    fn element_declaration(
        &mut self,
        expanded: &Expanded,
        declaration: &Location,
        documentation: Option<String>,
    ) -> Result<(), ParseError> {
        let text = expanded.text.as_str();
        let mut lexer = Lexer::new(text);
        let (token, name_range) = lexer.next();
        if token != Token::Name {
            return Err(ParseError::new(
                name_range,
                "element name expected after '<!ELEMENT'",
            ));
        }
        let name = text[name_range.clone()].to_owned();
        let location = Self::located(expanded, declaration, &name_range);
        self.check_name(&name, &location);
        let parsed = parse_content_spec(&mut lexer, text)
            .and_then(|content| expect_end(&mut lexer).map(|()| content));
        let (content, result) = match parsed {
            Ok(content) => (content, Ok(())),
            // Element registered anyway to avoid cascading errors.
            Err(error) => (ContentSpec::Any, Err(error)),
        };
        if self.dtd.element_index.contains_key(&name) {
            self.problem(
                DtdProblemKind::DuplicateElement,
                location,
                format!("the element '{name}' is already declared"),
            );
        } else {
            self.dtd
                .element_index
                .insert(name.clone(), self.dtd.elements.len());
            self.dtd.elements.push(ElementDecl {
                name,
                location,
                declaration: declaration.clone(),
                content,
                documentation,
            });
        }
        result
    }

    fn attlist_declaration(
        &mut self,
        expanded: &Expanded,
        declaration: &Location,
        documentation: Option<String>,
    ) -> Result<(), ParseError> {
        let text = expanded.text.as_str();
        let mut lexer = Lexer::new(text);
        let (token, element_range) = lexer.next();
        if token != Token::Name {
            return Err(ParseError::new(
                element_range,
                "element name expected after '<!ATTLIST'",
            ));
        }
        let element = text[element_range].to_owned();
        loop {
            let (token, name_range) = lexer.next();
            match token {
                Token::End => return Ok(()),
                Token::Name => {}
                _ => {
                    return Err(ParseError::new(
                        name_range,
                        "attribute name or end of declaration '>' expected",
                    ));
                }
            }
            let name = text[name_range.clone()].to_owned();
            let (token, type_range) = lexer.next();
            let attribute_type = match token {
                Token::Name => match &text[type_range.clone()] {
                    "CDATA" => AttributeType::CData,
                    "ID" => AttributeType::Id,
                    "IDREF" => AttributeType::IdRef,
                    "IDREFS" => AttributeType::IdRefs,
                    "ENTITY" => AttributeType::Entity,
                    "ENTITIES" => AttributeType::Entities,
                    "NMTOKEN" => AttributeType::NmToken,
                    "NMTOKENS" => AttributeType::NmTokens,
                    "NOTATION" => {
                        let (token, range) = lexer.next();
                        if token != Token::Punct(b'(') {
                            return Err(ParseError::new(range, "'(' expected after NOTATION"));
                        }
                        AttributeType::Notation(parse_enumeration(&mut lexer, text, true)?)
                    }
                    other => {
                        return Err(ParseError::new(
                            type_range.clone(),
                            format!(
                                "unknown attribute type '{other}': CDATA, ID, IDREF, IDREFS, ENTITY, ENTITIES, NMTOKEN, NMTOKENS, NOTATION or enumeration expected"
                            ),
                        ));
                    }
                },
                Token::Punct(b'(') => {
                    AttributeType::Enumeration(parse_enumeration(&mut lexer, text, false)?)
                }
                _ => {
                    return Err(ParseError::new(
                        type_range,
                        format!("type expected for the attribute '{name}'"),
                    ));
                }
            };
            let cdata = attribute_type == AttributeType::CData;
            let (token, default_range) = lexer.next();
            let default = match token {
                Token::Hash => match &text[default_range.clone()] {
                    "#REQUIRED" => DefaultDecl::Required,
                    "#IMPLIED" => DefaultDecl::Implied,
                    "#FIXED" => {
                        let (token, range) = lexer.next();
                        let raw = literal_content(token, text, &range)?;
                        let location = Self::located(expanded, declaration, &range);
                        DefaultDecl::Fixed(self.default_value(raw, cdata, &location))
                    }
                    other => {
                        return Err(ParseError::new(
                            default_range.clone(),
                            format!("#REQUIRED, #IMPLIED or #FIXED expected, found '{other}'"),
                        ));
                    }
                },
                Token::Literal | Token::UnclosedLiteral => {
                    let raw = literal_content(token, text, &default_range)?;
                    let location = Self::located(expanded, declaration, &default_range);
                    DefaultDecl::Default(self.default_value(raw, cdata, &location))
                }
                _ => {
                    return Err(ParseError::new(
                        default_range,
                        format!(
                            "default value expected for the attribute '{name}': #REQUIRED, #IMPLIED, #FIXED or a value"
                        ),
                    ));
                }
            };
            let location = Self::located(expanded, declaration, &name_range);
            self.check_name(&name, &location);
            self.add_attribute(AttributeDecl {
                element: element.clone(),
                name,
                location,
                declaration: declaration.clone(),
                attribute_type,
                default,
                documentation: documentation.clone(),
            });
        }
    }

    /// Normalized default value of an attribute.
    fn default_value(&mut self, raw: &str, cdata: bool, location: &Location) -> String {
        if raw.contains('<') {
            self.problem(
                DtdProblemKind::Syntax,
                location.clone(),
                "'<' is not allowed in an attribute value",
            );
        }
        let mut budget = self.default_budget.min(MAX_ENTITY_EXPANSION);
        let before = budget;
        let value = self
            .dtd
            .normalize_attribute_value_within(raw, cdata, &mut budget);
        self.default_budget -= before - budget;
        value.unwrap_or_else(|| raw.to_owned())
    }

    fn add_attribute(&mut self, attribute: AttributeDecl) {
        let mut has_id = false;
        for other in self.dtd.attributes_of(&attribute.element) {
            if other.name == attribute.name {
                // The first declaration of an attribute wins.
                return;
            }
            has_id |= other.attribute_type == AttributeType::Id;
        }
        if attribute.attribute_type == AttributeType::Id {
            if has_id {
                self.problem(
                    DtdProblemKind::MultipleIdAttributes,
                    attribute.location.clone(),
                    format!(
                        "the element '{}' already has an ID attribute",
                        attribute.element
                    ),
                );
            }
            if attribute.default.value().is_some() {
                self.problem(
                    DtdProblemKind::IdAttributeDefault,
                    attribute.location.clone(),
                    format!(
                        "the ID attribute '{}' must be #IMPLIED or #REQUIRED",
                        attribute.name
                    ),
                );
            }
        }
        let index = self.dtd.attributes.len();
        self.dtd
            .attribute_index
            .entry(attribute.element.clone())
            .or_default()
            .push(index);
        self.dtd.attributes.push(attribute);
    }

    fn entity_declaration(
        &mut self,
        expanded: &Expanded,
        declaration: &Location,
        documentation: Option<String>,
    ) -> Result<(), ParseError> {
        let text = expanded.text.as_str();
        let mut lexer = Lexer::new(text);
        let (mut token, mut name_range) = lexer.next();
        let parameter = token == Token::Percent;
        if parameter {
            (token, name_range) = lexer.next();
        }
        if token != Token::Name {
            return Err(ParseError::new(name_range, "entity name expected"));
        }
        let name = text[name_range.clone()].to_owned();
        let location = Self::located(expanded, declaration, &name_range);
        self.check_name(&name, &location);
        let (token, value_range) = lexer.next();
        let value = match token {
            Token::Literal => {
                let raw = &text[value_range.start + 1..value_range.end - 1];
                let at = Self::located(expanded, declaration, &value_range);
                EntityValue::Internal(self.entity_value(raw, &at))
            }
            Token::Name => {
                let (public, system) = external_id(&mut lexer, text, value_range, false)?;
                let (token, range) = lexer.peek();
                let notation = if token == Token::Name && &text[range.clone()] == "NDATA" {
                    lexer.next();
                    if parameter {
                        return Err(ParseError::new(
                            range,
                            "NDATA is not allowed for a parameter entity",
                        ));
                    }
                    let (token, notation_range) = lexer.next();
                    if token != Token::Name {
                        return Err(ParseError::new(
                            notation_range,
                            "notation name expected after NDATA",
                        ));
                    }
                    Some(text[notation_range].to_owned())
                } else {
                    None
                };
                EntityValue::External {
                    public,
                    system: system.unwrap_or_default(),
                    notation,
                    base: self.base_path(declaration.source),
                }
            }
            Token::UnclosedLiteral => {
                return Err(ParseError::new(
                    value_range,
                    "unclosed literal: closing quote expected",
                ));
            }
            _ => {
                return Err(ParseError::new(
                    value_range,
                    "quoted value, SYSTEM or PUBLIC expected",
                ));
            }
        };
        let end = expect_end(&mut lexer);
        let entity = EntityDecl {
            name: name.clone(),
            parameter,
            location,
            declaration: declaration.clone(),
            value,
            documentation,
            expansion: EntityExpansion::default(),
        };
        // The first declaration of an entity wins.
        if parameter {
            if !self.dtd.parameter_index.contains_key(&name) {
                self.dtd
                    .parameter_index
                    .insert(name, self.dtd.parameter_entities.len());
                self.dtd.parameter_entities.push(entity);
            }
        } else if !self.dtd.general_index.contains_key(&name) {
            self.dtd
                .general_index
                .insert(name, self.dtd.general_entities.len());
            self.dtd.general_entities.push(entity);
        }
        end
    }

    fn notation_declaration(
        &mut self,
        expanded: &Expanded,
        declaration: &Location,
        documentation: Option<String>,
    ) -> Result<(), ParseError> {
        let text = expanded.text.as_str();
        let mut lexer = Lexer::new(text);
        let (token, name_range) = lexer.next();
        if token != Token::Name {
            return Err(ParseError::new(
                name_range,
                "notation name expected after '<!NOTATION'",
            ));
        }
        let name = text[name_range.clone()].to_owned();
        let location = Self::located(expanded, declaration, &name_range);
        self.check_name(&name, &location);
        let (token, keyword) = lexer.next();
        if token != Token::Name {
            return Err(ParseError::new(keyword, "SYSTEM or PUBLIC expected"));
        }
        let (public, system) = external_id(&mut lexer, text, keyword, true)?;
        let end = expect_end(&mut lexer);
        if self.dtd.notation_index.contains_key(&name) {
            self.problem(
                DtdProblemKind::DuplicateNotation,
                location,
                format!("the notation '{name}' is already declared"),
            );
        } else {
            self.dtd
                .notation_index
                .insert(name.clone(), self.dtd.notations.len());
            self.dtd.notations.push(NotationDecl {
                name,
                location,
                declaration: declaration.clone(),
                public,
                system,
                documentation,
            });
        }
        end
    }

    /// Notations of `NDATA` entities and enumerated default values.
    fn check_references(&mut self) {
        let mut problems = Vec::new();
        if !self.dtd.incomplete {
            for entity in &self.dtd.general_entities {
                if let EntityValue::External {
                    notation: Some(notation),
                    ..
                } = &entity.value
                    && self.dtd.notation(notation).is_none()
                {
                    problems.push((
                        DtdProblemKind::UndeclaredNotation,
                        entity.location.clone(),
                        format!(
                            "the notation '{notation}' of the entity '{}' is not declared",
                            entity.name
                        ),
                    ));
                }
            }
        }
        for attribute in &self.dtd.attributes {
            if let (Some(values), Some(value)) =
                (attribute.attribute_type.values(), attribute.default.value())
                && !values.iter().any(|allowed| allowed == value)
            {
                problems.push((
                    DtdProblemKind::InvalidDefaultValue,
                    attribute.location.clone(),
                    format!(
                        "the default value '{value}' of the attribute '{}' is not in the enumeration ({})",
                        attribute.name,
                        values.join(" | ")
                    ),
                ));
            }
        }
        for (kind, location, message) in problems {
            self.problem(kind, location, message);
        }
    }

    /// Computes the expansion of each general entity without materializing
    /// it (memoized, saturated sizes).
    fn compute_expansions(&mut self) {
        let count = self.dtd.general_entities.len();
        let mut computed: Vec<Option<EntityExpansion>> = vec![None; count];
        let mut stack = Vec::new();
        for index in 0..count {
            self.expansion(index, &mut computed, &mut stack);
        }
        for (entity, expansion) in self.dtd.general_entities.iter_mut().zip(computed) {
            entity.expansion = expansion.unwrap_or_default();
        }
    }

    fn expansion(
        &mut self,
        index: usize,
        computed: &mut Vec<Option<EntityExpansion>>,
        stack: &mut Vec<usize>,
    ) -> EntityExpansion {
        if let Some(expansion) = &computed[index] {
            return expansion.clone();
        }
        let entity = &self.dtd.general_entities[index];
        let text = match &entity.value {
            EntityValue::External { notation, .. } => {
                let expansion = EntityExpansion {
                    external: true,
                    unparsed: notation.is_some(),
                    ..EntityExpansion::default()
                };
                computed[index] = Some(expansion.clone());
                return expansion;
            }
            EntityValue::Internal(text) => text.clone(),
        };
        let name = entity.name.clone();
        let location = entity.location.clone();
        let mut result = EntityExpansion {
            blank: true,
            markup: text.contains('<'),
            ..EntityExpansion::default()
        };
        let mut own_problem = None;
        if stack.len() >= MAX_ENTITY_DEPTH {
            result.error = Some(ExpansionError::TooLarge);
            own_problem = Some((
                DtdProblemKind::EntityExpansionLimit,
                format!("entities nested too deeply from '{name}' (limit {MAX_ENTITY_DEPTH})"),
            ));
        } else {
            stack.push(index);
            let mut rest = text.as_str();
            while !rest.is_empty() {
                let (plain, reference, tail) = match rest.find('&') {
                    Some(position) => {
                        let after = &rest[position + 1..];
                        match after.find(';') {
                            Some(end) => {
                                (&rest[..position], Some(&after[..end]), &after[end + 1..])
                            }
                            None => (rest, None, ""),
                        }
                    }
                    None => (rest, None, ""),
                };
                result.length = result.length.saturating_add(plain.len());
                result.blank &= plain
                    .chars()
                    .all(|character| character.is_ascii_whitespace());
                rest = tail;
                let Some(reference) = reference else {
                    continue;
                };
                if reference.starts_with('#')
                    || PREDEFINED_ENTITIES
                        .iter()
                        .any(|(predefined, _)| *predefined == reference)
                {
                    result.length = result.length.saturating_add(1);
                    result.blank = false;
                    continue;
                }
                let Some(&nested) = self.dtd.general_index.get(reference) else {
                    result.length = result.length.saturating_add(reference.len() + 2);
                    result.blank = false;
                    continue;
                };
                if stack.contains(&nested) {
                    result.error = Some(ExpansionError::Recursive);
                    own_problem = Some((
                        DtdProblemKind::EntityRecursion,
                        format!("the entity '{name}' references itself (via '&{reference};')"),
                    ));
                    break;
                }
                let inner = self.expansion(nested, computed, stack);
                result.length = result.length.saturating_add(inner.length);
                result.blank &= inner.blank;
                result.markup |= inner.markup;
                result.external |= inner.external;
                if inner.unparsed {
                    result.markup = true;
                }
                if inner.error.is_some() {
                    result.error = inner.error;
                    break;
                }
                if result.length > MAX_ENTITY_EXPANSION {
                    break;
                }
            }
            stack.pop();
            if result.error.is_none() && result.length > MAX_ENTITY_EXPANSION {
                result.error = Some(ExpansionError::TooLarge);
                own_problem = Some((
                    DtdProblemKind::EntityExpansionLimit,
                    format!(
                        "the expansion of the entity '{name}' exceeds the limit of {MAX_ENTITY_EXPANSION} bytes (exponential entity expansion)"
                    ),
                ));
            }
        }
        if let Some((kind, message)) = own_problem {
            self.problem(kind, location, message);
        }
        computed[index] = Some(result.clone());
        result
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// Test loader: virtual files by system identifier.
    #[derive(Default)]
    struct MemoryLoader {
        files: HashMap<String, String>,
        requests: Vec<(Option<String>, String, Option<PathBuf>)>,
    }

    impl ExternalLoader for MemoryLoader {
        fn load(
            &mut self,
            public: Option<&str>,
            system: &str,
            base: Option<&Path>,
        ) -> Result<(PathBuf, String), LoadError> {
            self.requests.push((
                public.map(str::to_owned),
                system.to_owned(),
                base.map(Path::to_path_buf),
            ));
            if system.starts_with("http") {
                return Err(LoadError {
                    message: format!("remote DTD '{system}'"),
                    remote: true,
                });
            }
            self.files
                .get(system)
                .map(|text| (PathBuf::from(format!("/dtd/{system}")), text.clone()))
                .ok_or_else(|| LoadError {
                    message: format!("file '{system}' not found"),
                    remote: false,
                })
        }
    }

    fn parse(text: &str) -> Dtd {
        parse_dtd(text, Some(PathBuf::from("/dtd/main.dtd")), &mut NoLoader)
    }

    fn problem_ids(dtd: &Dtd) -> Vec<&'static str> {
        dtd.problems
            .iter()
            .map(|problem| problem.kind.id())
            .collect()
    }

    fn text_at<'d>(dtd: &'d Dtd, location: &Location) -> &'d str {
        &dtd.source_text(location.source)[location.range.clone()]
    }

    #[test]
    fn parses_element_declarations_and_content_models() {
        let dtd = parse(
            "<!-- A book. -->\n<!ELEMENT book (title, (chapter | appendix)+, index?)>\n<!ELEMENT title (#PCDATA)>\n<!ELEMENT p (#PCDATA|em | strong)*>\n<!ELEMENT br EMPTY>\n<!ELEMENT any ANY>\n<!ELEMENT one (item)>",
        );
        assert!(dtd.problems.is_empty(), "{:?}", dtd.problems);
        let book = dtd.element("book").expect("book should be declared");
        assert_eq!(
            book.content.to_string(),
            "(title, (chapter | appendix)+, index?)"
        );
        assert_eq!(book.documentation.as_deref(), Some("A book."));
        assert_eq!(text_at(&dtd, &book.location), "book");
        assert!(text_at(&dtd, &book.declaration).starts_with("<!ELEMENT book"));
        assert_eq!(
            dtd.element("title").unwrap().content,
            ContentSpec::Mixed(Vec::new())
        );
        assert_eq!(
            dtd.element("p").unwrap().content,
            ContentSpec::Mixed(vec!["em".to_owned(), "strong".to_owned()])
        );
        assert_eq!(dtd.element("br").unwrap().content, ContentSpec::Empty);
        assert_eq!(dtd.element("any").unwrap().content, ContentSpec::Any);
        assert_eq!(dtd.element("one").unwrap().content.to_string(), "(item)");
        // Documentation only applies to the following declaration.
        assert_eq!(dtd.element("title").unwrap().documentation, None);
    }

    #[test]
    fn parses_attribute_lists() {
        let dtd = parse(
            "<!ENTITY company \"ACME\">\n<!NOTATION gif SYSTEM \"image/gif\">\n<!-- Common attributes. -->\n<!ATTLIST item\n  id ID #REQUIRED\n  ref IDREF #IMPLIED\n  refs IDREFS #IMPLIED\n  kind (a|b | c) \"b\"\n  version CDATA #FIXED \"1.0\"\n  owner CDATA '&company; Inc'\n  tokens NMTOKENS #IMPLIED\n  token NMTOKEN #IMPLIED\n  logo ENTITY #IMPLIED\n  logos ENTITIES #IMPLIED\n  format NOTATION (gif) #IMPLIED>\n<!ATTLIST item kind CDATA #IMPLIED extra CDATA #IMPLIED>",
        );
        assert!(dtd.problems.is_empty(), "{:?}", dtd.problems);
        let names = dtd
            .attributes_of("item")
            .map(|attribute| attribute.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "id", "ref", "refs", "kind", "version", "owner", "tokens", "token", "logo",
                "logos", "format", "extra"
            ]
        );
        let kind = dtd.attribute("item", "kind").unwrap();
        // First declaration of `kind` kept.
        assert_eq!(
            kind.attribute_type,
            AttributeType::Enumeration(vec!["a".into(), "b".into(), "c".into()])
        );
        assert_eq!(kind.default, DefaultDecl::Default("b".into()));
        assert_eq!(kind.documentation.as_deref(), Some("Common attributes."));
        assert_eq!(kind.display(), "<!ATTLIST item kind (a | b | c) \"b\">");
        assert_eq!(
            dtd.attribute("item", "version").unwrap().default,
            DefaultDecl::Fixed("1.0".into())
        );
        assert_eq!(
            dtd.attribute("item", "owner").unwrap().default,
            DefaultDecl::Default("ACME Inc".into())
        );
        assert_eq!(
            dtd.attribute("item", "format")
                .unwrap()
                .attribute_type
                .to_string(),
            "NOTATION (gif)"
        );
        assert_eq!(
            text_at(&dtd, &dtd.attribute("item", "id").unwrap().location),
            "id"
        );
    }

    #[test]
    fn parses_entities_and_notations() {
        let dtd = parse(
            "<!ENTITY % inline \"em | strong\">\n<!ENTITY copy \"&#169; &#x41;\">\n<!ENTITY chapter SYSTEM \"chapter.xml\">\n<!ENTITY logo PUBLIC \"-//ACME//Logo\" \"logo.gif\" NDATA gif>\n<!ENTITY amp \"&#38;#38;\">\n<!NOTATION gif PUBLIC \"-//GIF\">\n<!NOTATION png SYSTEM \"image/png\">\n<!ENTITY inline-copy \"%inline;\">",
        );
        assert!(dtd.problems.is_empty(), "{:?}", dtd.problems);
        let inline = dtd.parameter_entity("inline").unwrap();
        assert_eq!(inline.value, EntityValue::Internal("em | strong".into()));
        assert_eq!(inline.display(), "<!ENTITY % inline \"em | strong\">");
        assert_eq!(
            dtd.general_entity("copy").unwrap().value,
            EntityValue::Internal("© A".into())
        );
        assert_eq!(
            dtd.general_entity("inline-copy").unwrap().value,
            EntityValue::Internal("em | strong".into())
        );
        let chapter = dtd.general_entity("chapter").unwrap();
        assert!(chapter.expansion.external);
        assert!(!chapter.expansion.unparsed);
        assert_eq!(
            chapter.value,
            EntityValue::External {
                public: None,
                system: "chapter.xml".into(),
                notation: None,
                base: Some(PathBuf::from("/dtd/main.dtd")),
            }
        );
        let logo = dtd.general_entity("logo").unwrap();
        assert!(logo.expansion.unparsed);
        assert_eq!(
            logo.display(),
            "<!ENTITY logo PUBLIC \"-//ACME//Logo\" \"logo.gif\" NDATA gif>"
        );
        assert_eq!(
            dtd.notation("gif").unwrap().public.as_deref(),
            Some("-//GIF")
        );
        assert_eq!(dtd.notation("gif").unwrap().system, None);
        assert_eq!(
            dtd.general_entity("amp").unwrap().value,
            EntityValue::Internal("&#38;".into())
        );
        assert!(dtd.general_entity("parameter").is_none());
    }

    #[test]
    fn expands_parameter_entities_in_and_between_declarations() {
        let dtd = parse(
            "<!ENTITY % inline \"em | strong\">\n<!ENTITY % common \"id ID #IMPLIED class CDATA #IMPLIED\">\n<!ENTITY % decls \"<!ELEMENT em (#PCDATA)> <!ELEMENT strong (#PCDATA)>\">\n<!ELEMENT p (#PCDATA | %inline;)*>\n<!ATTLIST p %common; lang NMTOKEN #IMPLIED>\n%decls;",
        );
        assert!(dtd.problems.is_empty(), "{:?}", dtd.problems);
        assert_eq!(
            dtd.element("p").unwrap().content,
            ContentSpec::Mixed(vec!["em".into(), "strong".into()])
        );
        let class = dtd.attribute("p", "class").unwrap();
        // Name coming from an entity: located on the `%common;` reference.
        assert_eq!(text_at(&dtd, &class.location), "%common;");
        let lang = dtd.attribute("p", "lang").unwrap();
        assert_eq!(text_at(&dtd, &lang.location), "lang");
        let em = dtd.element("em").unwrap();
        // Declaration coming from a replacement text: anchored on `%decls;`.
        assert!(matches!(
            dtd.sources[em.location.source].kind,
            SourceKind::Replacement { .. }
        ));
        assert_eq!(text_at(&dtd, &dtd.anchor(&em.location)), "%decls;");
    }

    #[test]
    fn handles_conditional_sections_in_external_subsets() {
        let dtd = parse(
            "<!ENTITY % draft \"INCLUDE\">\n<!ENTITY % final \"IGNORE\">\n<![%draft;[\n  <!ELEMENT note (#PCDATA)>\n  <![ IGNORE [ <!ELEMENT nested EMPTY> ]]>\n]]>\n<![%final;[ <!ELEMENT hidden EMPTY> <![INCLUDE[ <!ELEMENT deep EMPTY> ]]> ]]>\n<![ INCLUDE [ <!ELEMENT shown EMPTY> ]]>",
        );
        assert!(dtd.problems.is_empty(), "{:?}", dtd.problems);
        assert!(dtd.element("note").is_some());
        assert!(dtd.element("shown").is_some());
        assert!(dtd.element("nested").is_none());
        assert!(dtd.element("hidden").is_none());
        assert!(dtd.element("deep").is_none());

        // Not allowed in the internal subset.
        let document = "<!DOCTYPE r [ <![INCLUDE[ <!ELEMENT r EMPTY> ]]> ]><r/>";
        let (_, dtd) = load_document_dtd(document, None, &mut NoLoader).unwrap();
        assert_eq!(problem_ids(&dtd), vec!["conditionalSection"]);
        assert!(dtd.element("r").is_none());
    }

    #[test]
    fn reports_syntax_errors_and_recovers() {
        let source = "<!ELEMENT a (b, c | d)>\n<!ELEMENT b (#PCDATA | x)>\n<!ELEMENT c EMPTY>\n<!ELEMENT c ANY>\n<!ATTLIST c kind (x|y) \"z\" id ID #IMPLIED key ID \"k\">\n<!ENTITY e>\n<!FOO bar>\nstray text\n<!ENTITY % p \"x\"> %missing;\n<!ELEMENT d (#PCDATA)>\n<!ATTLIST d a BOGUS #IMPLIED>\n<!ENTITY u SYSTEM \"u.bin\" NDATA nowhere>\n<!ELEMENT 1bad EMPTY>\n<!ELEMENT open (x";
        let dtd = parse(source);
        let ids = problem_ids(&dtd);
        assert_eq!(
            ids,
            vec![
                "dtdSyntax",            // mixed , and |
                "dtdSyntax",            // missing * after the mixed model
                "duplicateElement",     // c
                "multipleIdAttributes", // key
                "idAttributeDefault",   // key "k"
                "dtdSyntax",            // <!ENTITY e>
                "dtdSyntax",            // <!FOO
                "dtdSyntax",            // stray text
                "undeclaredParameterEntity",
                "dtdSyntax", // BOGUS
                "dtdSyntax", // 1bad
                "dtdSyntax", // unterminated model
                "dtdSyntax", // unclosed declaration
                "undeclaredNotation",
                "invalidDefaultValue", // "z"
            ],
            "{:#?}",
            dtd.problems
        );
        // Valid declarations stay available.
        assert!(dtd.element("d").is_some());
        // Invalid element declarations are registered (ANY).
        assert_eq!(dtd.element("a").unwrap().content, ContentSpec::Any);
        let mixed = &dtd.problems[0];
        assert_eq!(text_at(&dtd, &mixed.location), "|");
        let stray = &dtd.problems[7];
        assert_eq!(text_at(&dtd, &stray.location), "stray text");
        let undeclared = &dtd.problems[8];
        assert_eq!(text_at(&dtd, &undeclared.location), "%missing;");
    }

    #[test]
    fn detects_parameter_entity_recursion() {
        // `&#37;` becomes `%` in the replacement text: `%b;` references
        // itself once expanded.
        let dtd = parse("<!ENTITY % b \"<!ELEMENT x EMPTY> &#37;b;\">\n%b;");
        assert!(
            problem_ids(&dtd).contains(&"entityRecursion"),
            "{:?}",
            dtd.problems
        );
        assert!(dtd.element("x").is_some());
    }

    #[test]
    fn guards_against_exponential_entity_expansion() {
        let mut source = String::from("<!ENTITY lol0 \"lol\">\n");
        for level in 1..=12 {
            let previous = format!("&lol{};", level - 1).repeat(10);
            source.push_str(&format!("<!ENTITY lol{level} \"{previous}\">\n"));
        }
        source.push_str("<!ENTITY a \"&b;\">\n<!ENTITY b \"x&a;\">\n<!ENTITY self \"&self;\">");
        let dtd = parse(&source);
        let lol4 = dtd.general_entity("lol4").unwrap();
        assert_eq!(lol4.expansion.length, 30_000);
        assert_eq!(lol4.expansion.error, None);
        for name in ["lol6", "lol9", "lol12"] {
            assert_eq!(
                dtd.general_entity(name).unwrap().expansion.error,
                Some(ExpansionError::TooLarge),
                "{name}"
            );
        }
        // The overflow is reported only once, on the first entity.
        let limits = dtd
            .problems
            .iter()
            .filter(|problem| problem.kind == DtdProblemKind::EntityExpansionLimit)
            .collect::<Vec<_>>();
        assert_eq!(limits.len(), 1);
        assert_eq!(text_at(&dtd, &limits[0].location), "lol6");
        for name in ["a", "b", "self"] {
            assert_eq!(
                dtd.general_entity(name).unwrap().expansion.error,
                Some(ExpansionError::Recursive),
                "{name}"
            );
        }
        let recursions = dtd
            .problems
            .iter()
            .filter(|problem| problem.kind == DtdProblemKind::EntityRecursion)
            .map(|problem| text_at(&dtd, &problem.location))
            .collect::<Vec<_>>();
        assert_eq!(recursions, vec!["b", "self"]);

        // Bounded parameter entity expansion.
        let mut source = String::from(
            "<!ENTITY % l0 \"<!-- xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx -->\">\n",
        );
        for level in 1..=8 {
            let previous = format!("%l{};", level - 1).repeat(10);
            source.push_str(&format!("<!ENTITY % l{level} \"{previous}\">\n"));
        }
        source.push_str("%l8;");
        let dtd = parse(&source);
        assert!(problem_ids(&dtd).contains(&"entityExpansionLimit"));
    }

    #[test]
    fn bounds_the_normalization_of_attribute_defaults() {
        let big = "x".repeat(512 * 1024);
        let mut source = format!("<!ENTITY big \"{big}\">\n");
        for index in 0..64 {
            source.push_str(&format!("<!ATTLIST e{index} a CDATA \"&big;\">\n"));
        }
        let dtd = parse(&source);
        let normalized = (0..64)
            .filter(|index| {
                matches!(
                    &dtd.attribute(&format!("e{index}"), "a").unwrap().default,
                    DefaultDecl::Default(value) if value.len() == big.len()
                )
            })
            .count();
        assert_eq!(normalized, MAX_DOCUMENT_EXPANSION / big.len());
        // The others keep their raw value.
        assert_eq!(
            dtd.attribute("e63", "a").unwrap().default,
            DefaultDecl::Default("&big;".to_owned())
        );
    }

    #[test]
    fn bounds_repeated_parameter_entity_references() {
        // Linear ("quadratic blowup") rather than exponential: one large
        // parameter entity referenced many times.
        let comment = format!("<!-- {} -->", "x".repeat(100 * 1024));
        let mut source = format!("<!ENTITY % big \"{comment}\">\n");
        source.push_str(&"%big;".repeat(1000));
        let dtd = parse(&source);
        assert!(problem_ids(&dtd).contains(&"entityExpansionLimit"));
        assert!(dtd.sources.len() <= MAX_SOURCES);
    }

    #[test]
    fn finds_doctype_declarations() {
        let document = "<?xml version=\"1.0\"?>\n<!-- c -->\n<!DOCTYPE note PUBLIC \"-//ACME//Note\" 'note.dtd' [\n  <!ENTITY a \"]\">\n  <!-- ] -->\n]>\n<note/>";
        let doctype = find_doctype(document).unwrap();
        assert_eq!(doctype.name, "note");
        assert_eq!(&document[doctype.name_range.clone()], "note");
        assert_eq!(doctype.public_id.as_ref().unwrap().0, "-//ACME//Note");
        let (system, range) = doctype.system_id.clone().unwrap();
        assert_eq!(system, "note.dtd");
        assert_eq!(&document[range], "note.dtd");
        let subset = doctype.internal_subset.clone().unwrap();
        assert_eq!(&document[subset], "\n  <!ENTITY a \"]\">\n  <!-- ] -->\n");
        assert!(document[doctype.range.clone()].ends_with("]>"));

        let system = find_doctype("<!DOCTYPE html SYSTEM \"about:legacy-compat\"><html/>").unwrap();
        assert_eq!(system.system_id.unwrap().0, "about:legacy-compat");
        assert_eq!(system.internal_subset, None);
        assert!(find_doctype("<root><!DOCTYPE x></root>").is_none());
        assert!(find_doctype("<root/>").is_none());
    }

    #[test]
    fn loads_internal_and_external_subsets_in_order() {
        let mut loader = MemoryLoader::default();
        loader.files.insert(
            "note.dtd".into(),
            "<!ENTITY % body.content \"(#PCDATA)\">\n<!ELEMENT note (to, body)>\n<!ELEMENT to (#PCDATA)>\n<!ELEMENT body %body.content;>\n<!ENTITY % mod SYSTEM \"module.mod\">\n%mod;\n<!ENTITY sig \"external\">".into(),
        );
        loader
            .files
            .insert("module.mod".into(), "<!ELEMENT extra EMPTY>".into());
        let document = "<!DOCTYPE note SYSTEM \"note.dtd\" [\n  <!ENTITY % body.content \"(#PCDATA | b)*\">\n  <!ENTITY sig \"internal\">\n  <!ELEMENT b (#PCDATA)>\n]>\n<note/>";
        let (doctype, dtd) =
            load_document_dtd(document, Some(PathBuf::from("/docs/note.xml")), &mut loader)
                .unwrap();
        assert!(dtd.problems.is_empty(), "{:?}", dtd.problems);
        assert!(!dtd.incomplete);
        assert_eq!(doctype.name, "note");
        assert_eq!(dtd.doctype_name.as_deref(), Some("note"));
        // The internal subset takes precedence (first declaration).
        assert_eq!(
            dtd.element("body").unwrap().content,
            ContentSpec::Mixed(vec!["b".into()])
        );
        assert_eq!(
            dtd.general_entity("sig").unwrap().value,
            EntityValue::Internal("internal".into())
        );
        assert!(dtd.element("extra").is_some());
        let extra = dtd.element("extra").unwrap();
        assert_eq!(
            dtd.source_path(extra.location.source),
            Some(Path::new("/dtd/module.mod"))
        );
        assert_eq!(
            loader.requests[0],
            (
                None,
                "note.dtd".into(),
                Some(PathBuf::from("/docs/note.xml"))
            )
        );
        assert_eq!(loader.requests[1].2, Some(PathBuf::from("/dtd/note.dtd")));
        assert_eq!(dtd.source_path(0), Some(Path::new("/docs/note.xml")));
        // Walking up to the document: the module's element is attached to
        // the system identifier of the DOCTYPE.
        let origin = dtd.origin_in(&extra.location, 0).unwrap();
        assert_eq!(&document[origin.range], "note.dtd");
        assert_eq!(dtd.origin_in(&extra.location, 99), None);
    }

    #[test]
    fn reports_unloadable_external_subsets() {
        let mut loader = MemoryLoader::default();
        let document = "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Strict//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd\"><html/>";
        let (_, dtd) = load_document_dtd(document, None, &mut loader).unwrap();
        assert!(dtd.incomplete);
        assert_eq!(
            dtd.problems[0].kind,
            DtdProblemKind::ExternalLoad { remote: true }
        );
        assert_eq!(
            &document[dtd.problems[0].location.range.clone()],
            "http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd"
        );
        assert_eq!(
            loader.requests[0].0.as_deref(),
            Some("-//W3C//DTD XHTML 1.0 Strict//EN")
        );

        // Reference to an unknown parameter entity after a failure: no
        // additional error (it may come from the missing resource).
        let document = "<!DOCTYPE r [ <!ENTITY % ext SYSTEM \"missing.ent\"> %ext; %later; ]><r/>";
        let (_, dtd) = load_document_dtd(document, None, &mut loader).unwrap();
        assert_eq!(problem_ids(&dtd), vec!["externalLoad"]);
    }
}
