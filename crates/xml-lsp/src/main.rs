//! Serveur LSP XML natif.

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use lsp_server::{Connection, Message, Notification, Response};
use quick_xml::{Reader, events::Event};
use serde_json::{Value, json};
use xml_core::{XmlDiagnostic, auto_close_tag, complete_xml, format_xml, parse_xml};
use xsd_core::{
    XsdSchema, complete_attribute_values, complete_attributes, complete_elements, merge_schemas,
    parse_xsd, resolve_schema_dependencies, resolve_schema_locations, validate_document_located,
};

#[cfg(test)]
const INITIALIZE_METHOD: &str = "initialize";
const EXIT_METHOD: &str = "exit";
const DID_OPEN_METHOD: &str = "textDocument/didOpen";
const DID_CHANGE_METHOD: &str = "textDocument/didChange";
const DID_CLOSE_METHOD: &str = "textDocument/didClose";
const PUBLISH_DIAGNOSTICS_METHOD: &str = "textDocument/publishDiagnostics";
const FORMATTING_METHOD: &str = "textDocument/formatting";
const RANGE_FORMATTING_METHOD: &str = "textDocument/rangeFormatting";
const SYMBOL_METHOD: &str = "textDocument/documentSymbol";
const HOVER_METHOD: &str = "textDocument/hover";
const DEFINITION_METHOD: &str = "textDocument/definition";
const REFERENCES_METHOD: &str = "textDocument/references";
const COMPLETION_METHOD: &str = "textDocument/completion";

type SchemaCache = HashMap<PathBuf, (SystemTime, XsdSchema)>;

#[derive(Debug)]
struct SchemaLoadError {
    path: PathBuf,
    message: String,
    offset: usize,
}

struct XmlLanguageServer {
    documents: HashMap<String, String>,
    schema_cache: SchemaCache,
    schema_index: HashMap<String, Vec<PathBuf>>,
}

impl XmlLanguageServer {
    fn new() -> Self {
        Self {
            documents: HashMap::new(),
            schema_cache: HashMap::new(),
            schema_index: HashMap::new(),
        }
    }

    fn handle_notification(
        &mut self,
        connection: &Connection,
        notification: Notification,
    ) -> Result<bool, Box<dyn Error + Send + Sync>> {
        if notification.method == EXIT_METHOD {
            return Ok(true);
        }
        if notification.method == DID_CLOSE_METHOD {
            if let Some(uri) = notification
                .params
                .get("textDocument")
                .and_then(|document| document.get("uri"))
                .and_then(Value::as_str)
            {
                self.documents.remove(uri);
                connection.sender.send(
                    Notification {
                        method: PUBLISH_DIAGNOSTICS_METHOD.to_owned(),
                        params: json!({"uri": uri, "diagnostics": []}),
                    }
                    .into(),
                )?;
            }
            return Ok(false);
        }

        let Some((uri, text)) = (match notification.method.as_str() {
            DID_OPEN_METHOD => Self::opened_document(&notification.params),
            DID_CHANGE_METHOD => {
                let Some(uri) = notification
                    .params
                    .get("textDocument")
                    .and_then(|document| document.get("uri"))
                    .and_then(Value::as_str)
                else {
                    return Ok(false);
                };
                Self::changed_document(&notification.params, self.documents.get(uri))
            }
            _ => None,
        }) else {
            return Ok(false);
        };

        self.documents.insert(uri.clone(), text.clone());
        let diagnostics = parse_xml(&text).diagnostics;
        let schema_diagnostics = self.schema_diagnostics(&uri, &text);
        connection.sender.send(
            Notification {
                method: PUBLISH_DIAGNOSTICS_METHOD.to_owned(),
                params: diagnostics_params(&uri, &text, &diagnostics, &schema_diagnostics),
            }
            .into(),
        )?;

        if is_xsd_uri(&uri) {
            let dependent_uris = self
                .documents
                .keys()
                .filter(|document_uri| *document_uri != &uri)
                .filter(|document_uri| self.references_schema(document_uri, &uri))
                .cloned()
                .collect::<Vec<_>>();
            for dependent_uri in dependent_uris {
                let Some(dependent_source) = self.documents.get(&dependent_uri).cloned() else {
                    continue;
                };
                let dependent_xml_diagnostics = parse_xml(&dependent_source).diagnostics;
                let dependent_xsd_diagnostics =
                    self.schema_diagnostics(&dependent_uri, &dependent_source);
                connection.sender.send(
                    Notification {
                        method: PUBLISH_DIAGNOSTICS_METHOD.to_owned(),
                        params: diagnostics_params(
                            &dependent_uri,
                            &dependent_source,
                            &dependent_xml_diagnostics,
                            &dependent_xsd_diagnostics,
                        ),
                    }
                    .into(),
                )?;
            }
        }

        Ok(false)
    }

    fn opened_document(params: &Value) -> Option<(String, String)> {
        let document = params.get("textDocument")?;
        Some((
            document.get("uri")?.as_str()?.to_owned(),
            document.get("text")?.as_str()?.to_owned(),
        ))
    }

    fn completion(&mut self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?.clone();
        let position = params.get("position")?;
        let line = position.get("line")?.as_u64()? as usize;
        let character = position.get("character")?.as_u64()? as usize;
        let offset = offset_at(&source, line, character);
        let mut items = complete_xml(&source, offset)
            .into_iter()
            .map(|completion| {
                json!({
                    "label": completion.label,
                    "insertText": completion.insert_text,
                })
            })
            .collect::<Vec<_>>();
        if let Some(completion) = auto_close_tag(&source, offset) {
            items.push(json!({
                "label": completion.label,
                "insertText": completion.insert_text,
            }));
        }
        items.extend(self.schema_completions(uri, &source, offset));
        items.extend(self.schema_attributes(uri, &source, offset));
        items.extend(self.schema_attribute_values(uri, &source, offset));
        deduplicate_completion_items(&mut items);
        Some(json!({"isIncomplete": false, "items": items}))
    }

    fn load_schema(&mut self, uri: &str, source: &str) -> Option<XsdSchema> {
        let document_path = uri_to_path(uri);
        let references = resolve_schema_locations(schema_resolution_source(source), document_path)
            .unwrap_or_default();
        let (schemas, _) =
            load_schema_graph(references, &mut self.schema_cache, &mut self.schema_index);
        (!schemas.is_empty()).then(|| merge_schemas(schemas))
    }

    fn schema_completions(&mut self, uri: &str, source: &str, offset: usize) -> Vec<Value> {
        self.load_schema(uri, source)
            .into_iter()
            .flat_map(|schema| complete_elements(source, offset, &schema))
            .map(|completion| {
                json!({
                    "label": completion.label,
                    "insertText": completion.insert_text,
                })
            })
            .collect()
    }

    fn schema_attributes(&mut self, uri: &str, source: &str, offset: usize) -> Vec<Value> {
        self.load_schema(uri, source)
            .into_iter()
            .flat_map(|schema| complete_attributes(source, offset, &schema))
            .map(|completion| {
                json!({
                    "label": completion.label,
                    "insertText": completion.insert_text,
                })
            })
            .collect()
    }

    fn schema_attribute_values(&mut self, uri: &str, source: &str, offset: usize) -> Vec<Value> {
        self.load_schema(uri, source)
            .into_iter()
            .flat_map(|schema| complete_attribute_values(source, offset, &schema))
            .map(|completion| {
                json!({
                    "label": completion.label,
                    "insertText": completion.insert_text,
                })
            })
            .collect()
    }

    fn references_schema(&self, document_uri: &str, schema_uri: &str) -> bool {
        let Some(source) = self.documents.get(document_uri) else {
            return false;
        };
        let document_path = uri_to_path(document_uri);
        let schema_path = uri_to_path(schema_uri);
        resolve_schema_locations(schema_resolution_source(source), document_path)
            .map(|references| {
                references
                    .into_iter()
                    .any(|reference| reference.path == schema_path)
            })
            .unwrap_or(false)
    }

    fn schema_diagnostics(&mut self, uri: &str, source: &str) -> Vec<Value> {
        let document_path = uri_to_path(uri);
        let references =
            match resolve_schema_locations(schema_resolution_source(source), &document_path) {
                Ok(references) => references,
                Err(error) => return vec![xsd_error_diagnostic(error)],
            };
        let (schemas, errors) =
            load_schema_graph(references, &mut self.schema_cache, &mut self.schema_index);
        let mut diagnostics = errors
            .into_iter()
            .map(xsd_schema_error_diagnostic)
            .collect::<Vec<_>>();
        if !schemas.is_empty() {
            let schema = merge_schemas(schemas);
            diagnostics.extend(validate_document_located(source, &schema).into_iter().map(
                |diagnostic| {
                    xsd_error_diagnostic_at(&diagnostic.message, source, diagnostic.offset)
                },
            ));
        }

        diagnostics
    }

    fn references(&mut self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?.clone();
        let position = params.get("position")?;
        let line = position.get("line")?.as_u64()? as usize;
        let character = position.get("character")?.as_u64()? as usize;
        let offset = offset_at(&source, line, character);
        let name = element_name_at(&source, offset)?;
        let mut locations = Vec::new();
        if let Some((path, schema_source, declaration_offset)) =
            self.xsd_definition(uri, &source, &name)
        {
            locations.push(json!({
                "uri": path_to_uri(&path),
                "range": {
                    "start": position_at(&schema_source, declaration_offset),
                    "end": position_at(&schema_source, declaration_offset + name.len()),
                },
            }));
        }
        let mut search_from = 0usize;
        while let Some(relative) = source[search_from..].find(&format!("<{name}")) {
            let start = search_from + relative;
            locations.push(json!({
                "uri": uri,
                "range": {
                    "start": position_at(&source, start),
                    "end": position_at(&source, start + name.len() + 1),
                },
            }));
            search_from = start + name.len() + 1;
        }
        Some(Value::Array(locations))
    }

    fn definition(&mut self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?.clone();
        let position = params.get("position")?;
        let line = position.get("line")?.as_u64()? as usize;
        let character = position.get("character")?.as_u64()? as usize;
        let offset = offset_at(&source, line, character);
        let name = element_name_at(&source, offset)?;
        if let Some((path, schema_source, offset)) = self.xsd_definition(uri, &source, &name) {
            return Some(json!([{
                "uri": path_to_uri(&path),
                "range": {
                    "start": position_at(&schema_source, offset),
                    "end": position_at(&schema_source, offset + name.len()),
                },
            }]));
        }
        let declaration = source.find(&format!("<{name}"))?;
        Some(json!([{
            "uri": uri,
            "range": {
                "start": position_at(&source, declaration),
                "end": position_at(&source, declaration + name.len() + 1),
            },
        }]))
    }

    fn xsd_definition(
        &mut self,
        uri: &str,
        source: &str,
        name: &str,
    ) -> Option<(PathBuf, String, usize)> {
        let references =
            resolve_schema_locations(schema_resolution_source(source), uri_to_path(uri)).ok()?;
        let mut queue = references;
        let mut visited = HashSet::new();
        while let Some(reference) = queue.pop() {
            if !visited.insert(reference.path.clone()) {
                continue;
            }
            let schema_source = fs::read_to_string(&reference.path).ok()?;
            if let Some(offset) = xsd_element_name_offset(&schema_source, name) {
                return Some((reference.path, schema_source, offset));
            }
            if let Ok(dependencies) = resolve_schema_dependencies(&schema_source, &reference.path) {
                queue.extend(dependencies);
            }
        }
        None
    }

    fn hover(&mut self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?.clone();
        let position = params.get("position")?;
        let line = position.get("line")?.as_u64()? as usize;
        let character = position.get("character")?.as_u64()? as usize;
        let offset = offset_at(&source, line, character);
        let element_name = element_name_at(&source, offset)?;
        let schema = self.load_schema(uri, &source);
        let detail = schema
            .as_ref()
            .and_then(|schema| {
                schema
                    .elements
                    .iter()
                    .find(|element| element.name == element_name)
            })
            .map(|element| {
                format!(
                    "Élément `<{}>`\n\nType : `{}`",
                    element.name,
                    element.type_name.as_deref().unwrap_or("complexType")
                )
            })
            .unwrap_or_else(|| format!("Élément XML `<{element_name}>`"));
        Some(json!({
            "contents": {"kind": "markdown", "value": detail},
        }))
    }

    fn symbols(&self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?;
        Some(xml_symbols(source))
    }

    fn formatting(&self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?;
        let formatted = format_xml(source).ok()?;
        Some(json!([{
            "range": {
                "start": {"line": 0, "character": 0},
                "end": position_at(source, source.len()),
            },
            "newText": formatted,
        }]))
    }

    fn changed_document(params: &Value, current: Option<&String>) -> Option<(String, String)> {
        let document = params.get("textDocument")?;
        let uri = document.get("uri")?.as_str()?.to_owned();
        let changes = params.get("contentChanges")?.as_array()?;
        let mut text = current.cloned();
        for change in changes {
            let replacement = change.get("text")?.as_str()?;
            if let Some(range) = change.get("range") {
                let current_text = text.as_ref()?;
                let start = range.get("start")?;
                let end = range.get("end")?;
                let start_offset = offset_at(
                    current_text,
                    start.get("line")?.as_u64()? as usize,
                    start.get("character")?.as_u64()? as usize,
                );
                let end_offset = offset_at(
                    current_text,
                    end.get("line")?.as_u64()? as usize,
                    end.get("character")?.as_u64()? as usize,
                );
                let mut updated = current_text.clone();
                updated.replace_range(
                    start_offset.min(updated.len())..end_offset.min(updated.len()),
                    replacement,
                );
                text = Some(updated);
            } else {
                text = Some(replacement.to_owned());
            }
        }
        let text = text?;
        Some((uri, text))
    }
}

fn is_xsd_uri(uri: &str) -> bool {
    uri_to_path(uri)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xsd"))
}

fn deduplicate_completion_items(items: &mut Vec<Value>) {
    let mut seen = HashSet::new();
    items.retain(|item| {
        let key = (
            item.get("label")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            item.get("insertText")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        );
        seen.insert(key)
    });
}

fn schema_resolution_source(source: &str) -> &str {
    source.strip_suffix('<').unwrap_or(source)
}

fn uri_to_path(uri: &str) -> PathBuf {
    let raw = uri.strip_prefix("file://").unwrap_or(uri);
    let raw = if cfg!(windows) && raw.starts_with('/') {
        raw.strip_prefix('/').unwrap_or(raw)
    } else {
        raw
    };
    PathBuf::from(percent_decode(raw))
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) =
                (hex_digit(bytes[index + 1]), hex_digit(bytes[index + 2]))
        {
            decoded.push(high * 16 + low);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn path_to_uri(path: &std::path::Path) -> String {
    let path = encode_uri_path(&path.to_string_lossy().replace('\\', "/"));
    if path.as_bytes().get(1) == Some(&b':') {
        format!("file:///{path}")
    } else if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

fn encode_uri_path(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' | b':' | b'~' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

fn xsd_element_name_offset(source: &str, expected_name: &str) -> Option<usize> {
    let mut reader = Reader::from_str(source);
    let mut search_from = 0usize;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) | Ok(Event::Empty(element)) => {
                let qname = element.name();
                let local = String::from_utf8_lossy(qname.as_ref());
                let event_end = reader.buffer_position() as usize;
                let event_start = source[search_from..event_end]
                    .find('<')
                    .map(|offset| search_from + offset)
                    .unwrap_or(search_from);
                search_from = event_end;
                if !local.ends_with(":element") && local != "element" {
                    continue;
                }
                for attribute in element.attributes().flatten() {
                    if attribute.key.as_ref() == b"name" {
                        let value = attribute.unescape_value().ok()?.into_owned();
                        if value != expected_name {
                            continue;
                        }
                        let attribute_start = source[event_start..event_end]
                            .find("name=\"")
                            .map(|offset| event_start + offset + 6)?;
                        return Some(attribute_start);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

fn load_schema_graph(
    references: Vec<xsd_core::SchemaReference>,
    cache: &mut SchemaCache,
    index: &mut HashMap<String, Vec<PathBuf>>,
) -> (Vec<XsdSchema>, Vec<SchemaLoadError>) {
    let mut queue = references;
    let mut visited = HashSet::new();
    let mut schemas = Vec::new();
    let mut errors = Vec::new();

    while let Some(reference) = queue.pop() {
        let path = reference.path;
        if !visited.insert(path.clone()) {
            continue;
        }
        let modified = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        let schema_source = match fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                errors.push(SchemaLoadError {
                    path: path.clone(),
                    message: format!("impossible de lire le schéma : {error}"),
                    offset: 0,
                });
                continue;
            }
        };
        let schema = if let Some((cached_time, schema)) = cache.get(&path)
            && *cached_time == modified
        {
            schema.clone()
        } else {
            let schema = match parse_xsd(&schema_source) {
                Ok(schema) => schema,
                Err(error) => {
                    errors.push(SchemaLoadError {
                        path: path.clone(),
                        message: format!("schéma XSD invalide : {error}"),
                        offset: xsd_parse_error_offset(&schema_source),
                    });
                    continue;
                }
            };
            cache.insert(path.clone(), (modified, schema.clone()));
            schema
        };
        if let Some(namespace) = &schema.target_namespace {
            let paths = index.entry(namespace.clone()).or_default();
            if !paths.contains(&path) {
                paths.push(path.clone());
            }
        }
        match resolve_schema_dependencies(&schema_source, &path) {
            Ok(dependencies) => queue.extend(dependencies),
            Err(error) => errors.push(SchemaLoadError {
                path: path.clone(),
                message: format!("dépendances XSD invalides : {error}"),
                offset: xsd_parse_error_offset(&schema_source),
            }),
        }
        schemas.push(schema);
    }

    (schemas, errors)
}

fn xsd_error_diagnostic_at(message: &str, source: &str, offset: usize) -> Value {
    let position = position_at(source, offset);
    json!({
        "range": {"start": position, "end": position_at(source, offset)},
        "severity": 1,
        "source": "xml-lsp",
        "code": "xsd-validation",
        "data": {"category": "xsd", "kind": "validation"},
        "message": message,
    })
}

fn xsd_schema_error_diagnostic(error: SchemaLoadError) -> Value {
    json!({
        "range": {
            "start": {"line": 0, "character": 0},
            "end": {"line": 0, "character": 0},
        },
        "severity": 1,
        "source": "xml-lsp",
        "code": "xsd-validation",
        "data": {
            "category": "xsd",
            "kind": "loading",
            "schemaUri": path_to_uri(&error.path),
            "schemaOffset": error.offset,
        },
        "message": error.message,
    })
}

fn xsd_error_diagnostic(diagnostic: impl Into<String>) -> Value {
    json!({
        "range": {
            "start": {"line": 0, "character": 0},
            "end": {"line": 0, "character": 0},
        },
        "severity": 1,
        "source": "xml-lsp",
        "code": "xsd-validation",
        "data": {"category": "xsd", "kind": "loading"},
        "message": diagnostic.into(),
    })
}

fn xsd_parse_error_offset(source: &str) -> usize {
    let mut reader = Reader::from_str(source);
    loop {
        match reader.read_event() {
            Ok(Event::Eof) => return source.len(),
            Err(_) => return reader.buffer_position() as usize,
            Ok(_) => {}
        }
    }
}

fn element_name_at(source: &str, offset: usize) -> Option<String> {
    let prefix = &source[..offset.min(source.len())];
    let opening = prefix.rfind('<')?;
    let fragment = &prefix[opening + 1..];
    let fragment = fragment.strip_prefix('/').unwrap_or(fragment);
    fragment.split_whitespace().next().map(str::to_owned)
}

fn xml_symbols(source: &str) -> Value {
    let mut reader = Reader::from_str(source);
    let mut stack: Vec<(String, usize)> = Vec::new();
    let mut search_from = 0usize;
    let mut symbols = Vec::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                let start = source[search_from..]
                    .find(&format!("<{name}"))
                    .map(|offset| search_from + offset)
                    .unwrap_or(search_from);
                search_from = start + name.len() + 1;
                stack.push((name, start));
            }
            Ok(Event::Empty(element)) => {
                let name = String::from_utf8_lossy(element.name().as_ref()).into_owned();
                let start = source[search_from..]
                    .find(&format!("<{name}"))
                    .map(|offset| search_from + offset)
                    .unwrap_or(search_from);
                search_from = start + name.len() + 1;
                let end = reader.buffer_position() as usize;
                symbols.push(symbol_value(&name, start, end, source));
            }
            Ok(Event::End(_)) => {
                if let Some((name, start)) = stack.pop() {
                    symbols.push(symbol_value(
                        &name,
                        start,
                        reader.buffer_position() as usize,
                        source,
                    ));
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
    Value::Array(symbols)
}

fn symbol_value(name: &str, start: usize, end: usize, source: &str) -> Value {
    json!({
        "name": name,
        "kind": 13,
        "range": {"start": position_at(source, start), "end": position_at(source, end)},
        "selectionRange": {"start": position_at(source, start), "end": position_at(source, start + name.len() + 1)},
    })
}

fn server_capabilities() -> Value {
    json!({
        "completionProvider": {"triggerCharacters": ["<", " ", "/"]},
        "documentFormattingProvider": true,
        "documentRangeFormattingProvider": false,
        "documentSymbolProvider": true,
        "hoverProvider": true,
        "definitionProvider": true,
        "referencesProvider": true,
    })
}

fn offset_at(source: &str, line: usize, character: usize) -> usize {
    let mut current_line = 0;
    let mut current_character = 0;

    for (index, value) in source.char_indices() {
        if current_line == line {
            if current_character >= character {
                return index;
            }
            current_character += value.len_utf16();
        }
        if value == '\n' {
            current_line += 1;
            current_character = 0;
        }
    }

    source.len()
}

fn position_at(source: &str, offset: usize) -> Value {
    let mut line = 0;
    let mut character = 0;
    let bounded_offset = offset.min(source.len());

    for (index, value) in source.char_indices() {
        if index >= bounded_offset {
            break;
        }
        if value == '\n' {
            line += 1;
            character = 0;
        } else {
            character += value.len_utf16();
        }
    }

    json!({ "line": line, "character": character })
}

fn diagnostics_params(
    uri: &str,
    source: &str,
    diagnostics: &[XmlDiagnostic],
    schema_diagnostics: &[Value],
) -> Value {
    let diagnostics = diagnostics.iter().map(|diagnostic| {
        let position = position_at(source, diagnostic.offset);
        json!({
            "range": {
                "start": position,
                "end": position_at(source, diagnostic.offset),
            },
            "severity": 1,
            "source": "xml-lsp",
            "code": diagnostic.code(),
            "message": diagnostic.message,
        })
    });

    let mut diagnostics = diagnostics.collect::<Vec<_>>();
    diagnostics.extend(schema_diagnostics.iter().cloned());
    json!({
        "uri": uri,
        "diagnostics": diagnostics,
    })
}

fn run(connection: Connection) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (initialize_id, _) = connection.initialize_start()?;
    connection.initialize_finish(
        initialize_id,
        json!({
            "capabilities": server_capabilities(),
            "serverInfo": {
                "name": "xml-lsp",
                "version": env!("CARGO_PKG_VERSION"),
            },
        }),
    )?;
    let mut server = XmlLanguageServer::new();

    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if request.method == COMPLETION_METHOD {
                    let result = server
                        .completion(&request.params)
                        .unwrap_or_else(|| json!({"isIncomplete": false, "items": []}));
                    connection
                        .sender
                        .send(Response::new_ok(request.id, result).into())?;
                    continue;
                }

                if request.method == SYMBOL_METHOD {
                    let symbols = server.symbols(&request.params).unwrap_or_else(|| json!([]));
                    connection
                        .sender
                        .send(Response::new_ok(request.id, symbols).into())?;
                    continue;
                }

                if request.method == DEFINITION_METHOD {
                    let definition = server
                        .definition(&request.params)
                        .unwrap_or_else(|| json!([]));
                    connection
                        .sender
                        .send(Response::new_ok(request.id, definition).into())?;
                    continue;
                }

                if request.method == REFERENCES_METHOD {
                    let references = server
                        .references(&request.params)
                        .unwrap_or_else(|| json!([]));
                    connection
                        .sender
                        .send(Response::new_ok(request.id, references).into())?;
                    continue;
                }

                if request.method == HOVER_METHOD {
                    let hover = server.hover(&request.params).unwrap_or(Value::Null);
                    connection
                        .sender
                        .send(Response::new_ok(request.id, hover).into())?;
                    continue;
                }

                if matches!(
                    request.method.as_str(),
                    FORMATTING_METHOD | RANGE_FORMATTING_METHOD
                ) {
                    let edits = server
                        .formatting(&request.params)
                        .unwrap_or_else(|| json!([]));
                    connection
                        .sender
                        .send(Response::new_ok(request.id, edits).into())?;
                    continue;
                }

                if connection.handle_shutdown(&request)? {
                    break;
                }

                let response = Response::new_err(
                    request.id,
                    -32601,
                    format!("unsupported request: {}", request.method),
                );
                connection.sender.send(response.into())?;
            }
            Message::Notification(notification) => {
                if server.handle_notification(&connection, notification)? {
                    break;
                }
            }
            Message::Response(_) => {}
        }
    }

    Ok(())
}

fn main() {
    if std::env::args().any(|argument| argument == "--version") {
        println!("xml-lsp {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let (connection, io_threads) = Connection::stdio();

    if let Err(error) = run(connection) {
        eprintln!("xml-lsp stopped: {error}");
    }

    if let Err(error) = io_threads.join() {
        eprintln!("xml-lsp transport stopped: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_server::{Request, RequestId};
    use std::thread;

    #[test]
    fn removes_duplicate_completion_items() {
        let mut items = vec![
            json!({"label": "child", "insertText": "child"}),
            json!({"label": "child", "insertText": "child"}),
            json!({"label": "other", "insertText": "other"}),
        ];
        deduplicate_completion_items(&mut items);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn tracks_open_workspace_documents_referencing_an_xsd() {
        let schema_path =
            std::env::temp_dir().join(format!("xml-lsp-workspace-{}.xsd", std::process::id()));
        std::fs::write(
            &schema_path,
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="root"/></xs:schema>"#,
        )
        .expect("schema should be written");
        let xml_path = schema_path.with_file_name("workspace.xml");
        let xml_uri = path_to_uri(&xml_path);
        let xsd_uri = path_to_uri(&schema_path);
        let xml_source = format!(
            "<root xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"{}\" />",
            schema_path.file_name().unwrap().to_string_lossy()
        );
        let mut server = XmlLanguageServer::new();
        server.documents.insert(xml_uri.clone(), xml_source);
        server.documents.insert(xsd_uri.clone(), String::new());
        assert!(server.references_schema(&xml_uri, &xsd_uri));
        assert!(is_xsd_uri(&xsd_uri));
        std::fs::remove_file(schema_path).expect("schema should be removed");
    }

    #[test]
    fn serves_initialize_diagnostics_shutdown_and_exit() {
        let (server, client) = Connection::memory();
        let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));

        client
            .sender
            .send(
                Request {
                    id: RequestId::from(1),
                    method: INITIALIZE_METHOD.to_owned(),
                    params: json!({}),
                }
                .into(),
            )
            .expect("initialize should be sent");

        let initialize_response = client
            .receiver
            .recv()
            .expect("initialize response should be received");
        match initialize_response {
            Message::Response(response) => {
                assert_eq!(response.id, RequestId::from(1));
                assert_eq!(
                    response.result,
                    Some(json!({
                        "capabilities": {
                            "completionProvider": {"triggerCharacters": ["<", " ", "/"]},
                            "documentFormattingProvider": true,
                            "documentRangeFormattingProvider": false,
                            "documentSymbolProvider": true,
                            "hoverProvider": true,
                            "definitionProvider": true,
                            "referencesProvider": true,
                        },
                        "serverInfo": {
                            "name": "xml-lsp",
                            "version": env!("CARGO_PKG_VERSION"),
                        },
                    }))
                );
            }
            message => panic!("expected initialize response, got {message:?}"),
        }

        client
            .sender
            .send(
                Notification {
                    method: "initialized".to_owned(),
                    params: json!({}),
                }
                .into(),
            )
            .expect("initialized should be sent");

        client
            .sender
            .send(
                Notification {
                    method: DID_OPEN_METHOD.to_owned(),
                    params: json!({
                        "textDocument": {
                            "uri": "file:///document.xml",
                            "text": "<root>",
                        }
                    }),
                }
                .into(),
            )
            .expect("didOpen should be sent");

        let diagnostics_notification = client
            .receiver
            .recv()
            .expect("diagnostics should be published");
        match diagnostics_notification {
            Message::Notification(notification) => {
                assert_eq!(notification.method, PUBLISH_DIAGNOSTICS_METHOD);
                assert_eq!(
                    notification.params["diagnostics"][0]["range"]["start"],
                    json!({"line": 0, "character": 6})
                );
                assert_eq!(notification.params["diagnostics"][0]["severity"], 1);
                assert_eq!(
                    notification.params["diagnostics"][0]["code"],
                    "xml-structure"
                );
            }
            message => panic!("expected diagnostics notification, got {message:?}"),
        }

        client
            .sender
            .send(
                Notification {
                    method: DID_CHANGE_METHOD.to_owned(),
                    params: json!({
                        "textDocument": {"uri": "file:///document.xml", "version": 2},
                        "contentChanges": [{"text": "<root />"}],
                    }),
                }
                .into(),
            )
            .expect("didChange should be sent");

        let clean_notification = client
            .receiver
            .recv()
            .expect("clean diagnostics should be published");
        match clean_notification {
            Message::Notification(notification) => {
                assert_eq!(notification.method, PUBLISH_DIAGNOSTICS_METHOD);
                assert_eq!(notification.params["diagnostics"], json!([]));
            }
            message => panic!("expected clean diagnostics notification, got {message:?}"),
        }

        client
            .sender
            .send(
                Notification {
                    method: DID_CHANGE_METHOD.to_owned(),
                    params: json!({
                        "textDocument": {"uri": "file:///document.xml", "version": 3},
                        "contentChanges": [{"text": "<root><item /></root><it"}],
                    }),
                }
                .into(),
            )
            .expect("completion source should be sent");
        client
            .receiver
            .recv()
            .expect("diagnostics for completion source should be published");

        client
            .sender
            .send(
                Request {
                    id: RequestId::from(4),
                    method: COMPLETION_METHOD.to_owned(),
                    params: json!({
                        "textDocument": {"uri": "file:///document.xml"},
                        "position": {"line": 0, "character": 24},
                    }),
                }
                .into(),
            )
            .expect("completion should be sent");

        let completion_response = client
            .receiver
            .recv()
            .expect("completion response should be received");
        match completion_response {
            Message::Response(response) => {
                assert_eq!(response.id, RequestId::from(4));
                assert_eq!(
                    response.result,
                    Some(json!({
                        "isIncomplete": false,
                        "items": [{"label": "item", "insertText": "item"}],
                    }))
                );
            }
            message => panic!("expected completion response, got {message:?}"),
        }

        client
            .sender
            .send(
                Notification {
                    method: DID_CHANGE_METHOD.to_owned(),
                    params: json!({
                        "textDocument": {"uri": "file:///document.xml", "version": 4},
                        "contentChanges": [{"text": "<root />"}],
                    }),
                }
                .into(),
            )
            .expect("formatting source should be sent");
        client
            .receiver
            .recv()
            .expect("diagnostics for formatting source should be published");

        client
            .sender
            .send(
                Request {
                    id: RequestId::from(3),
                    method: FORMATTING_METHOD.to_owned(),
                    params: json!({
                        "textDocument": {"uri": "file:///document.xml"},
                        "options": {"tabSize": 2, "insertSpaces": true},
                    }),
                }
                .into(),
            )
            .expect("formatting should be sent");

        let formatting_response = client
            .receiver
            .recv()
            .expect("formatting response should be received");
        match formatting_response {
            Message::Response(response) => {
                assert_eq!(response.id, RequestId::from(3));
                assert_eq!(
                    response.result,
                    Some(json!([{
                        "range": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 0, "character": 8},
                        },
                        "newText": "<root />\n",
                    }]))
                );
            }
            message => panic!("expected formatting response, got {message:?}"),
        }

        client
            .sender
            .send(
                Notification {
                    method: DID_CHANGE_METHOD.to_owned(),
                    params: json!({
                        "textDocument": {"uri": "file:///document.xml", "version": 5},
                        "contentChanges": [{"text": "<root>"}],
                    }),
                }
                .into(),
            )
            .expect("auto-close source should be sent");
        client
            .receiver
            .recv()
            .expect("diagnostics for auto-close source should be published");

        client
            .sender
            .send(
                Request {
                    id: RequestId::from(5),
                    method: COMPLETION_METHOD.to_owned(),
                    params: json!({
                        "textDocument": {"uri": "file:///document.xml"},
                        "position": {"line": 0, "character": 6},
                    }),
                }
                .into(),
            )
            .expect("auto-close completion should be sent");

        let auto_close_response = client
            .receiver
            .recv()
            .expect("auto-close response should be received");
        match auto_close_response {
            Message::Response(response) => {
                assert_eq!(response.id, RequestId::from(5));
                assert_eq!(
                    response.result,
                    Some(json!({
                        "isIncomplete": false,
                        "items": [{"label": "</root>", "insertText": "</root>"}],
                    }))
                );
            }
            message => panic!("expected auto-close response, got {message:?}"),
        }

        client
            .sender
            .send(
                Notification {
                    method: DID_CLOSE_METHOD.to_owned(),
                    params: json!({
                        "textDocument": {"uri": "file:///document.xml"},
                    }),
                }
                .into(),
            )
            .expect("didClose should be sent");

        let close_notification = client
            .receiver
            .recv()
            .expect("clear diagnostics should be published on close");
        match close_notification {
            Message::Notification(notification) => {
                assert_eq!(notification.method, PUBLISH_DIAGNOSTICS_METHOD);
                assert_eq!(notification.params["diagnostics"], json!([]));
            }
            message => panic!("expected close diagnostics notification, got {message:?}"),
        }

        client
            .sender
            .send(
                Request {
                    id: RequestId::from(2),
                    method: "shutdown".to_owned(),
                    params: json!(null),
                }
                .into(),
            )
            .expect("shutdown should be sent");

        let shutdown_response = client
            .receiver
            .recv()
            .expect("shutdown response should be received");
        match shutdown_response {
            Message::Response(response) => {
                assert_eq!(response.id, RequestId::from(2));
                assert_eq!(response.result, Some(json!(null)));
            }
            message => panic!("expected shutdown response, got {message:?}"),
        }

        client
            .sender
            .send(
                Notification {
                    method: EXIT_METHOD.to_owned(),
                    params: json!(null),
                }
                .into(),
            )
            .expect("exit should be sent");

        server_thread.join().expect("server thread should stop");
    }

    #[test]
    fn applies_incremental_lsp_changes() {
        let updated = XmlLanguageServer::changed_document(
            &json!({
                "textDocument": {"uri": "file:///document.xml"},
                "contentChanges": [{
                    "range": {
                        "start": {"line": 0, "character": 6},
                        "end": {"line": 0, "character": 11}
                    },
                    "text": "item"
                }]
            }),
            Some(&"<root>child</root>".to_owned()),
        )
        .unwrap();

        assert_eq!(updated.1, "<root>item</root>");
    }

    #[test]
    fn applies_all_incremental_changes_in_one_notification() {
        let updated = XmlLanguageServer::changed_document(
            &json!({
                "textDocument": {"uri": "file:///document.xml"},
                "contentChanges": [
                    {
                        "range": {
                            "start": {"line": 0, "character": 6},
                            "end": {"line": 0, "character": 11}
                        },
                        "text": "thing"
                    },
                    {
                        "range": {
                            "start": {"line": 0, "character": 12},
                            "end": {"line": 0, "character": 17}
                        },
                        "text": "entry"
                    }
                ]
            }),
            Some(&"<root>child value</root>".to_owned()),
        )
        .unwrap();

        assert_eq!(updated.1, "<root>thing entry</root>");
    }

    #[test]
    fn finds_local_xml_element_references() {
        let mut server = XmlLanguageServer::new();
        server.documents.insert(
            "file:///document.xml".to_owned(),
            "<root><child /><child /></root>".to_owned(),
        );
        let references = server
            .references(&json!({
                "textDocument": {"uri": "file:///document.xml"},
                "position": {"line": 0, "character": 12},
            }))
            .unwrap();

        assert_eq!(references.as_array().unwrap().len(), 2);
    }

    #[test]
    fn resolves_local_xml_element_definitions() {
        let mut server = XmlLanguageServer::new();
        server.documents.insert(
            "file:///document.xml".to_owned(),
            "<root><child /></root>".to_owned(),
        );
        let definition = server
            .definition(&json!({
                "textDocument": {"uri": "file:///document.xml"},
                "position": {"line": 0, "character": 12},
            }))
            .unwrap();

        assert_eq!(definition[0]["uri"], "file:///document.xml");
        assert_eq!(
            definition[0]["range"]["start"],
            json!({"line": 0, "character": 6})
        );
    }

    #[test]
    fn resolves_xsd_element_definitions() {
        let schema_path =
            std::env::temp_dir().join(format!("xml-lsp-definition-{}.xsd", std::process::id()));
        let schema_source = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="root"/><xs:attribute name="root"/></xs:schema>"#;
        std::fs::write(&schema_path, schema_source).expect("schema should be written");

        let document_path = schema_path.with_file_name("definition.xml");
        let uri = path_to_uri(&document_path);
        let schema_name = schema_path.file_name().unwrap().to_string_lossy();
        let source = format!(
            "<root xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"{schema_name}\" />"
        );
        let mut server = XmlLanguageServer::new();
        server.documents.insert(uri.clone(), source);
        let definition = server
            .definition(&json!({
                "textDocument": {"uri": uri},
                "position": {"line": 0, "character": 5},
            }))
            .expect("XSD definition should be found");

        assert_eq!(definition[0]["uri"], path_to_uri(&schema_path));
        let expected_offset = schema_source.find("name=\"root\"").unwrap() + 6;
        assert_eq!(
            definition[0]["range"]["start"],
            position_at(schema_source, expected_offset)
        );
        assert_eq!(
            definition[0]["range"]["end"],
            position_at(schema_source, expected_offset + "root".len())
        );

        std::fs::remove_file(schema_path).expect("schema should be removed");
    }

    #[test]
    fn finds_xsd_and_xml_element_references() {
        let schema_path =
            std::env::temp_dir().join(format!("xml-lsp-references-{}.xsd", std::process::id()));
        let schema_source = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="root"/></xs:schema>"#;
        std::fs::write(&schema_path, schema_source).expect("schema should be written");
        let uri = path_to_uri(&schema_path.with_file_name("references.xml"));
        let schema_name = schema_path.file_name().unwrap().to_string_lossy();
        let source = format!(
            "<root xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"{schema_name}\"><root /></root>"
        );
        let mut server = XmlLanguageServer::new();
        server.documents.insert(uri.clone(), source);
        let references = server
            .references(&json!({
                "textDocument": {"uri": uri},
                "position": {"line": 0, "character": 5},
            }))
            .expect("references should be returned");
        assert_eq!(references.as_array().unwrap().len(), 3);
        assert_eq!(references[0]["uri"], path_to_uri(&schema_path));
        assert_eq!(references[1]["uri"], uri);
        assert_eq!(references[2]["uri"], uri);
        std::fs::remove_file(schema_path).expect("schema should be removed");
    }

    #[test]
    fn provides_hover_for_xml_elements() {
        let mut server = XmlLanguageServer::new();
        server.documents.insert(
            "file:///document.xml".to_owned(),
            "<root><child /></root>".to_owned(),
        );
        let hover = server
            .hover(&json!({
                "textDocument": {"uri": "file:///document.xml"},
                "position": {"line": 0, "character": 5},
            }))
            .unwrap();

        assert!(
            hover["contents"]["value"]
                .as_str()
                .unwrap()
                .contains("root")
        );
    }

    #[test]
    fn builds_document_symbols_from_xml_elements() {
        let symbols = xml_symbols("<root><child /></root>");
        let symbols = symbols.as_array().expect("symbols should be an array");

        assert_eq!(symbols.len(), 2);
        assert_eq!(symbols[0]["name"], "child");
        assert_eq!(symbols[1]["name"], "root");
        assert_eq!(symbols[0]["kind"], 13);
    }

    #[test]
    fn round_trips_file_uris_with_spaces_and_reserved_characters() {
        #[cfg(windows)]
        let path = std::path::PathBuf::from(r"C:\workspace\xml files\schema#1.xsd");
        #[cfg(not(windows))]
        let path = std::path::PathBuf::from("/workspace/xml files/schema#1.xsd");
        let uri = path_to_uri(&path);
        #[cfg(windows)]
        assert_eq!(uri, "file:///C:/workspace/xml%20files/schema%231.xsd");
        #[cfg(not(windows))]
        assert_eq!(uri, "file:///workspace/xml%20files/schema%231.xsd");
        assert_eq!(uri_to_path(&uri), path);
    }

    #[test]
    fn formats_nested_xml_through_lsp_request() {
        let uri = "file:///document.xml";
        let source =
            "<?xml version=\"1.0\" encoding=\"iso-8859-1\" ?><root><outer><inner /></outer></root>";
        let mut server = XmlLanguageServer::new();
        server.documents.insert(uri.to_owned(), source.to_owned());
        let response = server
            .formatting(&json!({"textDocument": {"uri": uri}}))
            .expect("formatting should return a workspace edit");
        assert_eq!(
            response[0]["newText"],
            "<?xml version=\"1.0\" encoding=\"iso-8859-1\" ?>\n<root>\n  <outer>\n    <inner />\n  </outer>\n</root>\n"
        );
    }

    #[test]
    fn converts_unicode_offsets_to_utf16_positions() {
        assert_eq!(
            position_at("é\n😀<root>", 7),
            json!({"line": 1, "character": 2})
        );
    }

    #[test]
    fn locates_xsd_parse_errors() {
        let source = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="root"></xs:schema>"#;
        let offset = xsd_parse_error_offset(source);
        assert!(offset > source.find("</xs:schema>").unwrap());
        assert!(offset <= source.len());
    }

    #[test]
    fn publishes_xsd_diagnostics_for_a_referenced_schema() {
        let schema_path =
            std::env::temp_dir().join(format!("xml-lsp-schema-{}.xsd", std::process::id()));
        std::fs::write(
            &schema_path,
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" targetNamespace="urn:test"><xs:element name="root"><xs:complexType><xs:sequence><xs:element name="child"/></xs:sequence></xs:complexType></xs:element></xs:schema>"#,
        )
        .expect("schema should be written");

        let document_path = schema_path.with_file_name("document.xml");
        let uri = format!(
            "file:///{}",
            document_path.to_string_lossy().replace('\\', "/")
        );
        let source = format!(
            "<wrong xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"{}\" />",
            schema_path.file_name().unwrap().to_string_lossy()
        );
        let mut server = XmlLanguageServer::new();
        let diagnostics = server.schema_diagnostics(&uri, &source);

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0]["code"], "xsd-validation");
        assert!(
            diagnostics[0]["message"]
                .as_str()
                .unwrap()
                .contains("<wrong>")
        );
        assert!(
            server
                .schema_index
                .get("urn:test")
                .is_some_and(|paths| paths.contains(&schema_path))
        );

        let completion_source = format!(
            "<root xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"{}\"><",
            schema_path.file_name().unwrap().to_string_lossy()
        );
        let mut server = XmlLanguageServer::new();
        server
            .documents
            .insert(uri.clone(), completion_source.clone());
        let completion = server
            .completion(&json!({
                "textDocument": {"uri": uri},
                "position": {"line": 0, "character": completion_source.len()},
            }))
            .expect("completion should be available");
        assert!(
            completion["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["label"] == "child")
        );

        std::fs::remove_file(schema_path).expect("schema should be removed");
    }
}
