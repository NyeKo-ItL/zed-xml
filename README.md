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
- Selection ranges (`textDocument/selectionRange`, LemMinX/IntelliJ "extend selection"): prefix or local name → qualified name → attribute value (word, token, without and with quotes) → attribute → tag → element content → element → parent content → parent element … → document, also from text, comments, CDATA sections, processing instructions and end tags. Zed does not request LSP selection ranges today: its "select larger/smaller syntax node" actions use the tree-sitter tree; the provider serves other LSP clients (Helix, Neovim, VS Code-style clients).
- Document and range formatting (`textDocument/formatting`, `textDocument/rangeFormatting`, Zed "Format Selections"): honours the editor's `tabSize`/`insertSpaces` (tabs or spaces), `trimTrailingWhitespace` (outside CDATA sections), `insertFinalNewline` and `trimFinalNewlines`, keeps the document's line endings (LF or CRLF) and returns minimal line-based edits so cursors stay in place. Like LemMinX, range formatting expands the selection to the enclosing complete elements and re-indents only that region at its depth, even when the rest of the document is malformed; a region that is not well-formed is left untouched.
- XSD-aware hover (`textDocument/hover`, LemMinX-style Markdown with the hovered range): element names resolve their declaration in context (local declarations of the parent's content model, `ref`, groups, extensions, substitution groups, `xsi:type`) and show namespace, type and base type, cardinality, default/fixed values, `xs:annotation/xs:documentation` (untagged or English `xml:lang` preferred, nested XHTML reduced to text) and a link to the source schema; attribute names show type, `use`, default/fixed values and documentation; attribute values and simple-typed text show the enumeration value's documentation and a facet summary (allowed values, pattern, length and bounds, list/union). In XSD files, `type`/`ref`/`base`/`itemType`/`memberTypes`/`substitutionGroup` references and global component names show the referenced component's documentation, including from included/imported schemas and unsaved open buffers. Without a schema, element and attribute names keep a minimal hover (name and namespace).
- Document links (`textDocument/documentLink`, LemMinX-style) and go to definition on referenced files: each location of `xsi:schemaLocation`, `xsi:noNamespaceSchemaLocation`, `schemaLocation` of `xs:include`/`xs:import`/`xs:redefine`/`xs:override`, `href` of `xi:include`, `xsl:import` and `xsl:include`, `<?xml-stylesheet href?>`, `<?xml-model href?>` and the `<!DOCTYPE>` system identifier. Prefixes are resolved by namespace URI; relative paths (also percent-encoded) are resolved against the document, `http(s)` URLs are kept as-is, and only existing local files and `http(s)` URLs become links (with a tooltip). Zed shows these links on cmd-hover and opens them on cmd-click (`lsp_document_links`, on by default, Zed 1.5.3+, zed-industries/zed#56011); in older Zed versions and other clients, cmd-click on a local file path works through go to definition, which jumps to the start of the file.
- Code actions (`textDocument/codeAction`, LemMinX-style, Zed `editor: toggle code actions` / lightbulb): quick fixes for a mismatched end tag (rename the end tag, or the start tag), a stray end tag (remove it), an unclosed element (insert `</name>` or make it self-closing), a tag missing `>` (`>` or `/>`), a duplicate attribute (remove it), an unquoted attribute value (quote it), an unescaped `&`/`<` (`&amp;`/`&lt;`), missing required XSD attributes (inserted with their fixed/default value, first enumeration value or an empty value), a value outside its XSD enumeration (one action per allowed value, the closest preferred) and an unknown element ("Did you mean `<title>`?" by edit distance); refactorings between `<a></a>` and `<a/>`; and a source action binding an unbound document to a sibling `.xsd` (or a placeholder) with `xmlns:xsi` + `xsi:noNamespaceSchemaLocation`/`xsi:schemaLocation`. Well-formedness diagnostics are now precise (all problems, not only the first, on the offending name/value) and carry `data.kind`; XSD diagnostics carry `data.rule` and are located on the offending occurrence, and values outside an enumeration are reported.
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
