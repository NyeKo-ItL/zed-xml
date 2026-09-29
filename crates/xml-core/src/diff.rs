//! Différence minimale entre deux textes, pour transformer un résultat de
//! formatage en modifications ciblées (les curseurs de l'éditeur restent
//! stables hors des lignes modifiées).

use std::ops::Range;

/// Remplacement de `old[range]` (offsets UTF-8) par `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChange {
    pub range: Range<usize>,
    pub text: String,
}

/// Distance d'édition (en lignes) au-delà de laquelle l'algorithme de Myers
/// est abandonné au profit d'un seul remplacement.
const MAX_EDIT_DISTANCE: usize = 1024;

/// Calcule les remplacements, triés et disjoints, qui transforment `old` en
/// `new`.
///
/// La comparaison se fait ligne à ligne (algorithme de Myers), puis chaque
/// bloc modifié est réduit à la partie qui diffère réellement. Au-delà de
/// [`MAX_EDIT_DISTANCE`] lignes modifiées, un seul remplacement couvre la
/// zone comprise entre le préfixe et le suffixe communs.
pub fn diff_text(old: &str, new: &str) -> Vec<TextChange> {
    if old == new {
        return Vec::new();
    }
    let old_starts = line_starts(old);
    let new_starts = line_starts(new);
    let old_lines = lines(old, &old_starts);
    let new_lines = lines(new, &new_starts);

    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_lines.len() - prefix
        && suffix < new_lines.len() - prefix
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let old_middle = &old_lines[prefix..old_lines.len() - suffix];
    let new_middle = &new_lines[prefix..new_lines.len() - suffix];
    let matches = myers(old_middle, new_middle).unwrap_or_default();

    let mut changes = Vec::new();
    let (mut old_index, mut new_index) = (0, 0);
    for (old_match, new_match) in matches
        .into_iter()
        .chain(std::iter::once((old_middle.len(), new_middle.len())))
    {
        if old_match > old_index || new_match > new_index {
            let range = old_starts[prefix + old_index]..old_starts[prefix + old_match];
            let text = &new[new_starts[prefix + new_index]..new_starts[prefix + new_match]];
            changes.push(refine(old, range, text));
        }
        old_index = old_match + 1;
        new_index = new_match + 1;
    }
    changes
}

/// Début de chaque ligne, suivi de la longueur du texte.
fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(index, _)| index + 1));
    if starts.last() != Some(&text.len()) {
        starts.push(text.len());
    }
    starts
}

/// Lignes (fin de ligne comprise) délimitées par `starts`.
fn lines<'a>(text: &'a str, starts: &[usize]) -> Vec<&'a str> {
    starts
        .windows(2)
        .map(|bounds| &text[bounds[0]..bounds[1]])
        .collect()
}

/// Réduit un remplacement à la partie qui diffère.
fn refine(old: &str, range: Range<usize>, text: &str) -> TextChange {
    let replaced = &old[range.clone()];
    let mut prefix = replaced
        .bytes()
        .zip(text.bytes())
        .take_while(|(left, right)| left == right)
        .count();
    while !replaced.is_char_boundary(prefix) || !text.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let mut suffix = replaced[prefix..]
        .bytes()
        .rev()
        .zip(text[prefix..].bytes().rev())
        .take_while(|(left, right)| left == right)
        .count();
    while !replaced.is_char_boundary(replaced.len() - suffix)
        || !text.is_char_boundary(text.len() - suffix)
    {
        suffix -= 1;
    }
    TextChange {
        range: range.start + prefix..range.end - suffix,
        text: text[prefix..text.len() - suffix].to_owned(),
    }
}

/// Paires de lignes identiques d'une plus courte suite d'éditions (Myers),
/// ou `None` si la distance dépasse [`MAX_EDIT_DISTANCE`].
fn myers(old: &[&str], new: &[&str]) -> Option<Vec<(usize, usize)>> {
    let (n, m) = (old.len() as isize, new.len() as isize);
    if n == 0 || m == 0 {
        return Some(Vec::new());
    }
    let limit = (old.len() + new.len()).min(MAX_EDIT_DISTANCE) as isize;
    let offset = limit + 1;
    let mut v = vec![0isize; 2 * limit as usize + 3];
    // trace[d] : valeurs de v pour k dans -d..=d après l'étape d.
    let mut trace: Vec<Vec<isize>> = Vec::new();
    for d in 0..=limit {
        for k in (-d..=d).step_by(2) {
            let index = (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[index - 1] < v[index + 1]) {
                v[index + 1]
            } else {
                v[index - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && old[x as usize] == new[y as usize] {
                x += 1;
                y += 1;
            }
            v[index] = x;
            if x >= n && y >= m {
                trace.push(v[(offset - d) as usize..=(offset + d) as usize].to_vec());
                return Some(backtrack(&trace, n, m));
            }
        }
        trace.push(v[(offset - d) as usize..=(offset + d) as usize].to_vec());
    }
    None
}

fn backtrack(trace: &[Vec<isize>], n: isize, m: isize) -> Vec<(usize, usize)> {
    let mut matches = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..trace.len()).rev() {
        if d == 0 {
            while x > 0 && y > 0 {
                x -= 1;
                y -= 1;
                matches.push((x as usize, y as usize));
            }
            break;
        }
        let d = d as isize;
        let previous = &trace[d as usize - 1];
        let at = |k: isize| previous[(k + d - 1) as usize];
        let k = x - y;
        let previous_k = if k == -d || (k != d && at(k - 1) < at(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let previous_x = at(previous_k);
        let previous_y = previous_x - previous_k;
        let (snake_x, snake_y) = if previous_k == k + 1 {
            (previous_x, previous_y + 1)
        } else {
            (previous_x + 1, previous_y)
        };
        while x > snake_x && y > snake_y {
            x -= 1;
            y -= 1;
            matches.push((x as usize, y as usize));
        }
        x = previous_x;
        y = previous_y;
    }
    matches.reverse();
    matches
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(old: &str, changes: &[TextChange]) -> String {
        let mut result = old.to_owned();
        for change in changes.iter().rev() {
            result.replace_range(change.range.clone(), &change.text);
        }
        result
    }

    fn check(old: &str, new: &str) -> Vec<TextChange> {
        let changes = diff_text(old, new);
        assert_eq!(apply(old, &changes), new, "{old:?} -> {new:?}: {changes:?}");
        for pair in changes.windows(2) {
            assert!(pair[0].range.end < pair[1].range.start, "{changes:?}");
        }
        changes
    }

    #[test]
    fn returns_no_change_for_identical_texts() {
        assert!(diff_text("<a/>\n", "<a/>\n").is_empty());
        assert!(diff_text("", "").is_empty());
    }

    #[test]
    fn limits_changes_to_modified_lines() {
        let old = "<root>\n<a/>\n  <b/>\n<c/>\n</root>\n";
        let new = "<root>\n  <a/>\n  <b/>\n  <c/>\n</root>\n";
        let changes = check(old, new);
        assert_eq!(
            changes,
            vec![
                TextChange {
                    range: 7..7,
                    text: "  ".to_owned()
                },
                TextChange {
                    range: 19..19,
                    text: "  ".to_owned()
                },
            ]
        );
    }

    #[test]
    fn handles_insertions_deletions_and_unicode() {
        check("<a>é</a>", "<a>\n  é\n</a>\n");
        check("<a>\n\n\n</a>\n", "<a>\n</a>\n");
        check("", "<a/>\n");
        check("<a/>\n", "");
        check("x😀y\n", "x😁y\n");
        check("<é/>\r\n<b/>", "<é/>\r\n  <b/>\r\n");
        let changes = check("😀a", "😀b");
        assert_eq!(changes[0].range, 4..5);
    }

    #[test]
    fn falls_back_to_a_single_change_for_large_distances() {
        let old = (0..3000).map(|i| format!("<a{i}/>\n")).collect::<String>();
        let new = (0..3000)
            .map(|i| format!("  <a{i}/>\n"))
            .collect::<String>();
        let changes = check(&old, &new);
        assert_eq!(changes.len(), 1);
    }

    #[test]
    fn matches_many_random_like_edits() {
        let lines = ["<a>", "</a>", "<b/>", "text", "", "  <c/>"];
        let mut seed = 7u32;
        let mut next = || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            (seed >> 16) as usize
        };
        for _ in 0..300 {
            let old = (0..next() % 12)
                .map(|_| lines[next() % lines.len()])
                .collect::<Vec<_>>()
                .join("\n");
            let new = (0..next() % 12)
                .map(|_| lines[next() % lines.len()])
                .collect::<Vec<_>>()
                .join("\n");
            check(&old, &new);
        }
    }
}
