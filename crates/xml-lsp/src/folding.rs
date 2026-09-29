//! `textDocument/foldingRange`: folding ranges modelled on LemMinX
//! (`XMLFoldings`, default setting `includeClosingTagInFold = false`).
//!
//! - a multi-line element folds from the line of its start tag to the line
//!   before its end tag, which stays visible; a multi-line self-closing tag
//!   (many attributes) folds up to the line
//!   before `/>`;
//! - a multi-line comment (`kind = "comment"`) folds up to the line
//!   before `-->`;
//! - `<!-- #region -->` ... `<!-- #endregion -->` (`kind = "region"`, nested
//!   regions) folds up to the line before `#endregion`;
//! - `<!DOCTYPE ... [ ... ]>`, CDATA sections and multi-line processing
//!   instructions fold up to the line before their closing
//!   delimiter (LemMinX only folds the DOCTYPE).
//!
//! Unclosed elements and unterminated constructs produce no range. Like
//! LemMinX, two consecutive element ranges (in closing order) sharing the
//! same start line are emitted only once, and `rangeLimit` keeps the least
//! nested ranges first. Only lines are emitted (`startCharacter`/`endCharacter`
//! omitted), which also honours `lineFoldingOnly`.

use serde_json::{Map, Value, json};
use xml_core::tags::{XmlMarkupKind, XmlTagKind, XmlTagTree, scan_markup};

const KIND_COMMENT: &str = "comment";
const KIND_REGION: &str = "region";

/// Folding preferences announced by the client in `initialize`
/// (`capabilities.textDocument.foldingRange`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FoldingSettings {
    /// Maximum number of ranges wanted by the client.
    pub range_limit: Option<usize>,
    /// Range kinds (`foldingRangeKind.valueSet`) understood by the client;
    /// `None` when the client does not say.
    pub supported_kinds: Option<Vec<String>>,
}

impl FoldingSettings {
    /// Reads the client's folding capabilities from the `initialize`
    /// parameters.
    pub fn from_initialize_params(params: &Value) -> Self {
        let capabilities = params.pointer("/capabilities/textDocument/foldingRange");
        let range_limit = capabilities
            .and_then(|value| value.get("rangeLimit"))
            .and_then(Value::as_u64)
            .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX));
        let supported_kinds = capabilities
            .and_then(|value| value.pointer("/foldingRangeKind/valueSet"))
            .and_then(Value::as_array)
            .map(|kinds| {
                kinds
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            });
        Self {
            range_limit,
            supported_kinds,
        }
    }

    fn supports_kind(&self, kind: &str) -> bool {
        self.supported_kinds
            .as_ref()
            .is_none_or(|kinds| kinds.iter().any(|supported| supported == kind))
    }
}

/// Folding range in lines (0-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fold {
    pub start_line: usize,
    pub end_line: usize,
    pub kind: Option<&'static str>,
}

/// Returns the LSP folding ranges (`FoldingRange[]`) of `source`.
pub fn folding_ranges(source: &str, settings: &FoldingSettings) -> Vec<Value> {
    folds(source, settings.range_limit)
        .into_iter()
        .map(|fold| {
            let mut range = Map::new();
            range.insert("startLine".to_owned(), json!(fold.start_line));
            range.insert("endLine".to_owned(), json!(fold.end_line));
            if let Some(kind) = fold.kind.filter(|kind| settings.supports_kind(kind)) {
                range.insert("kind".to_owned(), json!(kind));
            }
            Value::Object(range)
        })
        .collect()
}

/// Folding candidate, dated by the offset of its closing delimiter to
/// reproduce LemMinX's emission order.
struct Candidate {
    close: usize,
    fold: Fold,
    /// Skipped when the range emitted just before starts on the same line
    /// (elements, regions and DOCTYPE in LemMinX).
    deduplicated: bool,
}

/// Computes the folding ranges sorted by start line.
pub fn folds(source: &str, range_limit: Option<usize>) -> Vec<Fold> {
    let lines = LineIndex::new(source);
    let mut candidates = Vec::new();
    // The end line is the one before the closing delimiter; an empty or
    // single-line range cannot be folded.
    let mut push = |start: usize, close: usize, kind, deduplicated| {
        let start_line = lines.line_of(start);
        let close_line = lines.line_of(close);
        if close_line > start_line + 1 {
            candidates.push(Candidate {
                close,
                fold: Fold {
                    start_line,
                    end_line: close_line - 1,
                    kind,
                },
                deduplicated,
            });
        }
    };

    for element in XmlTagTree::parse(source).elements() {
        let close = match (&element.end_tag, element.start_tag.kind) {
            (Some(end_tag), _) if end_tag.closed => end_tag.range.end - 1,
            (Some(end_tag), _) => end_tag.range.start,
            (None, XmlTagKind::SelfClosing) => element.start_tag.range.end - 2,
            (None, _) => continue,
        };
        push(element.start_tag.range.start, close, None, true);
    }

    let mut regions = Vec::new();
    for markup in scan_markup(source) {
        if !markup.closed {
            continue;
        }
        match markup.kind {
            XmlMarkupKind::Comment => match region_marker(markup.content(source)) {
                Some(RegionMarker::Start) => regions.push(markup.range.start),
                Some(RegionMarker::End) => {
                    if let Some(start) = regions.pop() {
                        push(start, markup.range.start, Some(KIND_REGION), true);
                    }
                }
                None => push(
                    markup.range.start,
                    markup.content.end,
                    Some(KIND_COMMENT),
                    false,
                ),
            },
            XmlMarkupKind::Declaration => push(markup.range.start, markup.content.end, None, true),
            XmlMarkupKind::CData | XmlMarkupKind::ProcessingInstruction => {
                push(markup.range.start, markup.content.end, None, false)
            }
        }
    }

    candidates.sort_by_key(|candidate| candidate.close);
    let mut folds = Vec::with_capacity(candidates.len());
    let mut previous_start = None;
    for candidate in candidates {
        if candidate.deduplicated && previous_start == Some(candidate.fold.start_line) {
            continue;
        }
        previous_start = Some(candidate.fold.start_line);
        folds.push(candidate.fold);
    }

    folds.sort_by_key(|fold| (fold.start_line, fold.end_line));
    match range_limit {
        Some(limit) if folds.len() > limit => limit_folds(folds, limit),
        _ => folds,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RegionMarker {
    Start,
    End,
}

/// Recognizes `#region` / `#endregion` at the start of a comment (leading
/// whitespace ignored), followed by a word boundary.
fn region_marker(comment: &str) -> Option<RegionMarker> {
    let rest = comment.trim_start().strip_prefix('#')?;
    [
        ("region", RegionMarker::Start),
        ("endregion", RegionMarker::End),
    ]
    .into_iter()
    .find_map(|(keyword, marker)| {
        let after = rest.strip_prefix(keyword)?;
        let boundary = !after
            .chars()
            .next()
            .is_some_and(|character| character.is_alphanumeric() || character == '_');
        boundary.then_some(marker)
    })
}

/// Depth beyond which ranges are no longer counted
/// (like LemMinX / vscode-html-languageservice).
const MAX_NESTING_LEVEL: usize = 30;

/// Reduces `folds` (sorted by start line then end line) to `limit` ranges,
/// keeping the least nested first, like LemMinX's `limitRanges`.
fn limit_folds(folds: Vec<Fold>, limit: usize) -> Vec<Fold> {
    let mut levels: Vec<Option<usize>> = vec![None; folds.len()];
    let mut level_counts = [0usize; MAX_NESTING_LEVEL];
    let mut top: Option<Fold> = None;
    let mut previous: Vec<Fold> = Vec::new();
    for (index, fold) in folds.iter().enumerate() {
        let level = match top {
            None => Some(0),
            Some(current) if fold.start_line > current.start_line => {
                if fold.end_line <= current.end_line {
                    previous.push(current);
                    Some(previous.len())
                } else if fold.start_line > current.end_line {
                    while previous
                        .last()
                        .is_some_and(|parent| fold.start_line > parent.end_line)
                    {
                        previous.pop();
                    }
                    Some(previous.len())
                } else {
                    // Overlap without nesting: range skipped.
                    None
                }
            }
            Some(_) => None,
        };
        if let Some(level) = level {
            top = Some(*fold);
            levels[index] = Some(level);
            if let Some(count) = level_counts.get_mut(level) {
                *count += 1;
            }
        }
    }

    let mut entries = 0;
    let mut max_level = MAX_NESTING_LEVEL;
    for (level, &count) in level_counts.iter().enumerate() {
        if entries + count > limit {
            max_level = level;
            break;
        }
        entries += count;
    }

    folds
        .into_iter()
        .zip(levels)
        .filter_map(|(fold, level)| {
            let level = level?;
            let keep = level < max_level || (level == max_level && entries < limit);
            if level == max_level && keep {
                entries += 1;
            }
            keep.then_some(fold)
        })
        .collect()
}

/// UTF-8 offset -> line number conversion (`\n`, hence also `\r\n`).
struct LineIndex {
    newlines: Vec<usize>,
}

impl LineIndex {
    fn new(source: &str) -> Self {
        Self {
            newlines: source
                .bytes()
                .enumerate()
                .filter(|&(_, byte)| byte == b'\n')
                .map(|(offset, _)| offset)
                .collect(),
        }
    }

    fn line_of(&self, offset: usize) -> usize {
        self.newlines.partition_point(|&newline| newline < offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(source: &str) -> Vec<(usize, usize, Option<&'static str>)> {
        folds(source, None)
            .into_iter()
            .map(|fold| (fold.start_line, fold.end_line, fold.kind))
            .collect()
    }

    #[test]
    fn folds_multiline_elements_up_to_the_line_before_the_end_tag() {
        let source = "<root>\n  <a>\n    <b/>\n  </a>\n  <c>text</c>\n</root>\n";
        assert_eq!(lines(source), vec![(0, 4, None), (1, 2, None)]);
    }

    #[test]
    fn single_line_and_two_line_elements_produce_nothing() {
        assert!(lines("<root><a>text</a></root>").is_empty());
        assert!(lines("<root>\n</root>").is_empty());
        assert!(lines("<root>text\n</root>").is_empty());
        assert!(lines("<root/>").is_empty());
        assert!(lines("").is_empty());
    }

    #[test]
    fn folds_multiline_start_tags_with_many_attributes() {
        let source = "<root>\n  <item\n    a=\"1\"\n    b=\"2\"\n    c=\"3\"/>\n  <other\n    a=\"1\"\n  >\n  </other>\n</root>";
        assert_eq!(
            lines(source),
            vec![(0, 8, None), (1, 3, None), (5, 7, None)]
        );
    }

    #[test]
    fn keeps_a_single_range_per_start_line_like_lemminx() {
        // `<a><b>`: LemMinX emits `b` (closed first) and skips `a`.
        let source = "<a><b>\n  x\n  y\n</b>\n</a>";
        assert_eq!(lines(source), vec![(0, 2, None)]);
    }

    #[test]
    fn folds_comments_cdata_processing_instructions_and_doctype() {
        let source = "<?xml version=\"1.0\"?>\n<!DOCTYPE root [\n  <!ELEMENT root ANY>\n  <!-- ]> -->\n]>\n<!--\n  comment\n-->\n<root>\n  <![CDATA[\n    <x>\n  ]]>\n  <?pi\n    data\n  ?>\n</root>";
        assert_eq!(
            lines(source),
            vec![
                (1, 3, None),
                (5, 6, Some(KIND_COMMENT)),
                (8, 14, None),
                (9, 10, None),
                (12, 13, None),
            ]
        );
        assert!(lines("<!-- a -->\n<!-- b\n-->").is_empty());
    }

    #[test]
    fn folds_nested_regions() {
        let source = "<root>\n  <!-- #region outer -->\n  <a/>\n  <!--#region inner-->\n  <b/>\n  <!-- #endregion -->\n  <!-- #endregion outer -->\n</root>";
        assert_eq!(
            lines(source),
            vec![
                (0, 6, None),
                (1, 5, Some(KIND_REGION)),
                (3, 4, Some(KIND_REGION)),
            ]
        );
        // Unpaired or misspelled markers: plain comments.
        let source = "<!-- #endregion -->\n<!-- #regionx -->\n<a/>\n<!-- #region -->";
        assert!(lines(source).is_empty());
        assert_eq!(region_marker("  #region"), Some(RegionMarker::Start));
        assert_eq!(region_marker("#endregion-x"), Some(RegionMarker::End));
        assert_eq!(region_marker("# region"), None);
        assert_eq!(region_marker("text #region"), None);
    }

    #[test]
    fn handles_crlf_line_endings() {
        let source =
            "<root>\r\n  <a>\r\n    <b/>\r\n  </a>\r\n  <!--\r\n  x\r\n  -->\r\n</root>\r\n";
        assert_eq!(
            lines(source),
            vec![(0, 6, None), (1, 2, None), (4, 5, Some(KIND_COMMENT))]
        );
    }

    #[test]
    fn handles_deeply_nested_documents() {
        let depth = 500;
        let mut source = String::new();
        for level in 0..depth {
            source.push_str(&format!("<e{level}>\n"));
        }
        for level in (0..depth).rev() {
            source.push_str(&format!("</e{level}>\n"));
        }
        let folds = folds(&source, None);
        assert_eq!(folds.len(), depth - 1);
        assert_eq!(
            folds[0],
            Fold {
                start_line: 0,
                end_line: 2 * depth - 2,
                kind: None
            }
        );
        let limited = super::folds(&source, Some(10));
        assert_eq!(limited.len(), 10);
        assert_eq!(limited, folds[..10]);
    }

    #[test]
    fn skips_unclosed_and_malformed_constructs() {
        let source = "<root>\n  <open>\n    <child>\n    </child>\n  <!-- unterminated\n\n";
        assert_eq!(lines(source), vec![]);
        let source = "<root>\n  <open>\n    <child>\n    </child>\n\n</root>";
        assert_eq!(lines(source), vec![(0, 4, None)]);
        for source in [
            "<",
            "<a",
            "<a\n\n\n",
            "<a b=\"\n\n\n",
            "</a>\n\n</a>",
            "<![CDATA[\n\n",
            "<?pi\n\n",
            "<!DOCTYPE r [\n\n",
            "<a>\n\n</a",
            "<a>\n\n</a\n<b>",
        ] {
            let _ = lines(source);
        }
        assert_eq!(lines("<a>\n\n\n</a"), vec![(0, 2, None)]);
    }

    #[test]
    fn limits_ranges_by_nesting_level() {
        let fold = |start_line, end_line| Fold {
            start_line,
            end_line,
            kind: None,
        };
        let all = vec![
            fold(0, 20),
            fold(1, 5),
            fold(2, 4),
            fold(6, 10),
            fold(7, 9),
            fold(11, 19),
        ];
        assert_eq!(
            limit_folds(all.clone(), 4),
            vec![fold(0, 20), fold(1, 5), fold(6, 10), fold(11, 19)]
        );
        assert_eq!(
            limit_folds(all.clone(), 5),
            vec![
                fold(0, 20),
                fold(1, 5),
                fold(2, 4),
                fold(6, 10),
                fold(11, 19)
            ]
        );
        assert_eq!(limit_folds(all.clone(), 1), vec![fold(0, 20)]);
        assert!(limit_folds(all, 0).is_empty());
    }

    #[test]
    fn reads_client_capabilities_and_filters_kinds() {
        let settings = FoldingSettings::from_initialize_params(&json!({
            "capabilities": {"textDocument": {"foldingRange": {
                "rangeLimit": 1,
                "lineFoldingOnly": true,
                "foldingRangeKind": {"valueSet": ["region"]},
            }}}
        }));
        assert_eq!(settings.range_limit, Some(1));
        let source = "<!--\n\n-->\n<a>\n\n</a>";
        assert_eq!(
            folding_ranges(source, &settings),
            vec![json!({"startLine": 0, "endLine": 1})]
        );
        let settings = FoldingSettings::from_initialize_params(&json!({}));
        assert_eq!(settings, FoldingSettings::default());
        assert_eq!(
            folding_ranges(source, &settings),
            vec![
                json!({"startLine": 0, "endLine": 1, "kind": "comment"}),
                json!({"startLine": 3, "endLine": 4}),
            ]
        );
    }
}
