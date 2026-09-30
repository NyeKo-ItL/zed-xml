//! XPath 1.0/2.0/3.1 syntax for editor features: a tolerant tokenizer
//! ([`lexer`]), a recursive-descent parser that reports the first syntax
//! error with its range and records variable references (resolved against
//! the expression's own range variables) and function calls ([`parser`]),
//! and XSLT value templates ([`template`]).
//!
//! All ranges are UTF-8 byte ranges into the parsed text. The crate has no
//! dependency and knows nothing about XML or LSP.

pub mod lexer;
pub mod parser;
pub mod template;

pub use lexer::{Token, TokenKind, is_ncname, is_qname, tokenize};
pub use parser::{
    Argument, BindingKind, FunctionCall, VariableBinding, VariableReference, XPathAnalysis,
    XPathError, parse, parse_range,
};
pub use template::{parse_value_template, value_template_expressions};
