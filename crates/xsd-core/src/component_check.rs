//! Schema component constraints that need the whole set of schemas:
//! derivation by restriction and extension of complex types, Unique
//! Particle Attribution and Element Declarations Consistent (XML Schema 1.0
//! Part 1 §3.4.6, §3.8.6, §3.9.6).
//!
//! Every check is skipped when a component it needs cannot be resolved (an
//! include or import that was not loaded), so an incomplete schema set never
//! produces a false problem.

use crate::{
    content::ModelProblem,
    datatypes::BuiltinType,
    model::{
        Located, XsdDerivation, XsdElementDecl, XsdModel, XsdModelSet, XsdParticle, XsdTypeDef,
        XsdTypeRef,
    },
};

/// Complex types of a model, with a label for messages: global types,
/// anonymous types of elements (nested included) and of groups.
fn complex_types(model: &XsdModel) -> Vec<(&XsdTypeDef, String)> {
    let mut found = Vec::new();
    let mut pending: Vec<(&XsdTypeDef, String)> = Vec::new();
    for definition in &model.types {
        if let Some(name) = &definition.name {
            pending.push((definition, format!("complex type '{name}'")));
        }
    }
    for element in &model.elements {
        if let Some(anonymous) = &element.anonymous_type {
            pending.push((anonymous, format!("the type of element '{}'", element.name)));
        }
    }
    let mut particles: Vec<&XsdParticle> = model
        .groups
        .iter()
        .filter_map(|group| group.content.as_ref())
        .collect();
    while let Some((definition, label)) = pending.pop() {
        if let Some(content) = &definition.content {
            particles.push(content);
        }
        if definition.complex {
            found.push((definition, label));
        }
        while let Some(particle) = particles.pop() {
            match particle {
                XsdParticle::Element(declaration) => local_type(declaration, &mut pending),
                XsdParticle::Group {
                    particles: inner, ..
                } => particles.extend(inner),
                XsdParticle::GroupRef { .. } | XsdParticle::Any(_) => {}
            }
        }
    }
    found
}

fn local_type<'a>(declaration: &'a XsdElementDecl, pending: &mut Vec<(&'a XsdTypeDef, String)>) {
    if let Some(anonymous) = &declaration.anonymous_type {
        pending.push((
            anonymous,
            format!("the type of element '{}'", declaration.name),
        ));
    }
}

/// Kind of a particle for "Particle Valid (Restriction)".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Element,
    Any,
    All,
    Choice,
    Sequence,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Element => "an element",
            Self::Any => "a wildcard",
            Self::All => "an xs:all group",
            Self::Choice => "a choice",
            Self::Sequence => "a sequence",
        }
    }

    /// The derivation table of XML Schema 1.0 Part 1 §3.9.6: which kinds of
    /// particle can restrict which.
    fn can_restrict(self, base: Self) -> bool {
        use Kind::{All, Any, Choice, Element, Sequence};
        match (self, base) {
            (Element, _) | (_, Any) => true,
            (Any, _) => false,
            (All, other) => other == All,
            (Choice, other) => other == Choice,
            (Sequence, other) => other != Element,
        }
    }
}

impl XsdModelSet {
    /// Kind of the effective particle: groups holding a single particle
    /// that occurs exactly once are "pointless" and looked through, group
    /// references are replaced by the group they name.
    fn particle_kind(&self, particle: &XsdParticle, depth: usize) -> Option<Kind> {
        if depth > crate::model::MAX_DEPTH {
            return None;
        }
        match particle {
            XsdParticle::Element(declaration) => {
                // The head of a substitution group stands for a choice of
                // its members: not compared by kind.
                if let Some(reference) = &declaration.reference
                    && let Some(head) =
                        self.global_element(reference.namespace.as_deref(), &reference.local)
                    && !self.substitution_members(head).is_empty()
                {
                    return None;
                }
                Some(Kind::Element)
            }
            XsdParticle::Any(_) => Some(Kind::Any),
            XsdParticle::Group {
                compositor,
                particles,
                min_occurs,
                max_occurs,
            } => {
                if let [only] = particles.as_slice()
                    && *min_occurs == 1
                    && *max_occurs == Some(1)
                {
                    return self.particle_kind(only, depth + 1);
                }
                Some(match compositor {
                    crate::model::XsdCompositor::All => Kind::All,
                    crate::model::XsdCompositor::Choice => Kind::Choice,
                    crate::model::XsdCompositor::Sequence => Kind::Sequence,
                })
            }
            XsdParticle::GroupRef {
                name,
                min_occurs,
                max_occurs,
            } => {
                let group = self.group(name.namespace.as_deref(), &name.local)?;
                let content = group.item.content.as_ref()?;
                match content {
                    XsdParticle::Group {
                        compositor,
                        particles,
                        ..
                    } => {
                        if let [only] = particles.as_slice()
                            && *min_occurs == 1
                            && *max_occurs == Some(1)
                        {
                            return self.particle_kind(only, depth + 1);
                        }
                        Some(match compositor {
                            crate::model::XsdCompositor::All => Kind::All,
                            crate::model::XsdCompositor::Choice => Kind::Choice,
                            crate::model::XsdCompositor::Sequence => Kind::Sequence,
                        })
                    }
                    other => self.particle_kind(other, depth + 1),
                }
            }
        }
    }

    /// Element declarations of the particle tree of `particle` (groups
    /// followed; `ref`s resolved by the callers through `element_target`).
    fn collect_elements<'a>(
        &'a self,
        schema: usize,
        particle: &'a XsdParticle,
        out: &mut Vec<Located<'a, XsdElementDecl>>,
        depth: usize,
    ) {
        if depth > crate::model::MAX_DEPTH {
            return;
        }
        match particle {
            XsdParticle::Element(declaration) => out.push(Located {
                schema,
                item: declaration,
            }),
            XsdParticle::Group { particles, .. } => {
                for inner in particles {
                    self.collect_elements(schema, inner, out, depth + 1);
                }
            }
            XsdParticle::GroupRef { name, .. } => {
                if let Some(group) = self.group(name.namespace.as_deref(), &name.local)
                    && let Some(content) = &group.item.content
                {
                    self.collect_elements(group.schema, content, out, depth + 1);
                }
            }
            XsdParticle::Any(_) => {}
        }
    }

    /// Whether `derived` is `base` or reached from it by restrictions only;
    /// `None` when this cannot be decided (unresolved or built-in types).
    fn derived_by_restriction(
        &self,
        derived: XsdTypeRef<'_>,
        base: XsdTypeRef<'_>,
    ) -> Option<bool> {
        match self.restriction_chain(derived, base) {
            // A member of a union (or what derives from one) restricts it.
            Some(false)
                if base
                    .definition
                    .is_some_and(|base| base.derivation == Some(XsdDerivation::Union)) =>
            {
                match self.union_member_derivation(derived, base, 0) {
                    Some(Some(_)) => Some(true),
                    Some(None) => Some(false),
                    None => None,
                }
            }
            other => other,
        }
    }

    fn restriction_chain(&self, derived: XsdTypeRef<'_>, base: XsdTypeRef<'_>) -> Option<bool> {
        let mut current = derived;
        for _ in 0..crate::model::MAX_DEPTH {
            let same = match (current.definition, base.definition) {
                (Some(a), Some(b)) => std::ptr::eq(a, b),
                (None, None) => match (current.name, base.name) {
                    (Some(a), Some(b)) => a.namespace == b.namespace && a.local == b.local,
                    _ => return None,
                },
                _ => false,
            };
            if same {
                return Some(true);
            }
            let Some(definition) = current.definition else {
                // A built-in type: compare along the built-in hierarchy; it
                // never derives from a type of a schema.
                let (Some(a), Some(b)) = (current.name, base.name) else {
                    return None;
                };
                if !a.is_builtin() {
                    return None;
                }
                if !b.is_builtin() {
                    return Some(false);
                }
                let a = BuiltinType::from_local_name(&a.local)?;
                let b = BuiltinType::from_local_name(&b.local)?;
                return Some(a.derives_from(b));
            };
            let base_is = |local: &str| {
                base.name
                    .is_some_and(|name| name.is_builtin() && name.local == local)
            };
            match definition.derivation {
                Some(XsdDerivation::Restriction) => {}
                Some(XsdDerivation::Extension) => return Some(false),
                // A list or union is derived from anySimpleType only.
                Some(_) => return Some(base_is("anySimpleType") || base_is("anyType")),
                None if definition.complex => return Some(base_is("anyType")),
                None => return None,
            }
            current = self.base_type(current)?;
        }
        None
    }

    /// Whether the fixed value of a restricting element equals `base` in the
    /// value space of its type (`1` and `1.0` for a decimal), by string
    /// comparison when the type is not simple.
    fn same_fixed_value(
        &self,
        element: Located<'_, XsdElementDecl>,
        fixed: Option<&str>,
        base: &str,
    ) -> bool {
        let Some(fixed) = fixed else {
            return false;
        };
        if fixed == base {
            return true;
        }
        self.element_type(element)
            .and_then(|reference| self.simple_type(reference))
            .is_some_and(|simple| simple.values_equal(fixed, base, None))
    }

    /// "Particle Valid (Restriction)", NameAndTypeOK: an element of a
    /// restricted type that also appears in its base type must not be more
    /// permissive than the base one (nillable, fixed value, disallowed
    /// substitutions, type derived by restriction).
    fn element_restriction_problems(
        &self,
        derived: XsdTypeRef<'_>,
        base: XsdTypeRef<'_>,
    ) -> Vec<String> {
        let mut derived_elements = Vec::new();
        let mut base_elements = Vec::new();
        let mut particles = Vec::new();
        self.content_particles(derived, 0, &mut particles);
        for (schema, particle) in particles.drain(..) {
            self.collect_elements(schema, particle, &mut derived_elements, 0);
        }
        self.content_particles(base, 0, &mut particles);
        for (schema, particle) in particles {
            self.collect_elements(schema, particle, &mut base_elements, 0);
        }
        let mut problems = Vec::new();
        for particle in derived_elements {
            let restricted = self.element_target(particle);
            let mut same = base_elements
                .iter()
                .map(|candidate| self.element_target(*candidate))
                .filter(|candidate| {
                    candidate.item.name == restricted.item.name
                        && candidate.item.namespace == restricted.item.namespace
                });
            let (Some(original), None) = (same.next(), same.next()) else {
                continue;
            };
            let (r, b) = (restricted.item, original.item);
            let name = &r.name;
            if r.nillable && !b.nillable {
                problems.push(format!(
                    "element '{name}' is nillable but the element it restricts is not"
                ));
            }
            if let Some(fixed) = &b.fixed
                && !self.same_fixed_value(restricted, r.fixed.as_deref(), fixed)
            {
                problems.push(format!(
                    "element '{name}' must keep the fixed value '{fixed}' of the element it restricts"
                ));
            }
            if (b.blocks_extension && !r.blocks_extension)
                || (b.blocks_restriction && !r.blocks_restriction)
                || (b.blocks_substitution && !r.blocks_substitution)
            {
                problems.push(format!(
                    "element '{name}' must disallow at least the derivations and substitutions the element it restricts disallows"
                ));
            }
            let (rt, bt) = (self.element_type(restricted), self.element_type(original));
            let Some(bt) = bt else {
                continue;
            };
            let Some(rt) = rt else {
                // No type is xs:anyType, derived from nothing else.
                let any_type = bt
                    .name
                    .is_some_and(|name| name.is_builtin() && name.local == "anyType");
                if !any_type && bt.definition.is_some() {
                    problems.push(format!(
                        "the type of element '{name}' (xs:anyType) is not derived by restriction from the type of the element it restricts"
                    ));
                }
                continue;
            };
            if self.derived_by_restriction(rt, bt) == Some(false) {
                problems.push(format!(
                    "the type of element '{name}' is not derived by restriction from the type of the element it restricts"
                ));
            }
        }
        problems
    }

    /// Derivations forbidden by `final` / `finalDefault`: a type derived by a
    /// method its base type forbids, and a member of a substitution group
    /// whose type is derived that way from the type of its head.
    fn final_problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for (schema, model) in self.models().iter().enumerate() {
            let mut types = complex_types(model);
            for definition in &model.types {
                if !definition.complex
                    && let Some(name) = &definition.name
                {
                    types.push((definition, format!("simple type '{name}'")));
                }
            }
            for (definition, label) in types {
                let reference = XsdTypeRef {
                    schema,
                    name: None,
                    definition: Some(definition),
                };
                let forbidden = match definition.derivation {
                    Some(XsdDerivation::Restriction) => self
                        .base_type(reference)
                        .and_then(|base| base.definition)
                        .filter(|base| base.final_restriction)
                        .map(|_| "restriction"),
                    Some(XsdDerivation::Extension) => self
                        .base_type(reference)
                        .and_then(|base| base.definition)
                        .filter(|base| base.final_extension)
                        .map(|_| "extension"),
                    Some(XsdDerivation::List) => definition
                        .item_type
                        .as_ref()
                        .and_then(|item| self.resolve_type(schema, item).definition)
                        .filter(|item| item.final_list)
                        .map(|_| "list"),
                    Some(XsdDerivation::Union) => definition
                        .member_types
                        .iter()
                        .filter_map(|member| self.resolve_type(schema, member).definition)
                        .any(|member| member.final_union)
                        .then_some("union"),
                    None => None,
                };
                if let Some(method) = forbidden {
                    problems.push(format!(
                        "{label} cannot be derived by {method}: the type it derives from is final for {method}"
                    ));
                }
            }
            for element in &model.elements {
                let member = Located {
                    schema,
                    item: element,
                };
                for group in &element.substitution_groups {
                    let Some(head) = self.global_element(group.namespace.as_deref(), &group.local)
                    else {
                        continue;
                    };
                    if !head.item.final_extension && !head.item.final_restriction {
                        continue;
                    }
                    let (Some(member_type), Some(head_type)) =
                        (self.element_type(member), self.element_type(head))
                    else {
                        continue;
                    };
                    if let Some(Some((extension, restriction))) =
                        self.derivation_methods(member_type, head_type)
                        && ((extension && head.item.final_extension)
                            || (restriction && head.item.final_restriction))
                    {
                        problems.push(format!(
                            "element '{}' cannot be in the substitution group of '{}': its type is derived from the type of the head by a method the head declares final",
                            element.name, head.item.name
                        ));
                    }
                }
            }
        }
        problems
    }

    /// Attribute rules of a restriction: a required attribute of the base
    /// type stays required, a fixed value is kept, new attributes and the
    /// attribute wildcard must be allowed by the wildcard of the base type.
    fn attribute_restriction_problems(
        &self,
        derived: XsdTypeRef<'_>,
        base: XsdTypeRef<'_>,
    ) -> Vec<String> {
        let mut problems = Vec::new();
        let Some(definition) = derived.definition else {
            return problems;
        };
        let derived_uses = self.attribute_uses(derived);
        let base_uses = self.attribute_uses(base);
        let key = |usage: &Located<'_, crate::model::XsdAttributeDecl>| {
            (usage.item.name.clone(), usage.item.namespace.clone())
        };
        for base_use in &base_uses {
            if base_use.item.usage == crate::model::XsdUse::Prohibited {
                continue;
            }
            let own = derived_uses
                .iter()
                .find(|candidate| key(candidate) == key(base_use));
            let name = &base_use.item.name;
            if base_use.item.usage == crate::model::XsdUse::Required
                && own.is_none_or(|own| own.item.usage != crate::model::XsdUse::Required)
            {
                problems.push(format!(
                    "the required attribute '{name}' of the base type must stay required"
                ));
            }
            let base_declaration = self.attribute_declaration(*base_use);
            let base_fixed = base_use
                .item
                .fixed
                .as_deref()
                .or(base_declaration.item.fixed.as_deref());
            if let (Some(fixed), Some(own)) = (base_fixed, own) {
                let own_declaration = self.attribute_declaration(*own);
                let own_fixed = own
                    .item
                    .fixed
                    .as_deref()
                    .or(own_declaration.item.fixed.as_deref());
                let same = own_fixed.is_some_and(|own_fixed| {
                    own_fixed == fixed
                        || self
                            .attribute_type(base_declaration)
                            .and_then(|reference| self.simple_type(reference))
                            .is_some_and(|simple| simple.values_equal(own_fixed, fixed, None))
                });
                if !same {
                    problems.push(format!(
                        "the attribute '{name}' must keep the fixed value '{fixed}' of the base type"
                    ));
                }
            }
        }
        let base_wildcards = self.attribute_wildcards(base);
        for own in &definition.attributes {
            if own.usage == crate::model::XsdUse::Prohibited
                || base_uses.iter().any(|usage| {
                    usage.item.name == own.name && usage.item.namespace == own.namespace
                })
            {
                continue;
            }
            if !base_wildcards
                .iter()
                .any(|wildcard| wildcard.namespaces.allows(own.namespace.as_deref()))
            {
                problems.push(format!(
                    "the attribute '{}' is neither declared by the base type nor allowed by its attribute wildcard",
                    own.name
                ));
            }
        }
        if let Some(wildcard) = &definition.any_attribute {
            match base_wildcards.first() {
                None => problems.push(
                    "the attribute wildcard needs an attribute wildcard in the base type"
                        .to_owned(),
                ),
                Some(base_wildcard)
                    if !crate::content::wildcard_covers(
                        &base_wildcard.namespaces,
                        &wildcard.namespaces,
                    ) =>
                {
                    problems.push(
                        "the attribute wildcard allows namespaces the wildcard of the base type does not"
                            .to_owned(),
                    );
                }
                Some(_) => {}
            }
        }
        problems
    }

    /// `default` and `fixed` values of the local declarations of complex types
    /// (elements of the content model, attributes): valid for their type, no
    /// value on an element of a complex type without simple or mixed
    /// content, a local reference to a global attribute with another fixed
    /// value.
    fn local_declaration_problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for (schema, model) in self.models().iter().enumerate() {
            for (definition, label) in complex_types(model) {
                for attribute in &definition.attributes {
                    let local = Located {
                        schema,
                        item: attribute,
                    };
                    let target = self.attribute_target(local);
                    if attribute.reference.is_none() {
                        self.check_default(
                            schema,
                            attribute.default.as_deref(),
                            attribute.fixed.as_deref(),
                            || self.attribute_type(local),
                            &format!("attribute '{}' of {label}", attribute.name),
                            &mut problems,
                        );
                    } else if let (Some(own), Some(global)) = (&attribute.fixed, &target.item.fixed)
                    {
                        let same = own == global
                            || self
                                .attribute_type(target)
                                .and_then(|reference| self.simple_type(reference))
                                .is_some_and(|simple| simple.values_equal(own, global, None));
                        if !same {
                            problems.push(format!(
                                "{label}: the fixed value '{own}' of attribute '{}' differs from the fixed value '{global}' of the attribute it references",
                                attribute.name
                            ));
                        }
                    }
                }
                let mut elements = Vec::new();
                if let Some(content) = &definition.content {
                    self.collect_elements(schema, content, &mut elements, 0);
                }
                for particle in elements {
                    let declaration = self.element_target(particle);
                    let item = declaration.item;
                    if item.default.is_none() && item.fixed.is_none() {
                        continue;
                    }
                    if particle.item.reference.is_some() {
                        // A global declaration, checked with its model.
                        continue;
                    }
                    if let Some(reference) = self.element_type(declaration)
                        && let Some(type_definition) = reference.definition
                        && type_definition.complex
                        && !type_definition.simple_content
                        && !type_definition.mixed
                    {
                        problems.push(format!(
                            "{label}: element '{}' has a default or fixed value but its type has element-only content",
                            item.name
                        ));
                        continue;
                    }
                    self.check_default(
                        schema,
                        item.default.as_deref(),
                        item.fixed.as_deref(),
                        || self.element_type(declaration),
                        &format!("element '{}' of {label}", item.name),
                        &mut problems,
                    );
                }
            }
        }
        problems
    }

    /// The kind of base a complex type can derive from: `complexContent`
    /// needs a complex type (or `xs:anyType`), a `simpleContent` restriction
    /// a complex type with simple content, a `simpleContent` extension a
    /// simple type or a complex type with simple content.
    fn derivation_kind_problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for (schema, model) in self.models().iter().enumerate() {
            for (definition, label) in complex_types(model) {
                let Some(derivation) = definition.derivation else {
                    continue;
                };
                if !matches!(
                    derivation,
                    XsdDerivation::Restriction | XsdDerivation::Extension
                ) {
                    continue;
                }
                let reference = XsdTypeRef {
                    schema,
                    name: None,
                    definition: Some(definition),
                };
                let Some(base) = self.base_type(reference) else {
                    continue;
                };
                let restriction = derivation == XsdDerivation::Restriction;
                let base_name = base
                    .name
                    .map_or_else(|| "its base type".to_owned(), |name| name.local.clone());
                let message = match base.definition {
                    None => {
                        let Some(name) = base.name.filter(|name| name.is_builtin()) else {
                            // Unresolved: the reference check reports it.
                            continue;
                        };
                        let any_type = name.local == "anyType";
                        if definition.simple_content {
                            if any_type && restriction && !definition.inline_types.is_empty() {
                                // xs:anyType is mixed and emptiable: a
                                // restriction with a simple type is valid.
                                None
                            } else if any_type {
                                Some(
                                    "xs:anyType is not a simple type or a complex type with simple content",
                                )
                            } else if restriction {
                                Some(
                                    "a simpleContent restriction needs a complex base type with simple content",
                                )
                            } else {
                                None
                            }
                        } else if any_type {
                            None
                        } else {
                            Some("a complexContent derivation needs a complex base type")
                        }
                    }
                    Some(base_definition) => {
                        if definition.simple_content {
                            // A complex base with simple content, or mixed
                            // (emptiable) content, can be restricted.
                            let restrictable = base_definition.complex
                                && (base_definition.simple_content || base_definition.mixed);
                            if restriction && !restrictable {
                                Some(
                                    "a simpleContent restriction needs a complex base type with simple content",
                                )
                            } else if !restriction
                                && base_definition.complex
                                && !base_definition.simple_content
                            {
                                Some(
                                    "a simpleContent extension needs a simple type or a complex type with simple content as base",
                                )
                            } else {
                                None
                            }
                        } else if !base_definition.complex {
                            Some("a complexContent derivation needs a complex base type")
                        } else {
                            None
                        }
                    }
                };
                if let Some(message) = message {
                    problems.push(format!("{label} (base '{base_name}'): {message}"));
                }
                // An extension appends its content to that of the base type
                // in a sequence, which cannot hold an xs:all group.
                if derivation == XsdDerivation::Extension
                    && !definition.simple_content
                    && let Some(content) = &definition.content
                    && matches!(
                        content,
                        XsdParticle::Group {
                            compositor: crate::model::XsdCompositor::All,
                            ..
                        }
                    )
                    && base.definition.is_some_and(|base| base.content.is_some())
                {
                    problems.push(format!(
                        "{label}: an extension cannot add an xs:all group to a base type that has content"
                    ));
                }
            }
        }
        problems
    }

    /// Problems of the derivation and content model of the complex types of
    /// the set.
    pub fn component_problems(&self) -> Vec<String> {
        let mut problems: Vec<String> = self.final_problems();
        problems.extend(self.derivation_kind_problems());
        problems.extend(self.local_declaration_problems());
        for (schema, model) in self.models().iter().enumerate() {
            for (definition, label) in complex_types(model) {
                let reference = XsdTypeRef {
                    schema,
                    name: None,
                    definition: Some(definition),
                };
                if definition.simple_content {
                    continue;
                }
                let own = self.content_model(reference);
                if let Some(content) = &own {
                    for problem in content.problems() {
                        let message = match problem {
                            ModelProblem::Ambiguous(message)
                            | ModelProblem::Inconsistent(message) => message,
                        };
                        problems.push(format!("{label}: {message}"));
                    }
                }
                let Some(derivation) = definition.derivation else {
                    continue;
                };
                if !matches!(
                    derivation,
                    XsdDerivation::Restriction | XsdDerivation::Extension
                ) {
                    continue;
                }
                let Some(base) = self.base_type(reference) else {
                    continue;
                };
                let Some(base_definition) = base.definition else {
                    continue;
                };
                if !base_definition.complex || base_definition.simple_content {
                    continue;
                }
                match derivation {
                    XsdDerivation::Restriction => {
                        if definition.mixed && !base_definition.mixed {
                            problems.push(format!(
                                "{label}: a restriction of an element-only type cannot be mixed"
                            ));
                        }
                        if let (Some(own_content), Some(base_content)) =
                            (&definition.content, &base_definition.content)
                            && base_definition.derivation != Some(XsdDerivation::Extension)
                            && let (Some(derived_kind), Some(base_kind)) = (
                                self.particle_kind(own_content, 0),
                                self.particle_kind(base_content, 0),
                            )
                            && !derived_kind.can_restrict(base_kind)
                        {
                            problems.push(format!(
                                "{label} is not a valid restriction of '{}': {} cannot restrict {}",
                                base_definition.name.as_deref().unwrap_or("its base type"),
                                derived_kind.name(),
                                base_kind.name()
                            ));
                        }
                        problems.extend(
                            self.attribute_restriction_problems(reference, base)
                                .into_iter()
                                .map(|problem| format!("{label}: {problem}")),
                        );
                        problems.extend(
                            self.element_restriction_problems(reference, base)
                                .into_iter()
                                .map(|problem| format!("{label}: {problem}")),
                        );
                        if let (Some(own), Some(parent)) = (&own, self.content_model(base))
                            && let Err(reason) = own.restricts(&parent)
                        {
                            problems.push(format!(
                                "{label} is not a valid restriction of '{}': {reason}",
                                base_definition.name.as_deref().unwrap_or("its base type")
                            ));
                        }
                    }
                    XsdDerivation::Extension
                        if definition.mixed != base_definition.mixed
                            && base_definition.content.is_some()
                            && definition.content.is_some() =>
                    {
                        problems.push(format!(
                            "{label}: an extension must be mixed if and only if its base type '{}' is",
                            base_definition.name.as_deref().unwrap_or("its base type")
                        ));
                    }
                    _ => {}
                }
            }
        }
        problems.sort();
        problems.dedup();
        problems
    }
}

#[cfg(test)]
mod tests {
    use crate::{merge_schemas, parse_xsd};

    fn problems(body: &str) -> Vec<String> {
        let source = format!(
            r###"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">{body}</xs:schema>"###
        );
        merge_schemas([parse_xsd(&source).unwrap()]).problems
    }

    const BASE: &str = r###"<xs:complexType name="b"><xs:sequence><xs:element name="a"/><xs:element name="c" minOccurs="0" maxOccurs="3"/><xs:any namespace="##other" minOccurs="0" processContents="lax"/></xs:sequence></xs:complexType>"###;

    fn restriction(content: &str) -> Vec<String> {
        problems(&format!(
            r###"{BASE}<xs:complexType name="r"><xs:complexContent><xs:restriction base="b">{content}</xs:restriction></xs:complexContent></xs:complexType>"###
        ))
    }

    #[test]
    fn accepts_valid_restrictions() {
        assert_eq!(
            restriction(r###"<xs:sequence><xs:element name="a"/></xs:sequence>"###),
            Vec::<String>::new()
        );
        assert_eq!(
            restriction(
                r###"<xs:sequence><xs:element name="a"/><xs:element name="c" maxOccurs="2"/><xs:any namespace="urn:x" processContents="strict"/></xs:sequence>"###
            ),
            Vec::<String>::new()
        );
    }

    #[test]
    fn reports_restrictions_that_widen_the_base() {
        let widening = |content: &str, needle: &str| {
            let found = restriction(content);
            assert!(
                found.iter().any(|message| message.contains(needle)),
                "{content}: {found:?}"
            );
        };
        // Missing required element, element not in the base, larger bound,
        // weaker processContents, empty restriction of required content.
        widening(
            r###"<xs:sequence><xs:element name="c"/></xs:sequence>"###,
            "not allowed by the base",
        );
        widening(
            r###"<xs:sequence><xs:element name="a"/><xs:element name="z"/></xs:sequence>"###,
            "<z>",
        );
        widening(
            r###"<xs:sequence><xs:element name="a"/><xs:element name="c" maxOccurs="4"/></xs:sequence>"###,
            "<c>",
        );
        widening(
            r###"<xs:sequence><xs:element name="a"/><xs:any namespace="##other" processContents="skip"/></xs:sequence>"###,
            "wildcard",
        );
        widening("", "requires content");
    }

    #[test]
    fn reports_ambiguous_content_models() {
        let found = problems(
            r###"<xs:complexType name="t"><xs:sequence><xs:element name="a" minOccurs="0"/><xs:element name="a"/></xs:sequence></xs:complexType>"###,
        );
        assert!(
            found[0].contains("Unique Particle Attribution"),
            "{found:?}"
        );
        // The same particle matched in several iterations is not ambiguous.
        assert_eq!(
            problems(
                r###"<xs:complexType name="t"><xs:sequence maxOccurs="3"><xs:element name="a" minOccurs="0"/><xs:element name="b"/></xs:sequence></xs:complexType>"###
            ),
            Vec::<String>::new()
        );
        // A wildcard overlapping an element of the content model.
        let wildcard = problems(
            r###"<xs:complexType name="t"><xs:sequence><xs:any minOccurs="0" maxOccurs="unbounded"/><xs:element name="a"/></xs:sequence></xs:complexType>"###,
        );
        assert!(
            wildcard[0].contains("Unique Particle Attribution"),
            "{wildcard:?}"
        );
        let types = problems(
            r###"<xs:complexType name="t"><xs:sequence><xs:choice><xs:element name="a" type="xs:int"/><xs:sequence><xs:element name="b"/><xs:element name="a" type="xs:string"/></xs:sequence></xs:choice></xs:sequence></xs:complexType>"###,
        );
        assert!(
            types
                .iter()
                .any(|message| message.contains("different types")),
            "{types:?}"
        );
    }

    #[test]
    fn substitution_blocked_heads_do_not_overlap() {
        assert_eq!(
            problems(
                r###"<xs:element name="h" type="xs:string" block="substitution"/><xs:element name="m" substitutionGroup="h" type="xs:string"/>
                   <xs:complexType name="t"><xs:all><xs:element ref="h"/><xs:element ref="m"/></xs:all></xs:complexType>"###
            ),
            Vec::<String>::new()
        );
    }
}
