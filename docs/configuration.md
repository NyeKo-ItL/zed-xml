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

When validation settings, file associations or catalogs change, the diagnostics of every open document are re-published (cleared when validation is disabled). Missing keys, unknown values and values of the wrong type keep their default. The defaults reproduce the server's behaviour before settings existed.

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
| `xml.validation.disallowDocTypeDecl` | boolean | `false` | Report every `<!DOCTYPE>` declaration as an error (`doctype-disallowed`). |
| `xml.validation.resolveExternalEntities` | boolean | `false` | Reserved for DTD support: resolve external entities. |
| `xml.completion.autoCloseTags` | boolean | `true` | Offer the matching end tag after typing `>`. |
| `xml.symbols.enabled` | boolean | `true` | Serve document symbols (outline). |
| `xml.symbols.maxItemsComputed` | number | unlimited | Maximum number of document symbols, counted in document order (children included). |
| `xml.colors.enabled` | boolean | `true` | Serve document colors (SVG, CSS, Android). |
| `xml.catalogs` | string[] | `[]` | XML catalog files used for schema resolution. |
| `xml.fileAssociations` | `{ "pattern": string, "systemId": string }[]` | `[]` | Validate files matching `pattern` with the XSD `systemId` when they declare no `xsi:schemaLocation`/`xsi:noNamespaceSchemaLocation`. See below. |

### File associations

- `pattern` is a glob: `*` and `?` match within a path segment, `**` matches any number of segments, `{a,b}` lists alternatives. A pattern without `/` matches the file name (`*.project`); otherwise it is matched against the path relative to the workspace folder (`config/**/*.xml`), then against the absolute path.
- `systemId` is a path relative to the workspace folder (or to the document's directory outside a workspace), an absolute path or a `file://` URI. Remote URLs are ignored.
- The associated schema is used for diagnostics, completion, hover and code actions, and documents are revalidated when the open schema changes.
