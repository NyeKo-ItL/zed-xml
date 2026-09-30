//! XSLT 1.0, 2.0 and 3.0 vocabulary: elements, their attributes (with the
//! syntax of their values) and allowed children, used by completion,
//! hover and the XPath checks.

/// Version in which an element or attribute appeared (`10`, `20`, `30`).
pub(crate) type Since = u8;

/// Syntax of an attribute value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Syntax {
    /// XPath expression.
    Expression,
    /// XSLT pattern (checked as an XPath expression, which accepts every
    /// pattern).
    Pattern,
    /// Attribute value template.
    Avt,
    /// Sequence type (`as`).
    SequenceType,
    /// Anything else (names, tokens, URIs...).
    Text,
}

/// Named stylesheet component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ComponentKind {
    Template,
    Function,
    Mode,
    Key,
    AttributeSet,
    DecimalFormat,
    Accumulator,
    CharacterMap,
    Output,
}

impl ComponentKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Template => "named template",
            Self::Function => "function",
            Self::Mode => "mode",
            Self::Key => "key",
            Self::AttributeSet => "attribute set",
            Self::DecimalFormat => "decimal format",
            Self::Accumulator => "accumulator",
            Self::CharacterMap => "character map",
            Self::Output => "output definition",
        }
    }
}

/// Role of a name-valued attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// Declares the component.
    Declares(ComponentKind),
    /// Refers to one component.
    Refers(ComponentKind),
    /// Whitespace-separated list of references.
    RefersList(ComponentKind),
}

#[derive(Debug)]
pub(crate) struct Attribute {
    pub(crate) name: &'static str,
    pub(crate) since: Since,
    pub(crate) required: bool,
    pub(crate) syntax: Syntax,
    /// Enumerated values (possibly an AVT that must evaluate to one).
    pub(crate) values: &'static [&'static str],
    pub(crate) role: Option<Role>,
}

/// Allowed content of an element.
#[derive(Debug)]
pub(crate) enum Content {
    Empty,
    /// Instructions and literal result elements.
    SequenceConstructor,
    /// Top-level declarations.
    Declarations,
    /// Specific XSLT children, optionally followed by a sequence
    /// constructor.
    Children {
        names: &'static [&'static str],
        sequence_constructor: bool,
    },
}

#[derive(Debug)]
pub(crate) struct Element {
    pub(crate) name: &'static str,
    pub(crate) since: Since,
    /// Allowed as a top-level declaration.
    pub(crate) declaration: bool,
    /// Allowed in a sequence constructor.
    pub(crate) instruction: bool,
    pub(crate) content: Content,
    pub(crate) attributes: &'static [Attribute],
    pub(crate) documentation: &'static str,
}

impl Element {
    pub(crate) fn attribute(&self, name: &str) -> Option<&'static Attribute> {
        self.attributes
            .iter()
            .chain(STANDARD_ATTRIBUTES)
            .find(|attribute| attribute.name == name)
    }
}

const YES_NO: &[&str] = &["yes", "no"];
const VALIDATION: &[&str] = &["strict", "lax", "preserve", "strip"];
const VISIBILITY: &[&str] = &["public", "private", "final", "abstract"];
const STREAMABILITY: &[&str] = &[
    "unclassified",
    "absorbing",
    "inspection",
    "filter",
    "shallow-descent",
    "deep-descent",
    "ascent",
];
const OUTPUT_METHODS: &[&str] = &["xml", "html", "xhtml", "text", "json", "adaptive"];
const NORMALIZATION_FORMS: &[&str] = &["NFC", "NFD", "NFKC", "NFKD", "fully-normalized", "none"];

const fn attr(name: &'static str, since: Since, required: bool, syntax: Syntax) -> Attribute {
    Attribute {
        name,
        since,
        required,
        syntax,
        values: &[],
        role: None,
    }
}

const fn values(
    name: &'static str,
    since: Since,
    syntax: Syntax,
    values: &'static [&'static str],
) -> Attribute {
    Attribute {
        name,
        since,
        required: false,
        syntax,
        values,
        role: None,
    }
}

const fn role(name: &'static str, since: Since, required: bool, role: Role) -> Attribute {
    Attribute {
        name,
        since,
        required,
        syntax: Syntax::Text,
        values: &[],
        role: Some(role),
    }
}

use Syntax::{Avt, Expression, Pattern, SequenceType, Text};

/// Standard attributes allowed on every XSLT element (unprefixed) and, in
/// the XSLT namespace, on literal result elements.
pub(crate) const STANDARD_ATTRIBUTES: &[Attribute] = &[
    attr("use-when", 20, false, Expression),
    attr("exclude-result-prefixes", 10, false, Text),
    attr("extension-element-prefixes", 10, false, Text),
    attr("xpath-default-namespace", 20, false, Text),
    attr("default-collation", 20, false, Text),
    role("default-mode", 30, false, Role::Refers(ComponentKind::Mode)),
    values("default-validation", 20, Text, &["preserve", "strip"]),
    values("expand-text", 30, Text, YES_NO),
    attr("version", 10, false, Text),
];

const ROOT_ATTRIBUTES: &[Attribute] = &[
    values("version", 10, Text, &["1.0", "2.0", "3.0"]),
    attr("id", 10, false, Text),
    values(
        "input-type-annotations",
        20,
        Text,
        &["preserve", "strip", "unspecified"],
    ),
];

const OUTPUT_ATTRIBUTES: &[Attribute] = &[
    role("name", 10, false, Role::Declares(ComponentKind::Output)),
    values("method", 10, Text, OUTPUT_METHODS),
    values("allow-duplicate-names", 30, Text, YES_NO),
    values("build-tree", 30, Text, YES_NO),
    values("byte-order-mark", 20, Text, YES_NO),
    attr("cdata-section-elements", 10, false, Text),
    attr("doctype-public", 10, false, Text),
    attr("doctype-system", 10, false, Text),
    attr("encoding", 10, false, Text),
    values("escape-uri-attributes", 20, Text, YES_NO),
    values("html-version", 30, Text, &["5.0"]),
    values("include-content-type", 20, Text, YES_NO),
    values("indent", 10, Text, YES_NO),
    attr("item-separator", 30, false, Text),
    values("json-node-output-method", 30, Text, OUTPUT_METHODS),
    attr("media-type", 10, false, Text),
    values("normalization-form", 20, Text, NORMALIZATION_FORMS),
    values("omit-xml-declaration", 10, Text, YES_NO),
    attr("parameter-document", 30, false, Text),
    values("standalone", 10, Text, &["yes", "no", "omit"]),
    attr("suppress-indentation", 30, false, Text),
    values("undeclare-prefixes", 20, Text, YES_NO),
    role(
        "use-character-maps",
        20,
        false,
        Role::RefersList(ComponentKind::CharacterMap),
    ),
    attr("version", 10, false, Text),
];

const RESULT_DOCUMENT_ATTRIBUTES: &[Attribute] = &[
    Attribute {
        name: "format",
        since: 20,
        required: false,
        syntax: Avt,
        values: &[],
        role: Some(Role::Refers(ComponentKind::Output)),
    },
    attr("href", 20, false, Avt),
    values("validation", 20, Text, VALIDATION),
    attr("type", 20, false, Text),
    values("method", 20, Avt, OUTPUT_METHODS),
    values("allow-duplicate-names", 30, Avt, YES_NO),
    values("build-tree", 30, Avt, YES_NO),
    values("byte-order-mark", 20, Avt, YES_NO),
    attr("cdata-section-elements", 20, false, Avt),
    attr("doctype-public", 20, false, Avt),
    attr("doctype-system", 20, false, Avt),
    attr("encoding", 20, false, Avt),
    values("escape-uri-attributes", 20, Avt, YES_NO),
    values("html-version", 30, Avt, &["5.0"]),
    values("include-content-type", 20, Avt, YES_NO),
    values("indent", 20, Avt, YES_NO),
    attr("item-separator", 30, false, Avt),
    values("json-node-output-method", 30, Avt, OUTPUT_METHODS),
    attr("media-type", 20, false, Avt),
    values("normalization-form", 20, Avt, NORMALIZATION_FORMS),
    values("omit-xml-declaration", 20, Avt, YES_NO),
    attr("parameter-document", 30, false, Avt),
    values("standalone", 20, Avt, &["yes", "no", "omit"]),
    attr("suppress-indentation", 30, false, Avt),
    values("undeclare-prefixes", 20, Avt, YES_NO),
    role(
        "use-character-maps",
        20,
        false,
        Role::RefersList(ComponentKind::CharacterMap),
    ),
    attr("output-version", 20, false, Avt),
];

const SORT_ATTRIBUTES: &[Attribute] = &[
    attr("select", 10, false, Expression),
    attr("lang", 10, false, Avt),
    values("data-type", 10, Avt, &["text", "number"]),
    values("order", 10, Avt, &["ascending", "descending"]),
    values("case-order", 10, Avt, &["upper-first", "lower-first"]),
    attr("collation", 20, false, Avt),
    values("stable", 20, Avt, YES_NO),
];

const VARIABLE_ATTRIBUTES: &[Attribute] = &[
    attr("name", 10, true, Text),
    attr("select", 10, false, Expression),
    attr("as", 20, false, SequenceType),
    values("static", 30, Text, YES_NO),
    values("visibility", 30, Text, VISIBILITY),
];

const PARAM_ATTRIBUTES: &[Attribute] = &[
    attr("name", 10, true, Text),
    attr("select", 10, false, Expression),
    attr("as", 20, false, SequenceType),
    values("required", 20, Text, YES_NO),
    values("tunnel", 20, Text, YES_NO),
    values("static", 30, Text, YES_NO),
];

const NUMBER_ATTRIBUTES: &[Attribute] = &[
    attr("value", 10, false, Expression),
    attr("select", 20, false, Expression),
    values("level", 10, Text, &["single", "multiple", "any"]),
    attr("count", 10, false, Pattern),
    attr("from", 10, false, Pattern),
    attr("format", 10, false, Avt),
    attr("lang", 10, false, Avt),
    values("letter-value", 10, Avt, &["alphabetic", "traditional"]),
    attr("ordinal", 20, false, Avt),
    attr("start-at", 30, false, Avt),
    attr("grouping-separator", 10, false, Avt),
    attr("grouping-size", 10, false, Avt),
];

macro_rules! element {
    ($name:literal, $since:literal, decl: $declaration:literal, instr: $instruction:literal,
     $content:expr, [$($attribute:expr),* $(,)?], $documentation:literal) => {
        Element {
            name: $name,
            since: $since,
            declaration: $declaration,
            instruction: $instruction,
            content: $content,
            attributes: &[$($attribute),*],
            documentation: $documentation,
        }
    };
}

const SC: Content = Content::SequenceConstructor;

const fn children(names: &'static [&'static str], sequence_constructor: bool) -> Content {
    Content::Children {
        names,
        sequence_constructor,
    }
}

/// Every XSLT 3.0 element (XSLT 1.0 and 2.0 are subsets).
pub(crate) const ELEMENTS: &[Element] = &[
    Element {
        name: "stylesheet",
        since: 10,
        declaration: false,
        instruction: false,
        content: Content::Declarations,
        attributes: ROOT_ATTRIBUTES,
        documentation: "Root element of a stylesheet module; contains the top-level declarations.",
    },
    Element {
        name: "transform",
        since: 10,
        declaration: false,
        instruction: false,
        content: Content::Declarations,
        attributes: ROOT_ATTRIBUTES,
        documentation: "Root element of a stylesheet module (synonym of `xsl:stylesheet`).",
    },
    element!("package", 30, decl: false, instr: false, Content::Declarations, [
        attr("name", 30, false, Text),
        attr("package-version", 30, false, Text),
        values("version", 30, Text, &["3.0"]),
        values("declared-modes", 30, Text, YES_NO),
        attr("id", 30, false, Text),
        values("input-type-annotations", 30, Text, &["preserve", "strip", "unspecified"]),
    ], "Root element of a library package (XSLT 3.0)."),
    // Declarations.
    element!("accumulator", 30, decl: true, instr: false, children(&["accumulator-rule"], false), [
        role("name", 30, true, Role::Declares(ComponentKind::Accumulator)),
        attr("initial-value", 30, true, Expression),
        attr("as", 30, false, SequenceType),
        values("streamable", 30, Text, YES_NO),
    ], "Declares an accumulator: a value computed during a document traversal, read with `accumulator-before()` / `accumulator-after()`."),
    element!("accumulator-rule", 30, decl: false, instr: false, SC, [
        attr("match", 30, true, Pattern),
        values("phase", 30, Text, &["start", "end"]),
        attr("select", 30, false, Expression),
    ], "Rule updating an accumulator when a node matching `match` is visited."),
    element!("attribute-set", 10, decl: true, instr: false, children(&["attribute"], false), [
        role("name", 10, true, Role::Declares(ComponentKind::AttributeSet)),
        role("use-attribute-sets", 10, false, Role::RefersList(ComponentKind::AttributeSet)),
        values("visibility", 30, Text, VISIBILITY),
        values("streamable", 30, Text, YES_NO),
    ], "Declares a named set of attributes, added with `use-attribute-sets`."),
    element!("character-map", 20, decl: true, instr: false, children(&["output-character"], false), [
        role("name", 20, true, Role::Declares(ComponentKind::CharacterMap)),
        role("use-character-maps", 20, false, Role::RefersList(ComponentKind::CharacterMap)),
    ], "Declares a character map used during serialization."),
    element!("decimal-format", 10, decl: true, instr: false, Content::Empty, [
        role("name", 10, false, Role::Declares(ComponentKind::DecimalFormat)),
        attr("decimal-separator", 10, false, Text),
        attr("grouping-separator", 10, false, Text),
        attr("infinity", 10, false, Text),
        attr("minus-sign", 10, false, Text),
        attr("exponent-separator", 30, false, Text),
        attr("NaN", 10, false, Text),
        attr("percent", 10, false, Text),
        attr("per-mille", 10, false, Text),
        attr("zero-digit", 10, false, Text),
        attr("digit", 10, false, Text),
        attr("pattern-separator", 10, false, Text),
    ], "Declares a decimal format used by `format-number()`."),
    element!("function", 20, decl: true, instr: false, children(&["param"], true), [
        role("name", 20, true, Role::Declares(ComponentKind::Function)),
        attr("as", 20, false, SequenceType),
        values("visibility", 30, Text, VISIBILITY),
        values("streamability", 30, Text, STREAMABILITY),
        values("override-extension-function", 30, Text, YES_NO),
        values("override", 20, Text, YES_NO),
        values("new-each-time", 30, Text, &["yes", "no", "maybe"]),
        values("cache", 30, Text, YES_NO),
    ], "Declares a stylesheet function callable from XPath expressions (the name must be prefixed)."),
    element!("global-context-item", 30, decl: true, instr: false, Content::Empty, [
        attr("as", 30, false, SequenceType),
        values("use", 30, Text, &["required", "optional", "absent"]),
    ], "Declares whether a global context item is required and its type."),
    element!("import", 10, decl: true, instr: false, Content::Empty, [
        attr("href", 10, true, Text),
    ], "Imports a stylesheet module; its declarations have a lower import precedence. Must come first."),
    element!("import-schema", 20, decl: true, instr: false, Content::Empty, [
        attr("namespace", 20, false, Text),
        attr("schema-location", 20, false, Text),
    ], "Imports the schema components of a namespace (schema-aware processors)."),
    element!("include", 10, decl: true, instr: false, Content::Empty, [
        attr("href", 10, true, Text),
    ], "Includes a stylesheet module; its declarations have the same import precedence."),
    element!("key", 10, decl: true, instr: false, SC, [
        role("name", 10, true, Role::Declares(ComponentKind::Key)),
        attr("match", 10, true, Pattern),
        attr("use", 10, false, Expression),
        values("composite", 30, Text, YES_NO),
        attr("collation", 20, false, Text),
    ], "Declares a key: an index of the nodes matching `match` by the value of `use`, queried with `key()`."),
    element!("mode", 30, decl: true, instr: false, Content::Empty, [
        role("name", 30, false, Role::Declares(ComponentKind::Mode)),
        values("streamable", 30, Text, YES_NO),
        role("use-accumulators", 30, false, Role::RefersList(ComponentKind::Accumulator)),
        values("on-no-match", 30, Text, &["deep-copy", "shallow-copy", "deep-skip", "shallow-skip", "text-only-copy", "fail"]),
        values("on-multiple-match", 30, Text, &["use-last", "fail"]),
        values("warning-on-no-match", 30, Text, YES_NO),
        values("warning-on-multiple-match", 30, Text, YES_NO),
        values("typed", 30, Text, &["yes", "no", "strict", "lax", "unspecified"]),
        values("visibility", 30, Text, &["public", "private", "final"]),
    ], "Declares the properties of a mode (XSLT 3.0)."),
    element!("namespace-alias", 10, decl: true, instr: false, Content::Empty, [
        attr("stylesheet-prefix", 10, true, Text),
        attr("result-prefix", 10, true, Text),
    ], "Maps a namespace used in the stylesheet to another namespace in the result."),
    Element {
        name: "output",
        since: 10,
        declaration: true,
        instruction: false,
        content: Content::Empty,
        attributes: OUTPUT_ATTRIBUTES,
        documentation: "Declares serialization parameters of the principal (or a named) result document.",
    },
    Element {
        name: "param",
        since: 10,
        declaration: true,
        instruction: false,
        content: SC,
        attributes: PARAM_ATTRIBUTES,
        documentation: "Declares a stylesheet, template, function or iteration parameter; its value is set by the caller (`xsl:with-param`) or defaults to `select`/the content.",
    },
    element!("preserve-space", 10, decl: true, instr: false, Content::Empty, [
        attr("elements", 10, true, Text),
    ], "Lists the source elements whose whitespace-only text nodes are preserved."),
    element!("strip-space", 10, decl: true, instr: false, Content::Empty, [
        attr("elements", 10, true, Text),
    ], "Lists the source elements whose whitespace-only text nodes are removed."),
    element!("template", 10, decl: true, instr: false, children(&["context-item", "param"], true), [
        attr("match", 10, false, Pattern),
        role("name", 10, false, Role::Declares(ComponentKind::Template)),
        attr("priority", 10, false, Text),
        role("mode", 10, false, Role::RefersList(ComponentKind::Mode)),
        attr("as", 20, false, SequenceType),
        values("visibility", 30, Text, VISIBILITY),
    ], "Declares a template rule (`match`) and/or a named template (`name`)."),
    element!("use-package", 30, decl: true, instr: false, children(&["accept", "override"], false), [
        attr("name", 30, true, Text),
        attr("package-version", 30, false, Text),
    ], "Uses the components of another package."),
    Element {
        name: "variable",
        since: 10,
        declaration: true,
        instruction: true,
        content: SC,
        attributes: VARIABLE_ATTRIBUTES,
        documentation: "Binds a name to a value (`select` or the content); referenced as `$name` in XPath expressions.",
    },
    element!("expose", 30, decl: false, instr: false, Content::Empty, [
        values("component", 30, Text, &["template", "function", "attribute-set", "variable", "mode", "*"]),
        attr("names", 30, true, Text),
        values("visibility", 30, Text, &["public", "private", "final", "abstract", "hidden"]),
    ], "Changes the visibility of components exposed by a package."),
    element!("accept", 30, decl: false, instr: false, Content::Empty, [
        values("component", 30, Text, &["template", "function", "attribute-set", "variable", "mode", "*"]),
        attr("names", 30, true, Text),
        values("visibility", 30, Text, &["public", "private", "final", "abstract", "hidden"]),
    ], "Changes the visibility of components accepted from a used package."),
    element!("override", 30, decl: false, instr: false, children(&["template", "function", "variable", "param", "attribute-set"], false), [
    ], "Overrides components of a used package."),
    // Instructions.
    element!("analyze-string", 20, decl: false, instr: true, children(&["matching-substring", "non-matching-substring", "fallback"], false), [
        attr("select", 20, true, Expression),
        attr("regex", 20, true, Avt),
        attr("flags", 20, false, Avt),
    ], "Splits a string with a regular expression, processing matching and non-matching substrings."),
    element!("matching-substring", 20, decl: false, instr: false, SC, [], "Processes each substring matched by the regular expression of `xsl:analyze-string` (`regex-group()` gives its groups)."),
    element!("non-matching-substring", 20, decl: false, instr: false, SC, [], "Processes each substring not matched by the regular expression of `xsl:analyze-string`."),
    element!("apply-imports", 10, decl: false, instr: true, children(&["with-param"], false), [], "Applies the imported template rules to the current node."),
    element!("apply-templates", 10, decl: false, instr: true, children(&["sort", "with-param"], false), [
        attr("select", 10, false, Expression),
        role("mode", 10, false, Role::Refers(ComponentKind::Mode)),
    ], "Applies template rules to the selected nodes (default: the children of the context node)."),
    element!("assert", 30, decl: false, instr: true, SC, [
        attr("test", 30, true, Expression),
        attr("select", 30, false, Expression),
        attr("error-code", 30, false, Avt),
    ], "Raises a dynamic error when `test` is false (when assertions are enabled)."),
    element!("attribute", 10, decl: false, instr: true, SC, [
        attr("name", 10, true, Avt),
        attr("namespace", 10, false, Avt),
        attr("select", 20, false, Expression),
        attr("separator", 20, false, Avt),
        attr("type", 20, false, Text),
        values("validation", 20, Text, VALIDATION),
    ], "Creates an attribute node."),
    element!("break", 30, decl: false, instr: true, SC, [
        attr("select", 30, false, Expression),
    ], "Stops an `xsl:iterate` early."),
    element!("call-template", 10, decl: false, instr: true, children(&["with-param"], false), [
        role("name", 10, true, Role::Refers(ComponentKind::Template)),
    ], "Invokes a named template."),
    element!("catch", 30, decl: false, instr: false, SC, [
        attr("errors", 30, false, Text),
        attr("select", 30, false, Expression),
    ], "Handles the dynamic errors raised in the enclosing `xsl:try`."),
    element!("choose", 10, decl: false, instr: true, children(&["when", "otherwise"], false), [], "Selects the first `xsl:when` whose test is true, else `xsl:otherwise`."),
    element!("comment", 10, decl: false, instr: true, SC, [
        attr("select", 20, false, Expression),
    ], "Creates a comment node."),
    element!("context-item", 30, decl: false, instr: false, Content::Empty, [
        attr("as", 30, false, SequenceType),
        values("use", 30, Text, &["required", "optional", "absent"]),
    ], "Declares the context item expected by a template."),
    element!("copy", 10, decl: false, instr: true, SC, [
        attr("select", 30, false, Expression),
        values("copy-namespaces", 20, Text, YES_NO),
        values("inherit-namespaces", 20, Text, YES_NO),
        role("use-attribute-sets", 10, false, Role::RefersList(ComponentKind::AttributeSet)),
        attr("type", 20, false, Text),
        values("validation", 20, Text, VALIDATION),
    ], "Shallow copy of the context item (the content creates its children)."),
    element!("copy-of", 10, decl: false, instr: true, Content::Empty, [
        attr("select", 10, true, Expression),
        values("copy-accumulators", 30, Text, YES_NO),
        values("copy-namespaces", 20, Text, YES_NO),
        attr("type", 20, false, Text),
        values("validation", 20, Text, VALIDATION),
    ], "Deep copy of the selected items."),
    element!("document", 20, decl: false, instr: true, SC, [
        values("validation", 20, Text, VALIDATION),
        attr("type", 20, false, Text),
    ], "Creates a document node."),
    element!("element", 10, decl: false, instr: true, SC, [
        attr("name", 10, true, Avt),
        attr("namespace", 10, false, Avt),
        values("inherit-namespaces", 20, Text, YES_NO),
        role("use-attribute-sets", 10, false, Role::RefersList(ComponentKind::AttributeSet)),
        attr("type", 20, false, Text),
        values("validation", 20, Text, VALIDATION),
    ], "Creates an element node with a computed name."),
    element!("evaluate", 30, decl: false, instr: true, children(&["with-param", "fallback"], false), [
        attr("xpath", 30, true, Expression),
        attr("as", 30, false, SequenceType),
        attr("base-uri", 30, false, Avt),
        attr("with-params", 30, false, Expression),
        attr("context-item", 30, false, Expression),
        attr("namespace-context", 30, false, Expression),
        attr("schema-aware", 30, false, Avt),
    ], "Evaluates an XPath expression built at run time."),
    element!("fallback", 10, decl: false, instr: true, SC, [], "Content evaluated when the parent instruction is not supported by the processor."),
    element!("for-each", 10, decl: false, instr: true, children(&["sort"], true), [
        attr("select", 10, true, Expression),
    ], "Processes each selected item (the context item inside)."),
    element!("for-each-group", 20, decl: false, instr: true, children(&["sort"], true), [
        attr("select", 20, true, Expression),
        attr("group-by", 20, false, Expression),
        attr("group-adjacent", 20, false, Expression),
        attr("group-starting-with", 20, false, Pattern),
        attr("group-ending-with", 20, false, Pattern),
        values("composite", 30, Text, YES_NO),
        attr("collation", 20, false, Avt),
    ], "Groups the selected items (`current-group()`, `current-grouping-key()`)."),
    element!("fork", 30, decl: false, instr: true, children(&["sequence", "for-each-group", "fallback"], false), [], "Evaluates its branches independently (streaming)."),
    element!("if", 10, decl: false, instr: true, SC, [
        attr("test", 10, true, Expression),
    ], "Evaluates its content when `test` is true."),
    element!("iterate", 30, decl: false, instr: true, children(&["param", "on-completion"], true), [
        attr("select", 30, true, Expression),
    ], "Processes the selected items in order, passing parameters from one iteration to the next."),
    element!("map", 30, decl: false, instr: true, SC, [], "Creates a map from the `xsl:map-entry` (or map) items of its content."),
    element!("map-entry", 30, decl: false, instr: true, SC, [
        attr("key", 30, true, Expression),
        attr("select", 30, false, Expression),
    ], "Creates a single-entry map."),
    element!("merge", 30, decl: false, instr: true, children(&["merge-source", "merge-action", "fallback"], false), [], "Merges several sorted input sequences."),
    element!("merge-action", 30, decl: false, instr: false, SC, [], "Processes each group of merged items."),
    element!("merge-key", 30, decl: false, instr: false, SC, [
        attr("select", 30, false, Expression),
        attr("lang", 30, false, Avt),
        values("order", 30, Avt, &["ascending", "descending"]),
        attr("collation", 30, false, Avt),
        values("case-order", 30, Avt, &["upper-first", "lower-first"]),
        values("data-type", 30, Avt, &["text", "number"]),
    ], "Key on which the inputs of `xsl:merge` are sorted."),
    element!("merge-source", 30, decl: false, instr: false, children(&["merge-key"], false), [
        attr("name", 30, false, Text),
        attr("for-each-item", 30, false, Expression),
        attr("for-each-source", 30, false, Expression),
        attr("select", 30, true, Expression),
        values("streamable", 30, Text, YES_NO),
        role("use-accumulators", 30, false, Role::RefersList(ComponentKind::Accumulator)),
        values("sort-before-merge", 30, Text, YES_NO),
        values("validation", 30, Text, VALIDATION),
        attr("type", 30, false, Text),
    ], "Input of `xsl:merge`."),
    element!("message", 10, decl: false, instr: true, SC, [
        attr("select", 20, false, Expression),
        values("terminate", 10, Avt, YES_NO),
        attr("error-code", 30, false, Avt),
    ], "Outputs a message (and optionally terminates the transformation)."),
    element!("namespace", 20, decl: false, instr: true, SC, [
        attr("name", 20, true, Avt),
        attr("select", 20, false, Expression),
    ], "Creates a namespace node."),
    element!("next-iteration", 30, decl: false, instr: true, children(&["with-param"], false), [], "Sets the parameters of the next iteration of `xsl:iterate`."),
    element!("next-match", 20, decl: false, instr: true, children(&["with-param", "fallback"], false), [], "Applies the next matching template rule to the context item."),
    Element {
        name: "number",
        since: 10,
        declaration: false,
        instruction: true,
        content: Content::Empty,
        attributes: NUMBER_ATTRIBUTES,
        documentation: "Outputs a formatted number (the position of a node or `value`).",
    },
    element!("on-completion", 30, decl: false, instr: false, SC, [
        attr("select", 30, false, Expression),
    ], "Evaluated after the last iteration of `xsl:iterate`."),
    element!("on-empty", 30, decl: false, instr: true, SC, [
        attr("select", 30, false, Expression),
    ], "Content produced when the rest of the sequence constructor produces nothing."),
    element!("on-non-empty", 30, decl: false, instr: true, SC, [
        attr("select", 30, false, Expression),
    ], "Content produced only when the rest of the sequence constructor produces something."),
    element!("otherwise", 10, decl: false, instr: false, SC, [], "Branch of `xsl:choose` evaluated when no `xsl:when` test is true."),
    element!("output-character", 20, decl: false, instr: false, Content::Empty, [
        attr("character", 20, true, Text),
        attr("string", 20, true, Text),
    ], "Maps a character to a string during serialization."),
    element!("perform-sort", 20, decl: false, instr: true, children(&["sort"], true), [
        attr("select", 20, false, Expression),
    ], "Sorts a sequence."),
    element!("processing-instruction", 10, decl: false, instr: true, SC, [
        attr("name", 10, true, Avt),
        attr("select", 20, false, Expression),
    ], "Creates a processing instruction node."),
    Element {
        name: "result-document",
        since: 20,
        declaration: false,
        instruction: true,
        content: SC,
        attributes: RESULT_DOCUMENT_ATTRIBUTES,
        documentation: "Creates a secondary result document.",
    },
    element!("sequence", 20, decl: false, instr: true, SC, [
        attr("select", 20, false, Expression),
    ], "Returns the selected items (or the content)."),
    Element {
        name: "sort",
        since: 10,
        declaration: false,
        instruction: false,
        content: SC,
        attributes: SORT_ATTRIBUTES,
        documentation: "Sort key of `xsl:apply-templates`, `xsl:for-each`, `xsl:for-each-group` or `xsl:perform-sort`.",
    },
    element!("source-document", 30, decl: false, instr: true, SC, [
        attr("href", 30, true, Avt),
        values("streamable", 30, Text, YES_NO),
        role("use-accumulators", 30, false, Role::RefersList(ComponentKind::Accumulator)),
        values("validation", 30, Text, VALIDATION),
        attr("type", 30, false, Text),
    ], "Reads (and possibly streams) a source document."),
    element!("text", 10, decl: false, instr: true, SC, [
        values("disable-output-escaping", 10, Text, YES_NO),
    ], "Creates a text node (whitespace is preserved)."),
    element!("try", 30, decl: false, instr: true, children(&["catch", "fallback"], true), [
        attr("select", 30, false, Expression),
        values("rollback-output", 30, Text, YES_NO),
    ], "Evaluates its content, handing dynamic errors to `xsl:catch`."),
    element!("value-of", 10, decl: false, instr: true, SC, [
        attr("select", 10, false, Expression),
        attr("separator", 20, false, Avt),
        values("disable-output-escaping", 10, Text, YES_NO),
    ], "Creates a text node from the string value of `select` (or the content)."),
    element!("when", 10, decl: false, instr: false, SC, [
        attr("test", 10, true, Expression),
    ], "Branch of `xsl:choose` evaluated when `test` is true."),
    element!("where-populated", 30, decl: false, instr: true, SC, [], "Discards the elements and documents of its content that are empty."),
    element!("with-param", 10, decl: false, instr: false, SC, [
        attr("name", 10, true, Text),
        attr("select", 10, false, Expression),
        attr("as", 20, false, SequenceType),
        values("tunnel", 20, Text, YES_NO),
    ], "Passes a parameter to a template, an iteration or `xsl:evaluate`."),
];

/// Element `name` of the XSLT namespace.
pub(crate) fn element(name: &str) -> Option<&'static Element> {
    ELEMENTS.iter().find(|element| element.name == name)
}

/// Version code of a `version` attribute value (`1.0` → 10, ...).
pub(crate) fn version_code(value: &str) -> Since {
    match value.trim() {
        "1.0" | "1" => 10,
        "2.0" | "2" => 20,
        _ => 30,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_and_children_are_consistent() {
        for element in ELEMENTS {
            assert!(
                element.since >= 10 && element.since <= 30,
                "{}",
                element.name
            );
            if let Content::Children { names, .. } = element.content {
                for name in names {
                    assert!(super::element(name).is_some(), "{} > {name}", element.name);
                }
            }
            let mut names = element
                .attributes
                .iter()
                .map(|attribute| attribute.name)
                .collect::<Vec<_>>();
            names.sort_unstable();
            let count = names.len();
            names.dedup();
            assert_eq!(
                count,
                names.len(),
                "duplicate attribute in {}",
                element.name
            );
        }
        assert_eq!(
            element("template")
                .and_then(|e| e.attribute("match"))
                .map(|a| a.syntax),
            Some(Pattern)
        );
        assert_eq!(
            element("if")
                .and_then(|e| e.attribute("use-when"))
                .map(|a| a.syntax),
            Some(Expression)
        );
        assert_eq!(version_code(" 2.0 "), 20);
        assert_eq!(version_code("3.0"), 30);
    }
}
