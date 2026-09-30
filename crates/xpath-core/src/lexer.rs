//! Tolerant XPath 1.0/2.0/3.1 tokenizer.
//!
//! Tokens carry byte ranges into the expression. Whitespace and
//! (nested) comments `(: ... :)` are skipped. Names are lexed greedily
//! (`a-b` is one name, as the specification requires); the parser decides
//! whether a name is an operator keyword, an axis, a function or a name
//! test. Problems (unterminated string or comment, unexpected character)
//! become [`TokenKind::Error`] tokens so that the parser reports them at
//! their position.

use std::ops::Range;

/// Kind of a token.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    /// `NCName` or `prefix:local` QName (no whitespace around the colon).
    Name,
    /// `Q{uri}local` (the braced URI literal included).
    BracedName,
    /// `prefix:*`.
    PrefixWildcard,
    /// `*:local`.
    LocalWildcard,
    /// `Q{uri}*`.
    BracedWildcard,
    /// Integer literal.
    Integer,
    /// Decimal literal (`1.5`, `.5`, `5.`).
    Decimal,
    /// Double literal (`1e3`).
    Double,
    /// String literal; the value has its doubled delimiters collapsed.
    String(String),
    Dollar,
    LeftParen,
    RightParen,
    LeftBracket,
    RightBracket,
    LeftBrace,
    RightBrace,
    Comma,
    Dot,
    DotDot,
    At,
    ColonColon,
    Colon,
    Assign,
    Slash,
    SlashSlash,
    Pipe,
    Concat,
    Plus,
    Minus,
    Star,
    Equals,
    NotEquals,
    Less,
    LessEquals,
    Greater,
    GreaterEquals,
    Precedes,
    Follows,
    Bang,
    Arrow,
    Question,
    Hash,
    /// Lexical error, with its message.
    Error(String),
    /// End of the expression.
    End,
}

/// Token with its byte range in the expression.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub range: Range<usize>,
}

/// XML `NameStartChar` without `:`.
pub fn is_ncname_start_char(character: char) -> bool {
    matches!(character,
        'A'..='Z' | '_' | 'a'..='z'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

/// XML `NameChar` without `:`.
pub fn is_ncname_char(character: char) -> bool {
    is_ncname_start_char(character)
        || matches!(character,
            '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

/// `NCName`.
pub fn is_ncname(value: &str) -> bool {
    let mut characters = value.chars();
    characters.next().is_some_and(is_ncname_start_char) && characters.all(is_ncname_char)
}

/// `QName` (`NCName` or `prefix:local`).
pub fn is_qname(value: &str) -> bool {
    match value.split_once(':') {
        Some((prefix, local)) => is_ncname(prefix) && is_ncname(local),
        None => is_ncname(value),
    }
}

/// Splits `expression` into tokens; the last token is always
/// [`TokenKind::End`].
pub fn tokenize(expression: &str) -> Vec<Token> {
    let mut lexer = Lexer {
        text: expression,
        position: 0,
        tokens: Vec::new(),
    };
    lexer.run();
    lexer.tokens
}

struct Lexer<'a> {
    text: &'a str,
    position: usize,
    tokens: Vec<Token>,
}

impl Lexer<'_> {
    fn peek(&self) -> Option<char> {
        self.text[self.position..].chars().next()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.text.get(self.position + offset..)?.chars().next()
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        self.tokens.push(Token {
            kind,
            range: start..self.position,
        });
    }

    fn run(&mut self) {
        loop {
            if let Err(start) = self.skip_trivia() {
                self.push(
                    TokenKind::Error("Unterminated comment: expected ':)'.".to_owned()),
                    start,
                );
                break;
            }
            let start = self.position;
            let Some(character) = self.peek() else {
                break;
            };
            if is_ncname_start_char(character) {
                self.name(start);
                continue;
            }
            if character.is_ascii_digit()
                || (character == '.' && self.peek_at(1).is_some_and(|next| next.is_ascii_digit()))
            {
                self.number(start);
                continue;
            }
            if character == '"' || character == '\'' {
                self.string(start, character);
                continue;
            }
            let two = self
                .text
                .get(self.position..self.position + 2)
                .unwrap_or("");
            let (kind, length) = match two {
                ".." => (TokenKind::DotDot, 2),
                "::" => (TokenKind::ColonColon, 2),
                ":=" => (TokenKind::Assign, 2),
                "//" => (TokenKind::SlashSlash, 2),
                "||" => (TokenKind::Concat, 2),
                "!=" => (TokenKind::NotEquals, 2),
                "<=" => (TokenKind::LessEquals, 2),
                ">=" => (TokenKind::GreaterEquals, 2),
                "<<" => (TokenKind::Precedes, 2),
                ">>" => (TokenKind::Follows, 2),
                "=>" => (TokenKind::Arrow, 2),
                _ => match character {
                    '$' => (TokenKind::Dollar, 1),
                    '(' => (TokenKind::LeftParen, 1),
                    ')' => (TokenKind::RightParen, 1),
                    '[' => (TokenKind::LeftBracket, 1),
                    ']' => (TokenKind::RightBracket, 1),
                    '{' => (TokenKind::LeftBrace, 1),
                    '}' => (TokenKind::RightBrace, 1),
                    ',' => (TokenKind::Comma, 1),
                    '.' => (TokenKind::Dot, 1),
                    '@' => (TokenKind::At, 1),
                    ':' => (TokenKind::Colon, 1),
                    '/' => (TokenKind::Slash, 1),
                    '|' => (TokenKind::Pipe, 1),
                    '+' => (TokenKind::Plus, 1),
                    '-' => (TokenKind::Minus, 1),
                    '=' => (TokenKind::Equals, 1),
                    '<' => (TokenKind::Less, 1),
                    '>' => (TokenKind::Greater, 1),
                    '!' => (TokenKind::Bang, 1),
                    '?' => (TokenKind::Question, 1),
                    '#' => (TokenKind::Hash, 1),
                    '*' => {
                        self.position += 1;
                        if self.peek() == Some(':')
                            && self.peek_at(1).is_some_and(is_ncname_start_char)
                        {
                            self.position += 1;
                            self.skip_ncname();
                            self.push(TokenKind::LocalWildcard, start);
                        } else {
                            self.push(TokenKind::Star, start);
                        }
                        continue;
                    }
                    other => {
                        self.position += other.len_utf8();
                        self.push(
                            TokenKind::Error(format!("Unexpected character '{other}'.")),
                            start,
                        );
                        continue;
                    }
                },
            };
            self.position += length;
            self.push(kind, start);
        }
        let end = self.text.len();
        self.tokens.push(Token {
            kind: TokenKind::End,
            range: end..end,
        });
    }

    /// Skips whitespace and comments; `Err(start)` for an unterminated
    /// comment.
    fn skip_trivia(&mut self) -> Result<(), usize> {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.position += self.peek().map_or(1, char::len_utf8);
            }
            if !self.text[self.position..].starts_with("(:") {
                return Ok(());
            }
            let start = self.position;
            self.position += 2;
            let mut depth = 1;
            while depth > 0 {
                let rest = &self.text[self.position..];
                if rest.is_empty() {
                    return Err(start);
                }
                if rest.starts_with("(:") {
                    depth += 1;
                    self.position += 2;
                } else if rest.starts_with(":)") {
                    depth -= 1;
                    self.position += 2;
                } else {
                    self.position += rest.chars().next().map_or(1, char::len_utf8);
                }
            }
        }
    }

    fn skip_ncname(&mut self) {
        while let Some(character) = self.peek() {
            if !is_ncname_char(character) {
                break;
            }
            self.position += character.len_utf8();
        }
    }

    fn name(&mut self, start: usize) {
        // `Q{uri}local` / `Q{uri}*`.
        if self.text[self.position..].starts_with("Q{") {
            self.position += 2;
            match self.text[self.position..].find(['{', '}']) {
                Some(relative) if self.text.as_bytes()[self.position + relative] == b'}' => {
                    self.position += relative + 1;
                    if self.peek() == Some('*') {
                        self.position += 1;
                        self.push(TokenKind::BracedWildcard, start);
                    } else if self.peek().is_some_and(is_ncname_start_char) {
                        self.skip_ncname();
                        self.push(TokenKind::BracedName, start);
                    } else {
                        self.push(
                            TokenKind::Error(
                                "Expected a local name or '*' after the braced URI literal."
                                    .to_owned(),
                            ),
                            start,
                        );
                    }
                }
                _ => {
                    self.position = self.text.len();
                    self.push(
                        TokenKind::Error("Unterminated braced URI literal: expected '}'.".into()),
                        start,
                    );
                }
            }
            return;
        }
        self.skip_ncname();
        if self.peek() == Some(':') {
            match self.peek_at(1) {
                Some(next) if is_ncname_start_char(next) => {
                    self.position += 1;
                    self.skip_ncname();
                }
                Some('*') => {
                    self.position += 2;
                    self.push(TokenKind::PrefixWildcard, start);
                    return;
                }
                _ => {}
            }
        }
        self.push(TokenKind::Name, start);
    }

    fn skip_digits(&mut self) {
        while self
            .peek()
            .is_some_and(|character| character.is_ascii_digit())
        {
            self.position += 1;
        }
    }

    fn number(&mut self, start: usize) {
        self.skip_digits();
        let mut kind = TokenKind::Integer;
        if self.peek() == Some('.') && self.peek_at(1) != Some('.') {
            self.position += 1;
            self.skip_digits();
            kind = TokenKind::Decimal;
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let mark = self.position;
            self.position += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.position += 1;
            }
            if self
                .peek()
                .is_some_and(|character| character.is_ascii_digit())
            {
                self.skip_digits();
                kind = TokenKind::Double;
            } else {
                self.position = mark;
            }
        }
        if self.peek().is_some_and(is_ncname_start_char) {
            // `10div 3`: XPath 2.0+ requires a separator after a number.
            let name_start = self.position;
            self.skip_ncname();
            self.push(
                TokenKind::Error(format!(
                    "A number must not be followed by a name ('{}'): add a space.",
                    &self.text[name_start..self.position]
                )),
                start,
            );
            return;
        }
        self.push(kind, start);
    }

    fn string(&mut self, start: usize, delimiter: char) {
        self.position += 1;
        let mut value = String::new();
        loop {
            let Some(character) = self.peek() else {
                self.push(
                    TokenKind::Error(format!(
                        "Unterminated string literal: expected {delimiter}."
                    )),
                    start,
                );
                return;
            };
            self.position += character.len_utf8();
            if character == delimiter {
                if self.peek() == Some(delimiter) {
                    self.position += 1;
                    value.push(delimiter);
                    continue;
                }
                self.push(TokenKind::String(value), start);
                return;
            }
            value.push(character);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(expression: &str) -> Vec<TokenKind> {
        tokenize(expression)
            .into_iter()
            .map(|token| token.kind)
            .collect()
    }

    #[test]
    fn lexes_names_wildcards_and_operators() {
        assert_eq!(
            kinds("a-b div *:c | p:* // Q{u}x"),
            vec![
                TokenKind::Name,
                TokenKind::Name,
                TokenKind::LocalWildcard,
                TokenKind::Pipe,
                TokenKind::PrefixWildcard,
                TokenKind::SlashSlash,
                TokenKind::BracedName,
                TokenKind::End,
            ]
        );
        assert_eq!(
            kinds("$x!=1.5e3"),
            vec![
                TokenKind::Dollar,
                TokenKind::Name,
                TokenKind::NotEquals,
                TokenKind::Double,
                TokenKind::End,
            ]
        );
        assert_eq!(
            kinds("f#2 => g() ?a := ||"),
            vec![
                TokenKind::Name,
                TokenKind::Hash,
                TokenKind::Integer,
                TokenKind::Arrow,
                TokenKind::Name,
                TokenKind::LeftParen,
                TokenKind::RightParen,
                TokenKind::Question,
                TokenKind::Name,
                TokenKind::Assign,
                TokenKind::Concat,
                TokenKind::End,
            ]
        );
    }

    #[test]
    fn lexes_strings_numbers_and_comments() {
        let tokens = tokenize("'it''s' (: a (: nested :) comment :) .5 5. 12");
        assert_eq!(tokens[0].kind, TokenKind::String("it's".to_owned()));
        assert_eq!(tokens[1].kind, TokenKind::Decimal);
        assert_eq!(tokens[1].range, 37..39);
        assert_eq!(tokens[2].kind, TokenKind::Decimal);
        assert_eq!(tokens[3].kind, TokenKind::Integer);
    }

    #[test]
    fn reports_lexical_errors_at_their_position() {
        let tokens = tokenize("concat('a, 'b')");
        assert!(
            tokens
                .iter()
                .any(|token| matches!(token.kind, TokenKind::Error(_)))
        );
        let tokens = tokenize("1 (: open");
        assert!(matches!(tokens[1].kind, TokenKind::Error(_)));
        assert_eq!(tokens[1].range.start, 2);
        let tokens = tokenize("a ; b");
        assert_eq!(tokens[1].range, 2..3);
        assert!(matches!(tokens[1].kind, TokenKind::Error(_)));
        let tokens = tokenize("été + ü");
        assert_eq!(tokens[0].kind, TokenKind::Name);
        assert_eq!(tokens[0].range, 0..5);
    }
}
