//! Hand-written cases derived from the specifications, one rule per case,
//! each named after the section it exercises:
//!
//! - XML 1.0 fifth edition (<https://www.w3.org/TR/xml/>), cited as `xml §`;
//! - Namespaces in XML 1.0 third edition
//!   (<https://www.w3.org/TR/xml-names/>), cited as `ns §`;
//! - RFC 7303 (XML media types) for documents an editor has already decoded;
//! - XML Schema 1.0 Part 2: Datatypes and Part 1: Structures
//!   (<https://www.w3.org/TR/xmlschema-2/>, `xsd2 §`;
//!   <https://www.w3.org/TR/xmlschema-1/>, `xsd1 §`).
//!
//! Every case states the result the specification requires. Cases the
//! implementation does not meet yet are listed in `baselines/spec-*.txt`, so
//! each roadmap item shows up as baseline lines to delete.

use xml_conformance::{Outcome, SuiteRun, guarded, well_formedness_errors};
use xsd_core::{parse_xsd, validate_document_located};

const WF: bool = true;
const NOT_WF: bool = false;

/// `(case, document, well-formed)`.
const XML_CASES: &[(&str, &str, bool)] = &[
    // Documents (§2.1).
    ("xml §2.1 [1] empty document", "", NOT_WF),
    (
        "xml §2.1 [1] prolog without root element",
        "<?xml version=\"1.0\"?>\n<!-- c -->",
        NOT_WF,
    ),
    ("xml §2.1 [1] two root elements", "<a/><b/>", NOT_WF),
    (
        "xml §2.1 [1] text after the root element",
        "<a/>text",
        NOT_WF,
    ),
    (
        "xml §2.1 [1] comment and PI after the root element",
        "<a/><!-- c --><?pi data?>\n",
        WF,
    ),
    ("xml §2.1 [1] whitespace-only misc", "\n\t <a/>\r\n ", WF),
    // Characters (§2.2).
    (
        "xml §2.2 [2] character reference to NUL",
        "<a>&#0;</a>",
        NOT_WF,
    ),
    (
        "xml §2.2 [2] character reference to a surrogate",
        "<a>&#xD800;</a>",
        NOT_WF,
    ),
    (
        "xml §2.2 [2] character reference to U+FFFE",
        "<a>&#xFFFE;</a>",
        NOT_WF,
    ),
    (
        "xml §2.2 [2] character reference to U+10FFFF",
        "<a>&#x10FFFF;</a>",
        WF,
    ),
    (
        "xml §2.2 [2] literal control character",
        "<a>\u{1}</a>",
        NOT_WF,
    ),
    (
        "xml §2.2 [2] literal tab, newline and carriage return",
        "<a>\t\n\r</a>",
        WF,
    ),
    ("xml §2.2 [2] non-BMP character", "<a>𝄞 😀</a>", WF),
    // Names (§2.3).
    ("xml §2.3 [5] name starting with a digit", "<1a/>", NOT_WF),
    ("xml §2.3 [5] name starting with a hyphen", "<-a/>", NOT_WF),
    (
        "xml §2.3 [5] name with dot, hyphen, underscore and digits",
        "<a.b-c_d9/>",
        WF,
    ),
    (
        "xml §2.3 [5] name with non-ASCII letters",
        "<élément größe=\"1\"/>",
        WF,
    ),
    ("xml §2.3 [5] name with a middle dot", "<a·b/>", WF),
    // Character data and markup (§2.4).
    ("xml §2.4 [14] ]]> in content", "<a>x ]]> y</a>", NOT_WF),
    ("xml §2.4 [14] > in content", "<a>x > y</a>", WF),
    (
        "xml §2.4 [14] bare ampersand in content",
        "<a>fish & chips</a>",
        NOT_WF,
    ),
    ("xml §2.4 [14] less-than in content", "<a>1 < 2</a>", NOT_WF),
    ("xml §2.4 [14] ]] without > in content", "<a>x ]] y</a>", WF),
    // Comments (§2.5).
    (
        "xml §2.5 [15] double hyphen inside a comment",
        "<a><!-- a -- b --></a>",
        NOT_WF,
    ),
    (
        "xml §2.5 [15] comment ending with --->",
        "<a><!-- a ---></a>",
        NOT_WF,
    ),
    ("xml §2.5 [15] empty comment", "<a><!----></a>", WF),
    (
        "xml §2.5 [15] markup inside a comment",
        "<a><!-- <b> & </c> --></a>",
        WF,
    ),
    // Processing instructions (§2.6).
    (
        "xml §2.6 [16] processing instruction",
        "<?xml-stylesheet href=\"a.xsl\" type=\"text/xsl\"?><a/>",
        WF,
    ),
    (
        "xml §2.6 [17] PI target xml in another case",
        "<a><?XmL data?></a>",
        NOT_WF,
    ),
    (
        "xml §2.6 [16] PI without target",
        "<a><? data?></a>",
        NOT_WF,
    ),
    (
        "xml §2.8 [23] XML declaration after whitespace",
        " <?xml version=\"1.0\"?><a/>",
        NOT_WF,
    ),
    (
        "xml §2.8 [23] XML declaration after a comment",
        "<!-- c --><?xml version=\"1.0\"?><a/>",
        NOT_WF,
    ),
    // CDATA sections (§2.7).
    (
        "xml §2.7 [18] CDATA section with markup",
        "<a><![CDATA[<b>&amp;</b>]]></a>",
        WF,
    ),
    (
        "xml §2.7 [18] empty CDATA section",
        "<a><![CDATA[]]></a>",
        WF,
    ),
    (
        "xml §2.7 [19] lower-case CDATA keyword",
        "<a><![cdata[x]]></a>",
        NOT_WF,
    ),
    (
        "xml §2.7 [18] unterminated CDATA section",
        "<a><![CDATA[x</a>",
        NOT_WF,
    ),
    // Prolog (§2.8, §4.3.3).
    (
        "xml §2.8 [23] XML declaration without version",
        "<?xml encoding=\"UTF-8\"?><a/>",
        NOT_WF,
    ),
    (
        "xml §2.8 [23] encoding before version",
        "<?xml encoding=\"UTF-8\" version=\"1.0\"?><a/>",
        NOT_WF,
    ),
    (
        "xml §2.9 [32] invalid standalone value",
        "<?xml version=\"1.0\" standalone=\"maybe\"?><a/>",
        NOT_WF,
    ),
    (
        "xml §2.8 [24] single-quoted declaration",
        "<?xml version='1.0' encoding='utf-8' standalone='yes'?><a/>",
        WF,
    ),
    (
        "xml §2.8 [26] version 1.x",
        "<?xml version=\"1.7\"?><a/>",
        WF,
    ),
    (
        "xml §2.8 [28] DOCTYPE after the root element",
        "<a/><!DOCTYPE a>",
        NOT_WF,
    ),
    (
        "xml §2.8 [28] two DOCTYPE declarations",
        "<!DOCTYPE a><!DOCTYPE a><a/>",
        NOT_WF,
    ),
    (
        "xml §2.8 [28] DOCTYPE with public identifier",
        "<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Strict//EN\" \"http://www.w3.org/TR/xhtml1/DTD/xhtml1-strict.dtd\"><html/>",
        WF,
    ),
    (
        "xml §2.8 [28] internal subset",
        "<!DOCTYPE a [<!ELEMENT a (#PCDATA)><!ATTLIST a b CDATA \"x\">]><a/>",
        WF,
    ),
    (
        "xml §2.8 [28] internal subset with > in a literal",
        "<!DOCTYPE a [<!ENTITY gt2 \">>\">]><a>&gt2;</a>",
        WF,
    ),
    (
        "xml §4.3.3 encoding declaration of an editor-decoded document",
        "<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><a>é</a>",
        WF,
    ),
    (
        "RFC 7303 §3 UTF-16 declaration on text decoded by the editor",
        "<?xml version=\"1.0\" encoding=\"UTF-16\"?><a/>",
        WF,
    ),
    // Elements and attributes (§3).
    ("xml §3 [39] element type match", "<a></b>", NOT_WF),
    ("xml §3 [39] end tag case must match", "<a></A>", NOT_WF),
    ("xml §3.1 [40] whitespace before >", "<a ></a >", WF),
    ("xml §3.1 [40] whitespace after <", "< a/>", NOT_WF),
    ("xml §3.1 [44] whitespace between / and >", "<a/ >", NOT_WF),
    (
        "xml §3.1 [41] unique attribute specification",
        "<a x=\"1\" x=\"2\"/>",
        NOT_WF,
    ),
    ("xml §3.1 [41] attribute without value", "<a x/>", NOT_WF),
    ("xml §3.1 [41] unquoted attribute value", "<a x=1/>", NOT_WF),
    (
        "xml §3.1 [10] less-than in an attribute value",
        "<a x=\"1 < 2\"/>",
        NOT_WF,
    ),
    (
        "xml §3.1 [10] greater-than in an attribute value",
        "<a x=\"2 > 1\"/>",
        WF,
    ),
    (
        "xml §3.1 [10] other quote inside an attribute value",
        "<a x='say \"hi\"' y=\"it's\"/>",
        WF,
    ),
    (
        "xml §3.1 [40] attributes without whitespace between them",
        "<a x=\"1\"y=\"2\"/>",
        NOT_WF,
    ),
    ("xml §3.1 [41] whitespace around =", "<a x = \"1\"/>", WF),
    // References (§4.1, §4.6).
    (
        "xml §4.6 predefined entities",
        "<a>&lt;&gt;&amp;&apos;&quot;</a>",
        WF,
    ),
    (
        "xml §4.1 [68] undeclared entity without DTD",
        "<a>&nbsp;</a>",
        NOT_WF,
    ),
    (
        "xml §4.1 [68] entity declared in the internal subset",
        "<!DOCTYPE a [<!ENTITY e \"x\">]><a>&e;</a>",
        WF,
    ),
    (
        "xml §4.1 WFC no recursion",
        "<!DOCTYPE a [<!ENTITY e \"&f;\"><!ENTITY f \"&e;\">]><a>&e;</a>",
        NOT_WF,
    ),
    (
        "xml §4.1 [66] hexadecimal character reference",
        "<a>&#x41;&#65;</a>",
        WF,
    ),
    (
        "xml §4.1 [66] empty hexadecimal character reference",
        "<a>&#x;</a>",
        NOT_WF,
    ),
    (
        "xml §4.1 [66] character reference without semicolon",
        "<a>&#65 </a>",
        NOT_WF,
    ),
    (
        "xml §4.1 [68] entity reference without semicolon",
        "<a>&amp </a>",
        NOT_WF,
    ),
    (
        "xml §4.1 [68] entity reference in an attribute value",
        "<a x=\"&lt;&#x20;&amp;\"/>",
        WF,
    ),
    (
        "xml §4.1 WFC no < in attribute values via entities",
        "<!DOCTYPE a [<!ENTITY e \"<\">]><a x=\"&e;\"/>",
        NOT_WF,
    ),
    // End-of-line handling (§2.11).
    (
        "xml §2.11 CRLF and lone CR line ends",
        "<a>\r\n<b/>\r<c/>\n</a>",
        WF,
    ),
];

/// `(case, document, namespace-well-formed)`.
const NAMESPACE_CASES: &[(&str, &str, bool)] = &[
    ("ns §5 undeclared element prefix", "<p:a/>", NOT_WF),
    (
        "ns §5 undeclared attribute prefix",
        "<a p:x=\"1\"/>",
        NOT_WF,
    ),
    (
        "ns §5 undeclared end-tag prefix",
        "<a xmlns:p=\"urn:p\"><p:b></q:b></a>",
        NOT_WF,
    ),
    (
        "ns §3 prefix declared on the same element",
        "<p:a xmlns:p=\"urn:p\" p:x=\"1\"/>",
        WF,
    ),
    (
        "ns §3 prefix declared on an ancestor",
        "<a xmlns:p=\"urn:p\"><b><p:c/></b></a>",
        WF,
    ),
    (
        "ns §3 prefix out of scope",
        "<a><b xmlns:p=\"urn:p\"/><p:c/></a>",
        NOT_WF,
    ),
    (
        "ns §3 NSC empty prefixed namespace name",
        "<a xmlns:p=\"\"/>",
        NOT_WF,
    ),
    (
        "ns §6.2 default namespace undeclared",
        "<a xmlns=\"urn:a\"><b xmlns=\"\"/></a>",
        WF,
    ),
    (
        "ns §3 NSC reserved prefix xml bound elsewhere",
        "<a xmlns:xml=\"urn:other\"/>",
        NOT_WF,
    ),
    (
        "ns §3 xml prefix bound to its own namespace",
        "<a xmlns:xml=\"http://www.w3.org/XML/1998/namespace\"/>",
        WF,
    ),
    (
        "ns §3 xml prefix without declaration",
        "<a xml:lang=\"en\" xml:space=\"preserve\"/>",
        WF,
    ),
    (
        "ns §3 NSC reserved prefix xmlns declared",
        "<a xmlns:xmlns=\"urn:x\"/>",
        NOT_WF,
    ),
    (
        "ns §3 NSC other prefix bound to the xml namespace",
        "<a xmlns:p=\"http://www.w3.org/XML/1998/namespace\"/>",
        NOT_WF,
    ),
    (
        "ns §3 NSC other prefix bound to the xmlns namespace",
        "<a xmlns:p=\"http://www.w3.org/2000/xmlns/\"/>",
        NOT_WF,
    ),
    (
        "ns §5 xmlns used as an element prefix",
        "<xmlns:a/>",
        NOT_WF,
    ),
    (
        "ns §6.3 duplicate expanded attribute names",
        "<a xmlns:p=\"urn:u\" xmlns:q=\"urn:u\" p:x=\"1\" q:x=\"2\"/>",
        NOT_WF,
    ),
    (
        "ns §6.3 same local name in different namespaces",
        "<a xmlns:p=\"urn:p\" xmlns:q=\"urn:q\" p:x=\"1\" q:x=\"2\" x=\"3\"/>",
        WF,
    ),
    (
        "ns §3 [7] name with two colons",
        "<a:b:c xmlns:a=\"urn:a\"/>",
        NOT_WF,
    ),
    (
        "ns §3 [7] name ending with a colon",
        "<a: xmlns:a=\"urn:a\"/>",
        NOT_WF,
    ),
    (
        "ns §3 [4] prefix starting with a digit",
        "<a xmlns:1p=\"urn:p\"/>",
        NOT_WF,
    ),
    (
        "ns §2 relative namespace URI (deprecated but well-formed)",
        "<a xmlns=\"relative/uri\"/>",
        WF,
    ),
];

/// `(case, built-in type, value, valid)`: one element of that type.
const DATATYPE_CASES: &[(&str, &str, &str, bool)] = &[
    ("xsd2 §3.2.2 boolean true", "boolean", "true", true),
    ("xsd2 §3.2.2 boolean 1", "boolean", "1", true),
    (
        "xsd2 §3.2.2 boolean is case-sensitive",
        "boolean",
        "TRUE",
        false,
    ),
    (
        "xsd2 §4.3.6 boolean whitespace is collapsed",
        "boolean",
        " true\n",
        true,
    ),
    ("xsd2 §3.2.3 decimal leading dot", "decimal", ".5", true),
    ("xsd2 §3.2.3 decimal trailing dot", "decimal", "5.", true),
    (
        "xsd2 §3.2.3 decimal explicit plus",
        "decimal",
        "+1.50",
        true,
    ),
    ("xsd2 §3.2.3 decimal exponent", "decimal", "1e3", false),
    ("xsd2 §3.2.4 float exponent", "float", "1.5E-10", true),
    ("xsd2 §3.2.4 float INF", "float", "-INF", true),
    ("xsd2 §3.2.4 float NaN", "float", "NaN", true),
    ("xsd2 §3.2.4 float lower-case inf", "float", "inf", false),
    ("xsd2 §3.2.5 double", "double", "6.02e23", true),
    ("xsd2 §3.2.5 double garbage", "double", "1.2.3", false),
    ("xsd2 §3.3.13 integer negative zero", "integer", "-0", true),
    (
        "xsd2 §3.3.13 integer with fraction",
        "integer",
        "1.0",
        false,
    ),
    ("xsd2 §3.3.13 integer empty", "integer", "", false),
    ("xsd2 §3.3.17 int maximum", "int", "2147483647", true),
    ("xsd2 §3.3.17 int overflow", "int", "2147483648", false),
    ("xsd2 §3.3.19 byte overflow", "byte", "128", false),
    (
        "xsd2 §3.3.24 unsignedByte negative",
        "unsignedByte",
        "-1",
        false,
    ),
    (
        "xsd2 §3.3.25 positiveInteger zero",
        "positiveInteger",
        "0",
        false,
    ),
    (
        "xsd2 §3.3.20 nonNegativeInteger zero",
        "nonNegativeInteger",
        "0",
        true,
    ),
    (
        "xsd2 §3.3.14 nonPositiveInteger",
        "nonPositiveInteger",
        "1",
        false,
    ),
    ("xsd2 §3.2.9 date", "date", "2026-09-29", true),
    (
        "xsd2 §3.2.9 date with time zone",
        "date",
        "2026-09-29+02:00",
        true,
    ),
    ("xsd2 §3.2.9 date February 30", "date", "2026-02-30", false),
    (
        "xsd2 §3.2.9 date without zero padding",
        "date",
        "2026-9-29",
        false,
    ),
    (
        "xsd2 §3.2.9 date February 29 of a leap year",
        "date",
        "2024-02-29",
        true,
    ),
    (
        "xsd2 §3.2.7 dateTime UTC",
        "dateTime",
        "2026-09-29T21:38:46Z",
        true,
    ),
    (
        "xsd2 §3.2.7 dateTime fractional seconds",
        "dateTime",
        "2026-09-29T21:38:46.123-05:00",
        true,
    ),
    (
        "xsd2 §3.2.7 dateTime 24:00:00",
        "dateTime",
        "2026-09-29T24:00:00",
        true,
    ),
    (
        "xsd2 §3.2.7 dateTime with a space",
        "dateTime",
        "2026-09-29 21:38:46",
        false,
    ),
    ("xsd2 §3.2.8 time", "time", "13:20:00", true),
    ("xsd2 §3.2.8 time hour 25", "time", "25:00:00", false),
    ("xsd2 §3.2.6 duration", "duration", "P1Y2M3DT10H30M", true),
    ("xsd2 §3.2.6 negative duration", "duration", "-P1D", true),
    (
        "xsd2 §3.2.6 duration without fields",
        "duration",
        "P",
        false,
    ),
    (
        "xsd2 §3.2.6 duration T without time fields",
        "duration",
        "P1DT",
        false,
    ),
    (
        "xsd2 §3.2.6 duration fractional year",
        "duration",
        "P1.5Y",
        false,
    ),
    ("xsd2 §3.2.11 gYear", "gYear", "2026", true),
    ("xsd2 §3.2.10 gYearMonth", "gYearMonth", "2026-09", true),
    ("xsd2 §3.2.12 gMonthDay", "gMonthDay", "--09-29", true),
    ("xsd2 §3.2.13 gDay", "gDay", "---29", true),
    ("xsd2 §3.2.14 gMonth", "gMonth", "--09", true),
    ("xsd2 §3.2.14 gMonth thirteen", "gMonth", "--13", false),
    ("xsd2 §3.2.15 hexBinary", "hexBinary", "0FB7", true),
    (
        "xsd2 §3.2.15 hexBinary odd length",
        "hexBinary",
        "0FB",
        false,
    ),
    (
        "xsd2 §3.2.16 base64Binary",
        "base64Binary",
        "SGVsbG8=",
        true,
    ),
    (
        "xsd2 §3.2.16 base64Binary bad padding",
        "base64Binary",
        "SGVsbG8",
        false,
    ),
    ("xsd2 §3.3.3 language", "language", "en-US", true),
    (
        "xsd2 §3.3.3 language with underscore",
        "language",
        "en_US",
        false,
    ),
    ("xsd2 §3.3.4 NMTOKEN", "NMTOKEN", "a-b.c", true),
    ("xsd2 §3.3.4 NMTOKEN with a space", "NMTOKEN", "a b", false),
    ("xsd2 §3.3.5 NMTOKENS", "NMTOKENS", "a b c", true),
    ("xsd2 §3.3.7 NCName", "NCName", "a", true),
    ("xsd2 §3.3.7 NCName with colon", "NCName", "a:b", false),
    (
        "xsd2 §3.3.6 Name starting with a digit",
        "Name",
        "1a",
        false,
    ),
    ("xsd2 §3.3.2 token", "token", "a b", true),
];

/// `(case, xs:restriction of a named simple type, value, valid)`.
const FACET_CASES: &[(&str, &str, &str, bool)] = &[
    (
        "xsd2 §4.3.4 pattern is anchored",
        "<xs:restriction base=\"xs:string\"><xs:pattern value=\"\\d{3}\"/></xs:restriction>",
        "1234",
        false,
    ),
    (
        "xsd2 §4.3.4 pattern match",
        "<xs:restriction base=\"xs:string\"><xs:pattern value=\"\\d{3}-[A-Z]{2}\"/></xs:restriction>",
        "926-AA",
        true,
    ),
    (
        "xsd2 §F.1 pattern \\i\\c* name escapes",
        "<xs:restriction base=\"xs:string\"><xs:pattern value=\"\\i\\c*\"/></xs:restriction>",
        "abc",
        true,
    ),
    (
        "xsd2 §F.1 pattern \\i rejects a digit",
        "<xs:restriction base=\"xs:string\"><xs:pattern value=\"\\i\\c*\"/></xs:restriction>",
        "1abc",
        false,
    ),
    (
        "xsd2 §F.1 pattern character class subtraction",
        "<xs:restriction base=\"xs:string\"><xs:pattern value=\"[a-z-[aeiou]]+\"/></xs:restriction>",
        "bcd",
        true,
    ),
    (
        "xsd2 §F.1 pattern character class subtraction rejects",
        "<xs:restriction base=\"xs:string\"><xs:pattern value=\"[a-z-[aeiou]]+\"/></xs:restriction>",
        "abc",
        false,
    ),
    (
        "xsd2 §F.1 pattern Unicode block escape",
        "<xs:restriction base=\"xs:string\"><xs:pattern value=\"\\p{IsBasicLatin}+\"/></xs:restriction>",
        "abc",
        true,
    ),
    (
        "xsd2 §4.3.4 several patterns in one step are alternatives",
        "<xs:restriction base=\"xs:string\"><xs:pattern value=\"a+\"/><xs:pattern value=\"b+\"/></xs:restriction>",
        "bb",
        true,
    ),
    (
        "xsd2 §4.3.5 enumeration",
        "<xs:restriction base=\"xs:token\"><xs:enumeration value=\"red\"/><xs:enumeration value=\"green\"/></xs:restriction>",
        "blue",
        false,
    ),
    (
        "xsd2 §4.3.5 enumeration after whitespace collapse",
        "<xs:restriction base=\"xs:token\"><xs:enumeration value=\"red\"/></xs:restriction>",
        "  red ",
        true,
    ),
    (
        "xsd2 §4.3.1 length",
        "<xs:restriction base=\"xs:string\"><xs:length value=\"3\"/></xs:restriction>",
        "abcd",
        false,
    ),
    (
        "xsd2 §4.3.1 length counts characters, not bytes",
        "<xs:restriction base=\"xs:string\"><xs:length value=\"3\"/></xs:restriction>",
        "été",
        true,
    ),
    (
        "xsd2 §4.3.3 maxLength",
        "<xs:restriction base=\"xs:string\"><xs:maxLength value=\"2\"/></xs:restriction>",
        "ab",
        true,
    ),
    (
        "xsd2 §4.3.11 totalDigits",
        "<xs:restriction base=\"xs:decimal\"><xs:totalDigits value=\"3\"/></xs:restriction>",
        "12.34",
        false,
    ),
    (
        "xsd2 §4.3.12 fractionDigits",
        "<xs:restriction base=\"xs:decimal\"><xs:fractionDigits value=\"2\"/></xs:restriction>",
        "1.234",
        false,
    ),
    (
        "xsd2 §4.3.10 minInclusive",
        "<xs:restriction base=\"xs:integer\"><xs:minInclusive value=\"10\"/></xs:restriction>",
        "9",
        false,
    ),
    (
        "xsd2 §4.3.8 maxExclusive",
        "<xs:restriction base=\"xs:positiveInteger\"><xs:maxExclusive value=\"100\"/></xs:restriction>",
        "100",
        false,
    ),
    (
        "xsd2 §4.3.10 minInclusive on dates",
        "<xs:restriction base=\"xs:date\"><xs:minInclusive value=\"2026-01-01\"/></xs:restriction>",
        "2025-12-31",
        false,
    ),
    (
        "xsd2 §4.3.6 whitespace replace keeps length",
        "<xs:restriction base=\"xs:normalizedString\"><xs:length value=\"3\"/></xs:restriction>",
        "a\tb",
        true,
    ),
];

/// `(case, schema body, instance, valid)`: the body is wrapped in an
/// `xs:schema` element binding `xs`.
const STRUCTURE_CASES: &[(&str, &str, &str, bool)] = &[
    (
        "xsd1 §3.3 element ref with minOccurs 0",
        "<xs:element name=\"c\" type=\"xs:string\"/><xs:element name=\"r\"><xs:complexType><xs:sequence><xs:element ref=\"c\" minOccurs=\"0\"/><xs:element name=\"d\"/></xs:sequence></xs:complexType></xs:element>",
        "<r><d/></r>",
        true,
    ),
    (
        "xsd1 §3.8 sequence order",
        "<xs:element name=\"r\"><xs:complexType><xs:sequence><xs:element name=\"a\"/><xs:element name=\"b\"/></xs:sequence></xs:complexType></xs:element>",
        "<r><b/><a/></r>",
        false,
    ),
    (
        "xsd1 §3.8 maxOccurs",
        "<xs:element name=\"r\"><xs:complexType><xs:sequence><xs:element name=\"a\" maxOccurs=\"2\"/></xs:sequence></xs:complexType></xs:element>",
        "<r><a/><a/><a/></r>",
        false,
    ),
    (
        "xsd1 §3.8 all group in any order",
        "<xs:element name=\"r\"><xs:complexType><xs:all><xs:element name=\"a\"/><xs:element name=\"b\"/></xs:all></xs:complexType></xs:element>",
        "<r><b/><a/></r>",
        true,
    ),
    (
        "xsd1 §3.8 all group missing element",
        "<xs:element name=\"r\"><xs:complexType><xs:all><xs:element name=\"a\"/><xs:element name=\"b\"/></xs:all></xs:complexType></xs:element>",
        "<r><b/></r>",
        false,
    ),
    (
        "xsd1 §3.8 choice picks one",
        "<xs:element name=\"r\"><xs:complexType><xs:choice><xs:element name=\"a\"/><xs:element name=\"b\"/></xs:choice></xs:complexType></xs:element>",
        "<r><a/><b/></r>",
        false,
    ),
    (
        "xsd1 §3.8 nested sequence in choice",
        "<xs:element name=\"r\"><xs:complexType><xs:choice maxOccurs=\"unbounded\"><xs:sequence><xs:element name=\"k\"/><xs:element name=\"v\"/></xs:sequence><xs:element name=\"x\"/></xs:choice></xs:complexType></xs:element>",
        "<r><k/><v/><x/><k/><v/></r>",
        true,
    ),
    (
        "xsd1 §3.2 required attribute missing",
        "<xs:element name=\"r\"><xs:complexType><xs:attribute name=\"id\" use=\"required\"/></xs:complexType></xs:element>",
        "<r/>",
        false,
    ),
    (
        "xsd1 §3.2 undeclared attribute",
        "<xs:element name=\"r\"><xs:complexType><xs:attribute name=\"id\"/></xs:complexType></xs:element>",
        "<r other=\"1\"/>",
        false,
    ),
    (
        "xsd1 §3.2 fixed attribute value",
        "<xs:element name=\"r\"><xs:complexType><xs:attribute name=\"v\" fixed=\"1\"/></xs:complexType></xs:element>",
        "<r v=\"2\"/>",
        false,
    ),
    (
        "xsd1 §3.3 fixed element value",
        "<xs:element name=\"r\" type=\"xs:string\" fixed=\"x\"/>",
        "<r>y</r>",
        false,
    ),
    (
        "xsd1 §3.4 simple content with attribute",
        "<xs:element name=\"r\"><xs:complexType><xs:simpleContent><xs:extension base=\"xs:decimal\"><xs:attribute name=\"currency\"/></xs:extension></xs:simpleContent></xs:complexType></xs:element>",
        "<r currency=\"EUR\">12.50</r>",
        true,
    ),
    (
        "xsd1 §3.4 mixed content",
        "<xs:element name=\"r\"><xs:complexType mixed=\"true\"><xs:sequence><xs:element name=\"b\" minOccurs=\"0\"/></xs:sequence></xs:complexType></xs:element>",
        "<r>text <b/> more</r>",
        true,
    ),
    (
        "xsd1 §3.4 text in element-only content",
        "<xs:element name=\"r\"><xs:complexType><xs:sequence><xs:element name=\"b\"/></xs:sequence></xs:complexType></xs:element>",
        "<r>text<b/></r>",
        false,
    ),
    (
        "xsd1 §3.4 empty content",
        "<xs:element name=\"r\"><xs:complexType/></xs:element>",
        "<r><x/></r>",
        false,
    ),
    (
        "xsd1 §3.4 extension appends to the base sequence",
        "<xs:complexType name=\"base\"><xs:sequence><xs:element name=\"a\"/></xs:sequence></xs:complexType><xs:element name=\"r\"><xs:complexType><xs:complexContent><xs:extension base=\"base\"><xs:sequence><xs:element name=\"b\"/></xs:sequence></xs:extension></xs:complexContent></xs:complexType></xs:element>",
        "<r><a/><b/></r>",
        true,
    ),
    (
        "xsd1 §3.3 nillable with xsi:nil",
        "<xs:element name=\"r\" type=\"xs:int\" nillable=\"true\"/>",
        "<r xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:nil=\"true\"/>",
        true,
    ),
    (
        "xsd1 §3.3 xsi:nil on a non-nillable element",
        "<xs:element name=\"r\" type=\"xs:int\"/>",
        "<r xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:nil=\"true\"/>",
        false,
    ),
    (
        "xsd1 §3.10 xs:any skip",
        "<xs:element name=\"r\"><xs:complexType><xs:sequence><xs:any processContents=\"skip\" maxOccurs=\"unbounded\"/></xs:sequence></xs:complexType></xs:element>",
        "<r><anything x=\"1\"/><else/></r>",
        true,
    ),
    (
        "xsd1 §3.10 anyAttribute lax",
        "<xs:element name=\"r\"><xs:complexType><xs:anyAttribute processContents=\"lax\"/></xs:complexType></xs:element>",
        "<r a=\"1\" b=\"2\"/>",
        true,
    ),
    (
        "xsd1 §3.3 substitution group member",
        "<xs:element name=\"shape\" abstract=\"true\"/><xs:element name=\"circle\" substitutionGroup=\"shape\"/><xs:element name=\"r\"><xs:complexType><xs:sequence><xs:element ref=\"shape\" maxOccurs=\"unbounded\"/></xs:sequence></xs:complexType></xs:element>",
        "<r><circle/></r>",
        true,
    ),
    (
        "xsd1 §3.3 abstract element used directly",
        "<xs:element name=\"shape\" abstract=\"true\"/><xs:element name=\"r\"><xs:complexType><xs:sequence><xs:element ref=\"shape\"/></xs:sequence></xs:complexType></xs:element>",
        "<r><shape/></r>",
        false,
    ),
    (
        "xsd1 §3.14 anonymous simple type",
        "<xs:element name=\"r\"><xs:simpleType><xs:restriction base=\"xs:string\"><xs:enumeration value=\"a\"/></xs:restriction></xs:simpleType></xs:element>",
        "<r>b</r>",
        false,
    ),
    (
        "xsd1 §3.14 list of integers",
        "<xs:element name=\"r\"><xs:simpleType><xs:list itemType=\"xs:integer\"/></xs:simpleType></xs:element>",
        "<r>1 2 x</r>",
        false,
    ),
    (
        "xsd1 §3.14 union member types",
        "<xs:element name=\"r\"><xs:simpleType><xs:union memberTypes=\"xs:integer xs:boolean\"/></xs:simpleType></xs:element>",
        "<r>true</r>",
        true,
    ),
    (
        "xsd1 §3.11 ID uniqueness",
        "<xs:element name=\"r\"><xs:complexType><xs:sequence><xs:element name=\"i\" maxOccurs=\"unbounded\"><xs:complexType><xs:attribute name=\"id\" type=\"xs:ID\"/></xs:complexType></xs:element></xs:sequence></xs:complexType></xs:element>",
        "<r><i id=\"a\"/><i id=\"a\"/></r>",
        false,
    ),
    (
        "xsd1 §3.11 IDREF to a missing ID",
        "<xs:element name=\"r\"><xs:complexType><xs:sequence><xs:element name=\"i\" maxOccurs=\"unbounded\"><xs:complexType><xs:attribute name=\"id\" type=\"xs:ID\"/><xs:attribute name=\"ref\" type=\"xs:IDREF\"/></xs:complexType></xs:element></xs:sequence></xs:complexType></xs:element>",
        "<r><i id=\"a\"/><i ref=\"b\"/></r>",
        false,
    ),
    (
        "xsd1 §3.11 key uniqueness",
        "<xs:element name=\"r\"><xs:complexType><xs:sequence><xs:element name=\"i\" maxOccurs=\"unbounded\"><xs:complexType><xs:attribute name=\"k\"/></xs:complexType></xs:element></xs:sequence></xs:complexType><xs:key name=\"key\"><xs:selector xpath=\"i\"/><xs:field xpath=\"@k\"/></xs:key></xs:element>",
        "<r><i k=\"1\"/><i k=\"1\"/></r>",
        false,
    ),
    (
        "xsd1 §3.3 qualified local elements",
        "<xs:element name=\"r\"><xs:complexType><xs:sequence><xs:element name=\"a\" form=\"qualified\"/></xs:sequence></xs:complexType></xs:element>",
        "<r><a/></r>",
        true,
    ),
    (
        "xsd1 §3.4.2 xsi:type selects a derived type",
        "<xs:complexType name=\"base\"><xs:sequence><xs:element name=\"a\"/></xs:sequence></xs:complexType><xs:complexType name=\"derived\"><xs:complexContent><xs:extension base=\"base\"><xs:sequence><xs:element name=\"b\"/></xs:sequence></xs:extension></xs:complexContent></xs:complexType><xs:element name=\"r\" type=\"base\"/>",
        "<r xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xsi:type=\"derived\"><a/><b/></r>",
        true,
    ),
];

#[test]
fn xml_well_formedness_cases() {
    run_well_formedness("spec-xml", XML_CASES);
}

#[test]
fn namespace_well_formedness_cases() {
    run_well_formedness("spec-namespaces", NAMESPACE_CASES);
}

fn run_well_formedness(name: &str, cases: &[(&str, &str, bool)]) {
    let mut run = SuiteRun::new(name);
    for (case, document, well_formed) in cases {
        let outcome = guarded(|| {
            let errors = well_formedness_errors(document);
            match (well_formed, errors.is_empty()) {
                (true, true) | (false, false) => Outcome::Pass,
                (true, false) => {
                    Outcome::Fail(format!("reported not well-formed: {}", errors.join("; ")))
                }
                (false, true) => Outcome::Fail("accepted a not well-formed document".to_owned()),
            }
        });
        run.record(*case, outcome);
    }
    run.check();
}

#[test]
fn xsd_datatype_cases() {
    let mut run = SuiteRun::new("spec-xsd-datatypes");
    for (case, datatype, value, valid) in DATATYPE_CASES {
        let body = format!("<xs:element name=\"v\" type=\"xs:{datatype}\"/>");
        run.record(
            *case,
            validity_case(&body, &format!("<v>{value}</v>"), *valid),
        );
    }
    for (case, restriction, value, valid) in FACET_CASES {
        let body = format!(
            "<xs:simpleType name=\"t\">{restriction}</xs:simpleType><xs:element name=\"v\" type=\"t\"/>"
        );
        run.record(
            *case,
            validity_case(&body, &format!("<v>{value}</v>"), *valid),
        );
    }
    run.check();
}

#[test]
fn xsd_structure_cases() {
    let mut run = SuiteRun::new("spec-xsd-structures");
    for (case, body, instance, valid) in STRUCTURE_CASES {
        run.record(*case, validity_case(body, instance, *valid));
    }
    run.check();
}

fn validity_case(body: &str, instance: &str, valid: bool) -> Outcome {
    let schema =
        format!("<xs:schema xmlns:xs=\"http://www.w3.org/2001/XMLSchema\">{body}</xs:schema>");
    guarded(|| {
        let schema = match parse_xsd(&schema) {
            Ok(schema) => schema,
            Err(error) => return Outcome::Fail(format!("schema rejected: {error}")),
        };
        let diagnostics = validate_document_located(instance, &schema);
        match (valid, diagnostics.is_empty()) {
            (true, true) | (false, false) => Outcome::Pass,
            (true, false) => Outcome::Fail(format!(
                "valid instance rejected: {}",
                diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            )),
            (false, true) => Outcome::Fail("invalid instance accepted".to_owned()),
        }
    })
}
