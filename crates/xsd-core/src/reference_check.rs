//! Schema component constraints on references and declarations that need
//! the whole set of schemas: references that resolve to nothing, duplicate
//! component names, circular definitions, `default`/`fixed` values that are
//! not valid for their type, and the attribute uses of complex types.
//!
//! References are only reported as unresolved when the set is complete (every
//! `xs:include` and `xs:import` with a location was loaded) and the namespace
//! they name is not imported without being loaded.

use std::collections::{HashMap, HashSet};

use crate::{
    datatypes::{BuiltinType, IdKind},
    model::{
        Located, XsdAttributeDecl, XsdDerivation, XsdElementDecl, XsdModel, XsdModelSet,
        XsdParticle, XsdQName, XsdTypeDef, XsdTypeRef, XsdUse,
    },
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Space {
    Type,
    Element,
    Attribute,
    Group,
    AttributeGroup,
}

impl Space {
    fn name(self) -> &'static str {
        match self {
            Self::Type => "type",
            Self::Element => "element",
            Self::Attribute => "attribute",
            Self::Group => "group",
            Self::AttributeGroup => "attribute group",
        }
    }
}

struct Use<'a> {
    space: Space,
    name: &'a XsdQName,
    label: String,
}

fn visit_element<'a>(
    declaration: &'a XsdElementDecl,
    out: &mut Vec<Use<'a>>,
    types: &mut Vec<(&'a XsdTypeDef, String)>,
) {
    let label = format!("element '{}'", declaration.name);
    if let Some(reference) = &declaration.reference {
        out.push(Use {
            space: Space::Element,
            name: reference,
            label: label.clone(),
        });
    }
    if let Some(name) = &declaration.type_name {
        out.push(Use {
            space: Space::Type,
            name,
            label: label.clone(),
        });
    }
    for head in &declaration.substitution_groups {
        out.push(Use {
            space: Space::Element,
            name: head,
            label: format!("{label} (substitutionGroup)"),
        });
    }
    if let Some(anonymous) = &declaration.anonymous_type {
        types.push((anonymous, format!("the type of {label}")));
    }
}

fn visit_particles<'a>(
    particle: &'a XsdParticle,
    label: &str,
    out: &mut Vec<Use<'a>>,
    types: &mut Vec<(&'a XsdTypeDef, String)>,
) {
    let mut pending = vec![particle];
    while let Some(particle) = pending.pop() {
        match particle {
            XsdParticle::Element(declaration) => visit_element(declaration, out, types),
            XsdParticle::Group { particles, .. } => pending.extend(particles),
            XsdParticle::GroupRef { name, .. } => out.push(Use {
                space: Space::Group,
                name,
                label: label.to_owned(),
            }),
            XsdParticle::Any(_) => {}
        }
    }
}

fn visit_attribute<'a>(
    declaration: &'a XsdAttributeDecl,
    label: &str,
    out: &mut Vec<Use<'a>>,
    types: &mut Vec<(&'a XsdTypeDef, String)>,
) {
    let label = format!("attribute '{}' of {label}", declaration.name);
    if let Some(reference) = &declaration.reference {
        out.push(Use {
            space: Space::Attribute,
            name: reference,
            label: label.clone(),
        });
    }
    if let Some(name) = &declaration.type_name {
        out.push(Use {
            space: Space::Type,
            name,
            label: label.clone(),
        });
    }
    if let Some(anonymous) = &declaration.anonymous_type {
        types.push((anonymous, label));
    }
}

/// Every reference of a model, with a label naming the referring component.
fn uses(model: &XsdModel) -> Vec<Use<'_>> {
    let mut out = Vec::new();
    let mut types: Vec<(&XsdTypeDef, String)> = Vec::new();
    for declaration in &model.elements {
        visit_element(declaration, &mut out, &mut types);
    }
    for declaration in &model.attributes {
        visit_attribute(declaration, "the schema", &mut out, &mut types);
    }
    for definition in &model.types {
        let label = definition
            .name
            .as_ref()
            .map_or("a type".to_owned(), |name| format!("type '{name}'"));
        types.push((definition, label));
    }
    for group in &model.groups {
        if let Some(content) = &group.content {
            let label = format!("group '{}'", group.name);
            visit_particles(content, &label, &mut out, &mut types);
        }
    }
    for group in &model.attribute_groups {
        let label = format!("attribute group '{}'", group.name);
        for declaration in &group.attributes {
            visit_attribute(declaration, &label, &mut out, &mut types);
        }
        for name in &group.attribute_group_refs {
            out.push(Use {
                space: Space::AttributeGroup,
                name,
                label: label.clone(),
            });
        }
    }
    while let Some((definition, label)) = types.pop() {
        for name in definition
            .base
            .iter()
            .chain(definition.item_type.iter())
            .chain(definition.member_types.iter())
        {
            out.push(Use {
                space: Space::Type,
                name,
                label: label.clone(),
            });
        }
        for name in &definition.attribute_group_refs {
            out.push(Use {
                space: Space::AttributeGroup,
                name,
                label: label.clone(),
            });
        }
        for declaration in &definition.attributes {
            visit_attribute(declaration, &label, &mut out, &mut types);
        }
        if let Some(content) = &definition.content {
            visit_particles(content, &label, &mut out, &mut types);
        }
        for inline in &definition.inline_types {
            types.push((inline, label.clone()));
        }
    }
    out
}

fn is_builtin_type(name: &XsdQName) -> bool {
    name.is_builtin()
        && (BuiltinType::from_local_name(&name.local).is_some() || name.local == "anyType")
}

impl XsdModelSet {
    /// Effective declaration of an attribute use: the global declaration for
    /// a `ref`.
    fn attribute_target<'a>(
        &'a self,
        usage: Located<'a, XsdAttributeDecl>,
    ) -> Located<'a, XsdAttributeDecl> {
        usage
            .item
            .reference
            .as_ref()
            .and_then(|reference| {
                self.global_attribute(reference.namespace.as_deref(), &reference.local)
            })
            .unwrap_or(usage)
    }

    /// Problems of references, duplicate names, cycles and default values.
    /// `complete`: every schema document the set refers to was loaded;
    /// `imported`: namespaces named by an `xs:import`.
    pub fn reference_problems(&self, complete: bool, imported: &HashSet<String>) -> Vec<String> {
        let mut problems = Vec::new();
        self.unresolved_references(complete, imported, &mut problems);
        self.duplicate_names(&mut problems);
        self.cycles(&mut problems);
        self.default_values(&mut problems);
        self.attribute_uses_problems(&mut problems);
        problems.sort();
        problems.dedup();
        problems
    }

    fn unresolved_references(
        &self,
        complete: bool,
        imported: &HashSet<String>,
        problems: &mut Vec<String>,
    ) {
        let namespaces: HashSet<Option<&str>> = self
            .models()
            .iter()
            .map(|model| model.target_namespace.as_deref())
            .collect();
        for model in self.models() {
            for reference in uses(model) {
                let name = reference.name;
                let resolved = match reference.space {
                    Space::Type => {
                        is_builtin_type(name)
                            || (!name.is_builtin()
                                && self
                                    .global_type(name.namespace.as_deref(), &name.local)
                                    .is_some())
                    }
                    Space::Element => self
                        .global_element(name.namespace.as_deref(), &name.local)
                        .is_some(),
                    Space::Attribute => {
                        name.namespace.as_deref() == Some(crate::model::XML_NAMESPACE)
                            || name.namespace.as_deref()
                                == Some("http://www.w3.org/2001/XMLSchema-instance")
                            || self
                                .global_attribute(name.namespace.as_deref(), &name.local)
                                .is_some()
                    }
                    Space::Group => self.group(name.namespace.as_deref(), &name.local).is_some(),
                    Space::AttributeGroup => self
                        .attribute_group(name.namespace.as_deref(), &name.local)
                        .is_some(),
                };
                if resolved {
                    continue;
                }
                // A name of the XML Schema namespace is always decidable.
                let decidable = name.is_builtin()
                    || (complete
                        && match name.namespace.as_deref() {
                            Some(namespace) => {
                                namespaces.contains(&Some(namespace))
                                    || !imported.contains(namespace)
                            }
                            None => true,
                        });
                if decidable {
                    problems.push(format!(
                        "{} refers to the {} '{}', which is not declared",
                        reference.label,
                        reference.space.name(),
                        name.display()
                    ));
                }
            }
        }
    }

    fn duplicate_names(&self, problems: &mut Vec<String>) {
        // Names are compared through owned keys to keep the borrow simple.
        let mut names: HashMap<(&'static str, Option<String>, String), usize> = HashMap::new();
        for (schema, model) in self.models().iter().enumerate() {
            let namespace = model.target_namespace.clone();
            let redefined = |kind: &str, name: &str| {
                self.models().iter().any(|other| {
                    other
                        .redefined
                        .iter()
                        .any(|(other_kind, other_name)| *other_kind == kind && other_name == name)
                })
            };
            let mut record = |space: &'static str, kind: &str, local: &str| {
                if redefined(kind, local) {
                    return;
                }
                let key = (space, namespace.clone(), local.to_owned());
                match names.get(&key) {
                    Some(first) if *first == schema => problems.push(format!(
                        "the {space} '{local}' is declared twice in the same schema"
                    )),
                    Some(_) => problems.push(format!(
                        "the {space} '{local}' is declared in several schemas of the same namespace"
                    )),
                    None => {
                        names.insert(key, schema);
                    }
                }
            };
            for definition in &model.types {
                if let Some(name) = &definition.name {
                    record("type", "type", name);
                }
            }
            for declaration in &model.elements {
                record("element", "element", &declaration.name);
            }
            for declaration in &model.attributes {
                record("attribute", "attribute", &declaration.name);
            }
            for group in &model.groups {
                record("group", "group", &group.name);
            }
            for group in &model.attribute_groups {
                record("attribute group", "attributeGroup", &group.name);
            }
        }
    }

    fn cycles(&self, problems: &mut Vec<String>) {
        // Type derivation.
        for (schema, model) in self.models().iter().enumerate() {
            for definition in &model.types {
                let Some(name) = &definition.name else {
                    continue;
                };
                let mut current = XsdTypeRef {
                    schema,
                    name: None,
                    definition: Some(definition),
                };
                let mut seen: Vec<*const XsdTypeDef> = vec![definition];
                for _ in 0..crate::model::MAX_DEPTH {
                    let Some(base) = self.base_type(current) else {
                        break;
                    };
                    let Some(base_definition) = base.definition else {
                        break;
                    };
                    if seen.contains(&std::ptr::from_ref(base_definition)) {
                        problems.push(format!("the type '{name}' is derived from itself"));
                        break;
                    }
                    seen.push(base_definition);
                    current = base;
                }
            }
        }
        // Groups and attribute groups referencing themselves.
        for model in self.models() {
            for group in &model.groups {
                if model
                    .redefined
                    .iter()
                    .any(|(kind, name)| *kind == "group" && *name == group.name)
                {
                    continue;
                }
                let namespace = group.namespace.as_deref();
                if let Some(content) = &group.content
                    && self.group_reaches(content, namespace, &group.name, 0)
                {
                    problems.push(format!("the group '{}' contains itself", group.name));
                }
            }
            for group in &model.attribute_groups {
                if model
                    .redefined
                    .iter()
                    .any(|(kind, name)| *kind == "attributeGroup" && *name == group.name)
                {
                    continue;
                }
                let mut pending: Vec<(&XsdQName, usize)> = group
                    .attribute_group_refs
                    .iter()
                    .map(|name| (name, 0))
                    .collect();
                while let Some((name, depth)) = pending.pop() {
                    if depth > crate::model::MAX_DEPTH {
                        break;
                    }
                    if name.local == group.name && name.namespace == group.namespace {
                        problems.push(format!(
                            "the attribute group '{}' contains itself",
                            group.name
                        ));
                        break;
                    }
                    if let Some(found) =
                        self.attribute_group(name.namespace.as_deref(), &name.local)
                    {
                        pending.extend(
                            found
                                .item
                                .attribute_group_refs
                                .iter()
                                .map(|next| (next, depth + 1)),
                        );
                    }
                }
            }
        }
        // Substitution groups.
        for model in self.models() {
            for declaration in &model.elements {
                let mut current = declaration;
                for _ in 0..crate::model::MAX_DEPTH {
                    let Some(head) = current.substitution_groups.first() else {
                        break;
                    };
                    let Some(found) = self.global_element(head.namespace.as_deref(), &head.local)
                    else {
                        break;
                    };
                    if std::ptr::eq(found.item, declaration) {
                        problems.push(format!(
                            "the element '{}' is in its own substitution group",
                            declaration.name
                        ));
                        break;
                    }
                    current = found.item;
                }
            }
        }
    }

    /// Whether the particle tree reaches the group `name` through group
    /// references.
    fn group_reaches(
        &self,
        particle: &XsdParticle,
        namespace: Option<&str>,
        name: &str,
        depth: usize,
    ) -> bool {
        if depth > crate::model::MAX_DEPTH {
            return false;
        }
        match particle {
            XsdParticle::Group { particles, .. } => particles
                .iter()
                .any(|inner| self.group_reaches(inner, namespace, name, depth + 1)),
            XsdParticle::GroupRef {
                name: reference, ..
            } => {
                if reference.local == name && reference.namespace.as_deref() == namespace {
                    return true;
                }
                self.group(reference.namespace.as_deref(), &reference.local)
                    .and_then(|group| group.item.content.as_ref())
                    .is_some_and(|content| self.group_reaches(content, namespace, name, depth + 1))
            }
            XsdParticle::Element(_) | XsdParticle::Any(_) => false,
        }
    }

    fn default_values(&self, problems: &mut Vec<String>) {
        for (schema, model) in self.models().iter().enumerate() {
            for declaration in &model.elements {
                self.check_default(
                    schema,
                    declaration.default.as_deref(),
                    declaration.fixed.as_deref(),
                    || {
                        self.element_type(Located {
                            schema,
                            item: declaration,
                        })
                    },
                    &format!("element '{}'", declaration.name),
                    problems,
                );
            }
            for declaration in &model.attributes {
                self.check_default(
                    schema,
                    declaration.default.as_deref(),
                    declaration.fixed.as_deref(),
                    || {
                        self.attribute_type(Located {
                            schema,
                            item: declaration,
                        })
                    },
                    &format!("attribute '{}'", declaration.name),
                    problems,
                );
            }
        }
    }

    fn check_default<'a>(
        &'a self,
        _schema: usize,
        default: Option<&str>,
        fixed: Option<&str>,
        reference: impl FnOnce() -> Option<XsdTypeRef<'a>>,
        label: &str,
        problems: &mut Vec<String>,
    ) {
        let (kind, value) = match (default, fixed) {
            (Some(value), _) => ("default", value),
            (None, Some(value)) => ("fixed", value),
            _ => return,
        };
        let Some(reference) = reference() else {
            return;
        };
        let Some(simple) = self.simple_type(reference) else {
            return;
        };
        if simple.id_kind() == Some(IdKind::Id) {
            problems.push(format!(
                "the {label} has type ID and cannot have a {kind} value"
            ));
            return;
        }
        if let Err(error) = simple.validate(value, None) {
            problems.push(format!(
                "the {kind} value '{value}' of the {label} is not valid for the type {}: {}",
                simple.name, error.message
            ));
        }
    }

    fn attribute_uses_problems(&self, problems: &mut Vec<String>) {
        for (schema, model) in self.models().iter().enumerate() {
            for definition in &model.types {
                if !definition.complex {
                    continue;
                }
                let Some(name) = &definition.name else {
                    continue;
                };
                let reference = XsdTypeRef {
                    schema,
                    name: None,
                    definition: Some(definition),
                };
                let uses = self.attribute_uses(reference);
                let mut seen: HashSet<(Option<&str>, &str)> = HashSet::new();
                let mut ids = 0;
                for usage in &uses {
                    let item = usage.item;
                    if item.usage == XsdUse::Prohibited {
                        continue;
                    }
                    let target = self.attribute_target(*usage);
                    let key = (target.item.namespace.as_deref(), target.item.name.as_str());
                    if !seen.insert(key) {
                        problems.push(format!(
                            "the type '{name}' has two attributes named '{}'",
                            target.item.name
                        ));
                    }
                    if self
                        .attribute_type(target)
                        .and_then(|reference| self.simple_type(reference))
                        .is_some_and(|simple| simple.id_kind() == Some(IdKind::Id))
                    {
                        ids += 1;
                    }
                }
                if ids > 1 {
                    problems.push(format!("the type '{name}' has more than one ID attribute"));
                }
                // A restriction keeps the required attributes of its base.
                if definition.derivation == Some(XsdDerivation::Restriction)
                    && let Some(base) = self.base_type(reference)
                    && base.definition.is_some_and(|base| base.complex)
                {
                    let base_uses = self.attribute_uses(base);
                    for own in &definition.attributes {
                        let Some(base_use) = base_uses.iter().find(|candidate| {
                            candidate.item.name == own.name
                                && candidate.item.namespace == own.namespace
                        }) else {
                            continue;
                        };
                        if base_use.item.usage == XsdUse::Required && own.usage == XsdUse::Optional
                        {
                            problems.push(format!(
                                "the restriction '{name}' makes the required attribute '{}' optional",
                                own.name
                            ));
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::{model::XsdModelSet, parse_xsd};

    fn problems(body: &str, complete: bool) -> Vec<String> {
        let source = format!(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema" xmlns:t="urn:t">{body}</xs:schema>"#
        );
        let schema = parse_xsd(&source).unwrap();
        XsdModelSet::new(schema.models).reference_problems(complete, &HashSet::new())
    }

    #[test]
    fn reports_references_to_nothing_only_on_complete_sets() {
        let body =
            r#"<xs:element name="a" type="missing"/><xs:element name="b" type="xs:nothing"/>"#;
        let found = problems(body, true);
        assert_eq!(found.len(), 2, "{found:?}");
        // On an incomplete set only the XML Schema namespace is decidable.
        let incomplete = problems(body, false);
        assert_eq!(incomplete.len(), 1, "{incomplete:?}");
        assert!(incomplete[0].contains("xs:nothing"));
        assert_eq!(
            problems(
                r#"<xs:element name="a" type="t"/><xs:simpleType name="t"><xs:restriction base="xs:string"/></xs:simpleType><xs:element name="b" ref="a"/>"#,
                true
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn reports_duplicates_and_cycles() {
        let duplicate = problems(
            r#"<xs:element name="a"/><xs:element name="a"/><xs:complexType name="c"/><xs:simpleType name="c"><xs:restriction base="xs:string"/></xs:simpleType>"#,
            true,
        );
        assert_eq!(duplicate.len(), 2, "{duplicate:?}");
        let cycle = problems(
            r#"<xs:complexType name="a"><xs:complexContent><xs:extension base="b"/></xs:complexContent></xs:complexType>
               <xs:complexType name="b"><xs:complexContent><xs:extension base="a"/></xs:complexContent></xs:complexType>
               <xs:group name="g"><xs:sequence><xs:group ref="g"/></xs:sequence></xs:group>
               <xs:element name="h" substitutionGroup="i"/><xs:element name="i" substitutionGroup="h"/>"#,
            true,
        );
        assert!(
            cycle
                .iter()
                .any(|message| message.contains("derived from itself")),
            "{cycle:?}"
        );
        assert!(
            cycle
                .iter()
                .any(|message| message.contains("group 'g' contains itself")),
            "{cycle:?}"
        );
        assert!(
            cycle
                .iter()
                .any(|message| message.contains("substitution group")),
            "{cycle:?}"
        );
    }

    #[test]
    fn reports_invalid_default_values_and_attribute_uses() {
        let found = problems(
            r#"<xs:element name="a" type="xs:int" default="x"/>
               <xs:attribute name="b" type="xs:ID" fixed="i"/>
               <xs:complexType name="c"><xs:attribute name="p" type="xs:ID"/><xs:attribute name="q" type="xs:ID"/><xs:attribute name="p"/></xs:complexType>"#,
            true,
        );
        assert!(
            found
                .iter()
                .any(|message| message.contains("default value 'x'")),
            "{found:?}"
        );
        assert!(
            found.iter().any(|message| message.contains("type ID")),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|message| message.contains("two attributes named 'p'")),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|message| message.contains("more than one ID")),
            "{found:?}"
        );
        assert_eq!(
            problems(
                r#"<xs:element name="a" type="xs:int" default="12"/><xs:attribute name="b" default="anything"/>"#,
                true
            ),
            Vec::<String>::new()
        );
    }
}
