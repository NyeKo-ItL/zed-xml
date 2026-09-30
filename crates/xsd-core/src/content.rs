//! Content model validation of complex types (XML Schema 1.0 Part 1, §3.4
//! and §3.8): the particles of a type (sequences, choices, `xs:all`, group
//! references, wildcards, occurrence ranges, extension, substitution
//! groups) become a nondeterministic automaton that is run over the child
//! elements of an instance element, one child at a time.
//!
//! The automaton is built once per type and shared by every element of that
//! type. Counted repetitions are expanded up to [`MAX_COPIES`]: a larger
//! bound is treated as unbounded, and a model that would need more than
//! [`MAX_STATES`] states is not checked at all (no false positive on a
//! hostile or huge schema).

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use crate::model::{
    Located, MAX_DEPTH, XsdCompositor, XsdDerivation, XsdElementDecl, XsdModelSet, XsdParticle,
    XsdTypeRef, XsdWildcardNamespaces,
};

/// Maximum number of automaton states of one content model.
const MAX_STATES: usize = 100_000;
/// Largest `minOccurs`/`maxOccurs` that is expanded into copies.
const MAX_COPIES: usize = 256;
/// Number of expected names listed in a message.
const MAX_EXPECTED: usize = 8;

type Namespace = Option<String>;

/// Names an element symbol accepts: `local name -> (namespace, chameleon)`.
/// `chameleon`: declared without namespace by a schema without target
/// namespace next to namespaced ones, so possibly adopting the namespace of
/// the schema including it.
type Names = HashMap<String, Vec<(Namespace, bool)>>;

enum Matcher {
    Names(Names),
    Wildcard(XsdWildcardNamespaces),
}

struct Symbol {
    matcher: Matcher,
    /// Name shown in messages.
    label: String,
}

impl Symbol {
    fn matches(&self, namespace: Option<&str>, local: &str, strict: bool) -> bool {
        match &self.matcher {
            Matcher::Names(names) => names.get(local).is_some_and(|namespaces| {
                namespaces.iter().any(|(candidate, chameleon)| {
                    candidate.as_deref() == namespace || (!strict && *chameleon)
                })
            }),
            Matcher::Wildcard(allowed) => allowed.allows(namespace),
        }
    }
}

struct Nfa {
    epsilon: Vec<Vec<u32>>,
    /// The symbol edge `(symbol, target)` leaving each state, if any.
    edge: Vec<Option<(u32, u32)>>,
    symbols: Vec<Symbol>,
    start: u32,
    accept: u32,
    /// Memoized epsilon closures, reduced to their significant states (with
    /// a symbol edge, or the accepting one).
    closures: RefCell<Vec<Option<Rc<[u32]>>>>,
}

impl Nfa {
    fn closure(&self, state: u32) -> Rc<[u32]> {
        if let Some(Some(closure)) = self.closures.borrow().get(state as usize) {
            return Rc::clone(closure);
        }
        let mut seen = HashSet::new();
        let mut pending = vec![state];
        let mut significant = Vec::new();
        while let Some(current) = pending.pop() {
            if !seen.insert(current) {
                continue;
            }
            if self.edge[current as usize].is_some() || current == self.accept {
                significant.push(current);
            }
            pending.extend(self.epsilon[current as usize].iter().copied());
        }
        significant.sort_unstable();
        let closure: Rc<[u32]> = significant.into();
        self.closures.borrow_mut()[state as usize] = Some(Rc::clone(&closure));
        closure
    }

    fn step(&self, states: &[u32], namespace: Option<&str>, local: &str, strict: bool) -> Vec<u32> {
        let mut next = Vec::new();
        for &state in states {
            if let Some((symbol, target)) = self.edge[state as usize]
                && self.symbols[symbol as usize].matches(namespace, local, strict)
            {
                next.extend(self.closure(target).iter().copied());
            }
        }
        next.sort_unstable();
        next.dedup();
        next
    }

    fn expected(&self, states: &[u32]) -> Vec<String> {
        let mut labels = Vec::new();
        for &state in states {
            if let Some((symbol, _)) = self.edge[state as usize] {
                let label = &self.symbols[symbol as usize].label;
                if !labels.contains(label) {
                    labels.push(label.clone());
                }
            }
        }
        labels.truncate(MAX_EXPECTED);
        labels
    }
}

/// Member of an `xs:all` group.
struct AllMember {
    names: Names,
    label: String,
    min: usize,
    max: Option<usize>,
}

enum Kind {
    /// No child element is allowed.
    Empty,
    Nfa(Nfa),
    All {
        members: Vec<AllMember>,
        /// The group itself is optional.
        optional: bool,
    },
}

/// Content model of a complex type.
pub(crate) struct ContentModel {
    kind: Kind,
    /// Every element name of the model (local name to namespaces), and the
    /// namespaces of its wildcards: tells an element that is not allowed
    /// at all from one that is misplaced.
    known: Names,
    wildcards: Vec<XsdWildcardNamespaces>,
}

impl ContentModel {
    fn is_known(&self, namespace: Option<&str>, local: &str) -> bool {
        self.known.get(local).is_some_and(|entries| {
            entries
                .iter()
                .any(|(candidate, _)| candidate.as_deref() == namespace)
        }) || self
            .wildcards
            .iter()
            .any(|allowed| allowed.allows(namespace))
    }

    /// The expanded name (`{namespace}local`) the model expects for a child
    /// named `local` that is in another namespace.
    fn other_namespace(&self, local: &str) -> Option<String> {
        self.known.get(local)?.first().map(|(namespace, _)| {
            format!("{{{}}}{local}", namespace.as_deref().unwrap_or_default())
        })
    }
}

/// Why a child element or the end of an element does not match its content
/// model.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ContentError {
    /// The child is not allowed here. `known`: the model allows it
    /// elsewhere; `repeated`: it repeats the previous child once too often.
    Unexpected {
        expected: Vec<String>,
        known: bool,
        repeated: bool,
        /// The model expects an element of that expanded name instead.
        wrong_namespace: Option<String>,
    },
    /// The element ends before its content is complete.
    Incomplete { expected: Vec<String> },
}

enum State {
    Empty,
    Nfa(Vec<u32>),
    All(Vec<usize>),
}

/// Progress of one element through its content model.
pub(crate) struct ContentRun {
    model: Rc<ContentModel>,
    state: State,
    /// A misplaced child was reported: the rest of the element is not
    /// checked (one error per element, like Xerces).
    failed: bool,
    last: Option<(Namespace, String)>,
}

impl ContentRun {
    pub(crate) fn new(model: Rc<ContentModel>) -> Self {
        let state = match &model.kind {
            Kind::Empty => State::Empty,
            Kind::Nfa(nfa) => State::Nfa(nfa.closure(nfa.start).to_vec()),
            Kind::All { members, .. } => State::All(vec![0; members.len()]),
        };
        Self {
            model,
            state,
            failed: false,
            last: None,
        }
    }

    /// Consumes the next child element.
    pub(crate) fn step(
        &mut self,
        namespace: Option<&str>,
        local: &str,
    ) -> Result<(), ContentError> {
        if self.failed {
            return Ok(());
        }
        let repeated = self
            .last
            .as_ref()
            .is_some_and(|(last_namespace, last_local)| {
                last_local == local && last_namespace.as_deref() == namespace
            });
        let expected = match (&self.model.kind, &mut self.state) {
            (Kind::Nfa(nfa), State::Nfa(states)) => {
                let mut next = nfa.step(states, namespace, local, true);
                if next.is_empty() {
                    // Namespaces are compared strictly first, then names
                    // that may have adopted the namespace of the schema
                    // including their own ("chameleon" includes).
                    next = nfa.step(states, namespace, local, false);
                }
                if !next.is_empty() {
                    *states = next;
                    self.last = Some((namespace.map(str::to_owned), local.to_owned()));
                    return Ok(());
                }
                nfa.expected(states)
            }
            (Kind::All { members, .. }, State::All(counts)) => {
                let matching = |strict: bool| {
                    members.iter().position(|member| {
                        member.names.get(local).is_some_and(|namespaces| {
                            namespaces.iter().any(|(candidate, chameleon)| {
                                candidate.as_deref() == namespace || (!strict && *chameleon)
                            })
                        })
                    })
                };
                let position = matching(true).or_else(|| matching(false));
                match position {
                    Some(index) if members[index].max.is_none_or(|max| counts[index] < max) => {
                        counts[index] += 1;
                        self.last = Some((namespace.map(str::to_owned), local.to_owned()));
                        return Ok(());
                    }
                    _ => members
                        .iter()
                        .enumerate()
                        .filter(|(index, member)| member.max.is_none_or(|max| counts[*index] < max))
                        .map(|(_, member)| member.label.clone())
                        .take(MAX_EXPECTED)
                        .collect(),
                }
            }
            _ => Vec::new(),
        };
        let known = self.model.is_known(namespace, local);
        if known {
            self.failed = true;
        }
        Err(ContentError::Unexpected {
            expected,
            known,
            repeated: known && repeated,
            wrong_namespace: if known {
                None
            } else {
                self.model.other_namespace(local)
            },
        })
    }

    /// Checks that the content is complete at the end tag.
    pub(crate) fn finish(&self) -> Result<(), ContentError> {
        if self.failed {
            return Ok(());
        }
        match (&self.model.kind, &self.state) {
            (Kind::Nfa(nfa), State::Nfa(states)) if !states.contains(&nfa.accept) => {
                Err(ContentError::Incomplete {
                    expected: nfa.expected(states),
                })
            }
            (Kind::All { members, optional }, State::All(counts)) => {
                if *optional && counts.iter().all(|count| *count == 0) {
                    return Ok(());
                }
                let missing = members
                    .iter()
                    .zip(counts)
                    .filter(|(member, count)| **count < member.min)
                    .map(|(member, _)| member.label.clone())
                    .take(MAX_EXPECTED)
                    .collect::<Vec<_>>();
                if missing.is_empty() {
                    Ok(())
                } else {
                    Err(ContentError::Incomplete { expected: missing })
                }
            }
            _ => Ok(()),
        }
    }
}

struct Builder<'a> {
    models: &'a XsdModelSet,
    epsilon: Vec<Vec<u32>>,
    edge: Vec<Option<(u32, u32)>>,
    symbols: Vec<Symbol>,
    known: Names,
    wildcards: Vec<XsdWildcardNamespaces>,
    /// The model cannot be built reliably (unresolved group, too large).
    failed: bool,
    /// Named groups being expanded: a group reference to one of them is a
    /// redefinition referring to the group it replaces.
    expanding: Vec<*const crate::model::XsdGroupDef>,
}

impl<'a> Builder<'a> {
    fn state(&mut self) -> u32 {
        if self.epsilon.len() >= MAX_STATES {
            self.failed = true;
            return 0;
        }
        self.epsilon.push(Vec::new());
        self.edge.push(None);
        (self.epsilon.len() - 1) as u32
    }

    fn link(&mut self, from: u32, to: u32) {
        if let Some(targets) = self.epsilon.get_mut(from as usize) {
            targets.push(to);
        }
    }

    /// A fragment matching one occurrence of `symbol`.
    fn symbol_fragment(&mut self, symbol: u32) -> (u32, u32) {
        let start = self.state();
        let end = self.state();
        if let Some(edge) = self.edge.get_mut(start as usize) {
            *edge = Some((symbol, end));
        }
        (start, end)
    }

    fn empty_fragment(&mut self) -> (u32, u32) {
        let state = self.state();
        (state, state)
    }

    /// `min`..`max` (unbounded when `None`) copies of the fragment built by
    /// `build`.
    fn repeat(
        &mut self,
        min: usize,
        max: Option<usize>,
        mut build: impl FnMut(&mut Self) -> (u32, u32),
    ) -> (u32, u32) {
        if max == Some(0) {
            return self.empty_fragment();
        }
        if (min, max) == (1, Some(1)) {
            return build(self);
        }
        let max = max.filter(|max| *max <= MAX_COPIES);
        let min = min.min(MAX_COPIES).min(max.unwrap_or(usize::MAX));
        let start = self.state();
        let end = self.state();
        let mut current = start;
        for _ in 0..min {
            if self.failed {
                break;
            }
            let (first, last) = build(self);
            self.link(current, first);
            current = last;
        }
        match max {
            None => {
                let (first, last) = build(self);
                self.link(current, first);
                self.link(current, end);
                self.link(last, first);
                self.link(last, end);
            }
            Some(max) => {
                for _ in min..max {
                    if self.failed {
                        break;
                    }
                    let (first, last) = build(self);
                    self.link(current, first);
                    self.link(current, end);
                    current = last;
                }
                self.link(current, end);
            }
        }
        (start, end)
    }

    fn names_of(&mut self, schema: usize, declaration: &XsdElementDecl) -> (Names, String) {
        let target = self.models.element_target(Located {
            schema,
            item: declaration,
        });
        let namespaced_schemas = self
            .models
            .models()
            .iter()
            .any(|model| model.target_namespace.is_some());
        let mut names = Names::new();
        let mut add = |declaration: Located<'_, XsdElementDecl>| {
            let chameleon = namespaced_schemas
                && declaration.item.namespace.is_none()
                && self.models.models()[declaration.schema]
                    .target_namespace
                    .is_none();
            let namespaces = names.entry(declaration.item.name.clone()).or_default();
            let entry = (declaration.item.namespace.clone(), chameleon);
            if !namespaces.contains(&entry) {
                namespaces.push(entry);
            }
        };
        if !target.item.is_abstract {
            add(target);
        }
        for member in self.models.substitution_members(target) {
            add(member);
        }
        for (local, namespaces) in &names {
            let known = self.known.entry(local.clone()).or_default();
            for entry in namespaces {
                if !known.contains(entry) {
                    known.push(entry.clone());
                }
            }
        }
        (names, target.item.name.clone())
    }

    fn particle(&mut self, schema: usize, particle: &XsdParticle, depth: usize) -> (u32, u32) {
        if depth > MAX_DEPTH || self.failed {
            self.failed = true;
            return self.empty_fragment();
        }
        let (min, max) = particle.occurs();
        match particle {
            XsdParticle::Element(declaration) => {
                let (names, label) = self.names_of(schema, declaration);
                let symbol = self.symbols.len() as u32;
                self.symbols.push(Symbol {
                    matcher: Matcher::Names(names),
                    label,
                });
                self.repeat(min, max, |builder| builder.symbol_fragment(symbol))
            }
            XsdParticle::Any(wildcard) => {
                self.wildcards.push(wildcard.namespaces.clone());
                let symbol = self.symbols.len() as u32;
                self.symbols.push(Symbol {
                    matcher: Matcher::Wildcard(wildcard.namespaces.clone()),
                    label: match &wildcard.namespaces {
                        XsdWildcardNamespaces::Any => "*".to_owned(),
                        XsdWildcardNamespaces::Other(_) => "{other}*".to_owned(),
                        XsdWildcardNamespaces::Set(namespaces) => namespaces
                            .iter()
                            .map(|namespace| {
                                format!("{{{}}}*", namespace.as_deref().unwrap_or_default())
                            })
                            .collect::<Vec<_>>()
                            .join(" | "),
                    },
                });
                self.repeat(min, max, |builder| builder.symbol_fragment(symbol))
            }
            XsdParticle::Group {
                compositor,
                particles,
                ..
            } => {
                let compositor = *compositor;
                self.repeat(min, max, |builder| {
                    builder.group(schema, compositor, particles, depth)
                })
            }
            XsdParticle::GroupRef { name, .. } => {
                let mut found = self.models.group(name.namespace.as_deref(), &name.local);
                if let Some(current) = found
                    && self.expanding.contains(&std::ptr::from_ref(current.item))
                {
                    found = self.models.group_other_than(
                        name.namespace.as_deref(),
                        &name.local,
                        current.item,
                    );
                }
                let Some(group) = found else {
                    self.failed = true;
                    return self.empty_fragment();
                };
                let Some(content) = &group.item.content else {
                    return self.empty_fragment();
                };
                let group_schema = group.schema;
                let pointer = std::ptr::from_ref(group.item);
                self.repeat(min, max, |builder| {
                    builder.expanding.push(pointer);
                    let fragment = builder.particle(group_schema, content, depth + 1);
                    builder.expanding.pop();
                    fragment
                })
            }
        }
    }

    fn group(
        &mut self,
        schema: usize,
        compositor: XsdCompositor,
        particles: &[XsdParticle],
        depth: usize,
    ) -> (u32, u32) {
        match compositor {
            XsdCompositor::Sequence => {
                let start = self.state();
                let mut current = start;
                for particle in particles {
                    let (first, last) = self.particle(schema, particle, depth + 1);
                    self.link(current, first);
                    current = last;
                }
                (start, current)
            }
            XsdCompositor::Choice => {
                let start = self.state();
                let end = self.state();
                if particles.is_empty() {
                    self.link(start, end);
                }
                for particle in particles {
                    let (first, last) = self.particle(schema, particle, depth + 1);
                    self.link(start, first);
                    self.link(last, end);
                }
                (start, end)
            }
            XsdCompositor::All => {
                // An `xs:all` nested in another group (XSD 1.1) is checked
                // as any number of its members in any order.
                let start = self.state();
                let end = self.state();
                self.link(start, end);
                for particle in particles {
                    let (first, last) = self.particle(schema, particle, depth + 1);
                    self.link(start, first);
                    self.link(last, start);
                }
                (start, end)
            }
        }
    }
}

impl XsdModelSet {
    /// Content model of a complex type with element-only or mixed content,
    /// including the content inherited by extension; `None` when the type
    /// has no content model to check (simple content, unresolved base type
    /// or group, model too large).
    pub(crate) fn content_model(&self, reference: XsdTypeRef<'_>) -> Option<ContentModel> {
        let definition = reference.definition?;
        if !definition.complex || definition.simple_content {
            return None;
        }
        let mut current = reference;
        for _ in 0..MAX_DEPTH {
            let Some(definition) = current.definition else {
                break;
            };
            if definition.derivation != Some(XsdDerivation::Extension) {
                break;
            }
            let base = self.base_type(current)?;
            match base.definition {
                Some(base_definition) if base_definition.complex => current = base,
                Some(_) => return None,
                None => {
                    if base
                        .name
                        .is_some_and(|name| name.is_builtin() && name.local == "anyType")
                    {
                        break;
                    }
                    return None;
                }
            }
        }

        let mut particles = Vec::new();
        self.content_particles(reference, 0, &mut particles);
        let mut builder = Builder {
            models: self,
            epsilon: Vec::new(),
            edge: Vec::new(),
            symbols: Vec::new(),
            known: Names::new(),
            wildcards: Vec::new(),
            failed: false,
            expanding: Vec::new(),
        };
        let kind = if particles.is_empty() {
            Kind::Empty
        } else if let Some(all) = self.all_group(&particles, &mut builder) {
            all
        } else {
            let start = builder.state();
            let mut cursor = start;
            for (schema, particle) in &particles {
                let (first, last) = builder.particle(*schema, particle, 0);
                builder.link(cursor, first);
                cursor = last;
            }
            if builder.failed {
                return None;
            }
            let count = builder.epsilon.len();
            Kind::Nfa(Nfa {
                epsilon: std::mem::take(&mut builder.epsilon),
                edge: std::mem::take(&mut builder.edge),
                symbols: std::mem::take(&mut builder.symbols),
                start,
                accept: cursor,
                closures: RefCell::new(vec![None; count]),
            })
        };
        if builder.failed {
            return None;
        }
        Some(ContentModel {
            kind,
            known: builder.known,
            wildcards: builder.wildcards,
        })
    }

    /// The `xs:all` group that is the whole content of a type, if it is.
    fn all_group(
        &self,
        particles: &[(usize, &XsdParticle)],
        builder: &mut Builder<'_>,
    ) -> Option<Kind> {
        let [(schema, particle)] = particles else {
            return None;
        };
        let (mut schema, mut particle) = (*schema, *particle);
        let mut optional = false;
        for _ in 0..MAX_DEPTH {
            match particle {
                XsdParticle::GroupRef {
                    name, min_occurs, ..
                } => {
                    optional |= *min_occurs == 0;
                    let group = self.group(name.namespace.as_deref(), &name.local)?;
                    schema = group.schema;
                    particle = group.item.content.as_ref()?;
                }
                XsdParticle::Group {
                    compositor: XsdCompositor::All,
                    particles,
                    min_occurs,
                    ..
                } => {
                    optional |= *min_occurs == 0;
                    let mut members = Vec::new();
                    for member in particles {
                        let XsdParticle::Element(declaration) = member else {
                            return None;
                        };
                        let (names, label) = builder.names_of(schema, declaration);
                        members.push(AllMember {
                            names,
                            label,
                            min: declaration.min_occurs,
                            max: declaration.max_occurs,
                        });
                    }
                    return Some(Kind::All { members, optional });
                }
                _ => return None,
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::model::parse_xsd_model;

    use super::*;

    fn run_for(schema: &str, children: &[&str]) -> Result<(), ContentError> {
        let model = parse_xsd_model(&format!(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">{schema}</xs:schema>"#
        ))
        .unwrap();
        let models = XsdModelSet::new(vec![Arc::new(model)]);
        let root = models.global_elements().next().unwrap();
        let element_type = models.element_type(root).unwrap();
        let content = models.content_model(element_type).unwrap();
        let mut run = ContentRun::new(Rc::new(content));
        for child in children {
            run.step(None, child)?;
        }
        run.finish()
    }

    fn root(body: &str) -> String {
        format!(r#"<xs:element name="r"><xs:complexType>{body}</xs:complexType></xs:element>"#)
    }

    #[test]
    fn matches_sequences_with_optional_and_repeated_particles() {
        let schema = root(
            r#"<xs:sequence><xs:element name="a"/><xs:element name="b" minOccurs="0"/><xs:element name="c" maxOccurs="2"/></xs:sequence>"#,
        );
        assert_eq!(run_for(&schema, &["a", "c"]), Ok(()));
        assert_eq!(run_for(&schema, &["a", "b", "c", "c"]), Ok(()));
        assert!(matches!(
            run_for(&schema, &["a", "c", "c", "c"]),
            Err(ContentError::Unexpected { repeated: true, .. })
        ));
        assert!(matches!(
            run_for(&schema, &["a"]),
            Err(ContentError::Incomplete { .. })
        ));
        assert!(matches!(
            run_for(&schema, &["c", "a"]),
            Err(ContentError::Unexpected {
                known: true,
                repeated: false,
                ..
            })
        ));
        assert!(matches!(
            run_for(&schema, &["a", "z"]),
            Err(ContentError::Unexpected { known: false, .. })
        ));
    }

    #[test]
    fn matches_choices_and_nested_groups() {
        let schema = root(
            r#"<xs:choice maxOccurs="unbounded"><xs:sequence><xs:element name="k"/><xs:element name="v"/></xs:sequence><xs:element name="x"/></xs:choice>"#,
        );
        assert_eq!(run_for(&schema, &["k", "v", "x", "k", "v"]), Ok(()));
        assert!(matches!(
            run_for(&schema, &["k", "x"]),
            Err(ContentError::Unexpected { known: true, .. })
        ));
        let single = root(r#"<xs:choice><xs:element name="a"/><xs:element name="b"/></xs:choice>"#);
        assert_eq!(run_for(&single, &["b"]), Ok(()));
        assert!(run_for(&single, &["a", "b"]).is_err());
        assert!(run_for(&single, &[]).is_err());
    }

    #[test]
    fn matches_all_groups_in_any_order() {
        let schema =
            root(r#"<xs:all><xs:element name="a"/><xs:element name="b" minOccurs="0"/></xs:all>"#);
        assert_eq!(run_for(&schema, &["b", "a"]), Ok(()));
        assert_eq!(run_for(&schema, &["a"]), Ok(()));
        assert!(matches!(
            run_for(&schema, &["b"]),
            Err(ContentError::Incomplete { .. })
        ));
        assert!(matches!(
            run_for(&schema, &["a", "a"]),
            Err(ContentError::Unexpected { repeated: true, .. })
        ));
    }

    #[test]
    fn appends_the_content_of_an_extension_to_its_base() {
        let schema = r#"<xs:element name="r"><xs:complexType><xs:complexContent><xs:extension base="base"><xs:sequence><xs:element name="b"/></xs:sequence></xs:extension></xs:complexContent></xs:complexType></xs:element><xs:complexType name="base"><xs:sequence><xs:element name="a"/></xs:sequence></xs:complexType>"#;
        assert_eq!(run_for(schema, &["a", "b"]), Ok(()));
        assert!(run_for(schema, &["b"]).is_err());
        assert!(matches!(
            run_for(schema, &["a"]),
            Err(ContentError::Incomplete { .. })
        ));
    }

    #[test]
    fn honours_the_occurrences_of_groups_and_group_references() {
        let schema = format!(
            "{}{}",
            root(
                r#"<xs:sequence><xs:group ref="g" minOccurs="0" maxOccurs="2"/><xs:element name="z"/></xs:sequence>"#
            ),
            r#"<xs:group name="g"><xs:sequence><xs:element name="p"/><xs:element name="q"/></xs:sequence></xs:group>"#
        );
        assert_eq!(run_for(&schema, &["z"]), Ok(()));
        assert_eq!(run_for(&schema, &["p", "q", "p", "q", "z"]), Ok(()));
        assert!(run_for(&schema, &["p", "z"]).is_err());
        assert!(run_for(&schema, &["p", "q", "p", "q", "p", "q", "z"]).is_err());
    }

    #[test]
    fn matches_wildcards_by_namespace() {
        let schema = r###"<xs:element name="r"><xs:complexType><xs:sequence><xs:any namespace="##other" maxOccurs="unbounded"/></xs:sequence></xs:complexType></xs:element>"###;
        // Elements without a namespace are not `##other`.
        assert!(matches!(
            run_for(schema, &["x"]),
            Err(ContentError::Unexpected { .. })
        ));
    }

    #[test]
    fn treats_large_bounds_as_unbounded_and_huge_models_as_unchecked() {
        let schema =
            root(r#"<xs:sequence><xs:element name="a" maxOccurs="100000"/></xs:sequence>"#);
        let children = vec!["a"; 500];
        assert_eq!(run_for(&schema, &children), Ok(()));
    }
}
