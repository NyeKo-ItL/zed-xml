//! Contrôle tolérant de la bonne formation, localisé précisément.
//!
//! `quick-xml` s'arrête à la première erreur et ne vérifie ni les attributs
//! dupliqués, ni les valeurs sans guillemets, ni les caractères `&`/`<` non
//! échappés. Ce module s'appuie sur l'analyseur lexical de [`crate::tags`]
//! pour relever tous les problèmes du document avec l'étendue exacte de la
//! construction fautive et les informations nécessaires aux correctifs
//! rapides (`textDocument/codeAction`) :
//!
//! - balise fermante qui ne correspond pas à l'élément ouvert
//!   ([`XmlProblemKind::MismatchedEndTag`]) ou sans élément ouvert
//!   ([`XmlProblemKind::UnmatchedEndTag`]) ;
//! - élément sans balise fermante ([`XmlProblemKind::UnclosedElement`]) ;
//! - balise sans `>` final ([`XmlProblemKind::UnclosedTag`]) ;
//! - attribut dupliqué ([`XmlProblemKind::DuplicateAttribute`]) ou valeur
//!   sans guillemets ([`XmlProblemKind::UnquotedAttributeValue`]) ;
//! - `&` ou `<` non échappé dans le texte ou une valeur d'attribut
//!   ([`XmlProblemKind::UnescapedCharacter`]).
//!
//! Les offsets sont des offsets d'octets UTF-8 dans la source.

use std::ops::Range;

use crate::tags::{XmlTag, XmlTagKind, scan_attributes, scan_markup, scan_tags};

/// Nature d'un problème de bonne formation et données utiles à sa correction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XmlProblemKind {
    /// Balise fermante (étendue signalée : son nom) qui ferme l'élément
    /// ouvert `expected`, dont le nom est en `start_name`.
    MismatchedEndTag {
        expected: String,
        start_name: Range<usize>,
    },
    /// Balise fermante (étendue signalée : son nom) sans élément ouvert
    /// correspondant ; `tag` couvre toute la balise.
    UnmatchedEndTag { tag: Range<usize> },
    /// Élément (étendue signalée : nom de la balise ouvrante) sans balise
    /// fermante. `insert_at` est la position où la balise fermante est
    /// attendue et `tag_end` la position du `>` de la balise ouvrante.
    UnclosedElement { insert_at: usize, tag_end: usize },
    /// Balise (étendue signalée : son nom) sans `>` final ; `insert_at` suit
    /// le dernier caractère significatif de la balise. `start_tag` indique une
    /// balise ouvrante (qui peut aussi devenir auto-fermante).
    UnclosedTag { insert_at: usize, start_tag: bool },
    /// Attribut (étendue signalée : son nom) déjà présent sur la balise ;
    /// `removal` couvre l'attribut et les espaces qui le précèdent.
    DuplicateAttribute { removal: Range<usize> },
    /// Valeur d'attribut (étendue signalée) sans guillemets.
    UnquotedAttributeValue,
    /// Caractère `&` ou `<` (étendue signalée) à remplacer par `&amp;` ou
    /// `&lt;`.
    UnescapedCharacter { character: char },
}

impl XmlProblemKind {
    /// Identifiant stable du problème, publié dans `data.kind` des
    /// diagnostics LSP.
    pub fn id(&self) -> &'static str {
        match self {
            Self::MismatchedEndTag { .. } => "mismatchedEndTag",
            Self::UnmatchedEndTag { .. } => "unmatchedEndTag",
            Self::UnclosedElement { .. } => "unclosedElement",
            Self::UnclosedTag { .. } => "unclosedTag",
            Self::DuplicateAttribute { .. } => "duplicateAttribute",
            Self::UnquotedAttributeValue => "unquotedAttributeValue",
            Self::UnescapedCharacter { .. } => "unescapedCharacter",
        }
    }

    /// Indique un problème de structure (appariement des balises) plutôt
    /// qu'un problème de syntaxe.
    pub fn is_structural(&self) -> bool {
        matches!(
            self,
            Self::MismatchedEndTag { .. }
                | Self::UnmatchedEndTag { .. }
                | Self::UnclosedElement { .. }
        )
    }
}

/// Problème de bonne formation localisé.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlProblem {
    pub kind: XmlProblemKind,
    /// Étendue signalée (voir [`XmlProblemKind`]).
    pub range: Range<usize>,
    pub message: String,
}

/// Relève les problèmes de bonne formation de la source, dans l'ordre du
/// document.
pub fn check_well_formedness(source: &str) -> Vec<XmlProblem> {
    let tags = scan_tags(source);
    let mut problems = Vec::new();
    check_tags(source, &tags, &mut problems);
    for tag in &tags {
        check_attributes(source, tag, &mut problems);
    }
    check_text(source, &tags, &mut problems);
    problems.sort_by_key(|problem| (problem.range.start, problem.range.end));
    problems
}

fn check_tags(source: &str, tags: &[XmlTag], problems: &mut Vec<XmlProblem>) {
    let mut open: Vec<usize> = Vec::new();
    let unclosed = |index: usize, insert_at: usize, problems: &mut Vec<XmlProblem>| {
        let tag: &XmlTag = &tags[index];
        // Une balise ouvrante sans `>` est déjà signalée.
        if tag.closed {
            problems.push(XmlProblem {
                kind: XmlProblemKind::UnclosedElement {
                    insert_at,
                    tag_end: tag.range.end - 1,
                },
                range: tag.name.clone(),
                message: format!("balise non fermée <{}>", tag.name(source)),
            });
        }
    };

    for (index, tag) in tags.iter().enumerate() {
        let name = tag.name(source);
        if !tag.closed {
            let insert_at = source[..tag.range.end.min(source.len())].trim_end().len();
            let (start_tag, label) = match tag.kind {
                XmlTagKind::End => (false, format!("balise fermante </{name}>")),
                _ => (true, format!("balise <{name}>")),
            };
            problems.push(XmlProblem {
                kind: XmlProblemKind::UnclosedTag {
                    insert_at: insert_at.max(tag.name.end),
                    start_tag,
                },
                range: tag.name.clone(),
                message: format!("{label} non terminée : `>` manquant"),
            });
        }
        match tag.kind {
            XmlTagKind::Start => open.push(index),
            XmlTagKind::SelfClosing => {}
            XmlTagKind::End => {
                if let Some(position) = open
                    .iter()
                    .rposition(|&candidate| tags[candidate].name(source) == name)
                {
                    for &inner in open[position + 1..].iter().rev() {
                        unclosed(inner, tag.range.start, problems);
                    }
                    open.truncate(position);
                    continue;
                }
                let Some(&top) = open.last() else {
                    problems.push(unmatched(source, tag));
                    continue;
                };
                // Si la balise fermante suivante ferme l'élément ouvert, celle-ci
                // est en trop ; sinon elle était destinée à l'élément ouvert.
                let expected = tags[top].name(source);
                let next_closes_top = tags[index + 1..]
                    .iter()
                    .find(|next| next.kind == XmlTagKind::End)
                    .is_some_and(|next| next.name(source) == expected);
                if next_closes_top {
                    problems.push(unmatched(source, tag));
                } else {
                    problems.push(XmlProblem {
                        kind: XmlProblemKind::MismatchedEndTag {
                            expected: expected.to_owned(),
                            start_name: tags[top].name.clone(),
                        },
                        range: tag.name.clone(),
                        message: format!("balise fermante </{name}> attend </{expected}>"),
                    });
                    open.pop();
                }
            }
        }
    }
    for &index in open.iter().rev() {
        unclosed(index, source.len(), problems);
    }
}

fn unmatched(source: &str, tag: &XmlTag) -> XmlProblem {
    XmlProblem {
        kind: XmlProblemKind::UnmatchedEndTag {
            tag: tag.range.clone(),
        },
        range: tag.name.clone(),
        message: format!("balise fermante inattendue </{}>", tag.name(source)),
    }
}

fn check_attributes(source: &str, tag: &XmlTag, problems: &mut Vec<XmlProblem>) {
    let attributes = scan_attributes(source, tag);
    let bytes = source.as_bytes();
    for (index, attribute) in attributes.iter().enumerate() {
        let name = attribute.name(source);
        if attributes[..index]
            .iter()
            .any(|previous| previous.name(source) == name)
        {
            let start = source[..attribute.name.start].trim_end().len();
            let end = match &attribute.value {
                Some(value) if is_quote(bytes.get(value.end)) => value.end + 1,
                Some(value) => value.end,
                None => attribute.name.end,
            };
            problems.push(XmlProblem {
                kind: XmlProblemKind::DuplicateAttribute {
                    removal: start..end,
                },
                range: attribute.name.clone(),
                message: format!("attribut {name} dupliqué sur <{}>", tag.name(source)),
            });
        }
        let Some(value) = &attribute.value else {
            continue;
        };
        if !is_quote(value.start.checked_sub(1).and_then(|at| bytes.get(at))) {
            problems.push(XmlProblem {
                kind: XmlProblemKind::UnquotedAttributeValue,
                range: value.clone(),
                message: format!("valeur de l'attribut {name} sans guillemets"),
            });
            continue;
        }
        check_characters(source, value.clone(), problems);
    }
}

fn is_quote(byte: Option<&u8>) -> bool {
    matches!(byte, Some(b'"' | b'\''))
}

/// Texte hors balises, commentaires, CDATA, instructions de traitement et
/// déclarations.
fn check_text(source: &str, tags: &[XmlTag], problems: &mut Vec<XmlProblem>) {
    let mut constructs = tags
        .iter()
        .map(|tag| tag.range.clone())
        .chain(scan_markup(source).into_iter().map(|markup| markup.range))
        .collect::<Vec<_>>();
    constructs.sort_by_key(|range| range.start);
    let mut position = 0;
    for construct in constructs {
        if construct.start > position {
            check_characters(source, position..construct.start, problems);
        }
        position = position.max(construct.end);
    }
    if position < source.len() {
        check_characters(source, position..source.len(), problems);
    }
}

fn check_characters(source: &str, range: Range<usize>, problems: &mut Vec<XmlProblem>) {
    let text = &source[range.clone()];
    for (index, character) in text.char_indices() {
        let escaped = match character {
            '<' => "&lt;",
            '&' if !starts_with_reference(&text[index..]) => "&amp;",
            _ => continue,
        };
        let start = range.start + index;
        problems.push(XmlProblem {
            kind: XmlProblemKind::UnescapedCharacter { character },
            range: start..start + 1,
            message: format!("caractère `{character}` non échappé (utiliser `{escaped}`)"),
        });
    }
}

/// Indique si `text` (qui commence par `&`) débute par une référence
/// d'entité ou de caractère complète (`&name;`, `&#10;`, `&#x1F;`).
fn starts_with_reference(text: &str) -> bool {
    let Some(end) = text.find(';') else {
        return false;
    };
    let body = &text[1..end];
    if let Some(hex) = body.strip_prefix("#x") {
        return !hex.is_empty() && hex.chars().all(|character| character.is_ascii_hexdigit());
    }
    if let Some(decimal) = body.strip_prefix('#') {
        return !decimal.is_empty() && decimal.chars().all(|character| character.is_ascii_digit());
    }
    let mut characters = body.chars();
    characters.next().is_some_and(|first| {
        first.is_alphabetic() || first == '_' || first == ':' || !first.is_ascii()
    }) && characters.all(|character| {
        character.is_alphanumeric()
            || matches!(character, '_' | ':' | '.' | '-')
            || !character.is_ascii()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<(&'static str, &str)> {
        check_well_formedness(source)
            .into_iter()
            .map(|problem| (problem.kind.id(), &source[problem.range]))
            .collect()
    }

    #[test]
    fn accepts_well_formed_documents() {
        let source = "<?xml version=\"1.0\"?>\n<!DOCTYPE r [<!ENTITY e \"&#38;\">]>\n<r a=\"1 &amp; 2\" b='&#x3C;'><!-- a < b & c --><![CDATA[<&>]]><x/>&e; &#10;</r>\n";
        assert_eq!(kinds(source), vec![]);
    }

    #[test]
    fn reports_mismatched_and_unmatched_end_tags() {
        let source = "<root><child></chidl></root>";
        let problems = check_well_formedness(source);
        assert_eq!(problems.len(), 1);
        assert_eq!(&source[problems[0].range.clone()], "chidl");
        assert_eq!(
            problems[0].kind,
            XmlProblemKind::MismatchedEndTag {
                expected: "child".to_owned(),
                start_name: 7..12,
            }
        );
        assert_eq!(
            problems[0].message,
            "balise fermante </chidl> attend </child>"
        );

        // La balise suivante ferme l'élément ouvert : celle-ci est en trop.
        let source = "<root></extra></root>";
        assert_eq!(kinds(source), vec![("unmatchedEndTag", "extra")]);
        assert_eq!(kinds("<a/></b>"), vec![("unmatchedEndTag", "b")]);
        assert_eq!(kinds("<a></b>"), vec![("mismatchedEndTag", "b")]);
    }

    #[test]
    fn reports_unclosed_elements_with_their_insertion_point() {
        let source = "<root>\n  <item>\n  <other/>\n</root>";
        let problems = check_well_formedness(source);
        assert_eq!(problems.len(), 1);
        assert_eq!(&source[problems[0].range.clone()], "item");
        assert_eq!(
            problems[0].kind,
            XmlProblemKind::UnclosedElement {
                insert_at: source.find("</root>").unwrap(),
                tag_end: source.find("<item>").unwrap() + 5,
            }
        );

        let source = "<root><child>";
        assert_eq!(
            kinds(source),
            vec![("unclosedElement", "root"), ("unclosedElement", "child")]
        );
        assert!(check_well_formedness(source).iter().all(|problem| matches!(
            problem.kind,
            XmlProblemKind::UnclosedElement { insert_at, .. } if insert_at == source.len()
        )));
    }

    #[test]
    fn reports_tags_missing_their_closing_bracket() {
        let source = "<a><b x=\"1\"  \n</a>";
        let problems = check_well_formedness(source);
        assert_eq!(problems.len(), 1);
        assert_eq!(
            problems[0].kind,
            XmlProblemKind::UnclosedTag {
                insert_at: source.find("  \n").unwrap(),
                start_tag: true,
            }
        );
        assert_eq!(kinds("<a></a"), vec![("unclosedTag", "a")]);
    }

    #[test]
    fn reports_duplicate_attributes_and_unquoted_values() {
        let source = "<a x=\"1\" y=2 x='3'/>";
        let problems = check_well_formedness(source);
        assert_eq!(
            problems
                .iter()
                .map(|problem| (problem.kind.id(), &source[problem.range.clone()]))
                .collect::<Vec<_>>(),
            vec![("unquotedAttributeValue", "2"), ("duplicateAttribute", "x")]
        );
        let XmlProblemKind::DuplicateAttribute { removal } = &problems[1].kind else {
            panic!("expected a duplicate attribute");
        };
        assert_eq!(&source[removal.clone()], " x='3'");
    }

    #[test]
    fn reports_unescaped_characters_in_text_and_attribute_values() {
        let source = "<a t=\"x < y &\">1 < 2 & 3 &amp; &bad &#xZ;</a>";
        assert_eq!(
            kinds(source),
            vec![
                ("unescapedCharacter", "<"),
                ("unescapedCharacter", "&"),
                ("unescapedCharacter", "<"),
                ("unescapedCharacter", "&"),
                ("unescapedCharacter", "&"),
                ("unescapedCharacter", "&"),
            ]
        );
        // Unicode et CRLF : les offsets restent des frontières de caractères.
        let source = "<é>\r\n😀 & ü\r\n</é>";
        let problems = check_well_formedness(source);
        assert_eq!(problems.len(), 1);
        assert_eq!(&source[problems[0].range.clone()], "&");
    }

    #[test]
    fn handles_namespaced_names() {
        assert_eq!(
            kinds("<p:a xmlns:p=\"urn:p\"><p:b></p:c></p:a>"),
            vec![("mismatchedEndTag", "p:c")]
        );
    }
}
