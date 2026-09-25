# XML for Zed

XML language support for Zed, including XML, XSD, XSLT, SVG, WSDL, plist, XJB and Android XML files.

## Features

- Tree-sitter syntax highlighting, indentation and outline support.
- Automatic bracket and tag editing provided by Zed.
- LemMinX language server for diagnostics, schema-aware completion, navigation, symbols and formatting.
- `xsi:schemaLocation`, `xsi:noNamespaceSchemaLocation`, XML catalogs and XSD files are handled by LemMinX.

## LemMinX

Java 17+ is the only runtime prerequisite. No package needs to be downloaded manually before installing the extension: LemMinX is downloaded automatically from the official Eclipse Maven repository the first time the server starts, then launched from the extension's private working directory.

This is not fully offline/self-contained. The first start requires network access unless a local LemMinX installation or JAR is already available. Zed extensions run as `wasm32-wasip2` modules; the extension API does not provide a portable way to embed one native executable for Windows, Linux and macOS and launch it directly.

The extension detects the following, in order:

1. a `lemminx` executable available on `PATH`;
2. `LEMMINX_JAR`, containing the path to a local JAR;
3. the pinned official LemMinX `0.31.2` JAR, downloaded automatically.

`LEMMINX_DOWNLOAD_URL` can optionally override the default download URL. This is only intended for testing a mirror or a newer compatible build:

```powershell
$env:LEMMINX_DOWNLOAD_URL = "https://example/lemminx.jar"
```

Java remains required because the official LemMinX distribution is a Java server. No LemMinX download or path configuration is required from the user, but Java itself must already be installed.

Zed passes the workspace root as the process working directory, so relative schema references work as expected.

## Native Rust alternative

There is currently no mature native/Rust XML language server with LemMinX's feature set. [`oxml-lsp`](https://github.com/sebastienrousseau/oxml-lsp) is a possible native Rust alternative, but its current documented scope is limited to XML well-formedness diagnostics through `publishDiagnostics`. It does not currently provide formatting, completion, XSD validation, `xsi:schemaLocation` support, hover or navigation.

Therefore this extension uses LemMinX for the complete XML/XSD experience. Replacing it with `oxml-lsp` would remove the features listed above; implementing a fully native replacement would require a separate Rust LSP and platform-specific packaging.

Install this repository in Zed with **Install Dev Extension**, then open an XML file. Use **Format Document** for LemMinX formatting and inspect `zed: open log` if the server does not start.

## Build

This is a Zed WASI extension, not a console binary:

```powershell
rustup target add wasm32-wasip2
cargo build --target wasm32-wasip2
```
