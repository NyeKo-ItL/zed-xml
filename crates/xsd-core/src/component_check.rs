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
    model::{
        XsdDerivation, XsdElementDecl, XsdModel, XsdModelSet, XsdParticle, XsdTypeDef, XsdTypeRef,
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

impl XsdModelSet {
    /// Problems of the derivation and content model of the complex types of
    /// the set.
    pub fn component_problems(&self) -> Vec<String> {
        let mut problems: Vec<String> = Vec::new();
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
                        if let (Some(own), Some(parent)) = (&own, self.content_model(base))
                            && let Err(reason) = own.restricts(&parent)
                        {
                            problems.push(format!(
                                "{label} is not a valid restriction of '{}': {reason}",
                                base_definition.name.as_deref().unwrap_or("its base type")
                            ));
                        }
                    }
                    XsdDerivation::Extension => {
                        if definition.mixed != base_definition.mixed
                            && base_definition.content.is_some()
                            && definition.content.is_some()
                        {
                            problems.push(format!(
                                "{label}: an extension must be mixed if and only if its base type '{}' is",
                                base_definition.name.as_deref().unwrap_or("its base type")
                            ));
                        }
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
