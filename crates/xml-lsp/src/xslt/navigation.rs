//! Definition, references, rename and hover of XSLT components and
//! variables.

use std::ops::Range;

use serde_json::{Map, Value, json};
use xml_core::tags::XmlTagKind;
use xpath_core::BindingKind;

use super::{
    Texts, XsltContext,
    stylesheet::{ExpandedName, Module, WithParam},
    vocabulary::{self, ComponentKind, Syntax},
};
use crate::{
    rename::{self, INVALID_PARAMS, RenameError},
    selection::LineIndex,
    uri_to_path,
};

/// Named thing under the cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Symbol {
    /// `xsl:variable`/`xsl:param` element of a module.
    Variable { module: usize, element: usize },
    /// `$name` without a visible declaration.
    Unresolved(ExpandedName),
    /// Range variable (`for`, `let`, `some`, `every`, inline function
    /// parameter) of an XPath site.
    Range {
        module: usize,
        site: usize,
        binding: usize,
    },
    Component {
        kind: ComponentKind,
        name: ExpandedName,
        /// Arity of a function.
        arity: Option<usize>,
    },
}

pub(super) struct Hit {
    pub(super) symbol: Symbol,
    /// Range of the name under the cursor.
    pub(super) range: Range<usize>,
}

/// One occurrence of a symbol.
struct Location {
    module: usize,
    range: Range<usize>,
    declaration: bool,
}

fn contains(range: &Range<usize>, offset: usize) -> bool {
    range.start <= offset && offset <= range.end
}

/// Symbol at `offset` of the first module.
pub(super) fn symbol_at(modules: &[Module<'_>], offset: usize) -> Option<Hit> {
    let module = modules.first()?;
    for (site_index, site) in module.sites.iter().enumerate() {
        if !contains(&site.value, offset) {
            continue;
        }
        for variable in &site.analysis.variables {
            // The cursor may be on the `$`.
            if contains(
                &(variable.range.start.saturating_sub(1)..variable.range.end),
                offset,
            ) {
                let symbol = match variable.binding {
                    Some(binding) => Symbol::Range {
                        module: 0,
                        site: site_index,
                        binding,
                    },
                    None => {
                        resolve_variable(modules, 0, site.element, site.value.start, &variable.name)
                    }
                };
                return Some(Hit {
                    symbol,
                    range: variable.range.clone(),
                });
            }
        }
        for (binding_index, binding) in site.analysis.bindings.iter().enumerate() {
            if contains(&binding.range, offset) {
                return Some(Hit {
                    symbol: Symbol::Range {
                        module: 0,
                        site: site_index,
                        binding: binding_index,
                    },
                    range: binding.range.clone(),
                });
            }
        }
    }
    if let Some(occurrence) = module
        .occurrences
        .iter()
        .find(|occurrence| contains(&occurrence.range, offset))
    {
        return Some(Hit {
            symbol: Symbol::Component {
                kind: occurrence.kind,
                name: occurrence.name.clone(),
                arity: occurrence.arity,
            },
            range: occurrence.range.clone(),
        });
    }
    if let Some(variable) = module
        .variables
        .iter()
        .find(|variable| contains(&variable.range, offset))
    {
        return Some(Hit {
            symbol: Symbol::Variable {
                module: 0,
                element: variable.element,
            },
            range: variable.range.clone(),
        });
    }
    let with_param = module
        .with_params
        .iter()
        .find(|with_param| contains(&with_param.range, offset))?;
    Some(Hit {
        symbol: with_param_target(modules, 0, with_param)
            .unwrap_or_else(|| Symbol::Unresolved(with_param.name.clone())),
        range: with_param.range.clone(),
    })
}

/// Declaration of `$name` read at `offset` of `element` in module
/// `index`: local variable, else global variable of that module, else of
/// the other modules.
fn resolve_variable(
    modules: &[Module<'_>],
    index: usize,
    element: usize,
    offset: usize,
    name: &str,
) -> Symbol {
    let module = &modules[index];
    let expanded = module.expand(element, name);
    if let Some(declaration) = module.local_variable(element, offset, &expanded) {
        return Symbol::Variable {
            module: index,
            element: declaration,
        };
    }
    std::iter::once(index)
        .chain((0..modules.len()).filter(|other| *other != index))
        .find_map(|candidate| {
            modules[candidate]
                .global_variable(&expanded)
                .map(|declaration| Symbol::Variable {
                    module: candidate,
                    element: declaration.element,
                })
        })
        .unwrap_or(Symbol::Unresolved(expanded))
}

/// Parameter receiving `xsl:with-param`: the `xsl:param` of the called
/// named template, or of the enclosing `xsl:iterate` for
/// `xsl:next-iteration`.
fn with_param_target(
    modules: &[Module<'_>],
    index: usize,
    with_param: &WithParam,
) -> Option<Symbol> {
    let module = &modules[index];
    let parent = module.parent(with_param.element)?;
    let parameter_of = |module_index: usize, owner: usize| {
        let owner_module = &modules[module_index];
        owner_module.children[owner]
            .iter()
            .copied()
            .find(|child| {
                owner_module.is(*child, "param")
                    && owner_module
                        .variable_declaration(*child)
                        .is_some_and(|declaration| declaration.name == with_param.name)
            })
            .map(|element| Symbol::Variable {
                module: module_index,
                element,
            })
    };
    if module.is(parent, "call-template") {
        let template = module
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.element == parent && occurrence.kind == ComponentKind::Template
            })?
            .name
            .clone();
        return modules
            .iter()
            .enumerate()
            .find_map(|(module_index, other)| {
                other
                    .occurrences
                    .iter()
                    .filter(|occurrence| {
                        occurrence.declaration
                            && occurrence.kind == ComponentKind::Template
                            && occurrence.name == template
                    })
                    .find_map(|occurrence| parameter_of(module_index, occurrence.element))
            });
    }
    if module.is(parent, "next-iteration") {
        let iterate = module
            .document
            .tree
            .ancestors(parent)
            .find(|ancestor| module.is(*ancestor, "iterate"))?;
        return parameter_of(index, iterate);
    }
    None
}

/// Every occurrence of `symbol` in the modules.
fn occurrences(modules: &[Module<'_>], symbol: &Symbol) -> Vec<Location> {
    let mut locations = Vec::new();
    match symbol {
        Symbol::Variable { module, element } => {
            let Some(declaration) = modules[*module].variable_declaration(*element) else {
                return locations;
            };
            locations.push(Location {
                module: *module,
                range: declaration.range.clone(),
                declaration: true,
            });
            for (index, other) in modules.iter().enumerate() {
                for site in &other.sites {
                    for variable in &site.analysis.variables {
                        if variable.binding.is_none()
                            && other.expand(site.element, &variable.name) == declaration.name
                            && resolve_variable(
                                modules,
                                index,
                                site.element,
                                site.value.start,
                                &variable.name,
                            ) == *symbol
                        {
                            locations.push(Location {
                                module: index,
                                range: variable.range.clone(),
                                declaration: false,
                            });
                        }
                    }
                }
                for with_param in &other.with_params {
                    if with_param.name == declaration.name
                        && with_param_target(modules, index, with_param).as_ref() == Some(symbol)
                    {
                        locations.push(Location {
                            module: index,
                            range: with_param.range.clone(),
                            declaration: false,
                        });
                    }
                }
            }
        }
        Symbol::Unresolved(name) => {
            for (index, other) in modules.iter().enumerate() {
                for site in &other.sites {
                    for variable in &site.analysis.variables {
                        if variable.binding.is_none()
                            && other.expand(site.element, &variable.name) == *name
                            && resolve_variable(
                                modules,
                                index,
                                site.element,
                                site.value.start,
                                &variable.name,
                            ) == *symbol
                        {
                            locations.push(Location {
                                module: index,
                                range: variable.range.clone(),
                                declaration: false,
                            });
                        }
                    }
                }
            }
        }
        Symbol::Range {
            module,
            site,
            binding,
        } => {
            let analysis = &modules[*module].sites[*site].analysis;
            locations.push(Location {
                module: *module,
                range: analysis.bindings[*binding].range.clone(),
                declaration: true,
            });
            for variable in &analysis.variables {
                if variable.binding == Some(*binding) {
                    locations.push(Location {
                        module: *module,
                        range: variable.range.clone(),
                        declaration: false,
                    });
                }
            }
        }
        Symbol::Component { kind, name, arity } => {
            for (index, other) in modules.iter().enumerate() {
                for occurrence in &other.occurrences {
                    let arity_matches = *kind != ComponentKind::Function
                        || arity.is_none()
                        || occurrence.arity.is_none()
                        || occurrence.arity == *arity;
                    if occurrence.kind == *kind && occurrence.name == *name && arity_matches {
                        locations.push(Location {
                            module: index,
                            range: occurrence.range.clone(),
                            declaration: occurrence.declaration,
                        });
                    }
                }
            }
        }
    }
    locations.sort_by_key(|location| (location.module, location.range.start));
    locations.dedup_by(|left, right| left.module == right.module && left.range == right.range);
    locations
}

/// Declarations of `symbol`; for a mode without `xsl:mode`, the template
/// rules of the mode.
fn definitions(modules: &[Module<'_>], symbol: &Symbol) -> Vec<Location> {
    let all = occurrences(modules, symbol);
    let declarations = all
        .iter()
        .filter(|location| location.declaration)
        .map(|location| Location {
            module: location.module,
            range: location.range.clone(),
            declaration: true,
        })
        .collect::<Vec<_>>();
    if !declarations.is_empty() {
        return declarations;
    }
    if let Symbol::Component {
        kind: ComponentKind::Mode,
        name,
        ..
    } = symbol
    {
        return modules
            .iter()
            .enumerate()
            .flat_map(|(index, module)| {
                module
                    .occurrences
                    .iter()
                    .filter(|occurrence| {
                        occurrence.kind == ComponentKind::Mode
                            && occurrence.name == *name
                            && module.is(occurrence.element, "template")
                    })
                    .map(move |occurrence| Location {
                        module: index,
                        range: occurrence.range.clone(),
                        declaration: false,
                    })
            })
            .collect();
    }
    Vec::new()
}

struct Positions<'m> {
    modules: &'m [Module<'m>],
    lines: Vec<Option<LineIndex>>,
}

impl<'m> Positions<'m> {
    fn new(modules: &'m [Module<'m>]) -> Self {
        Self {
            modules,
            lines: (0..modules.len()).map(|_| None).collect(),
        }
    }

    fn range(&mut self, module: usize, range: &Range<usize>) -> Value {
        let source = self.modules[module].source;
        let lines = self.lines[module].get_or_insert_with(|| LineIndex::new(source));
        json!({
            "start": lines.position(source, range.start),
            "end": lines.position(source, range.end),
        })
    }

    fn location(&mut self, location: &Location) -> Value {
        json!({
            "uri": self.modules[location.module].uri,
            "range": self.range(location.module, &location.range),
        })
    }
}

/// `textDocument/definition` on an XSLT reference; `None` when the cursor
/// is on no XSLT symbol with a declaration.
/// Modules of a request on the symbol at `offset`, loaded only when the
/// document has a symbol there (other modules are not read otherwise).
fn symbol_texts(
    context: &XsltContext<'_>,
    uri: &str,
    source: &str,
    offset: usize,
) -> Option<Texts> {
    let single = Texts::single(uri, source)?;
    symbol_at(&single.modules()?, offset)?;
    Texts::load(context, uri, source)
}

pub(crate) fn definition(
    context: &XsltContext<'_>,
    uri: &str,
    source: &str,
    offset: usize,
    link_support: bool,
) -> Option<Value> {
    let texts = symbol_texts(context, uri, source, offset)?;
    let modules = texts.modules()?;
    let hit = symbol_at(&modules, offset)?;
    let targets = definitions(&modules, &hit.symbol);
    if targets.is_empty() {
        return None;
    }
    let mut positions = Positions::new(&modules);
    let origin = positions.range(0, &hit.range);
    Some(Value::Array(
        targets
            .iter()
            .map(|target| {
                if link_support {
                    let range = positions.range(target.module, &target.range);
                    json!({
                        "originSelectionRange": origin,
                        "targetUri": modules[target.module].uri,
                        "targetRange": range,
                        "targetSelectionRange": range,
                    })
                } else {
                    positions.location(target)
                }
            })
            .collect(),
    ))
}

/// `textDocument/references` of the XSLT symbol at `offset`.
pub(crate) fn references(
    context: &XsltContext<'_>,
    uri: &str,
    source: &str,
    offset: usize,
    include_declaration: bool,
) -> Option<Value> {
    let texts = symbol_texts(context, uri, source, offset)?;
    let modules = texts.modules()?;
    let hit = symbol_at(&modules, offset)?;
    let mut positions = Positions::new(&modules);
    Some(Value::Array(
        occurrences(&modules, &hit.symbol)
            .iter()
            .filter(|location| include_declaration || !location.declaration)
            .map(|location| positions.location(location))
            .collect(),
    ))
}

/// `textDocument/prepareRename` on an XSLT symbol.
pub(crate) fn prepare_rename(uri: &str, source: &str, offset: usize) -> Option<Value> {
    let texts = Texts::single(uri, source)?;
    let modules = texts.modules()?;
    let hit = symbol_at(&modules, offset)?;
    let mut positions = Positions::new(&modules);
    Some(json!({
        "range": positions.range(0, &hit.range),
        "placeholder": &source[hit.range.clone()],
    }))
}

/// `textDocument/rename` of an XSLT symbol: a `WorkspaceEdit` over every
/// module; `None` when the cursor is on no XSLT symbol.
pub(crate) fn rename(
    context: &XsltContext<'_>,
    uri: &str,
    source: &str,
    offset: usize,
    new_name: &str,
) -> Option<Result<Value, RenameError>> {
    let texts = symbol_texts(context, uri, source, offset)?;
    let modules = texts.modules()?;
    let hit = symbol_at(&modules, offset)?;
    if !xpath_core::is_qname(new_name) {
        return Some(Err(RenameError {
            code: INVALID_PARAMS,
            message: format!("'{new_name}' is not a valid XSLT name (QName expected)."),
        }));
    }
    let mut changes = Map::new();
    let locations = occurrences(&modules, &hit.symbol);
    for (index, module) in modules.iter().enumerate() {
        let ranges = locations
            .iter()
            .filter(|location| location.module == index)
            .map(|location| location.range.clone())
            .collect::<Vec<_>>();
        if !ranges.is_empty() {
            changes.insert(
                module.uri.to_owned(),
                Value::Array(rename::text_edits(module.source, &ranges, new_name)),
            );
        }
    }
    Some(Ok(json!({"changes": changes})))
}

/// `textDocument/hover` on XSLT elements, attributes and references.
pub(crate) fn hover(
    context: &XsltContext<'_>,
    uri: &str,
    source: &str,
    offset: usize,
) -> Option<Value> {
    let single = Texts::single(uri, source)?;
    let texts = match symbol_at(&single.modules()?, offset) {
        Some(_) => Texts::load(context, uri, source)?,
        None => single,
    };
    let modules = texts.modules()?;
    let (markdown, range) =
        symbol_hover(&modules, offset).or_else(|| element_hover(&modules[0], offset))?;
    let mut positions = Positions::new(&modules);
    Some(json!({
        "contents": {"kind": "markdown", "value": markdown},
        "range": positions.range(0, &range),
    }))
}

fn symbol_hover(modules: &[Module<'_>], offset: usize) -> Option<(String, Range<usize>)> {
    let hit = symbol_at(modules, offset)?;
    let markdown = match &hit.symbol {
        Symbol::Unresolved(_) => return None,
        Symbol::Range {
            module,
            site,
            binding,
        } => {
            let binding = &modules[*module].sites[*site].analysis.bindings[*binding];
            let construct = match binding.kind {
                BindingKind::For => "`for` range variable",
                BindingKind::Let => "`let` variable",
                BindingKind::Some => "`some` range variable",
                BindingKind::Every => "`every` range variable",
                BindingKind::Parameter => "inline function parameter",
            };
            format!("**${}**: {construct}", binding.name)
        }
        Symbol::Variable { module, element } => {
            let declaring = &modules[*module];
            let declaration = declaring.variable_declaration(*element)?;
            let kind = match (declaring.is(*element, "param"), declaration.global) {
                (true, true) => "Stylesheet parameter",
                (true, false) => "Parameter",
                (false, true) => "Global variable",
                (false, false) => "Local variable",
            };
            declaration_markdown(modules, *module, *element, kind)
        }
        Symbol::Component { kind, .. } => {
            let target = definitions(modules, &hit.symbol)
                .into_iter()
                .find(|location| location.declaration)?;
            let module = &modules[target.module];
            let element = module
                .occurrences
                .iter()
                .find(|occurrence| occurrence.range == target.range && occurrence.declaration)?
                .element;
            let mut markdown = declaration_markdown(modules, target.module, element, kind.label());
            let parameters = module.children[element]
                .iter()
                .filter_map(|child| module.variable_declaration(*child))
                .filter(|declaration| module.is(declaration.element, "param"))
                .map(|declaration| {
                    let as_type = module
                        .attribute_value(declaration.element, "as")
                        .map(|value| format!(" as {value}"))
                        .unwrap_or_default();
                    format!(
                        "- `${}`{as_type}",
                        &module.source[declaration.range.clone()]
                    )
                })
                .collect::<Vec<_>>();
            if !parameters.is_empty() {
                markdown.push_str("\n\nParameters:\n");
                markdown.push_str(&parameters.join("\n"));
            }
            markdown
        }
    };
    Some((markdown, hit.range))
}

fn declaration_markdown(
    modules: &[Module<'_>],
    module: usize,
    element: usize,
    kind: &str,
) -> String {
    let declaring = &modules[module];
    let mut start_tag = declaring.start_tag(element).to_owned();
    if start_tag.len() > 500 {
        let mut end = 500;
        while !start_tag.is_char_boundary(end) {
            end -= 1;
        }
        start_tag.truncate(end);
        start_tag.push('…');
    }
    let mut markdown = format!("**{kind}**\n\n```xml\n{start_tag}\n```");
    if module != 0 {
        let path = uri_to_path(declaring.uri);
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| declaring.uri.to_owned());
        markdown.push_str(&format!("\n\nDeclared in `{file}`"));
    }
    markdown
}

/// Hover on the name of an XSLT element or of one of its attributes.
fn element_hover(module: &Module<'_>, offset: usize) -> Option<(String, Range<usize>)> {
    let elements = module.document.tree.elements();
    for (index, element) in elements.iter().enumerate() {
        let Some(local) = module.xslt[index] else {
            continue;
        };
        let start = &element.start_tag;
        if start.range.start > offset {
            break;
        }
        let definition = vocabulary::element(local);
        let on_name = start.name_contains(offset)
            || element
                .end_tag
                .as_ref()
                .is_some_and(|end| end.kind == XmlTagKind::End && end.name_contains(offset));
        if on_name {
            let range = if start.name_contains(offset) {
                start.name.clone()
            } else {
                element.end_tag.as_ref()?.name.clone()
            };
            let definition = definition?;
            return Some((element_markdown(definition), range));
        }
        if contains(&start.range, offset) {
            let attribute = module.document.attributes[index]
                .iter()
                .find(|attribute| contains(&attribute.name, offset))?;
            let name = attribute.name(module.source);
            let definition = definition?.attribute(name)?;
            return Some((
                attribute_markdown(local, definition),
                attribute.name.clone(),
            ));
        }
    }
    None
}

fn since_label(since: vocabulary::Since) -> &'static str {
    match since {
        10 => "XSLT 1.0",
        20 => "XSLT 2.0",
        _ => "XSLT 3.0",
    }
}

pub(super) fn element_markdown(definition: &vocabulary::Element) -> String {
    let mut markdown = format!(
        "**xsl:{}** ({})\n\n{}",
        definition.name,
        since_label(definition.since),
        definition.documentation
    );
    if !definition.attributes.is_empty() {
        let attributes = definition
            .attributes
            .iter()
            .map(|attribute| {
                if attribute.required {
                    format!("**`{}`**", attribute.name)
                } else {
                    format!("`{}`", attribute.name)
                }
            })
            .collect::<Vec<_>>();
        markdown.push_str(&format!(
            "\n\nAttributes (required in bold): {}",
            attributes.join(", ")
        ));
    }
    markdown
}

pub(super) fn syntax_label(syntax: Syntax) -> &'static str {
    match syntax {
        Syntax::Expression => "XPath expression",
        Syntax::Pattern => "pattern",
        Syntax::Avt => "attribute value template",
        Syntax::SequenceType => "sequence type",
        Syntax::Text => "value",
    }
}

pub(super) fn attribute_markdown(element: &str, attribute: &vocabulary::Attribute) -> String {
    let mut markdown = format!(
        "**{}** on `xsl:{element}`: {}{} ({})",
        attribute.name,
        syntax_label(attribute.syntax),
        if attribute.required { ", required" } else { "" },
        since_label(attribute.since)
    );
    if !attribute.values.is_empty() {
        markdown.push_str(&format!(
            "\n\nValues: {}",
            attribute
                .values
                .iter()
                .map(|value| format!("`{value}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(role) = attribute.role {
        let (verb, kind) = match role {
            vocabulary::Role::Declares(kind) => ("Declares", kind),
            vocabulary::Role::Refers(kind) => ("Refers to", kind),
            vocabulary::Role::RefersList(kind) => ("Lists", kind),
        };
        markdown.push_str(&format!("\n\n{verb} a {}.", kind.label()));
    }
    markdown
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"<xsl:stylesheet version="3.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform" xmlns:f="urn:f">
  <xsl:param name="x" select="0"/>
  <xsl:key name="k" match="item" use="@id"/>
  <xsl:mode name="m"/>
  <xsl:template match="a" mode="m">
    <xsl:variable name="x" select="$x + 1"/>
    <xsl:variable name="x" select="$x * 2"/>
    <xsl:value-of select="for $x in $x return $x, key('k', 'a')"/>
    <xsl:apply-templates mode="m"/>
  </xsl:template>
  <xsl:template match="b"><xsl:value-of select="$x"/></xsl:template>
  <xsl:function name="f:g"><xsl:param name="p"/><xsl:sequence select="$p"/></xsl:function>
  <xsl:function name="f:g"><xsl:param name="p"/><xsl:param name="q"/><xsl:sequence select="f:g($p) + f:g($p, $q)"/></xsl:function>
  <xsl:template match="c">
    <xsl:iterate select="*">
      <xsl:param name="n" select="0"/>
      <xsl:next-iteration><xsl:with-param name="n" select="$n + 1"/></xsl:next-iteration>
    </xsl:iterate>
  </xsl:template>
</xsl:stylesheet>"#;

    /// Line and text of the occurrences of the symbol at the `index`-th
    /// occurrence of `needle` (cursor on its last character).
    fn occurrences_of(needle: &str, index: usize) -> Vec<(usize, &'static str)> {
        let offset = SOURCE
            .match_indices(needle)
            .nth(index)
            .map(|(offset, _)| offset + needle.len() - 1)
            .expect("needle");
        let modules = vec![Module::parse("file:///n.xsl", SOURCE).expect("stylesheet")];
        let hit = symbol_at(&modules, offset).expect("symbol");
        occurrences(&modules, &hit.symbol)
            .into_iter()
            .map(|location| {
                let line = SOURCE[..location.range.start].matches('\n').count();
                (line, &SOURCE[location.range])
            })
            .collect()
    }

    #[test]
    fn resolves_shadowed_variables_by_xslt_scope() {
        // Global `$x`: the reference in the first local variable and the one
        // in the other template.
        assert_eq!(
            occurrences_of("name=\"x", 0),
            vec![(1, "x"), (5, "x"), (10, "x")]
        );
        // First local `x`: used by the second one.
        assert_eq!(occurrences_of("name=\"x", 1), vec![(5, "x"), (6, "x")]);
        // Second local `x`: the `in` expression of the `for`.
        assert_eq!(occurrences_of("name=\"x", 2), vec![(6, "x"), (7, "x")]);
        // Range variable of the `for`.
        assert_eq!(occurrences_of("for $x", 0), vec![(7, "x"), (7, "x")]);
    }

    #[test]
    fn finds_components_by_kind_name_and_arity() {
        assert_eq!(occurrences_of("key('k", 0), vec![(2, "k"), (7, "k")]);
        assert_eq!(
            occurrences_of("mode=\"m", 1),
            vec![(3, "m"), (4, "m"), (8, "m")]
        );
        assert_eq!(occurrences_of("f:g", 2), vec![(11, "f:g"), (12, "f:g")]);
        assert_eq!(occurrences_of("f:g", 3), vec![(12, "f:g"), (12, "f:g")]);
        assert_eq!(
            occurrences_of("with-param name=\"n", 0),
            vec![(15, "n"), (16, "n"), (16, "n")]
        );
    }
}
