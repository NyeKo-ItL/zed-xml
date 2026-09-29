# XML for Zed

XML language support for Zed, including XML, XSD, XSLT, SVG, WSDL, plist, XJB and Android XML files.

## Features

- Tree-sitter syntax highlighting, indentation and outline support.
- Automatic bracket and tag editing provided by Zed.
- Native Rust LSP for XML diagnostics, formatting, completion, navigation and XSD validation.
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
