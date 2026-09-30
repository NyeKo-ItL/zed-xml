//! XSLT completion: `xsl:` elements allowed in context, their attributes
//! and values, component names, variables and key names.

use serde_json::{Value, json};
use xml_core::tags::{XmlTagKind, XmlTagTree};
use xpath_core::lexer::is_ncname_char;

use super::{
    Texts, XsltContext,
    navigation::{attribute_markdown, element_markdown, syntax_label},
    stylesheet::Module,
    vocabulary::{self, ComponentKind, Content, Role, STANDARD_ATTRIBUTES},
};
use crate::dtd::{TagContext, floor_boundary, in_markup, tag_context};

const FUNCTION_KIND: u8 = 3;
const FIELD_KIND: u8 = 5;
const VARIABLE_KIND: u8 = 6;
const ELEMENT_KIND: u8 = 10;
const VALUE_KIND: u8 = 12;
const REFERENCE_KIND: u8 = 18;

/// XSLT completion items at `offset` (empty outside stylesheets).
pub(crate) fn completions(
    context: &XsltContext<'_>,
    uri: &str,
    source: &str,
    offset: usize,
) -> Vec<Value> {
    let Some(single) = Texts::single(uri, source) else {
        return Vec::new();
    };
    let Some(document) = single.modules() else {
        return Vec::new();
    };
    let offset = floor_boundary(source, offset);
    if in_markup(source, offset) {
        return Vec::new();
    }
    // Names from the other modules are only needed in attribute values
    // and XPath sites.
    let with_all_modules = |complete: &dyn Fn(&[Module<'_>]) -> Vec<Value>| {
        Texts::load(context, uri, source)
            .and_then(|texts| texts.modules().map(|modules| complete(&modules)))
            .unwrap_or_default()
    };
    let prefix = &source[..offset];
    if let Some(opening) = prefix.rfind('<')
        && !prefix[opening..].contains('>')
    {
        return match tag_context(&source[opening + 1..offset]) {
            Some(TagContext::ElementName) => element_items(&document[0], source, opening),
            Some(TagContext::AttributeName { element, present }) => {
                attribute_items(&document[0], opening, element, &present)
            }
            Some(TagContext::AttributeValue { element, attribute }) => {
                with_all_modules(&|modules| {
                    let mut items = value_items(modules, opening, element, attribute);
                    items.extend(xpath_items(modules, offset));
                    items
                })
            }
            None => Vec::new(),
        };
    }
    let in_site = document[0]
        .sites
        .iter()
        .any(|site| site.value.start <= offset && offset <= site.value.end);
    if !in_site {
        return Vec::new();
    }
    with_all_modules(&|modules| xpath_items(modules, offset))
}

fn item(label: &str, kind: u8, insert_text: String, snippet: bool) -> Value {
    let mut item = json!({"label": label, "kind": kind, "insertText": insert_text});
    if snippet {
        item["insertTextFormat"] = json!(2);
    }
    item
}

/// Element of the module whose start tag begins at `start`.
fn element_starting_at(module: &Module<'_>, start: usize) -> Option<usize> {
    module
        .document
        .tree
        .elements()
        .iter()
        .position(|element| element.start_tag.range.start == start)
}

/// `xsl:` elements allowed in the parent of the tag opened at `opening`.
fn element_items(module: &Module<'_>, source: &str, opening: usize) -> Vec<Value> {
    let before = XmlTagTree::parse(&source[..opening]);
    let Some(parent_start) = before
        .elements()
        .iter()
        .rev()
        .find(|element| {
            element.start_tag.kind == XmlTagKind::Start
                && element.start_tag.closed
                && element.end_tag.is_none()
        })
        .map(|element| element.start_tag.range.start)
    else {
        return Vec::new();
    };
    let Some(parent) = element_starting_at(module, parent_start) else {
        return Vec::new();
    };
    if module.data[parent] {
        return Vec::new();
    }
    let (names, instructions, declarations): (&[&str], bool, bool) = match module.xslt[parent] {
        None => (&[], true, false),
        Some(name) => match vocabulary::element(name).map(|element| &element.content) {
            Some(Content::Declarations) => (&[], false, true),
            Some(Content::SequenceConstructor) => (&[], true, false),
            Some(Content::Children {
                names,
                sequence_constructor,
            }) => (names, *sequence_constructor, false),
            Some(Content::Empty) | None => return Vec::new(),
        },
    };
    let package = module.is(parent, "package");
    let prefix = module
        .xslt_prefix(parent)
        .unwrap_or_else(|| "xsl".to_owned());
    vocabulary::ELEMENTS
        .iter()
        .filter(|element| element.since <= module.version)
        .filter(|element| {
            names.contains(&element.name)
                || (instructions && element.instruction)
                || (declarations && element.declaration)
                || (declarations && package && element.name == "expose")
        })
        .map(|element| {
            let qualified = if prefix.is_empty() {
                element.name.to_owned()
            } else {
                format!("{prefix}:{}", element.name)
            };
            let mut insert_text = qualified.clone();
            for (index, attribute) in element
                .attributes
                .iter()
                .filter(|attribute| attribute.required && attribute.since <= module.version)
                .enumerate()
            {
                insert_text.push_str(&format!(" {}=\"${}\"", attribute.name, index + 1));
            }
            let mut value = item(&qualified, ELEMENT_KIND, insert_text, true);
            value["detail"] = json!(format!("XSLT element (since {})", since(element.since)));
            value["documentation"] =
                json!({"kind": "markdown", "value": element_markdown(element)});
            value
        })
        .collect()
}

fn since(version: vocabulary::Since) -> &'static str {
    match version {
        10 => "1.0",
        20 => "2.0",
        _ => "3.0",
    }
}

/// Attributes of the XSLT element whose tag is opened at `opening`.
fn attribute_items(
    module: &Module<'_>,
    opening: usize,
    element_name: &str,
    present: &[&str],
) -> Vec<Value> {
    let Some(element) = element_starting_at(module, opening) else {
        return Vec::new();
    };
    let Some(local) = module.xslt[element] else {
        return Vec::new();
    };
    debug_assert!(element_name.ends_with(local));
    let Some(definition) = vocabulary::element(local) else {
        return Vec::new();
    };
    let standard = if module.is_container(element) {
        &[][..]
    } else {
        STANDARD_ATTRIBUTES
    };
    let mut seen = Vec::new();
    definition
        .attributes
        .iter()
        .map(|attribute| (attribute, false))
        .chain(standard.iter().map(|attribute| (attribute, true)))
        .filter(|(attribute, _)| attribute.since <= module.version)
        .filter(|(attribute, _)| !present.contains(&attribute.name))
        .filter(|(attribute, _)| {
            let new = !seen.contains(&attribute.name);
            seen.push(attribute.name);
            new
        })
        .map(|(attribute, standard)| {
            let mut value = item(
                attribute.name,
                FIELD_KIND,
                format!("{}=\"$1\"", attribute.name),
                true,
            );
            let rank = match (attribute.required, standard) {
                (true, _) => 0,
                (false, false) => 1,
                (false, true) => 2,
            };
            value["sortText"] = json!(format!("{rank}{}", attribute.name));
            value["detail"] = json!(format!(
                "{}{}",
                syntax_label(attribute.syntax),
                if attribute.required {
                    " (required)"
                } else {
                    ""
                }
            ));
            value["documentation"] =
                json!({"kind": "markdown", "value": attribute_markdown(local, attribute)});
            value
        })
        .collect()
}

/// Enumerated values and component names for the attribute `attribute`
/// of the XSLT element opened at `opening`.
fn value_items(
    modules: &[Module<'_>],
    opening: usize,
    _element_name: &str,
    attribute_name: &str,
) -> Vec<Value> {
    let module = &modules[0];
    let Some(element) = element_starting_at(module, opening) else {
        return Vec::new();
    };
    let Some(local) = module.xslt[element] else {
        return Vec::new();
    };
    if attribute_name == "name" && local == "with-param" {
        return with_param_names(modules, element);
    }
    let Some(attribute) =
        vocabulary::element(local).and_then(|definition| definition.attribute(attribute_name))
    else {
        return Vec::new();
    };
    let mut items = attribute
        .values
        .iter()
        .map(|value| item(value, VALUE_KIND, (*value).to_owned(), false))
        .collect::<Vec<_>>();
    let kind = match attribute.role {
        Some(Role::Refers(kind) | Role::RefersList(kind)) => kind,
        _ => return items,
    };
    if kind == ComponentKind::Mode {
        let special: &[&str] = match local {
            "template" => &["#all", "#default", "#unnamed"],
            "apply-templates" => &["#current", "#default", "#unnamed"],
            _ => &["#default", "#unnamed"],
        };
        items.extend(
            special
                .iter()
                .filter(|value| module.version >= 30 || **value != "#unnamed")
                .map(|value| item(value, VALUE_KIND, (*value).to_owned(), false)),
        );
    }
    items.extend(component_names(modules, kind));
    items
}

/// Names of the declared components of `kind` (and, for modes, the modes
/// used by template rules).
fn component_names(modules: &[Module<'_>], kind: ComponentKind) -> Vec<Value> {
    let mut names: Vec<String> = Vec::new();
    for module in modules {
        for occurrence in &module.occurrences {
            let usable = occurrence.declaration
                || (kind == ComponentKind::Mode && module.is(occurrence.element, "template"));
            if occurrence.kind == kind && usable {
                let text = module.source[occurrence.range.clone()].to_owned();
                if !names.contains(&text) {
                    names.push(text);
                }
            }
        }
    }
    names
        .into_iter()
        .map(|name| {
            let mut value = item(&name, REFERENCE_KIND, name.clone(), false);
            value["detail"] = json!(kind.label());
            value
        })
        .collect()
}

/// Parameters of the template called by the `xsl:call-template` parent of
/// the `xsl:with-param` `element`.
fn with_param_names(modules: &[Module<'_>], element: usize) -> Vec<Value> {
    let module = &modules[0];
    let Some(call) = module
        .parent(element)
        .filter(|parent| module.is(*parent, "call-template"))
    else {
        return Vec::new();
    };
    let Some(template) = module
        .occurrences
        .iter()
        .find(|occurrence| occurrence.element == call && occurrence.kind == ComponentKind::Template)
        .map(|occurrence| occurrence.name.clone())
    else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for other in modules {
        for occurrence in &other.occurrences {
            if !(occurrence.declaration
                && occurrence.kind == ComponentKind::Template
                && occurrence.name == template)
            {
                continue;
            }
            for child in &other.children[occurrence.element] {
                if let Some(declaration) = other
                    .variable_declaration(*child)
                    .filter(|declaration| other.is(declaration.element, "param"))
                {
                    let name = &other.source[declaration.range.clone()];
                    let mut value = item(name, VARIABLE_KIND, name.to_owned(), false);
                    value["detail"] = json!("template parameter");
                    items.push(value);
                }
            }
        }
    }
    items
}

/// Completion inside an XPath site: variables after `$`, key names in
/// `key('...')`, stylesheet functions elsewhere.
fn xpath_items(modules: &[Module<'_>], offset: usize) -> Vec<Value> {
    let module = &modules[0];
    let Some(site) = module
        .sites
        .iter()
        .find(|site| site.value.start <= offset && offset <= site.value.end)
    else {
        return Vec::new();
    };
    if site.kind == super::stylesheet::SiteKind::ValueTemplate
        && !inside_braces(module.source, site.value.start, offset)
    {
        return Vec::new();
    }
    let source = module.source;
    let word_start = source[site.value.start..offset]
        .char_indices()
        .rev()
        .take_while(|(_, character)| is_ncname_char(*character) || *character == ':')
        .last()
        .map_or(offset, |(index, _)| site.value.start + index);
    let before = &source[site.value.start..word_start];
    if before.ends_with('$') {
        return variable_items(modules, site.element, site.value.start, word_start, site);
    }
    let call = before
        .trim_end()
        .strip_suffix(['\'', '"'])
        .map(str::trim_end)
        .and_then(|text| text.strip_suffix('('))
        .map(str::trim_end);
    if let Some(call) = call {
        let function = call
            .rsplit(|character: char| !(is_ncname_char(character) || character == ':'))
            .next()
            .unwrap_or_default();
        let kind = match function.trim_start_matches("fn:") {
            "key" => Some(ComponentKind::Key),
            "accumulator-before" | "accumulator-after" => Some(ComponentKind::Accumulator),
            _ => None,
        };
        if let Some(kind) = kind {
            return component_names(modules, kind);
        }
    }
    if before.ends_with(['\'', '"']) {
        return Vec::new();
    }
    let mut seen = Vec::new();
    let mut items = Vec::new();
    for other in modules {
        for occurrence in &other.occurrences {
            if occurrence.kind != ComponentKind::Function || !occurrence.declaration {
                continue;
            }
            let name = &other.source[occurrence.range.clone()];
            let arity = occurrence.arity.unwrap_or(0);
            if seen.contains(&(name, arity)) {
                continue;
            }
            seen.push((name, arity));
            let arguments = (1..=arity)
                .map(|index| format!("${{{index}}}"))
                .collect::<Vec<_>>()
                .join(", ");
            let mut value = item(name, FUNCTION_KIND, format!("{name}({arguments})"), true);
            value["detail"] = json!(format!("stylesheet function, arity {arity}"));
            items.push(value);
        }
    }
    items
}

/// The cursor is inside `{...}` of a value template starting at `start`.
fn inside_braces(source: &str, start: usize, offset: usize) -> bool {
    let (expressions, _) = xpath_core::value_template_expressions(&source[start..]);
    expressions
        .iter()
        .any(|range| start + range.start <= offset && offset <= start + range.end)
        || source[start..offset].rfind('{') > source[start..offset].rfind('}')
}

fn variable_items(
    modules: &[Module<'_>],
    element: usize,
    site_start: usize,
    offset: usize,
    site: &super::stylesheet::Site,
) -> Vec<Value> {
    let module = &modules[0];
    let mut names: Vec<(String, &'static str)> = Vec::new();
    for binding in site.analysis.bindings_before(offset) {
        names.push((binding.name.clone(), "range variable"));
    }
    for declaration in module.visible_locals(element, site_start) {
        let kind = if module.is(declaration.element, "param") {
            "parameter"
        } else {
            "local variable"
        };
        names.push((module.source[declaration.range.clone()].to_owned(), kind));
    }
    for other in modules {
        for declaration in other
            .variables
            .iter()
            .filter(|declaration| declaration.global)
        {
            let kind = if other.is(declaration.element, "param") {
                "stylesheet parameter"
            } else {
                "global variable"
            };
            names.push((other.source[declaration.range.clone()].to_owned(), kind));
        }
    }
    let mut seen = Vec::new();
    names
        .into_iter()
        .filter(|(name, _)| {
            let new = !seen.contains(name);
            seen.push(name.clone());
            new
        })
        .map(|(name, kind)| {
            let mut value = item(&name, VARIABLE_KIND, name.clone(), false);
            value["detail"] = json!(kind);
            value
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::catalog::Catalogs;

    fn labels(source_with_cursor: &str) -> Vec<String> {
        let offset = source_with_cursor.find('|').expect("cursor");
        let source = source_with_cursor.replacen('|', "", 1);
        let documents = HashMap::new();
        let catalogs = Catalogs::default();
        let context = XsltContext {
            documents: &documents,
            catalogs: &catalogs,
        };
        completions(&context, "file:///c.xsl", &source, offset)
            .iter()
            .filter_map(|item| item["label"].as_str().map(str::to_owned))
            .collect()
    }

    const HEAD: &str = r#"<xsl:stylesheet version="2.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:f="urn:f">"#;

    #[test]
    fn completes_elements_allowed_in_context() {
        let top = labels(&format!("{HEAD}\n  <|\n</xsl:stylesheet>"));
        assert!(top.contains(&"xsl:template".to_owned()));
        assert!(top.contains(&"xsl:function".to_owned()));
        assert!(!top.contains(&"xsl:value-of".to_owned()));
        assert!(!top.contains(&"xsl:mode".to_owned()), "3.0 only");
        let body = labels(&format!(
            "{HEAD}<xsl:template match=\"/\"><xsl:choose><|</xsl:choose></xsl:template></xsl:stylesheet>"
        ));
        let mut body = body;
        body.sort();
        assert_eq!(body, vec!["xsl:otherwise", "xsl:when"]);
        let instructions = labels(&format!(
            "{HEAD}<xsl:template match=\"/\"><out><xsl:v|</out></xsl:template></xsl:stylesheet>"
        ));
        assert!(instructions.contains(&"xsl:value-of".to_owned()));
        assert!(instructions.contains(&"xsl:for-each-group".to_owned()));
        assert!(!instructions.contains(&"xsl:template".to_owned()));
    }

    #[test]
    fn completes_attributes_and_values() {
        let attributes = labels(&format!(
            "{HEAD}<xsl:template match=\"/\"><xsl:apply-templates |/></xsl:template></xsl:stylesheet>"
        ));
        assert!(attributes.contains(&"select".to_owned()));
        assert!(attributes.contains(&"mode".to_owned()));
        assert!(attributes.contains(&"use-when".to_owned()));
        let values = labels(&format!(
            "{HEAD}<xsl:output method=\"|\"/></xsl:stylesheet>"
        ));
        assert!(values.contains(&"xml".to_owned()) && values.contains(&"json".to_owned()));
        let modes = labels(&format!(
            "{HEAD}<xsl:template match=\"a\" mode=\"toc\"/><xsl:template match=\"/\"><xsl:apply-templates mode=\"|\"/></xsl:template></xsl:stylesheet>"
        ));
        assert!(modes.contains(&"toc".to_owned()) && modes.contains(&"#current".to_owned()));
        let templates = labels(&format!(
            "{HEAD}<xsl:template name=\"header\"><xsl:param name=\"title\"/></xsl:template><xsl:template match=\"/\"><xsl:call-template name=\"|\"/><xsl:call-template name=\"header\"><xsl:with-param name=\"\"/></xsl:call-template></xsl:template></xsl:stylesheet>"
        ));
        assert_eq!(templates, vec!["header".to_owned()]);
        let parameters = labels(&format!(
            "{HEAD}<xsl:template name=\"header\"><xsl:param name=\"title\"/></xsl:template><xsl:template match=\"/\"><xsl:call-template name=\"header\"><xsl:with-param name=\"|\"/></xsl:call-template></xsl:template></xsl:stylesheet>"
        ));
        assert_eq!(parameters, vec!["title".to_owned()]);
    }

    #[test]
    fn completes_variables_keys_and_functions_in_xpath() {
        let variables = labels(&format!(
            "{HEAD}<xsl:param name=\"global\"/><xsl:template match=\"/\"><xsl:variable name=\"local\" select=\"1\"/><xsl:value-of select=\"for $i in 1 return $|\"/><xsl:variable name=\"later\"/></xsl:template></xsl:stylesheet>"
        ));
        assert_eq!(variables, vec!["i", "local", "global"]);
        let keys = labels(&format!(
            "{HEAD}<xsl:key name=\"by-id\" match=\"*\" use=\"@id\"/><xsl:template match=\"/\"><xsl:value-of select=\"key('|')\"/></xsl:template></xsl:stylesheet>"
        ));
        assert_eq!(keys, vec!["by-id"]);
        let functions = labels(&format!(
            "{HEAD}<xsl:function name=\"f:twice\"><xsl:param name=\"n\"/></xsl:function><xsl:template match=\"/\"><out a=\"{{f:|}}\"/></xsl:template></xsl:stylesheet>"
        ));
        assert_eq!(functions, vec!["f:twice"]);
    }
}
