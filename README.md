# XML for Zed

XML language support for Zed, including XML, XSD, XSLT, SVG, WSDL, RELAX NG, XHTML, XAML, plist, storyboards, XLIFF, KML, GPX, Android XML, .NET and WiX project files and DTD files (see [File types](#file-types)), powered by a native Rust language server (`xml-lsp`) with XSD, DTD and XML catalog support.

## Installation

Install the extension from Zed's extensions page (`zed: extensions`), or from a clone of this repository with `zed: install dev extension` (see [CONTRIBUTING.md](CONTRIBUTING.md#development-extension)). The first time an XML file is opened, the extension downloads the `xml-lsp` binary matching its version from the [GitHub releases](https://github.com/NyeKo-ItL/zed-xml/releases) (Linux x86_64 and aarch64, macOS x86_64 and Apple silicon, Windows x86_64 and arm64) and verifies its SHA-256 checksum before starting it. On other platforms (such as 32-bit x86), build the server yourself and point `XML_LSP_PATH` at it (see [Language server](#language-server)).

## Features

- Tree-sitter syntax highlighting, indentation and outline support.
- Around fifty XML file types recognized by their suffix (XAML, `.resx`, storyboards, XLIFF, KML, GPX, RELAX NG, WiX, `.pom`, …; see [File types](#file-types)), all served by `xml-lsp` (diagnostics, completion, formatting, and workspace symbols and `workspace/didChangeWatchedFiles` for the files on disk).
- Matching start/end tag pairs and delimiters (`<`/`>`, `<?`/`?>`, quotes) for bracket highlighting and jumping.
- Text objects for elements (function/class) and comments, e.g. for Vim mode.
- Embedded CSS in `<style>` and JavaScript in `<script>` (SVG, XHTML, CDATA sections included).
- Comment and string scopes so auto-closing brackets and quotes stay out of comments and attribute values.
- Automatic bracket and tag editing provided by Zed.
- Native Rust LSP for XML diagnostics, formatting, completion, navigation and XSD validation.
- Well-formedness and Namespaces in XML diagnostics: mismatched/unclosed tags, duplicate attributes (also by expanded name), undeclared or reserved prefixes and invalid `xmlns` declarations, with quick fixes.
- Matching start/end tag name highlighting (`textDocument/documentHighlight`), including prefixed names and malformed documents.
- Linked editing of start/end tag names (`textDocument/linkedEditingRange`, Zed `linked_edits` setting): renaming `<ns:item>` also renames `</ns:item>`, including `-`, `:` and `.` in names.
- Rename symbol (`textDocument/prepareRename` + `textDocument/rename`): element names (start and end tags), namespace prefixes (the `xmlns:ns` declaration and every use in its scope, including `type="ns:T"` in XSD and `xsi:type`, honouring nested redeclarations), and global XSD components (`xs:element`, `xs:attribute`, `xs:complexType`, `xs:simpleType`, `xs:group`, `xs:attributeGroup`) with their `ref`/`type`/`base`/`itemType`/`memberTypes`/`substitutionGroup` references and matching elements in open XML documents bound to the schema. Invalid XML names are rejected.
- Folding ranges (`textDocument/foldingRange`, LemMinX-style): multi-line elements fold up to the line before their end tag (which stays visible), multi-line start tags with many attributes, comments, CDATA sections, processing instructions, the `<!DOCTYPE ... [...]>` internal subset and nested `<!-- #region -->` / `<!-- #endregion -->` regions. The client `rangeLimit` is honoured. Zed only uses LSP folding ranges when `document_folding_ranges` is `"on"` (see below).
- Selection ranges (`textDocument/selectionRange`, LemMinX/IntelliJ "extend selection"): prefix or local name → qualified name → attribute value (word, token, without and with quotes) → attribute → tag → element content → element → parent content → parent element … → document, also from text, comments, CDATA sections, processing instructions and end tags. Zed does not request LSP selection ranges today: its "select larger/smaller syntax node" actions use the tree-sitter tree; the provider serves other LSP clients (Helix, Neovim, VS Code-style clients).
- Document and range formatting (`textDocument/formatting`, `textDocument/rangeFormatting`, Zed "Format Selections"): honours the editor's `tabSize`/`insertSpaces` (tabs or spaces), `trimTrailingWhitespace` (outside CDATA sections), `insertFinalNewline` and `trimFinalNewlines`, keeps the document's line endings (LF or CRLF) and returns minimal line-based edits so cursors stay in place. Like LemMinX, range formatting expands the selection to the enclosing complete elements and re-indents only that region at its depth, even when the rest of the document is malformed; a region that is not well-formed is left untouched.
- XSD-aware hover (`textDocument/hover`, LemMinX-style Markdown with the hovered range): element names resolve their declaration in context (local declarations of the parent's content model, `ref`, groups, extensions, substitution groups, `xsi:type`) and show namespace, type and base type, cardinality, default/fixed values, `xs:annotation/xs:documentation` (untagged or English `xml:lang` preferred, nested XHTML reduced to text) and a link to the source schema; attribute names show type, `use`, default/fixed values and documentation; attribute values and simple-typed text show the enumeration value's documentation and a facet summary (allowed values, pattern, length and bounds, list/union). In XSD files, `type`/`ref`/`base`/`itemType`/`memberTypes`/`substitutionGroup` references and global component names show the referenced component's documentation, including from included/imported schemas and unsaved open buffers. Without a schema, element and attribute names keep a minimal hover (name and namespace).
- Document links (`textDocument/documentLink`, LemMinX-style) and go to definition on referenced files: each location of `xsi:schemaLocation`, `xsi:noNamespaceSchemaLocation`, `schemaLocation` of `xs:include`/`xs:import`/`xs:redefine`/`xs:override`, `href` of `xi:include`, `xsl:import` and `xsl:include`, `<?xml-stylesheet href?>`, `<?xml-model href?>` and the `<!DOCTYPE>` system identifier. Prefixes are resolved by namespace URI; relative paths (also percent-encoded) are resolved against the document, `http(s)` URLs are kept as-is, and only existing local files and `http(s)` URLs become links (with a tooltip). Zed shows these links on cmd-hover and opens them on cmd-click (`lsp_document_links`, on by default, Zed 1.5.3+, zed-industries/zed#56011); in older Zed versions and other clients, cmd-click on a local file path works through go to definition, which jumps to the start of the file.
- Code actions (`textDocument/codeAction`, LemMinX-style, Zed `editor: toggle code actions` / lightbulb): quick fixes for a mismatched end tag (rename the end tag, or the start tag), a stray end tag (remove it), an unclosed element (insert `</name>` or make it self-closing), a tag missing `>` (`>` or `/>`), a duplicate attribute (remove it), an unquoted attribute value (quote it), an unescaped `&`/`<` (`&amp;`/`&lt;`), missing required XSD attributes (inserted with their fixed/default value, first enumeration value or an empty value), a value outside its XSD enumeration (one action per allowed value, the closest preferred) and an unknown element ("Did you mean `<title>`?" by edit distance); refactorings between `<a></a>` and `<a/>`; and a source action binding an unbound document to a sibling `.xsd` (or a placeholder) with `xmlns:xsi` + `xsi:noNamespaceSchemaLocation`/`xsi:schemaLocation`. Well-formedness diagnostics are now precise (all problems, not only the first, on the offending name/value) and carry `data.kind`; XSD diagnostics carry `data.rule` and are located on the offending occurrence, and values outside an enumeration are reported.
- Workspace symbols (`workspace/symbol`, Zed `project symbols: toggle`, IntelliJ "Go to Symbol"-style): global XSD components (`xs:element`, `xs:attribute`, `xs:complexType`, `xs:simpleType`, `xs:group`, `xs:attributeGroup`, `xs:notation`, also under `xs:redefine`/`xs:override`) with their target namespace (or file name) as container, plus the root element and elements identified by `xml:id`/`id`/`name` (e.g. `<bean id="dataSource">`) in XML files — not every element, which would drown the results. Open documents (including unsaved changes) and `*.xml`/`*.xsd`/`*.xsl`/`*.svg`/… files of the workspace folders are indexed lazily on the first query (hidden directories, `target/` and `node_modules/` skipped; at most 5000 files of 4 MiB), cached by modification time and refreshed through `workspace/didChangeWatchedFiles` and workspace folder changes. Queries match case-insensitively (exact, prefix, substring, then fuzzy subsequence); an empty query returns a bounded list.
- Hierarchical document symbols (`textDocument/documentSymbol`) for clients that support them: nested elements with their `xml:id`/`id`/`name` attribute as detail, tolerant of malformed documents.
- Document colors (`textDocument/documentColor` + `textDocument/colorPresentation`, VS Code/IntelliJ-style color swatches): SVG presentation attributes (`fill`, `stroke`, `stop-color`, `flood-color`, `lighting-color`, `color`, `solid-color`), `style="..."` declarations and `<style>` CSS (CDATA included, comments, strings and `url()` skipped) with `#rgb`/`#rgba`/`#rrggbb`/`#rrggbbaa`, `rgb()`/`rgba()`/`hsl()`/`hsla()` (comma or space syntax, percentages, angle units) and all CSS named colors including `transparent` (`currentColor`/`none` ignored); Android resources (`<color>`, `<item>` and `<drawable>` values in `<resources>` or `res/values*/` files, and color-like `android:`/`app:`/`tools:` attributes such as `android:textColor`, `app:tint` or `android:background`) with Android hex semantics where alpha comes first (`#ARGB`, `#AARRGGBB`). Color presentations keep the original format first, then hex, `rgb()`, `hsl()` and the color name (Android: `#AARRGGBB`, `#RRGGBB`). Zed renders them according to `lsp_document_colors` (`inlay` by default, or `background`, `border`, `none`).
- XSD identity constraints (in `textDocument/publishDiagnostics`, LemMinX/Xerces-style): duplicate `xs:ID` values, `xs:IDREF(S)` without target, and `xs:unique`/`xs:key`/`xs:keyref` evaluated with their selector/field XPath subset (namespace prefixes from the schema, values compared in the value space). Go to definition (`textDocument/definition`) jumps from an `IDREF(S)` value (XSD or DTD) or a keyref value to the ID or key it designates, and find references (`textDocument/references`) on an ID or key lists its references (IntelliJ-style).
- XSD datatype validation (in `textDocument/publishDiagnostics`, LemMinX/Xerces-style): element text and attribute values are checked against all XSD 1.0 built-in types (numbers, dates and durations, `QName`, `anyURI`, binary, names and tokens, built-in lists), user list/union types and every facet (`length`/`minLength`/`maxLength`, `pattern` in the XSD regex dialect including `\i`/`\c`, class subtraction and `\p{IsBlock}`, `enumeration` and `min`/`maxInclusive`/`Exclusive` compared in the value space, `totalDigits`/`fractionDigits`, `whiteSpace`), with `xsi:type`, `xsi:nil`, `default`/`fixed` values and the error on the offending value.
- `xsi:schemaLocation` and `xsi:noNamespaceSchemaLocation` support (relative, absolute, `file://` and percent-encoded paths such as `my%20schemas/a%20b.xsd`).
- OASIS XML Catalogs 1.1 (`xml.catalogs`, like LemMinX): `uri`, `system`, `public`, `rewriteURI`, `rewriteSystem`, `uriSuffix`, `systemSuffix`, `delegateURI`/`delegateSystem`/`delegatePublic`, `nextCatalog` (cycle-safe), `group`, `xml:base` and `prefer`. Catalogs map remote `http(s)` schema locations, namespaces (`xsi:schemaLocation`, `xs:import` without `schemaLocation`), `xs:include` locations, `fileAssociations` system IDs and `<!DOCTYPE>` public/system identifiers to local files, so validation, completion, hover, document links and go to definition work offline. Catalogs are cached by modification time, watched, and re-read on change (open documents are revalidated); an open catalog reports entries whose local target is missing. A remote schema that no catalog maps is reported as a warning.
- DTD support (LemMinX-style, new `dtd-core` crate): the `<!DOCTYPE>` internal subset and the external DTD (`SYSTEM`/`PUBLIC`, resolved through `xml.catalogs`, then relative to the document; local files only, remote DTDs are never downloaded and reported as a warning) with parameter entities, external parameter entities and `INCLUDE`/`IGNORE` conditional sections. Diagnostics: DTD syntax errors (in the internal subset, in `.dtd`/`.ent` files, or summarized on the DOCTYPE system identifier for an external DTD), undeclared entity references `&foo;` (also without a DTD; the five predefined entities are always allowed), and validation against the DTD (root name, undeclared elements/attributes, content models `EMPTY`/`ANY`/mixed/`(a, (b | c)*, d?)+` via an NFA, `#REQUIRED`/`#FIXED`/enumerated/`NOTATION` attributes, unique `ID`s and existing `IDREF(S)` targets, `NMTOKEN(S)`, `ENTITY/ENTITIES`), alongside XSD validation when both are present. Completion of elements allowed by the parent's content model at the cursor, attributes, enumerated values (and existing IDs for `IDREF`), entities after `&`, and in DTDs `<!ELEMENT`/`<!ATTLIST`/… snippets, `#PCDATA`/`#REQUIRED`/…, `%parameter;` entities and element names. Hover shows the DTD declaration (with the element's attribute list and the preceding `<!-- comment -->` as documentation); go to definition jumps from elements, attributes and `&entity;`/`%entity;` references to their declaration; quick fixes declare a missing entity, add a missing required attribute or replace an invalid enumerated/fixed value. `.dtd`/`.ent` files get their own DTD language (tree-sitter-xml `dtd` grammar) with declaration symbols. Entity expansion is bounded (1 MiB per general entity, 4 MiB of parameter-entity text, depth 32) against "billion laughs" attacks; external general entities are never read (`xml.validation.resolveExternalEntities` only checks that they resolve).
- Security hardening (see [Security model](docs/configuration.md#security-model) and [SECURITY.md](SECURITY.md)): no network access at all (remote locations and network shares such as `\\server\share` or `file://host/…` are never read, only mapped through catalogs), only regular local files of bounded size are read (a link to `/dev/zero` or a FIFO is refused), bounded entity expansion including "quadratic blowup" through attribute values, bounded schema and catalog graphs (256 schema documents, 64 catalogs, cycles included), and deeply nested documents (100 000 levels) handled without stack overflow or quadratic slowdowns.
- XSLT awareness (IntelliJ-style, for documents whose root is in the XSLT namespace or simplified stylesheets): XPath 1.0/2.0/3.1 syntax errors (`xpath-syntax`) in expressions (`select`, `test`, `use`, `group-by`, `use-when`, ...), patterns (`match`, `count`, `from`, `group-starting-with`, ...), attribute value templates (`href="{$base}/{@id}"`, including unmatched `{`/`}`) and XSLT 3.0 text value templates (`expand-text="yes"`), located on the offending token; completion of the `xsl:` elements allowed in context (declarations at the top level, `xsl:when`/`xsl:otherwise` in `xsl:choose`, instructions elsewhere, filtered by the stylesheet `version`) with their required attributes, of attributes and enumerated values (`method`, `order`, `on-no-match`, ...), of named templates, modes, attribute sets, keys, template parameters in `xsl:with-param`, of variables and parameters after `$` (`$` triggers completion) and of stylesheet functions; go to definition (`textDocument/definition`), references and rename (`textDocument/rename`) of named templates (`xsl:call-template`), variables and parameters (`$x` resolved with XSLT scoping and shadowing, XPath `for`/`let`/`some`/`every` variables, `xsl:with-param` to the called template's `xsl:param`), `xsl:function` calls by name and arity, modes, keys (`key('k', ...)`), attribute sets, decimal formats, accumulators, character maps and output definitions, across `xsl:include`/`xsl:import` (and open stylesheets including the current one); hover on XSLT elements, attributes and references. Attribute values using DTD entities are not reported.
- Workspace-aware revalidation when an open XSD changes.
- Verified server downloads: prebuilt `xml-lsp` binaries for Linux (static musl builds, any distribution), macOS and Windows on x86_64 and arm64, each published with a SHA-256 checksum that the extension checks before starting the binary; unsupported platforms get an error listing the supported ones.
- Standard Zed language server binary settings: `lsp.xml-lsp.binary.path`, `arguments` and `env` (see [Language server](#language-server)).
- Robust server: validation runs on a background thread with the latest document snapshot (typing, completion and hover never wait for a large schema; results of outdated versions are dropped and publications carry the document `version`), `$/cancelRequest` answers queued requests with `RequestCancelled`, a failing request answers `InternalError` instead of stopping the server, clean `shutdown`/`exit` (exit code 0 after `shutdown`), negotiated `positionEncoding` (`utf-8` when the client offers it, else `utf-16`, or `utf-32`), documents with a byte order mark (kept by formatting), and UTF-16/ISO-8859-1 schemas, DTDs and catalogs read from disk.
- Fast on large files: the tag tree of each document version is shared by highlights, linked editing, folding and symbols; parsed schemas and merged schema sets are reused until a file changes on disk; validation is debounced while typing (`xml.validation.debounce`, 200 ms by default); documents over `xml.maxFileSize` (10 MiB by default) are only checked for well-formedness, with an information diagnostic (`large-file`) saying so.
- LemMinX-style `xml.*` settings (formatting, validation, file associations, completion, symbols, colors) from Zed `lsp.xml-lsp.settings`, applied live (see [Configuration](#configuration)).

## Language server

The extension runs the `xml-lsp` server from `crates/xml-lsp`. The server is a native executable; the Zed extension itself is a `wasm32-wasip2` module.

The extension looks for the server in this order:

1. the `lsp.xml-lsp.binary.path` Zed setting;
2. `XML_LSP_PATH`, containing the path to a local `xml-lsp` executable;
3. the cached native binary, when its `--version` output matches the extension version and it still matches the checksum recorded at download time;
4. a native binary downloaded from the matching GitHub release.

`lsp.xml-lsp.binary.arguments` replaces the default `--stdio` argument and `lsp.xml-lsp.binary.env` adds environment variables, whichever binary is started:

```json
{
  "lsp": {
    "xml-lsp": {
      "binary": { "path": "/path/to/xml-lsp", "arguments": ["--stdio"] }
    }
  }
}
```

Every release asset `xml-lsp-<target>[.exe]` is published with a `xml-lsp-<target>[.exe].sha256` file (and a `SHA256SUMS` file listing them all). The extension downloads the checksum first, then the binary, and deletes and rejects a binary whose SHA-256 differs; it then checks the binary with `--version`. On Linux it uses the statically linked musl builds, which do not depend on the distribution's glibc version (the glibc builds are published too, for other clients).

Set `XML_LSP_DOWNLOAD_URL` to use a custom download URL; the checksum is then read from `<URL>.sha256`, or from `XML_LSP_DOWNLOAD_SHA256` (the expected hexadecimal SHA-256) when set. The cache and release download are independent of the directory containing the XML file.

The server never downloads schemas, DTDs or entities: remote locations must be mapped to local files through [XML catalogs](docs/configuration.md#xml-catalogs). Full XSD conformance is still a work in progress.

## File types

The `XML` language (and so `xml-lsp`) is used for files with these suffixes, and for files without a known suffix whose first line starts with `<` and contains `xml` (such as `<?xml version="1.0"?>`):

| Family | Suffixes |
|--------|----------|
| XML, schemas, transformations | `xml`, `xsd`, `xsl`, `xslt`, `rng` (RELAX NG, XML syntax), `wsdl`, `xjb` |
| Web, documents, feeds | `svg`, `xhtml`, `xht`, `rss`, `atom`, `opml`, `opf` (EPUB package), `dita`, `ditamap`, `xul` |
| Apple | `plist`, `entitlements`, `storyboard`, `xib`, `xcscheme`, `xcworkspacedata`, `tmTheme`, `tmLanguage` |
| .NET, MSBuild, WiX | `xaml`, `axaml`, `fsproj`, `vbproj`, `vcxproj`, `vcxproj.filters`, `csproj.user`, `nuspec`, `resx`, `pubxml`, `wxs`, `wxi`, `wxl` |
| Java, Android | `pom`, `fxml`, `iml`, `tld`, `axml` (and `pom.xml`, `AndroidManifest.xml`, … through `xml`) |
| Localization | `xlf`, `xliff`, `tmx` |
| Geography, graphs, music, processes | `kml`, `gpx`, `graphml`, `musicxml`, `bpmn` |

`.dtd` and `.ent` files use the `DTD` language.

Some XML suffixes are deliberately left out because another extension of the Zed registry claims them, and Zed picks one of the two languages arbitrarily when two extensions claim the same suffix: `csproj`, `proj`, `props`, `targets` and `slnx` (the C# extension, whose MSBuild language servers expect its own language names), `html`/`htm` (HTML) and `urdf` (URDF). Generic suffixes that are not always XML (`config`, `rdf`, `ts`, `ui`, `mod`) are left out too. To open such files as XML anyway, map them with Zed's `file_types` setting; the server serves every document Zed sends it:

```json
{
  "file_types": {
    "XML": ["csproj", "props", "targets", "config", "rdf"]
  }
}
```

## Editor settings

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

Document colors are shown as inlay swatches by default. Change how Zed renders them (or turn them off) with the editor setting:

```json
{
  "lsp_document_colors": "background"
}
```

## Configuration

The server reads LemMinX-style settings from an `xml` section. In Zed, put them under `lsp.xml-lsp.settings` (the `xml` key is optional); the extension sends them as `initializationOptions` and answers the server's `workspace/configuration` requests, and changes are applied live through `workspace/didChangeConfiguration` (diagnostics of open documents are re-published when validation settings change). `lsp.xml-lsp.initialization_options`, when set, is sent as-is instead. Every setting is optional and the defaults keep the historical behaviour.

```json
{
  "lsp": {
    "xml-lsp": {
      "settings": {
        "xml": {
          "format": {
            "enabled": true,
            "splitAttributes": "alignWithFirstAttr",
            "maxLineWidth": 120,
            "preservedNewlines": 1,
            "closingBracketNewLine": false,
            "emptyElements": "collapse",
            "preserveAttributeLineBreaks": true
          },
          "validation": {
            "enabled": true,
            "schema": { "enabled": "always" },
            "noGrammar": "hint",
            "disallowDocTypeDecl": false
          },
          "completion": { "autoCloseTags": true },
          "symbols": { "enabled": true, "maxItemsComputed": 5000 },
          "colors": { "enabled": true },
          "catalogs": ["catalog.xml"],
          "fileAssociations": [
            { "pattern": "**/*.project", "systemId": "schemas/project.xsd" }
          ]
        }
      }
    }
  }
}
```

- `xml.format.*`: enable/disable formatting, attribute layout (`splitAttributes`: `preserve`, `splitNewLine`, `alignWithFirstAttr`), attribute wrapping at `maxLineWidth`, kept blank lines, closing bracket on its own line, empty element expansion/collapse, and fallbacks for `tabSize`/`insertSpaces`/`trimFinalNewlines`/`insertFinalNewline`/`trimTrailingWhitespace` when the editor does not send them.
- `xml.validation.*`: turn all diagnostics off, choose XSD validation (`always`, `never`, `onValidSchema`), report documents without a grammar (`noGrammar`: `ignore`, `hint`, `info`, `warning`) and forbid `<!DOCTYPE>`.
- `xml.completion.autoCloseTags`, `xml.symbols.enabled`/`maxItemsComputed`, `xml.colors.enabled`.
- `xml.fileAssociations`: bind files matching a glob (`*`, `?`, `**`, `{a,b}`; relative to the workspace folder, or the file name when the pattern has no `/`) to an XSD (`systemId`: path relative to the workspace folder, absolute path or `file://` URI) for validation, completion, hover and code actions, when the file declares no schema itself.
- `xml.catalogs`: OASIS XML catalog files (absolute path, `file://` URI, `~/…` or path relative to the workspace folder) used to resolve schemas and DTDs; `xml.autoDetectCatalogs` (off by default) also uses a `catalog.xml` at the root of each workspace folder.

See [docs/configuration.md](docs/configuration.md) for the full reference and [docs/diagnostics.md](docs/diagnostics.md) for every diagnostic code (a stable API).

## Troubleshooting

- **The server does not start**: run `zed: open log` and look for `xml-lsp` messages. A failed download mentions the release asset name; set `XML_LSP_PATH` to a locally built binary (`cargo build -p xml-lsp --release`) to work around it. Restart the server with `editor: restart language server`.
- **Checksum mismatch**: the log says `SHA-256 mismatch` when the downloaded binary differs from the published checksum (truncated download, proxy rewriting the response, tampered mirror). The binary is deleted and downloaded again at the next start; if it persists, check your proxy or use `XML_LSP_PATH`.
- **Unsupported platform**: the error lists the platforms with prebuilt binaries; elsewhere, build the server (`cargo build -p xml-lsp --release`) and set `XML_LSP_PATH`.
- **Wrong or outdated binary**: `lsp.xml-lsp.binary.path`, then `XML_LSP_PATH`, always win, so make sure they point at an up-to-date build or remove them. Without them, a cached binary whose `xml-lsp --version` differs from the extension version is replaced by the matching release. `XML_LSP_PATH`, `XML_LSP_DOWNLOAD_URL` and `XML_LSP_DOWNLOAD_SHA256` are read from the shell environment of the project, so set them in the shell Zed is launched from.
- **No XSD validation or completion**: check that the document declares `xsi:schemaLocation`/`xsi:noNamespaceSchemaLocation`, or add an `xml.fileAssociations` entry. Remote (`http(s)://`) schemas are not downloaded: map them with `xml.catalogs` (a warning diagnostic points at unmapped locations).
- **Formatting does nothing**: make sure `formatter` is `language_server` for XML (see [Editor settings](#editor-settings)) and that `xml.format.enabled` is not `false`. Regions that are not well-formed are left untouched by range formatting.
- **Folding ranges from the server are not used**: set `document_folding_ranges` to `"on"` for XML.
- **Detailed traces**: `dev: open language server logs` shows the LSP messages exchanged with `xml-lsp`; the server logs errors (unreadable catalogs, schema loading failures) to its stderr, visible there too.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development setup, building the server and the extension, tests and the release process, and [AGENTS.md](AGENTS.md) for the architecture and coding conventions. Notable changes are listed in [CHANGELOG.md](CHANGELOG.md).

## License

[MIT](LICENSE)
