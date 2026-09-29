//! Noms XML 1.0 (5e édition) : `Name`, `Nmtoken`.

/// `NameStartChar` de XML 1.0 5e édition.
pub(crate) fn is_name_start_char(character: char) -> bool {
    matches!(character,
        ':' | 'A'..='Z' | '_' | 'a'..='z'
        | '\u{C0}'..='\u{D6}' | '\u{D8}'..='\u{F6}' | '\u{F8}'..='\u{2FF}'
        | '\u{370}'..='\u{37D}' | '\u{37F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

/// `NameChar` de XML 1.0 5e édition.
pub fn is_name_char(character: char) -> bool {
    is_name_start_char(character)
        || matches!(character,
            '-' | '.' | '0'..='9' | '\u{B7}' | '\u{300}'..='\u{36F}' | '\u{203F}'..='\u{2040}')
}

/// `Name` : premier caractère `NameStartChar`, suivants `NameChar`.
pub fn is_name(value: &str) -> bool {
    let mut characters = value.chars();
    characters.next().is_some_and(is_name_start_char) && characters.all(is_name_char)
}

/// `Nmtoken` : un ou plusieurs `NameChar`.
pub fn is_nmtoken(value: &str) -> bool {
    !value.is_empty() && value.chars().all(is_name_char)
}

/// Fin de la suite de `NameChar` qui commence à `start` dans `text[..end]`.
pub(crate) fn scan_name_chars(text: &str, start: usize, end: usize) -> usize {
    let end = end.min(text.len());
    if start >= end {
        return start;
    }
    text[start..end]
        .char_indices()
        .find(|(_, character)| !is_name_char(*character))
        .map_or(end, |(offset, _)| start + offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_names_and_name_tokens() {
        assert!(is_name("xhtml:p"));
        assert!(is_name("_a-b.c"));
        assert!(is_name("élément"));
        assert!(!is_name("1abc"));
        assert!(!is_name(""));
        assert!(!is_name("a b"));
        assert!(is_nmtoken("1abc"));
        assert!(is_nmtoken("-."));
        assert!(!is_nmtoken(""));
        assert!(!is_nmtoken("a,b"));
        assert_eq!(scan_name_chars("ab;c", 0, 4), 2);
        assert_eq!(scan_name_chars("abc", 1, 3), 3);
        assert_eq!(scan_name_chars("abc", 3, 3), 3);
    }
}
