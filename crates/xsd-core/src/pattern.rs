//! XML Schema regular expressions (XML Schema 1.0 Part 2, appendix F),
//! translated to the syntax of the `regex` crate.
//!
//! The dialect differs from Perl-like syntaxes:
//!
//! - a pattern matches the whole value (implicit anchoring), and `^`/`$`
//!   are ordinary characters;
//! - `.` matches any character except `\n` and `\r`;
//! - `\i`, `\I`, `\c`, `\C` are the XML name start and name character
//!   classes and their complements; `\s` is `[ \t\n\r]`, `\d` is `\p{Nd}`,
//!   `\w` is everything except punctuation, separators and "other"
//!   characters;
//! - character classes support subtraction (`[a-z-[aeiou]]`);
//! - `\p{IsBlock}` designates a Unicode block (the `regex` crate only knows
//!   scripts and general categories, so blocks are expanded to ranges).
//!
//! Compiled expressions are kept in a process-wide cache keyed by the
//! pattern: validation compiles each pattern once, not once per value.

use std::{
    collections::HashMap,
    fmt::Write as _,
    sync::{Arc, Mutex, OnceLock},
};

use regex::{Regex, RegexBuilder};

/// Maximum number of patterns kept in the cache before it is cleared.
const MAX_CACHED_PATTERNS: usize = 4096;
/// Compiled size limit handed to `regex` (large quantified classes).
const REGEX_SIZE_LIMIT: usize = 32 * 1024 * 1024;

/// XML 1.0 (5th edition) `NameStartChar` ranges, without the colon.
const NAME_START_RANGES: &[(u32, u32)] = &[
    (0x41, 0x5A),
    (0x5F, 0x5F),
    (0x61, 0x7A),
    (0xC0, 0xD6),
    (0xD8, 0xF6),
    (0xF8, 0x2FF),
    (0x370, 0x37D),
    (0x37F, 0x1FFF),
    (0x200C, 0x200D),
    (0x2070, 0x218F),
    (0x2C00, 0x2FEF),
    (0x3001, 0xD7FF),
    (0xF900, 0xFDCF),
    (0xFDF0, 0xFFFD),
    (0x10000, 0xEFFFF),
];

/// Additional `NameChar` ranges.
const NAME_EXTRA_RANGES: &[(u32, u32)] = &[
    (0x2D, 0x2E),
    (0x30, 0x39),
    (0xB7, 0xB7),
    (0x300, 0x36F),
    (0x203F, 0x2040),
];

/// General categories accepted by `\p{..}`.
const CATEGORIES: &[&str] = &[
    "L", "Lu", "Ll", "Lt", "Lm", "Lo", "M", "Mn", "Mc", "Me", "N", "Nd", "Nl", "No", "P", "Pc",
    "Pd", "Ps", "Pe", "Pi", "Pf", "Po", "Z", "Zs", "Zl", "Zp", "S", "Sm", "Sc", "Sk", "So", "C",
    "Cc", "Cf", "Co", "Cn",
];

/// Unicode 3.1 blocks named by XML Schema 1.0 (`\p{IsBasicLatin}`...), with
/// a few later names for the same ranges.
const BLOCKS: &[(&str, &[(u32, u32)])] = &[
    ("BasicLatin", &[(0x0000, 0x007F)]),
    ("Latin-1Supplement", &[(0x0080, 0x00FF)]),
    ("LatinExtended-A", &[(0x0100, 0x017F)]),
    ("LatinExtended-B", &[(0x0180, 0x024F)]),
    ("IPAExtensions", &[(0x0250, 0x02AF)]),
    ("SpacingModifierLetters", &[(0x02B0, 0x02FF)]),
    ("CombiningDiacriticalMarks", &[(0x0300, 0x036F)]),
    ("Greek", &[(0x0370, 0x03FF)]),
    ("GreekandCoptic", &[(0x0370, 0x03FF)]),
    ("Cyrillic", &[(0x0400, 0x04FF)]),
    ("Armenian", &[(0x0530, 0x058F)]),
    ("Hebrew", &[(0x0590, 0x05FF)]),
    ("Arabic", &[(0x0600, 0x06FF)]),
    ("Syriac", &[(0x0700, 0x074F)]),
    ("Thaana", &[(0x0780, 0x07BF)]),
    ("Devanagari", &[(0x0900, 0x097F)]),
    ("Bengali", &[(0x0980, 0x09FF)]),
    ("Gurmukhi", &[(0x0A00, 0x0A7F)]),
    ("Gujarati", &[(0x0A80, 0x0AFF)]),
    ("Oriya", &[(0x0B00, 0x0B7F)]),
    ("Tamil", &[(0x0B80, 0x0BFF)]),
    ("Telugu", &[(0x0C00, 0x0C7F)]),
    ("Kannada", &[(0x0C80, 0x0CFF)]),
    ("Malayalam", &[(0x0D00, 0x0D7F)]),
    ("Sinhala", &[(0x0D80, 0x0DFF)]),
    ("Thai", &[(0x0E00, 0x0E7F)]),
    ("Lao", &[(0x0E80, 0x0EFF)]),
    ("Tibetan", &[(0x0F00, 0x0FFF)]),
    ("Myanmar", &[(0x1000, 0x109F)]),
    ("Georgian", &[(0x10A0, 0x10FF)]),
    ("HangulJamo", &[(0x1100, 0x11FF)]),
    ("Ethiopic", &[(0x1200, 0x137F)]),
    ("Cherokee", &[(0x13A0, 0x13FF)]),
    ("UnifiedCanadianAboriginalSyllabics", &[(0x1400, 0x167F)]),
    ("Ogham", &[(0x1680, 0x169F)]),
    ("Runic", &[(0x16A0, 0x16FF)]),
    ("Khmer", &[(0x1780, 0x17FF)]),
    ("Mongolian", &[(0x1800, 0x18AF)]),
    ("LatinExtendedAdditional", &[(0x1E00, 0x1EFF)]),
    ("GreekExtended", &[(0x1F00, 0x1FFF)]),
    ("GeneralPunctuation", &[(0x2000, 0x206F)]),
    ("SuperscriptsandSubscripts", &[(0x2070, 0x209F)]),
    ("CurrencySymbols", &[(0x20A0, 0x20CF)]),
    ("CombiningMarksforSymbols", &[(0x20D0, 0x20FF)]),
    ("CombiningDiacriticalMarksforSymbols", &[(0x20D0, 0x20FF)]),
    ("LetterlikeSymbols", &[(0x2100, 0x214F)]),
    ("NumberForms", &[(0x2150, 0x218F)]),
    ("Arrows", &[(0x2190, 0x21FF)]),
    ("MathematicalOperators", &[(0x2200, 0x22FF)]),
    ("MiscellaneousTechnical", &[(0x2300, 0x23FF)]),
    ("ControlPictures", &[(0x2400, 0x243F)]),
    ("OpticalCharacterRecognition", &[(0x2440, 0x245F)]),
    ("EnclosedAlphanumerics", &[(0x2460, 0x24FF)]),
    ("BoxDrawing", &[(0x2500, 0x257F)]),
    ("BlockElements", &[(0x2580, 0x259F)]),
    ("GeometricShapes", &[(0x25A0, 0x25FF)]),
    ("MiscellaneousSymbols", &[(0x2600, 0x26FF)]),
    ("Dingbats", &[(0x2700, 0x27BF)]),
    ("BraillePatterns", &[(0x2800, 0x28FF)]),
    ("CJKRadicalsSupplement", &[(0x2E80, 0x2EFF)]),
    ("KangxiRadicals", &[(0x2F00, 0x2FDF)]),
    ("IdeographicDescriptionCharacters", &[(0x2FF0, 0x2FFF)]),
    ("CJKSymbolsandPunctuation", &[(0x3000, 0x303F)]),
    ("Hiragana", &[(0x3040, 0x309F)]),
    ("Katakana", &[(0x30A0, 0x30FF)]),
    ("Bopomofo", &[(0x3100, 0x312F)]),
    ("HangulCompatibilityJamo", &[(0x3130, 0x318F)]),
    ("Kanbun", &[(0x3190, 0x319F)]),
    ("BopomofoExtended", &[(0x31A0, 0x31BF)]),
    ("EnclosedCJKLettersandMonths", &[(0x3200, 0x32FF)]),
    ("CJKCompatibility", &[(0x3300, 0x33FF)]),
    ("CJKUnifiedIdeographsExtensionA", &[(0x3400, 0x4DB5)]),
    ("CJKUnifiedIdeographs", &[(0x4E00, 0x9FFF)]),
    ("YiSyllables", &[(0xA000, 0xA48F)]),
    ("YiRadicals", &[(0xA490, 0xA4CF)]),
    ("HangulSyllables", &[(0xAC00, 0xD7A3)]),
    // Surrogate blocks contain no character.
    ("HighSurrogates", &[]),
    ("HighPrivateUseSurrogates", &[]),
    ("LowSurrogates", &[]),
    (
        "PrivateUse",
        &[(0xE000, 0xF8FF), (0xF0000, 0xFFFFD), (0x100000, 0x10FFFD)],
    ),
    ("PrivateUseArea", &[(0xE000, 0xF8FF)]),
    ("CJKCompatibilityIdeographs", &[(0xF900, 0xFAFF)]),
    ("AlphabeticPresentationForms", &[(0xFB00, 0xFB4F)]),
    ("ArabicPresentationForms-A", &[(0xFB50, 0xFDFF)]),
    ("CombiningHalfMarks", &[(0xFE20, 0xFE2F)]),
    ("CJKCompatibilityForms", &[(0xFE30, 0xFE4F)]),
    ("SmallFormVariants", &[(0xFE50, 0xFE6F)]),
    ("ArabicPresentationForms-B", &[(0xFE70, 0xFEFE)]),
    ("Specials", &[(0xFEFF, 0xFEFF), (0xFFF0, 0xFFFD)]),
    ("HalfwidthandFullwidthForms", &[(0xFF00, 0xFFEF)]),
    ("OldItalic", &[(0x10300, 0x1032F)]),
    ("Gothic", &[(0x10330, 0x1034F)]),
    ("Deseret", &[(0x10400, 0x1044F)]),
    ("ByzantineMusicalSymbols", &[(0x1D000, 0x1D0FF)]),
    ("MusicalSymbols", &[(0x1D100, 0x1D1FF)]),
    ("MathematicalAlphanumericSymbols", &[(0x1D400, 0x1D7FF)]),
    ("CJKUnifiedIdeographsExtensionB", &[(0x20000, 0x2A6D6)]),
    (
        "CJKCompatibilityIdeographsSupplement",
        &[(0x2F800, 0x2FA1F)],
    ),
    ("Tags", &[(0xE0000, 0xE007F)]),
    ("SupplementaryPrivateUseArea-A", &[(0xF0000, 0xFFFFD)]),
    ("SupplementaryPrivateUseArea-B", &[(0x100000, 0x10FFFD)]),
];

/// Translates an XML Schema regular expression to an anchored expression of
/// the `regex` crate, or explains why the pattern is not a valid XML Schema
/// regular expression.
pub fn translate(pattern: &str) -> Result<String, String> {
    let mut parser = Parser {
        chars: pattern.chars().collect(),
        position: 0,
    };
    let body = parser.reg_exp()?;
    if let Some(character) = parser.peek() {
        return Err(format!(
            "unexpected '{character}' at position {}",
            parser.position + 1
        ));
    }
    Ok(format!(r"\A(?:{body})\z"))
}

/// A compiled pattern, or why it cannot be used.
type Compiled = Result<Arc<Regex>, String>;

/// Compiled form of an XML Schema pattern, from the shared cache. `Err`
/// explains why the pattern cannot be used (invalid or unsupported).
pub fn compile(pattern: &str) -> Result<Arc<Regex>, String> {
    static CACHE: OnceLock<Mutex<HashMap<String, Compiled>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Ok(cache) = cache.lock()
        && let Some(compiled) = cache.get(pattern)
    {
        return compiled.clone();
    }
    let compiled = translate(pattern).and_then(|translated| {
        RegexBuilder::new(&translated)
            .size_limit(REGEX_SIZE_LIMIT)
            .build()
            .map(Arc::new)
            .map_err(|error| error.to_string())
    });
    if let Ok(mut cache) = cache.lock() {
        if cache.len() >= MAX_CACHED_PATTERNS {
            cache.clear();
        }
        cache.insert(pattern.to_owned(), compiled.clone());
    }
    compiled
}

/// Whether `value` matches the XML Schema pattern `pattern` (`None` when the
/// pattern cannot be compiled).
pub fn is_match(pattern: &str, value: &str) -> Option<bool> {
    compile(pattern).ok().map(|regex| regex.is_match(value))
}

struct Parser {
    chars: Vec<char>,
    position: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.position).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.position + offset).copied()
    }

    fn next(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.position += 1;
        Some(character)
    }

    fn expect(&mut self, expected: char) -> Result<(), String> {
        match self.next() {
            Some(character) if character == expected => Ok(()),
            Some(character) => Err(format!(
                "expected '{expected}' but found '{character}' at position {}",
                self.position
            )),
            None => Err(format!("expected '{expected}' at the end of the pattern")),
        }
    }

    /// `regExp ::= branch ( '|' branch )*`
    fn reg_exp(&mut self) -> Result<String, String> {
        let mut output = self.branch()?;
        while self.peek() == Some('|') {
            self.position += 1;
            output.push('|');
            output.push_str(&self.branch()?);
        }
        Ok(output)
    }

    /// `branch ::= piece*`
    fn branch(&mut self) -> Result<String, String> {
        let mut output = String::new();
        while let Some(character) = self.peek() {
            if character == '|' || character == ')' {
                break;
            }
            output.push_str(&self.piece()?);
        }
        Ok(output)
    }

    /// `piece ::= atom quantifier?`
    fn piece(&mut self) -> Result<String, String> {
        let atom = self.atom()?;
        let mut output = format!("(?:{atom})");
        match self.peek() {
            Some(quantifier @ ('?' | '*' | '+')) => {
                self.position += 1;
                output.push(quantifier);
            }
            Some('{') => {
                let quantity = self.quantity()?;
                output.push_str(&quantity);
            }
            _ => {}
        }
        if matches!(self.peek(), Some('?' | '*' | '+' | '{')) {
            return Err(format!(
                "a quantifier cannot follow another quantifier at position {}",
                self.position + 1
            ));
        }
        Ok(output)
    }

    /// `quantifier ::= '{' quantity '}'` with `quantity ::= n | n, | n,m`.
    fn quantity(&mut self) -> Result<String, String> {
        self.expect('{')?;
        let minimum = self.digits();
        if minimum.is_empty() {
            return Err(format!(
                "a quantity must start with a number at position {}",
                self.position + 1
            ));
        }
        let mut output = format!("{{{}", trim_number(&minimum));
        if self.peek() == Some(',') {
            self.position += 1;
            output.push(',');
            let maximum = self.digits();
            if !maximum.is_empty() {
                if parse_count(&maximum) < parse_count(&minimum) {
                    return Err(format!("quantity {{{minimum},{maximum}}} is decreasing"));
                }
                output.push_str(trim_number(&maximum));
            }
        }
        self.expect('}')?;
        output.push('}');
        Ok(output)
    }

    fn digits(&mut self) -> String {
        let mut digits = String::new();
        while let Some(character) = self.peek().filter(char::is_ascii_digit) {
            digits.push(character);
            self.position += 1;
        }
        digits
    }

    /// `atom ::= Char | charClass | '(' regExp ')'`
    fn atom(&mut self) -> Result<String, String> {
        let position = self.position + 1;
        match self.next() {
            None => Err("unexpected end of the pattern".to_owned()),
            Some('(') => {
                let inner = self.reg_exp()?;
                self.expect(')')?;
                Ok(inner)
            }
            Some('[') => self.class_expression(),
            Some('.') => Ok(r"[^\n\r]".to_owned()),
            Some('\\') => self.escape(false),
            Some(character @ ('?' | '*' | '+' | '{' | '}' | ')' | ']')) => Err(format!(
                "'{character}' at position {position} must be escaped"
            )),
            Some(character) => Ok(literal(character)),
        }
    }

    /// An escape after `\`: single character escape, multi-character escape
    /// or category escape. `in_class` selects the class item syntax.
    fn escape(&mut self, in_class: bool) -> Result<String, String> {
        let Some(character) = self.next() else {
            return Err("the pattern ends with a lone '\\'".to_owned());
        };
        let class = |items: String, negated: bool| {
            if negated {
                format!("[^{items}]")
            } else if in_class {
                items
            } else {
                format!("[{items}]")
            }
        };
        Ok(match character {
            'n' => literal('\n'),
            'r' => literal('\r'),
            't' => literal('\t'),
            '\\' | '|' | '.' | '-' | '^' | '?' | '*' | '+' | '{' | '}' | '(' | ')' | '[' | ']' => {
                literal(character)
            }
            's' => class(r"\x20\t\n\r".to_owned(), false),
            'S' => class(r"\x20\t\n\r".to_owned(), true),
            'i' => class(name_class(false), false),
            'I' => class(name_class(false), true),
            'c' => class(name_class(true), false),
            'C' => class(name_class(true), true),
            'd' => r"\p{Nd}".to_owned(),
            'D' => r"\P{Nd}".to_owned(),
            'w' => r"[^\p{P}\p{Z}\p{C}]".to_owned(),
            'W' => r"[\p{P}\p{Z}\p{C}]".to_owned(),
            'p' | 'P' => {
                let items = self.category()?;
                let negated = character == 'P';
                match items {
                    Category::General(name) => {
                        format!(r"\{}{{{name}}}", if negated { 'P' } else { 'p' })
                    }
                    Category::Ranges(ranges) => {
                        if ranges.is_empty() {
                            // No character (surrogate blocks): an empty class.
                            if negated {
                                r"[\x{0}-\x{10FFFF}]".to_owned()
                            } else {
                                r"[\x{0}&&\x{1}]".to_owned()
                            }
                        } else {
                            class(ranges_class(ranges), negated)
                        }
                    }
                }
            }
            other => return Err(format!("unknown escape '\\{other}'")),
        })
    }

    /// `\p{..}` name: general category or `Is` block.
    fn category(&mut self) -> Result<Category, String> {
        self.expect('{')?;
        let mut name = String::new();
        loop {
            match self.next() {
                Some('}') => break,
                Some(character) => name.push(character),
                None => return Err("unterminated \\p{...} escape".to_owned()),
            }
        }
        if CATEGORIES.contains(&name.as_str()) {
            return Ok(Category::General(name));
        }
        if let Some(block) = name.strip_prefix("Is") {
            let wanted = block_key(block);
            if let Some((_, ranges)) = BLOCKS
                .iter()
                .find(|(candidate, _)| block_key(candidate) == wanted)
            {
                return Ok(Category::Ranges(ranges));
            }
            return Err(format!("unknown Unicode block '{block}'"));
        }
        Err(format!("unknown character category '{name}'"))
    }

    /// Character class expression after `[`: `^`? items (`-[` subtraction
    /// `]`)? `]`.
    fn class_expression(&mut self) -> Result<String, String> {
        let negated = self.peek() == Some('^');
        if negated {
            self.position += 1;
        }
        let mut items = String::new();
        let mut first = true;
        let mut subtraction = None;
        loop {
            let Some(character) = self.peek() else {
                return Err("unterminated character class".to_owned());
            };
            match character {
                ']' if !first => {
                    self.position += 1;
                    break;
                }
                '-' if self.peek_at(1) == Some('[') && !first => {
                    self.position += 2;
                    subtraction = Some(self.class_expression()?);
                    self.expect(']')?;
                    break;
                }
                '[' => {
                    return Err(format!(
                        "'[' must be escaped in a character class at position {}",
                        self.position + 1
                    ));
                }
                _ => {
                    items.push_str(&self.class_item(first)?);
                }
            }
            first = false;
        }
        if items.is_empty() {
            return Err("empty character class".to_owned());
        }
        let class = if negated {
            format!("[^{items}]")
        } else {
            format!("[{items}]")
        };
        Ok(match subtraction {
            Some(subtracted) => format!("[{class}--{subtracted}]"),
            None => class,
        })
    }

    /// One item of a character class: escape, single character or range.
    fn class_item(&mut self, first: bool) -> Result<String, String> {
        let position = self.position + 1;
        let start = match self.next() {
            None => return Err("unterminated character class".to_owned()),
            Some('\\') => {
                let escape_start = self.position;
                let item = self.escape(true)?;
                // Only single character escapes can start a range.
                match single_char_escape(self.chars[escape_start]) {
                    Some(character) => character,
                    None => return Ok(item),
                }
            }
            Some('-') => {
                // A hyphen is literal at the start or the end of a class.
                let at_end = self.peek() == Some(']');
                if !first && !at_end {
                    return Err(format!(
                        "'-' at position {position} must be escaped in a character class"
                    ));
                }
                '-'
            }
            Some(character) => character,
        };
        if self.peek() == Some('-') && !matches!(self.peek_at(1), Some(']' | '[') | None) {
            self.position += 1;
            let end = match self.next() {
                Some('\\') => {
                    let escape = self.next().unwrap_or('\\');
                    single_char_escape(escape)
                        .ok_or_else(|| format!("'\\{escape}' cannot end a range"))?
                }
                Some(character) => character,
                None => return Err("unterminated character class".to_owned()),
            };
            if end < start {
                return Err(format!("range {start}-{end} is decreasing"));
            }
            return Ok(format!("{}-{}", literal(start), literal(end)));
        }
        Ok(literal(start))
    }
}

enum Category {
    General(String),
    Ranges(&'static [(u32, u32)]),
}

/// Character designated by a single character escape (`\n`, `\-`...).
fn single_char_escape(character: char) -> Option<char> {
    match character {
        'n' => Some('\n'),
        'r' => Some('\r'),
        't' => Some('\t'),
        '\\' | '|' | '.' | '-' | '^' | '?' | '*' | '+' | '{' | '}' | '(' | ')' | '[' | ']' => {
            Some(character)
        }
        _ => None,
    }
}

/// A literal character in `regex` syntax (inside or outside a class).
fn literal(character: char) -> String {
    if character.is_ascii_alphanumeric() {
        character.to_string()
    } else {
        format!(r"\x{{{:X}}}", character as u32)
    }
}

/// Class items of `\i` (name start characters, with `:`) or `\c` (name
/// characters).
fn name_class(name_chars: bool) -> String {
    let mut items = literal(':');
    items.push_str(&ranges_class(NAME_START_RANGES));
    if name_chars {
        items.push_str(&ranges_class(NAME_EXTRA_RANGES));
    }
    items
}

fn ranges_class(ranges: &[(u32, u32)]) -> String {
    let mut items = String::new();
    for (start, end) in ranges {
        if start == end {
            let _ = write!(items, r"\x{{{start:X}}}");
        } else {
            let _ = write!(items, r"\x{{{start:X}}}-\x{{{end:X}}}");
        }
    }
    items
}

/// Block names are compared without spaces, hyphens, underscores or case.
fn block_key(name: &str) -> String {
    name.chars()
        .filter(|character| !matches!(character, ' ' | '-' | '_'))
        .flat_map(char::to_lowercase)
        .collect()
}

fn trim_number(digits: &str) -> &str {
    let trimmed = digits.trim_start_matches('0');
    if trimmed.is_empty() { "0" } else { trimmed }
}

fn parse_count(digits: &str) -> u128 {
    trim_number(digits).parse().unwrap_or(u128::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, value: &str) -> bool {
        is_match(pattern, value).unwrap_or_else(|| panic!("pattern {pattern} should compile"))
    }

    #[test]
    fn anchors_the_whole_value() {
        assert!(matches(r"\d{3}", "123"));
        assert!(!matches(r"\d{3}", "1234"));
        assert!(!matches(r"\d{3}", "a123"));
        assert!(matches("a|b", "b"));
        assert!(!matches("a|b", "ab"));
    }

    #[test]
    fn treats_caret_and_dollar_as_characters() {
        assert!(matches("^a$", "^a$"));
        assert!(!matches("^a$", "a"));
        assert!(matches("[$^]+", "$^"));
    }

    #[test]
    fn dot_excludes_line_breaks() {
        assert!(matches("a.c", "abc"));
        assert!(matches("a.c", "a\tc"));
        assert!(!matches("a.c", "a\nc"));
        assert!(!matches("a.c", "a\rc"));
    }

    #[test]
    fn supports_name_escapes() {
        assert!(matches(r"\i\c*", "abc"));
        assert!(matches(r"\i\c*", "_a-b.c:d"));
        assert!(matches(r"\i\c*", "élément"));
        assert!(!matches(r"\i\c*", "1abc"));
        assert!(!matches(r"\i\c*", "a b"));
        assert!(matches(r"\I", "1"));
        assert!(!matches(r"\I", "a"));
        assert!(matches(r"\C", " "));
        assert!(!matches(r"\C", "-"));
        assert!(matches(r"[\i-[:]][\c-[:]]*", "ncname"));
        assert!(!matches(r"[\i-[:]][\c-[:]]*", "a:b"));
    }

    #[test]
    fn supports_class_subtraction() {
        assert!(matches("[a-z-[aeiou]]+", "bcd"));
        assert!(!matches("[a-z-[aeiou]]+", "abc"));
        assert!(matches("[^a-z-[0-9]]", "A"));
        assert!(!matches("[^a-z-[0-9]]", "5"));
        assert!(matches("[a-z-[b-y-[c]]]+", "acz"));
    }

    #[test]
    fn supports_whitespace_digit_and_word_escapes() {
        assert!(matches(r"\s", " "));
        assert!(!matches(r"\s", "\u{A0}"));
        assert!(matches(r"\S", "\u{A0}"));
        assert!(matches(r"\d+", "٣4"));
        assert!(matches(r"\D", "a"));
        assert!(matches(r"\w+", "abc1"));
        assert!(!matches(r"\w", "!"));
        assert!(matches(r"\W", " "));
    }

    #[test]
    fn supports_categories_and_blocks() {
        assert!(matches(r"\p{Lu}+", "ABC"));
        assert!(!matches(r"\p{Lu}", "a"));
        assert!(matches(r"\P{Lu}", "a"));
        assert!(matches(r"\p{IsBasicLatin}+", "abc"));
        assert!(!matches(r"\p{IsBasicLatin}", "é"));
        assert!(matches(r"\p{IsLatin-1Supplement}", "é"));
        assert!(matches(r"\P{IsBasicLatin}", "é"));
        assert!(matches(r"[\p{IsGreek}a]+", "αa"));
        assert!(!matches(r"\p{IsHighSurrogates}", "a"));
        assert!(matches(r"\P{IsHighSurrogates}", "a"));
        assert!(matches(r"\p{IsPrivateUse}", "\u{E000}"));
    }

    #[test]
    fn escapes_regex_syntax_of_the_target() {
        assert!(matches(r"a&&b", "a&&b"));
        assert!(matches(r"[&~]+", "&~"));
        assert!(matches(r"\-\{\}", "-{}"));
        assert!(matches(r"[\-a]", "-"));
        assert!(matches(r"[-a]", "-"));
        assert!(matches(r"[a-]", "-"));
        assert!(matches(r"[\[\]]", "]"));
        assert!(matches(r"#", "#"));
        assert!(matches(r"[+\-]?\d", "+1"));
    }

    #[test]
    fn supports_quantities() {
        assert!(matches("a{2}", "aa"));
        assert!(!matches("a{2}", "aaa"));
        assert!(matches("a{2,}", "aaaa"));
        assert!(matches("a{1,2}", "a"));
        assert!(matches("a{0,0}b", "b"));
        assert!(matches("(ab){2}", "abab"));
    }

    #[test]
    fn rejects_invalid_patterns() {
        for pattern in [
            "a{2,1}",
            "(a",
            "a)",
            "[a",
            "[]",
            "a**",
            "\\q",
            "\\p{Foo}",
            "\\p{IsFoo}",
            "*a",
            "a{,2}",
            "[a-[b]]x]",
            "[z-a]",
            "[a-b-c]",
        ] {
            assert!(translate(pattern).is_err(), "{pattern} should be rejected");
        }
        assert!(compile("\\").is_err());
    }

    #[test]
    fn caches_compiled_patterns() {
        let first = compile(r"\d+-cache").unwrap();
        let second = compile(r"\d+-cache").unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }
}
