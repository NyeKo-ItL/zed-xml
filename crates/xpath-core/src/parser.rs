//! Recursive-descent XPath 3.1 parser (accepting XPath 1.0 and 2.0).
//!
//! The parser does not build a tree: it checks the syntax, stops at the
//! first error (like XSLT processors) and records what editor features
//! need: variable references resolved against the range variables of the
//! expression (`for`, `let`, `some`, `every`, inline function
//! parameters), and function calls with their arity and string literal
//! arguments (`key('name', ...)`).

use std::ops::Range;

use crate::lexer::{Token, TokenKind, tokenize};

/// Syntax error with its byte range in the expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XPathError {
    pub message: String,
    pub range: Range<usize>,
}

/// Construct that binds a variable inside an expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingKind {
    For,
    Let,
    Some,
    Every,
    /// Parameter of an inline function.
    Parameter,
}

/// Variable bound inside the expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariableBinding {
    /// Name as written (`x`, `p:x` or `Q{uri}x`).
    pub name: String,
    /// Range of the name (without `$`).
    pub range: Range<usize>,
    pub kind: BindingKind,
}

/// `$name` reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariableReference {
    /// Name as written.
    pub name: String,
    /// Range of the name (without `$`).
    pub range: Range<usize>,
    /// Index in [`XPathAnalysis::bindings`] of the range variable it
    /// refers to; `None` for a variable of the context (XSLT variable or
    /// parameter).
    pub binding: Option<usize>,
}

/// Argument of a function call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Argument {
    pub range: Range<usize>,
    /// The argument is a single string literal: its value and the range of
    /// its content (quotes excluded).
    pub string_literal: Option<(String, Range<usize>)>,
}

/// Static function call `name(...)`, arrow call `=> name(...)` or named
/// function reference `name#arity`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionCall {
    /// Name as written.
    pub name: String,
    pub range: Range<usize>,
    pub arity: usize,
    /// Explicit arguments (without the arrow's left operand).
    pub arguments: Vec<Argument>,
    /// `name#arity` rather than a call.
    pub reference: bool,
}

/// Result of [`parse`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XPathAnalysis {
    /// At most one error: parsing stops at the first one.
    pub errors: Vec<XPathError>,
    pub bindings: Vec<VariableBinding>,
    pub variables: Vec<VariableReference>,
    pub functions: Vec<FunctionCall>,
}

impl XPathAnalysis {
    /// Moves every range by `offset` (expression embedded in a larger
    /// text).
    pub fn shift(&mut self, offset: usize) {
        let shift = |range: &mut Range<usize>| {
            range.start += offset;
            range.end += offset;
        };
        self.errors
            .iter_mut()
            .for_each(|error| shift(&mut error.range));
        self.bindings
            .iter_mut()
            .for_each(|binding| shift(&mut binding.range));
        self.variables
            .iter_mut()
            .for_each(|variable| shift(&mut variable.range));
        for function in &mut self.functions {
            shift(&mut function.range);
            for argument in &mut function.arguments {
                shift(&mut argument.range);
                if let Some((_, range)) = &mut argument.string_literal {
                    shift(range);
                }
            }
        }
    }

    /// Appends `other`, renumbering its bindings.
    pub fn extend(&mut self, other: XPathAnalysis) {
        let base = self.bindings.len();
        self.errors.extend(other.errors);
        self.bindings.extend(other.bindings);
        self.variables
            .extend(other.variables.into_iter().map(|mut variable| {
                variable.binding = variable.binding.map(|index| index + base);
                variable
            }));
        self.functions.extend(other.functions);
    }

    /// Range variables in scope at `offset` (used by completion), innermost
    /// last. Approximation: bindings declared before `offset` whose
    /// references all occur after them.
    pub fn bindings_before(&self, offset: usize) -> impl Iterator<Item = &VariableBinding> {
        self.bindings
            .iter()
            .filter(move |binding| binding.range.end <= offset)
    }
}

/// Maximum nesting of expressions, to keep the recursion bounded.
const MAX_DEPTH: usize = 200;

const AXES: [&str; 13] = [
    "ancestor",
    "ancestor-or-self",
    "attribute",
    "child",
    "descendant",
    "descendant-or-self",
    "following",
    "following-sibling",
    "namespace",
    "parent",
    "preceding",
    "preceding-sibling",
    "self",
];

const KIND_TESTS: [&str; 10] = [
    "attribute",
    "comment",
    "document-node",
    "element",
    "namespace-node",
    "node",
    "processing-instruction",
    "schema-attribute",
    "schema-element",
    "text",
];

/// Parses the XPath expression `expression`.
pub fn parse(expression: &str) -> XPathAnalysis {
    let tokens = tokenize(expression);
    let mut parser = Parser {
        text: expression,
        tokens,
        index: 0,
        depth: 0,
        scopes: Vec::new(),
        analysis: XPathAnalysis::default(),
    };
    if parser.tokens[0].kind == TokenKind::End {
        parser.analysis.errors.push(XPathError {
            message: "Empty XPath expression.".to_owned(),
            range: 0..expression.len(),
        });
        return parser.analysis;
    }
    if parser.expr().is_ok() && parser.kind() != &TokenKind::End {
        let message = format!(
            "Unexpected {} after the end of the expression.",
            parser.describe_current()
        );
        let range = parser.current().range.clone();
        parser.analysis.errors.push(XPathError { message, range });
    }
    parser.analysis
}

/// Parses `text[range]` and returns ranges relative to `text`.
pub fn parse_range(text: &str, range: Range<usize>) -> XPathAnalysis {
    let mut analysis = parse(&text[range.clone()]);
    analysis.shift(range.start);
    analysis
}

/// Parsing stopped on an error (already recorded).
struct Stop;

type Parsed = Result<(), Stop>;

struct Parser<'a> {
    text: &'a str,
    tokens: Vec<Token>,
    index: usize,
    depth: usize,
    /// Range variables in scope: indices into `analysis.bindings`.
    scopes: Vec<usize>,
    analysis: XPathAnalysis,
}

impl Parser<'_> {
    fn current(&self) -> &Token {
        &self.tokens[self.index]
    }

    fn kind(&self) -> &TokenKind {
        &self.tokens[self.index].kind
    }

    fn kind_at(&self, ahead: usize) -> &TokenKind {
        let index = (self.index + ahead).min(self.tokens.len() - 1);
        &self.tokens[index].kind
    }

    fn token_text(&self, index: usize) -> &str {
        let range = self.tokens[index.min(self.tokens.len() - 1)].range.clone();
        &self.text[range]
    }

    fn current_text(&self) -> &str {
        self.token_text(self.index)
    }

    /// The current token is the unprefixed name `name`.
    fn at_name(&self, name: &str) -> bool {
        self.kind() == &TokenKind::Name && self.current_text() == name
    }

    fn name_at(&self, ahead: usize, name: &str) -> bool {
        self.kind_at(ahead) == &TokenKind::Name && self.token_text(self.index + ahead) == name
    }

    fn advance(&mut self) {
        if self.index + 1 < self.tokens.len() {
            self.index += 1;
        }
    }

    fn describe_current(&self) -> String {
        match self.kind() {
            TokenKind::End => "end of the expression".to_owned(),
            TokenKind::Error(message) => message.clone(),
            _ => format!("'{}'", self.current_text()),
        }
    }

    fn fail(&mut self, message: String) -> Parsed {
        let token = self.current();
        let range = match (&token.kind, self.index) {
            // At the end, point at the last token.
            (TokenKind::End, index) if index > 0 => self.tokens[index - 1].range.clone(),
            _ => token.range.clone(),
        };
        let message = match &token.kind {
            TokenKind::Error(lexical) => lexical.clone(),
            _ => message,
        };
        self.analysis.errors.push(XPathError { message, range });
        Err(Stop)
    }

    fn expected(&mut self, what: &str) -> Parsed {
        let found = match self.kind() {
            TokenKind::End => "reached the end of the expression".to_owned(),
            _ => format!("found {}", self.describe_current()),
        };
        self.fail(format!("Expected {what} but {found}."))
    }

    fn expect(&mut self, kind: TokenKind, what: &str) -> Parsed {
        if self.kind() == &kind {
            self.advance();
            Ok(())
        } else {
            self.expected(what)
        }
    }

    fn expect_name(&mut self, name: &str) -> Parsed {
        if self.at_name(name) {
            self.advance();
            Ok(())
        } else {
            self.expected(&format!("'{name}'"))
        }
    }

    fn enter(&mut self) -> Parsed {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.fail("Expression nested too deeply.".to_owned());
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn is_eqname(&self) -> bool {
        matches!(self.kind(), TokenKind::Name | TokenKind::BracedName)
    }

    // ------------------------------------------------------------------
    // Expressions
    // ------------------------------------------------------------------

    fn expr(&mut self) -> Parsed {
        self.expr_single()?;
        while self.kind() == &TokenKind::Comma {
            self.advance();
            self.expr_single()?;
        }
        Ok(())
    }

    fn expr_single(&mut self) -> Parsed {
        self.enter()?;
        let result = self.expr_single_inner();
        self.leave();
        result
    }

    fn expr_single_inner(&mut self) -> Parsed {
        if self.kind() == &TokenKind::Name && self.kind_at(1) == &TokenKind::Dollar {
            let keyword = self.current_text();
            let kind = match keyword {
                "for" => Some(BindingKind::For),
                "let" => Some(BindingKind::Let),
                "some" => Some(BindingKind::Some),
                "every" => Some(BindingKind::Every),
                _ => None,
            };
            if let Some(kind) = kind {
                return self.binding_expr(kind);
            }
        }
        if self.at_name("if") && self.kind_at(1) == &TokenKind::LeftParen {
            self.advance();
            self.advance();
            self.expr()?;
            self.expect(TokenKind::RightParen, "')'")?;
            self.expect_name("then")?;
            self.expr_single()?;
            self.expect_name("else")?;
            return self.expr_single();
        }
        self.or_expr()
    }

    fn binding_expr(&mut self, kind: BindingKind) -> Parsed {
        self.advance();
        let mark = self.scopes.len();
        loop {
            self.expect(TokenKind::Dollar, "'$'")?;
            if !self.is_eqname() {
                return self.expected("a variable name");
            }
            let range = self.current().range.clone();
            let name = self.current_text().to_owned();
            self.advance();
            if kind == BindingKind::Let {
                self.expect(TokenKind::Assign, "':='")?;
            } else {
                self.expect_name("in")?;
            }
            self.expr_single()?;
            self.analysis
                .bindings
                .push(VariableBinding { name, range, kind });
            self.scopes.push(self.analysis.bindings.len() - 1);
            if self.kind() == &TokenKind::Comma && self.kind_at(1) == &TokenKind::Dollar {
                self.advance();
                continue;
            }
            break;
        }
        let keyword = match kind {
            BindingKind::For | BindingKind::Let => "return",
            _ => "satisfies",
        };
        self.expect_name(keyword)?;
        let result = self.expr_single();
        self.scopes.truncate(mark);
        result
    }

    fn or_expr(&mut self) -> Parsed {
        self.and_expr()?;
        while self.at_name("or") {
            self.advance();
            self.and_expr()?;
        }
        Ok(())
    }

    fn and_expr(&mut self) -> Parsed {
        self.comparison_expr()?;
        while self.at_name("and") {
            self.advance();
            self.comparison_expr()?;
        }
        Ok(())
    }

    fn comparison_expr(&mut self) -> Parsed {
        self.concat_expr()?;
        // XPath 1.0 chains comparisons (`a = b = c`): accepted.
        loop {
            let operator = matches!(
                self.kind(),
                TokenKind::Equals
                    | TokenKind::NotEquals
                    | TokenKind::Less
                    | TokenKind::LessEquals
                    | TokenKind::Greater
                    | TokenKind::GreaterEquals
                    | TokenKind::Precedes
                    | TokenKind::Follows
            ) || (self.kind() == &TokenKind::Name
                && matches!(
                    self.current_text(),
                    "eq" | "ne" | "lt" | "le" | "gt" | "ge" | "is"
                ));
            if !operator {
                return Ok(());
            }
            self.advance();
            self.concat_expr()?;
        }
    }

    fn concat_expr(&mut self) -> Parsed {
        self.range_expr()?;
        while self.kind() == &TokenKind::Concat {
            self.advance();
            self.range_expr()?;
        }
        Ok(())
    }

    fn range_expr(&mut self) -> Parsed {
        self.additive_expr()?;
        if self.at_name("to") {
            self.advance();
            self.additive_expr()?;
        }
        Ok(())
    }

    fn additive_expr(&mut self) -> Parsed {
        self.multiplicative_expr()?;
        while matches!(self.kind(), TokenKind::Plus | TokenKind::Minus) {
            self.advance();
            self.multiplicative_expr()?;
        }
        Ok(())
    }

    fn multiplicative_expr(&mut self) -> Parsed {
        self.union_expr()?;
        while self.kind() == &TokenKind::Star
            || (self.kind() == &TokenKind::Name
                && matches!(self.current_text(), "div" | "idiv" | "mod"))
        {
            self.advance();
            self.union_expr()?;
        }
        Ok(())
    }

    fn union_expr(&mut self) -> Parsed {
        self.intersect_expr()?;
        while self.kind() == &TokenKind::Pipe || self.at_name("union") {
            self.advance();
            self.intersect_expr()?;
        }
        Ok(())
    }

    fn intersect_expr(&mut self) -> Parsed {
        self.type_expr()?;
        while self.at_name("intersect") || self.at_name("except") {
            self.advance();
            self.type_expr()?;
        }
        Ok(())
    }

    /// `instance of`, `treat as`, `castable as`, `cast as`.
    fn type_expr(&mut self) -> Parsed {
        self.arrow_expr()?;
        if self.at_name("cast") && self.name_at(1, "as") {
            self.advance();
            self.advance();
            self.single_type()?;
        }
        if self.at_name("castable") && self.name_at(1, "as") {
            self.advance();
            self.advance();
            self.single_type()?;
        }
        if self.at_name("treat") && self.name_at(1, "as") {
            self.advance();
            self.advance();
            self.sequence_type()?;
        }
        if self.at_name("instance") && self.name_at(1, "of") {
            self.advance();
            self.advance();
            self.sequence_type()?;
        }
        Ok(())
    }

    fn arrow_expr(&mut self) -> Parsed {
        self.unary_expr()?;
        while self.kind() == &TokenKind::Arrow {
            self.advance();
            match self.kind() {
                TokenKind::Name | TokenKind::BracedName => {
                    let range = self.current().range.clone();
                    let name = self.current_text().to_owned();
                    self.advance();
                    let arguments = self.argument_list()?;
                    self.analysis.functions.push(FunctionCall {
                        name,
                        range,
                        arity: arguments.len() + 1,
                        arguments,
                        reference: false,
                    });
                }
                TokenKind::Dollar => {
                    self.variable_reference()?;
                    self.argument_list()?;
                }
                TokenKind::LeftParen => {
                    self.parenthesized()?;
                    self.argument_list()?;
                }
                _ => return self.expected("a function name, variable or '(' after '=>'"),
            }
        }
        Ok(())
    }

    fn unary_expr(&mut self) -> Parsed {
        while matches!(self.kind(), TokenKind::Plus | TokenKind::Minus) {
            self.advance();
        }
        self.simple_map_expr()
    }

    fn simple_map_expr(&mut self) -> Parsed {
        self.path_expr()?;
        while self.kind() == &TokenKind::Bang {
            self.advance();
            self.path_expr()?;
        }
        Ok(())
    }

    fn path_expr(&mut self) -> Parsed {
        match self.kind() {
            TokenKind::Slash => {
                self.advance();
                if self.starts_step() {
                    self.relative_path()?;
                }
                Ok(())
            }
            TokenKind::SlashSlash => {
                self.advance();
                self.relative_path()
            }
            _ => self.relative_path(),
        }
    }

    /// The current token can start a step (after a leading `/`).
    fn starts_step(&self) -> bool {
        match self.kind() {
            // As the specifications require, `/div` is a path even though
            // `div` is also an operator.
            TokenKind::Name => true,
            TokenKind::BracedName
            | TokenKind::PrefixWildcard
            | TokenKind::LocalWildcard
            | TokenKind::BracedWildcard
            | TokenKind::Star
            | TokenKind::At
            | TokenKind::Dot
            | TokenKind::DotDot
            | TokenKind::Dollar
            | TokenKind::LeftParen
            | TokenKind::LeftBracket
            | TokenKind::String(_)
            | TokenKind::Integer
            | TokenKind::Decimal
            | TokenKind::Double => true,
            _ => false,
        }
    }

    fn relative_path(&mut self) -> Parsed {
        self.step()?;
        while matches!(self.kind(), TokenKind::Slash | TokenKind::SlashSlash) {
            self.advance();
            self.step()?;
        }
        Ok(())
    }

    fn step(&mut self) -> Parsed {
        self.enter()?;
        let result = self.step_inner();
        self.leave();
        result
    }

    fn step_inner(&mut self) -> Parsed {
        match self.kind().clone() {
            TokenKind::DotDot => {
                self.advance();
                self.predicates()
            }
            TokenKind::At => {
                self.advance();
                self.node_test()?;
                self.predicates()
            }
            TokenKind::Name if self.kind_at(1) == &TokenKind::ColonColon => {
                let axis = self.current_text().to_owned();
                if !AXES.contains(&axis.as_str()) {
                    return self.fail(format!("Unknown axis '{axis}'."));
                }
                self.advance();
                self.advance();
                self.node_test()?;
                self.predicates()
            }
            TokenKind::Name | TokenKind::BracedName if self.kind_at(1) == &TokenKind::LeftParen => {
                let name = self.current_text().to_owned();
                if self.kind() == &TokenKind::Name && KIND_TESTS.contains(&name.as_str()) {
                    self.kind_test()?;
                    return self.predicates();
                }
                if self.kind() == &TokenKind::Name && name == "function" {
                    self.inline_function()?;
                    return self.postfix();
                }
                let range = self.current().range.clone();
                self.advance();
                let arguments = self.argument_list()?;
                self.analysis.functions.push(FunctionCall {
                    name,
                    range,
                    arity: arguments.len(),
                    arguments,
                    reference: false,
                });
                self.postfix()
            }
            TokenKind::Name | TokenKind::BracedName if self.kind_at(1) == &TokenKind::Hash => {
                let name = self.current_text().to_owned();
                let range = self.current().range.clone();
                self.advance();
                self.advance();
                if self.kind() != &TokenKind::Integer {
                    return self.expected("an arity after '#'");
                }
                let arity = self.current_text().parse().unwrap_or(0);
                self.advance();
                self.analysis.functions.push(FunctionCall {
                    name,
                    range,
                    arity,
                    arguments: Vec::new(),
                    reference: true,
                });
                self.postfix()
            }
            TokenKind::Name
                if matches!(self.current_text(), "map" | "array")
                    && self.kind_at(1) == &TokenKind::LeftBrace =>
            {
                if self.current_text() == "map" {
                    self.map_constructor()?;
                } else {
                    self.advance();
                    self.advance();
                    if self.kind() != &TokenKind::RightBrace {
                        self.expr()?;
                    }
                    self.expect(TokenKind::RightBrace, "'}'")?;
                }
                self.postfix()
            }
            TokenKind::Name
            | TokenKind::BracedName
            | TokenKind::PrefixWildcard
            | TokenKind::LocalWildcard
            | TokenKind::BracedWildcard
            | TokenKind::Star => {
                self.advance();
                self.predicates()
            }
            TokenKind::Dot
            | TokenKind::String(_)
            | TokenKind::Integer
            | TokenKind::Decimal
            | TokenKind::Double => {
                self.advance();
                self.postfix()
            }
            TokenKind::Dollar => {
                self.variable_reference()?;
                self.postfix()
            }
            TokenKind::LeftParen => {
                self.parenthesized()?;
                self.postfix()
            }
            TokenKind::LeftBracket => {
                self.advance();
                if self.kind() != &TokenKind::RightBracket {
                    self.expr_single()?;
                    while self.kind() == &TokenKind::Comma {
                        self.advance();
                        self.expr_single()?;
                    }
                }
                self.expect(TokenKind::RightBracket, "']'")?;
                self.postfix()
            }
            TokenKind::Question => {
                self.advance();
                self.key_specifier()?;
                self.postfix()
            }
            _ => self.expected("an expression"),
        }
    }

    fn parenthesized(&mut self) -> Parsed {
        self.expect(TokenKind::LeftParen, "'('")?;
        if self.kind() != &TokenKind::RightParen {
            self.expr()?;
        }
        self.expect(TokenKind::RightParen, "')'")
    }

    fn variable_reference(&mut self) -> Parsed {
        self.expect(TokenKind::Dollar, "'$'")?;
        if !self.is_eqname() {
            return self.expected("a variable name after '$'");
        }
        let name = self.current_text().to_owned();
        let range = self.current().range.clone();
        self.advance();
        let binding = self
            .scopes
            .iter()
            .rev()
            .copied()
            .find(|index| self.analysis.bindings[*index].name == name);
        self.analysis.variables.push(VariableReference {
            name,
            range,
            binding,
        });
        Ok(())
    }

    fn predicates(&mut self) -> Parsed {
        while self.kind() == &TokenKind::LeftBracket {
            self.predicate()?;
        }
        Ok(())
    }

    fn predicate(&mut self) -> Parsed {
        self.advance();
        self.expr()?;
        self.expect(TokenKind::RightBracket, "']'")
    }

    fn postfix(&mut self) -> Parsed {
        loop {
            match self.kind() {
                TokenKind::LeftBracket => self.predicate()?,
                TokenKind::LeftParen => {
                    self.argument_list()?;
                }
                TokenKind::Question => {
                    self.advance();
                    self.key_specifier()?;
                }
                _ => return Ok(()),
            }
        }
    }

    fn key_specifier(&mut self) -> Parsed {
        match self.kind() {
            TokenKind::Name if !self.current_text().contains(':') => {
                self.advance();
                Ok(())
            }
            TokenKind::Integer | TokenKind::Star => {
                self.advance();
                Ok(())
            }
            TokenKind::LeftParen => self.parenthesized(),
            _ => self.expected("a key (name, integer, '*' or parenthesized expression) after '?'"),
        }
    }

    fn argument_list(&mut self) -> Result<Vec<Argument>, Stop> {
        self.expect(TokenKind::LeftParen, "'('")?;
        let mut arguments = Vec::new();
        if self.kind() == &TokenKind::RightParen {
            self.advance();
            return Ok(arguments);
        }
        loop {
            let first = self.index;
            let start = self.current().range.start;
            if self.kind() == &TokenKind::Question
                && matches!(self.kind_at(1), TokenKind::Comma | TokenKind::RightParen)
            {
                // Argument placeholder of a partial function application.
                self.advance();
            } else {
                self.expr_single()?;
            }
            let end = self.tokens[self.index.saturating_sub(1)].range.end;
            let string_literal = match &self.tokens[first].kind {
                TokenKind::String(value) if self.index == first + 1 => {
                    let range = self.tokens[first].range.clone();
                    Some((value.clone(), range.start + 1..range.end.saturating_sub(1)))
                }
                _ => None,
            };
            arguments.push(Argument {
                range: start..end.max(start),
                string_literal,
            });
            match self.kind() {
                TokenKind::Comma => self.advance(),
                TokenKind::RightParen => {
                    self.advance();
                    return Ok(arguments);
                }
                _ => {
                    self.expected("',' or ')'")?;
                }
            }
        }
    }

    fn inline_function(&mut self) -> Parsed {
        self.advance();
        self.expect(TokenKind::LeftParen, "'('")?;
        let mark = self.scopes.len();
        let mut parameters = Vec::new();
        if self.kind() != &TokenKind::RightParen {
            loop {
                self.expect(TokenKind::Dollar, "'$'")?;
                if !self.is_eqname() {
                    return self.expected("a parameter name");
                }
                parameters.push(VariableBinding {
                    name: self.current_text().to_owned(),
                    range: self.current().range.clone(),
                    kind: BindingKind::Parameter,
                });
                self.advance();
                if self.at_name("as") {
                    self.advance();
                    self.sequence_type()?;
                }
                if self.kind() == &TokenKind::Comma {
                    self.advance();
                    continue;
                }
                break;
            }
        }
        self.expect(TokenKind::RightParen, "')'")?;
        if self.at_name("as") {
            self.advance();
            self.sequence_type()?;
        }
        for parameter in parameters {
            self.analysis.bindings.push(parameter);
            self.scopes.push(self.analysis.bindings.len() - 1);
        }
        self.expect(TokenKind::LeftBrace, "'{'")?;
        if self.kind() != &TokenKind::RightBrace {
            self.expr()?;
        }
        let result = self.expect(TokenKind::RightBrace, "'}'");
        self.scopes.truncate(mark);
        result
    }

    fn map_constructor(&mut self) -> Parsed {
        self.advance();
        self.advance();
        if self.kind() != &TokenKind::RightBrace {
            loop {
                self.expr_single()?;
                self.expect(TokenKind::Colon, "':' between a map key and its value")?;
                self.expr_single()?;
                if self.kind() == &TokenKind::Comma {
                    self.advance();
                    continue;
                }
                break;
            }
        }
        self.expect(TokenKind::RightBrace, "'}'")
    }

    // ------------------------------------------------------------------
    // Node tests and types
    // ------------------------------------------------------------------

    fn node_test(&mut self) -> Parsed {
        match self.kind() {
            TokenKind::Name
                if self.kind_at(1) == &TokenKind::LeftParen
                    && KIND_TESTS.contains(&self.current_text()) =>
            {
                self.kind_test()
            }
            TokenKind::Name
            | TokenKind::BracedName
            | TokenKind::PrefixWildcard
            | TokenKind::LocalWildcard
            | TokenKind::BracedWildcard
            | TokenKind::Star => {
                self.advance();
                Ok(())
            }
            _ => self.expected("a node test"),
        }
    }

    /// `element(...)`, `text()`, ... (the current token is the name, the
    /// next one `(`).
    fn kind_test(&mut self) -> Parsed {
        let name = self.current_text().to_owned();
        self.advance();
        self.advance();
        match name.as_str() {
            "document-node" => {
                if self.kind() == &TokenKind::Name
                    && matches!(self.current_text(), "element" | "schema-element")
                    && self.kind_at(1) == &TokenKind::LeftParen
                {
                    self.kind_test()?;
                }
            }
            "element" | "attribute" => {
                if self.kind() != &TokenKind::RightParen {
                    if self.kind() == &TokenKind::Star || self.is_eqname() {
                        self.advance();
                    } else {
                        return self.expected("a name or '*'");
                    }
                    if self.kind() == &TokenKind::Comma {
                        self.advance();
                        if !self.is_eqname() {
                            return self.expected("a type name");
                        }
                        self.advance();
                        if self.kind() == &TokenKind::Question {
                            self.advance();
                        }
                    }
                }
            }
            "schema-element" | "schema-attribute" => {
                if !self.is_eqname() {
                    return self.expected("a name");
                }
                self.advance();
            }
            "processing-instruction" => {
                if matches!(self.kind(), TokenKind::Name | TokenKind::String(_)) {
                    self.advance();
                }
            }
            _ => {}
        }
        self.expect(TokenKind::RightParen, "')'")
    }

    fn single_type(&mut self) -> Parsed {
        if !self.is_eqname() {
            return self.expected("a type name");
        }
        self.advance();
        if self.kind() == &TokenKind::Question {
            self.advance();
        }
        Ok(())
    }

    fn sequence_type(&mut self) -> Parsed {
        self.enter()?;
        let result = self.sequence_type_inner();
        self.leave();
        result
    }

    fn sequence_type_inner(&mut self) -> Parsed {
        if self.at_name("empty-sequence") && self.kind_at(1) == &TokenKind::LeftParen {
            self.advance();
            self.advance();
            return self.expect(TokenKind::RightParen, "')'");
        }
        self.item_type()?;
        if matches!(
            self.kind(),
            TokenKind::Question | TokenKind::Star | TokenKind::Plus
        ) {
            self.advance();
        }
        Ok(())
    }

    fn item_type(&mut self) -> Parsed {
        match self.kind() {
            TokenKind::Name if self.kind_at(1) == &TokenKind::LeftParen => {
                let name = self.current_text().to_owned();
                if KIND_TESTS.contains(&name.as_str()) {
                    return self.kind_test();
                }
                match name.as_str() {
                    "item" => {
                        self.advance();
                        self.advance();
                        self.expect(TokenKind::RightParen, "')'")
                    }
                    "function" | "map" | "array" => {
                        self.advance();
                        self.advance();
                        if self.kind() == &TokenKind::Star {
                            self.advance();
                            return self.expect(TokenKind::RightParen, "')'");
                        }
                        match name.as_str() {
                            "function" => {
                                if self.kind() != &TokenKind::RightParen {
                                    self.sequence_type()?;
                                    while self.kind() == &TokenKind::Comma {
                                        self.advance();
                                        self.sequence_type()?;
                                    }
                                }
                                self.expect(TokenKind::RightParen, "')'")?;
                                self.expect_name("as")?;
                                self.sequence_type()
                            }
                            "map" => {
                                self.single_type_name()?;
                                self.expect(TokenKind::Comma, "','")?;
                                self.sequence_type()?;
                                self.expect(TokenKind::RightParen, "')'")
                            }
                            _ => {
                                self.sequence_type()?;
                                self.expect(TokenKind::RightParen, "')'")
                            }
                        }
                    }
                    _ => self.expected("an item type"),
                }
            }
            TokenKind::Name | TokenKind::BracedName => {
                self.advance();
                Ok(())
            }
            TokenKind::LeftParen => {
                self.advance();
                self.item_type()?;
                self.expect(TokenKind::RightParen, "')'")
            }
            _ => self.expected("a type"),
        }
    }

    fn single_type_name(&mut self) -> Parsed {
        if !self.is_eqname() {
            return self.expected("an atomic type name");
        }
        self.advance();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(expression: &str) -> Option<(String, Range<usize>)> {
        parse(expression)
            .errors
            .into_iter()
            .next()
            .map(|error| (error.message, error.range))
    }

    fn assert_valid(expression: &str) {
        assert_eq!(error(expression), None, "{expression}");
    }

    #[test]
    fn accepts_xpath_1_expressions() {
        for expression in [
            "/",
            "/*",
            "//para",
            "chapter//para[@type='x']",
            "../@lang",
            "child::para[position() = last()]",
            "ancestor-or-self::*[1]/@xml:lang",
            "count(//item) div 2 mod 3",
            "@*|node()",
            "text() | comment() | processing-instruction('xml-stylesheet')",
            "-1 + -(2 * 3)",
            "a = b = c",
            "string-length(normalize-space(.)) > 0 and not(@hidden) or $debug",
            "key('by-id', @ref)/title",
            "id('a')",
            "div/mod",
            "/div",
            "/div/p | /*",
            "*[self::div or self::p]",
            "document('')/*/xsl:template",
            "html:p/html:b",
            "1.5 + .5 + 5.",
            "/descendant::figure[position()=42]",
            "..",
            ".",
            "@*[local-name() != 'id']",
        ] {
            assert_valid(expression);
        }
    }

    #[test]
    fn accepts_xpath_2_and_3_expressions() {
        for expression in [
            "for $i in 1 to 10, $j in $i return $i * $j",
            "some $x in //a satisfies $x/@id = 'b'",
            "if ($a) then 'yes' else 'no'",
            "let $x := 1, $y := $x + 1 return $y",
            "$node instance of element(para, xs:string)?",
            "'1' cast as xs:integer? castable as xs:integer",
            "(1, 2, 3)[. gt 1]",
            "$a is $b or $a << $b",
            "'a' || 'b'",
            "//a ! string(@href)",
            "$s => upper-case() => substring(2)",
            "map { 'a': 1, 'b': (2, 3) }?a",
            "[1, 2, 3]?2",
            "array { 1 to 3 }?*",
            "function($x as xs:integer) as xs:integer { $x + 1 }(2)",
            "sum#1",
            "Q{http://www.w3.org/2005/xpath-functions}concat('a', 'b')",
            "document-node(element(*))",
            "element() except attribute()",
            "$m?('key')",
            "fn:substring(?, 1, 2)",
            "(: comment :) 1 (: another (: nested :) :)",
            "every $x in (1, 2) satisfies $x > 0",
            "$x treat as item()*",
            "$f instance of function(*)",
            "$f instance of function(xs:string) as xs:boolean",
            "$m instance of map(xs:string, item()*)",
            "$a instance of array(*)",
            "empty-sequence() instance of empty-sequence()",
            ".[@x]",
            "*:item/p:*",
        ] {
            assert_valid(expression);
        }
    }

    #[test]
    fn reports_errors_with_precise_ranges() {
        assert_eq!(
            error("concat('a', )"),
            Some(("Expected an expression but found ')'.".to_owned(), 12..13))
        );
        assert_eq!(
            error("a[1"),
            Some((
                "Expected ']' but reached the end of the expression.".to_owned(),
                2..3
            ))
        );
        assert_eq!(
            error("a b"),
            Some((
                "Unexpected 'b' after the end of the expression.".to_owned(),
                2..3
            ))
        );
        assert_eq!(
            error("foo::bar"),
            Some(("Unknown axis 'foo'.".to_owned(), 0..3))
        );
        assert_eq!(error("").map(|error| error.1), Some(0..0));
        assert!(error("   ").is_some());
        assert_eq!(error("'abc").map(|error| error.1), Some(0..4));
        assert_eq!(error("1 +").map(|error| error.1), Some(2..3));
        assert_eq!(error("if (a) then b").map(|error| error.1), Some(12..13));
        assert_eq!(
            error("for $x in 1 return").map(|error| error.1),
            Some(12..18)
        );
        assert!(error("a//").is_some());
        assert!(error("@").is_some());
        assert!(error("$").is_some());
        assert!(error("map{'a' 1}").is_some());
        assert!(error("10div 3").is_some());
    }

    #[test]
    fn resolves_range_variables_by_scope() {
        let analysis =
            parse("for $x in $x return (let $y := $x return $y, some $x in 1 satisfies $x, $y)");
        assert!(analysis.errors.is_empty());
        let references = analysis
            .variables
            .iter()
            .map(|variable| (variable.name.as_str(), variable.binding))
            .collect::<Vec<_>>();
        assert_eq!(
            references,
            vec![
                ("x", None),
                ("x", Some(0)),
                ("y", Some(1)),
                ("x", Some(2)),
                ("y", None),
            ]
        );
        let analysis = parse("function($a) { $a + $b }($a)");
        assert_eq!(
            analysis
                .variables
                .iter()
                .map(|variable| variable.binding)
                .collect::<Vec<_>>(),
            vec![Some(0), None, None]
        );
    }

    #[test]
    fn records_function_calls_and_string_arguments() {
        let analysis = parse("key('by-id', @ref) | f:do($x => f:twice('a'), 2) | f:do#2");
        let calls = analysis
            .functions
            .iter()
            .map(|call| (call.name.as_str(), call.arity, call.reference))
            .collect::<Vec<_>>();
        assert_eq!(
            calls,
            vec![
                ("key", 2, false),
                ("f:twice", 2, false),
                ("f:do", 2, false),
                ("f:do", 2, true),
            ]
        );
        assert_eq!(
            analysis.functions[0].arguments[0].string_literal,
            Some(("by-id".to_owned(), 5..10))
        );
        assert_eq!(analysis.functions[0].arguments[1].string_literal, None);
        assert_eq!(analysis.functions[0].range, 0..3);
    }

    #[test]
    fn bounds_the_recursion_depth() {
        let deep = "(".repeat(5000) + &")".repeat(5000);
        assert!(error(&deep).is_some());
        let deep = "-".repeat(100_000) + "1";
        assert_valid(&deep);
    }

    #[test]
    fn shifts_ranges_of_embedded_expressions() {
        let analysis = parse_range("{$a + }", 1..6);
        assert_eq!(analysis.errors[0].range, 4..5);
        assert_eq!(analysis.variables[0].range, 2..3);
    }
}
