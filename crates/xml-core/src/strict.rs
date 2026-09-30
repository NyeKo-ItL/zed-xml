//! Strict XML 1.0 (fifth edition) well-formedness check, following the
//! grammar of the recommendation: legal characters ([2]), names ([4]-[5]),
//! comments ([15]), processing instructions ([16]-[17]), CDATA sections
//! ([18]-[21]), the XML declaration ([23]-[32]), the document type
//! declaration ([28]-[30], [75]-[76]), elements and attributes ([39]-[44],
//! [10]) and references ([66]-[68]).
//!
//! The tolerant checks of [`crate::wellformed`] locate the problems an editor
//! meets while typing (unclosed tags, duplicate attributes...); this one
//! catches every other violation of the grammar. It never recurses on the
//! document structure (open elements are kept on a stack) and never reads
//! external resources: the detailed syntax of the markup declarations of a
//! DTD belongs to the DTD parser, only their structure is checked here.

use std::ops::Range;

use crate::names::{is_name_char, is_name_start_char};

/// Maximum number of problems reported.
const MAX_PROBLEMS: usize = 100;

/// Violation of the XML grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictProblem {
    /// Stable identifier (`data.kind` of the diagnostic).
    pub rule: &'static str,
    pub range: Range<usize>,
    pub message: String,
}

/// Whether the XML declaration announces XML 1.1, whose character set also
/// allows the control characters (`#x1`-`#x1F`, `#x7F`-`#x9F`).
fn is_xml_1_1(source: &str) -> bool {
    let source = source.strip_prefix('\u{FEFF}').unwrap_or(source);
    source.starts_with("<?xml")
        && source.find("?>").is_some_and(|end| {
            let declaration = &source[..end];
            declaration.contains("\"1.1\"") || declaration.contains("'1.1'")
        })
}

/// Whether `character` is a legal XML character ([2]).
pub fn is_xml_char(character: char) -> bool {
    matches!(character,
        '\t' | '\n' | '\r'
        | '\u{20}'..='\u{D7FF}'
        | '\u{E000}'..='\u{FFFD}'
        | '\u{10000}'..='\u{10FFFF}')
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

/// Range of the `<!DOCTYPE ...>` declaration of the prolog, when there is
/// one and it is terminated.
pub fn doctype_range(source: &str) -> Option<Range<usize>> {
    let mut parser = Parser::new(source);
    parser.xml_declaration();
    parser.misc();
    let start = parser.position;
    if parser.fatal || !parser.starts_with("<!DOCTYPE") {
        return None;
    }
    parser.doctype();
    (!parser.fatal).then_some(start..parser.position)
}

/// The source with its DOCTYPE declaration blanked out (same length, line
/// breaks kept): general purpose XML readers such as `quick-xml` do not
/// lex quoted literals and comments of a DTD, and the declaration is checked
/// by [`check`] and by the DTD parser instead.
pub fn mask_doctype(source: &str) -> std::borrow::Cow<'_, str> {
    let Some(range) = doctype_range(source) else {
        return std::borrow::Cow::Borrowed(source);
    };
    let mut masked = String::with_capacity(source.len());
    masked.push_str(&source[..range.start]);
    for character in source[range.clone()].chars() {
        if matches!(character, '\n' | '\r') {
            masked.push(character);
        } else {
            masked.extend(std::iter::repeat_n(' ', character.len_utf8()));
        }
    }
    masked.push_str(&source[range.end..]);
    std::borrow::Cow::Owned(masked)
}

/// Why the replacement text of an entity (references already resolved by the
/// declaration) is not well-formed content, when it is not.
pub fn check_replacement_text(text: &str) -> Option<String> {
    check(&format!("<a>{text}</a>"))
        .into_iter()
        .next()
        .map(|problem| problem.message)
}

/// Checks the whole document.
pub fn check(source: &str) -> Vec<StrictProblem> {
    let mut parser = Parser::new(source);
    parser.characters();
    parser.document();
    parser.problems
}

struct Parser<'a> {
    source: &'a str,
    bytes: &'a [u8],
    position: usize,
    problems: Vec<StrictProblem>,
    /// A problem made the rest of the document meaningless.
    fatal: bool,
    /// XML 1.1 document: control characters are legal (as references).
    xml_1_1: bool,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str) -> Self {
        let position = if source.starts_with('\u{FEFF}') { 3 } else { 0 };
        Self {
            source,
            bytes: source.as_bytes(),
            position,
            problems: Vec::new(),
            fatal: false,
            xml_1_1: is_xml_1_1(source),
        }
    }

    fn is_char(&self, character: char) -> bool {
        is_xml_char(character)
            || (self.xml_1_1 && !matches!(character, '\0' | '\u{FFFE}' | '\u{FFFF}'))
    }

    fn report(&mut self, rule: &'static str, range: Range<usize>, message: impl Into<String>) {
        if self.problems.len() < MAX_PROBLEMS {
            self.problems.push(StrictProblem {
                rule,
                range,
                message: message.into(),
            });
        }
    }

    fn fail(&mut self, rule: &'static str, range: Range<usize>, message: impl Into<String>) {
        self.report(rule, range, message);
        self.fatal = true;
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn rest(&self) -> &'a str {
        &self.source[self.position..]
    }

    fn starts_with(&self, text: &str) -> bool {
        self.rest().starts_with(text)
    }

    fn at_end(&self) -> bool {
        self.position >= self.bytes.len()
    }

    fn skip_spaces(&mut self) -> bool {
        let start = self.position;
        while self.peek().is_some_and(is_space) {
            self.position += 1;
        }
        self.position > start
    }

    /// `[3] S`-preceded end of a construct: the character at the position
    /// (or the end of the document) as a range for messages.
    fn here(&self) -> Range<usize> {
        let end = self
            .rest()
            .chars()
            .next()
            .map_or(self.position, |character| {
                self.position + character.len_utf8()
            });
        self.position..end
    }

    fn name(&mut self) -> Option<Range<usize>> {
        let start = self.position;
        let mut characters = self.rest().char_indices();
        match characters.next() {
            Some((_, first)) if is_name_start_char(first) => {}
            _ => return None,
        }
        let end = characters
            .find(|(_, character)| !is_name_char(*character))
            .map_or(self.bytes.len(), |(offset, _)| start + offset);
        self.position = end;
        Some(start..end)
    }

    // -- characters ---------------------------------------------------------

    fn characters(&mut self) {
        for (offset, character) in self.source.char_indices() {
            if !self.is_char(character) {
                self.report(
                    "invalidCharacter",
                    offset..offset + character.len_utf8(),
                    format!(
                        "the character U+{:04X} is not allowed in XML documents",
                        character as u32
                    ),
                );
            }
        }
    }

    // -- document -------------------------------------------------------------

    fn document(&mut self) {
        if self.source.trim().is_empty() {
            return;
        }
        self.xml_declaration();
        self.misc();
        if self.fatal {
            return;
        }
        if self.starts_with("<!DOCTYPE") {
            self.doctype();
            self.misc();
        }
        if self.fatal {
            return;
        }
        if self.peek() != Some(b'<') || self.starts_with("<!") || self.starts_with("<?") {
            if !self.at_end() {
                let range = self.here();
                self.fail(
                    "contentOutsideRoot",
                    range,
                    "the root element is expected here",
                );
            } else {
                self.report(
                    "missingRoot",
                    self.position..self.position,
                    "the document has no root element",
                );
            }
            return;
        }
        self.elements();
        if self.fatal {
            return;
        }
        self.misc();
        if !self.fatal && !self.at_end() {
            let range = self.here();
            self.fail(
                "contentOutsideRoot",
                range,
                "content is not allowed after the root element",
            );
        }
    }

    /// `[23] XMLDecl`.
    fn xml_declaration(&mut self) {
        let start = self.position;
        if !self.starts_with("<?xml") || !self.bytes.get(start + 5).copied().is_some_and(is_space) {
            return;
        }
        self.position += 5;
        let Some(end) = self.rest().find("?>") else {
            self.fail(
                "malformedDeclaration",
                start..start + 5,
                "the XML declaration is not terminated by `?>`",
            );
            return;
        };
        let body_end = self.position + end;
        // version (required), encoding and standalone, in that order.
        let mut seen_version = false;
        let mut seen_encoding = false;
        let mut seen_standalone = false;
        loop {
            let had_space = self.skip_spaces();
            if self.position >= body_end {
                break;
            }
            let attribute_start = self.position;
            let Some(name) = self.name() else {
                self.fail(
                    "malformedDeclaration",
                    self.here(),
                    "unexpected content in the XML declaration",
                );
                return;
            };
            let name_text = &self.source[name.clone()];
            if !had_space {
                self.fail(
                    "malformedDeclaration",
                    name.clone(),
                    "whitespace is required between the pseudo-attributes of the XML declaration",
                );
                return;
            }
            let expected_order_ok = match name_text {
                "version" => !seen_version && !seen_encoding && !seen_standalone,
                "encoding" => seen_version && !seen_encoding && !seen_standalone,
                "standalone" => seen_version && !seen_standalone,
                _ => false,
            };
            if !expected_order_ok {
                self.fail(
                    "malformedDeclaration",
                    name.clone(),
                    format!("unexpected `{name_text}` in the XML declaration (expected version, encoding, standalone in this order)"),
                );
                return;
            }
            self.skip_spaces();
            if self.peek() != Some(b'=') {
                self.fail(
                    "malformedDeclaration",
                    attribute_start..self.position,
                    format!("`=` expected after `{name_text}`"),
                );
                return;
            }
            self.position += 1;
            self.skip_spaces();
            let Some(quote @ (b'"' | b'\'')) = self.peek() else {
                self.fail(
                    "malformedDeclaration",
                    self.here(),
                    format!("the value of `{name_text}` must be quoted"),
                );
                return;
            };
            self.position += 1;
            let value_start = self.position;
            while self.position < body_end && self.peek() != Some(quote) {
                self.position += 1;
            }
            if self.peek() != Some(quote) || self.position >= body_end {
                self.fail(
                    "malformedDeclaration",
                    value_start..self.position,
                    "unterminated value in the XML declaration",
                );
                return;
            }
            let value = &self.source[value_start..self.position];
            self.position += 1;
            let valid = match name_text {
                "version" => {
                    seen_version = true;
                    value.strip_prefix("1.").is_some_and(|minor| {
                        !minor.is_empty() && minor.bytes().all(|byte| byte.is_ascii_digit())
                    })
                }
                "encoding" => {
                    seen_encoding = true;
                    is_encoding_name(value)
                }
                _ => {
                    seen_standalone = true;
                    matches!(value, "yes" | "no")
                }
            };
            if !valid {
                self.report(
                    "malformedDeclaration",
                    value_start..value_start + value.len(),
                    format!("invalid value `{value}` for `{name_text}` in the XML declaration"),
                );
            }
        }
        if !seen_version {
            self.report(
                "malformedDeclaration",
                start..start + 5,
                "the XML declaration requires a version",
            );
        }
        self.position = body_end + 2;
    }

    /// `[27] Misc*` (comments, processing instructions and whitespace). The
    /// XML declaration is only accepted at the start of the document by
    /// [`Self::xml_declaration`].
    fn misc(&mut self) {
        loop {
            self.skip_spaces();
            if self.fatal {
                return;
            }
            if self.starts_with("<!--") {
                self.comment();
            } else if self.starts_with("<?") {
                self.processing_instruction();
            } else {
                return;
            }
        }
    }

    /// `[15] Comment`.
    fn comment(&mut self) {
        let start = self.position;
        let Some(end) = self.source[start + 4..].find("-->") else {
            self.fail(
                "malformedComment",
                start..start + 4,
                "the comment is not terminated by `-->`",
            );
            return;
        };
        let body = &self.source[start + 4..start + 4 + end];
        if let Some(offset) = body.find("--") {
            self.report(
                "malformedComment",
                start + 4 + offset..start + 6 + offset,
                "`--` is not allowed inside a comment",
            );
        } else if body.ends_with('-') {
            self.report(
                "malformedComment",
                start + 3 + end..start + 4 + end,
                "a comment cannot end with `--->`",
            );
        }
        self.position = start + 4 + end + 3;
    }

    /// `[16] PI`.
    fn processing_instruction(&mut self) {
        let start = self.position;
        self.position += 2;
        let Some(target) = self.name() else {
            self.fail(
                "malformedProcessingInstruction",
                start..start + 2,
                "a processing instruction needs a target name",
            );
            return;
        };
        let target_text = &self.source[target.clone()];
        if target_text.contains(':') {
            self.report(
                "invalidName",
                target.clone(),
                "a processing instruction target cannot contain a colon (Namespaces in XML)",
            );
        }
        if target_text.eq_ignore_ascii_case("xml") {
            self.report(
                "malformedProcessingInstruction",
                target.clone(),
                if target_text == "xml" {
                    "the XML declaration is only allowed at the very start of the document"
                } else {
                    "`xml` in any case is reserved and cannot be a processing instruction target"
                },
            );
        }
        match self.peek() {
            Some(byte) if is_space(byte) => {}
            _ if self.starts_with("?>") => {}
            _ => {
                self.fail(
                    "malformedProcessingInstruction",
                    target,
                    "the target must be followed by whitespace or `?>`",
                );
                return;
            }
        }
        match self.rest().find("?>") {
            Some(end) => self.position += end + 2,
            None => self.fail(
                "malformedProcessingInstruction",
                start..start + 2,
                "the processing instruction is not terminated by `?>`",
            ),
        }
    }

    // -- DOCTYPE -----------------------------------------------------------------

    /// `[28] doctypedecl`.
    fn doctype(&mut self) {
        let start = self.position;
        self.position += "<!DOCTYPE".len();
        if !self.skip_spaces() {
            self.fail(
                "malformedDeclaration",
                start..self.position,
                "whitespace is required after `<!DOCTYPE`",
            );
            return;
        }
        if self.name().is_none() {
            self.fail(
                "invalidName",
                self.here(),
                "the DOCTYPE needs the name of the root element",
            );
            return;
        }
        let had_space = self.skip_spaces();
        if self.starts_with("SYSTEM") || self.starts_with("PUBLIC") {
            if !had_space {
                self.fail(
                    "malformedDeclaration",
                    self.here(),
                    "whitespace is required before the external identifier",
                );
                return;
            }
            self.external_id(true);
            if self.fatal {
                return;
            }
            self.skip_spaces();
        }
        if self.peek() == Some(b'[') {
            self.position += 1;
            self.internal_subset();
            if self.fatal {
                return;
            }
            self.skip_spaces();
        }
        if self.peek() == Some(b'>') {
            self.position += 1;
        } else {
            self.fail(
                "malformedDeclaration",
                self.here(),
                "`>` expected at the end of the DOCTYPE declaration",
            );
        }
    }

    /// `[75] ExternalID` (`require_system`: a `PUBLIC` identifier must be
    /// followed by a system literal, as in a DOCTYPE).
    fn external_id(&mut self, require_system: bool) {
        let public = self.starts_with("PUBLIC");
        self.position += 6;
        if !self.skip_spaces() {
            self.fail(
                "malformedDeclaration",
                self.here(),
                "whitespace expected after the external identifier keyword",
            );
            return;
        }
        if public {
            let Some(literal) = self.literal() else {
                self.fail(
                    "malformedDeclaration",
                    self.here(),
                    "the public identifier must be a quoted literal",
                );
                return;
            };
            if let Some((offset, character)) = self.source[literal.clone()]
                .char_indices()
                .find(|(_, character)| !is_pubid_char(*character))
            {
                self.report(
                    "malformedDeclaration",
                    literal.start + offset..literal.start + offset + character.len_utf8(),
                    format!("`{character}` is not allowed in a public identifier"),
                );
            }
            let spaced = self.skip_spaces();
            if !require_system && !matches!(self.peek(), Some(b'"' | b'\'')) {
                return;
            }
            if !spaced {
                self.fail(
                    "malformedDeclaration",
                    self.here(),
                    "whitespace expected before the system literal",
                );
                return;
            }
        }
        if self.literal().is_none() {
            self.fail(
                "malformedDeclaration",
                self.here(),
                "the system identifier must be a quoted literal",
            );
        }
    }

    /// A quoted literal without escapes; returns the range of its content.
    fn literal(&mut self) -> Option<Range<usize>> {
        let quote = match self.peek() {
            Some(quote @ (b'"' | b'\'')) => quote,
            _ => return None,
        };
        let start = self.position + 1;
        let end = start + self.bytes[start..].iter().position(|byte| *byte == quote)?;
        self.position = end + 1;
        Some(start..end)
    }

    /// The internal subset up to and including its closing `]`: markup
    /// declarations, comments, processing instructions, parameter entity
    /// references and whitespace.
    fn internal_subset(&mut self) {
        loop {
            self.skip_spaces();
            let start = self.position;
            match self.peek() {
                None => {
                    self.fail(
                        "malformedDeclaration",
                        start..start,
                        "the internal subset is not closed by `]`",
                    );
                    return;
                }
                Some(b']') => {
                    self.position += 1;
                    return;
                }
                Some(b'%') => {
                    self.position += 1;
                    let ok = self.name().is_some() && self.peek() == Some(b';');
                    if ok {
                        self.position += 1;
                    } else {
                        self.fail(
                            "invalidReference",
                            start..self.position.max(start + 1),
                            "malformed parameter entity reference",
                        );
                        return;
                    }
                }
                Some(b'<') if self.starts_with("<!--") => {
                    self.comment();
                    if self.fatal {
                        return;
                    }
                }
                Some(b'<') if self.starts_with("<?") => {
                    self.processing_instruction();
                    if self.fatal {
                        return;
                    }
                }
                Some(b'<') if self.starts_with("<!") => {
                    let keyword = ["ELEMENT", "ATTLIST", "ENTITY", "NOTATION"]
                        .into_iter()
                        .find(|keyword| self.rest()[2..].starts_with(keyword));
                    if keyword.is_none() {
                        self.fail(
                            "malformedDeclaration",
                            start..start + 2,
                            "an ELEMENT, ATTLIST, ENTITY or NOTATION declaration is expected",
                        );
                        return;
                    }
                    self.skip_declaration();
                    if self.fatal {
                        return;
                    }
                }
                Some(_) => {
                    let range = self.here();
                    self.fail(
                        "malformedDeclaration",
                        range,
                        "a markup declaration is expected in the internal subset",
                    );
                    return;
                }
            }
        }
    }

    /// Skips a markup declaration up to its `>`, respecting quoted literals.
    fn skip_declaration(&mut self) {
        let start = self.position;
        let mut quote: Option<u8> = None;
        while let Some(byte) = self.peek() {
            self.position += 1;
            match (quote, byte) {
                (Some(open), byte) if byte == open => quote = None,
                (None, b'"' | b'\'') => quote = Some(byte),
                (None, b'>') => return,
                _ => {}
            }
        }
        self.fail(
            "malformedDeclaration",
            start..start + 2,
            "the declaration is not terminated by `>`",
        );
    }

    // -- elements ------------------------------------------------------------------

    /// The root element and its content, up to its end tag.
    fn elements(&mut self) {
        let mut open: Vec<Range<usize>> = Vec::new();
        loop {
            if self.fatal {
                return;
            }
            if self.at_end() {
                // The tolerant checks report unclosed elements.
                return;
            }
            if self.peek() == Some(b'<') {
                if self.starts_with("<!--") {
                    self.comment();
                } else if self.starts_with("<![CDATA[") {
                    self.cdata();
                } else if self.starts_with("<?") {
                    self.processing_instruction();
                } else if self.starts_with("</") {
                    self.end_tag(&mut open);
                    if open.is_empty() && !self.fatal {
                        return;
                    }
                } else if self.starts_with("<!") {
                    self.fail(
                        "malformedDeclaration",
                        self.position..self.position + 2,
                        "a declaration is not allowed in element content",
                    );
                } else {
                    self.start_tag(&mut open);
                    if open.is_empty() && !self.fatal {
                        return;
                    }
                }
            } else if self.peek() == Some(b'&') {
                self.reference();
            } else {
                self.char_data();
            }
        }
    }

    /// `[43]` character data up to the next markup or reference.
    fn char_data(&mut self) {
        let start = self.position;
        let end = self
            .rest()
            .find(['<', '&'])
            .map_or(self.bytes.len(), |offset| start + offset);
        if let Some(offset) = self.source[start..end].find("]]>") {
            self.report(
                "malformedCData",
                start + offset..start + offset + 3,
                "`]]>` is not allowed in character data",
            );
        }
        self.position = end;
    }

    /// `[18] CDSect`.
    fn cdata(&mut self) {
        let start = self.position;
        match self.source[start + 9..].find("]]>") {
            Some(end) => self.position = start + 9 + end + 3,
            None => self.fail(
                "malformedCData",
                start..start + 9,
                "the CDATA section is not terminated by `]]>`",
            ),
        }
    }

    /// `[67] Reference`.
    fn reference(&mut self) {
        let start = self.position;
        self.position += 1;
        if self.peek() == Some(b'#') {
            self.position += 1;
            let hexadecimal = self.peek() == Some(b'x');
            if hexadecimal {
                self.position += 1;
            }
            let digits_start = self.position;
            while self.peek().is_some_and(|byte| {
                if hexadecimal {
                    byte.is_ascii_hexdigit()
                } else {
                    byte.is_ascii_digit()
                }
            }) {
                self.position += 1;
            }
            let digits = &self.source[digits_start..self.position];
            if digits.is_empty() || self.peek() != Some(b';') {
                self.report(
                    "invalidReference",
                    start..self.position.max(start + 2),
                    "malformed character reference (expected `&#digits;` or `&#xhex;`)",
                );
                return;
            }
            self.position += 1;
            let value = u32::from_str_radix(digits, if hexadecimal { 16 } else { 10 }).ok();
            if !value
                .and_then(char::from_u32)
                .is_some_and(|character| self.is_char(character))
            {
                self.report(
                    "invalidReference",
                    start..self.position,
                    "the character reference does not designate a legal XML character",
                );
            }
            return;
        }
        if self.name().is_some() && self.peek() == Some(b';') {
            self.position += 1;
        } else {
            self.report(
                "invalidReference",
                start..self.position.max(start + 1),
                "malformed entity reference (expected `&name;`)",
            );
        }
    }

    /// `[40] STag` / `[44] EmptyElemTag`.
    fn start_tag(&mut self, open: &mut Vec<Range<usize>>) {
        let start = self.position;
        self.position += 1;
        let Some(name) = self.name() else {
            self.fail(
                "invalidName",
                start..start + 1 + self.here().len().min(1),
                "an element name is expected after `<`",
            );
            return;
        };
        loop {
            let had_space = self.skip_spaces();
            match self.peek() {
                Some(b'>') => {
                    self.position += 1;
                    open.push(name);
                    return;
                }
                Some(b'/') => {
                    if self.bytes.get(self.position + 1) == Some(&b'>') {
                        self.position += 2;
                        return;
                    }
                    self.fail(
                        "malformedDeclaration",
                        self.here(),
                        "`/` must be followed by `>` in a tag",
                    );
                    return;
                }
                None => {
                    self.fail(
                        "malformedDeclaration",
                        name,
                        "the start tag is not terminated by `>`",
                    );
                    return;
                }
                Some(_) => {
                    if !had_space {
                        self.fail(
                            "malformedDeclaration",
                            self.here(),
                            "whitespace is required between attributes",
                        );
                        return;
                    }
                    if !self.attribute() {
                        return;
                    }
                }
            }
        }
    }

    /// `[41] Attribute`. Returns false after a fatal problem.
    fn attribute(&mut self) -> bool {
        let Some(name) = self.name() else {
            self.fail("invalidName", self.here(), "an attribute name is expected");
            return false;
        };
        self.skip_spaces();
        if self.peek() != Some(b'=') {
            self.fail(
                "malformedDeclaration",
                name,
                "an attribute needs a value (`name=\"value\"`)",
            );
            return false;
        }
        self.position += 1;
        self.skip_spaces();
        let Some(quote @ (b'"' | b'\'')) = self.peek() else {
            self.fail(
                "malformedDeclaration",
                self.here(),
                "the attribute value must be quoted",
            );
            return false;
        };
        self.position += 1;
        loop {
            match self.peek() {
                None => {
                    self.fail(
                        "malformedDeclaration",
                        name,
                        "the attribute value is not terminated",
                    );
                    return false;
                }
                Some(byte) if byte == quote => {
                    self.position += 1;
                    return true;
                }
                Some(b'<') => {
                    self.report(
                        "malformedDeclaration",
                        self.position..self.position + 1,
                        "`<` is not allowed in an attribute value (use `&lt;`)",
                    );
                    self.position += 1;
                }
                Some(b'&') => self.reference(),
                Some(_) => self.position += 1,
            }
        }
    }

    /// `[42] ETag`.
    fn end_tag(&mut self, open: &mut Vec<Range<usize>>) {
        let start = self.position;
        self.position += 2;
        let Some(name) = self.name() else {
            self.fail(
                "invalidName",
                start..start + 2,
                "an element name is expected after `</`",
            );
            return;
        };
        self.skip_spaces();
        if self.peek() != Some(b'>') {
            self.fail(
                "malformedDeclaration",
                self.here(),
                "`>` expected at the end of the end tag",
            );
            return;
        }
        self.position += 1;
        match open.pop() {
            Some(expected) if self.source[expected.clone()] == self.source[name.clone()] => {}
            Some(expected) => self.fail(
                "malformedDeclaration",
                name,
                format!("the end tag does not match <{}>", &self.source[expected]),
            ),
            None => self.fail("malformedDeclaration", name, "unexpected end tag"),
        }
    }
}

/// `[81] EncName`.
fn is_encoding_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// `[13] PubidChar`.
fn is_pubid_char(character: char) -> bool {
    matches!(character, ' ' | '\r' | '\n')
        || character.is_ascii_alphanumeric()
        || "-'()+,./:=?;!*#@$_%".contains(character)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(source: &str) -> Vec<&'static str> {
        check(source).iter().map(|problem| problem.rule).collect()
    }

    #[test]
    fn accepts_well_formed_documents() {
        for source in [
            "<a/>",
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone='yes'?>\n<!-- c --><a x='1' y=\"2\"><b/>text &amp; &#x41; &#65;<![CDATA[ ]] > ]]></a>\n<!-- end -->",
            "<!DOCTYPE a [<!ENTITY e \"a>b\"><!ELEMENT a ANY><?pi x?><!-- c -->%p;]><a>&e;</a>",
            "<!DOCTYPE a SYSTEM \"a.dtd\"><a/>",
            "<!DOCTYPE a PUBLIC \"-//A//B//EN\" 'http://x/y'><a/>",
            "\u{FEFF}<a/>",
            "",
            "   \n",
        ] {
            assert_eq!(check(source), vec![], "{source:?}");
        }
    }

    #[test]
    fn reports_illegal_characters_and_references() {
        assert_eq!(rules("<a>\u{1}</a>"), ["invalidCharacter"]);
        assert_eq!(rules("<a>\u{FFFE}</a>"), ["invalidCharacter"]);
        assert_eq!(rules("<a>&#0;</a>"), ["invalidReference"]);
        assert_eq!(rules("<a>&#xD800;</a>"), ["invalidReference"]);
        assert_eq!(rules("<a>&#x110000;</a>"), ["invalidReference"]);
        assert_eq!(rules("<a>&#;</a>"), ["invalidReference"]);
        assert_eq!(rules("<a>&1;</a>"), ["invalidReference"]);
        assert_eq!(rules("<a x=\"&\"/>"), ["invalidReference"]);
    }

    #[test]
    fn reports_prolog_and_epilog_problems() {
        assert_eq!(rules("<?xml version=\"1.0\"?>"), ["missingRoot"]);
        assert_eq!(rules("text<a/>"), ["contentOutsideRoot"]);
        assert_eq!(rules("<a/>text"), ["contentOutsideRoot"]);
        assert_eq!(rules(" <?xml version=\"1.0\"?><a/>").len(), 1);
        assert_eq!(rules("<!-- c --><?xml version=\"1.0\"?><a/>").len(), 1);
        assert_eq!(rules("<?xml?><a/>"), ["malformedProcessingInstruction"]);
        assert_eq!(
            rules("<?xml version=\"1.0\" standalone=\"maybe\"?><a/>").len(),
            1
        );
        assert_eq!(
            rules("<?xml encoding=\"UTF-8\" version=\"1.0\"?><a/>").len(),
            1
        );
        assert_eq!(rules("<?XML version=\"1.0\"?><a/>").len(), 1);
    }

    #[test]
    fn reports_comment_pi_and_cdata_problems() {
        assert_eq!(rules("<a><!-- a -- b --></a>"), ["malformedComment"]);
        assert_eq!(rules("<a><!-- a ---></a>"), ["malformedComment"]);
        assert_eq!(rules("<a><? x?></a>"), ["malformedProcessingInstruction"]);
        assert_eq!(
            rules("<a><?xml x?></a>"),
            ["malformedProcessingInstruction"]
        );
        assert_eq!(rules("<a>]]></a>"), ["malformedCData"]);
        assert_eq!(rules("<a><![CDATA[x</a>"), ["malformedCData"]);
    }

    #[test]
    fn reports_tag_and_attribute_problems() {
        assert_eq!(rules("<1a/>"), ["invalidName"]);
        assert_eq!(rules("<a 1x=\"1\"/>"), ["invalidName"]);
        assert_eq!(rules("<a x/>"), ["malformedDeclaration"]);
        assert_eq!(rules("<a x=1/>"), ["malformedDeclaration"]);
        assert_eq!(rules("<a x=\"1\"y=\"2\"/>"), ["malformedDeclaration"]);
        assert_eq!(rules("<a x=\"<\"/>"), ["malformedDeclaration"]);
        assert_eq!(rules("<a><!DOCTYPE a></a>"), ["malformedDeclaration"]);
    }

    #[test]
    fn reports_doctype_problems() {
        assert_eq!(rules("<!DOCTYPE><a/>"), ["malformedDeclaration"]);
        assert_eq!(rules("<!DOCTYPE a SYSTEM><a/>"), ["malformedDeclaration"]);
        assert_eq!(
            rules("<!DOCTYPE a PUBLIC \"x\"><a/>"),
            ["malformedDeclaration"]
        );
        assert_eq!(rules("<!DOCTYPE a PUBLIC \"a\u{1}b\" \"s\"><a/>").len(), 2);
        assert_eq!(rules("<!DOCTYPE a [ text ]><a/>"), ["malformedDeclaration"]);
        assert_eq!(
            rules("<!DOCTYPE a [<!ELEMENT a ANY>]><a/>"),
            Vec::<&str>::new()
        );
        assert_eq!(
            rules("<!DOCTYPE a><!DOCTYPE a><a/>"),
            ["contentOutsideRoot"]
        );
        assert_eq!(rules("<a/><!DOCTYPE a>"), ["contentOutsideRoot"]);
    }

    #[test]
    fn finds_the_doctype_even_with_markup_in_literals() {
        let source = "<!DOCTYPE doc [\n<!ENTITY e \"<foo/&#62;\">\n<!-- <x> -->\n]>\n<doc/>";
        let range = doctype_range(source).unwrap();
        assert_eq!(&source[range], &source[..source.find("]>").unwrap() + 2]);
        assert_eq!(doctype_range("<a/>"), None);
    }
}
