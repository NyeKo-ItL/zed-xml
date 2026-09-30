//! Minimal difference between two texts, to turn a formatting result into
//! targeted edits (editor cursors stay stable outside the modified
//! lines).

use std::ops::Range;

/// Replacement of `old[range]` (UTF-8 offsets) with `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChange {
    pub range: Range<usize>,
    pub text: String,
}

/// Edit distance (in lines) beyond which the Myers algorithm is abandoned in
/// favour of a single replacement.
const MAX_EDIT_DISTANCE: usize = 1024;

/// Computes the sorted, disjoint replacements that turn `old` into
/// `new`.
///
/// The comparison is done line by line (Myers algorithm), then each changed
/// block is reduced to the part that actually differs. Beyond
/// [`MAX_EDIT_DISTANCE`] changed lines, a single replacement covers the
/// region between the common prefix and suffix.
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

/// Start of each line, followed by the length of the text.
fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    starts.extend(text.match_indices('\n').map(|(index, _)| index + 1));
    if starts.last() != Some(&text.len()) {
        starts.push(text.len());
    }
    starts
}

/// Lines (line ending included) delimited by `starts`.
fn lines<'a>(text: &'a str, starts: &[usize]) -> Vec<&'a str> {
    starts
        .windows(2)
        .map(|bounds| &text[bounds[0]..bounds[1]])
        .collect()
}

/// Reduces a replacement to the part that differs.
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
    // An LSP position cannot designate the point between the `\r` and the
    // `\n` of a CRLF: the change is widened to the whole line break.
    let splits_crlf = |offset: usize| {
        old.as_bytes().get(offset.wrapping_sub(1)) == Some(&b'\r')
            && old.as_bytes().get(offset) == Some(&b'\n')
    };
    while prefix > 0 && splits_crlf(range.start + prefix) {
        prefix -= 1;
    }
    while suffix > 0 && splits_crlf(range.end - suffix) {
        suffix -= 1;
    }
    TextChange {
        range: range.start + prefix..range.end - suffix,
        text: text[prefix..text.len() - suffix].to_owned(),
    }
}

/// Pairs of identical lines of a shortest edit script (Myers), or `None` if
/// the distance exceeds [`MAX_EDIT_DISTANCE`].
fn myers(old: &[&str], new: &[&str]) -> Option<Vec<(usize, usize)>> {
    let (n, m) = (old.len() as isize, new.len() as isize);
    if n == 0 || m == 0 {
        return Some(Vec::new());
    }
    let limit = (old.len() + new.len()).min(MAX_EDIT_DISTANCE) as isize;
    let offset = limit + 1;
    let mut v = vec![0isize; 2 * limit as usize + 3];
    // trace[d]: values of v for k in -d..=d after step d.
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
    fn never_splits_a_crlf_line_break() {
        let splits = |old: &str, changes: &[TextChange]| {
            changes.iter().any(|change| {
                [change.range.start, change.range.end]
                    .iter()
                    .any(|&offset| {
                        offset > 0
                            && old.as_bytes().get(offset - 1) == Some(&b'\r')
                            && old.as_bytes().get(offset) == Some(&b'\n')
                    })
            })
        };
        for (old, new) in [
            ("<a>\n\r\n</a>\n", "<a>\n</a>\n"),
            ("<a>\r\n</a>", "<a>\n</a>"),
            ("<a>\r\n\r\n<b/>\r\n</a>", "<a>\r\n  <b/>\n</a>\n"),
            ("x\r\ny", "x\r\r\ny"),
        ] {
            let changes = check(old, new);
            assert!(!splits(old, &changes), "{old:?} -> {new:?}: {changes:?}");
        }
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
