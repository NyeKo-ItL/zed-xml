//! User settings of the server (`xml` section, LemMinX naming).
//!
//! Settings come from `initializationOptions` (`{"settings": {"xml":
//! …}}`, `{"xml": …}` or directly the content of the section), then from
//! `workspace/configuration` (`xml` section) and
//! `workspace/didChangeConfiguration`. Reading is tolerant: a missing key
//! or one of an unexpected type keeps its default value, and the defaults
//! reproduce the historical behaviour of the server.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use xml_core::{EmptyElements, FormatOptions, QuoteStyle, SplitAttributes};

/// `xml.*` settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub format: FormatSettings,
    pub validation: ValidationSettings,
    /// `xml.completion.autoCloseTags`: offers the end tag after `>`.
    pub auto_close_tags: bool,
    /// `xml.symbols.enabled`.
    pub symbols_enabled: bool,
    /// `xml.symbols.maxItemsComputed` (`None`: no limit).
    pub symbols_max_items: Option<usize>,
    /// `xml.colors.enabled`.
    pub colors_enabled: bool,
    /// `xml.catalogs`: raw OASIS XML catalog paths (resolved by
    /// [`crate::catalog::catalog_paths`] against the workspace
    /// folders).
    pub catalogs: Vec<String>,
    /// `xml.autoDetectCatalogs` (extension, `false` by default): also uses
    /// `catalog.xml` at the root of each workspace folder, when it is an
    /// OASIS catalog.
    pub auto_detect_catalogs: bool,
    /// `xml.fileAssociations`.
    pub file_associations: Vec<FileAssociation>,
    /// `xml.maxFileSize` (extension, bytes, `None`: no limit): beyond it,
    /// grammar validation and the whole-document features (symbols,
    /// folding, colors, links, code actions, selection ranges) are skipped.
    pub max_file_size: Option<usize>,
}

/// Default of `xml.maxFileSize`: 10 MiB.
pub const DEFAULT_MAX_FILE_SIZE: usize = 10 * 1024 * 1024;

/// Default of `xml.validation.debounce`, in milliseconds.
pub const DEFAULT_VALIDATION_DEBOUNCE: u64 = 200;

impl Default for Settings {
    fn default() -> Self {
        Self {
            format: FormatSettings::default(),
            validation: ValidationSettings::default(),
            auto_close_tags: true,
            symbols_enabled: true,
            symbols_max_items: None,
            colors_enabled: true,
            catalogs: Vec::new(),
            auto_detect_catalogs: false,
            file_associations: Vec::new(),
            max_file_size: Some(DEFAULT_MAX_FILE_SIZE),
        }
    }
}

impl Settings {
    /// The document exceeds `xml.maxFileSize`.
    pub fn is_large(&self, source: &str) -> bool {
        self.max_file_size.is_some_and(|limit| source.len() > limit)
    }
}

/// `xml.format.*` settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatSettings {
    /// `xml.format.enabled`.
    pub enabled: bool,
    pub split_attributes: SplitAttributes,
    pub max_line_width: usize,
    pub preserved_newlines: usize,
    pub closing_bracket_new_line: bool,
    pub empty_elements: EmptyElements,
    pub preserve_attribute_line_breaks: bool,
    pub space_before_empty_close_tag: bool,
    pub quote_style: QuoteStyle,
    /// Fallback values when the request does not provide the matching LSP
    /// option.
    pub insert_spaces: Option<bool>,
    pub tab_size: Option<usize>,
    pub trim_final_newlines: Option<bool>,
    pub insert_final_newline: Option<bool>,
    pub trim_trailing_whitespace: Option<bool>,
}

impl Default for FormatSettings {
    fn default() -> Self {
        let defaults = FormatOptions::default();
        Self {
            enabled: true,
            split_attributes: defaults.split_attributes,
            max_line_width: defaults.max_line_width,
            preserved_newlines: defaults.preserved_newlines,
            closing_bracket_new_line: defaults.closing_bracket_new_line,
            empty_elements: defaults.empty_elements,
            preserve_attribute_line_breaks: defaults.preserve_attribute_line_breaks,
            space_before_empty_close_tag: defaults.space_before_empty_close_tag,
            quote_style: defaults.quote_style,
            insert_spaces: None,
            tab_size: None,
            trim_final_newlines: None,
            insert_final_newline: None,
            trim_trailing_whitespace: None,
        }
    }
}

impl FormatSettings {
    /// Applies the `xml.format.*` settings to default options (before the
    /// request's `FormattingOptions`, which take precedence).
    pub fn apply(&self, options: &mut FormatOptions) {
        options.split_attributes = self.split_attributes;
        options.max_line_width = self.max_line_width;
        options.preserved_newlines = self.preserved_newlines;
        options.closing_bracket_new_line = self.closing_bracket_new_line;
        options.empty_elements = self.empty_elements;
        options.preserve_attribute_line_breaks = self.preserve_attribute_line_breaks;
        options.space_before_empty_close_tag = self.space_before_empty_close_tag;
        options.quote_style = self.quote_style;
        if let Some(value) = self.insert_spaces {
            options.insert_spaces = value;
        }
        if let Some(value) = self.tab_size {
            options.tab_size = value;
        }
        if let Some(value) = self.trim_final_newlines {
            options.trim_final_newlines = value;
        }
        if let Some(value) = self.insert_final_newline {
            options.insert_final_newline = value;
        }
        if let Some(value) = self.trim_trailing_whitespace {
            options.trim_trailing_whitespace = value;
        }
    }
}

/// `xml.validation.schema.enabled`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SchemaValidation {
    /// Validates with the loaded schemas and reports loading errors.
    #[default]
    Always,
    /// No XSD validation.
    Never,
    /// Validates only if all schemas load without error (loading errors
    /// are still reported).
    OnValidSchema,
}

/// Severity of the `xml.validation.noGrammar` diagnostic.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NoGrammar {
    #[default]
    Ignore,
    Hint,
    Info,
    Warning,
}

impl NoGrammar {
    /// LSP severity (`None`: no diagnostic).
    pub fn severity(self) -> Option<u8> {
        match self {
            Self::Ignore => None,
            Self::Warning => Some(2),
            Self::Info => Some(3),
            Self::Hint => Some(4),
        }
    }
}

/// `xml.validation.*` settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationSettings {
    /// `xml.validation.enabled`: disables all diagnostics.
    pub enabled: bool,
    pub schema: SchemaValidation,
    pub no_grammar: NoGrammar,
    /// `xml.validation.disallowDocTypeDecl`: reports any `<!DOCTYPE>`
    /// declaration.
    pub disallow_doc_type_decl: bool,
    /// `xml.validation.resolveExternalEntities`: referenced external general
    /// entities must be resolvable (never read).
    pub resolve_external_entities: bool,
    /// `xml.validation.debounce` (extension): delay in milliseconds between
    /// the last change of a document and its validation.
    pub debounce_ms: u64,
}

impl Default for ValidationSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            schema: SchemaValidation::Always,
            no_grammar: NoGrammar::Ignore,
            disallow_doc_type_decl: false,
            resolve_external_entities: false,
            debounce_ms: DEFAULT_VALIDATION_DEBOUNCE,
        }
    }
}

/// `xml.fileAssociations` association: files matching `pattern` (glob)
/// are validated with the `system_id` schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAssociation {
    pub pattern: String,
    pub system_id: String,
}

impl Settings {
    /// Reads the `xml` section (or its content); tolerant of invalid types.
    pub fn from_value(value: &Value) -> Self {
        let empty = Map::new();
        let root = xml_section(value)
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let mut settings = Self::default();
        let get = |path: &str| lookup(root, path);
        let flag = |path: &str| get(path).and_then(Value::as_bool);
        let number = |path: &str| get(path).and_then(Value::as_u64).map(|n| n as usize);
        let text = |path: &str| get(path).and_then(Value::as_str);

        let format = &mut settings.format;
        if let Some(value) = flag("format.enabled") {
            format.enabled = value;
        }
        match get("format.splitAttributes") {
            Some(Value::Bool(true)) => format.split_attributes = SplitAttributes::SplitNewLine,
            Some(Value::Bool(false)) => format.split_attributes = SplitAttributes::Preserve,
            Some(Value::String(value)) => {
                if let Some(split) = match value.as_str() {
                    "preserve" | "none" => Some(SplitAttributes::Preserve),
                    "splitNewLine" | "indent" => Some(SplitAttributes::SplitNewLine),
                    "alignWithFirstAttr" | "alignWithFirst" => {
                        Some(SplitAttributes::AlignWithFirstAttr)
                    }
                    _ => None,
                } {
                    format.split_attributes = split;
                }
            }
            _ => {}
        }
        if let Some(value) = number("format.maxLineWidth") {
            format.max_line_width = value;
        }
        if let Some(value) = number("format.preservedNewlines") {
            format.preserved_newlines = value.min(100);
        }
        if let Some(value) = flag("format.closingBracketNewLine") {
            format.closing_bracket_new_line = value;
        }
        if let Some(value) = text("format.emptyElements").and_then(|value| match value {
            "ignore" => Some(EmptyElements::Ignore),
            "expand" => Some(EmptyElements::Expand),
            "collapse" => Some(EmptyElements::Collapse),
            _ => None,
        }) {
            format.empty_elements = value;
        }
        if let Some(value) = flag("format.preserveAttributeLineBreaks") {
            format.preserve_attribute_line_breaks = value;
        }
        if let Some(value) = flag("format.spaceBeforeEmptyCloseTag") {
            format.space_before_empty_close_tag = value;
        }
        // LemMinX: `enforceQuoteStyle` ("ignore" | "preferred") enforces
        // `xml.preferences.quoteStyle` ("double" | "single").
        if text("format.enforceQuoteStyle") == Some("preferred") {
            format.quote_style = match text("preferences.quoteStyle") {
                Some("single") => QuoteStyle::Single,
                _ => QuoteStyle::Double,
            };
        }
        format.insert_spaces = flag("format.insertSpaces");
        format.tab_size = number("format.tabSize").map(|size| size.min(16));
        format.trim_final_newlines = flag("format.trimFinalNewlines");
        format.insert_final_newline = flag("format.insertFinalNewline");
        format.trim_trailing_whitespace = flag("format.trimTrailingWhitespace");

        let validation = &mut settings.validation;
        if let Some(value) = flag("validation.enabled") {
            validation.enabled = value;
        }
        // `xml.validation.schema`: `{enabled}` object (recent LemMinX) or
        // boolean (older versions).
        let schema = match get("validation.schema") {
            Some(Value::Bool(enabled)) => Some(Value::Bool(*enabled)),
            _ => get("validation.schema.enabled").cloned(),
        };
        match schema {
            Some(Value::Bool(true)) => validation.schema = SchemaValidation::Always,
            Some(Value::Bool(false)) => validation.schema = SchemaValidation::Never,
            Some(Value::String(value)) => match value.as_str() {
                "always" => validation.schema = SchemaValidation::Always,
                "never" => validation.schema = SchemaValidation::Never,
                "onValidSchema" => validation.schema = SchemaValidation::OnValidSchema,
                _ => {}
            },
            _ => {}
        }
        if let Some(value) = text("validation.noGrammar").and_then(|value| match value {
            "ignore" => Some(NoGrammar::Ignore),
            "hint" => Some(NoGrammar::Hint),
            "info" => Some(NoGrammar::Info),
            "warning" => Some(NoGrammar::Warning),
            _ => None,
        }) {
            validation.no_grammar = value;
        }
        if let Some(value) = flag("validation.disallowDocTypeDecl") {
            validation.disallow_doc_type_decl = value;
        }
        if let Some(value) = flag("validation.resolveExternalEntities") {
            validation.resolve_external_entities = value;
        }
        if let Some(value) = get("validation.debounce").and_then(Value::as_u64) {
            // More than 10 s would look like validation never runs.
            validation.debounce_ms = value.min(10_000);
        }
        match get("maxFileSize") {
            Some(Value::Number(number)) => {
                settings.max_file_size = number
                    .as_u64()
                    .map(|limit| (limit > 0).then_some(limit as usize))
                    .unwrap_or(settings.max_file_size);
            }
            Some(Value::Null) => settings.max_file_size = None,
            _ => {}
        }

        if let Some(value) = flag("completion.autoCloseTags") {
            settings.auto_close_tags = value;
        }
        if let Some(value) = flag("symbols.enabled") {
            settings.symbols_enabled = value;
        }
        if let Some(value) = number("symbols.maxItemsComputed") {
            settings.symbols_max_items = Some(value);
        }
        if let Some(value) = flag("colors.enabled") {
            settings.colors_enabled = value;
        }
        if let Some(catalogs) = get("catalogs").and_then(Value::as_array) {
            settings.catalogs = catalogs
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
        }
        if let Some(value) = flag("autoDetectCatalogs") {
            settings.auto_detect_catalogs = value;
        }
        if let Some(associations) = get("fileAssociations").and_then(Value::as_array) {
            settings.file_associations = associations
                .iter()
                .filter_map(|association| {
                    let pattern = association.get("pattern")?.as_str()?.trim();
                    let system_id = association.get("systemId")?.as_str()?.trim();
                    (!pattern.is_empty() && !system_id.is_empty()).then(|| FileAssociation {
                        pattern: pattern.to_owned(),
                        system_id: system_id.to_owned(),
                    })
                })
                .collect();
        }
        settings
    }

    /// Published diagnostics depend on these settings.
    pub fn same_validation(&self, other: &Self) -> bool {
        self.validation == other.validation
            && self.file_associations == other.file_associations
            && self.catalogs == other.catalogs
            && self.auto_detect_catalogs == other.auto_detect_catalogs
            && self.max_file_size == other.max_file_size
    }
}

/// Value of a dotted key (`format.enabled`), also accepting a flat key
/// (`"format.enabled": true`).
fn lookup<'a>(root: &'a Map<String, Value>, path: &str) -> Option<&'a Value> {
    if let Some(value) = root.get(path) {
        return Some(value);
    }
    let (head, rest) = path.split_once('.')?;
    lookup(root.get(head)?.as_object()?, rest)
}

/// `xml` section of the received settings: `{"settings": {"xml": …}}`,
/// `{"xml": …}` or the content of the section itself.
pub fn xml_section(value: &Value) -> Option<&Value> {
    let object = value.as_object()?;
    if let Some(settings) = object.get("settings").filter(|value| value.is_object()) {
        return xml_section(settings);
    }
    match object.get("xml") {
        Some(section) => section.is_object().then_some(section),
        None => Some(value),
    }
}

/// Recursive merge: objects are merged, other values of `overlay` replace
/// those of `base`.
pub fn merge(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Object(base), Value::Object(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(key) {
                    Some(existing) => merge(existing, value),
                    None if value.is_null() => {}
                    None => {
                        base.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base, overlay) if !overlay.is_null() => *base = overlay.clone(),
        _ => {}
    }
}

/// Limits the number of symbols (pre-order traversal, children included).
pub fn limit_symbols(symbols: &mut Vec<Value>, limit: usize) {
    fn walk(symbols: &mut Vec<Value>, remaining: &mut usize) {
        let mut kept = 0;
        for symbol in symbols.iter_mut() {
            if *remaining == 0 {
                break;
            }
            *remaining -= 1;
            kept += 1;
            if let Some(Value::Array(children)) = symbol.get_mut("children") {
                walk(children, remaining);
            }
        }
        symbols.truncate(kept);
    }
    let mut remaining = limit;
    walk(symbols, &mut remaining);
}

/// Schemas associated with `document` by `xml.fileAssociations`.
///
/// A pattern without `/` applies to the file name; otherwise it is matched
/// against the path relative to each workspace folder, then against the
/// absolute path. `**` spans several segments, `*` and `?` one segment,
/// `{a,b}` alternatives. The `systemId` is an absolute path, a `file://`
/// URI or a path relative to the workspace folder (to the document's
/// directory outside a workspace); a remote URL is only kept if an XML
/// catalog (`catalogs`) maps it to a local file.
pub fn associated_schemas(
    associations: &[FileAssociation],
    roots: &[PathBuf],
    document: &Path,
    catalogs: &crate::catalog::Catalogs,
) -> Vec<PathBuf> {
    let mut schemas = Vec::new();
    let document_text = slashes(document);
    let file_name = document
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let root = roots.iter().find(|root| document.starts_with(root));
    for association in associations {
        let pattern = association.pattern.replace('\\', "/");
        let pattern = pattern.strip_prefix("./").unwrap_or(&pattern);
        let matches = if !pattern.contains('/') {
            glob_match(pattern, &file_name)
        } else {
            root.and_then(|root| document.strip_prefix(root).ok())
                .is_some_and(|relative| glob_match(pattern, &slashes(relative)))
                || glob_match(pattern, &document_text)
                || (!pattern.starts_with('/')
                    && !pattern.starts_with("**")
                    && glob_match(&format!("**/{pattern}"), &document_text)
                    && root.is_none())
        };
        if !matches {
            continue;
        }
        let system_id = association.system_id.as_str();
        let path = if let Some(path) = catalogs.resolve_location(system_id) {
            path
        } else if system_id.starts_with("file:") {
            crate::uri_to_path(system_id)
        } else if system_id.contains("://") {
            continue;
        } else if Path::new(system_id).is_absolute() {
            PathBuf::from(system_id)
        } else {
            let base = root
                .cloned()
                .or_else(|| document.parent().map(Path::to_path_buf))
                .unwrap_or_default();
            base.join(system_id)
        };
        if !schemas.contains(&path) {
            schemas.push(path);
        }
    }
    schemas
}

fn slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Glob matching (`**`, `*`, `?`, `{a,b}`) on `/`-separated paths.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    // Expansion of `{a,b}` alternatives (not nested).
    if let Some(open) = pattern.find('{')
        && let Some(close) = pattern[open..].find('}').map(|index| open + index)
    {
        let (prefix, suffix) = (&pattern[..open], &pattern[close + 1..]);
        return pattern[open + 1..close]
            .split(',')
            .any(|alternative| glob_match(&format!("{prefix}{alternative}{suffix}"), text));
    }
    let pattern = pattern.chars().collect::<Vec<_>>();
    let text = text.chars().collect::<Vec<_>>();
    // Dynamic programming: matches[j] = pattern[..i] matches text[..j].
    let mut matches = vec![false; text.len() + 1];
    matches[0] = true;
    let mut index = 0;
    while index < pattern.len() {
        let mut next = vec![false; text.len() + 1];
        match pattern[index] {
            '*' if pattern.get(index + 1) == Some(&'*') => {
                // `**` matches any sequence; `**/` zero or more complete
                // segments.
                let slash = pattern.get(index + 2) == Some(&'/');
                let mut reachable = false;
                for position in 0..=text.len() {
                    next[position] = if slash {
                        matches[position]
                            || (reachable && position > 0 && text[position - 1] == '/')
                    } else {
                        reachable || matches[position]
                    };
                    reachable |= matches[position];
                }
                index += if slash { 3 } else { 2 };
                matches = next;
                continue;
            }
            '*' => {
                let mut reachable = false;
                for position in 0..=text.len() {
                    if matches[position] {
                        reachable = true;
                    } else if position > 0 && text[position - 1] == '/' {
                        reachable = false;
                    }
                    next[position] = reachable;
                }
            }
            '?' => {
                for position in 1..=text.len() {
                    next[position] = matches[position - 1] && text[position - 1] != '/';
                }
            }
            literal => {
                for position in 1..=text.len() {
                    next[position] = matches[position - 1] && text[position - 1] == literal;
                }
            }
        }
        matches = next;
        index += 1;
    }
    matches[text.len()]
}

#[cfg(test)]
mod tests;
