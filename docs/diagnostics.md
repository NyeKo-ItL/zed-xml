# Diagnostics reference

Every diagnostic published by `xml-lsp` has a `source` of `xml-lsp`, a `code` and (for most) a `data` object with a `category` and a `kind` (or `rule`). **These identifiers are a stable API** (quick fixes, editor configurations and scripts rely on them): new ones are added, existing ones are never renamed or repurposed, and a removed rule keeps its identifier reserved.

Severities are `1` (error) unless a row says otherwise.

| `code` | `data.category` | What it reports |
|--------|-----------------|-----------------|
| `xml-syntax` | `xml` | Well-formedness problems of the markup (the tolerant checks and the strict XML 1.0 grammar check). |
| `xml-structure` | `xml` | Tag matching problems (mismatched, unmatched and unclosed elements). |
| `xsd-validation` | `xsd` | The document is invalid against its XSD, and the errors of the schema set it uses. |
| `xsd-schema` | `xsd` | Problems of an open schema document itself (schema representation constraints). |
| `dtd-grammar` | `xml` / `dtd` | Syntax and grammar errors of a DTD (internal subset, external subset, `.dtd` files). |
| `dtd-validation` | `dtd` | The document is invalid against its DTD. |
| `xml-entity` | `xml` | Entity reference problems (also without a DTD). |
| `xpath-syntax` | `xpath` | XPath syntax errors in XSLT stylesheets. |
| `no-grammar` | `xml` | The document has no grammar (severity set by `xml.validation.noGrammar`). |
| `doctype-disallowed` | `xml` | `<!DOCTYPE>` with `xml.validation.disallowDocTypeDecl`. |
| `catalog-target-missing` | `xml` | A local target of an open XML catalog does not exist (warning). |
| `large-file` | `xml` | The document exceeds `xml.maxFileSize`: only well-formedness is checked (information). |

## `xml-syntax` and `xml-structure` (`data.kind`)

Tolerant checks (precise ranges, quick fixes): `mismatchedEndTag`, `unmatchedEndTag`, `unclosedElement`, `unclosedTag`, `duplicateAttribute` (also for attributes with the same expanded name), `unquotedAttributeValue`, `unescapedCharacter`.

Namespaces in XML: `undeclaredPrefix`, `invalidQualifiedName`, `invalidNamespaceDeclaration`.

Strict XML 1.0 grammar (reported once the tolerant checks find nothing): `invalidCharacter`, `invalidName`, `invalidReference`, `malformedDeclaration` (XML declaration, DOCTYPE, attributes and tags), `malformedComment`, `malformedProcessingInstruction`, `malformedCData`, `contentOutsideRoot`, `missingRoot`.

## `xsd-validation` (`data.rule`)

`missingRoot`, `unknownRoot`, `unexpectedElement`, `unexpectedOrder`, `missingElement`, `tooManyElements`, `fixedValue`, `invalidContent`, `notNillable`, `unexpectedAttribute`, `missingAttribute`, `invalidEnumeration`, `invalidAttributeValue`, `duplicateId`, `unknownIdref`, `duplicateKey`, `missingKeyField`, `invalidKeyField`, `unknownKeyref`, `invalidXsiType` (`xsi:type` unknown, not derived from the declared type, blocked by `block`, or abstract; an element of an abstract type without `xsi:type`).

Errors of the schema set a document uses (unresolved references, invalid derivations, ...) are published with `data.kind` `loading` and the schema location in `data.schemaUri`.

## `xsd-schema` (`data.kind`)

`invalidSchemaElement`, `invalidSchemaAttribute`, `invalidSchemaValue`, `missingSchemaAttribute`, `missingSchemaContent`, `invalidSchemaAttributes`.

## `dtd-grammar` (`data.kind`)

`dtdSyntax`, `duplicateElement`, `duplicateNotation`, `multipleIdAttributes`, `idAttributeDefault`, `invalidDefaultValue`, `defaultEntityReference`, `properNesting`, `undeclaredParameterEntity`, `undeclaredNotation`, `entityRecursion`, `entityExpansionLimit`, `conditionalSection`, `externalLoad` (warning for a remote DTD), `externalGrammar` (summary of the errors of an external DTD).

## `dtd-validation` and `xml-entity` (`data.kind`)

`undefinedEntity`, `entityExpansion`, `unparsedEntityReference`, `externalEntityInAttribute`, `standaloneEntity`, `malformedEntity`, `externalEntity`, `expansionBudget`; `rootMismatch`, `undeclaredElement`, `undeclaredAttribute`, `missingAttribute`, `invalidAttributeValue`, `invalidEnumeration`, `fixedValue`, `duplicateId`, `unknownIdref`, `invalidEntityAttribute`, `emptyContent`, `unexpectedElement`, `incompleteContent`, `textNotAllowed`.
