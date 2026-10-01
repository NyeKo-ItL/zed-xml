# User guide

What to do with `zed-xml`, by task. The [README](../README.md) lists every feature and the [configuration reference](configuration.md) every setting; this guide shows how they fit together. Zed settings go in `settings.json` (user or project); server settings go under `lsp.xml-lsp.settings` (the `xml` key is optional).

## 1. Open a file

Open any file of a [supported type](../README.md#file-types): it opens as `XML` (or `DTD` for `.dtd`/`.ent`) and the server starts. The first start downloads the `xml-lsp` binary for your platform and verifies its checksum; `zed: open log` shows what happens.

Without any setting you get, as you type:

- **Well-formedness and Namespaces in XML diagnostics**: mismatched, unclosed or stray tags, duplicate attributes, undeclared prefixes, bad characters, bad declarations. The lightbulb offers quick fixes (rename the end tag, close the element, quote a value, escape `&`, declare a well-known prefix such as `xsi` or `xsl` on the root element).
- **Editing help**: automatic closing tags, matching tag highlight, linked editing of start and end tags (`"linked_edits": true`), folding, selection extension, document outline.

## 2. Validate against a schema (XSD)

The server binds a schema through:

1. `xsi:schemaLocation` / `xsi:noNamespaceSchemaLocation` in the document (relative, absolute or `file://` paths).
2. An `xml.fileAssociations` entry, for files that declare nothing:

```json
{
  "lsp": {
    "xml-lsp": {
      "settings": {
        "xml": {
          "fileAssociations": [
            { "pattern": "**/*.project", "systemId": "schemas/project.xsd" }
          ]
        }
      }
    }
  }
}
```

Once a schema is bound you get validation (structure, attributes, datatypes and facets, identity constraints, `xsi:type`, `xsi:nil`), completion of elements, attributes and enumerated values, hover with the schema documentation, go to definition on the schema and on declarations, and `ID`/`IDREF`/`keyref` navigation. Editing the schema revalidates the open documents that use it, and the schema itself is checked as you edit it (`xsd-schema` diagnostics).

To see documents that have no grammar at all, set `xml.validation.noGrammar` to `"hint"`, `"info"` or `"warning"`.

### Remote schemas

The server never downloads anything. A schema referenced by an `http(s)://` location is reported as a warning until an [XML catalog](configuration.md#xml-catalogs) maps it to a local file:

```xml
<!-- catalog.xml -->
<catalog xmlns="urn:oasis:names:tc:entity:xmlns:xml:catalog">
  <uri name="http://maven.apache.org/POM/4.0.0" uri="schemas/maven-4.0.0.xsd"/>
  <system systemId="http://example.com/schemas/project.xsd" uri="schemas/project.xsd"/>
</catalog>
```

```json
{ "lsp": { "xml-lsp": { "settings": { "xml": { "catalogs": ["catalog.xml"] } } } } }
```

The schema of the XML namespace (`xml:lang`, `xml:space`, …), imported by many schemas from `http://www.w3.org/2001/xml.xsd`, needs no catalog entry: the server carries a copy. For any other schema, download it once (`curl -O`) into the catalog's directory, next to `catalog.xml`. `xml.autoDetectCatalogs` also uses a `catalog.xml` at the root of each workspace folder.

## 3. Validate against a DTD

A `<!DOCTYPE>` is enough: the internal subset and the external DTD (`SYSTEM`/`PUBLIC`, resolved through catalogs, then relative to the document) are read, and the document is validated (content models, attributes, `ID`/`IDREF`, entities). `.dtd` files get syntax diagnostics, completion, hover and navigation. A remote DTD needs a catalog entry, like a schema.

## 4. Format

Make sure formatting goes through the server:

```json
{ "languages": { "XML": { "formatter": "language_server" } } }
```

Then `editor: format` (document) or format selection (region). The defaults keep text intact and only change whitespace between elements; tune the layout with `xml.format.*` (attribute splitting and wrapping, blank lines, empty elements, quote style, `<a />` spacing). Formatting is idempotent, and whitespace in mixed content (text next to elements) is kept as written. Regions that are not well-formed are left untouched by range formatting.

## 5. Navigate and refactor

| Want to | Use |
|---------|-----|
| Jump to a schema, `xs:include`, `xsl:import`, DOCTYPE | go to definition on the location |
| Jump from an `IDREF` / `keyref` value to its target | go to definition |
| Rename an element (both tags), a prefix, or an XSD component everywhere | rename symbol |
| Find a global XSD component in the project | project symbols |
| See the structure | outline; fold with `"document_folding_ranges": "on"` |
| Preview colours in SVG / CSS / Android XML | swatches (`lsp_document_colors`) |

## 6. XSLT

Stylesheets (root in the XSLT namespace, or simplified stylesheets) get XPath 1.0/2.0/3.1 syntax checking in `select`, `test`, `match`, attribute value templates and text value templates, completion of XSLT elements and attributes, and navigation, rename and references for templates, modes, functions, keys and variables across `xsl:include` / `xsl:import`. XPath is not evaluated.

## 7. Large files

Documents above `xml.maxFileSize` (10 MiB by default) only get well-formedness diagnostics and the light features; an information diagnostic says so. Validation is debounced while you type (`xml.validation.debounce`, 200 ms by default). If a huge generated file is slow, raise the debounce or turn validation off for it (`xml.validation.enabled`).

## 8. Fixing common problems

| Symptom | Check |
|---------|-------|
| No validation or completion | The document declares a schema, or an `xml.fileAssociations` entry matches it; the schema is a local file (or mapped by a catalog). |
| A warning on a schema location | The location is remote: add a catalog entry. |
| Formatting does nothing | `"formatter": "language_server"`; `xml.format.enabled` is not `false`. |
| A suffix opens in another language | Another extension claims it: map it with `file_types` (see the README). |
| The server does not start | `zed: open log`; set `XML_LSP_PATH` to a local build; `editor: restart language server`. |

More in the README [Troubleshooting](../README.md#troubleshooting) section.

## 9. What this extension does not do

- **Download schemas or DTDs**: no network access at all; map remote locations with a catalog (see section 2).
- **Validate against RELAX NG, Schematron or XSD 1.1** (`xs:assert`, `xs:alternative`); `.rng` files are highlighted and checked for well-formedness only.
- **Evaluate XPath or run XSLT transformations**; XPath is only checked for syntax.
- **Generate** XSD, XML samples or code from a schema.

The first two follow from the [security model](configuration.md#security-model): the server never opens a connection and never runs external programs. The others are on the roadmap after 1.0.
