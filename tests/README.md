# Testing

The tests check `xml-core`, `xsd-core` and `xml-lsp` against the XML and XML
Schema specifications, against other parsers' test suites, and against
documents people actually edit.

```sh
cargo test --workspace                       # everything that needs no download
scripts/fetch-test-suites.sh                 # fetch the pinned W3C and libxml2 suites
cargo test --release -p xml-conformance      # now also runs the external suites
BLESS=1 cargo test --release -p xml-conformance   # rewrite the baselines after a fix
```

## What runs

| Test | Location | Input | Checks |
|------|----------|-------|--------|
| Unit tests | `crates/*/src`, `crates/*/tests` | inline snippets, `tests/fixtures/{xml,xsd}` | each feature in isolation |
| Specification cases | `crates/xml-conformance/tests/spec_cases.rs` | ~200 one-rule cases citing XML 1.0 5th ed., Namespaces 1.0 3rd ed., RFC 7303, XSD 1.0 Parts 1 and 2 | well-formedness, namespace constraints, every built-in datatype, facets, content models, identity constraints |
| Real-world documents | `crates/xml-conformance/tests/real_world.rs` | `tests/fixtures/real-world` (Maven, Android, .NET, Tomcat, Ant, Log4j, CXF WSDL, SVG, XSLT, WebDAV, RSS/RDF, XHTML, encodings, and spec-modelled Atom, SOAP, XML-DSig, XMPP, plist, XLIFF, KML, GPX, DocBook, UBL, EPUB…) | decoded and well-formed; formatting is idempotent and keeps the content (compared with roxmltree); schema-paired documents validate and a broken copy does not |
| Parser corpora | `crates/xml-conformance/tests/corpus.rs` | `tests/fixtures/corpus` (roxmltree, libxml2 `test/errors`) | well-formedness verdicts from `expectations.txt` |
| W3C XML conformance | `crates/xml-conformance/tests/xmlconf.rs` | xmlts 20130923 (fetched) | non-validating processor rules of XML 1.0 §5.1 |
| W3C XSD test suite | `crates/xml-conformance/tests/xsts.rs` | w3c/xsdtests (fetched) | schema and instance validity, XSD 1.0 expectations |
| libxml2 schema tests | `crates/xml-conformance/tests/libxml2_schemas.rs` | libxml2 `test/schemas` (fetched) | instance validity as reported by libxml2 |
| LSP smoke test | `crates/xml-lsp/src/fixture_smoke.rs` | every file in `tests/fixtures` | every request at positions spread through each document: no panic, every returned range inside the document, LSP formatting idempotent |

The external suites are skipped when they have not been fetched, except in
CI, where the `Conformance suites` job sets `XML_TEST_SUITES_REQUIRED=1`.

## Baselines

Suites that the implementation does not fully pass yet compare their
failures with `crates/xml-conformance/baselines/<suite>.txt`:

- a case failing outside its baseline is a regression and fails the test;
- a baseline case that now passes also fails the test, so fixes are recorded:
  delete the line, or run with `BLESS=1` and review the diff.

The baselines are the to-do list of the conformance work: every roadmap item
(XSD datatypes, identity constraints, DTD support, namespace constraints…)
should delete lines from them.

## Adding fixtures

- Real-world documents go in `tests/fixtures/real-world/<domain>/`. Record
  the source, revision and licence in `tests/fixtures/SOURCES.md` and add the
  licence text to `tests/fixtures/licenses/` when it is new. Only use
  licences compatible with MIT (MIT, BSD, ISC, Apache-2.0, W3C software
  licence). A document with a schema also gets an entry in `SCHEMA_CASES`.
- A corpus file needs a line in its `expectations.txt`.
- A specification case goes in the matching table of `spec_cases.rs`, named
  after the section it exercises, with the result the specification
  requires, even when the implementation does not meet it yet (then add it
  to the baseline).

## References

- XML 1.0 fifth edition: <https://www.w3.org/TR/xml/>
- Namespaces in XML 1.0 third edition: <https://www.w3.org/TR/xml-names/>
- XML Schema 1.0 Part 1 (Structures) and Part 2 (Datatypes):
  <https://www.w3.org/TR/xmlschema-1/>, <https://www.w3.org/TR/xmlschema-2/>
- RFC 7303, XML Media Types (encoding detection):
  <https://www.rfc-editor.org/rfc/rfc7303>
- RFC 3470, Guidelines for the Use of XML within IETF Protocols:
  <https://www.rfc-editor.org/rfc/rfc3470>
- OASIS XML Catalogs 1.1: <https://www.oasis-open.org/committees/download.php/14809/xml-catalogs.html>
- Language Server Protocol 3.17:
  <https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/>
- Test suites used by comparable projects: W3C xmlts (libxml2, Expat, Xerces,
  saxes), w3c/xsdtests (Xerces, Saxon, libxml2), libxml2 `test/`, roxmltree
  `tests/files`, and Eclipse LemMinX's scenario tests for the LSP features.
