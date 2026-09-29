//! Réglages utilisateur du serveur (section `xml`, nommage LemMinX).
//!
//! Les réglages proviennent de `initializationOptions` (`{"settings": {"xml":
//! …}}`, `{"xml": …}` ou directement le contenu de la section), puis de
//! `workspace/configuration` (section `xml`) et de
//! `workspace/didChangeConfiguration`. La lecture est tolérante : une clé
//! absente ou d'un type inattendu garde sa valeur par défaut, et les valeurs
//! par défaut reproduisent le comportement historique du serveur.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use xml_core::{EmptyElements, FormatOptions, SplitAttributes};

/// Réglages `xml.*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub format: FormatSettings,
    pub validation: ValidationSettings,
    /// `xml.completion.autoCloseTags` : propose la balise fermante après `>`.
    pub auto_close_tags: bool,
    /// `xml.symbols.enabled`.
    pub symbols_enabled: bool,
    /// `xml.symbols.maxItemsComputed` (`None` : pas de limite).
    pub symbols_max_items: Option<usize>,
    /// `xml.colors.enabled`.
    pub colors_enabled: bool,
    /// `xml.catalogs` : chemins de catalogues XML OASIS, bruts (résolus par
    /// [`crate::catalog::catalog_paths`] par rapport aux dossiers de
    /// l'espace de travail).
    pub catalogs: Vec<String>,
    /// `xml.autoDetectCatalogs` (extension, `false` par défaut) : utilise
    /// aussi `catalog.xml` à la racine de chaque dossier de l'espace de
    /// travail, s'il s'agit d'un catalogue OASIS.
    pub auto_detect_catalogs: bool,
    /// `xml.fileAssociations`.
    pub file_associations: Vec<FileAssociation>,
}

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
        }
    }
}

/// Réglages `xml.format.*`.
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
    /// Valeurs de repli lorsque la requête ne fournit pas l'option LSP
    /// correspondante.
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
            insert_spaces: None,
            tab_size: None,
            trim_final_newlines: None,
            insert_final_newline: None,
            trim_trailing_whitespace: None,
        }
    }
}

impl FormatSettings {
    /// Applique les réglages `xml.format.*` à des options par défaut (avant
    /// les `FormattingOptions` de la requête, qui restent prioritaires).
    pub fn apply(&self, options: &mut FormatOptions) {
        options.split_attributes = self.split_attributes;
        options.max_line_width = self.max_line_width;
        options.preserved_newlines = self.preserved_newlines;
        options.closing_bracket_new_line = self.closing_bracket_new_line;
        options.empty_elements = self.empty_elements;
        options.preserve_attribute_line_breaks = self.preserve_attribute_line_breaks;
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
    /// Valide avec les schémas chargés et signale les erreurs de chargement.
    #[default]
    Always,
    /// Aucune validation XSD.
    Never,
    /// Valide seulement si tous les schémas se chargent sans erreur (les
    /// erreurs de chargement restent signalées).
    OnValidSchema,
}

/// Sévérité du diagnostic `xml.validation.noGrammar`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NoGrammar {
    #[default]
    Ignore,
    Hint,
    Info,
    Warning,
}

impl NoGrammar {
    /// Sévérité LSP (`None` : pas de diagnostic).
    pub fn severity(self) -> Option<u8> {
        match self {
            Self::Ignore => None,
            Self::Warning => Some(2),
            Self::Info => Some(3),
            Self::Hint => Some(4),
        }
    }
}

/// Réglages `xml.validation.*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationSettings {
    /// `xml.validation.enabled` : désactive tous les diagnostics.
    pub enabled: bool,
    pub schema: SchemaValidation,
    pub no_grammar: NoGrammar,
    /// `xml.validation.disallowDocTypeDecl` : signale toute déclaration
    /// `<!DOCTYPE>`.
    pub disallow_doc_type_decl: bool,
    /// `xml.validation.resolveExternalEntities` : les entités générales
    /// externes référencées doivent être résolubles (jamais lues).
    pub resolve_external_entities: bool,
}

impl Default for ValidationSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            schema: SchemaValidation::Always,
            no_grammar: NoGrammar::Ignore,
            disallow_doc_type_decl: false,
            resolve_external_entities: false,
        }
    }
}

/// Association `xml.fileAssociations` : les fichiers correspondant à
/// `pattern` (glob) sont validés avec le schéma `system_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAssociation {
    pub pattern: String,
    pub system_id: String,
}

impl Settings {
    /// Lit la section `xml` (ou son contenu) ; tolérant aux types invalides.
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
        format.insert_spaces = flag("format.insertSpaces");
        format.tab_size = number("format.tabSize").map(|size| size.min(16));
        format.trim_final_newlines = flag("format.trimFinalNewlines");
        format.insert_final_newline = flag("format.insertFinalNewline");
        format.trim_trailing_whitespace = flag("format.trimTrailingWhitespace");

        let validation = &mut settings.validation;
        if let Some(value) = flag("validation.enabled") {
            validation.enabled = value;
        }
        // `xml.validation.schema` : objet `{enabled}` (LemMinX récent) ou
        // booléen (anciennes versions).
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

    /// Les diagnostics publiés dépendent de ces réglages.
    pub fn same_validation(&self, other: &Self) -> bool {
        self.validation == other.validation
            && self.file_associations == other.file_associations
            && self.catalogs == other.catalogs
            && self.auto_detect_catalogs == other.auto_detect_catalogs
    }
}

/// Valeur d'une clé pointée (`format.enabled`), en acceptant aussi une clé
/// plate (`"format.enabled": true`).
fn lookup<'a>(root: &'a Map<String, Value>, path: &str) -> Option<&'a Value> {
    if let Some(value) = root.get(path) {
        return Some(value);
    }
    let (head, rest) = path.split_once('.')?;
    lookup(root.get(head)?.as_object()?, rest)
}

/// Section `xml` des réglages reçus : `{"settings": {"xml": …}}`,
/// `{"xml": …}` ou le contenu de la section lui-même.
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

/// Fusion récursive : les objets sont fusionnés, les autres valeurs de
/// `overlay` remplacent celles de `base`.
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

/// Limite le nombre de symboles (parcours préfixe, enfants compris).
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

/// Schémas associés à `document` par `xml.fileAssociations`.
///
/// Un motif sans `/` s'applique au nom du fichier ; sinon il est comparé au
/// chemin relatif à chaque dossier de l'espace de travail, puis au chemin
/// absolu. `**` couvre plusieurs segments, `*` et `?` un segment, `{a,b}`
/// des alternatives. Le `systemId` est un chemin absolu, une URI `file://`
/// ou un chemin relatif au dossier de l'espace de travail (au dossier du
/// document hors espace de travail) ; une URL distante n'est retenue que si
/// un catalogue XML (`catalogs`) l'associe à un fichier local.
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

/// Correspondance glob (`**`, `*`, `?`, `{a,b}`) sur des chemins à `/`.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    // Développement des alternatives `{a,b}` (non imbriquées).
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
    // Programmation dynamique : matches[j] = pattern[..i] couvre text[..j].
    let mut matches = vec![false; text.len() + 1];
    matches[0] = true;
    let mut index = 0;
    while index < pattern.len() {
        let mut next = vec![false; text.len() + 1];
        match pattern[index] {
            '*' if pattern.get(index + 1) == Some(&'*') => {
                // `**` couvre n'importe quelle suite ; `**/` zéro ou plusieurs
                // segments complets.
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
mod tests {
    use serde_json::json;

    use super::*;
    use crate::catalog::Catalogs;

    #[test]
    fn missing_or_invalid_settings_keep_defaults() {
        let defaults = Settings::default();
        assert_eq!(Settings::from_value(&Value::Null), defaults);
        assert_eq!(Settings::from_value(&json!({})), defaults);
        assert_eq!(Settings::from_value(&json!({"xml": null})), defaults);
        assert_eq!(Settings::from_value(&json!("xml")), defaults);
        assert_eq!(
            Settings::from_value(&json!({"xml": {
                "format": {"enabled": "no", "maxLineWidth": -3, "splitAttributes": "diagonal",
                           "emptyElements": 1, "tabSize": "4", "preservedNewlines": null},
                "validation": {"enabled": 0, "schema": {"enabled": "sometimes"}, "noGrammar": true},
                "completion": [],
                "symbols": {"maxItemsComputed": "10"},
                "catalogs": "catalog.xml",
                "autoDetectCatalogs": "yes",
                "fileAssociations": [{"pattern": 1, "systemId": "a.xsd"}, {"pattern": "*.xml"}, "x"],
            }})),
            defaults
        );
        // Les valeurs par défaut du formatage sont celles de `FormatOptions`.
        let mut options = FormatOptions::default();
        defaults.format.apply(&mut options);
        assert_eq!(options, FormatOptions::default());
    }

    #[test]
    fn reads_every_lemminx_setting() {
        let settings = Settings::from_value(&json!({"settings": {"xml": {
            "format": {
                "enabled": false, "splitAttributes": "alignWithFirstAttr", "maxLineWidth": 80,
                "preservedNewlines": 2, "closingBracketNewLine": true, "emptyElements": "collapse",
                "preserveAttributeLineBreaks": false, "insertSpaces": false, "tabSize": 4,
                "trimFinalNewlines": false,
            },
            "validation": {
                "enabled": false, "schema": {"enabled": "onValidSchema"}, "noGrammar": "warning",
                "disallowDocTypeDecl": true, "resolveExternalEntities": true,
            },
            "completion": {"autoCloseTags": false},
            "symbols": {"enabled": false, "maxItemsComputed": 10},
            "colors": {"enabled": false},
            "catalogs": ["catalog.xml", 3],
            "autoDetectCatalogs": true,
            "fileAssociations": [{"pattern": "**/*.pom", "systemId": "maven.xsd"}],
        }}}));
        assert!(settings.auto_detect_catalogs);
        assert!(!settings.format.enabled);
        assert_eq!(
            settings.format.split_attributes,
            SplitAttributes::AlignWithFirstAttr
        );
        assert_eq!(settings.format.max_line_width, 80);
        assert_eq!(settings.format.empty_elements, EmptyElements::Collapse);
        let mut options = FormatOptions::default();
        settings.format.apply(&mut options);
        assert_eq!(
            options,
            FormatOptions {
                tab_size: 4,
                insert_spaces: false,
                trim_final_newlines: false,
                preserved_newlines: 2,
                split_attributes: SplitAttributes::AlignWithFirstAttr,
                max_line_width: 80,
                closing_bracket_new_line: true,
                empty_elements: EmptyElements::Collapse,
                preserve_attribute_line_breaks: false,
                ..FormatOptions::default()
            }
        );
        assert_eq!(
            settings.validation,
            ValidationSettings {
                enabled: false,
                schema: SchemaValidation::OnValidSchema,
                no_grammar: NoGrammar::Warning,
                disallow_doc_type_decl: true,
                resolve_external_entities: true,
            }
        );
        assert!(!settings.auto_close_tags);
        assert!(!settings.symbols_enabled);
        assert_eq!(settings.symbols_max_items, Some(10));
        assert!(!settings.colors_enabled);
        assert_eq!(settings.catalogs, vec!["catalog.xml"]);
        assert_eq!(
            settings.file_associations,
            vec![FileAssociation {
                pattern: "**/*.pom".to_owned(),
                system_id: "maven.xsd".to_owned(),
            }]
        );
    }

    #[test]
    fn accepts_alternative_setting_shapes() {
        // Contenu de la section, booléens des anciennes versions de LemMinX
        // et clés pointées.
        let settings = Settings::from_value(&json!({
            "format.splitAttributes": true,
            "validation": {"schema": false},
        }));
        assert_eq!(
            settings.format.split_attributes,
            SplitAttributes::SplitNewLine
        );
        assert_eq!(settings.validation.schema, SchemaValidation::Never);
        let settings = Settings::from_value(
            &json!({"xml": {"format": {"splitAttributes": "indent"}, "validation": {"schema": {"enabled": "never"}}}}),
        );
        assert_eq!(
            settings.format.split_attributes,
            SplitAttributes::SplitNewLine
        );
        assert_eq!(settings.validation.schema, SchemaValidation::Never);
    }

    #[test]
    fn merges_settings_recursively() {
        let mut base = json!({"format": {"enabled": false, "tabSize": 4}, "catalogs": ["a"]});
        merge(
            &mut base,
            &json!({"format": {"tabSize": 8}, "catalogs": ["b"], "colors": null}),
        );
        assert_eq!(
            base,
            json!({"format": {"enabled": false, "tabSize": 8}, "catalogs": ["b"]})
        );
    }

    #[test]
    fn limits_nested_symbols_in_document_order() {
        let mut symbols = vec![
            json!({"name": "a", "children": [{"name": "b"}, {"name": "c", "children": [{"name": "d"}]}]}),
            json!({"name": "e"}),
        ];
        limit_symbols(&mut symbols, 3);
        assert_eq!(
            symbols,
            vec![json!({"name": "a", "children": [{"name": "b"}, {"name": "c", "children": []}]})]
        );
    }

    #[test]
    fn matches_glob_patterns() {
        assert!(glob_match("*.xml", "pom.xml"));
        assert!(!glob_match("*.xml", "dir/pom.xml"));
        assert!(glob_match("**/*.xml", "pom.xml"));
        assert!(glob_match("**/*.xml", "a/b/pom.xml"));
        assert!(glob_match("src/**/beans-?.xml", "src/main/beans-1.xml"));
        assert!(glob_match("src/**/beans-?.xml", "src/beans-1.xml"));
        assert!(!glob_match("src/**/beans-?.xml", "src/beans-10.xml"));
        assert!(glob_match("config/*.{xml,xsl}", "config/a.xsl"));
        assert!(!glob_match("config/*.{xml,xsl}", "config/a.xsd"));
        assert!(glob_match("**", "any/thing"));
        assert!(glob_match("/abs/**/x.xml", "/abs/x.xml"));
        assert!(!glob_match("a*b", "a/b"));
    }

    #[test]
    fn resolves_file_associations() {
        let root = PathBuf::from("/work/project");
        let associations = [
            FileAssociation {
                pattern: "**/*.pom".to_owned(),
                system_id: "schemas/maven.xsd".to_owned(),
            },
            FileAssociation {
                pattern: "beans.xml".to_owned(),
                system_id: "file:///opt/spring%20beans.xsd".to_owned(),
            },
            FileAssociation {
                pattern: "config/*.xml".to_owned(),
                system_id: "/abs/config.xsd".to_owned(),
            },
            FileAssociation {
                pattern: "*.xml".to_owned(),
                system_id: "https://example.com/remote.xsd".to_owned(),
            },
        ];
        let roots = [root.clone()];
        let none = Catalogs::default();
        assert_eq!(
            associated_schemas(&associations, &roots, &root.join("a/b/project.pom"), &none),
            vec![root.join("schemas/maven.xsd")]
        );
        assert_eq!(
            associated_schemas(&associations, &roots, &root.join("x/beans.xml"), &none),
            vec![PathBuf::from("/opt/spring beans.xsd")]
        );
        assert_eq!(
            associated_schemas(&associations, &roots, &root.join("config/app.xml"), &none),
            vec![PathBuf::from("/abs/config.xsd")]
        );
        assert!(
            associated_schemas(&associations, &roots, &root.join("other/app.xml"), &none)
                .is_empty()
        );
        // Hors espace de travail : relatif au dossier du document.
        assert_eq!(
            associated_schemas(&associations, &[], Path::new("/tmp/x/a.pom"), &none),
            vec![PathBuf::from("/tmp/x/schemas/maven.xsd")]
        );

        // Un catalogue rend l'URL distante utilisable.
        let directory =
            std::env::temp_dir().join(format!("xml-lsp-associations {}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let catalog = directory.join("catalog.xml");
        std::fs::write(
            &catalog,
            r#"<catalog xmlns="urn:oasis:names:tc:entity:xmlns:xml:catalog">
                <system systemId="https://example.com/remote.xsd" uri="local.xsd"/>
            </catalog>"#,
        )
        .unwrap();
        let catalogs = Catalogs::new(vec![catalog]);
        assert_eq!(
            associated_schemas(
                &associations,
                &roots,
                &root.join("other/app.xml"),
                &catalogs
            ),
            vec![directory.join("local.xsd")]
        );
        let _ = std::fs::remove_dir_all(&directory);
    }
}
