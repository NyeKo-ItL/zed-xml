# Changelog

All notable changes to this project are documented in this file. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html). The extension and the `xml-lsp` server always share the same version.

## [Unreleased]

### Added

- Strict syntax of DTD markup declarations (`dtd-grammar`, `dtdSyntax`), checked on the raw text before parameter entities are expanded: whitespace where the grammar requires it, content model groups (`(a | b, c)`, whitespace before `?`/`*`/`+`), mixed content, attribute types and defaults, entity values and external identifiers (public identifier characters), notations, colons in entity and notation names, and parameter entity references inside markup declarations of the internal subset (well-formedness constraint "PEs in Internal Subset"). References to internal entities whose replacement text is not well-formed content (`<!ENTITY e "</a><a>">`) are reported at the reference (`xml-entity`, `malformedEntity`). Namespace URIs are compared after resolving character references; `xmlns:` without a prefix and processing instruction targets with a colon are reported.
- Strict XML 1.0 well-formedness (`xml_core::strict`, `xml-syntax` with new `data.kind` values `invalidCharacter`, `invalidName`, `invalidReference`, `malformedDeclaration`, `malformedComment`, `malformedProcessingInstruction`, `malformedCData`, `contentOutsideRoot`, `missingRoot`): illegal characters and character references, invalid names, `--` in comments, misplaced or malformed XML declarations, processing instructions and DOCTYPE declarations, `]]>` in text, missing attribute values, content outside the root element. It runs once the tolerant checks find nothing, so the problems being typed keep their precise diagnostics. An empty (or whitespace-only) document is deliberately not reported.
- Namespaces in XML constraints in the well-formedness diagnostics (`xml-syntax`, new `data.kind` values `undeclaredPrefix`, `invalidQualifiedName`, `invalidNamespaceDeclaration`): undeclared element/attribute prefixes (with a quick fix declaring well-known prefixes such as `xsi` or `xsl` on the root element), `a:b:c`/`a:` names, `xmlns` used as an element prefix, reserved `xml`/`xmlns` prefixes and namespaces, empty namespaces bound to a prefix, and attributes with the same expanded name (`duplicateAttribute`). Documents whose DOCTYPE may declare `xmlns:*` defaults are not checked for undeclared prefixes; formatting still works on documents that only break these constraints.
- More XML file types: `xhtml`, `xht`, `rss`, `atom`, `opml`, `opf`, `dita`, `ditamap`, `xul`, `rng`, `entitlements`, `storyboard`, `xib`, `xcscheme`, `xcworkspacedata`, `tmTheme`, `tmLanguage`, `xaml`, `axaml`, `fsproj`, `vbproj`, `vcxproj`, `vcxproj.filters`, `csproj.user`, `nuspec`, `resx`, `pubxml`, `wxs`, `wxi`, `wxl`, `pom`, `fxml`, `iml`, `tld`, `xlf`, `xliff`, `tmx`, `kml`, `gpx`, `graphml`, `musicxml` and `bpmn`, also indexed for workspace symbols and watched on disk. Suffixes claimed by other Zed registry extensions (`csproj`, `props`, `targets`, …) are left to them; see the README "File types" section.
- Zed queries for matching tag brackets, text objects, CSS/JavaScript injections in `<style>`/`<script>`, and comment/string overrides (#21).
- Matching start/end tag highlight (`textDocument/documentHighlight`) (#22).
- Linked editing of start and end tag names (`textDocument/linkedEditingRange`) (#23).
- Rename of elements, namespace prefixes and global XSD components, across open instance documents (#24).
- LemMinX-style folding ranges: elements, comments, CDATA, processing instructions, DOCTYPE subset, `#region` comments (#25).
- Selection ranges (`textDocument/selectionRange`) (#26).
- Range formatting, and support for the LSP formatting options (`tabSize`, `insertSpaces`, `trimTrailingWhitespace`, `insertFinalNewline`, `trimFinalNewlines`); formatting now returns minimal edits and keeps CRLF line endings (#27).
- XSD-aware hover with `xs:documentation`, types, cardinality, enumerations and facets (#28).
- Document links and go to definition for `xsi:schemaLocation`, `xs:include`/`xs:import`, `xi:include`, XSLT imports, `<?xml-stylesheet?>`, `<?xml-model?>` and DOCTYPE system identifiers (#29).
- Code actions: quick fixes for well-formedness, XSD required attributes, enumeration values and unknown elements, empty-element refactorings, and binding a document to an XSD (#30).
- Workspace symbols and hierarchical document symbols (#31).
- Document colors and color presentations for SVG, CSS and Android resources (#32).
- LemMinX-style `xml.*` settings (formatting, validation, file associations, completion, symbols, colors), applied live through `workspace/configuration` and `workspace/didChangeConfiguration` (#33).
- OASIS XML Catalogs 1.1 support for schema and DTD resolution (`xml.catalogs`, `xml.autoDetectCatalogs`) (#34).
- DTD support: internal and external subsets, entity checks with expansion limits, validation, completion, hover, go to definition, quick fixes, and a DTD language for `.dtd`/`.ent` files (new `dtd-core` crate) (#35).
- XSLT awareness for stylesheets (new `xpath-core` crate with a tolerant XPath 1.0/2.0/3.1 parser): `xpath-syntax` diagnostics in `select`/`test`/`match`/`use`/`group-by`/... expressions and patterns, `use-when`, attribute value templates and XSLT 3.0 text value templates; completion of `xsl:` elements allowed in context (XSLT 1.0/2.0/3.0 by the stylesheet `version`) with their required attributes, attributes, enumerated values, component names, variables after `$` and key names; go to definition, references and rename of named templates, variables and parameters (XSLT and XPath scoping), functions, modes, keys, attribute sets, decimal formats, accumulators, character maps and output definitions across `xsl:include`/`xsl:import`; hover on XSLT elements, attributes and references; `$` is a completion trigger character.
- Completion templates for `<?xml ...?>`, `<?xml-model ...?>` and `<?xml-stylesheet ...?>` (#17).
- Completion query characters (`<`, `/`, `>`, space, `=`, `"`, `?`) so Zed keeps completions open while typing XML (#19).
- Conformance testing: about 200 specification cases (XML 1.0, Namespaces, RFC 7303, XSD 1.0), 73 real-world documents, the roxmltree and libxml2 corpora, the W3C XML and XSD conformance suites and the libxml2 schema tests (fetched by `scripts/fetch-test-suites.sh`), with baselines of known failures, a `Conformance suites` CI job, and an LSP smoke test over every fixture (new `xml-conformance` crate) (#38).
- XSD identity constraints (`textDocument/publishDiagnostics`): unique `xs:ID` values and existing `xs:IDREF(S)` targets, `xs:unique`/`xs:key`/`xs:keyref` with the XPath subset of their selectors and fields (values compared in the value space, attribute defaults included), and invalid constraint declarations reported as schema errors; new `data.rule` values `duplicateId`, `unknownIdref`, `duplicateKey`, `missingKeyField`, `invalidKeyField`, `unknownKeyref`. Go to definition (`textDocument/definition`) from an `IDREF(S)` (XSD or DTD) or keyref value to the ID or key it designates, and references (`textDocument/references`) from an ID or key to its references.
- XSD datatype validation (`textDocument/publishDiagnostics`): the 44 XSD 1.0 built-in types with their derivation hierarchy and `whiteSpace` handling, list and union types, every constraining facet (enumerations and bounds compared in the value space, date/time partial order, durations), XSD regular expressions translated to the `regex` crate with a cache, element and attribute values resolved through local declarations, `xsi:type` and `QName` prefixes, `default`/`fixed` values, and precise messages on the value (`'2024-13-01' is not a valid xs:date: month must be 01-12`); new `data.rule` values `invalidEnumeration` and `invalidAttributeValue`.
- `CONTRIBUTING.md`, `CHANGELOG.md` and `AGENTS.md`; the README now focuses on users (installation, configuration, troubleshooting).
- Server robustness: diagnostics computed on a background worker (latest snapshot only, stale results dropped, `version` in `publishDiagnostics`), `$/cancelRequest` (`RequestCancelled` for queued requests), panic isolation per request and notification (`InternalError`, logged on stderr), LSP-conformant `shutdown`/`exit` (requests after `shutdown` rejected, exit code 0 or 1), `positionEncoding` negotiation (UTF-8, UTF-16, UTF-32), byte order marks and UTF-16 files read from disk (new `xml_core::text`).
- Incremental analysis: tag trees cached per document version and shared by highlights, linked editing, folding and document symbols; parsed schemas and merged schema sets cached by modification time; debounced validation (`xml.validation.debounce`); a size budget for large documents (`xml.maxFileSize`, `large-file` information diagnostic); `criterion` benchmarks for `xml-core`, `xsd-core` and the server, run in quick mode by a `Benchmarks` CI job. On a 1 MB document: document symbols about 40 times faster (no more quadratic position conversion), highlights 3 times, diagnostics after a change 1.7 times, completion 1.4 times.
- Security hardening: `SECURITY.md` (private vulnerability reporting) and a security model in `docs/configuration.md`; network shares (UNC paths, `file://host/…`) and non-regular files (devices, FIFOs, directories) are never read, referenced files are read with size bounds; at most 256 schema documents and 64 catalog files are loaded per document; attribute values expanding entities are bounded per document ("quadratic blowup", new `expansionBudget` diagnostic kind), and so are content model matching and diagnostic messages.

### Changed

- `lsp.xml-lsp.binary.path`, `binary.arguments` and `binary.env` Zed settings, taking precedence over `XML_LSP_PATH`, like other Zed language server extensions.
- `extension.toml` declares a `download_file` capability scoped to this repository's release of the extension version (kept in step by the version-sync job, checked by unit tests).
- Release binaries for Linux aarch64, static musl Linux (x86_64 and aarch64; the extension now downloads these on Linux, so it no longer depends on the distribution's glibc), macOS x86_64 and Windows arm64, in addition to Linux x86_64 (glibc), macOS arm64 and Windows x86_64.
- SHA-256 checksums for every release asset (`<asset>.sha256` and `SHA256SUMS`); the extension verifies the downloaded binary (and the cached one) before starting it, with `XML_LSP_DOWNLOAD_SHA256` for custom download URLs.
- A clear error listing the supported platforms when no prebuilt binary exists for the current one.
- Release notes generated from conventional commits with git-cliff (`cliff.toml`), after the hand-written CHANGELOG section.
- CI quality gates: `cargo clippy -D warnings`, `cargo-deny` (licences, advisories, bans, sources), a minimum supported Rust version (1.88, checked in CI), tests on Linux, Windows and macOS, a `wasm32-wasip2` build of the extension, and coverage reports (`cargo llvm-cov`).
- Comments, documentation and every user-facing message (diagnostics, hover, code action titles, errors) are now in English (#36).
- Well-formedness diagnostics report every problem with a precise range and a stable `data.kind`; XSD diagnostics carry `data.rule` and point at the offending element (#30).

### Fixed

- `xs:redefine` and `xs:override` schemas are loaded as dependencies (their `schemaLocation` was ignored), and a redefined type or group extends the definition it replaces instead of itself.
- A schema that only defines types, groups or attributes (as included by other schemas) is no longer rejected for containing no `xs:element`; only a document that is not an `xs:schema` is.
- Elements and types no longer resolve to a same-named component of another namespace: only components without namespace ("chameleon" schemas) match any namespace. An unqualified child where the schema requires qualified ones is reported as being in the wrong namespace, with the expected expanded name.
- Documents declaring XML 1.1 accept its control characters and references.
- A DOCTYPE whose literals or comments contain `<` or `>` (`<!ENTITY e "<foo/&#62;">`) no longer breaks the XML reader: the declaration is masked before reading, and checked by the strict check and the DTD parser.
- Formatting no longer drops the whitespace between inline elements of mixed content (`<code>jar</code> <code>war</code>` lost its space, changing the text).
- XSD content models are now checked with an automaton built from the component model (sequences, choices, `xs:all`, group references, wildcards with their namespace constraint, occurrence ranges on groups, extension, substitution groups) instead of name-keyed lists: valid documents are no longer rejected when several elements share a name (`<xs:element ref="comment" minOccurs="0"/>`), a `choice` accepts exactly one alternative, nested groups and extensions are honoured, abstract elements are refused, and empty elements (`<a/>`) are checked for missing children. Misplaced and surplus children are reported on the child (`unexpectedOrder`, `tooManyElements`, with the expected names), missing ones on the parent (`missingElement`).
- Prefixed or default-namespace root elements (`<t:root xmlns:t="urn:x">`) are matched by expanded name instead of being reported as undeclared.
- Deeply nested documents (100 000 levels) no longer overflow the stack in selection ranges, and no longer make namespace resolution, the tag tree, range formatting, document symbols, references, code actions and diagnostics quadratic; formatting a document whose indentation would take gigabytes is refused; XSD schemas nested more than 256 levels are reported instead of being parsed.
- Declared the `xml-lsp --version` process capability for versioned downloaded binaries on Windows and Unix-like platforms; release synchronization updates the permission names with the version.
- Downloaded `xml-lsp` binaries are cached under a versioned path, so an outdated or locked binary from a previous extension version cannot be reused.
- The cached `xml-lsp` binary is reused when its version matches, instead of being downloaded again on every start (#16).
- Formatting keeps explicit empty elements (`<tag></tag>`) as written (#17).
- Incremental document synchronization handles full replacements and out-of-range edits (#18).
- A position beyond the end of its line means the end of that line (it meant the end of the document), and edits never split a CRLF line break (formatting of documents mixing LF and CRLF returned invalid ranges).
- A non-ASCII character between DTD declarations no longer panics the DTD parser.

## [0.9.0] - 2026-09-29

Versions 0.7.0 and 0.8.0 were tagged while the release pipeline was being set up but not published; their changes are listed here.

### Added

- Automated releases: pushing a `vX.Y.Z` tag opens a pull request synchronizing every crate and `extension.toml` to that version; merging it builds the native servers and publishes the GitHub release (#8, #9, #10, #11, #12, #15).

### Fixed

- The released `xml-lsp` binary is preferred over local binaries picked up implicitly; only `XML_LSP_PATH` overrides it (#13).

## [0.6.0] - 2026-09-29

### Added

- `xml-lsp --version` and `serverInfo` in the `initialize` response; the extension checks the version of the binary it starts (#7).

## [0.5.0] - 2026-09-29

### Fixed

- Formatting of empty tags (#5).

## [0.4.0] - 2026-09-29

### Fixed

- Server downloads no longer use the GitHub API, avoiding rate limits (#4).

## [0.3.0] - 2026-09-29

### Fixed

- Download of the native server binary (#3).

## [0.2.0] - 2026-09-29

### Fixed

- Native server binaries are resolved from the GitHub release assets (#2).

## [0.1.0] - 2026-09-28

### Added

- Tree-sitter XML grammar with highlighting, indentation and outline for XML, XSD, XSLT, SVG, WSDL, plist, XJB and Android XML files.
- Native Rust language server `xml-lsp` (crates `xml-core`, `xsd-core`, `xml-lsp`) replacing LemMinX (#1): well-formedness diagnostics, formatting, completion with automatic closing tags, document symbols, hover, go to definition and references, incremental synchronization.
- XSD support: `xsi:schemaLocation`/`xsi:noNamespaceSchemaLocation` resolution, recursive `xs:include`/`xs:import`, schema cache, validation of content models (sequence, choice, all, groups, extensions, references, wildcards), attributes (required, fixed, attribute groups), built-in and restricted simple types (length, pattern, bounds, digits, enumerations, whitespace, lists and unions), nillable and fixed elements; schema-aware completion of elements, attributes and enumeration values; revalidation of open documents when a schema changes.

[Unreleased]: https://github.com/NyeKo-ItL/zed-xml/compare/v0.9.0...HEAD
[0.9.0]: https://github.com/NyeKo-ItL/zed-xml/compare/v0.6.0...v0.9.0
[0.6.0]: https://github.com/NyeKo-ItL/zed-xml/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/NyeKo-ItL/zed-xml/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/NyeKo-ItL/zed-xml/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/NyeKo-ItL/zed-xml/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/NyeKo-ItL/zed-xml/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/NyeKo-ItL/zed-xml/releases/tag/v0.1.0
