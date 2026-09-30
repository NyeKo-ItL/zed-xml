//! Go to definition and find references: links to referenced files, ID/IDREF
//! and key/keyref values, XSLT components, DTD declarations and the XSD
//! declaration of an element.

use super::*;

impl XmlLanguageServer {
    pub(crate) fn references(&mut self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?.clone();
        let position = params.get("position")?;
        let line = position.get("line")?.as_u64()? as usize;
        let character = position.get("character")?.as_u64()? as usize;
        let offset = offset_at(&source, line, character);
        let links = self.identity_links(uri, &source);
        if identity::applies(&links, offset) {
            let include_declaration = params
                .get("context")
                .and_then(|context| context.get("includeDeclaration"))
                .and_then(Value::as_bool)
                .unwrap_or(true);
            return identity::references(&links, uri, &source, offset, include_declaration);
        }
        let include_declaration = params
            .pointer("/context/includeDeclaration")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if let Some(locations) = xslt::references(
            &self.xslt_context(),
            uri,
            &source,
            offset,
            include_declaration,
        ) {
            return Some(locations);
        }
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
        let lines = selection::LineIndex::new(&source);
        let needle = format!("<{name}");
        let mut search_from = 0usize;
        while let Some(relative) = source[search_from..].find(&needle) {
            let start = search_from + relative;
            locations.push(json!({
                "uri": uri,
                "range": {
                    "start": lines.position(&source, start),
                    "end": lines.position(&source, start + name.len() + 1),
                },
            }));
            search_from = start + name.len() + 1;
        }
        Some(Value::Array(locations))
    }

    pub(crate) fn definition(&mut self, params: &Value) -> Option<Value> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        let source = self.documents.get(uri)?.clone();
        let position = params.get("position")?;
        let line = position.get("line")?.as_u64()? as usize;
        let character = position.get("character")?.as_u64()? as usize;
        let offset = offset_at(&source, line, character);
        if let Some(links) = links::definition(
            uri,
            &source,
            offset,
            self.definition_link_support,
            &self.catalogs,
        ) {
            return Some(links);
        }
        let identity_links = self.identity_links(uri, &source);
        if let Some(location) = identity::definition(&identity_links, uri, &source, offset) {
            return Some(location);
        }
        if let Some(location) = xslt::definition(
            &self.xslt_context(),
            uri,
            &source,
            offset,
            self.definition_link_support,
        ) {
            return Some(location);
        }
        if let Some(grammar) = self.dtd_grammar(uri, &source)
            && let Some(location) = dtd::definition(&grammar, uri, &source, offset)
        {
            return Some(location);
        }
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

    /// ID/IDREF (XSD and DTD) and key/keyref links of the document.
    pub(crate) fn identity_links(&mut self, uri: &str, source: &str) -> identity::Links {
        let mut links = Vec::new();
        if let Some(schema) = self.load_schema(uri, source) {
            links.extend(
                identity_links(source, &schema)
                    .into_iter()
                    .map(|link| (link.reference, link.target)),
            );
        }
        if let Some(grammar) = self.dtd_grammar(uri, source) {
            for link in dtd_core::id_links(source, &grammar.dtd) {
                if !links.contains(&link) {
                    links.push(link);
                }
            }
        }
        links
    }

    pub(crate) fn xsd_definition(
        &mut self,
        uri: &str,
        source: &str,
        name: &str,
    ) -> Option<(PathBuf, String, usize)> {
        let mut queue = self.schema_references(uri, source).ok()?;
        let mut visited = HashSet::new();
        while let Some(reference) = queue.pop() {
            if visited.len() >= MAX_SCHEMA_DOCUMENTS {
                return None;
            }
            if !visited.insert(reference.path.clone()) || is_remote_location(&reference.path) {
                continue;
            }
            let schema_source = read_text_file(&reference.path, MAX_RESOURCE_SIZE).ok()?;
            if let Some(offset) = xsd_element_name_offset(&schema_source, name) {
                return Some((reference.path, schema_source, offset));
            }
            if let Ok(dependencies) =
                resolve_schema_dependencies_with(&schema_source, &reference.path, &|request| {
                    self.catalogs.resolve_schema(request)
                })
            {
                queue.extend(dependencies);
            }
        }
        None
    }
}

pub(crate) fn xsd_element_name_offset(source: &str, expected_name: &str) -> Option<usize> {
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

pub(crate) fn element_name_at(source: &str, offset: usize) -> Option<String> {
    let prefix = &source[..offset.min(source.len())];
    let opening = prefix.rfind('<')?;
    let fragment = &prefix[opening + 1..];
    let fragment = fragment.strip_prefix('/').unwrap_or(fragment);
    fragment.split_whitespace().next().map(str::to_owned)
}
