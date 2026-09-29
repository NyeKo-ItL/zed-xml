# Fixture sources and licences

Every third-party file in `tests/fixtures` is copied unmodified from the
revision below and keeps its original licence; the licence texts are in
`licenses/`. Files not listed here were written for this repository and are
covered by the repository licence (MIT).

## `real-world/`

| Files | Source | Revision | Licence |
|-------|--------|----------|---------|
| `maven/commons-lang-pom.xml` | [apache/commons-lang](https://github.com/apache/commons-lang) `pom.xml` | `7b6048b7a9d1` | Apache-2.0 |
| `maven/spring-petclinic-pom.xml` | [spring-projects/spring-petclinic](https://github.com/spring-projects/spring-petclinic) `pom.xml` | `500158f73241` | Apache-2.0 |
| `maven/maven-4.0.0.xsd` | [apache/maven-site](https://github.com/apache/maven-site) `content/resources/xsd/maven-4.0.0.xsd` | `75816eac1598` | Apache-2.0 |
| `android/AndroidManifest.xml`, `android/values-*.xml` | [android/nowinandroid](https://github.com/android/nowinandroid) `app/src/main/` | `a49ed253d75e` | Apache-2.0 |
| `android/drawable-vector.xml` | [android/nowinandroid](https://github.com/android/nowinandroid) `core/designsystem/src/main/res/drawable/core_designsystem_ic_placeholder_default.xml` | `a49ed253d75e` | Apache-2.0 |
| `dotnet/System.Text.Json.csproj`, `dotnet/Directory.Build.props`, `dotnet/NuGet.config` | [dotnet/runtime](https://github.com/dotnet/runtime) | `754e940bff6c` | MIT |
| `java/tomcat-web.xml`, `java/tomcat-server.xml` | [apache/tomcat](https://github.com/apache/tomcat) `conf/` | `2a7e82c65296` | Apache-2.0 |
| `java/ant-build.xml` | [apache/ant](https://github.com/apache/ant) `build.xml` | `233e220ee2c7` | Apache-2.0 |
| `java/log4j2-test1.xml` | [apache/logging-log4j2](https://github.com/apache/logging-log4j2) `log4j-core-test/src/test/resources/log4j-test1.xml` | `d9a2c4ea9193` | Apache-2.0 |
| `soap/cxf-CustomerService.wsdl` | [apache/cxf](https://github.com/apache/cxf) `distribution/src/main/release/samples/wsdl_first/src/main/resources/CustomerService.wsdl` | `27fa5a47ef5c` | Apache-2.0 |
| `svg/lucide-*.svg` | [lucide-icons/lucide](https://github.com/lucide-icons/lucide) `icons/` | `5a92b9ba262d` | ISC |
| `xslt/docbook-admon.xsl` | [docbook/xslt10-stylesheets](https://github.com/docbook/xslt10-stylesheets) `xsl/html/admon.xsl` | `efd62655c11c` | DocBook XSL licence (MIT-style, `licenses/docbook-xsl.txt`) |
| `svg/libxml2-svg*.svg`, `webdav/libxml2-dav*.xml`, `feeds/libxml2-*.rdf`, `xhtml/libxml2-xhtml1.xhtml`, `misc/libxml2-*.xml`, `encodings/*` | [GNOME/libxml2](https://gitlab.gnome.org/GNOME/libxml2) `test/` (`svg1`, `dav*`, `slashdot.rdf`, `rdf1`, `xhtml1`, `wml.xml`, `p3p`, `dia1`, `utf16*.xml`, `utf8bom.xml`, `japancrlf.xml`, `isolat1`) | `c43dc98d27ac` | MIT |
| `specs/*` | Written for this repository, each modelled on the specification cited in its leading comment (XML Schema Primer, RFC 4287, RSS 2.0, sitemaps.org, SOAP 1.2 Primer, RFC 3275, RFC 6120, RFC 4918, Apple property lists, XLIFF 2.0, KML 2.2, GPX 1.1, DocBook 5, XInclude, JUnit XML, XSLT 3.0, XML 1.0 §2.8, UBL 2.1, EPUB 3.3, XHTML with MathML and SVG) | | MIT |

## `corpus/`

| Directory | Source | Revision | Licence |
|-----------|--------|----------|---------|
| `roxmltree/` | [RazrFalcon/roxmltree](https://github.com/RazrFalcon/roxmltree) `tests/files/*.xml` (crate 0.20.0) | 0.20.0 | MIT or Apache-2.0 |
| `libxml2-errors/` | [GNOME/libxml2](https://gitlab.gnome.org/GNOME/libxml2) `test/errors/` | `c43dc98d27ac` | MIT |

Each corpus has an `expectations.txt` giving the well-formedness verdict of
every file, derived from the upstream expected results.

## External suites (not vendored)

`scripts/fetch-test-suites.sh` downloads these pinned suites into
`target/test-suites`:

| Suite | Source | Revision | Licence |
|-------|--------|----------|---------|
| W3C XML Conformance Test Suite 20130923 | npm [`xml-conformance-suite`](https://www.npmjs.com/package/xml-conformance-suite) 1.2.0 | SHA-256 pinned | W3C Software Notice and License |
| W3C XML Schema test suite | [w3c/xsdtests](https://github.com/w3c/xsdtests) | `7bc3365c652a` | W3C Document Notice and License |
| libxml2 schema tests | [GNOME/libxml2](https://gitlab.gnome.org/GNOME/libxml2) `test/schemas`, `result/schemas` | `c43dc98d27ac` | MIT |
