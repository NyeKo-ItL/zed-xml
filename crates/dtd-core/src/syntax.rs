//! Strict syntax of markup declarations (XML 1.0 fifth edition, productions
//! [45]-[47] element declarations and content models, [52]-[60] attribute
//! list declarations, [70]-[76] entity declarations, [82]-[83] notation
//! declarations), checked on the raw text of a declaration, before any
//! parameter entity is expanded: the tolerant parser of [`crate::parser`]
//! reads the expanded text and accepts what it can make sense of (missing
//! whitespace, stray tokens), which a conforming processor must not.

use std::ops::Range;

use xml_core::names::{is_name_char, is_name_start_char};

/// Deepest nesting of a content model group.
const MAX_GROUP_DEPTH: usize = 256;

/// Violation of the grammar in a declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntaxError {
    /// Range in the checked text.
    pub range: Range<usize>,
    pub message: String,
}

/// Checks a whole declaration (`<!ELEMENT ... >` to `>`); `internal`: the
/// declaration is in the internal subset, where parameter entity references
/// are not allowed inside declarations (well-formedness constraint "PEs in
/// Internal Subset"). Declarations containing a parameter entity reference
/// outside an internal subset are not checked (their tokens are only known
/// once expanded).
pub fn check_declaration(text: &str, internal: bool) -> std::result::Result<(), SyntaxError> {
    let mut checker = Checker {
        text,
        bytes: text.as_bytes(),
        position: 2,
    };
    let keyword_end = 2 + text[2..]
        .find(|character: char| !character.is_ascii_uppercase())
        .unwrap_or(text.len() - 2);
    let keyword = &text[2..keyword_end];
    checker.position = keyword_end;
    // `%` of a parameter entity declaration (`<!ENTITY % name ...>`).
    let has_reference = checker.has_parameter_reference(keyword == "ENTITY");
    if has_reference {
        if internal {
            let offset = checker.reference_offset(keyword == "ENTITY").unwrap_or(0);
            return Err(SyntaxError {
                range: offset..offset + 1,
                message:
                    "parameter entity references are not allowed inside a markup declaration of the internal subset"
                        .to_owned(),
            });
        }
        return Ok(());
    }
    match keyword {
        "ELEMENT" => checker.element(),
        "ATTLIST" => checker.attlist(),
        "ENTITY" => checker.entity(),
        "NOTATION" => checker.notation(),
        _ => Ok(()),
    }
}

struct Checker<'a> {
    text: &'a str,
    bytes: &'a [u8],
    position: usize,
}

type Result<T> = std::result::Result<T, SyntaxError>;

impl<'a> Checker<'a> {
    fn error<T>(&self, range: Range<usize>, message: impl Into<String>) -> Result<T> {
        Err(SyntaxError {
            range,
            message: message.into(),
        })
    }

    fn here(&self) -> Range<usize> {
        let end = self.text[self.position.min(self.text.len())..]
            .chars()
            .next()
            .map_or(self.position, |character| {
                self.position + character.len_utf8()
            });
        self.position..end.max(self.position)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn starts_with(&self, prefix: &str) -> bool {
        self.text[self.position.min(self.text.len())..].starts_with(prefix)
    }

    fn has_parameter_reference(&self, entity: bool) -> bool {
        self.reference_offset(entity).is_some()
    }

    /// Offset of the first parameter entity reference (`%name;`) of the
    /// declaration: between its tokens, or in an entity value. Quoted
    /// literals other than entity values are plain text.
    fn reference_offset(&self, entity: bool) -> Option<usize> {
        // An entity declaration with an external identifier has no entity
        // value: its literals are plain text.
        let has_external_id = entity
            && self.text[self.position..]
                .split(['"', '\''])
                .step_by(2)
                .any(|tokens| {
                    tokens
                        .split(|character: char| !character.is_ascii_alphabetic())
                        .any(|word| matches!(word, "SYSTEM" | "PUBLIC"))
                });
        let mut index = self.position;
        let mut quote: Option<u8> = None;
        while index < self.bytes.len() {
            let byte = self.bytes[index];
            match quote {
                Some(open) if byte == open => quote = None,
                None if byte == b'"' || byte == b'\'' => quote = Some(byte),
                _ => {}
            }
            let in_plain_literal = quote.is_some() && (!entity || has_external_id);
            if byte == b'%' && !in_plain_literal {
                let name_start = index + 1;
                let mut end = name_start;
                while let Some(character) = self.text[end..].chars().next() {
                    let allowed = if end == name_start {
                        is_name_start_char(character)
                    } else {
                        is_name_char(character)
                    };
                    if !allowed {
                        break;
                    }
                    end += character.len_utf8();
                }
                // `<!ENTITY % name` has whitespace after its `%`: not a reference.
                if end > name_start && self.bytes.get(end) == Some(&b';') {
                    return Some(index);
                }
            }
            index += 1;
        }
        None
    }

    fn spaces(&mut self) -> bool {
        let start = self.position;
        while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.position += 1;
        }
        self.position > start
    }

    fn require_spaces(&mut self, what: &str) -> Result<()> {
        if self.spaces() {
            Ok(())
        } else {
            self.error(self.here(), format!("whitespace expected {what}"))
        }
    }

    fn name(&mut self) -> Option<Range<usize>> {
        let start = self.position;
        let mut characters = self.text[start..].char_indices();
        match characters.next() {
            Some((_, first)) if is_name_start_char(first) => {}
            _ => return None,
        }
        let end = characters
            .find(|(_, character)| !is_name_char(*character))
            .map_or(self.text.len(), |(offset, _)| start + offset);
        self.position = end;
        Some(start..end)
    }

    fn nmtoken(&mut self) -> Option<Range<usize>> {
        let start = self.position;
        let end = self.text[start..]
            .char_indices()
            .find(|(_, character)| !is_name_char(*character))
            .map_or(self.text.len(), |(offset, _)| start + offset);
        (end > start).then(|| {
            self.position = end;
            start..end
        })
    }

    fn expect_name(&mut self, what: &str) -> Result<Range<usize>> {
        match self.name() {
            Some(name) => Ok(name),
            None => self.error(self.here(), format!("{what} expected")),
        }
    }

    fn keyword(&mut self, keyword: &str) -> bool {
        if self.starts_with(keyword) {
            self.position += keyword.len();
            true
        } else {
            false
        }
    }

    fn end(&mut self) -> Result<()> {
        self.spaces();
        if self.peek() == Some(b'>') && self.position + 1 == self.text.len() {
            Ok(())
        } else {
            self.error(self.here(), "'>' expected at the end of the declaration")
        }
    }

    // -- ELEMENT ---------------------------------------------------------------

    fn element(&mut self) -> Result<()> {
        self.require_spaces("after '<!ELEMENT'")?;
        self.expect_name("the element name")?;
        self.require_spaces("after the element name")?;
        if self.keyword("EMPTY") || self.keyword("ANY") {
            return self.end();
        }
        if self.peek() != Some(b'(') {
            return self.error(self.here(), "EMPTY, ANY or a content model '(' expected");
        }
        // Mixed content?
        let group_start = self.position;
        self.position += 1;
        self.spaces();
        if self.starts_with("#PCDATA") {
            self.position += "#PCDATA".len();
            return self.mixed(group_start);
        }
        self.position = group_start;
        self.group(0)?;
        self.occurrence();
        self.end()
    }

    /// `[51] Mixed` after `(#PCDATA`.
    fn mixed(&mut self, group_start: usize) -> Result<()> {
        let mut names = 0;
        loop {
            self.spaces();
            match self.peek() {
                Some(b'|') => {
                    self.position += 1;
                    self.spaces();
                    self.expect_name("an element name")?;
                    names += 1;
                }
                Some(b')') => {
                    self.position += 1;
                    if names > 0 {
                        if self.peek() != Some(b'*') {
                            return self.error(
                                group_start..self.position,
                                "a mixed content model with element names must end with ')*'",
                            );
                        }
                        self.position += 1;
                    } else if self.peek() == Some(b'*') {
                        self.position += 1;
                    }
                    return self.end();
                }
                _ => return self.error(self.here(), "'|' or ')' expected in mixed content"),
            }
        }
    }

    /// `[49] choice | [50] seq` starting at '('.
    fn group(&mut self, depth: usize) -> Result<()> {
        if depth >= MAX_GROUP_DEPTH {
            return self.error(self.here(), "the content model is nested too deeply");
        }
        let start = self.position;
        self.position += 1;
        self.spaces();
        self.particle(depth)?;
        let mut separator: Option<u8> = None;
        loop {
            self.spaces();
            match self.peek() {
                Some(byte @ (b'|' | b',')) => {
                    match separator {
                        Some(previous) if previous != byte => {
                            return self
                                .error(self.here(), "'|' and ',' cannot be mixed in one group");
                        }
                        _ => separator = Some(byte),
                    }
                    self.position += 1;
                    self.spaces();
                    self.particle(depth)?;
                }
                Some(b')') => {
                    self.position += 1;
                    return Ok(());
                }
                _ => {
                    return self.error(
                        start..self.position.max(start + 1),
                        "'|', ',' or ')' expected in the content model",
                    );
                }
            }
        }
    }

    /// `[48] cp`.
    fn particle(&mut self, depth: usize) -> Result<()> {
        if self.peek() == Some(b'(') {
            self.group(depth + 1)?;
        } else {
            self.expect_name("an element name or '('")?;
        }
        self.occurrence();
        Ok(())
    }

    fn occurrence(&mut self) {
        if matches!(self.peek(), Some(b'?' | b'*' | b'+')) {
            self.position += 1;
        }
    }

    // -- ATTLIST ---------------------------------------------------------------

    fn attlist(&mut self) -> Result<()> {
        self.require_spaces("after '<!ATTLIST'")?;
        self.expect_name("the element name")?;
        loop {
            let had_spaces = self.spaces();
            if self.peek() == Some(b'>') {
                return self.end();
            }
            if !had_spaces {
                return self.error(self.here(), "whitespace expected before the attribute name");
            }
            self.expect_name("an attribute name")?;
            self.require_spaces("after the attribute name")?;
            self.attribute_type()?;
            self.require_spaces("after the attribute type")?;
            self.default_declaration()?;
        }
    }

    fn attribute_type(&mut self) -> Result<()> {
        if self.starts_with("NOTATION") {
            self.position += "NOTATION".len();
            self.require_spaces("after NOTATION")?;
            return self.enumeration(true);
        }
        if self.peek() == Some(b'(') {
            return self.enumeration(false);
        }
        for keyword in [
            "CDATA", "IDREFS", "IDREF", "ID", "ENTITIES", "ENTITY", "NMTOKENS", "NMTOKEN",
        ] {
            if self.starts_with(keyword) {
                let after = self.position + keyword.len();
                let continues = self.text[after..].chars().next().is_some_and(is_name_char);
                if !continues {
                    self.position = after;
                    return Ok(());
                }
            }
        }
        self.error(self.here(), "an attribute type is expected")
    }

    /// `[58] NotationType` / `[59] Enumeration` starting at '('.
    fn enumeration(&mut self, notation: bool) -> Result<()> {
        if self.peek() != Some(b'(') {
            return self.error(self.here(), "'(' expected");
        }
        self.position += 1;
        loop {
            self.spaces();
            if notation {
                self.expect_name("a notation name")?;
            } else if self.nmtoken().is_none() {
                return self.error(self.here(), "a name token expected in the enumeration");
            }
            self.spaces();
            match self.peek() {
                Some(b'|') => self.position += 1,
                Some(b')') => {
                    self.position += 1;
                    return Ok(());
                }
                _ => return self.error(self.here(), "'|' or ')' expected in the enumeration"),
            }
        }
    }

    fn default_declaration(&mut self) -> Result<()> {
        if self.keyword("#REQUIRED") || self.keyword("#IMPLIED") {
            return Ok(());
        }
        if self.keyword("#FIXED") {
            self.require_spaces("after #FIXED")?;
        }
        self.attribute_value()
    }

    /// `[10] AttValue`.
    fn attribute_value(&mut self) -> Result<()> {
        let Some(quote @ (b'"' | b'\'')) = self.peek() else {
            return self.error(
                self.here(),
                "a quoted default value or #REQUIRED/#IMPLIED/#FIXED is expected",
            );
        };
        self.position += 1;
        loop {
            match self.peek() {
                None => return self.error(self.here(), "unterminated literal"),
                Some(byte) if byte == quote => {
                    self.position += 1;
                    return Ok(());
                }
                Some(b'<') => {
                    return self.error(self.here(), "'<' is not allowed in an attribute value");
                }
                Some(b'&') => self.reference()?,
                Some(_) => self.position += 1,
            }
        }
    }

    /// `&name;`, `&#n;`, `&#xh;` in a literal.
    fn reference(&mut self) -> Result<()> {
        let start = self.position;
        self.position += 1;
        if self.peek() == Some(b'#') {
            self.position += 1;
            let hexadecimal = self.peek() == Some(b'x');
            if hexadecimal {
                self.position += 1;
            }
            let digits = self.position;
            while self.peek().is_some_and(|byte| {
                if hexadecimal {
                    byte.is_ascii_hexdigit()
                } else {
                    byte.is_ascii_digit()
                }
            }) {
                self.position += 1;
            }
            if self.position == digits || self.peek() != Some(b';') {
                return self.error(
                    start..self.position.max(start + 2),
                    "malformed character reference",
                );
            }
            self.position += 1;
            return Ok(());
        }
        if self.name().is_some() && self.peek() == Some(b';') {
            self.position += 1;
            Ok(())
        } else {
            self.error(
                start..self.position.max(start + 1),
                "malformed entity reference",
            )
        }
    }

    // -- ENTITY ---------------------------------------------------------------

    fn entity(&mut self) -> Result<()> {
        self.require_spaces("after '<!ENTITY'")?;
        let parameter = self.peek() == Some(b'%');
        if parameter {
            self.position += 1;
            self.require_spaces("after '%'")?;
        }
        let name = self.expect_name("the entity name")?;
        if self.text[name.clone()].contains(':') {
            return self.error(
                name,
                "an entity name cannot contain a colon (Namespaces in XML)",
            );
        }
        self.require_spaces("after the entity name")?;
        match self.peek() {
            Some(b'"' | b'\'') => self.entity_value()?,
            _ => {
                self.external_id()?;
                if !parameter {
                    let before = self.position;
                    if self.spaces() && self.keyword("NDATA") {
                        self.require_spaces("after NDATA")?;
                        self.expect_name("the notation name")?;
                    } else {
                        self.position = before;
                    }
                }
            }
        }
        self.end()
    }

    /// `[9] EntityValue` (no parameter entity references here: they are
    /// rejected beforehand in the internal subset).
    fn entity_value(&mut self) -> Result<()> {
        let quote = self.bytes[self.position];
        self.position += 1;
        loop {
            match self.peek() {
                None => return self.error(self.here(), "unterminated entity value"),
                Some(byte) if byte == quote => {
                    self.position += 1;
                    return Ok(());
                }
                Some(b'&') => self.reference()?,
                Some(b'%') => {
                    return self.error(
                        self.here(),
                        "'%' must start a parameter entity reference (%name;) in an entity value",
                    );
                }
                Some(_) => self.position += 1,
            }
        }
    }

    /// `[75] ExternalID` with both literals for `PUBLIC`.
    fn external_id(&mut self) -> Result<()> {
        if self.keyword("SYSTEM") {
            self.require_spaces("after SYSTEM")?;
            self.system_literal()
        } else if self.keyword("PUBLIC") {
            self.require_spaces("after PUBLIC")?;
            self.pubid_literal()?;
            self.require_spaces("between the public and the system literal")?;
            self.system_literal()
        } else {
            self.error(
                self.here(),
                "SYSTEM, PUBLIC or a quoted entity value expected",
            )
        }
    }

    fn literal(&mut self, what: &str) -> Result<Range<usize>> {
        let Some(quote @ (b'"' | b'\'')) = self.peek() else {
            return self.error(self.here(), format!("{what} must be a quoted literal"));
        };
        let start = self.position + 1;
        match self.bytes[start..].iter().position(|byte| *byte == quote) {
            Some(length) => {
                self.position = start + length + 1;
                Ok(start..start + length)
            }
            None => self.error(self.here(), format!("unterminated {what}")),
        }
    }

    fn system_literal(&mut self) -> Result<()> {
        self.literal("the system identifier").map(|_| ())
    }

    fn pubid_literal(&mut self) -> Result<()> {
        let range = self.literal("the public identifier")?;
        for (offset, character) in self.text[range.clone()].char_indices() {
            let allowed = matches!(character, ' ' | '\r' | '\n')
                || character.is_ascii_alphanumeric()
                || "-'()+,./:=?;!*#@$_%".contains(character);
            if !allowed {
                let start = range.start + offset;
                return self.error(
                    start..start + character.len_utf8(),
                    format!("'{character}' is not allowed in a public identifier"),
                );
            }
        }
        Ok(())
    }

    // -- NOTATION ---------------------------------------------------------------

    fn notation(&mut self) -> Result<()> {
        self.require_spaces("after '<!NOTATION'")?;
        let name = self.expect_name("the notation name")?;
        if self.text[name.clone()].contains(':') {
            return self.error(
                name,
                "a notation name cannot contain a colon (Namespaces in XML)",
            );
        }
        self.require_spaces("after the notation name")?;
        if self.keyword("SYSTEM") {
            self.require_spaces("after SYSTEM")?;
            self.system_literal()?;
        } else if self.keyword("PUBLIC") {
            self.require_spaces("after PUBLIC")?;
            self.pubid_literal()?;
            let before = self.position;
            if self.spaces() && matches!(self.peek(), Some(b'"' | b'\'')) {
                self.system_literal()?;
            } else {
                self.position = before;
            }
        } else {
            return self.error(self.here(), "SYSTEM or PUBLIC expected");
        }
        self.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(text: &str) {
        assert_eq!(check_declaration(text, true), Ok(()), "{text}");
    }

    fn bad(text: &str) {
        assert!(check_declaration(text, true).is_err(), "{text}");
    }

    #[test]
    fn accepts_valid_declarations() {
        ok("<!ELEMENT a EMPTY>");
        ok("<!ELEMENT a ANY >");
        ok("<!ELEMENT a (#PCDATA)>");
        ok("<!ELEMENT a (#PCDATA)*>");
        ok("<!ELEMENT a ( #PCDATA | b | c )*>");
        ok("<!ELEMENT a (b, (c | d)+, e?)*>");
        ok("<!ELEMENT a ((b))>");
        ok("<!ATTLIST a x CDATA #IMPLIED y (p|q) 'p' z NOTATION (n) #FIXED \"n\" i ID #REQUIRED>");
        ok("<!ATTLIST a>");
        ok("<!ENTITY e \"text &amp; &#65; &other;\">");
        ok("<!ENTITY % p 'v'>");
        ok("<!ENTITY e SYSTEM 'x.ent'>");
        ok("<!ENTITY e PUBLIC \"-//A//B\" \"x.ent\" NDATA gif>");
        ok("<!NOTATION n SYSTEM 'x'>");
        ok("<!NOTATION n PUBLIC \"-//A//B\">");
        ok("<!NOTATION n PUBLIC \"-//A//B\" 'x'>");
    }

    #[test]
    fn rejects_missing_whitespace_and_stray_tokens() {
        bad("<!ELEMENT a(#PCDATA)>");
        bad("<!ELEMENT root ((root) ?)>");
        bad("<!ELEMENT root (root +)>");
        bad("<!ELEMENT a (b | c, d)>");
        bad("<!ELEMENT a (#PCDATA | b)>");
        bad("<!ELEMENT a>");
        bad("<!ATTLIST a x (p|q)\"p\">");
        bad("<!ATTLIST a x(p|q) \"p\">");
        bad("<!ATTLIST a x NOTATION(n) #IMPLIED>");
        bad("<!ATTLIST a x CDATA #FIXED\"v\">");
        bad("<!ATTLIST a x CDATA>");
        bad("<!ENTITY e\"v\">");
        bad("<!ENTITY% p \"v\">");
        bad("<!ENTITY e \"a&b\">");
        bad("<!ENTITY e \"a%b\">");
        bad("<!ENTITY e \"&49;\">");
        bad("<!ENTITY e PUBLIC \"x\">");
        bad("<!ENTITY e PUBLIC \"w\"\"e.ent\">");
        bad("<!ENTITY e PUBLIC \"{x}\" \"e.ent\">");
        bad("<!ENTITY % p SYSTEM 'x' NDATA n>");
        bad("<!NOTATION n>");
        bad("<!NOTATION n PUBLIC \"<\">");
    }

    #[test]
    fn parameter_references_depend_on_the_subset() {
        let text = "<!ATTLIST a %p; CDATA #IMPLIED>";
        assert!(check_declaration(text, true).is_err());
        assert_eq!(check_declaration(text, false), Ok(()));
        assert!(check_declaration("<!ENTITY c \"%p;\">", true).is_err());
    }

    #[test]
    fn literals_other_than_entity_values_are_plain_text() {
        // `%e;` in an attribute default or a system identifier is no reference.
        ok("<!ATTLIST a x CDATA \"%e;\">");
        ok("<!ENTITY e SYSTEM \"%x;.ent\">");
        ok("<!NOTATION n SYSTEM \"a%b;\">");
        assert!(check_declaration("<!ENTITY e \"%p;\">", true).is_err());
    }

    #[test]
    fn bounds_the_nesting_of_groups() {
        let deep = format!("<!ELEMENT a {}b{}>", "(".repeat(1000), ")".repeat(1000));
        assert!(check_declaration(&deep, true).is_err());
    }
}
