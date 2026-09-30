//! XSLT attribute value templates (`href="{$base}/{@id}.html"`) and text
//! value templates (XSLT 3.0 `expand-text`).

use std::ops::Range;

use crate::parser::{XPathAnalysis, XPathError, parse_range};

/// Ranges of the expressions enclosed in `{...}` in `text`, and the errors
/// of the template itself (unmatched braces). `{{` and `}}` are escaped
/// braces.
pub fn value_template_expressions(text: &str) -> (Vec<Range<usize>>, Vec<XPathError>) {
    let bytes = text.as_bytes();
    let mut expressions = Vec::new();
    let mut errors = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'{' if bytes.get(index + 1) == Some(&b'{') => index += 2,
            b'}' if bytes.get(index + 1) == Some(&b'}') => index += 2,
            b'{' => {
                let start = index + 1;
                match expression_end(text, start) {
                    Some(end) => {
                        expressions.push(start..end);
                        index = end + 1;
                    }
                    None => {
                        errors.push(XPathError {
                            message: "Unterminated expression in value template: expected '}'."
                                .to_owned(),
                            range: index..text.len(),
                        });
                        break;
                    }
                }
            }
            b'}' => {
                errors.push(XPathError {
                    message: "Unescaped '}' in value template: write '}}'.".to_owned(),
                    range: index..index + 1,
                });
                index += 1;
            }
            _ => index += 1,
        }
    }
    (expressions, errors)
}

/// Offset of the `}` closing the expression starting at `start`, skipping
/// string literals, comments and nested braces (map constructors, inline
/// functions).
fn expression_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut index = start;
    while index < bytes.len() {
        match bytes[index] {
            quote @ (b'"' | b'\'') => {
                index += 1;
                loop {
                    let byte = *bytes.get(index)?;
                    index += 1;
                    if byte == quote {
                        if bytes.get(index) == Some(&quote) {
                            index += 1;
                            continue;
                        }
                        break;
                    }
                }
                continue;
            }
            b'(' if bytes.get(index + 1) == Some(&b':') => {
                let mut nesting = 1;
                index += 2;
                while nesting > 0 {
                    if index >= bytes.len() {
                        return None;
                    }
                    if bytes[index..].starts_with(b"(:") {
                        nesting += 1;
                        index += 2;
                    } else if bytes[index..].starts_with(b":)") {
                        nesting -= 1;
                        index += 2;
                    } else {
                        index += 1;
                    }
                }
                continue;
            }
            b'{' => depth += 1,
            b'}' if depth == 0 => return Some(index),
            b'}' => depth -= 1,
            _ => {}
        }
        index += 1;
    }
    None
}

/// Checks the value template `text`: brace errors and the syntax of each
/// non-empty enclosed expression. Ranges are relative to `text`.
pub fn parse_value_template(text: &str) -> XPathAnalysis {
    let (expressions, errors) = value_template_expressions(text);
    let mut analysis = XPathAnalysis {
        errors,
        ..XPathAnalysis::default()
    };
    for range in expressions {
        // XSLT 3.0 allows an empty expression (or only comments).
        let inner = &text[range.clone()];
        if inner.trim().is_empty() || is_only_comments(inner) {
            continue;
        }
        analysis.extend(parse_range(text, range));
    }
    analysis
}

fn is_only_comments(text: &str) -> bool {
    let tokens = crate::lexer::tokenize(text);
    tokens.len() == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_value_templates() {
        let (expressions, errors) = value_template_expressions("a{{b}}{$c}d{map{'k':'}'}?k}");
        assert!(errors.is_empty());
        assert_eq!(expressions, vec![7..9, 12..26]);
    }

    #[test]
    fn reports_unmatched_braces() {
        let (_, errors) = value_template_expressions("a}b");
        assert_eq!(errors[0].range, 1..2);
        let (_, errors) = value_template_expressions("x{concat('a'");
        assert_eq!(errors[0].range, 1..12);
    }

    #[test]
    fn parses_enclosed_expressions_with_ranges_in_the_template() {
        let analysis = parse_value_template("{$base}/{@id + }.html{}{(: c :)}");
        assert_eq!(analysis.variables[0].range, 2..6);
        assert_eq!(analysis.errors.len(), 1);
        assert_eq!(analysis.errors[0].range, 13..14);
    }
}
