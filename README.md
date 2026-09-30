# XML for Zed

XML language support for Zed, including XML, XSD, XSLT, SVG, WSDL, plist, XJB and Android XML files.

## Features

- Tree-sitter syntax highlighting, indentation and outline support.
- Matching start/end tag pairs and delimiters (`<`/`>`, `<?`/`?>`, quotes) for bracket highlighting and jumping.
- Text objects for elements (function/class) and comments, e.g. for Vim mode.
- Embedded CSS in `<style>` and JavaScript in `<script>` (SVG, XHTML, CDATA sections included).
- Comment and string scopes so auto-closing brackets and quotes stay out of comments and attribute values.
- Automatic bracket and tag editing provided by Zed.
- Native Rust LSP for XML diagnostics, formatting, completion, navigation and XSD validation.
- Matching start/end tag name highlighting (`textDocument/documentHighlight`), including prefixed names and malformed documents.
- Linked editing of start/end tag names (`textDocument/linkedEditingRange`, Zed `linked_edits` setting): renaming `<ns:item>` also renames `</ns:item>`, including `-`, `:` and `.` in names.
- Rename symbol (`textDocument/prepareRename` + `textDocument/rename`): element names (start and end tags), namespace prefixes (the `xmlns:ns` declaration and every use in its scope, including `type="ns:T"` in XSD and `xsi:type`, honouring nested redeclarations), and global XSD components (`xs:element`, `xs:attribute`, `xs:complexType`, `xs:simpleType`, `xs:group`, `xs:attributeGroup`) with their `ref`/`type`/`base`/`itemType`/`memberTypes`/`substitutionGroup` references and matching elements in open XML documents bound to the schema. Invalid XML names are rejected.
- Folding ranges (`textDocument/foldingRange`, LemMinX-style): multi-line elements fold up to the line before their end tag (which stays visible), multi-line start tags with many attributes, comments, CDATA sections, processing instructions, the `<!DOCTYPE ... [...]>` internal subset and nested `<!-- #region -->` / `<!-- #endregion -->` regions. The client `rangeLimit` is honoured. Zed only uses LSP folding ranges when `document_folding_ranges` is `"on"` (see below).
- `xsi:schemaLocation` and `xsi:noNamespaceSchemaLocation` support.
- Workspace-aware revalidation when an open XSD changes.

## Native Rust language server

The extension uses the `xml-lsp` server from `crates/xml-lsp`. The server is deliberately kept as a native executable; the Zed extension itself remains a `wasm32-wasip2` module.

The extension searches for the server in this order:

1. `XML_LSP_PATH`, containing the path to a local `xml-lsp` executable;
2. the cached native binary, when its `--version` output matches the extension version;
3. a native binary downloaded from the matching GitHub release.

Set `XML_LSP_DOWNLOAD_URL` to use a custom download URL. Downloaded binaries are always checked with `--version` before they are started. The cache and release download are independent of the directory containing the XML file; this is the mode used for normal installations.

From this repository on Windows, build the server with:

```powershell
cargo build -p xml-lsp
$env:XML_LSP_PATH = "$PWD\target\debug\xml-lsp.exe"
```

Then install the repository as a development extension in Zed. The **Rebuild** button recompiles the WASI extension. Restart the XML language server after changing the native Rust server or rebuild it with Cargo. When opening XML files outside this repository, keep `XML_LSP_PATH` configured or use a released native binary.

The native server is still an evolving subset of XML/XSD support. Full XSD conformance, XML catalogs and release-time native binary distribution remain separate tasks.

If Zed has a user or project formatter override, force XML formatting through the LSP with this setting:

```json
{
  "languages": {
    "XML": {
      "formatter": "language_server"
    }
  }
}
```

Zed uses tree-sitter and indentation folding by default. To use the server's folding ranges (regions, comments, DOCTYPE, CDATA...), enable them for XML:

```json
{
  "languages": {
    "XML": {
      "document_folding_ranges": "on"
    }
  }
}
```

## Development extension

1. Open the repository in Zed.
2. Run **Extensions: Install Dev Extension**.
3. Select this repository.
4. Use **Rebuild** from the development extensions list after changing `src/lib.rs` or `extension.toml`.
5. Open an XML document and inspect `zed: open log` if the server does not start.

## Build

The extension is a Zed WASI extension, not a console binary:

```powershell
rustup target add wasm32-wasip2
cargo build --target wasm32-wasip2
```

The native language server is built separately:

```powershell
cargo build -p xml-lsp
cargo run -p xml-lsp -- --stdio
```
