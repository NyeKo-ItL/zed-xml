//! End-to-end benchmarks of the `xml-lsp` binary over stdio: request
//! latencies on a large document bound to a schema, as an editor sees them
//! (JSON-RPC framing and serialization included).
//!
//! `cargo bench -p xml-lsp` (add `-- --quick` for a fast run).

use std::{
    hint::black_box,
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};

use criterion::{Criterion, criterion_group, criterion_main};
use serde_json::{Value, json};

const SCHEMA: &str = r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="catalog"><xs:complexType><xs:sequence>
    <xs:element ref="book" minOccurs="0" maxOccurs="unbounded"/>
  </xs:sequence></xs:complexType></xs:element>
  <xs:element name="book"><xs:complexType><xs:sequence>
    <xs:element name="title" type="xs:string"/>
    <xs:element name="author" type="xs:string" maxOccurs="unbounded"/>
    <xs:element name="price" type="xs:decimal"/>
    <xs:element name="format" type="Format"/>
  </xs:sequence>
  <xs:attribute name="id" type="xs:ID" use="required"/>
  </xs:complexType></xs:element>
  <xs:simpleType name="Format"><xs:restriction base="xs:string">
    <xs:enumeration value="paperback"/><xs:enumeration value="ebook"/>
  </xs:restriction></xs:simpleType>
</xs:schema>
"#;

/// Instance of about `size` bytes bound to `catalog.xsd`.
fn instance(size: usize) -> String {
    let mut text = String::from(
        "<catalog xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:noNamespaceSchemaLocation=\"catalog.xsd\">\n",
    );
    let mut index = 0;
    while text.len() < size {
        text.push_str(&format!(
            "  <book id=\"b{index}\"><title>Title {index}</title><author>A</author><price>{index}.5</price><format>ebook</format></book>\n"
        ));
        index += 1;
    }
    text.push_str("</catalog>\n");
    text
}

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
    version: i64,
}

impl Server {
    fn start(directory: &std::path::Path, initialization_options: Value) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_xml-lsp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("xml-lsp should start");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut server = Self {
            child,
            stdin,
            stdout,
            next_id: 0,
            version: 0,
        };
        server.request(
            "initialize",
            json!({
                "rootUri": uri(directory),
                "capabilities": {},
                "initializationOptions": initialization_options,
            }),
        );
        server.notify("initialized", json!({}));
        server
    }

    fn send(&mut self, message: Value) {
        let body = message.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).expect("write");
        self.stdin.flush().expect("flush");
    }

    fn receive(&mut self) -> Value {
        let mut length = 0;
        loop {
            let mut line = String::new();
            self.stdout.read_line(&mut line).expect("header");
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length: ") {
                length = value.parse().expect("length");
            }
        }
        let mut body = vec![0; length];
        self.stdout.read_exact(&mut body).expect("body");
        serde_json::from_slice(&body).expect("json")
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Sends a request and waits for its response (other messages skipped).
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let message = self.receive();
            if message.get("id") == Some(&json!(id)) && message.get("method").is_none() {
                return message["result"].clone();
            }
        }
    }

    /// Replaces the document and waits for the diagnostics of the new version.
    fn change_and_wait_for_diagnostics(&mut self, document: &str, text: &str) -> Value {
        self.version += 1;
        let version = self.version;
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": document, "version": version},
                "contentChanges": [{"text": text}],
            }),
        );
        self.wait_for_diagnostics(document, version)
    }

    fn wait_for_diagnostics(&mut self, document: &str, version: i64) -> Value {
        loop {
            let message = self.receive();
            if message["method"] == "textDocument/publishDiagnostics"
                && message["params"]["uri"] == document
                && message["params"]["version"]
                    .as_i64()
                    .is_none_or(|v| v >= version)
            {
                return message["params"].clone();
            }
        }
    }

    fn open(&mut self, document: &str, text: &str) {
        self.version += 1;
        let version = self.version;
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": document, "languageId": "xml", "version": version, "text": text}}),
        );
        self.wait_for_diagnostics(document, version);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        let _ = self.child.wait();
    }
}

fn uri(path: &std::path::Path) -> String {
    format!("file://{}", path.display())
}

fn workspace() -> PathBuf {
    let directory = std::env::temp_dir().join(format!("xml-lsp-bench-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("directory");
    std::fs::write(directory.join("catalog.xsd"), SCHEMA).expect("schema");
    directory
}

fn benches(c: &mut Criterion) {
    let directory = workspace();
    let document = uri(&directory.join("catalog.xml"));
    let text = instance(1_000_000);
    let last_line = text.lines().count() - 2;
    let position = json!({"line": last_line, "character": 4});
    let at = json!({"textDocument": {"uri": document}, "position": position});

    let mut group = c.benchmark_group("xml-lsp-1MB");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(5));

    // Validation without debounce: time to diagnostics after a change.
    let mut server = Server::start(&directory, json!({"xml": {"validation": {"debounce": 0}}}));
    server.open(&document, &text);
    group.bench_function("change_to_diagnostics", |b| {
        b.iter(|| black_box(server.change_and_wait_for_diagnostics(&document, &text)))
    });
    // Requests sent on cursor moves and typing, the document unchanged.
    for (name, method, params) in [
        (
            "document_highlight",
            "textDocument/documentHighlight",
            at.clone(),
        ),
        ("hover", "textDocument/hover", at.clone()),
        ("completion", "textDocument/completion", at.clone()),
        (
            "code_action",
            "textDocument/codeAction",
            json!({
                "textDocument": {"uri": document},
                "range": {"start": position, "end": position},
                "context": {"diagnostics": []},
            }),
        ),
        (
            "folding_range",
            "textDocument/foldingRange",
            json!({"textDocument": {"uri": document}}),
        ),
        (
            "document_symbol",
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": document}}),
        ),
    ] {
        group.bench_function(name, |b| {
            b.iter(|| black_box(server.request(method, params.clone())))
        });
    }
    // Typing: a change followed at once by a completion request; the
    // completion must not wait for the validation of the change.
    group.bench_function("change_then_completion", |b| {
        b.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                server.version += 1;
                let version = server.version;
                let start = Instant::now();
                server.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": {"uri": document, "version": version},
                        "contentChanges": [{"text": text}],
                    }),
                );
                black_box(server.request("textDocument/completion", at.clone()));
                total += start.elapsed();
                server.wait_for_diagnostics(&document, version);
            }
            total
        })
    });
    drop(server);
    group.finish();
    let _ = std::fs::remove_dir_all(&directory);
}

criterion_group!(server_benches, benches);
criterion_main!(server_benches);
