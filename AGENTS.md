# AGENTS.md

Guidance for coding agents (and humans) working in this repository. It complements, and does not repeat:

- [README.md](README.md): user-facing features, installation, editor settings, troubleshooting.
- [docs/configuration.md](docs/configuration.md): reference of the `xml.*` server settings, XML catalogs and DTD behaviour.
- [CONTRIBUTING.md](CONTRIBUTING.md): development setup, dev extension install, checks, pull requests, release process.
- [CHANGELOG.md](CHANGELOG.md): notable changes per version.

## Project overview

`zed-xml` is a [Zed](https://zed.dev) extension for XML (XML, XSD, XSLT, SVG, WSDL, plist, XJB, Android XML) and DTD files. It has two deliverables built from one Cargo workspace:

1. **The extension** (`extension.toml`, `src/lib.rs`, `languages/`): a `wasm32-wasip2` module that declares the tree-sitter grammars (`xml` and `dtd` from `tree-sitter-grammars/tree-sitter-xml`), the Zed languages and their queries, and starts the language server. It resolves the server from the `lsp.xml-lsp.binary.path` setting, else `XML_LSP_PATH`, else a cached binary whose `--version` matches the extension version and whose SHA-256 matches the recorded checksum, else the matching GitHub release asset, verified against its published `.sha256` (dependency-free SHA-256 in `src/sha256.rs`). Supported platforms are the `SUPPORTED_PLATFORMS` table of `src/lib.rs`, kept in sync with the release matrix of `ci.yml` by a unit test. It forwards `lsp.xml-lsp.settings` as `initializationOptions` and as the answer to `workspace/configuration`.
2. **The language server** `xml-lsp` (`crates/xml-lsp`): a native Rust LSP server over stdio (`lsp-server` + `serde_json`, JSON values rather than `lsp-types`), serving the languages `XML` and `DTD`. Behaviour is modelled on LemMinX (the Red Hat XML language server used by VS Code/Eclipse), with IntelliJ as a second reference.

## Architecture

### Crates

| Crate | Responsibility |
|-------|----------------|
| `zed-xml` (root, `src/lib.rs`, `src/sha256.rs`) | Zed extension: server command, download with checksum and version check, supported platforms, settings forwarding. No XML logic. |
| `crates/xml-core` | XML-only building blocks, no LSP types: `parse_xml` (quick-xml + tolerant well-formedness, `XmlDiagnostic` with codes `xml-syntax`/`xml-structure`), basic completion (`complete_xml`, `auto_close_tag`), formatter, tag scanner, text diff. |
| `crates/xsd-core` | XSD: flat `XsdSchema` (`parse_xsd`, `merge_schemas`) used for validation (`validate_document_located`) and completion (`complete_elements`/`complete_attributes`/`complete_attribute_values`); schema location resolution (`resolve_schema_locations_with`, `resolve_schema_dependencies_with`, `LocationResolver`, `resolve_location`, `file_uri_to_path`, `percent_decode`, `resolve_path`); and the namespace-resolved component model in `model`. |
| `crates/dtd-core` | DTD: `find_doctype`, `parse_dtd`/`load_document_dtd` with an `ExternalLoader`, `Dtd` (declarations, sources, problems), content models as NFAs (`content`), instance validation and entity checks (`validate`), XML name predicates (`names`). Depends on `xml-core`. |
| `crates/xml-lsp` | The server: document store, capabilities, request dispatch, and one module per LSP feature. Depends on the three core crates. |
| `crates/xml-conformance` | Test-only (`publish = false`): helpers (`decode`, `well_formedness_errors`, `check_formatting`, `load_schema_set`, `SuiteRun` baselines) and the suites in `tests/` (spec cases, real-world documents, corpora, W3C xmlconf, W3C xsdtests, libxml2 schemas). Dev-dependency of `xml-lsp` for the fixture smoke test. |

Keep the dependency direction: core crates never depend on `xml-lsp` or on LSP JSON; `xml-lsp` converts their byte ranges to LSP positions.

### Shared helpers (reuse them, do not write new scanners)

- `xml_core::tags`: `scan_tags` (start/end/self-closing tags with name ranges, tolerant), `scan_attributes`, `scan_markup` (comments, CDATA, processing instructions, declarations), `XmlTagTree` (elements with parent/depth, `tag_pair_at`, `innermost_element_at`, `ancestors`, orphan end tags), `qualified_name_parts`, `namespace_declaration`, `resolve_namespace`, `XML_NAMESPACE`.
- `xml_core::wellformed`: `check_well_formedness` returning every problem with a stable `XmlProblemKind::id()`.
- `xml_core` formatter (`format.rs`, re-exported): `FormatOptions`, `format_xml_with`, `format_xml_range`, `LineEnding`, `SplitAttributes`, `EmptyElements`.
- `xml_core::diff::diff_text`: turns a rewritten text into minimal `TextChange`s; use it for any edit that rewrites a document.
- `xsd_core::model`: `parse_xsd_model`, `XsdModelSet` (resolution across schemas: global components, `resolve_element_path`, `resolve_attribute`, `attribute_uses`, `child_elements`, `simple_type_info`, `enumeration`). Prefer it over the flat maps for new features.
- `dtd_core`: `Dtd::allowed_children`, `ContentAutomaton`, `validate::{check_entity_references, validate_instance, entity_reference_at}`, `is_name`/`is_nmtoken`.
- In `xml-lsp`: `selection::LineIndex` (offset to UTF-16 position in O(log n)), `position_at`/`offset_at` in `main.rs`, `rename::is_ncname`/`is_qname`, `links::unescape`/`uri_scheme`/`doctype_external_id`, `code_actions::Actions` and `edit_distance`, `symbols::match_score` and `scan_workspace`, `hover::load_models`/`instance_models`/`Document`, `catalog::Catalogs`.

### `crates/xml-lsp/src` module map

| Module | Role |
|--------|------|
| `main.rs` | `XmlLanguageServer` state (open documents, schema and DTD caches, settings, catalogs, workspace index, client capabilities), `server_capabilities()`, `run()` (initialize, post-init registrations, request dispatch loop), notifications (`didOpen`/`didChange`/`didClose`, configuration, workspace folders, watched files), diagnostics (`diagnostics()`/`publish_diagnostics()` is the single place computing them), XSD completion/definition/references, UTF-16 position helpers, and the LSP round-trip tests. |
| `settings.rs` | `Settings::from_value` (tolerant parsing of `xml.*`), format/validation settings, `fileAssociations` globbing (`glob_match`, `associated_schemas`). |
| `catalog.rs` | OASIS XML Catalogs 1.1: parsing, resolution (`resolve_uri`/`resolve_system`/`resolve_external`/`resolve_schema`), mtime cache, catalog diagnostics. |
| `dtd.rs` | DTD integration: `DtdCache`, `load`, diagnostics (`dtd-grammar`, `xml-entity`, `dtd-validation`), completion, hover, definition, quick fixes, DTD document symbols. |
| `hover.rs` | Hover (XSD documentation, types, facets), `ModelCache` of `XsdModel`s, schema graph loading preferring open buffers. |
| `code_actions.rs` | Quick fixes, refactorings and source actions; enumeration diagnostics. |
| `formatting.rs` | Document/range formatting, LSP `FormattingOptions` to `FormatOptions`. |
| `links.rs` | Document links and definition on schema/include/stylesheet/DOCTYPE references. |
| `symbols.rs` | Workspace symbols (`WorkspaceIndex`), hierarchical document symbols. |
| `rename.rs` | `prepareRename`/`rename` for elements, prefixes and XSD components. |
| `highlight.rs` | `documentHighlight` of matching tag names. |
| `linked_editing.rs` | `linkedEditingRange` of start/end tag names. |
| `folding.rs` | `foldingRange` (LemMinX style), client `rangeLimit`. |
| `selection.rs` | `selectionRange`, `LineIndex`. |
| `colors.rs` | `documentColor`/`colorPresentation` for SVG, CSS and Android. |

### Zed side

- `languages/xml/config.toml` and `languages/dtd/config.toml`: file suffixes, brackets, `word_characters`, `linked_edit_characters` (Zed ignores the LSP word pattern for linked edits), `completion_query_characters`.
- Queries: `highlights`, `brackets`, `indents`, `outline`, `injections`, `overrides`, `textobjects` (`.scm`). The XML grammar requires a root element, which is why `.dtd`/`.ent` files have their own `DTD` language using the `dtd` grammar.
- `grammars/` is generated by Zed from `extension.toml` and is never committed.

## Commands

```sh
cargo fmt --all -- --check              # formatting (CI)
cargo test --workspace                  # all tests (CI)
cargo clippy --workspace --all-targets -- -D warnings  # no warnings (CI)
cargo deny check                        # licences, advisories, bans, sources (CI)
cargo build --target wasm32-wasip2      # when src/lib.rs, Cargo.toml or extension.toml changed
cargo build -p xml-lsp                  # server binary for XML_LSP_PATH
cargo test -p xml-lsp -- <name filter>  # focused tests
```

CI denies every clippy warning (with the toolchain pinned by `CLIPPY_TOOLCHAIN` in `ci.yml`), runs the tests on Linux, Windows and macOS, and builds with the minimum supported Rust version (`rust-version` in every `Cargo.toml`, currently 1.88): do not use newer standard library APIs or language features without raising it everywhere.

## Coding conventions

- Rust edition 2024, `cargo fmt` default style. Keep dependencies minimal (the server only uses `lsp-server`, `serde_json`, `quick-xml`, `regex`); a new dependency must pass `cargo deny check` (`deny.toml`: MIT-compatible licences, crates.io only).
- Portability: tests run on Windows and macOS too. Build paths with `Path::join`, `file://` URIs from real paths (never by concatenating `/tmp/...`), and do not assume LF line endings in files read from disk.
- Write comments, doc comments, diagnostic messages, hover text, code action titles and every other user-facing string in English.
- **New LSP feature = new module** `crates/xml-lsp/src/<feature>.rs` (or in a core crate when it is not LSP-specific), declared in `main.rs`, with its capability added to `server_capabilities()` and its request dispatched in `run()`. Do not grow `main.rs` with feature logic.
- **Offsets**: everything internal is a UTF-8 byte offset/range into the document `&str`; convert to LSP positions (line + UTF-16 code units) only at the LSP boundary (`position_at`, `offset_at`, `selection::LineIndex`). Test with non-ASCII text and CRLF line endings.
- **Tolerance**: documents are usually being edited and malformed. Parsers and features must degrade gracefully and never panic (no `unwrap` on user input, no slicing that can fall inside a UTF-8 character). Return `None`/empty results rather than errors when a request does not apply.
- **Reuse** the helpers listed above instead of adding another tag scanner, namespace resolver, position converter or schema loader. If a private helper is needed elsewhere, make it `pub(crate)` or move it to a core crate.
- Per-request work must stay proportional to the request: Zed asks for code actions, highlights and hovers on cursor moves. Cache parsed schemas/DTDs (mtime-based caches exist) and prefer open buffers over the disk.
- Client capabilities come from `initialize_params` in `run()`. `lsp-server`'s `initialize_finish` consumes the `initialized` notification, so post-initialization requests (`client/registerCapability`, `workspace/configuration`) are sent from `run()` right after `initialize_finish`, never from an `initialized` handler.
- Diagnostics are computed only in `XmlLanguageServer::diagnostics()`. Their `code` values (`xml-syntax`, `xml-structure`, `xsd-validation`, `dtd-grammar`, `dtd-validation`, `xml-entity`, `no-grammar`, `doctype-disallowed`, `catalog-target-missing`, ...) and `data.kind`/`data.rule` identifiers (`XmlProblemKind::id()`, `XsdDiagnosticKind::id()`, `DtdProblemKind::id()`, `InstanceProblemKind::id()`) are a stable API used by code actions and users: add new ones, do not rename existing ones.
- Settings: new options go into `settings.rs` with a default that keeps the current behaviour, and are documented in `docs/configuration.md`. `FormatOptions::default()` must keep producing the historical output.

## Testing

- Unit tests live next to the code in `#[cfg(test)] mod tests`. Cover namespaces/prefixes, self-closing elements, malformed documents, UTF-16 positions and CRLF.
- Every LSP feature also gets a round-trip test in `crates/xml-lsp/src/main.rs`: `Connection::memory()`, `run(server)` on a thread, then `initialize`/`initialized`/`didOpen` and the request under test. The `TestClient` helper (`notify`, `request`, `next`, `take_diagnostics`) collects `publishDiagnostics` notifications; `expect_server_request` acknowledges server-to-client requests (tests that advertise dynamic registration must acknowledge the watcher registrations).
- `serves_initialize_diagnostics_shutdown_and_exit` asserts the exact `initialize` result: update it whenever `server_capabilities()` changes.
- Conformance: `crates/xml-conformance` checks the core crates against the specifications and real documents (see [tests/README.md](tests/README.md)). Known failures live in `crates/xml-conformance/baselines/*.txt`; a fix must delete its lines (`BLESS=1 cargo test --release -p xml-conformance` rewrites them after `scripts/fetch-test-suites.sh`). A new specification rule gets a case in `spec_cases.rs` named after its section, with the result the specification requires.
- `crates/xml-lsp/src/fixture_smoke.rs` runs every request over every file of `tests/fixtures`: no panic, ranges inside the document, idempotent LSP formatting. Known problems are listed in its `KNOWN_PROBLEMS`, with the same delete-on-fix rule. New fixtures go in `tests/fixtures/real-world/` with their source and licence in `tests/fixtures/SOURCES.md` (MIT-compatible licences only); `.gitattributes` keeps fixtures byte-exact.
- Use unique directories under `std::env::temp_dir()` for files on disk and `file://` URIs built from them; tests must not use the network.

## Security

- The server never downloads anything: remote schemas, DTDs and entities (`http(s)://`) are resolved only through XML catalogs to local files, otherwise reported as a warning.
- External general entities are never read; DTD entity expansion is computed, not materialized, and bounded (`dtd_core::MAX_ENTITY_EXPANSION`, `MAX_PARAMETER_EXPANSION`, `MAX_ENTITY_DEPTH`). Keep these limits for any new expansion code ("billion laughs").
- Keep workspace scans bounded (`symbols::scan_workspace` limits files and sizes, skips hidden directories, `target/` and `node_modules/`).
- The extension only reads its own work directory and the environment variables it documents.

## Git and pull requests

- Conventional Commits in English for commits and pull request titles (`feat:`, `fix:`, `docs:`, `test:`, `chore:`, `ci:`); pull request descriptions in English with a summary, the LemMinX/IntelliJ comparison when relevant, and the tests run.
- Large work is split into stacked pull requests: each branch starts from the previous one and its pull request targets that branch (`Stacked on #N` in the description). Rebase the stack instead of merging branches into each other.
- Never commit `target/`, `grammars/` or `*.wasm` (CI fails when they are tracked).

## Versioning and release

All crates and `extension.toml` share one version; the extension downloads the `xml-lsp` release with exactly its own version. Never bump versions by hand in a feature pull request: pushing a `vX.Y.Z` tag lets CI open the version-sync pull request, and merging it builds and publishes the binaries (details in [CONTRIBUTING.md](CONTRIBUTING.md#release-process)). A new workspace crate must be added to the version-sync list in `.github/workflows/ci.yml`. The versioned `process:exec` and `download_file` capabilities of `extension.toml` are updated by that job and checked by the unit tests of `src/lib.rs`.

## Documentation rules

- New or changed user-visible behaviour: one concise bullet in the README `Features` section (mention the LSP method and any Zed setting it needs), and an entry under `Unreleased` in `CHANGELOG.md`.
- New settings: `docs/configuration.md` reference table (and the README example when it is a common option).
- New module, crate, shared helper or convention: update this file.
- Development or release workflow changes: `CONTRIBUTING.md`.
