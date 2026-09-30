# Configuration

`xml-lsp` reads LemMinX-style settings from an `xml` section. In Zed, write them under `lsp.xml-lsp.settings` in `settings.json` (user or project):

```json
{
  "lsp": {
    "xml-lsp": {
      "settings": {
        "xml": {
          "format": { "splitAttributes": "splitNewLine", "emptyElements": "collapse" },
          "validation": { "noGrammar": "hint" },
          "fileAssociations": [
            { "pattern": "**/*.project", "systemId": "schemas/project.xsd" }
          ]
        }
      }
    }
  }
}
```

The `xml` key is optional: `"settings": { "format": { … } }` is equivalent.

## How settings reach the server

1. At startup the extension sends `lsp.xml-lsp.initialization_options` as `initializationOptions` when it is set, otherwise the settings wrapped as `{"settings": {"xml": …}}`. The server accepts `{"settings": {"xml": …}}`, `{"xml": …}` or the content of the section.
2. After initialization, when the client supports it, the server requests `workspace/configuration` (section `xml`); the answer is merged over the initialization options.
3. `workspace/didChangeConfiguration` applies a pushed `xml` section, or asks for the configuration again when the notification carries none.

When validation settings, file associations or catalogs change (including a catalog file modified on disk), the diagnostics of every open document are re-published (cleared when validation is disabled). Missing keys, unknown values and values of the wrong type keep their default. The defaults reproduce the server's behaviour before settings existed.

## Reference

| Name | Type | Default | Description |
|------|------|---------|-------------|
| `xml.format.enabled` | boolean | `true` | Enable document and range formatting. |
| `xml.format.splitAttributes` | `"preserve"` \| `"splitNewLine"` \| `"alignWithFirstAttr"` (also `"none"`, `"indent"`, `"alignWithFirst"`, or a boolean) | `"preserve"` | Layout of start tags with at least two attributes: kept as written, one attribute per line indented one level deeper than the element, or aligned with the first attribute. |
| `xml.format.maxLineWidth` | number | `0` | Wrap attributes that would make a start tag line longer than this width onto continuation lines; `0` disables it. Text content is never wrapped. |
| `xml.format.preservedNewlines` | number | `0` | Maximum number of blank lines kept between elements. |
| `xml.format.closingBracketNewLine` | boolean | `false` | Put `>` / `/>` on its own line when `splitAttributes` spreads the attributes over several lines. |
| `xml.format.emptyElements` | `"ignore"` \| `"expand"` \| `"collapse"` | `"ignore"` | Turn `<a/>` into `<a></a>` (`expand`), or empty/whitespace-only `<a></a>` into `<a/>` (`collapse`). Document formatting only: range formatting changes whitespace only. |
| `xml.format.preserveAttributeLineBreaks` | boolean | `true` | Keep existing line breaks before attributes. With `splitAttributes: "preserve"` and no `maxLineWidth`, start tags are copied verbatim; `false` joins the attributes on the tag line with single spaces. (LemMinX defaults to `false`.) |
| `xml.format.tabSize` | number | editor value, else `2` | Indentation width, used when the formatting request does not provide `tabSize`. |
| `xml.format.insertSpaces` | boolean | editor value, else `true` | Indent with spaces, used when the request does not provide `insertSpaces`. |
| `xml.format.trimFinalNewlines` | boolean | editor value, else `true` | Keep a single final newline, used when the request does not provide it. |
| `xml.format.insertFinalNewline` | boolean | editor value, else `true` | Ensure a final newline, used when the request does not provide it. |
| `xml.format.trimTrailingWhitespace` | boolean | editor value, else `false` | Trim trailing whitespace in text and comments, used when the request does not provide it. |
| `xml.validation.enabled` | boolean | `true` | Publish diagnostics. `false` clears every diagnostic (well-formedness included). |
| `xml.validation.schema.enabled` | `"always"` \| `"never"` \| `"onValidSchema"` (or `xml.validation.schema` as a boolean) | `"always"` | XSD validation: always, never, or only when every referenced schema loads without error (schema loading errors are still reported). |
| `xml.validation.noGrammar` | `"ignore"` \| `"hint"` \| `"info"` \| `"warning"` | `"ignore"` | Severity of the `no-grammar` diagnostic on the root element of documents bound to no XSD, DTD, `<?xml-model?>` or file association (XSD files excluded). |
| `xml.validation.disallowDocTypeDecl` | boolean | `false` | Report every `<!DOCTYPE>` declaration as an error (`doctype-disallowed`); its DTD is then ignored (no DTD, entity or validation diagnostics). |
| `xml.validation.resolveExternalEntities` | boolean | `false` | Resolve the external general entities (`<!ENTITY chap SYSTEM "chap.xml">`) referenced by the document: an entity whose file cannot be found (through catalogs, then relative to its declaring DTD) is reported as a warning (`xml-entity` / `externalEntity`). Their content is never read, and remote entities are never downloaded. The external DTD subset and external parameter entities are part of the grammar and are always loaded (local files only), like LemMinX. |
| `xml.completion.autoCloseTags` | boolean | `true` | Offer the matching end tag after typing `>`. |
| `xml.validation.debounce` | number (milliseconds) | `200` | Delay between the last change of a document and its validation: a burst of keystrokes is validated once, on its last version. Opening a document validates it at once; `0` validates every change (outdated results are still dropped). Capped at `10000`. |
| `xml.maxFileSize` | number (bytes) \| `null` | `10485760` (10 MiB) | Documents larger than this are only checked for well-formedness: no XSD/DTD validation, and no document symbols, folding ranges, selection ranges, document links, code actions or colors (their requests answer empty results). An information diagnostic (`large-file`, `data.kind` `largeFile`) says so at the start of the document. Highlights, linked editing, hover, completion, go to definition and formatting keep working. `0` or `null` removes the limit. |
| `xml.symbols.enabled` | boolean | `true` | Serve document symbols (outline). |
| `xml.symbols.maxItemsComputed` | number | unlimited | Maximum number of document symbols, counted in document order (children included). |
| `xml.colors.enabled` | boolean | `true` | Serve document colors (SVG, CSS, Android). |
| `xml.catalogs` | string[] | `[]` | OASIS XML catalog files used to resolve schema locations, namespaces and DTD identifiers. See below. |
| `xml.autoDetectCatalogs` | boolean | `false` | Extension to LemMinX: also use `catalog.xml` at the root of each workspace folder when it is an OASIS catalog. |
| `xml.fileAssociations` | `{ "pattern": string, "systemId": string }[]` | `[]` | Validate files matching `pattern` with the XSD `systemId` when they declare no `xsi:schemaLocation`/`xsi:noNamespaceSchemaLocation`. See below. |
| `xml.maxFileSize` | number (bytes) | `16777216` (16 MiB) | Extension to LemMinX: documents larger than this only get the light features (completion, tag highlights, linked editing). Diagnostics, symbols, folding, links, colors, selection ranges, code actions, formatting, hover, go to definition, references and rename are skipped, and a single `file-too-large` information diagnostic says so. `0` removes the limit (the XML parser still refuses documents over 16 MiB). See [Security model](#security-model). |

### XML catalogs

`xml.catalogs` lists OASIS XML Catalogs 1.1 files, like LemMinX. Each entry is an absolute path, a `file://` URI, `~/…` (home directory) or a path relative to the first workspace folder that contains it (the first folder otherwise). Catalogs are consulted in order.

```json
{ "lsp": { "xml-lsp": { "settings": { "xml": { "catalogs": ["catalog.xml", "~/xml/catalog.xml"] } } } } }
```

```xml
<catalog xmlns="urn:oasis:names:tc:entity:xmlns:xml:catalog" prefer="public">
  <!-- Namespace (xsi:schemaLocation pair, xs:import without schemaLocation). -->
  <uri name="http://maven.apache.org/POM/4.0.0" uri="schemas/maven-4.0.0.xsd"/>
  <!-- Exact location (xsi:noNamespaceSchemaLocation, xs:include, DOCTYPE SYSTEM). -->
  <system systemId="http://example.com/schemas/project.xsd" uri="schemas/project.xsd"/>
  <!-- Every location under a prefix; the longest matching prefix wins. -->
  <rewriteSystem systemIdStartString="http://www.springframework.org/schema/" rewritePrefix="spring/"/>
  <!-- DOCTYPE PUBLIC identifier. -->
  <public publicId="-//OASIS//DTD DocBook XML V4.5//EN" uri="docbook/docbookx.dtd"/>
  <group xml:base="vendor/">
    <uriSuffix uriSuffix="/common.xsd" uri="common.xsd"/>
  </group>
  <nextCatalog catalog="more/catalog.xml"/>
</catalog>
```

- Supported entries: `system`, `public`, `uri`, `rewriteSystem`, `rewriteURI`, `systemSuffix`, `uriSuffix`, `delegatePublic`, `delegateSystem`, `delegateURI` and `nextCatalog`, inside `catalog` and `group`. Relative targets are resolved against `xml:base` (allowed on any element), else against the catalog file. `prefer` (on `catalog`/`group`, `public` by default like Xerces) decides whether `public` entries apply when a system identifier is also given. Elements from other namespaces are ignored with their content.
- Resolution follows the specification: in each catalog, an exact match, then the longest `rewrite*` prefix, then the longest `*Suffix`, then delegation (the matching `delegate*` catalogs, longest prefix first, replace the catalog list); otherwise the catalog's `nextCatalog` entries are consulted (depth first, each catalog at most once, so cycles are harmless), then the next catalog. `urn:publicid:` identifiers are unwrapped.
- Schema locations: like Xerces/LemMinX, the namespace of an `xsi:schemaLocation` pair or of an `xs:import` is looked up first among `uri` entries, then the location among `system` entries, then among `uri` entries. `xs:include` locations, `xsi:noNamespaceSchemaLocation` and `xml.fileAssociations` system IDs use the location only. A `<!DOCTYPE>` uses its public and system identifiers, first `system` then `public` entries (document links, DTD loading and validation); so do external parameter entities (`<!ENTITY % mod PUBLIC "…" "…">`).
- A remote location (`http(s)://…`) that no catalog maps to a local file is not downloaded: it is reported as a warning asking for a catalog entry.
- Catalog files (and the catalogs they reach through `nextCatalog`/`delegate*`) are cached by modification time and size, re-read when they change on disk, and watched through `workspace/didChangeWatchedFiles` when the client supports dynamic registration; open documents are then revalidated. Unreadable or invalid catalogs are skipped (logged to the server's stderr).
- An open catalog file gets `catalog-target-missing` warnings on `uri`/`catalog` values whose local file is missing (and `rewritePrefix` directories ending with `/`).
- `xml.autoDetectCatalogs` is off by default so that a `catalog.xml` which happens to sit in a project does not silently change schema resolution.

### File associations

- `pattern` is a glob: `*` and `?` match within a path segment, `**` matches any number of segments, `{a,b}` lists alternatives. A pattern without `/` matches the file name (`*.project`); otherwise it is matched against the path relative to the workspace folder (`config/**/*.xml`), then against the absolute path.
- `systemId` is a path relative to the workspace folder (or to the document's directory outside a workspace), an absolute path or a `file://` URI. Remote URLs are used only when an XML catalog maps them to a local file.
- The associated schema is used for diagnostics, completion, hover and code actions, and documents are revalidated when the open schema changes.

## DTD

A document with a `<!DOCTYPE>` is validated against its DTD, like LemMinX:

- The internal subset is read first, then the external subset (`SYSTEM "…"` or `PUBLIC "…" "…"`), so internal declarations take precedence (first entity/attribute declaration wins). The external DTD and external parameter entities are resolved through `xml.catalogs`, then as `file://` URIs, absolute paths or paths relative to the declaring file; open editor buffers are preferred over the disk. Remote identifiers (`http(s)://…`) are never downloaded: map them in a catalog (a warning says so), e.g. `<public publicId="-//W3C//DTD XHTML 1.0 Strict//EN" uri="dtd/xhtml1-strict.dtd"/>`. DTD files are cached by modification time and size and limited to 4 MiB.
- Diagnostics codes: `dtd-grammar` (DTD syntax and grammar errors — duplicate elements, several `ID` attributes, undeclared notation, entity recursion or expansion limit —, loading failures, and a summary on the DOCTYPE system identifier for errors inside an external DTD), `xml-entity` (`&name;` references that are undeclared, unparsed, recursive, too large, or external/containing `<` in an attribute value; checked in every document), `dtd-validation` (root element name, undeclared elements and attributes, content models, required/fixed/enumerated attributes, `ID`/`IDREF(S)`, `NMTOKEN(S)`, `ENTITY/ENTITIES`). `data.kind` carries a stable identifier (`undefinedEntity`, `unexpectedElement`, `missingAttribute`, …).
- When the external DTD cannot be loaded, the document is not validated against it and undeclared entity references are warnings. A DTD that declares no element (e.g. only `<!ENTITY nbsp "&#160;">`) only defines entities: the document structure is not checked. Undeclared `xmlns`, `xmlns:*`, `xml:*` and `xsi:*` attributes are accepted so that documents also bound to an XSD validate with both grammars. The content of an element that references an entity containing markup (or an external entity) is not checked against its model.
- `.dtd` and `.ent` files use the DTD language: syntax errors, completion, hover, go to definition and document symbols; the XML formatter does not touch them.
- Entity expansion is never materialized for general entities (only their size is computed) and is limited to 1 MiB per entity, 4 MiB of parameter-entity replacement text and 32 nesting levels. Attribute values referencing entities are normalized within a budget of 8 MiB per document (and per DTD for attribute defaults); beyond it, one `dtd-validation` / `expansionBudget` diagnostic says that the remaining values are not checked. Content models are matched within a budget of about 2.7·10⁸ automaton steps per document. See [Security model](#security-model).

## Security model

`xml-lsp` analyses files that may come from untrusted sources (a cloned repository, a downloaded document). Opening such a file must not let it read arbitrary data into the editor, reach the network or exhaust the machine. The rules below apply to every feature; limits are fixed unless a setting is named. To report a vulnerability, see [SECURITY.md](../SECURITY.md).

### No network access

- The server never opens a network connection and has no HTTP client. Remote locations (`http://`, `https://`, `ftp://` and any other non-`file:` scheme) in `xsi:schemaLocation`, `xs:include`/`xs:import`/`xs:redefine`, `<!DOCTYPE>` identifiers, external parameter entities, `xi:include`, XSLT imports, `<?xml-stylesheet?>`/`<?xml-model?>`, `xml.fileAssociations` and catalog entries are only resolved through `xml.catalogs` to local files; otherwise they are reported (a warning for schemas and DTDs) or shown as plain links that the editor opens.
- Network file-system paths are treated as remote too and are never read or even probed: UNC paths (`\\server\share\…`, `//server/share/…`), Windows device paths (`\\.\…`, `\\?\UNC\…`) and `file://host/…` URIs naming a host other than `localhost`. This prevents a document from triggering SMB/WebDAV connections (and the credential leaks that come with them on Windows).

### Local files

- A document may reference any local file through a relative or absolute path, like in LemMinX and IntelliJ: resolving `../common/types.xsd` is the normal case. What the server reads is limited to what it needs to parse schemas, DTDs and catalogs, and it only reports what those grammars say (diagnostics, completion, hover); nothing is written.
- Only regular files are read. Symbolic links are followed, but a link to a device (`/dev/zero`, `/dev/random`), a FIFO, a socket or a directory is refused, so a document cannot block the server or stream endless data into it.
- Sizes are checked before and while reading: schemas, catalogs and other referenced resources at most 16 MiB, DTDs and external parameter entities 4 MiB, workspace files indexed for workspace symbols 4 MiB. Invalid UTF-8 is refused.
- External general entities (`<!ENTITY chap SYSTEM "chap.xml">`) are never read, only checked for existence with `xml.validation.resolveExternalEntities`. `xml.validation.disallowDocTypeDecl` rejects DOCTYPEs altogether.
- The workspace scan behind workspace symbols skips hidden directories, `target/` and `node_modules/`, does not follow symbolic links, and stops after 100 000 entries or 5000 files.

### Bounded work

| What | Limit |
|------|-------|
| Expansion of one general entity ("billion laughs") | 1 MiB (computed, never materialized) |
| Parameter-entity replacement text per grammar | 4 MiB |
| Entity and conditional section nesting | 32 levels |
| Sources (files, replacement texts) of one DTD | 256 |
| Entity text expanded into attribute values ("quadratic blowup") | 8 MiB per document, 8 MiB of attribute defaults per DTD |
| Content model matching | about 2.7·10⁸ automaton steps per document |
| Content model group nesting in a DTD | 64 levels |
| Schema documents loaded for one document (`xs:include`/`xs:import` graph, cycles included) | 256 |
| Catalog files loaded (`nextCatalog`, `delegate*`) | 64, delegation depth 16 |
| XSD nesting depth | 256 levels (deeper schemas are reported as invalid) |
| XML nesting depth reported by well-formedness checks | 512 levels |
| Documents analysed as a whole | `xml.maxFileSize` (16 MiB by default) |
| Formatted output | 8 times the input, at least 64 MiB (deeply nested siblings would otherwise format into gigabytes) |
| Selection range chain | 256 ranges (innermost ranges and the whole document) |
| Quoted values and name lists in diagnostic messages | 80 characters, 50 names |

Deeply nested documents (100 000 levels and more) are handled without recursion: parsers, the tag tree, namespace resolution, the formatter, document symbols (nested up to 128 levels, flat beyond) and selection ranges are iterative or bounded, and the LSP answers never nest deeper than a few hundred JSON levels. A smoke test runs every request over generated hostile documents to keep it so.
