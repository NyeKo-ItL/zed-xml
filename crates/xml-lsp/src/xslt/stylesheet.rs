//! Analysis of one XSLT stylesheet module: XSLT elements, XPath sites
//! (expressions, patterns and value templates, with ranges mapped back to
//! the document), named component occurrences and variable scoping.

use std::ops::Range;

use xml_core::tags::{XmlMarkupKind, resolve_namespace, scan_markup};
use xpath_core::{XPathAnalysis, parse_value_template};

use super::vocabulary::{self, ComponentKind, Content, Role, Since, Syntax};
use crate::{hover::Document, links::XSLT_NAMESPACE};

/// XPath functions namespace.
const FN_NAMESPACE: &str = "http://www.w3.org/2005/xpath-functions";

/// Namespace URI (or `None`) and local name.
pub(crate) type ExpandedName = (Option<String>, String);

/// Kind of XPath site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SiteKind {
    Expression,
    Pattern,
    /// Attribute or text value template.
    ValueTemplate,
}

/// XPath text of the document (attribute value or text node).
#[derive(Debug)]
pub(crate) struct Site {
    /// Element carrying the attribute, or parent of the text.
    pub(crate) element: usize,
    /// Range of the value in the document (quotes excluded).
    pub(crate) value: Range<usize>,
    pub(crate) kind: SiteKind,
    /// Analysis with ranges in the document.
    pub(crate) analysis: XPathAnalysis,
    /// Attribute name, `None` for a text value template.
    pub(crate) attribute: Option<String>,
}

/// Declaration or reference of a named component.
#[derive(Debug, Clone)]
pub(crate) struct Occurrence {
    pub(crate) kind: ComponentKind,
    pub(crate) name: ExpandedName,
    pub(crate) range: Range<usize>,
    pub(crate) declaration: bool,
    pub(crate) element: usize,
    /// Arity of a function declaration or call.
    pub(crate) arity: Option<usize>,
}

/// `xsl:variable` or `xsl:param`.
#[derive(Debug, Clone)]
pub(crate) struct VariableDeclaration {
    pub(crate) element: usize,
    pub(crate) name: ExpandedName,
    /// Range of the `name` value.
    pub(crate) range: Range<usize>,
    pub(crate) global: bool,
}

/// `xsl:with-param/@name`.
#[derive(Debug, Clone)]
pub(crate) struct WithParam {
    pub(crate) element: usize,
    pub(crate) name: ExpandedName,
    pub(crate) range: Range<usize>,
}

/// Analyzed stylesheet module.
pub(crate) struct Module<'a> {
    pub(crate) uri: &'a str,
    pub(crate) source: &'a str,
    pub(crate) document: Document<'a>,
    /// XSLT local name of each element in the XSLT namespace.
    pub(crate) xslt: Vec<Option<&'a str>>,
    pub(crate) children: Vec<Vec<usize>>,
    /// The element is user data (top-level element outside the XSLT
    /// namespace, or inside one).
    pub(crate) data: Vec<bool>,
    pub(crate) version: Since,
    pub(crate) sites: Vec<Site>,
    pub(crate) occurrences: Vec<Occurrence>,
    pub(crate) variables: Vec<VariableDeclaration>,
    pub(crate) with_params: Vec<WithParam>,
}

/// Decoded attribute value or text, with the document offset of each byte.
pub(crate) struct Decoded {
    pub(crate) text: String,
    offsets: Vec<usize>,
    /// The text references entities declared in a DTD, whose replacement
    /// text is unknown here.
    unresolved_entities: bool,
}

impl Decoded {
    /// Resolves the predefined entities and character references of
    /// `source[range]`.
    pub(crate) fn new(source: &str, range: Range<usize>) -> Self {
        let raw = &source[range.clone()];
        let mut text = String::with_capacity(raw.len());
        let mut offsets = Vec::with_capacity(raw.len() + 1);
        let mut index = 0;
        let mut unresolved_entities = false;
        while index < raw.len() {
            let rest = &raw[index..];
            let (character, length) = if rest.starts_with('&') {
                entity(rest).unwrap_or_else(|| {
                    unresolved_entities = true;
                    ('&', 1)
                })
            } else {
                let character = rest.chars().next().unwrap_or('\u{FFFD}');
                (character, character.len_utf8().max(1))
            };
            for _ in 0..character.len_utf8() {
                offsets.push(range.start + index);
            }
            text.push(character);
            index += length;
        }
        offsets.push(range.end);
        Self {
            text,
            offsets,
            unresolved_entities,
        }
    }

    fn offset(&self, decoded: usize) -> usize {
        self.offsets[decoded.min(self.offsets.len() - 1)]
    }

    pub(crate) fn map(&self, range: &Range<usize>) -> Range<usize> {
        self.offset(range.start)..self.offset(range.end)
    }

    /// Maps every range of `analysis` to the document. Syntax errors are
    /// dropped when the text references DTD entities.
    fn map_analysis(&self, analysis: &mut XPathAnalysis) {
        if self.unresolved_entities {
            analysis.errors.clear();
        }
        for error in &mut analysis.errors {
            error.range = self.map(&error.range);
        }
        for binding in &mut analysis.bindings {
            binding.range = self.map(&binding.range);
        }
        for variable in &mut analysis.variables {
            variable.range = self.map(&variable.range);
        }
        for function in &mut analysis.functions {
            function.range = self.map(&function.range);
            for argument in &mut function.arguments {
                argument.range = self.map(&argument.range);
                if let Some((_, range)) = &mut argument.string_literal {
                    *range = self.map(range);
                }
            }
        }
    }
}

/// `&name;` or `&#...;` at the start of `text`: the character and the
/// length of the reference.
fn entity(text: &str) -> Option<(char, usize)> {
    let end = text.get(..text.len().min(12))?.find(';')?;
    let name = &text[1..end];
    let character = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => name
            .strip_prefix("#x")
            .or_else(|| name.strip_prefix("#X"))
            .map(|hex| u32::from_str_radix(hex, 16))
            .or_else(|| name.strip_prefix('#').map(str::parse::<u32>))?
            .ok()
            .and_then(char::from_u32)?,
    };
    Some((character, end + 1))
}

/// The source quickly looks like a stylesheet (cheap pre-check).
pub(crate) fn may_be_stylesheet(source: &str) -> bool {
    source.contains(XSLT_NAMESPACE)
}

impl<'a> Module<'a> {
    /// Analyzes `source`; `None` if it is not an XSLT stylesheet module
    /// (root in the XSLT namespace, or simplified stylesheet with an
    /// `xsl:version` attribute).
    pub(crate) fn parse(uri: &'a str, source: &'a str) -> Option<Self> {
        if !may_be_stylesheet(source) {
            return None;
        }
        let document = Document::parse(source);
        let elements = document.tree.elements();
        let root = elements
            .iter()
            .position(|element| element.parent.is_none())?;
        let mut children = vec![Vec::new(); elements.len()];
        for (index, element) in elements.iter().enumerate() {
            if let Some(parent) = element.parent {
                children[parent].push(index);
            }
        }
        let xslt = (0..elements.len())
            .map(|index| {
                let name = document.element_name(index);
                let (prefix, local) = match name.split_once(':') {
                    Some((prefix, local)) => (Some(prefix), local),
                    None => (None, name),
                };
                is_xslt_namespace(&document, index, prefix).then_some(local)
            })
            .collect::<Vec<_>>();
        let mut module = Self {
            uri,
            source,
            document,
            xslt,
            children,
            data: Vec::new(),
            version: 30,
            sites: Vec::new(),
            occurrences: Vec::new(),
            variables: Vec::new(),
            with_params: Vec::new(),
        };
        let root_version = match module.xslt[root] {
            Some("stylesheet" | "transform" | "package") => module.attribute_value(root, "version"),
            Some(_) => return None,
            None => module.xslt_attribute(root, "version")?,
        };
        module.version = root_version.map_or(30, vocabulary::version_code);
        module.analyze();
        Some(module)
    }

    pub(crate) fn element_count(&self) -> usize {
        self.xslt.len()
    }

    /// Unprefixed attribute `name` of `element`.
    pub(crate) fn attribute_value(&self, element: usize, name: &str) -> Option<&'a str> {
        let source = self.source;
        self.document.attributes[element]
            .iter()
            .find(|attribute| attribute.name(source) == name)
            .and_then(|attribute| attribute.value(source))
    }

    /// Attribute `xsl:name` (XSLT namespace) of `element`: `None` if
    /// absent, `Some(None)` without a value.
    fn xslt_attribute(&self, element: usize, local: &str) -> Option<Option<&'a str>> {
        let source = self.source;
        self.document.attributes[element]
            .iter()
            .find(|attribute| {
                attribute
                    .name(source)
                    .split_once(':')
                    .is_some_and(|(prefix, name)| {
                        name == local && is_xslt_namespace(&self.document, element, Some(prefix))
                    })
            })
            .map(|attribute| attribute.value(source))
    }

    /// The element is the XSLT element `name`.
    pub(crate) fn is(&self, element: usize, name: &str) -> bool {
        self.xslt.get(element).copied().flatten() == Some(name)
    }

    /// Top-level container (`xsl:stylesheet`, `xsl:transform`,
    /// `xsl:package`, `xsl:override`).
    pub(crate) fn is_container(&self, element: usize) -> bool {
        matches!(
            self.xslt[element],
            Some("stylesheet" | "transform" | "package" | "override")
        )
    }

    pub(crate) fn parent(&self, element: usize) -> Option<usize> {
        self.document.tree.elements()[element].parent
    }

    /// Expanded name of the QName `name` read in the context of `element`
    /// (unprefixed names are in no namespace, as for every XSLT name).
    pub(crate) fn expand(&self, element: usize, name: &str) -> ExpandedName {
        expand_name(&self.document, element, name)
    }

    fn analyze(&mut self) {
        let count = self.element_count();
        // Top-level elements outside the XSLT namespace are user data, not
        // literal result elements.
        let mut data = vec![false; count];
        let mut expand_text = vec![false; count];
        for index in 0..count {
            let parent = self.parent(index);
            data[index] = match parent {
                Some(parent) => {
                    data[parent] || (self.is_container(parent) && self.xslt[index].is_none())
                }
                None => false,
            };
            let inherited = parent.is_some_and(|parent| expand_text[parent]);
            let own = if self.xslt[index].is_some() {
                self.attribute_value(index, "expand-text")
            } else {
                self.xslt_attribute(index, "expand-text").flatten()
            };
            expand_text[index] = own.map_or(inherited, |value| {
                matches!(value.trim(), "yes" | "true" | "1")
            });
            if data[index] {
                continue;
            }
            match self.xslt[index] {
                Some(name) => self.analyze_xslt_element(index, name),
                None => self.analyze_literal_element(index),
            }
            if expand_text[index] && self.version >= 30 {
                self.analyze_text(index);
            }
        }
        self.data = data;
    }

    fn analyze_xslt_element(&mut self, index: usize, name: &str) {
        let Some(definition) = vocabulary::element(name) else {
            // Unknown or future element: only `use-when` is checked.
            if let Some(attribute) = self.find_attribute(index, "use-when") {
                self.add_site(index, attribute, SiteKind::Expression, "use-when");
            }
            return;
        };
        let attributes = self.document.attributes[index].clone();
        for attribute in &attributes {
            let attribute_name = attribute.name(self.source);
            let Some(value) = attribute.value.clone() else {
                continue;
            };
            if attribute_name.contains(':') || attribute_name == "xmlns" {
                continue;
            }
            if attribute_name == "name" && matches!(name, "variable" | "param") {
                let text = self.source[value.clone()].trim();
                let start = value.start
                    + (self.source[value.clone()].len()
                        - self.source[value.clone()].trim_start().len());
                let global = self
                    .parent(index)
                    .is_some_and(|parent| self.is_container(parent));
                self.variables.push(VariableDeclaration {
                    element: index,
                    name: self.expand(index, text),
                    range: start..start + text.len(),
                    global,
                });
                continue;
            }
            if attribute_name == "name" && name == "with-param" {
                let text = self.source[value.clone()].trim();
                let start = value.start
                    + (self.source[value.clone()].len()
                        - self.source[value.clone()].trim_start().len());
                self.with_params.push(WithParam {
                    element: index,
                    name: self.expand(index, text),
                    range: start..start + text.len(),
                });
                continue;
            }
            let Some(definition) = definition.attribute(attribute_name) else {
                continue;
            };
            match definition.syntax {
                Syntax::Expression => {
                    self.add_site(index, value.clone(), SiteKind::Expression, attribute_name)
                }
                Syntax::Pattern => {
                    self.add_site(index, value.clone(), SiteKind::Pattern, attribute_name)
                }
                Syntax::Avt => self.add_site(
                    index,
                    value.clone(),
                    SiteKind::ValueTemplate,
                    attribute_name,
                ),
                Syntax::SequenceType | Syntax::Text => {}
            }
            if let Some(role) = definition.role
                && !(definition.syntax == Syntax::Avt && self.source[value.clone()].contains('{'))
            {
                self.add_names(index, value, role);
            }
        }
        if name == "function"
            && let Some(occurrence) = self
                .occurrences
                .iter_mut()
                .rev()
                .find(|occurrence| occurrence.element == index && occurrence.declaration)
        {
            let arity = self.children[index]
                .iter()
                .filter(|child| self.xslt[**child] == Some("param"))
                .count();
            occurrence.arity = Some(arity);
        }
    }

    fn analyze_literal_element(&mut self, index: usize) {
        let attributes = self.document.attributes[index].clone();
        for attribute in &attributes {
            let name = attribute.name(self.source);
            let Some(value) = attribute.value.clone() else {
                continue;
            };
            if name == "xmlns" || name.starts_with("xmlns:") {
                continue;
            }
            if let Some((prefix, local)) = name.split_once(':')
                && is_xslt_namespace(&self.document, index, Some(prefix))
            {
                match local {
                    "use-when" => self.add_site(index, value, SiteKind::Expression, name),
                    "use-attribute-sets" => {
                        self.add_names(index, value, Role::RefersList(ComponentKind::AttributeSet))
                    }
                    "default-mode" => {
                        self.add_names(index, value, Role::Refers(ComponentKind::Mode))
                    }
                    _ => {}
                }
                continue;
            }
            self.add_site(index, value, SiteKind::ValueTemplate, name);
        }
    }

    /// Text value templates of the element's text content (XSLT 3.0
    /// `expand-text="yes"`).
    fn analyze_text(&mut self, index: usize) {
        let accepts_text = match self.xslt[index] {
            Some(name) => vocabulary::element(name).is_some_and(|element| {
                matches!(
                    element.content,
                    Content::SequenceConstructor
                        | Content::Children {
                            sequence_constructor: true,
                            ..
                        }
                )
            }),
            None => true,
        };
        let Some(content) = self.document.tree.elements()[index].content_range() else {
            return;
        };
        if !accepts_text || !self.source[content.clone()].contains(['{', '}']) {
            return;
        }
        let mut segments = vec![content.clone()];
        let mut exclude = self.children[index]
            .iter()
            .map(|child| self.document.tree.elements()[*child].range())
            .collect::<Vec<_>>();
        let mut cdata = Vec::new();
        for markup in scan_markup(&self.source[content.clone()]) {
            let range = content.start + markup.range.start..content.start + markup.range.end;
            if markup.kind == XmlMarkupKind::CData {
                cdata
                    .push(content.start + markup.content.start..content.start + markup.content.end);
            }
            exclude.push(range);
        }
        for range in exclude {
            segments = segments
                .into_iter()
                .flat_map(|segment| subtract(segment, &range))
                .collect();
        }
        segments.extend(cdata);
        segments.sort_by_key(|segment| segment.start);
        for segment in segments {
            if self.source[segment.clone()].contains(['{', '}']) {
                let decoded = Decoded::new(self.source, segment.clone());
                let mut analysis = parse_value_template(&decoded.text);
                decoded.map_analysis(&mut analysis);
                self.push_site(index, segment, SiteKind::ValueTemplate, analysis, None);
            }
        }
    }

    fn find_attribute(&self, element: usize, name: &str) -> Option<Range<usize>> {
        self.document.attributes[element]
            .iter()
            .find(|attribute| attribute.name(self.source) == name)
            .and_then(|attribute| attribute.value.clone())
    }

    fn add_site(&mut self, element: usize, value: Range<usize>, kind: SiteKind, attribute: &str) {
        let decoded = Decoded::new(self.source, value.clone());
        let mut analysis = match kind {
            SiteKind::Expression | SiteKind::Pattern => xpath_core::parse(&decoded.text),
            SiteKind::ValueTemplate => parse_value_template(&decoded.text),
        };
        decoded.map_analysis(&mut analysis);
        self.push_site(element, value, kind, analysis, Some(attribute.to_owned()));
    }

    fn push_site(
        &mut self,
        element: usize,
        value: Range<usize>,
        kind: SiteKind,
        analysis: XPathAnalysis,
        attribute: Option<String>,
    ) {
        for function in &analysis.functions {
            let name = self.expand(element, &function.name);
            let builtin = name.0.is_none() || name.0.as_deref() == Some(FN_NAMESPACE);
            let literal = |index: usize| {
                function
                    .arguments
                    .get(index)
                    .and_then(|argument| argument.string_literal.clone())
            };
            let named = match (builtin, name.1.as_str(), function.arity) {
                (true, "key", 2 | 3) => literal(0).map(|value| (ComponentKind::Key, value)),
                (true, "format-number", 3) => {
                    literal(2).map(|value| (ComponentKind::DecimalFormat, value))
                }
                (true, "accumulator-before" | "accumulator-after", 1) => {
                    literal(0).map(|value| (ComponentKind::Accumulator, value))
                }
                _ => None,
            };
            if let Some((kind, (text, range))) = named {
                let trimmed = text.trim();
                if !trimmed.is_empty() && !function.reference {
                    let start = range.start + (text.len() - text.trim_start().len());
                    self.occurrences.push(Occurrence {
                        kind,
                        name: self.expand(element, trimmed),
                        range: start..(start + trimmed.len()).min(range.end),
                        declaration: false,
                        element,
                        arity: None,
                    });
                }
            }
            if !builtin {
                self.occurrences.push(Occurrence {
                    kind: ComponentKind::Function,
                    name,
                    range: function.range.clone(),
                    declaration: false,
                    element,
                    arity: Some(function.arity),
                });
            }
        }
        self.sites.push(Site {
            element,
            value,
            kind,
            analysis,
            attribute,
        });
    }

    fn add_names(&mut self, element: usize, value: Range<usize>, role: Role) {
        let (kind, declaration, list) = match role {
            Role::Declares(kind) => (kind, true, false),
            Role::Refers(kind) => (kind, false, false),
            Role::RefersList(kind) => (kind, false, true),
        };
        let text = &self.source[value.clone()];
        let mut tokens = Vec::new();
        let mut start = None;
        for (offset, character) in text
            .char_indices()
            .chain(std::iter::once((text.len(), ' ')))
        {
            if character.is_whitespace() {
                if let Some(token_start) = start.take() {
                    tokens.push(token_start..offset);
                }
            } else if start.is_none() {
                start = Some(offset);
            }
        }
        if !list {
            tokens.truncate(1);
        }
        for token in tokens {
            let name = &text[token.clone()];
            if name.starts_with('#') || name == "*" {
                // `#default`, `#all`, `#current`, `#unnamed`.
                continue;
            }
            let expanded = self.expand(element, name);
            self.occurrences.push(Occurrence {
                kind,
                name: expanded,
                range: value.start + token.start..value.start + token.end,
                declaration,
                element,
                arity: None,
            });
        }
    }

    /// Declaration (`xsl:variable`/`xsl:param` element) of the variable
    /// `name` visible at `offset` from `element` without leaving the
    /// template or function: children of `element` ending before
    /// `offset`, then preceding siblings of the element and of its
    /// ancestors.
    pub(crate) fn local_variable(
        &self,
        element: usize,
        offset: usize,
        name: &ExpandedName,
    ) -> Option<usize> {
        let elements = self.document.tree.elements();
        let matches = |candidate: usize| {
            self.variables
                .iter()
                .find(|variable| variable.element == candidate)
                .is_some_and(|variable| &variable.name == name && !variable.global)
        };
        if let Some(found) = self.children[element]
            .iter()
            .rev()
            .copied()
            .filter(|child| elements[*child].range().end <= offset)
            .find(|child| matches(*child))
        {
            return Some(found);
        }
        let mut node = element;
        loop {
            let parent = self.parent(node)?;
            if self.is_container(parent) {
                return None;
            }
            let position = self.children[parent]
                .iter()
                .position(|child| *child == node)?;
            if let Some(found) = self.children[parent][..position]
                .iter()
                .rev()
                .copied()
                .find(|sibling| matches(*sibling))
            {
                return Some(found);
            }
            node = parent;
        }
    }

    /// Local variables and parameters visible at `offset` from `element`
    /// (see [`Self::local_variable`]), nearest first, one per name.
    pub(crate) fn visible_locals(
        &self,
        element: usize,
        offset: usize,
    ) -> Vec<&VariableDeclaration> {
        let elements = self.document.tree.elements();
        let mut candidates = self.children[element]
            .iter()
            .rev()
            .copied()
            .filter(|child| elements[*child].range().end <= offset)
            .collect::<Vec<_>>();
        let mut node = element;
        while let Some(parent) = self.parent(node) {
            if self.is_container(parent) {
                break;
            }
            let Some(position) = self.children[parent]
                .iter()
                .position(|child| *child == node)
            else {
                break;
            };
            candidates.extend(self.children[parent][..position].iter().rev().copied());
            node = parent;
        }
        let mut visible: Vec<&VariableDeclaration> = Vec::new();
        for candidate in candidates {
            if let Some(variable) = self
                .variables
                .iter()
                .find(|variable| variable.element == candidate && !variable.global)
                && !visible.iter().any(|seen| seen.name == variable.name)
            {
                visible.push(variable);
            }
        }
        visible
    }

    /// Global variable or parameter `name` of this module.
    pub(crate) fn global_variable(&self, name: &ExpandedName) -> Option<&VariableDeclaration> {
        self.variables
            .iter()
            .find(|variable| variable.global && &variable.name == name)
    }

    pub(crate) fn variable_declaration(&self, element: usize) -> Option<&VariableDeclaration> {
        self.variables
            .iter()
            .find(|variable| variable.element == element)
    }

    /// Start tag text of `element` (for hovers).
    pub(crate) fn start_tag(&self, element: usize) -> &'a str {
        let range = self.document.tree.elements()[element]
            .start_tag
            .range
            .clone();
        &self.source[range]
    }

    /// XSLT prefix to use in new elements inside `element` (`Some("")`
    /// when XSLT is the default namespace).
    pub(crate) fn xslt_prefix(&self, element: usize) -> Option<String> {
        let source = self.source;
        let mut prefix: Option<String> = None;
        for index in std::iter::once(element).chain(self.document.tree.ancestors(element)) {
            for attribute in &self.document.attributes[index] {
                let name = attribute.name(source);
                if attribute.value(source) != Some(XSLT_NAMESPACE) {
                    continue;
                }
                if name == "xmlns" {
                    return Some(String::new());
                }
                if let Some(declared) = name.strip_prefix("xmlns:")
                    && prefix.is_none()
                {
                    prefix = Some(declared.to_owned());
                }
            }
            if prefix.is_some() {
                return prefix;
            }
        }
        prefix
    }
}

/// Expanded name of `name` (`p:local`, `local` or `Q{uri}local`) in the
/// context of `element`. An unbound prefix is kept as `prefix:` so that
/// names with the same unbound prefix still match.
pub(crate) fn expand_name(document: &Document<'_>, element: usize, name: &str) -> ExpandedName {
    let name = name.trim();
    if let Some(rest) = name.strip_prefix("Q{")
        && let Some((uri, local)) = rest.split_once('}')
    {
        return ((!uri.is_empty()).then(|| uri.to_owned()), local.to_owned());
    }
    match name.split_once(':') {
        Some((prefix, local)) => {
            let namespace = resolve_namespace(
                document.source,
                &document.tree,
                &document.attributes,
                element,
                Some(prefix),
            )
            .flatten()
            .map_or_else(|| format!("{prefix}:"), str::to_owned);
            (Some(namespace), local.to_owned())
        }
        None => (None, name.to_owned()),
    }
}

/// `prefix` (or the default namespace) is bound to the XSLT namespace at
/// `element`; the conventional `xsl` prefix is accepted when undeclared.
fn is_xslt_namespace(document: &Document<'_>, element: usize, prefix: Option<&str>) -> bool {
    match resolve_namespace(
        document.source,
        &document.tree,
        &document.attributes,
        element,
        prefix,
    ) {
        Some(namespace) => namespace == Some(XSLT_NAMESPACE),
        None => prefix == Some("xsl"),
    }
}

fn subtract(segment: Range<usize>, remove: &Range<usize>) -> Vec<Range<usize>> {
    if remove.end <= segment.start || remove.start >= segment.end {
        return vec![segment];
    }
    let mut parts = Vec::new();
    if segment.start < remove.start {
        parts.push(segment.start..remove.start);
    }
    if remove.end < segment.end {
        parts.push(remove.end..segment.end);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    const STYLESHEET: &str = r#"<?xml version="1.0"?>
<xsl:stylesheet version="3.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"
    xmlns:f="urn:f" expand-text="yes">
  <xsl:key name="k" match="item" use="@id"/>
  <xsl:param name="p" select="1"/>
  <xsl:template match="/" name="main" mode="m1 #default">
    <xsl:variable name="v" select="$p + 1"/>
    <out a="{$v}" b="{{literal}}">{$v} &amp; {key('k', 'x')}</out>
    <xsl:call-template name="main"/>
    <xsl:value-of select="f:twice($v)"/>
  </xsl:template>
  <xsl:function name="f:twice">
    <xsl:param name="n"/>
    <xsl:sequence select="$n * 2"/>
  </xsl:function>
  <data xmlns="urn:data"><entry value="{"/></data>
</xsl:stylesheet>"#;

    #[test]
    fn analyzes_sites_components_and_variables() {
        let module = Module::parse("file:///a.xsl", STYLESHEET).expect("stylesheet");
        assert_eq!(module.version, 30);
        assert!(
            module
                .sites
                .iter()
                .all(|site| site.analysis.errors.is_empty()),
            "{:?}",
            module
                .sites
                .iter()
                .flat_map(|site| &site.analysis.errors)
                .collect::<Vec<_>>()
        );
        let kinds = module
            .occurrences
            .iter()
            .map(|occurrence| {
                (
                    occurrence.kind,
                    occurrence.name.1.as_str(),
                    occurrence.declaration,
                    &STYLESHEET[occurrence.range.clone()],
                )
            })
            .collect::<Vec<_>>();
        assert!(kinds.contains(&(ComponentKind::Key, "k", true, "k")));
        assert!(kinds.contains(&(ComponentKind::Key, "k", false, "k")));
        assert!(kinds.contains(&(ComponentKind::Template, "main", true, "main")));
        assert!(kinds.contains(&(ComponentKind::Template, "main", false, "main")));
        assert!(kinds.contains(&(ComponentKind::Mode, "m1", false, "m1")));
        assert!(kinds.contains(&(ComponentKind::Function, "twice", true, "f:twice")));
        assert!(kinds.contains(&(ComponentKind::Function, "twice", false, "f:twice")));
        let function = module
            .occurrences
            .iter()
            .find(|occurrence| occurrence.kind == ComponentKind::Function && occurrence.declaration)
            .expect("function");
        assert_eq!(function.arity, Some(1));
        assert_eq!(function.name.0.as_deref(), Some("urn:f"));

        // `$v` in the text value template resolves to the local variable.
        let text_site = module
            .sites
            .iter()
            .find(|site| site.attribute.is_none())
            .expect("text value template");
        let reference = &text_site.analysis.variables[0];
        assert_eq!(&STYLESHEET[reference.range.clone()], "v");
        let declaration = module
            .local_variable(
                text_site.element,
                text_site.value.start,
                &(None, "v".into()),
            )
            .expect("local variable");
        assert!(module.is(declaration, "variable"));
        assert!(module.global_variable(&(None, "p".into())).is_some());
        assert!(
            module
                .local_variable(text_site.element, 0, &(None, "p".into()))
                .is_none()
        );
    }

    #[test]
    fn maps_ranges_through_entities() {
        let source = r#"<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"><xsl:template match="/"><xsl:if test="$a &lt; $b and"/></xsl:template></xsl:stylesheet>"#;
        let module = Module::parse("file:///b.xsl", source).expect("stylesheet");
        let site = module
            .sites
            .iter()
            .find(|site| site.attribute.as_deref() == Some("test"))
            .expect("test");
        assert_eq!(&source[site.analysis.variables[1].range.clone()], "b");
        let error = &site.analysis.errors[0];
        assert_eq!(&source[error.range.clone()], "and");
    }

    #[test]
    fn ignores_non_stylesheets_and_accepts_simplified_stylesheets() {
        assert!(Module::parse("file:///c.xml", "<root/>").is_none());
        let simplified = r#"<html xsl:version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"><p class="{@x"><xsl:value-of select="title"/></p></html>"#;
        let module = Module::parse("file:///d.xsl", simplified).expect("simplified");
        assert_eq!(module.version, 10);
        let errors = module
            .sites
            .iter()
            .flat_map(|site| &site.analysis.errors)
            .collect::<Vec<_>>();
        assert_eq!(errors.len(), 1);
    }
}
