use super::*;
use lsp_server::{Request, RequestId};
use std::thread;

/// Requests only the tests send, to observe the server deterministically.
pub(super) const TEST_METHOD_PREFIX: &str = "xml-lsp/test/";
/// Answers once the diagnostics of the changes sent so far are published.
const SYNC_DIAGNOSTICS_METHOD: &str = "xml-lsp/test/syncDiagnostics";
/// Panics in the handler.
const PANIC_METHOD: &str = "xml-lsp/test/panic";
/// Keeps the request loop busy for `params.milliseconds`.
const SLEEP_METHOD: &str = "xml-lsp/test/sleep";

pub(super) fn handle_test_request(
    server: &mut XmlLanguageServer,
    method: &str,
    params: &Value,
) -> Result<Value, dispatch::RequestError> {
    match method {
        SYNC_DIAGNOSTICS_METHOD => {
            if let Some(worker) = &server.diagnostics_worker {
                assert!(
                    worker.flush(std::time::Duration::from_secs(20)),
                    "the diagnostics worker should become idle"
                );
            }
        }
        PANIC_METHOD => panic!("test panic in a request handler"),
        SLEEP_METHOD => thread::sleep(std::time::Duration::from_millis(
            params["milliseconds"].as_u64().unwrap_or(100),
        )),
        _ => {
            return Err(dispatch::RequestError::new(
                ErrorCode::MethodNotFound,
                method,
            ));
        }
    }
    Ok(Value::Null)
}

#[test]
fn preserves_a_user_change_from_self_closing_to_explicit_empty_element() {
    let uri = "file:///document.xml";
    let previous = "<root><item /></root>".to_owned();
    let params = json!({
        "textDocument": {"uri": uri, "version": 2},
        "contentChanges": [{
            "range": {
                "start": {"line": 0, "character": 6},
                "end": {"line": 0, "character": 14}
            },
            "text": "<item></item>"
        }]
    });

    let (_, current) = XmlLanguageServer::changed_document(&params, Some(&previous))
        .expect("the incremental change should be applied");
    assert_eq!(current, "<root><item></item></root>");

    let mut server = XmlLanguageServer::new();
    server.documents.insert(uri.to_owned(), current);
    let edits = server
        .formatting(&json!({"textDocument": {"uri": uri}}))
        .expect("the document should be formatted");
    assert_eq!(
        formatting::apply_edits(&server.documents[uri], &edits),
        "<root>\n  <item>\n  </item>\n</root>\n"
    );
}

#[test]
fn accepts_a_full_document_change_with_a_null_range() {
    let params = json!({
        "textDocument": {"uri": "file:///document.xml", "version": 2},
        "contentChanges": [{"range": null, "text": "<root />"}]
    });

    let (_, current) = XmlLanguageServer::changed_document(&params, Some(&"<old />".to_owned()))
        .expect("a null range should mean full-document replacement");
    assert_eq!(current, "<root />");
}

#[test]
fn formats_many_realistic_user_edit_sequences_without_stale_content() {
    let scenarios = [
        [
            "<root />",
            "<root><item /></root>",
            "<root><item>one</item></root>",
            "<root><item id=\"1\">two</item><empty /></root>",
            "<root><item id=\"2\">trois &amp; quatre</item><empty></empty></root>",
        ],
        [
            "<catalog />",
            "<catalog><book /></catalog>",
            "<catalog><book><title>XML</title></book></catalog>",
            "<catalog><book id=\"é\"><title>Édition</title><author /></book></catalog>",
            "<catalog><!-- note --><book><![CDATA[a < b]]></book></catalog>",
        ],
        [
            "<Message />",
            "<Message><Header /></Message>",
            "<Message><Header><Id>1</Id></Header></Message>",
            "<Message><Header><Id>2</Id><Date>2026-09-29</Date></Header><Body /></Message>",
            "<Message><Header><Id>3</Id></Header><Body><Value>42.5</Value></Body></Message>",
        ],
    ];

    for scenario in scenarios.iter().cycle().take(30) {
        let mut current = scenario[0].to_owned();
        for desired in scenario.iter().skip(1) {
            let params = incremental_replacement_params(&current, desired);
            let (_, updated) = XmlLanguageServer::changed_document(&params, Some(&current))
                .expect("the simulated editor change should be applied");
            assert_eq!(&updated, desired);
            current = updated;

            let formatted = format_xml(&current).expect("valid edited XML should format");
            assert_eq!(
                format_xml(&formatted).unwrap(),
                formatted,
                "formatting must be idempotent for {current:?}"
            );
            assert_eq!(
                formatted.matches("<root").count(),
                current.matches("<root").count()
            );
            assert_eq!(
                formatted.matches("<catalog").count(),
                current.matches("<catalog").count()
            );
            assert_eq!(
                formatted.matches("<Message").count(),
                current.matches("<Message").count()
            );
        }
    }
}

#[test]
fn completion_after_typing_a_greater_than_sign_suggests_the_closing_tag() {
    let mut server = XmlLanguageServer::new();
    server.documents.insert(
        "file:///document.xml".to_owned(),
        "<root><child>".to_owned(),
    );

    let result = server
        .completion(&json!({
            "textDocument": {"uri": "file:///document.xml"},
            "position": {"line": 0, "character": 13}
        }))
        .expect("completion should be available after >");

    assert_eq!(
        result["items"],
        json!([{"label": "</child>", "insertText": "</child>"}])
    );
}

#[test]
fn completion_after_xml_processing_instruction_prefix_offers_xsd_template() {
    let mut server = XmlLanguageServer::new();
    server
        .documents
        .insert("file:///document.xml".to_owned(), "<?".to_owned());

    let result = server
        .completion(&json!({
            "textDocument": {"uri": "file:///document.xml"},
            "position": {"line": 0, "character": 2}
        }))
        .expect("processing instruction completion should be available");

    assert_eq!(
        result["items"][1],
        json!({
            "label": "xml-model",
            "insertText": "xml-model href=\"schema.xsd\" type=\"application/xml\" schematypens=\"http://www.w3.org/2001/XMLSchema\"?>"
        })
    );
}

#[test]
fn completion_triggers_cover_xml_typing_contexts() {
    let capabilities = server_capabilities(positions::PositionEncoding::default());
    assert_eq!(
        capabilities["completionProvider"]["triggerCharacters"],
        json!(["<", " ", "/", ">", "=", "\"", "?", "&", "%", "$"])
    );
}

fn incremental_replacement_params(current: &str, desired: &str) -> Value {
    let prefix = current
        .bytes()
        .zip(desired.bytes())
        .take_while(|(left, right)| left == right)
        .count();
    let suffix = current[prefix..]
        .bytes()
        .rev()
        .zip(desired[prefix..].bytes().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let end = current.len() - suffix;
    json!({
        "textDocument": {"uri": "file:///document.xml", "version": 2},
        "contentChanges": [{
            "range": {
                "start": position_at(current, prefix),
                "end": position_at(current, end)
            },
            "text": &desired[prefix..desired.len() - suffix]
        }]
    })
}

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
fn serves_document_highlight_requests() {
    let (server, client) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let request = |id: i32, method: &str, params: Value| {
        client
            .sender
            .send(
                Request {
                    id: RequestId::from(id),
                    method: method.to_owned(),
                    params,
                }
                .into(),
            )
            .expect("request should be sent");
        loop {
            match client.receiver.recv().expect("a message should arrive") {
                Message::Response(response) => {
                    assert_eq!(response.id, RequestId::from(id));
                    return response.result;
                }
                Message::Notification(_) => {}
                message => panic!("unexpected message {message:?}"),
            }
        }
    };
    let notify = |method: &str, params: Value| {
        client
            .sender
            .send(
                Notification {
                    method: method.to_owned(),
                    params,
                }
                .into(),
            )
            .expect("notification should be sent");
    };

    let initialize = request(1, INITIALIZE_METHOD, json!({}));
    assert_eq!(
        initialize.unwrap()["capabilities"]["documentHighlightProvider"],
        true
    );
    notify("initialized", json!({}));
    notify(
        DID_OPEN_METHOD,
        json!({
            "textDocument": {
                "uri": "file:///document.xml",
                "text": "<ns:root>\r\n  <ns:item><ns:item/></ns:item>\r\n</ns:root>",
            }
        }),
    );
    let params = |line: u32, character: u32| {
        json!({
            "textDocument": {"uri": "file:///document.xml"},
            "position": {"line": line, "character": character},
        })
    };

    assert_eq!(
        request(2, DOCUMENT_HIGHLIGHT_METHOD, params(2, 4)),
        Some(json!([
            {"range": {"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 8}}, "kind": 2},
            {"range": {"start": {"line": 2, "character": 2}, "end": {"line": 2, "character": 9}}, "kind": 2},
        ]))
    );
    assert_eq!(
        request(3, DOCUMENT_HIGHLIGHT_METHOD, params(1, 5)),
        Some(json!([
            {"range": {"start": {"line": 1, "character": 3}, "end": {"line": 1, "character": 10}}, "kind": 2},
            {"range": {"start": {"line": 1, "character": 23}, "end": {"line": 1, "character": 30}}, "kind": 2},
        ]))
    );
    assert_eq!(
        request(4, DOCUMENT_HIGHLIGHT_METHOD, params(1, 13)),
        Some(json!([
            {"range": {"start": {"line": 1, "character": 12}, "end": {"line": 1, "character": 19}}, "kind": 2},
        ]))
    );
    assert_eq!(
        request(5, DOCUMENT_HIGHLIGHT_METHOD, params(1, 0)),
        Some(json!([]))
    );

    assert_eq!(request(6, "shutdown", json!(null)), Some(Value::Null));
    notify(EXIT_METHOD, json!(null));
    server_thread.join().expect("server thread should stop");
}

#[test]
fn serves_linked_editing_range_requests() {
    let (server, client) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let request = |id: i32, method: &str, params: Value| {
        client
            .sender
            .send(
                Request {
                    id: RequestId::from(id),
                    method: method.to_owned(),
                    params,
                }
                .into(),
            )
            .expect("request should be sent");
        loop {
            match client.receiver.recv().expect("a message should arrive") {
                Message::Response(response) => {
                    assert_eq!(response.id, RequestId::from(id));
                    assert!(response.error.is_none(), "{:?}", response.error);
                    return response.result;
                }
                Message::Notification(_) => {}
                message => panic!("unexpected message {message:?}"),
            }
        }
    };
    let notify = |method: &str, params: Value| {
        client
            .sender
            .send(
                Notification {
                    method: method.to_owned(),
                    params,
                }
                .into(),
            )
            .expect("notification should be sent");
    };

    let initialize = request(1, INITIALIZE_METHOD, json!({}));
    assert_eq!(
        initialize.unwrap()["capabilities"]["linkedEditingRangeProvider"],
        true
    );
    notify("initialized", json!({}));
    notify(
        DID_OPEN_METHOD,
        json!({
            "textDocument": {
                "uri": "file:///document.xml",
                "text": "<ns:root>\r\n  <ns:item><ns:item/></ns:item>\r\n</ns:root>",
            }
        }),
    );
    let params = |line: u32, character: u32| {
        json!({
            "textDocument": {"uri": "file:///document.xml"},
            "position": {"line": line, "character": character},
        })
    };

    assert_eq!(
        request(2, LINKED_EDITING_RANGE_METHOD, params(2, 9)),
        Some(json!({
            "ranges": [
                {"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 8}},
                {"start": {"line": 2, "character": 2}, "end": {"line": 2, "character": 9}},
            ],
            "wordPattern": linked_editing::XML_NAME_WORD_PATTERN,
        }))
    );
    assert_eq!(
        request(3, LINKED_EDITING_RANGE_METHOD, params(1, 5)),
        Some(json!({
            "ranges": [
                {"start": {"line": 1, "character": 3}, "end": {"line": 1, "character": 10}},
                {"start": {"line": 1, "character": 23}, "end": {"line": 1, "character": 30}},
            ],
            "wordPattern": linked_editing::XML_NAME_WORD_PATTERN,
        }))
    );
    // Self-closing element, content and unknown document: `null`.
    assert_eq!(
        request(4, LINKED_EDITING_RANGE_METHOD, params(1, 13)),
        Some(Value::Null)
    );
    assert_eq!(
        request(5, LINKED_EDITING_RANGE_METHOD, params(1, 0)),
        Some(Value::Null)
    );
    assert_eq!(
        request(
            6,
            LINKED_EDITING_RANGE_METHOD,
            json!({
                "textDocument": {"uri": "file:///missing.xml"},
                "position": {"line": 0, "character": 1},
            })
        ),
        Some(Value::Null)
    );

    assert_eq!(request(7, "shutdown", json!(null)), Some(Value::Null));
    notify(EXIT_METHOD, json!(null));
    server_thread.join().expect("server thread should stop");
}

#[test]
fn serves_folding_range_requests_within_the_client_range_limit() {
    let (server, client) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let request = |id: i32, method: &str, params: Value| {
        client
            .sender
            .send(
                Request {
                    id: RequestId::from(id),
                    method: method.to_owned(),
                    params,
                }
                .into(),
            )
            .expect("request should be sent");
        loop {
            match client.receiver.recv().expect("a message should arrive") {
                Message::Response(response) => {
                    assert_eq!(response.id, RequestId::from(id));
                    return response.result;
                }
                Message::Notification(_) => {}
                message => panic!("unexpected message {message:?}"),
            }
        }
    };
    let notify = |method: &str, params: Value| {
        client
            .sender
            .send(
                Notification {
                    method: method.to_owned(),
                    params,
                }
                .into(),
            )
            .expect("notification should be sent");
    };

    let initialize = request(
        1,
        INITIALIZE_METHOD,
        json!({"capabilities": {"textDocument": {"foldingRange": {
            "lineFoldingOnly": true,
            "rangeLimit": 3,
        }}}}),
    );
    assert_eq!(
        initialize.unwrap()["capabilities"]["foldingRangeProvider"],
        true
    );
    notify("initialized", json!({}));
    notify(
        DID_OPEN_METHOD,
        json!({
            "textDocument": {
                "uri": "file:///document.xml",
                "text": "<root>\r\n  <!-- #region items -->\r\n  <item\r\n    a=\"1\"\r\n    b=\"2\"/>\r\n  <!-- #endregion -->\r\n  <!--\r\n    note\r\n  -->\r\n</root>\r\n",
            }
        }),
    );
    let params = json!({"textDocument": {"uri": "file:///document.xml"}});

    assert_eq!(
        request(2, FOLDING_RANGE_METHOD, params.clone()),
        Some(json!([
            {"startLine": 0, "endLine": 8},
            {"startLine": 1, "endLine": 4, "kind": "region"},
            {"startLine": 6, "endLine": 7, "kind": "comment"},
        ]))
    );
    notify(
        DID_CHANGE_METHOD,
        json!({
            "textDocument": {"uri": "file:///document.xml", "version": 2},
            "contentChanges": [{"text": "<root><item/></root>"}],
        }),
    );
    assert_eq!(request(3, FOLDING_RANGE_METHOD, params), Some(json!([])));
    assert_eq!(
        request(
            4,
            FOLDING_RANGE_METHOD,
            json!({"textDocument": {"uri": "file:///missing.xml"}})
        ),
        Some(json!([]))
    );

    assert_eq!(request(5, "shutdown", json!(null)), Some(Value::Null));
    notify(EXIT_METHOD, json!(null));
    server_thread.join().expect("server thread should stop");
}

#[test]
fn serves_selection_range_requests_for_several_positions() {
    let (server, client) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let request = |id: i32, method: &str, params: Value| {
        client
            .sender
            .send(
                Request {
                    id: RequestId::from(id),
                    method: method.to_owned(),
                    params,
                }
                .into(),
            )
            .expect("request should be sent");
        loop {
            match client.receiver.recv().expect("a message should arrive") {
                Message::Response(response) => {
                    assert_eq!(response.id, RequestId::from(id));
                    return response.result;
                }
                Message::Notification(_) => {}
                message => panic!("unexpected message {message:?}"),
            }
        }
    };
    let notify = |method: &str, params: Value| {
        client
            .sender
            .send(
                Notification {
                    method: method.to_owned(),
                    params,
                }
                .into(),
            )
            .expect("notification should be sent");
    };
    let range = |start: (u32, u32), end: (u32, u32)| {
        json!({
            "start": {"line": start.0, "character": start.1},
            "end": {"line": end.0, "character": end.1},
        })
    };
    let ranges = |mut selection: &Value| {
        let mut ranges = vec![selection["range"].clone()];
        while let Some(parent) = selection.get("parent") {
            ranges.push(parent["range"].clone());
            selection = parent;
        }
        ranges
    };

    let initialize = request(1, INITIALIZE_METHOD, json!({"capabilities": {}}));
    assert_eq!(
        initialize.unwrap()["capabilities"]["selectionRangeProvider"],
        true
    );
    notify("initialized", json!({}));
    notify(
        DID_OPEN_METHOD,
        json!({
            "textDocument": {
                "uri": "file:///document.xml",
                "text": "<root>\r\n  <ns:😀 a=\"x y\">text</ns:😀>\r\n</root>",
            }
        }),
    );

    let result = request(
        2,
        SELECTION_RANGE_METHOD,
        json!({
            "textDocument": {"uri": "file:///document.xml"},
            "positions": [
                {"line": 1, "character": 12},
                {"line": 1, "character": 18},
                {"line": 1, "character": 26},
            ],
        }),
    )
    .expect("selection ranges should be returned");
    let root = range((0, 0), (2, 7));
    let content = range((0, 6), (2, 0));
    let element = range((1, 2), (1, 29));
    assert_eq!(
        ranges(&result[0]),
        vec![
            range((1, 12), (1, 13)),
            range((1, 12), (1, 15)),
            range((1, 11), (1, 16)),
            range((1, 9), (1, 16)),
            range((1, 2), (1, 17)),
            element.clone(),
            content.clone(),
            root.clone(),
        ]
    );
    assert_eq!(
        ranges(&result[1]),
        vec![
            range((1, 17), (1, 21)),
            element.clone(),
            content.clone(),
            root.clone(),
        ]
    );
    assert_eq!(
        ranges(&result[2]),
        vec![
            range((1, 26), (1, 28)),
            range((1, 23), (1, 28)),
            range((1, 21), (1, 29)),
            element,
            content,
            root,
        ]
    );

    assert_eq!(
        request(
            3,
            SELECTION_RANGE_METHOD,
            json!({
                "textDocument": {"uri": "file:///missing.xml"},
                "positions": [{"line": 0, "character": 0}],
            })
        ),
        Some(Value::Null)
    );

    assert_eq!(request(4, "shutdown", json!(null)), Some(Value::Null));
    notify(EXIT_METHOD, json!(null));
    server_thread.join().expect("server thread should stop");
}

/// Test LSP client: sends messages and collects the received
/// `publishDiagnostics` notifications.
struct TestClient {
    connection: Connection,
    diagnostics: std::cell::RefCell<Vec<Value>>,
}

impl TestClient {
    fn send(&self, message: Message) {
        self.connection
            .sender
            .send(message)
            .expect("message should be sent");
    }

    fn notify(&self, method: &str, params: Value) {
        self.send(
            Notification {
                method: method.to_owned(),
                params,
            }
            .into(),
        );
    }

    /// Next message that is not a diagnostics publication.
    fn next(&self) -> Message {
        loop {
            match self
                .connection
                .receiver
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("a message should arrive")
            {
                Message::Notification(notification)
                    if notification.method == PUBLISH_DIAGNOSTICS_METHOD =>
                {
                    self.diagnostics.borrow_mut().push(notification.params);
                }
                message => return message,
            }
        }
    }

    fn request(&self, id: i32, method: &str, params: Value) -> Value {
        self.send(
            Request {
                id: RequestId::from(id),
                method: method.to_owned(),
                params,
            }
            .into(),
        );
        match self.next() {
            Message::Response(response) => {
                assert_eq!(response.id, RequestId::from(id));
                response.result.expect("request should succeed")
            }
            message => panic!("unexpected message {message:?}"),
        }
    }

    /// Diagnostics published so far, then cleared. The synchronisation
    /// request guarantees that previous publications have been received.
    fn take_diagnostics(&self, id: i32) -> Vec<Value> {
        self.request(id, SYNC_DIAGNOSTICS_METHOD, Value::Null);
        std::mem::take(&mut *self.diagnostics.borrow_mut())
    }
}

fn codes(publication: &Value) -> Vec<String> {
    publication["diagnostics"]
        .as_array()
        .expect("diagnostics should be an array")
        .iter()
        .map(|diagnostic| diagnostic["code"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// Next message: a server -> client `method` request, acknowledged.
fn expect_server_request(client: &TestClient, method: &str) -> Value {
    match client.next() {
        Message::Request(request) => {
            assert_eq!(request.method, method, "{request:?}");
            client.send(Response::new_ok(request.id, Value::Null).into());
            request.params
        }
        message => panic!("unexpected message {message:?}"),
    }
}

/// `data.kind` of the published diagnostics.
fn diagnostic_kinds(publication: &Value) -> Vec<String> {
    publication["diagnostics"]
        .as_array()
        .expect("diagnostics should be an array")
        .iter()
        .map(|diagnostic| {
            diagnostic["data"]["kind"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

#[test]
fn resolves_remote_schemas_through_xml_catalogs() {
    let directory = std::env::temp_dir().join(format!("xml-lsp-catalogs {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(directory.join("schemas")).expect("directory should be created");
    std::fs::create_dir_all(directory.join("my schemas")).expect("directory should be created");
    let strict = "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\n  <xs:element name=\"project\">\n    <xs:annotation><xs:documentation>Project resolved through a catalog.</xs:documentation></xs:annotation>\n    <xs:complexType><xs:sequence><xs:element name=\"name\" type=\"xs:string\"/></xs:sequence></xs:complexType>\n  </xs:element>\n</xs:schema>";
    let lenient = "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\n  <xs:element name=\"project\">\n    <xs:complexType><xs:sequence><xs:element name=\"other\" type=\"xs:string\"/></xs:sequence></xs:complexType>\n  </xs:element>\n</xs:schema>";
    let strict_path = directory.join("schemas/project.xsd");
    std::fs::write(&strict_path, strict).expect("schema should be written");
    std::fs::write(directory.join("schemas/lenient.xsd"), lenient)
        .expect("schema should be written");
    std::fs::write(directory.join("my schemas/project.xsd"), strict)
        .expect("schema should be written");
    let catalog_path = directory.join("catalog.xml");
    let catalog = |target: &str| {
        format!(
            "<catalog xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n  <system systemId=\"http://example.com/schemas/project.xsd\" uri=\"{target}\"/>\n</catalog>"
        )
    };
    std::fs::write(&catalog_path, catalog("schemas/project.xsd"))
        .expect("catalog should be written");

    let remote_uri = path_to_uri(&directory.join("remote.xml"));
    let remote = "<project xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"http://example.com/schemas/project.xsd\"><other/></project>";
    let spaced_uri = path_to_uri(&directory.join("spaced.xml"));
    let spaced = "<project xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"my%20schemas/project.xsd\"><other/></project>";
    let unmapped_uri = path_to_uri(&directory.join("unmapped.xml"));
    let unmapped = "<project xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"http://unmapped.example.com/x.xsd\"><other/></project>";
    let open_catalog_uri = path_to_uri(&directory.join("open-catalog.xml"));
    let open_catalog = "<catalog xmlns=\"urn:oasis:names:tc:entity:xmlns:xml:catalog\">\n  <uri name=\"a\" uri=\"schemas/project.xsd\"/>\n  <uri name=\"b\" uri=\"schemas/absent.xsd\"/>\n</catalog>";

    let (server, connection) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let client = TestClient {
        connection,
        diagnostics: Default::default(),
    };
    client.request(
        1,
        INITIALIZE_METHOD,
        json!({
            "rootUri": path_to_uri(&directory),
            "capabilities": {
                "workspace": {"didChangeWatchedFiles": {"dynamicRegistration": true}},
                "textDocument": {"definition": {"linkSupport": false}},
            },
            "initializationOptions": {"xml": {
                "catalogs": ["catalog.xml"],
                "validation": {"noGrammar": "hint"},
            }},
        }),
    );
    client.notify("initialized", json!({}));
    expect_server_request(&client, REGISTER_CAPABILITY_METHOD);
    let registration = expect_server_request(&client, REGISTER_CAPABILITY_METHOD);
    assert_eq!(
        registration["registrations"][0]["registerOptions"]["watchers"],
        json!([{"globPattern": catalog_path.to_string_lossy().replace('\\', "/")}])
    );

    for (uri, text) in [
        (&remote_uri, remote),
        (&spaced_uri, spaced),
        (&unmapped_uri, unmapped),
        (&open_catalog_uri, open_catalog),
    ] {
        client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": uri, "text": text}}),
        );
    }
    let published = client.take_diagnostics(2);
    assert_eq!(published.len(), 4, "{published:?}");
    // Remote URL resolved offline by the catalog: XSD validation.
    assert_eq!(published[0]["uri"], remote_uri);
    assert!(
        diagnostic_kinds(&published[0]).contains(&"validation".to_owned()),
        "{published:?}"
    );
    assert!(!diagnostic_kinds(&published[0]).contains(&"loading".to_owned()));
    // Percent-encoded path (`my%20schemas`): same validation.
    assert!(
        diagnostic_kinds(&published[1]).contains(&"validation".to_owned()),
        "{published:?}"
    );
    assert!(!diagnostic_kinds(&published[1]).contains(&"loading".to_owned()));
    // Uncataloged URL: explicit warning, no error.
    assert_eq!(diagnostic_kinds(&published[2]), vec!["loading"]);
    assert_eq!(published[2]["diagnostics"][0]["severity"], 2);
    assert!(
        published[2]["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("xml.catalogs")
    );
    // Open catalog: missing target reported, no "noGrammar".
    assert_eq!(codes(&published[3]), vec!["catalog-target-missing"]);
    assert_eq!(
        published[3]["diagnostics"][0]["range"],
        json!({"start": {"line": 2, "character": 21}, "end": {"line": 2, "character": 39}})
    );

    // Completion driven by the cataloged schema.
    let character = remote.find("<other").unwrap() + 1;
    let completion = client.request(
            3,
            COMPLETION_METHOD,
            json!({"textDocument": {"uri": remote_uri}, "position": {"line": 0, "character": character}}),
        );
    let labels = completion["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["label"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert!(labels.contains(&"name".to_owned()), "{labels:?}");

    // Hover: documentation of the cataloged schema.
    let hover = client.request(
        4,
        HOVER_METHOD,
        json!({"textDocument": {"uri": remote_uri}, "position": {"line": 0, "character": 2}}),
    );
    assert!(
        hover["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("Project resolved through a catalog."),
        "{hover:?}"
    );

    // Link and definition to the local file.
    let links = client.request(
        5,
        DOCUMENT_LINK_METHOD,
        json!({"textDocument": {"uri": remote_uri}}),
    );
    assert_eq!(links[0]["target"], path_to_uri(&strict_path));
    let location = remote.find("http://example.com").unwrap() + 4;
    let definition = client.request(
            6,
            DEFINITION_METHOD,
            json!({"textDocument": {"uri": remote_uri}, "position": {"line": 0, "character": location}}),
        );
    assert_eq!(definition[0]["uri"], path_to_uri(&strict_path));

    // Catalog modified on disk: reread, diagnostics republished.
    std::fs::write(&catalog_path, catalog("schemas/lenient.xsd"))
        .expect("catalog should be written");
    client.notify(
        DID_CHANGE_WATCHED_FILES_METHOD,
        json!({"changes": [{"uri": path_to_uri(&catalog_path), "type": 2}]}),
    );
    expect_server_request(&client, UNREGISTER_CAPABILITY_METHOD);
    expect_server_request(&client, REGISTER_CAPABILITY_METHOD);
    let published = client.take_diagnostics(7);
    assert_eq!(published.len(), 4, "{published:?}");
    let remote_publication = |published: &[Value]| {
        published
            .iter()
            .find(|publication| publication["uri"] == remote_uri)
            .cloned()
            .expect("remote.xml should be republished")
    };
    assert_eq!(remote_publication(&published)["diagnostics"], json!([]));

    // Catalogs removed from the settings: the URL is remote again.
    client.notify(
        DID_CHANGE_CONFIGURATION_METHOD,
        json!({"settings": {"xml": {"catalogs": []}}}),
    );
    expect_server_request(&client, UNREGISTER_CAPABILITY_METHOD);
    let published = client.take_diagnostics(8);
    assert_eq!(published.len(), 4, "{published:?}");
    assert_eq!(
        diagnostic_kinds(&remote_publication(&published)),
        vec!["loading"]
    );

    assert_eq!(client.request(9, "shutdown", json!(null)), Value::Null);
    client.notify(EXIT_METHOD, json!(null));
    server_thread.join().expect("server thread should stop");
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn navigates_ids_and_keys_and_reports_identity_problems() {
    let directory = std::env::temp_dir().join(format!("xml-lsp-identity {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("directory should be created");
    std::fs::write(
        directory.join("library.xsd"),
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="library">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="book" maxOccurs="unbounded">
          <xs:complexType>
            <xs:attribute name="id" type="xs:ID"/>
            <xs:attribute name="isbn" type="xs:string"/>
          </xs:complexType>
        </xs:element>
        <xs:element name="loan" minOccurs="0" maxOccurs="unbounded">
          <xs:complexType>
            <xs:attribute name="book" type="xs:IDREF"/>
            <xs:attribute name="isbn" type="xs:string"/>
          </xs:complexType>
        </xs:element>
      </xs:sequence>
    </xs:complexType>
    <xs:key name="isbn">
      <xs:selector xpath="book"/>
      <xs:field xpath="@isbn"/>
    </xs:key>
    <xs:keyref name="loanIsbn" refer="isbn">
      <xs:selector xpath="loan"/>
      <xs:field xpath="@isbn"/>
    </xs:keyref>
  </xs:element>
</xs:schema>"#,
    )
    .expect("schema should be written");
    let uri = path_to_uri(&directory.join("library.xml"));

    let (server, connection) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let client = TestClient {
        connection,
        diagnostics: Default::default(),
    };
    client.request(1, INITIALIZE_METHOD, json!({"capabilities": {}}));
    client.notify("initialized", json!({}));

    // Line 1: the book "é1" with the ISBN "42"; line 2: a duplicate
    // ISBN; line 3: two loans.
    let source = "<library xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"library.xsd\">\r\n<book id=\"é1\" isbn=\"42\"/>\r\n<book id=\"b2\" isbn=\"42\"/>\r\n<loan book=\"é1\" isbn=\"42\"/><loan book=\"é1\" isbn=\"7\"/>\r\n</library>";
    client.notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": uri, "text": source}}),
    );
    let published = client.take_diagnostics(2);
    let rules = published.last().expect("diagnostics should be published")["diagnostics"]
        .as_array()
        .expect("diagnostics should be an array")
        .iter()
        .map(|diagnostic| {
            (
                diagnostic["data"]["rule"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                diagnostic["range"]["start"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rules,
        [
            (
                "duplicateKey".to_owned(),
                json!({"line": 2, "character": 20})
            ),
            (
                "unknownKeyref".to_owned(),
                json!({"line": 3, "character": 49})
            ),
        ]
    );

    // From the IDREF of the first loan to the ID of the book.
    let definition = client.request(
        3,
        "textDocument/definition",
        json!({"textDocument": {"uri": uri}, "position": {"line": 3, "character": 12}}),
    );
    assert_eq!(
        definition,
        json!([{"uri": uri, "range": {
            "start": {"line": 1, "character": 10},
            "end": {"line": 1, "character": 12},
        }}])
    );
    // From the keyref value to the key value.
    let definition = client.request(
        4,
        "textDocument/definition",
        json!({"textDocument": {"uri": uri}, "position": {"line": 3, "character": 23}}),
    );
    assert_eq!(
        definition[0]["range"]["start"],
        json!({"line": 1, "character": 20})
    );
    // References of the ID: both loans.
    let references = client.request(
        5,
        "textDocument/references",
        json!({
            "textDocument": {"uri": uri},
            "position": {"line": 1, "character": 11},
            "context": {"includeDeclaration": false},
        }),
    );
    let starts = references
        .as_array()
        .expect("references should be an array")
        .iter()
        .map(|location| location["range"]["start"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        starts,
        [
            json!({"line": 3, "character": 12}),
            json!({"line": 3, "character": 39}),
        ]
    );

    assert_eq!(client.request(6, "shutdown", json!(null)), Value::Null);
    client.notify(EXIT_METHOD, json!(null));
    server_thread.join().expect("server thread should stop");
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn publishes_datatype_diagnostics_on_values() {
    let directory = std::env::temp_dir().join(format!("xml-lsp-datatypes {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("directory should be created");
    std::fs::write(
            directory.join("order.xsd"),
            "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\n  <xs:element name=\"order\">\n    <xs:complexType>\n      <xs:sequence>\n        <xs:element name=\"date\" type=\"xs:date\"/>\n        <xs:element name=\"total\" type=\"Total\"/>\n      </xs:sequence>\n      <xs:attribute name=\"count\" type=\"xs:unsignedByte\"/>\n    </xs:complexType>\n  </xs:element>\n  <xs:simpleType name=\"Total\">\n    <xs:restriction base=\"xs:decimal\">\n      <xs:enumeration value=\"1.0\"/>\n      <xs:enumeration value=\"2.5\"/>\n    </xs:restriction>\n  </xs:simpleType>\n</xs:schema>",
        )
        .expect("schema should be written");
    let uri = path_to_uri(&directory.join("order.xml"));

    let (server, connection) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let client = TestClient {
        connection,
        diagnostics: Default::default(),
    };
    client.request(1, INITIALIZE_METHOD, json!({"capabilities": {}}));
    client.notify("initialized", json!({}));

    // Non-ASCII text before the values and CRLF line endings: ranges are
    // UTF-16 positions of the values.
    let source = "<order xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\r\n  xsi:noNamespaceSchemaLocation=\"order.xsd\" é=\"\" count=\"300\">\r\n  <date>2024-13-01</date>\r\n  <total>1</total>\r\n</order>";
    client.notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": uri, "text": source}}),
    );
    let published = client.take_diagnostics(2);
    let diagnostics = published.last().expect("diagnostics should be published")["diagnostics"]
        .as_array()
        .expect("diagnostics should be an array")
        .iter()
        .filter(|diagnostic| diagnostic["data"]["rule"] != "unexpectedAttribute")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 2, "{diagnostics:#?}");
    assert_eq!(diagnostics[0]["code"], "xsd-validation");
    assert_eq!(diagnostics[0]["data"]["rule"], "invalidAttributeValue");
    assert_eq!(
        diagnostics[0]["range"],
        json!({"start": {"line": 1, "character": 56}, "end": {"line": 1, "character": 59}})
    );
    assert_eq!(
        diagnostics[0]["message"],
        "attribute @count of <order>: '300' is not a valid xs:unsignedByte: the value must be at most 255"
    );
    assert_eq!(diagnostics[1]["data"]["rule"], "invalidContent");
    assert_eq!(
        diagnostics[1]["message"],
        "content of <date>: '2024-13-01' is not a valid xs:date: month must be 01-12"
    );
    // `1` equals the enumerated `1.0` in the value space of xs:decimal.

    client.notify(
            DID_CHANGE_METHOD,
            json!({
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{"text": source.replace("count=\"300\"", "count=\"30\"").replace("2024-13-01", "2024-12-01").replace("<total>1<", "<total>3<")}],
            }),
        );
    let published = client.take_diagnostics(3);
    let diagnostics = published.last().expect("diagnostics should be published")["diagnostics"]
        .as_array()
        .expect("diagnostics should be an array")
        .iter()
        .filter(|diagnostic| diagnostic["data"]["rule"] != "unexpectedAttribute")
        .cloned()
        .collect::<Vec<_>>();
    // The enumeration is reported once, on the value.
    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(diagnostics[0]["data"]["rule"], "invalidEnumeration");
    assert_eq!(
        diagnostics[0]["range"],
        json!({"start": {"line": 3, "character": 9}, "end": {"line": 3, "character": 10}})
    );

    assert_eq!(client.request(4, "shutdown", json!(null)), Value::Null);
    client.notify(EXIT_METHOD, json!(null));
    server_thread.join().expect("server thread should stop");
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn applies_initialization_option_settings() {
    let directory = std::env::temp_dir().join(format!("xml-lsp-settings {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(directory.join("schemas")).expect("directory should be created");
    std::fs::write(
            directory.join("schemas/project.xsd"),
            "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\n  <xs:element name=\"project\">\n    <xs:complexType><xs:sequence><xs:element name=\"name\" type=\"xs:string\"/></xs:sequence></xs:complexType>\n  </xs:element>\n</xs:schema>",
        )
        .expect("schema should be written");
    let associated_uri = path_to_uri(&directory.join("app/build.project"));
    let plain_uri = path_to_uri(&directory.join("plain.xml"));
    let doctype_uri = path_to_uri(&directory.join("doctype.xml"));

    let (server, connection) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let client = TestClient {
        connection,
        diagnostics: Default::default(),
    };
    client.request(
            1,
            INITIALIZE_METHOD,
            json!({
                "rootUri": path_to_uri(&directory),
                "capabilities": {},
                "initializationOptions": {"settings": {"xml": {
                    "format": {"splitAttributes": "splitNewLine", "emptyElements": "collapse", "enabled": true},
                    "validation": {"noGrammar": "warning", "disallowDocTypeDecl": true},
                    "completion": {"autoCloseTags": false},
                    "symbols": {"maxItemsComputed": 2},
                    "colors": {"enabled": false},
                    "fileAssociations": [{"pattern": "**/*.project", "systemId": "schemas/project.xsd"}],
                }}},
            }),
        );
    client.notify("initialized", json!({}));

    client.notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": associated_uri, "text": "<project><other/></project>"}}),
    );
    client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": plain_uri, "text": "<svg a=\"1\" b=\"2\"><rect fill=\"red\"></rect><g><c/></g></svg>"}}),
        );
    client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": doctype_uri, "text": "<!DOCTYPE r [<!ELEMENT r ANY>]>\n<r/>"}}),
        );
    let published = client.take_diagnostics(2);
    assert_eq!(published.len(), 3);
    // File association: XSD validation without xsi:schemaLocation.
    assert_eq!(published[0]["uri"], associated_uri);
    assert!(
        codes(&published[0]).contains(&"xsd-validation".to_owned()),
        "{published:?}"
    );
    assert_eq!(codes(&published[1]), vec!["no-grammar"]);
    assert_eq!(published[1]["diagnostics"][0]["severity"], 2);
    assert_eq!(
        published[1]["diagnostics"][0]["range"],
        json!({"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 4}})
    );
    // A DTD is a grammar, but the declaration is not allowed.
    assert_eq!(codes(&published[2]), vec!["doctype-disallowed"]);

    // Completion driven by the associated schema, without auto-closing.
    let labels = |id: i32, character: u32| {
        let completion = client.request(
                id,
                COMPLETION_METHOD,
                json!({"textDocument": {"uri": associated_uri}, "position": {"line": 0, "character": character}}),
            );
        completion["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["label"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let after_start_tag = labels(30, 9);
    assert!(
        !after_start_tag.iter().any(|label| label.starts_with("</")),
        "{after_start_tag:?}"
    );
    let children = labels(3, 10);
    assert!(children.contains(&"name".to_owned()), "{children:?}");

    let edits = client.request(
            4,
            FORMATTING_METHOD,
            json!({"textDocument": {"uri": plain_uri}, "options": {"tabSize": 2, "insertSpaces": true}}),
        );
    assert_eq!(
        formatting::apply_edits(
            "<svg a=\"1\" b=\"2\"><rect fill=\"red\"></rect><g><c/></g></svg>",
            &edits
        ),
        "<svg\n  a=\"1\"\n  b=\"2\">\n  <rect fill=\"red\"/>\n  <g>\n    <c/>\n  </g>\n</svg>\n"
    );
    assert_eq!(
        client.request(
            5,
            DOCUMENT_COLOR_METHOD,
            json!({"textDocument": {"uri": plain_uri}})
        ),
        json!([])
    );
    let symbols = client.request(
        6,
        SYMBOL_METHOD,
        json!({"textDocument": {"uri": plain_uri}}),
    );
    assert_eq!(symbols.as_array().map(Vec::len), Some(2));

    assert_eq!(client.request(7, "shutdown", json!(null)), Value::Null);
    client.notify(EXIT_METHOD, json!(null));
    server_thread.join().expect("server thread should stop");
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn pulls_and_reacts_to_configuration_changes() {
    let uri = "file:///configuration/broken.xml";
    let (server, connection) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let client = TestClient {
        connection,
        diagnostics: Default::default(),
    };
    client.request(
        1,
        INITIALIZE_METHOD,
        json!({
            "capabilities": {"workspace": {"configuration": true}},
            "initializationOptions": {"xml": {"format": {"tabSize": 4}}},
        }),
    );
    client.notify("initialized", json!({}));
    let answer_configuration = |section: Value| match client.next() {
        Message::Request(request) => {
            assert_eq!(request.method, CONFIGURATION_METHOD);
            assert_eq!(request.params, json!({"items": [{"section": "xml"}]}));
            client.send(Response::new_ok(request.id, json!([section])).into());
        }
        message => panic!("unexpected message {message:?}"),
    };
    // The client returns nothing: the initialization options remain.
    answer_configuration(Value::Null);

    let source = "<root><a></root>";
    client.notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": uri, "text": source}}),
    );
    let published = client.take_diagnostics(2);
    assert_eq!(published.len(), 1);
    assert!(!codes(&published[0]).is_empty());

    // Pushed settings: validation is disabled, diagnostics cleared.
    client.notify(
        DID_CHANGE_CONFIGURATION_METHOD,
        json!({"settings": {"xml": {"validation": {"enabled": false}}}}),
    );
    let published = client.take_diagnostics(3);
    assert_eq!(published, vec![json!({"uri": uri, "diagnostics": []})]);

    // A change without effect on validation republishes nothing, and the
    // initialization options remain the base.
    client.notify(
            DID_CHANGE_CONFIGURATION_METHOD,
            json!({"settings": {"xml": {"validation": {"enabled": false}, "format": {"insertSpaces": false}}}}),
        );
    assert!(client.take_diagnostics(4).is_empty());
    let edits = client.request(5, FORMATTING_METHOD, json!({"textDocument": {"uri": uri}}));
    assert_eq!(edits, json!([]), "malformed documents are not formatted");

    // Without a pushed section, the server requests the configuration again.
    client.notify(DID_CHANGE_CONFIGURATION_METHOD, json!({"settings": null}));
    answer_configuration(json!({"validation": {"enabled": true}}));
    let published = client.take_diagnostics(6);
    assert_eq!(published.len(), 1);
    assert_eq!(published[0]["uri"], uri);
    assert!(!codes(&published[0]).is_empty());

    // Formatting disabled.
    client.notify(
        DID_CHANGE_CONFIGURATION_METHOD,
        json!({"settings": {"xml": {"format": {"enabled": false}}}}),
    );
    client.notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": "file:///configuration/ok.xml", "text": "<a><b/></a>"}}),
    );
    assert_eq!(
            client.request(
                7,
                FORMATTING_METHOD,
                json!({"textDocument": {"uri": "file:///configuration/ok.xml"}, "options": {"tabSize": 2, "insertSpaces": true}}),
            ),
            json!([])
        );

    assert_eq!(client.request(8, "shutdown", json!(null)), Value::Null);
    client.notify(EXIT_METHOD, json!(null));
    server_thread.join().expect("server thread should stop");
}

#[test]
fn validation_settings_select_schema_diagnostics() {
    let directory =
        std::env::temp_dir().join(format!("xml-lsp-schema-setting {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("directory should be created");
    std::fs::write(
            directory.join("valid.xsd"),
            "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"><xs:element name=\"root\"/></xs:schema>",
        )
        .expect("schema should be written");
    let document_path = directory.join("doc.xml");
    let uri = path_to_uri(&document_path);
    let source = "<other xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"valid.xsd\"/>";
    let broken = "<other xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"missing.xsd\"/>";
    let mut server = XmlLanguageServer::new();
    server.documents.insert(uri.clone(), source.to_owned());
    let codes_for = |server: &mut XmlLanguageServer, source: &str, settings: Value| {
        server.settings = settings::Settings::from_value(&settings);
        codes(&server.diagnostics(&uri, source))
    };
    assert_eq!(
        codes_for(&mut server, source, json!({})),
        vec!["xsd-validation"]
    );
    assert!(
        codes_for(
            &mut server,
            source,
            json!({"validation": {"schema": {"enabled": "never"}}})
        )
        .is_empty()
    );
    let on_valid = json!({"validation": {"schema": {"enabled": "onValidSchema"}}});
    assert_eq!(
        codes_for(&mut server, source, on_valid.clone()),
        vec!["xsd-validation"]
    );
    // Schema not found: only the loading error is reported.
    let diagnostics = {
        server.settings = settings::Settings::from_value(&on_valid);
        server.diagnostics(&uri, broken)
    };
    assert_eq!(diagnostics["diagnostics"].as_array().map(Vec::len), Some(1));
    assert_eq!(diagnostics["diagnostics"][0]["data"]["kind"], "loading");
    // A document bound to a schema is not reported by noGrammar.
    assert_eq!(
        codes_for(
            &mut server,
            source,
            json!({"validation": {"noGrammar": "hint"}})
        ),
        vec!["xsd-validation"]
    );
    assert_eq!(
        codes_for(
            &mut server,
            "<?xml-model href=\"x.rng\"?><r/>",
            json!({"validation": {"noGrammar": "hint"}})
        ),
        Vec::<String>::new()
    );
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn serves_document_colors_and_color_presentations() {
    let (server, client) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let send = |message: Message| client.sender.send(message).expect("message should be sent");
    let request = |id: i32, method: &str, params: Value| {
        send(
            Request {
                id: RequestId::from(id),
                method: method.to_owned(),
                params,
            }
            .into(),
        );
        loop {
            match client.receiver.recv().expect("a message should arrive") {
                Message::Response(response) => {
                    assert_eq!(response.id, RequestId::from(id));
                    return response.result.expect("request should succeed");
                }
                Message::Notification(_) => {}
                message => panic!("unexpected message {message:?}"),
            }
        }
    };
    let notify = |method: &str, params: Value| {
        send(
            Notification {
                method: method.to_owned(),
                params,
            }
            .into(),
        )
    };

    let initialize = request(1, INITIALIZE_METHOD, json!({"capabilities": {}}));
    assert_eq!(initialize["capabilities"]["colorProvider"], true);
    notify("initialized", json!({}));

    let svg = "file:///icon.svg";
    notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": svg, "text": "<svg>\r\n  <!-- é -->\r\n  <rect fill=\"#F008\" stroke=\"rgb(0 128 0)\"/>\r\n</svg>"}}),
    );
    let colors = request(
        2,
        DOCUMENT_COLOR_METHOD,
        json!({"textDocument": {"uri": svg}}),
    );
    assert_eq!(
        colors,
        json!([
            {
                "range": {"start": {"line": 2, "character": 14}, "end": {"line": 2, "character": 19}},
                "color": {"red": 1.0, "green": 0.0, "blue": 0.0, "alpha": 136.0 / 255.0},
            },
            {
                "range": {"start": {"line": 2, "character": 29}, "end": {"line": 2, "character": 41}},
                "color": {"red": 0.0, "green": 128.0 / 255.0, "blue": 0.0, "alpha": 1.0},
            },
        ])
    );

    let range = colors[0]["range"].clone();
    let presentations = request(
        3,
        COLOR_PRESENTATION_METHOD,
        json!({
            "textDocument": {"uri": svg},
            "color": {"red": 0.0, "green": 0.0, "blue": 1.0, "alpha": 1.0},
            "range": range,
        }),
    );
    let labels = presentations
        .as_array()
        .unwrap()
        .iter()
        .map(|presentation| presentation["label"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        labels,
        vec![
            "#00FF",
            "#0000FF",
            "rgb(0, 0, 255)",
            "hsl(240, 100%, 50%)",
            "blue"
        ]
    );
    assert_eq!(
        presentations[0]["textEdit"],
        json!({"range": range, "newText": "#00FF"})
    );

    let resources = "file:///app/res/values/colors.xml";
    notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": resources, "text": "<resources>\n  <color name=\"accent\">#80FF0000</color>\n</resources>"}}),
    );
    let colors = request(
        4,
        DOCUMENT_COLOR_METHOD,
        json!({"textDocument": {"uri": resources}}),
    );
    assert_eq!(colors[0]["color"]["red"], 1.0);
    assert_eq!(colors[0]["color"]["alpha"], 128.0 / 255.0);
    let presentations = request(
        5,
        COLOR_PRESENTATION_METHOD,
        json!({
            "textDocument": {"uri": resources},
            "color": {"red": 0.0, "green": 1.0, "blue": 0.0, "alpha": 1.0},
            "range": colors[0]["range"],
        }),
    );
    assert_eq!(presentations[0]["label"], "#FF00FF00");
    assert_eq!(presentations[1]["label"], "#00FF00");
    assert_eq!(presentations.as_array().map(Vec::len), Some(2));

    let unknown = request(
        6,
        DOCUMENT_COLOR_METHOD,
        json!({"textDocument": {"uri": "file:///closed.svg"}}),
    );
    assert_eq!(unknown, json!([]));

    assert_eq!(request(7, "shutdown", json!(null)), Value::Null);
    notify(EXIT_METHOD, json!(null));
    server_thread.join().expect("server thread should stop");
}

#[test]
fn serves_the_file_types_of_the_xml_language() {
    let directory = std::env::temp_dir().join(format!("xml-lsp-file-types {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    for (path, content) in [
        (
            "ui/MainWindow.xaml",
            "<Window xmlns=\"http://schemas.microsoft.com/winfx/2006/xaml/presentation\">\n  <Button id=\"saveButton\"/>\n</Window>",
        ),
        (
            "res/Strings.resx",
            "<root>\n  <data name=\"saveLabel\"><value>Save</value></data>\n</root>",
        ),
        (
            "vc/App.vcxproj.filters",
            "<Project>\n  <Filter id=\"saveFilter\"/>\n</Project>",
        ),
        // Claimed by the C# extension, not by the XML language.
        (
            "App.csproj",
            "<Project>\n  <PropertyGroup id=\"saveIgnored\"/>\n</Project>",
        ),
    ] {
        let path = directory.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).expect("directory should be created");
        std::fs::write(path, content).expect("file should be written");
    }

    let (server, connection) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let client = TestClient {
        connection,
        diagnostics: Default::default(),
    };
    client.request(
        1,
        INITIALIZE_METHOD,
        json!({
            "rootUri": path_to_uri(&directory),
            "capabilities": {
                "workspace": {"didChangeWatchedFiles": {"dynamicRegistration": true}},
            },
        }),
    );
    client.notify("initialized", json!({}));
    let registration = expect_server_request(&client, REGISTER_CAPABILITY_METHOD);
    let glob = registration["registrations"][0]["registerOptions"]["watchers"][0]["globPattern"]
        .as_str()
        .expect("a watched files glob")
        .to_owned();
    assert_eq!(glob, symbols::watched_files_glob());
    assert!(settings::glob_match(&glob, "ui/MainWindow.xaml"), "{glob}");
    assert!(!settings::glob_match(&glob, "App.csproj"), "{glob}");

    let mut names = client
        .request(2, WORKSPACE_SYMBOL_METHOD, json!({"query": "save"}))
        .as_array()
        .expect("symbols should be an array")
        .iter()
        .map(|symbol| symbol["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["saveButton", "saveFilter", "saveLabel"]);

    // Open documents of the new types get the usual diagnostics, whatever
    // language identifier the client sends.
    let storyboard = path_to_uri(&directory.join("Main.storyboard"));
    let gpx = path_to_uri(&directory.join("track.gpx"));
    client.notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": storyboard, "languageId": "xml", "version": 1,
                "text": "<document>\r\n  <scenes>\r\n</document>"}}),
    );
    client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": gpx, "languageId": "XML", "version": 1,
                "text": "<gpx version=\"1.1\" creator=\"t\"><trk><name>\u{e9}t\u{e9}</name></trk></gpx>"}}),
        );
    let published = client.take_diagnostics(3);
    assert_eq!(published.len(), 2, "{published:?}");
    assert_eq!(published[0]["uri"], storyboard);
    assert!(
        codes(&published[0])
            .iter()
            .any(|code| code.starts_with("xml-")),
        "{published:?}"
    );
    assert_eq!(published[1]["uri"], gpx);
    assert!(
        !codes(&published[1])
            .iter()
            .any(|code| code.starts_with("xml-")),
        "{published:?}"
    );

    assert_eq!(client.request(4, "shutdown", json!(null)), Value::Null);
    client.notify(EXIT_METHOD, json!(null));
    server_thread.join().expect("server thread should stop");
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn serves_workspace_and_hierarchical_document_symbols() {
    let directory =
        std::env::temp_dir().join(format!("xml-lsp-workspace-symbols {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    for (path, content) in [
        (
            "schemas/shop.xsd",
            "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" targetNamespace=\"urn:shop\">\n  <xs:element name=\"order\"/>\n  <xs:complexType name=\"OrderType\"/>\n</xs:schema>",
        ),
        (
            "config/beans.xml",
            "<beans>\n  <bean id=\"orderService\"/>\n</beans>",
        ),
        (
            "node_modules/lib/order.xsd",
            "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\"><xs:element name=\"orderIgnored\"/></xs:schema>",
        ),
        (".hidden/order.xml", "<order id=\"orderHidden\"/>"),
        ("target/order.xml", "<order id=\"orderBuilt\"/>"),
    ] {
        let path = directory.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).expect("directory should be created");
        std::fs::write(path, content).expect("file should be written");
    }
    let beans_uri = path_to_uri(&directory.join("config/beans.xml"));

    let (server, client) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let send = |message: Message| client.sender.send(message).expect("message should be sent");
    let request = |id: i32, method: &str, params: Value| {
        send(
            Request {
                id: RequestId::from(id),
                method: method.to_owned(),
                params,
            }
            .into(),
        );
        loop {
            match client.receiver.recv().expect("a message should arrive") {
                Message::Response(response) => {
                    assert_eq!(response.id, RequestId::from(id));
                    return response.result.expect("request should succeed");
                }
                Message::Notification(_) => {}
                message => panic!("unexpected message {message:?}"),
            }
        }
    };
    let notify = |method: &str, params: Value| {
        send(
            Notification {
                method: method.to_owned(),
                params,
            }
            .into(),
        )
    };
    let names = |symbols: &Value| {
        symbols
            .as_array()
            .expect("symbols should be an array")
            .iter()
            .map(|symbol| symbol["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };

    let initialize = request(
        1,
        INITIALIZE_METHOD,
        json!({
            "rootUri": "file:///ignored-when-workspace-folders-exist",
            "workspaceFolders": [{"uri": path_to_uri(&directory), "name": "shop"}],
            "capabilities": {
                "textDocument": {"documentSymbol": {"hierarchicalDocumentSymbolSupport": true}},
                "workspace": {"didChangeWatchedFiles": {"dynamicRegistration": true}},
            },
        }),
    );
    assert_eq!(initialize["capabilities"]["workspaceSymbolProvider"], true);
    notify("initialized", json!({}));
    match client
        .receiver
        .recv()
        .expect("a registration should arrive")
    {
        Message::Request(registration) => {
            assert_eq!(registration.method, REGISTER_CAPABILITY_METHOD);
            assert_eq!(
                registration.params["registrations"][0]["method"],
                DID_CHANGE_WATCHED_FILES_METHOD
            );
            send(Response::new_ok(registration.id, Value::Null).into());
        }
        message => panic!("unexpected message {message:?}"),
    }

    let symbols = request(2, WORKSPACE_SYMBOL_METHOD, json!({"query": "order"}));
    assert_eq!(names(&symbols), vec!["order", "OrderType", "orderService"]);
    assert_eq!(
        symbols[0],
        json!({
            "name": "order",
            "kind": symbols::kind::FIELD,
            "containerName": "urn:shop",
            "location": {
                "uri": path_to_uri(&directory.join("schemas/shop.xsd")),
                "range": {
                    "start": {"line": 1, "character": 20},
                    "end": {"line": 1, "character": 25},
                },
            },
        })
    );
    assert_eq!(symbols[1]["kind"], symbols::kind::CLASS);
    assert_eq!(symbols[2]["containerName"], "bean");

    // An unsaved open buffer replaces the file on disk.
    notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": beans_uri, "text": "<beans>\r\n  <bean id=\"orderRepository\"><property name=\"dataSource\"/></bean>\r\n</beans>"}}),
    );
    let symbols = request(3, WORKSPACE_SYMBOL_METHOD, json!({"query": "OrdRep"}));
    assert_eq!(names(&symbols), vec!["orderRepository"]);
    assert_eq!(
        symbols[0]["location"]["range"]["start"],
        json!({"line": 1, "character": 12})
    );

    let document = request(
        4,
        SYMBOL_METHOD,
        json!({"textDocument": {"uri": beans_uri}}),
    );
    assert_eq!(document[0]["name"], "beans");
    assert_eq!(document[0]["children"][0]["name"], "bean");
    assert_eq!(
        document[0]["children"][0]["detail"],
        "id=\"orderRepository\""
    );
    assert_eq!(
        document[0]["children"][0]["children"][0]["detail"],
        "name=\"dataSource\""
    );

    let all = request(5, WORKSPACE_SYMBOL_METHOD, json!({"query": ""}));
    assert_eq!(all.as_array().map(Vec::len), Some(5));

    notify(
        DID_CHANGE_WORKSPACE_FOLDERS_METHOD,
        json!({"event": {"added": [], "removed": [{"uri": path_to_uri(&directory), "name": "shop"}]}}),
    );
    let symbols = request(6, WORKSPACE_SYMBOL_METHOD, json!({"query": "order"}));
    assert_eq!(names(&symbols), vec!["orderRepository"]);

    assert_eq!(request(7, "shutdown", json!(null)), Value::Null);
    notify(EXIT_METHOD, json!(null));
    server_thread.join().expect("server thread should stop");
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn serves_code_actions_that_fix_published_diagnostics() {
    let directory =
        std::env::temp_dir().join(format!("xml-lsp-code-actions {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("directory should be created");
    std::fs::write(
        directory.join("items.xsd"),
        r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="items"><xs:complexType><xs:sequence>
    <xs:element name="item" maxOccurs="unbounded"><xs:complexType>
      <xs:attribute name="id" type="xs:string" use="required"/>
      <xs:attribute name="kind" use="required"><xs:simpleType><xs:restriction base="xs:string">
        <xs:enumeration value="book"/><xs:enumeration value="disc"/>
      </xs:restriction></xs:simpleType></xs:attribute>
    </xs:complexType></xs:element>
  </xs:sequence></xs:complexType></xs:element>
</xs:schema>"#,
    )
    .expect("schema should be written");
    let uri = path_to_uri(&directory.join("items.xml"));

    let (server, client) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let send = |message: Message| client.sender.send(message).expect("message should be sent");
    let request = |id: i32, method: &str, params: Value| {
        send(
            Request {
                id: RequestId::from(id),
                method: method.to_owned(),
                params,
            }
            .into(),
        );
        loop {
            match client.receiver.recv().expect("a message should arrive") {
                Message::Response(response) => {
                    assert_eq!(response.id, RequestId::from(id));
                    return response.result.expect("request should succeed");
                }
                Message::Notification(_) => {}
                message => panic!("unexpected message {message:?}"),
            }
        }
    };
    let notify = |method: &str, params: Value| {
        send(
            Notification {
                method: method.to_owned(),
                params,
            }
            .into(),
        )
    };
    let diagnostics = || match client
        .receiver
        .recv()
        .expect("diagnostics should be published")
    {
        Message::Notification(notification)
            if notification.method == PUBLISH_DIAGNOSTICS_METHOD =>
        {
            notification.params["diagnostics"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        }
        message => panic!("unexpected message {message:?}"),
    };

    let initialize = request(1, INITIALIZE_METHOD, json!({"capabilities": {}}));
    assert_eq!(
        initialize["capabilities"]["codeActionProvider"],
        json!({"codeActionKinds": ["quickfix", "refactor", "source"]})
    );
    notify("initialized", json!({}));

    // Each preferred quick fix is applied, then the document is
    // revalidated: the fixed diagnostic disappears.
    let mut source = "<items xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\r\n       xsi:noNamespaceSchemaLocation=\"items.xsd\">\r\n  <item id=\"é1\" kind=\"bok\"></itme>\r\n  <item/>\r\n</items>".to_owned();
    notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": uri, "text": source}}),
    );
    let mut published = diagnostics();
    let mut version = 1;
    let mut applied = Vec::new();
    for id in 2.. {
        let Some(diagnostic) = published.first().cloned() else {
            break;
        };
        assert!(id < 10, "diagnostics should converge: {published:#?}");
        let actions = request(
            id,
            CODE_ACTION_METHOD,
            json!({
                "textDocument": {"uri": uri},
                "range": diagnostic["range"],
                "context": {"diagnostics": [diagnostic], "only": ["quickfix"]},
            }),
        );
        let action = actions
            .as_array()
            .unwrap()
            .iter()
            .find(|action| action["isPreferred"] == true)
            .unwrap_or_else(|| panic!("no preferred fix for {diagnostic:#?}: {actions:#?}"))
            .clone();
        assert_eq!(action["diagnostics"], json!([diagnostic]));
        applied.push(action["title"].as_str().unwrap().to_owned());
        source = formatting::apply_edits(&source, &action["edit"]["changes"][&uri]);
        version += 1;
        notify(
            DID_CHANGE_METHOD,
            json!({
                "textDocument": {"uri": uri, "version": version},
                "contentChanges": [{"text": source}],
            }),
        );
        let remaining = diagnostics();
        assert!(
            !remaining.contains(&diagnostic),
            "{diagnostic:#?} should be fixed by {applied:?}"
        );
        published = remaining;
    }
    assert_eq!(
        applied,
        vec![
            "Replace </itme> with </item>",
            "Add required attributes id, kind",
            "Replace with `book`",
        ]
    );
    assert_eq!(
        source,
        "<items xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\r\n       xsi:noNamespaceSchemaLocation=\"items.xsd\">\r\n  <item id=\"é1\" kind=\"book\"></item>\r\n  <item id=\"\" kind=\"book\"/>\r\n</items>"
    );

    // Rewrite and schema binding on a document without a schema.
    let other = path_to_uri(&directory.join("other.xml"));
    notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": other, "text": "<root><a></a></root>"}}),
    );
    assert_eq!(diagnostics(), Vec::<Value>::new());
    let actions = request(
        20,
        CODE_ACTION_METHOD,
        json!({
            "textDocument": {"uri": other},
            "range": {"start": {"line": 0, "character": 7}, "end": {"line": 0, "character": 7}},
            "context": {"diagnostics": []},
        }),
    );
    let titles = actions
        .as_array()
        .unwrap()
        .iter()
        .map(|action| {
            (
                action["kind"].as_str().unwrap(),
                action["title"].as_str().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        titles,
        vec![
            ("refactor.rewrite", "Convert <a></a> to self-closing <a/>"),
            ("source", "Bind the document to the XSD schema items.xsd"),
        ]
    );
    assert_eq!(
        request(
            21,
            CODE_ACTION_METHOD,
            json!({
                "textDocument": {"uri": "file:///missing.xml"},
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}},
                "context": {"diagnostics": []},
            }),
        ),
        json!([])
    );

    assert_eq!(request(22, "shutdown", json!(null)), Value::Null);
    notify(EXIT_METHOD, json!(null));
    server_thread.join().expect("server thread should stop");
    std::fs::remove_dir_all(directory).expect("directory should be removed");
}

#[test]
fn serves_dtd_diagnostics_completion_hover_definition_and_fixes() {
    let directory = std::env::temp_dir().join(format!("xml-lsp-dtd {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("directory should be created");
    let dtd_path = directory.join("note.dtd");
    std::fs::write(
            &dtd_path,
            "<!-- Root note. -->\n<!ELEMENT note (to, body?)>\n<!ELEMENT to (#PCDATA)>\n<!ELEMENT body (#PCDATA)>\n<!ATTLIST note lang (fr | en) #REQUIRED>\n",
        )
        .expect("dtd should be written");
    let uri = path_to_uri(&directory.join("note.xml"));
    // Internal subset + external DTD, CRLF line endings.
    let source = "<!DOCTYPE note SYSTEM \"note.dtd\" [\r\n  <!ENTITY sig \"Alice\">\r\n]>\r\n<note lang=\"de\">\r\n  <to>Bob &sig; &unknown;</to>\r\n  <extra/>\r\n</note>";
    let broken_uri = path_to_uri(&directory.join("broken.dtd"));
    let broken = "<!ELEMENT a (b | c, d)>\n<!ELEMENT b EMPTY>";
    let typing_uri = path_to_uri(&directory.join("typing.xml"));
    let typing = "<!DOCTYPE note SYSTEM \"note.dtd\"><note lang=\"fr\"><";

    let (server, connection) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let client = TestClient {
        connection,
        diagnostics: Default::default(),
    };
    let initialize = client.request(1, INITIALIZE_METHOD, json!({"capabilities": {}}));
    assert_eq!(
        initialize["capabilities"]["completionProvider"]["triggerCharacters"],
        json!(["<", " ", "/", ">", "=", "\"", "?", "&", "%", "$"])
    );
    client.notify("initialized", json!({}));
    for (document_uri, text) in [(&uri, source), (&broken_uri, broken), (&typing_uri, typing)] {
        client.notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": document_uri, "text": text}}),
        );
    }
    let published = client.take_diagnostics(2);
    assert_eq!(published.len(), 3, "{published:?}");
    assert_eq!(published[0]["uri"], uri);
    assert_eq!(
        codes(&published[0]),
        vec![
            "xml-entity",
            "dtd-validation",
            "dtd-validation",
            "dtd-validation",
        ],
        "{published:?}"
    );
    assert_eq!(
        diagnostic_kinds(&published[0]),
        vec![
            "undefinedEntity",
            "invalidEnumeration",
            "unexpectedElement",
            "undeclaredElement",
        ]
    );
    assert_eq!(
        published[0]["diagnostics"][1]["range"],
        json!({"start": {"line": 3, "character": 12}, "end": {"line": 3, "character": 14}})
    );
    // DTD file: DTD errors only (no XML well-formedness).
    assert_eq!(codes(&published[1]), vec!["dtd-grammar"]);
    assert_eq!(
        published[1]["diagnostics"][0]["range"]["start"],
        json!({"line": 0, "character": 18})
    );
    // Document being typed: XML errors, no DTD error.
    assert!(
        !codes(&published[2])
            .iter()
            .any(|code| code.starts_with("dtd")),
        "{published:?}"
    );

    // Completion: children allowed by the content model.
    let completion = client.request(
        3,
        COMPLETION_METHOD,
        json!({
            "textDocument": {"uri": typing_uri},
            "position": {"line": 0, "character": typing.len()},
        }),
    );
    let items = completion["items"].as_array().unwrap();
    let to = items
        .iter()
        .find(|item| item["label"] == "to")
        .expect("`to` should be proposed");
    assert_eq!(to["detail"], "<!ELEMENT to (#PCDATA)>");
    assert!(!items.iter().any(|item| item["label"] == "body"));
    // Completion in the internal subset.
    let completion = client.request(
        4,
        COMPLETION_METHOD,
        json!({
            "textDocument": {"uri": uri},
            "position": {"line": 1, "character": 4},
        }),
    );
    let labels = completion["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["label"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(labels, vec!["ELEMENT", "ATTLIST", "ENTITY", "NOTATION"]);

    // Hover: declaration, attributes and comment of the external DTD.
    let hover = client.request(
        5,
        HOVER_METHOD,
        json!({"textDocument": {"uri": uri}, "position": {"line": 3, "character": 2}}),
    );
    let markdown = hover["contents"]["value"].as_str().unwrap();
    assert!(
        markdown.contains("<!ELEMENT note (to, body?)>"),
        "{markdown}"
    );
    assert!(
        markdown.contains("<!ATTLIST note lang (fr | en) #REQUIRED>"),
        "{markdown}"
    );
    assert!(markdown.contains("Root note."), "{markdown}");
    assert_eq!(
        hover["range"],
        json!({"start": {"line": 3, "character": 1}, "end": {"line": 3, "character": 5}})
    );

    // Definition: entity of the internal subset, element of the DTD.
    let definition = client.request(
        6,
        DEFINITION_METHOD,
        json!({"textDocument": {"uri": uri}, "position": {"line": 4, "character": 11}}),
    );
    assert_eq!(
        definition,
        json!([{"uri": uri, "range": {"start": {"line": 1, "character": 11}, "end": {"line": 1, "character": 14}}}])
    );
    let definition = client.request(
        7,
        DEFINITION_METHOD,
        json!({"textDocument": {"uri": uri}, "position": {"line": 4, "character": 3}}),
    );
    assert_eq!(definition[0]["uri"], path_to_uri(&dtd_path));
    assert_eq!(
        definition[0]["range"]["start"],
        json!({"line": 2, "character": 10})
    );

    // Fixes: enumerated value and entity to declare (CRLF preserved).
    let actions = client.request(
        8,
        CODE_ACTION_METHOD,
        json!({
            "textDocument": {"uri": uri},
            "range": {"start": {"line": 3, "character": 12}, "end": {"line": 4, "character": 20}},
            "context": {"diagnostics": published[0]["diagnostics"].clone()},
        }),
    );
    let titles = actions
        .as_array()
        .unwrap()
        .iter()
        .map(|action| action["title"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    for expected in [
        "Declare the entity '&unknown;' in the DOCTYPE",
        "Replace with 'fr'",
        "Replace with 'en'",
    ] {
        assert!(titles.iter().any(|title| title == expected), "{titles:?}");
    }
    let declare = actions
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["title"] == "Declare the entity '&unknown;' in the DOCTYPE")
        .unwrap();
    assert_eq!(
        declare["edit"]["changes"][&uri][0],
        json!({
            "range": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 0}},
            "newText": "  <!ENTITY unknown \"\">\r\n",
        })
    );
    assert_eq!(declare["diagnostics"][0]["data"]["kind"], "undefinedEntity");

    // Symbols and formatting of a DTD file.
    let symbols = client.request(
        9,
        SYMBOL_METHOD,
        json!({"textDocument": {"uri": broken_uri}}),
    );
    let names = symbols
        .as_array()
        .unwrap()
        .iter()
        .map(|symbol| symbol["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["a", "b"]);
    let formatting = client.request(
            10,
            FORMATTING_METHOD,
            json!({"textDocument": {"uri": broken_uri}, "options": {"tabSize": 2, "insertSpaces": true}}),
        );
    assert_eq!(formatting, json!([]));

    // Fixing the document: no diagnostics left.
    client.notify(
            DID_CHANGE_METHOD,
            json!({
                "textDocument": {"uri": uri, "version": 2},
                "contentChanges": [{"text": "<!DOCTYPE note SYSTEM \"note.dtd\" [\r\n  <!ENTITY sig \"Alice\">\r\n]>\r\n<note lang=\"fr\">\r\n  <to>Bob &sig;</to>\r\n</note>"}],
            }),
        );
    let published = client.take_diagnostics(11);
    assert_eq!(published.len(), 1);
    assert!(codes(&published[0]).is_empty(), "{published:?}");

    client.request(12, "shutdown", json!(null));
    client.notify(EXIT_METHOD, json!(null));
    server_thread.join().expect("server thread should stop");
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn serves_document_links_and_link_definitions() {
    let directory =
        std::env::temp_dir().join(format!("xml-lsp-document-links {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(directory.join("schémas")).expect("directory should be created");
    let schema_path = directory.join("schémas").join("a b.xsd");
    std::fs::write(
            &schema_path,
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="root"/></xs:schema>"#,
        )
        .expect("schema should be written");
    let stylesheet_path = directory.join("style.xsl");
    std::fs::write(&stylesheet_path, "<xsl:stylesheet/>").expect("stylesheet should be written");
    let uri = path_to_uri(&directory.join("document.xml"));
    let source = "<?xml-stylesheet type=\"text/xsl\" href=\"style.xsl\"?>\r\n\
<root xmlns:i=\"http://www.w3.org/2001/XMLSchema-instance\"\r\n      \
i:schemaLocation=\"urn:😀 sch%C3%A9mas/a%20b.xsd urn:x https://example.com/x.xsd\"\r\n      \
i:noNamespaceSchemaLocation=\"missing.xsd\">\r\n  \
<child/>\r\n\
</root>";

    let (server, client) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let request = |id: i32, method: &str, params: Value| {
        client
            .sender
            .send(
                Request {
                    id: RequestId::from(id),
                    method: method.to_owned(),
                    params,
                }
                .into(),
            )
            .expect("request should be sent");
        loop {
            match client.receiver.recv().expect("a message should arrive") {
                Message::Response(response) => {
                    assert_eq!(response.id, RequestId::from(id));
                    return response.result;
                }
                Message::Notification(_) => {}
                message => panic!("unexpected message {message:?}"),
            }
        }
    };
    let notify = |method: &str, params: Value| {
        client
            .sender
            .send(
                Notification {
                    method: method.to_owned(),
                    params,
                }
                .into(),
            )
            .expect("notification should be sent");
    };
    let range = |start: (u32, u32), end: (u32, u32)| {
        json!({
            "start": {"line": start.0, "character": start.1},
            "end": {"line": end.0, "character": end.1},
        })
    };
    let zero = range((0, 0), (0, 0));

    let initialize = request(
        1,
        INITIALIZE_METHOD,
        json!({"capabilities": {"textDocument": {"definition": {"linkSupport": true}}}}),
    );
    assert_eq!(
        initialize.unwrap()["capabilities"]["documentLinkProvider"],
        json!({"resolveProvider": false})
    );
    notify("initialized", json!({}));
    notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": uri, "text": source}}),
    );

    // Value at column 24; "urn:😀 ": the emoji counts as two UTF-16 code units.
    let schema = 24 + 7;
    let schema_range = range((2, schema), (2, schema + 22));
    assert_eq!(
        request(
            2,
            DOCUMENT_LINK_METHOD,
            json!({"textDocument": {"uri": uri}})
        ),
        Some(json!([
            {
                "range": range((0, 39), (0, 48)),
                "target": path_to_uri(&stylesheet_path),
                "tooltip": format!("Open stylesheet: {}", stylesheet_path.display()),
            },
            {
                "range": schema_range,
                "target": path_to_uri(&schema_path),
                "tooltip": format!("Open XSD schema: {}", schema_path.display()),
            },
            {
                "range": range((2, schema + 29), (2, schema + 54)),
                "target": "https://example.com/x.xsd",
                "tooltip": "Open XSD schema: https://example.com/x.xsd",
            },
        ]))
    );

    let definition = |id: i32, line: u32, character: u32| {
        request(
            id,
            DEFINITION_METHOD,
            json!({
                "textDocument": {"uri": uri},
                "position": {"line": line, "character": character},
            }),
        )
    };
    assert_eq!(
        definition(3, 2, schema + 4),
        Some(json!([{
            "originSelectionRange": schema_range,
            "targetUri": path_to_uri(&schema_path),
            "targetRange": zero,
            "targetSelectionRange": zero,
        }]))
    );
    assert_eq!(
        definition(4, 0, 42).unwrap()[0]["targetUri"],
        path_to_uri(&stylesheet_path)
    );
    // URL and missing file: no location (the URL remains a link).
    assert_eq!(definition(5, 2, schema + 30), Some(json!([])));
    assert_eq!(definition(6, 3, 36), Some(json!([])));
    // Outside link values, element definition is kept.
    let element = definition(7, 1, 5).expect("element definition should be found");
    assert_eq!(element[0]["uri"], uri);
    assert_eq!(element[0]["range"], range((1, 0), (1, 5)));

    assert_eq!(
        request(
            8,
            DOCUMENT_LINK_METHOD,
            json!({"textDocument": {"uri": "file:///missing.xml"}})
        ),
        Some(json!([]))
    );

    assert_eq!(request(9, "shutdown", json!(null)), Some(Value::Null));
    notify(EXIT_METHOD, json!(null));
    server_thread.join().expect("server thread should stop");
    std::fs::remove_dir_all(directory).expect("directory should be removed");
}

#[test]
fn serves_document_and_range_formatting_with_options() {
    let (server, client) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let request = |id: i32, method: &str, params: Value| {
        client
            .sender
            .send(
                Request {
                    id: RequestId::from(id),
                    method: method.to_owned(),
                    params,
                }
                .into(),
            )
            .expect("request should be sent");
        loop {
            match client.receiver.recv().expect("a message should arrive") {
                Message::Response(response) => {
                    assert_eq!(response.id, RequestId::from(id));
                    return response.result.expect("a result should be returned");
                }
                Message::Notification(_) => {}
                message => panic!("unexpected message {message:?}"),
            }
        }
    };
    let notify = |method: &str, params: Value| {
        client
            .sender
            .send(
                Notification {
                    method: method.to_owned(),
                    params,
                }
                .into(),
            )
            .expect("notification should be sent");
    };
    let open = |uri: &str, text: &str| {
        notify(
            DID_OPEN_METHOD,
            json!({"textDocument": {"uri": uri, "text": text}}),
        );
    };
    let range = |start: (u32, u32), end: (u32, u32)| {
        json!({
            "start": {"line": start.0, "character": start.1},
            "end": {"line": end.0, "character": end.1},
        })
    };

    let initialize = request(1, INITIALIZE_METHOD, json!({"capabilities": {}}));
    assert_eq!(
        initialize["capabilities"]["documentRangeFormattingProvider"],
        true
    );
    notify("initialized", json!({}));

    // Whole formatting: four spaces, CRLF, UTF-16, trailing whitespace.
    let uri = "file:///crlf.xml";
    let source = "<root>\r\n<outer><😀 a=\"1\"><b/></😀></outer>\r\n<c>t  \r\n</c>\r\n</root>";
    open(uri, source);
    let options = json!({
        "tabSize": 4,
        "insertSpaces": true,
        "trimTrailingWhitespace": true,
        "insertFinalNewline": true,
    });
    let edits = request(
        2,
        FORMATTING_METHOD,
        json!({"textDocument": {"uri": uri}, "options": options}),
    );
    assert!(edits.as_array().unwrap().len() > 1, "{edits}");
    assert!(
        edits
            .as_array()
            .unwrap()
            .iter()
            .all(|edit| edit["range"]["start"]["line"] != 0),
        "the unchanged first line must not be edited: {edits}"
    );
    let formatted = formatting::apply_edits(source, &edits);
    assert_eq!(
        formatted,
        "<root>\r\n    <outer>\r\n        <😀 a=\"1\">\r\n            <b/>\r\n        </😀>\r\n    </outer>\r\n    <c>t\r\n</c>\r\n</root>\r\n"
    );
    notify(
        DID_CHANGE_METHOD,
        json!({
            "textDocument": {"uri": uri, "version": 2},
            "contentChanges": [{"text": formatted}],
        }),
    );
    assert_eq!(
        request(
            3,
            FORMATTING_METHOD,
            json!({"textDocument": {"uri": uri}, "options": options}),
        ),
        json!([]),
        "formatting twice must not change anything"
    );
    // Range on an already formatted document: no change.
    assert_eq!(
        request(
            4,
            RANGE_FORMATTING_METHOD,
            json!({
                "textDocument": {"uri": uri},
                "range": range((2, 9), (4, 3)),
                "options": options,
            }),
        ),
        json!([])
    );

    // Range formatting with tabs in a document that is invalid outside
    // the range.
    let uri = "file:///malformed.xml";
    let source = "<root>\n<broken>\n  <outer><é><b/></é></outer>\n<oops></root>\n";
    open(uri, source);
    let tabs = json!({"tabSize": 4, "insertSpaces": false});
    let format_range = |id: i32, selection: Value| {
        let edits = request(
            id,
            RANGE_FORMATTING_METHOD,
            json!({
                "textDocument": {"uri": uri},
                "range": selection,
                "options": tabs,
            }),
        );
        formatting::apply_edits(source, &edits)
    };
    // Nested element: `<b/>` starts at UTF-16 column 12.
    assert_eq!(
        format_range(5, range((2, 12), (2, 16))),
        "<root>\n<broken>\n  <outer><é>\n\t\t\t<b/>\n\t\t</é></outer>\n<oops></root>\n"
    );
    // Range starting in `<outer` and ending in `</é>`: expanded to the
    // whole `outer` element.
    let expanded = format_range(6, range((2, 4), (2, 17)));
    assert_eq!(
        expanded,
        "<root>\n<broken>\n\t<outer>\n\t\t<é>\n\t\t\t<b/>\n\t\t</é>\n\t</outer>\n<oops></root>\n"
    );
    // The invalid region is never modified.
    assert_eq!(format_range(7, range((3, 0), (3, 6))), source);
    // Range formatting is idempotent.
    notify(
        DID_CHANGE_METHOD,
        json!({
            "textDocument": {"uri": uri, "version": 2},
            "contentChanges": [{"text": expanded}],
        }),
    );
    assert_eq!(
        request(
            8,
            RANGE_FORMATTING_METHOD,
            json!({
                "textDocument": {"uri": uri},
                "range": range((2, 1), (6, 3)),
                "options": tabs,
            }),
        ),
        json!([])
    );
    // Whole formatting refuses the invalid document.
    assert_eq!(
        request(
            9,
            FORMATTING_METHOD,
            json!({"textDocument": {"uri": uri}, "options": tabs}),
        ),
        json!([])
    );

    assert_eq!(request(10, "shutdown", json!(null)), Value::Null);
    notify(EXIT_METHOD, json!(null));
    server_thread.join().expect("server thread should stop");
}

#[test]
fn serves_prepare_rename_and_rename_requests() {
    let (server, client) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let request = |id: i32, method: &str, params: Value| {
        client
            .sender
            .send(
                Request {
                    id: RequestId::from(id),
                    method: method.to_owned(),
                    params,
                }
                .into(),
            )
            .expect("request should be sent");
        loop {
            match client.receiver.recv().expect("a message should arrive") {
                Message::Response(response) => {
                    assert_eq!(response.id, RequestId::from(id));
                    return response;
                }
                Message::Notification(_) => {}
                message => panic!("unexpected message {message:?}"),
            }
        }
    };
    let notify = |method: &str, params: Value| {
        client
            .sender
            .send(
                Notification {
                    method: method.to_owned(),
                    params,
                }
                .into(),
            )
            .expect("notification should be sent");
    };
    let range = |line: u32, start: u32, end: u32| json!({"start": {"line": line, "character": start}, "end": {"line": line, "character": end}});

    let initialize = request(1, INITIALIZE_METHOD, json!({})).result.unwrap();
    assert_eq!(
        initialize["capabilities"]["renameProvider"],
        json!({"prepareProvider": true})
    );
    notify("initialized", json!({}));
    notify(
        DID_OPEN_METHOD,
        json!({
            "textDocument": {
                "uri": "file:///document.xml",
                "text": "<ns:root xmlns:ns=\"urn:x\">\r\n  <ns:é😀 a=\"1\"/>\r\n  <item>text</item>\r\n</ns:root>",
            }
        }),
    );
    let params = |line: u32, character: u32| {
        json!({
            "textDocument": {"uri": "file:///document.xml"},
            "position": {"line": line, "character": character},
        })
    };
    let rename_params = |line: u32, character: u32, new_name: &str| {
        let mut params = params(line, character);
        params["newName"] = json!(new_name);
        params
    };

    assert_eq!(
        request(2, PREPARE_RENAME_METHOD, params(2, 16)).result,
        Some(json!({"range": range(2, 14, 18), "placeholder": "item"}))
    );
    assert_eq!(
        request(3, RENAME_METHOD, rename_params(2, 16, "entry")).result,
        Some(json!({"changes": {"file:///document.xml": [
            {"range": range(2, 3, 7), "newText": "entry"},
            {"range": range(2, 14, 18), "newText": "entry"},
        ]}}))
    );
    // Prefix: declaration and uses, UTF-16 positions.
    assert_eq!(
        request(4, PREPARE_RENAME_METHOD, params(1, 4)).result,
        Some(json!({"range": range(1, 3, 5), "placeholder": "ns"}))
    );
    assert_eq!(
        request(5, RENAME_METHOD, rename_params(0, 16, "p")).result,
        Some(json!({"changes": {"file:///document.xml": [
            {"range": range(0, 1, 3), "newText": "p"},
            {"range": range(0, 15, 17), "newText": "p"},
            {"range": range(1, 3, 5), "newText": "p"},
            {"range": range(3, 2, 4), "newText": "p"},
        ]}}))
    );
    assert_eq!(
        request(6, PREPARE_RENAME_METHOD, params(1, 8)).result,
        Some(json!({"range": range(1, 3, 9), "placeholder": "ns:é😀"}))
    );
    // Content: nothing to rename.
    assert_eq!(
        request(7, PREPARE_RENAME_METHOD, params(2, 10)).result,
        Some(Value::Null)
    );
    assert_eq!(
        request(8, RENAME_METHOD, rename_params(2, 10, "x")).result,
        Some(Value::Null)
    );
    // Invalid name: InvalidParams error.
    let invalid = request(9, RENAME_METHOD, rename_params(2, 16, "1 bad"));
    let error = invalid.error.expect("an error should be returned");
    assert_eq!(error.code, rename::INVALID_PARAMS);
    assert!(error.message.contains("1 bad"));
    assert!(
        request(10, RENAME_METHOD, params(2, 16))
            .error
            .is_some_and(|error| error.code == rename::INVALID_PARAMS)
    );
    assert_eq!(
        request(
            11,
            RENAME_METHOD,
            json!({
                "textDocument": {"uri": "file:///missing.xml"},
                "position": {"line": 0, "character": 1},
                "newName": "x",
            })
        )
        .result,
        Some(Value::Null)
    );

    assert_eq!(
        request(12, "shutdown", json!(null)).result,
        Some(Value::Null)
    );
    notify(EXIT_METHOD, json!(null));
    server_thread.join().expect("server thread should stop");
}

#[test]
fn serves_xslt_diagnostics_navigation_rename_hover_and_completion() {
    let directory = std::env::temp_dir().join(format!("xml-lsp-xslt {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("directory should be created");
    let library_path = directory.join("lib.xsl");
    std::fs::write(
            &library_path,
            "<xsl:stylesheet version=\"3.0\" xmlns:xsl=\"http://www.w3.org/1999/XSL/Transform\">\n  <xsl:variable name=\"prefix\" select=\"'#'\"/>\n  <xsl:template name=\"helper\">\n    <xsl:param name=\"label\"/>\n    <xsl:value-of select=\"$label\"/>\n  </xsl:template>\n</xsl:stylesheet>\n",
        )
        .expect("library should be written");
    let library_uri = path_to_uri(&library_path);
    let uri = path_to_uri(&directory.join("main.xsl"));
    let source = "<xsl:stylesheet version=\"3.0\" xmlns:xsl=\"http://www.w3.org/1999/XSL/Transform\">\n  <xsl:include href=\"lib.xsl\"/>\n  <xsl:template match=\"/\">\n    <xsl:variable name=\"count\" select=\"count(//item)\"/>\n    <xsl:for-each select=\"//item[position() le $count]\">\n      <xsl:call-template name=\"helper\">\n        <xsl:with-param name=\"label\" select=\"$prefix\"/>\n      </xsl:call-template>\n    </xsl:for-each>\n    <xsl:if test=\"$count >\"/>\n  </xsl:template>\n</xsl:stylesheet>\n";

    let (server, connection) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let client = TestClient {
        connection,
        diagnostics: Default::default(),
    };
    client.request(1, INITIALIZE_METHOD, json!({"capabilities": {}}));
    client.notify("initialized", json!({}));
    client.notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": uri, "text": source}}),
    );
    let published = client.take_diagnostics(2);
    let diagnostics = published[0]["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter(|diagnostic| diagnostic["code"] == "xpath-syntax")
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 1, "{published:?}");
    assert_eq!(
        diagnostics[0]["range"],
        json!({"start": {"line": 9, "character": 25}, "end": {"line": 9, "character": 26}})
    );
    let range = |line: u32, start: u32, end: u32| json!({"start": {"line": line, "character": start}, "end": {"line": line, "character": end}});
    let position = |line: u32, character: u32| json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": character}});

    // Named template, global variable and template parameter across
    // `xsl:include`.
    assert_eq!(
        client.request(3, DEFINITION_METHOD, position(5, 33)),
        json!([{"uri": library_uri, "range": range(2, 22, 28)}])
    );
    assert_eq!(
        client.request(4, DEFINITION_METHOD, position(6, 48)),
        json!([{"uri": library_uri, "range": range(1, 22, 28)}])
    );
    assert_eq!(
        client.request(5, DEFINITION_METHOD, position(6, 30)),
        json!([{"uri": library_uri, "range": range(3, 21, 26)}])
    );

    let mut references = position(3, 26);
    references["context"] = json!({"includeDeclaration": true});
    assert_eq!(
        client.request(6, REFERENCES_METHOD, references),
        json!([
            {"uri": uri, "range": range(3, 24, 29)},
            {"uri": uri, "range": range(4, 48, 53)},
            {"uri": uri, "range": range(9, 19, 24)},
        ])
    );

    assert_eq!(
        client.request(7, PREPARE_RENAME_METHOD, position(5, 33)),
        json!({"range": range(5, 31, 37), "placeholder": "helper"})
    );
    let mut rename = position(5, 33);
    rename["newName"] = json!("render");
    let edit = client.request(8, RENAME_METHOD, rename);
    assert_eq!(
        edit["changes"][&uri],
        json!([{"range": range(5, 31, 37), "newText": "render"}])
    );
    assert_eq!(
        edit["changes"][&library_uri],
        json!([{"range": range(2, 22, 28), "newText": "render"}])
    );
    let mut invalid = position(5, 33);
    invalid["newName"] = json!("not valid");
    client.send(
        Request {
            id: RequestId::from(9),
            method: RENAME_METHOD.to_owned(),
            params: invalid,
        }
        .into(),
    );
    match client.next() {
        Message::Response(response) => assert!(response.error.is_some()),
        message => panic!("unexpected message {message:?}"),
    }

    let hover = client.request(10, HOVER_METHOD, position(4, 8));
    assert!(
        hover["contents"]["value"]
            .as_str()
            .is_some_and(|value| value.contains("**xsl:for-each**")),
        "{hover}"
    );
    assert_eq!(hover["range"], range(4, 5, 17));
    let hover = client.request(11, HOVER_METHOD, position(6, 48));
    assert!(
        hover["contents"]["value"]
            .as_str()
            .is_some_and(|value| value.contains("Global variable") && value.contains("lib.xsl")),
        "{hover}"
    );

    let completion = client.request(12, COMPLETION_METHOD, position(9, 19));
    let labels = completion["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter_map(|item| item["label"].as_str())
        .collect::<Vec<_>>();
    assert!(
        labels.contains(&"count") && labels.contains(&"prefix"),
        "{labels:?}"
    );

    assert_eq!(client.request(13, "shutdown", json!(null)), Value::Null);
    client.notify(EXIT_METHOD, json!(null));
    server_thread.join().expect("server thread should stop");
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn renames_xsd_components_across_open_instance_documents() {
    let schema_path =
        std::env::temp_dir().join(format!("xml-lsp-rename-{}.xsd", std::process::id()));
    let schema_source = concat!(
        "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">",
        "<xs:element name=\"root\"><xs:complexType><xs:sequence>",
        "<xs:element ref=\"item\"/></xs:sequence></xs:complexType></xs:element>",
        "<xs:element name=\"item\" type=\"xs:string\"/>",
        "</xs:schema>"
    );
    std::fs::write(&schema_path, schema_source).expect("schema should be written");
    let schema_uri = path_to_uri(&schema_path);
    let schema_name = schema_path.file_name().unwrap().to_string_lossy();
    let bound_uri = path_to_uri(&schema_path.with_file_name("rename-bound.xml"));
    let bound = format!(
        "<root xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"{schema_name}\"><item>a</item><item/></root>"
    );
    let unrelated_uri = path_to_uri(&schema_path.with_file_name("rename-unrelated.xml"));
    let mut server = XmlLanguageServer::new();
    server
        .documents
        .insert(schema_uri.clone(), schema_source.to_owned());
    server.documents.insert(bound_uri.clone(), bound.clone());
    server
        .documents
        .insert(unrelated_uri, "<root><item/></root>".to_owned());

    let offset = schema_source.find("\"item\" type").unwrap() + 1;
    let character = position_at(schema_source, offset)["character"].clone();
    let edit = server
        .rename(&json!({
            "textDocument": {"uri": schema_uri},
            "position": {"line": 0, "character": character},
            "newName": "entry",
        }))
        .expect("rename should succeed")
        .expect("an edit should be returned");
    let changes = edit["changes"].as_object().unwrap();
    assert_eq!(changes.len(), 2, "{edit}");
    assert_eq!(changes[&schema_uri].as_array().unwrap().len(), 2);
    let bound_edits = changes[&bound_uri].as_array().unwrap();
    assert_eq!(bound_edits.len(), 3);
    let item = bound.find("<item>").unwrap() + 1;
    assert_eq!(bound_edits[0]["range"]["start"], position_at(&bound, item));
    assert_eq!(bound_edits[0]["newText"], "entry");

    std::fs::remove_file(schema_path).expect("schema should be removed");
}

#[test]
fn reports_representation_problems_in_schema_documents() {
    let mut server = XmlLanguageServer::new();
    let source = "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\n  <xs:element name=\"a\" foo=\"1\"/>\n  <xs:element/>\n</xs:schema>";
    let published = server.diagnostics("file:///tmp/a.xsd", source);
    let diagnostics = published["diagnostics"].as_array().unwrap();
    assert_eq!(
        diagnostics
            .iter()
            .map(|diagnostic| (
                diagnostic["code"].as_str().unwrap(),
                diagnostic["data"]["kind"].as_str().unwrap(),
                diagnostic["range"]["start"]["line"].as_u64().unwrap()
            ))
            .collect::<Vec<_>>(),
        [
            ("xsd-schema", "invalidSchemaAttribute", 1),
            ("xsd-schema", "missingSchemaAttribute", 2),
        ]
    );
    assert_eq!(diagnostics[0]["range"]["start"]["character"], 23);
    // An ordinary document is not a schema.
    let other = server.diagnostics("file:///tmp/a.xml", "<root><element/></root>");
    assert!(other["diagnostics"].as_array().unwrap().is_empty());
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
                        "positionEncoding": "utf-16",
                        "textDocumentSync": {"openClose": true, "change": 2},
                        "completionProvider": {"triggerCharacters": ["<", " ", "/", ">", "=", "\"", "?", "&", "%", "$"]},
                        "documentFormattingProvider": true,
                        "documentRangeFormattingProvider": true,
                        "documentSymbolProvider": true,
                        "hoverProvider": true,
                        "definitionProvider": true,
                        "referencesProvider": true,
                        "documentHighlightProvider": true,
                        "linkedEditingRangeProvider": true,
                        "renameProvider": {"prepareProvider": true},
                        "foldingRangeProvider": true,
                        "selectionRangeProvider": true,
                        "documentLinkProvider": {"resolveProvider": false},
                        "codeActionProvider": {"codeActionKinds": ["quickfix", "refactor", "source"]},
                        "workspaceSymbolProvider": true,
                        "colorProvider": true,
                        "workspace": {"workspaceFolders": {"supported": true, "changeNotifications": true}},
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
            // Unclosed element: reported on the name of its start tag.
            assert_eq!(
                notification.params["diagnostics"][0]["range"],
                json!({
                    "start": {"line": 0, "character": 1},
                    "end": {"line": 0, "character": 5},
                })
            );
            assert_eq!(
                notification.params["diagnostics"][0]["data"],
                json!({"category": "xml", "kind": "unclosedElement"})
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
                        "start": {"line": 0, "character": 8},
                        "end": {"line": 0, "character": 8},
                    },
                    "newText": "\n",
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

    // After `shutdown`, requests are rejected and notifications ignored.
    client
        .sender
        .send(
            Request {
                id: RequestId::from(6),
                method: HOVER_METHOD.to_owned(),
                params: json!({
                    "textDocument": {"uri": "file:///document.xml"},
                    "position": {"line": 0, "character": 1},
                }),
            }
            .into(),
        )
        .expect("request after shutdown should be sent");
    match client.receiver.recv().expect("an error should be received") {
        Message::Response(response) => {
            assert_eq!(response.id, RequestId::from(6));
            assert_eq!(
                response.error.map(|error| error.code),
                Some(ErrorCode::InvalidRequest as i32)
            );
        }
        message => panic!("expected an error response, got {message:?}"),
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

    let exit_code = server_thread.join().expect("server thread should stop");
    assert_eq!(exit_code, 0, "exit after shutdown is a clean exit");
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
fn serves_xsd_documentation_hover_requests() {
    let directory = std::env::temp_dir().join(format!("xml-lsp-hover-lsp-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("directory should be written");
    let schema_path = directory.join("order.xsd");
    let schema_source = concat!(
        "<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\" xmlns:o=\"urn:order\" targetNamespace=\"urn:order\" elementFormDefault=\"qualified\">\n",
        "  <xs:element name=\"order\"><xs:annotation><xs:documentation>A customer order.</xs:documentation></xs:annotation>\n",
        "    <xs:complexType><xs:sequence>\n",
        "      <xs:element name=\"status\" type=\"o:Status\"><xs:annotation><xs:documentation>Order status.</xs:documentation></xs:annotation></xs:element>\n",
        "    </xs:sequence>\n",
        "    <xs:attribute name=\"priority\" type=\"xs:int\" use=\"required\"><xs:annotation><xs:documentation>Priority level.</xs:documentation></xs:annotation></xs:attribute>\n",
        "    </xs:complexType>\n",
        "  </xs:element>\n",
        "  <xs:simpleType name=\"Status\"><xs:restriction base=\"xs:string\">\n",
        "    <xs:enumeration value=\"open\"><xs:annotation><xs:documentation>Not shipped yet.</xs:documentation></xs:annotation></xs:enumeration>\n",
        "    <xs:enumeration value=\"closed\"/>\n",
        "  </xs:restriction></xs:simpleType>\n",
        "</xs:schema>",
    );
    std::fs::write(&schema_path, schema_source).expect("schema should be written");
    let document_uri = path_to_uri(&directory.join("order.xml"));
    let schema_uri = path_to_uri(&schema_path);

    let (server, client) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let request = |id: i32, method: &str, params: Value| {
        client
            .sender
            .send(
                Request {
                    id: RequestId::from(id),
                    method: method.to_owned(),
                    params,
                }
                .into(),
            )
            .expect("request should be sent");
        loop {
            match client.receiver.recv().expect("a message should arrive") {
                Message::Response(response) => {
                    assert_eq!(response.id, RequestId::from(id));
                    assert!(response.error.is_none(), "{:?}", response.error);
                    return response.result;
                }
                Message::Notification(_) => {}
                message => panic!("unexpected message {message:?}"),
            }
        }
    };
    let notify = |method: &str, params: Value| {
        client
            .sender
            .send(
                Notification {
                    method: method.to_owned(),
                    params,
                }
                .into(),
            )
            .expect("notification should be sent");
    };

    let initialize = request(1, INITIALIZE_METHOD, json!({}));
    assert_eq!(initialize.unwrap()["capabilities"]["hoverProvider"], true);
    notify("initialized", json!({}));
    let text = "<order xmlns=\"urn:order\" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\r\n       xsi:schemaLocation=\"urn:order order.xsd\" priority=\"1\">\r\n  <status>open</status>\r\n</order>";
    notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": document_uri, "text": text}}),
    );
    notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": "file:///plain.xml", "text": "<note/>"}}),
    );
    let params = |uri: &str, line: u32, character: u32| {
        json!({
            "textDocument": {"uri": uri},
            "position": {"line": line, "character": character},
        })
    };

    assert_eq!(
        request(2, HOVER_METHOD, params(&document_uri, 3, 4)),
        Some(json!({
            "contents": {
                "kind": "markdown",
                "value": format!("**Element** `<order>`\n\n- Namespace: `urn:order`\n- Type: anonymous complex\n\nA customer order.\n\nSource: [order.xsd]({schema_uri})"),
            },
            "range": {"start": {"line": 3, "character": 2}, "end": {"line": 3, "character": 7}},
        }))
    );
    let status = request(3, HOVER_METHOD, params(&document_uri, 2, 4)).unwrap();
    assert_eq!(
        status["range"],
        json!({"start": {"line": 2, "character": 3}, "end": {"line": 2, "character": 9}})
    );
    let status = status["contents"]["value"].as_str().unwrap();
    assert!(status.contains("- Type: `o:Status` (restriction of `xs:string`)"));
    assert!(status.contains("Order status."), "{status}");
    let priority = request(4, HOVER_METHOD, params(&document_uri, 1, 50)).unwrap();
    let priority = priority["contents"]["value"].as_str().unwrap();
    assert!(
        priority.starts_with("**Attribute** `priority`"),
        "{priority}"
    );
    assert!(priority.contains("- Use: required"), "{priority}");
    assert!(priority.contains("Priority level."), "{priority}");
    let value = request(5, HOVER_METHOD, params(&document_uri, 2, 11)).unwrap();
    assert_eq!(
        value["range"],
        json!({"start": {"line": 2, "character": 10}, "end": {"line": 2, "character": 14}})
    );
    let value = value["contents"]["value"].as_str().unwrap();
    assert!(value.contains("Not shipped yet."), "{value}");
    assert!(value.contains("- Allowed values: `open`, `closed`"));
    assert_eq!(
        request(6, HOVER_METHOD, params("file:///plain.xml", 0, 2)),
        Some(json!({
            "contents": {"kind": "markdown", "value": "**Element** `<note>`"},
            "range": {"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 5}},
        }))
    );
    assert_eq!(
        request(7, HOVER_METHOD, params("file:///plain.xml", 0, 0)),
        Some(Value::Null)
    );

    assert_eq!(request(8, "shutdown", json!(null)), Some(Value::Null));
    notify(EXIT_METHOD, json!(null));
    server_thread.join().expect("server thread should stop");
    std::fs::remove_dir_all(directory).expect("directory should be removed");
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
        formatting::apply_edits(source, &response),
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
fn converts_positions_in_every_encoding_outside_crlf_line_breaks() {
    let source = "é\r\n😀<r>\r\n";
    let index = selection::LineIndex::new(source);
    // Offset 3 is between `\r` and `\n`: the end of the first line.
    assert_eq!(position_at(source, 3), json!({"line": 0, "character": 1}));
    assert_eq!(index.position(source, 3), position_at(source, 3));
    // A character beyond the line is its end, before the line break.
    assert_eq!(offset_at(source, 0, 40), 2);
    assert_eq!(offset_at(source, 1, 40), 11);
    assert_eq!(offset_at(source, 9, 0), source.len());
    for (encoding, character) in [
        (positions::PositionEncoding::Utf8, 5),
        (positions::PositionEncoding::Utf16, 3),
        (positions::PositionEncoding::Utf32, 2),
    ] {
        // Each thread has its own encoding: this one is left unchanged
        // for the other tests.
        thread::spawn(move || {
            encoding.install();
            let offset = source.find("r>").unwrap();
            let expected = json!({"line": 1, "character": character});
            assert_eq!(position_at(source, offset), expected, "{encoding:?}");
            assert_eq!(
                selection::LineIndex::new(source).position(source, offset),
                expected
            );
            assert_eq!(offset_at(source, 1, character), offset, "{encoding:?}");
        })
        .join()
        .expect("conversions should succeed");
    }
}

#[test]
fn locates_xsd_parse_errors() {
    let source = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="root"></xs:schema>"#;
    let offset = schemas::xsd_parse_error_offset(source);
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
            .schemas
            .index
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

/// Starts a server and sends `initialize` (with `params`) and
/// `initialized`; returns the client, the `initialize` result and the
/// server thread (its exit code).
fn start_server(params: Value) -> (TestClient, Value, thread::JoinHandle<i32>) {
    let (server, connection) = Connection::memory();
    let server_thread = thread::spawn(|| run(server).expect("server loop should succeed"));
    let client = TestClient {
        connection,
        diagnostics: Default::default(),
    };
    let initialize = client.request(0, INITIALIZE_METHOD, params);
    client.notify("initialized", json!({}));
    (client, initialize, server_thread)
}

/// Response (result or error) to the request `id`.
fn response(client: &TestClient, id: i32) -> Response {
    match client.next() {
        Message::Response(response) => {
            assert_eq!(response.id, RequestId::from(id));
            response
        }
        message => panic!("unexpected message {message:?}"),
    }
}

fn send_request(client: &TestClient, id: i32, method: &str, params: Value) {
    client.send(
        Request {
            id: RequestId::from(id),
            method: method.to_owned(),
            params,
        }
        .into(),
    );
}

fn error_code(response: &Response) -> Option<i32> {
    response.error.as_ref().map(|error| error.code)
}

#[test]
fn answers_requests_cancelled_while_queued() {
    let (client, _, server_thread) = start_server(json!({}));
    let uri = "file:///cancel.xml";
    client.notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": uri, "version": 1, "text": "<root/>"}}),
    );
    let hover = json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 2}});
    // The loop is busy while the hover and its cancellation are queued.
    send_request(&client, 1, SLEEP_METHOD, json!({"milliseconds": 300}));
    send_request(&client, 2, HOVER_METHOD, hover.clone());
    send_request(&client, 3, HOVER_METHOD, hover.clone());
    client.notify(dispatch::CANCEL_REQUEST_METHOD, json!({"id": 2}));
    // Unknown requests: ignored.
    client.notify(dispatch::CANCEL_REQUEST_METHOD, json!({"id": 99}));
    assert_eq!(error_code(&response(&client, 1)), None);
    assert_eq!(
        error_code(&response(&client, 2)),
        Some(ErrorCode::RequestCanceled as i32)
    );
    // Already answered: ignored.
    client.notify(dispatch::CANCEL_REQUEST_METHOD, json!({"id": 1}));
    assert_eq!(error_code(&response(&client, 3)), None);
    // String ids are cancelled as well.
    send_request(&client, 4, SLEEP_METHOD, json!({"milliseconds": 300}));
    client.send(
        Request {
            id: RequestId::from("five".to_owned()),
            method: HOVER_METHOD.to_owned(),
            params: hover,
        }
        .into(),
    );
    client.notify(dispatch::CANCEL_REQUEST_METHOD, json!({"id": "five"}));
    assert_eq!(error_code(&response(&client, 4)), None);
    match client.next() {
        Message::Response(response) => {
            assert_eq!(response.id, RequestId::from("five".to_owned()));
            assert_eq!(
                error_code(&response),
                Some(ErrorCode::RequestCanceled as i32)
            );
        }
        message => panic!("unexpected message {message:?}"),
    }
    assert_eq!(client.request(6, "shutdown", Value::Null), Value::Null);
    client.notify(EXIT_METHOD, Value::Null);
    assert_eq!(server_thread.join().expect("server should stop"), 0);
}

#[test]
fn a_panicking_handler_answers_an_internal_error_and_the_server_keeps_running() {
    let (client, _, server_thread) = start_server(json!({}));
    let uri = "file:///panic.xml";
    client.notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": uri, "version": 1, "text": "<root><a/>"}}),
    );
    send_request(&client, 1, PANIC_METHOD, Value::Null);
    let failed = response(&client, 1);
    assert_eq!(error_code(&failed), Some(ErrorCode::InternalError as i32));
    assert!(
        failed
            .error
            .as_ref()
            .is_some_and(|error| error.message.contains("test panic")),
        "{failed:?}"
    );
    // The server still answers and validates.
    let symbols = client.request(2, SYMBOL_METHOD, json!({"textDocument": {"uri": uri}}));
    assert_eq!(symbols.as_array().map(Vec::len), Some(1));
    let published = client.take_diagnostics(3);
    assert_eq!(
        codes(published.last().expect("diagnostics")),
        ["xml-structure"]
    );
    // Unknown requests are still `MethodNotFound`.
    send_request(&client, 4, "xml/unknown", Value::Null);
    assert_eq!(
        error_code(&response(&client, 4)),
        Some(ErrorCode::MethodNotFound as i32)
    );
    assert_eq!(client.request(5, "shutdown", Value::Null), Value::Null);
    client.notify(EXIT_METHOD, Value::Null);
    assert_eq!(server_thread.join().expect("server should stop"), 0);
}

#[test]
fn exit_without_shutdown_ends_with_code_1() {
    let (client, _, server_thread) = start_server(json!({}));
    client.notify(EXIT_METHOD, Value::Null);
    assert_eq!(server_thread.join().expect("server should stop"), 1);

    // A client that goes away without `exit`.
    let (client, _, server_thread) = start_server(json!({}));
    drop(client);
    assert_eq!(server_thread.join().expect("server should stop"), 1);
}

#[test]
fn negotiates_utf8_positions() {
    let (client, initialize, server_thread) = start_server(json!({
        "capabilities": {"general": {"positionEncodings": ["utf-16", "utf-8"]}},
    }));
    assert_eq!(initialize["capabilities"]["positionEncoding"], "utf-8");
    let uri = "file:///utf8.xml";
    // `é` is 2 bytes, `𝄞` 4 bytes: UTF-8 columns differ from UTF-16.
    let text = "<é𝄞 a=\"1\">\r\n  <b></c>\r\n</é𝄞>";
    client.notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": uri, "version": 1, "text": text}}),
    );
    let published = client.take_diagnostics(1);
    let diagnostic = &published.last().expect("diagnostics")["diagnostics"][0];
    assert_eq!(
        diagnostic["range"]["start"],
        json!({"line": 1, "character": 7})
    );
    let highlights = client.request(
        2,
        DOCUMENT_HIGHLIGHT_METHOD,
        json!({"textDocument": {"uri": uri}, "position": {"line": 2, "character": 3}}),
    );
    assert_eq!(
        highlights,
        json!([
            {"range": {"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 7}}, "kind": 2},
            {"range": {"start": {"line": 2, "character": 2}, "end": {"line": 2, "character": 8}}, "kind": 2},
        ])
    );
    // Incremental change in UTF-8 columns: `</c>` becomes `</b>`.
    client.notify(
        DID_CHANGE_METHOD,
        json!({
            "textDocument": {"uri": uri, "version": 2},
            "contentChanges": [{
                "range": {"start": {"line": 1, "character": 7}, "end": {"line": 1, "character": 8}},
                "text": "b",
            }],
        }),
    );
    let published = client.take_diagnostics(3);
    let last = published.last().expect("diagnostics");
    assert_eq!(last["diagnostics"], json!([]));
    assert_eq!(last["version"], 2);
    assert_eq!(client.request(4, "shutdown", Value::Null), Value::Null);
    client.notify(EXIT_METHOD, Value::Null);
    assert_eq!(server_thread.join().expect("server should stop"), 0);
}

#[test]
fn serves_documents_with_a_byte_order_mark_and_utf16_schemas() {
    let directory = std::env::temp_dir().join(format!("xml-lsp-bom-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("directory should be created");
    // UTF-16 LE schema with a byte order mark, as Windows tools write it.
    let schema = "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\n<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">\r\n  <xs:element name=\"root\"><xs:complexType><xs:sequence>\r\n    <xs:element name=\"item\" minOccurs=\"0\"/>\r\n  </xs:sequence></xs:complexType></xs:element>\r\n</xs:schema>\r\n";
    let bytes = [0xFF, 0xFE]
        .into_iter()
        .chain(schema.encode_utf16().flat_map(u16::to_le_bytes))
        .collect::<Vec<u8>>();
    std::fs::write(directory.join("schema.xsd"), bytes).expect("schema should be written");
    let uri = path_to_uri(&directory.join("document.xml"));
    let text = "\u{FEFF}<root xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"schema.xsd\"><wrong/></root>";

    let (client, _, server_thread) = start_server(json!({}));
    client.notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": uri, "version": 1, "text": text}}),
    );
    let published = client.take_diagnostics(1);
    let publication = published.last().expect("diagnostics");
    assert_eq!(codes(publication), ["xsd-validation"], "{publication}");
    assert!(
        publication["diagnostics"][0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("wrong")),
        "{publication}"
    );
    // The mark is one UTF-16 code unit before `<root`.
    let symbols = client.request(2, SYMBOL_METHOD, json!({"textDocument": {"uri": uri}}));
    let root = symbols
        .as_array()
        .and_then(|symbols| symbols.iter().find(|symbol| symbol["name"] == "root"))
        .expect("root symbol");
    assert_eq!(root["range"]["start"], json!({"line": 0, "character": 1}));
    // Formatting keeps the mark.
    let edits = client.request(
        3,
        FORMATTING_METHOD,
        json!({"textDocument": {"uri": uri}, "options": {"tabSize": 2, "insertSpaces": true}}),
    );
    let formatted = formatting::apply_edits(text, &edits);
    assert!(formatted.starts_with("\u{FEFF}<root"), "{formatted:?}");
    assert!(formatted.contains("\n  <wrong/>\n"), "{formatted:?}");
    // Completion reads the UTF-16 schema.
    client.notify(
        DID_CHANGE_METHOD,
        json!({
            "textDocument": {"uri": uri, "version": 2},
            "contentChanges": [{"text": text.replace("<wrong/>", "<")}],
        }),
    );
    let character = text.find("<wrong/>").expect("element") + 1 - 2;
    let completion = client.request(
        4,
        COMPLETION_METHOD,
        json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": character}}),
    );
    assert!(
        completion["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["label"] == "item")),
        "{completion}"
    );
    assert_eq!(client.request(5, "shutdown", Value::Null), Value::Null);
    client.notify(EXIT_METHOD, Value::Null);
    assert_eq!(server_thread.join().expect("server should stop"), 0);
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn publishes_the_diagnostics_of_the_latest_version_only_last() {
    let (client, _, server_thread) = start_server(json!({}));
    let uri = "file:///typing.xml";
    client.notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": uri, "version": 1, "text": "<root>"}}),
    );
    let mut text = "<root>".to_owned();
    for version in 2..=40 {
        text.push_str("<a/>");
        if version == 40 {
            text.push_str("</root>");
        }
        client.notify(
            DID_CHANGE_METHOD,
            json!({
                "textDocument": {"uri": uri, "version": version},
                "contentChanges": [{"text": text}],
            }),
        );
    }
    let published = client.take_diagnostics(1);
    let versions = published
        .iter()
        .map(|publication| publication["version"].as_i64().expect("version"))
        .collect::<Vec<_>>();
    assert!(
        versions.windows(2).all(|pair| pair[0] < pair[1]),
        "{versions:?}"
    );
    let last = published.last().expect("diagnostics");
    assert_eq!(last["version"], 40);
    assert_eq!(last["diagnostics"], json!([]));
    // Closing clears the diagnostics.
    client.notify(DID_CLOSE_METHOD, json!({"textDocument": {"uri": uri}}));
    let published = client.take_diagnostics(2);
    assert_eq!(published, [json!({"uri": uri, "diagnostics": []})]);
    assert_eq!(client.request(3, "shutdown", Value::Null), Value::Null);
    client.notify(EXIT_METHOD, Value::Null);
    assert_eq!(server_thread.join().expect("server should stop"), 0);
}

#[test]
fn debounces_the_validation_of_changes() {
    let (client, _, server_thread) =
        start_server(json!({"initializationOptions": {"xml": {"validation": {"debounce": 300}}}}));
    let uri = "file:///debounce.xml";
    // Opening validates at once.
    let opened = std::time::Instant::now();
    client.notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": uri, "version": 1, "text": "<a>"}}),
    );
    let receive_publication = || match client
        .connection
        .receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("diagnostics should be published")
    {
        Message::Notification(notification)
            if notification.method == PUBLISH_DIAGNOSTICS_METHOD =>
        {
            notification.params
        }
        message => panic!("unexpected message {message:?}"),
    };
    assert_eq!(receive_publication()["version"], 1);
    assert!(opened.elapsed() < std::time::Duration::from_millis(250));
    // A burst of changes: one validation, of the last version, once the
    // typing pauses.
    let mut changed = std::time::Instant::now();
    for version in 2..=6 {
        changed = std::time::Instant::now();
        client.notify(
            DID_CHANGE_METHOD,
            json!({
                "textDocument": {"uri": uri, "version": version},
                "contentChanges": [{"text": "<a>".repeat(version as usize)}],
            }),
        );
    }
    let publication = receive_publication();
    assert!(changed.elapsed() >= std::time::Duration::from_millis(300));
    assert_eq!(publication["version"], 6);
    assert!(
        client.take_diagnostics(1).is_empty(),
        "a single publication"
    );
    // Requests are answered during the debounce delay.
    client.notify(
        DID_CHANGE_METHOD,
        json!({
            "textDocument": {"uri": uri, "version": 7},
            "contentChanges": [{"text": "<a/>"}],
        }),
    );
    let started = std::time::Instant::now();
    client.request(
        2,
        DOCUMENT_HIGHLIGHT_METHOD,
        json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 1}}),
    );
    assert!(started.elapsed() < std::time::Duration::from_millis(250));
    let published = client.take_diagnostics(3);
    assert_eq!(published.last().expect("diagnostics")["version"], 7);
    assert_eq!(client.request(4, "shutdown", Value::Null), Value::Null);
    client.notify(EXIT_METHOD, Value::Null);
    assert_eq!(server_thread.join().expect("server should stop"), 0);
}

#[test]
fn limits_documents_larger_than_max_file_size_to_well_formedness() {
    let directory = std::env::temp_dir().join(format!("xml-lsp-large-file-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("directory should be created");
    std::fs::write(
            directory.join("a.xsd"),
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:element name="a"/></xs:schema>"#,
        )
        .expect("schema should be written");
    let uri = path_to_uri(&directory.join("large.xml"));
    let text = format!(
        "<a xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"a.xsd\">\n{}<b></c>\n</a>\n",
        "  <!-- padding -->\n".repeat(20)
    );
    let (client, _, server_thread) = start_server(json!({
        "initializationOptions": {"xml": {"maxFileSize": 200}},
        "capabilities": {"textDocument": {"documentSymbol": {"hierarchicalDocumentSymbolSupport": true}}},
    }));
    client.notify(
        DID_OPEN_METHOD,
        json!({"textDocument": {"uri": uri, "version": 1, "text": text}}),
    );
    let published = client.take_diagnostics(1);
    let publication = published.last().expect("diagnostics");
    // The mismatched end tag is still reported; `<b>` (not declared by
    // the schema) is not validated.
    assert_eq!(
        codes(publication),
        ["xml-structure", "large-file"],
        "{publication}"
    );
    let notice = &publication["diagnostics"][1];
    assert_eq!(notice["severity"], 3);
    assert_eq!(notice["data"]["kind"], "largeFile");
    assert!(
        notice["message"]
            .as_str()
            .is_some_and(|message| message.contains("xml.maxFileSize")),
        "{notice}"
    );
    let document = json!({"textDocument": {"uri": uri}});
    assert_eq!(
        client.request(2, FOLDING_RANGE_METHOD, document.clone()),
        json!([])
    );
    assert_eq!(
        client.request(3, SYMBOL_METHOD, document.clone()),
        json!([])
    );
    // Features proportional to the request still work.
    let highlights = client.request(
        4,
        DOCUMENT_HIGHLIGHT_METHOD,
        json!({"textDocument": {"uri": uri}, "position": {"line": 0, "character": 1}}),
    );
    assert_eq!(highlights.as_array().map(Vec::len), Some(2));
    // Raising the limit restores validation and the features.
    client.notify(
        DID_CHANGE_CONFIGURATION_METHOD,
        json!({"settings": {"xml": {"maxFileSize": 0}}}),
    );
    let published = client.take_diagnostics(5);
    let publication = published.last().expect("diagnostics");
    assert!(
        codes(publication).contains(&"xsd-validation".to_owned()),
        "{publication}"
    );
    assert!(!codes(publication).contains(&"large-file".to_owned()));
    assert_ne!(client.request(6, SYMBOL_METHOD, document), json!([]));
    assert_eq!(client.request(7, "shutdown", Value::Null), Value::Null);
    client.notify(EXIT_METHOD, Value::Null);
    assert_eq!(server_thread.join().expect("server should stop"), 0);
    let _ = std::fs::remove_dir_all(&directory);
}
