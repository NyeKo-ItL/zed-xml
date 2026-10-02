# Testing the extension in Zed

The automated tests cover the language server (round trips over the LSP, conformance suites) and the extension code that does not need Zed (checksums, platform table, manifest). What only Zed can exercise is listed here: run this checklist on Linux, macOS and Windows before a release.

## Setup

1. `cargo build -p xml-lsp` and `export XML_LSP_PATH="$PWD/target/debug/xml-lsp"` in the shell Zed is started from (PowerShell: `$env:XML_LSP_PATH`).
2. `zed: install dev extension` and select the repository. After a change: **Rebuild** in `zed: extensions`.
3. Open `tests/fixtures/real-world` in Zed (it contains documents of most supported file types).

## Startup and download (release candidates)

- [ ] Unset `XML_LSP_PATH` and any `lsp.xml-lsp.binary.path`, remove the extension's work directory, open an `.xml` file: the matching release binary is downloaded, its checksum verified, the server starts (`zed: open log`).
- [ ] Open a second XML file: the cached binary is reused (no new download).
- [ ] Corrupt the cached binary: it is rejected and downloaded again.
- [ ] An unsupported platform (or an offline machine) shows an actionable error, not a hang.

## Languages and file types

- [ ] Each family of `tests/fixtures/real-world`: the language is `XML` (status bar), highlighting looks right, brackets and tag matching work.
- [ ] A file without suffix starting with `<?xml` is recognised; `.dtd` and `.ent` open as `DTD`.
- [ ] `.csproj`, `.props`, `.html` are not claimed by this extension (the C# and HTML extensions keep them); `file_types` mapping to `XML` works.
- [ ] A fragment without root element (`<a/><b/>`, a snippet being typed) still highlights acceptably.

## Editing features

- [ ] Completion: elements, attributes and values from an XSD (`xsi:schemaLocation` fixture), automatic closing tag after `>`, `<?xml` templates.
- [x] Hover (XSD documentation), go to definition on `schemaLocation`, `xs:include`, `xsl:import`, `DOCTYPE`.
- [ ] Diagnostics appear while typing (debounced) and disappear when fixed; quick fixes from the lightbulb (mismatched tag, quote a value, declare `xsi`).
- [x] Format document and format selection with `"formatter": "language_server"`; formatting is idempotent.
- [x] Rename an element (both tags change), linked editing of start/end tags, matching tag highlight.
- [ ] Outline, workspace symbols, folding (`document_folding_ranges`), colors in an SVG.
- [ ] Rename of an XSD component across open instance documents.

## Robustness

- [ ] Open a 5 MiB XML file: Zed stays responsive, the `large-file` notice appears beyond `xml.maxFileSize`.
- [ ] Edit an `.xsd` used by an open instance: the instance is revalidated.
- [ ] `editor: restart language server` works, also after changing `lsp.xml-lsp.settings`.
- [ ] Killing the `xml-lsp` process is recovered by Zed's restart, without losing the buffers.
