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
    XsdProcessContents, XsdTypeRef, XsdWildcardNamespaces,
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
    /// `processContents` of a wildcard.
    process: Option<XsdProcessContents>,
    /// XSD 1.1 exclusions on a wildcard, not modelled: never compared.
    exclusions: bool,
    /// Identifier of the declaration (type) of an element particle, to
    /// find elements of the same name declared differently.
    declaration: Option<String>,
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

/// Order of `processContents`: a restriction may only make it stronger.
fn strength(process: Option<XsdProcessContents>) -> u8 {
    match process {
        Some(XsdProcessContents::Strict) | None => 2,
        Some(XsdProcessContents::Lax) => 1,
        Some(XsdProcessContents::Skip) => 0,
    }
}

/// Whether every namespace `inner` allows is allowed by `outer`.
fn wildcard_covers(outer: &XsdWildcardNamespaces, inner: &XsdWildcardNamespaces) -> bool {
    match (outer, inner) {
        (XsdWildcardNamespaces::Any, _) => true,
        (_, XsdWildcardNamespaces::Any) => false,
        (XsdWildcardNamespaces::Other(outer), XsdWildcardNamespaces::Other(inner)) => {
            outer == inner
        }
        (XsdWildcardNamespaces::Other(_), XsdWildcardNamespaces::Set(namespaces)) => namespaces
            .iter()
            .all(|namespace| outer.allows(namespace.as_deref())),
        (XsdWildcardNamespaces::Set(_), XsdWildcardNamespaces::Other(_)) => false,
        (XsdWildcardNamespaces::Set(outer), XsdWildcardNamespaces::Set(inner)) => {
            inner.iter().all(|namespace| outer.contains(namespace))
        }
    }
}

/// Whether some namespace is allowed by both wildcards.
fn wildcards_intersect(left: &XsdWildcardNamespaces, right: &XsdWildcardNamespaces) -> bool {
    match (left, right) {
        (XsdWildcardNamespaces::Any, _) | (_, XsdWildcardNamespaces::Any) => true,
        (XsdWildcardNamespaces::Other(_), XsdWildcardNamespaces::Other(_)) => true,
        (XsdWildcardNamespaces::Other(_), XsdWildcardNamespaces::Set(namespaces))
        | (XsdWildcardNamespaces::Set(namespaces), XsdWildcardNamespaces::Other(_)) => {
            let other = if matches!(left, XsdWildcardNamespaces::Other(_)) {
                left
            } else {
                right
            };
            namespaces
                .iter()
                .any(|namespace| other.allows(namespace.as_deref()))
        }
        (XsdWildcardNamespaces::Set(left), XsdWildcardNamespaces::Set(right)) => {
            left.iter().any(|namespace| right.contains(namespace))
        }
    }
}

/// Whether two symbols can match the same element.
fn symbols_overlap(left: &Symbol, right: &Symbol) -> bool {
    if left.exclusions || right.exclusions {
        return false;
    }
    match (&left.matcher, &right.matcher) {
        (Matcher::Names(left), Matcher::Names(right)) => left.iter().any(|(local, entries)| {
            right.get(local).is_some_and(|other| {
                entries
                    .iter()
                    .any(|(namespace, _)| other.iter().any(|(candidate, _)| candidate == namespace))
            })
        }),
        (Matcher::Names(names), Matcher::Wildcard(wildcard))
        | (Matcher::Wildcard(wildcard), Matcher::Names(names)) => names.values().any(|entries| {
            entries
                .iter()
                .any(|(namespace, _)| wildcard.allows(namespace.as_deref()))
        }),
        (Matcher::Wildcard(left), Matcher::Wildcard(right)) => wildcards_intersect(left, right),
    }
}

/// Problem of a content model as a whole.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ModelProblem {
    /// Unique Particle Attribution: two particles can match the same
    /// element.
    Ambiguous(String),
    /// Element Declarations Consistent: two elements of the same name are
    /// declared with different types.
    Inconsistent(String),
}

/// Pairs (derived state, base states) explored by [`ContentModel::restricts`].
const MAX_PAIRS: usize = 200_000;
/// Sets of states explored by the determinism check.
const MAX_SETS: usize = 20_000;

impl ContentModel {
    /// Whether every sequence of elements this model accepts is also
    /// accepted by `base` (derivation by restriction, XML Schema 1.0 Part 1
    /// §3.9.6 "Particle Valid (Restriction)", checked on the languages).
    /// Models too large to explore are accepted.
    pub(crate) fn restricts(&self, base: &ContentModel) -> std::result::Result<(), String> {
        let (derived, parent) = match (&self.kind, &base.kind) {
            (Kind::Nfa(derived), Kind::Nfa(parent)) => (derived, parent),
            (Kind::Empty, Kind::Nfa(parent)) => {
                return if parent.closure(parent.start).contains(&parent.accept) {
                    Ok(())
                } else {
                    Err("the base type requires content but the restriction is empty".to_owned())
                };
            }
            (Kind::Nfa(derived), Kind::Empty) => {
                return if derived
                    .closure(derived.start)
                    .iter()
                    .all(|state| derived.edge[*state as usize].is_none())
                {
                    Ok(())
                } else {
                    Err(
                        "the base type has empty content but the restriction allows elements"
                            .to_owned(),
                    )
                };
            }
            _ => return Ok(()),
        };
        let parent_start = parent.closure(parent.start).to_vec();
        let mut queue: Vec<(u32, Vec<u32>)> = derived
            .closure(derived.start)
            .iter()
            .map(|state| (*state, parent_start.clone()))
            .collect();
        let mut visited = HashSet::new();
        while let Some((state, parent_states)) = queue.pop() {
            if !visited.insert((state, parent_states.clone())) {
                continue;
            }
            if visited.len() > MAX_PAIRS {
                return Ok(());
            }
            if state == derived.accept && !parent_states.contains(&parent.accept) {
                return Err(
                    "the restriction accepts content that is incomplete for the base type"
                        .to_owned(),
                );
            }
            let Some((symbol, target)) = derived.edge[state as usize] else {
                continue;
            };
            let symbol = &derived.symbols[symbol as usize];
            let mut nexts: Vec<Vec<u32>> = Vec::new();
            match &symbol.matcher {
                Matcher::Names(names) => {
                    for (local, entries) in names {
                        for (namespace, _) in entries {
                            let mut next =
                                parent.step(&parent_states, namespace.as_deref(), local, true);
                            if next.is_empty() {
                                next =
                                    parent.step(&parent_states, namespace.as_deref(), local, false);
                            }
                            if next.is_empty() {
                                return Err(format!(
                                    "<{}> is not allowed by the base type at this position",
                                    symbol.label
                                ));
                            }
                            nexts.push(next);
                        }
                    }
                }
                Matcher::Wildcard(allowed) => {
                    let mut next = Vec::new();
                    for &parent_state in &parent_states {
                        let Some((parent_symbol, parent_target)) =
                            parent.edge[parent_state as usize]
                        else {
                            continue;
                        };
                        let parent_symbol = &parent.symbols[parent_symbol as usize];
                        if let Matcher::Wildcard(parent_allowed) = &parent_symbol.matcher
                            && (parent_symbol.exclusions
                                || symbol.exclusions
                                || wildcard_covers(parent_allowed, allowed))
                            && strength(symbol.process) >= strength(parent_symbol.process)
                        {
                            next.extend(parent.closure(parent_target).iter().copied());
                        }
                    }
                    next.sort_unstable();
                    next.dedup();
                    if next.is_empty() {
                        return Err(format!(
                            "the wildcard {} is not allowed by the base type at this position",
                            symbol.label
                        ));
                    }
                    nexts.push(next);
                }
            }
            for next in nexts {
                for derived_state in derived.closure(target).iter() {
                    queue.push((*derived_state, next.clone()));
                }
            }
        }
        Ok(())
    }

    /// Unique Particle Attribution and Element Declarations Consistent.
    pub(crate) fn problems(&self) -> Vec<ModelProblem> {
        let mut problems = Vec::new();
        match &self.kind {
            Kind::Nfa(nfa) => {
                // Elements of the same name declared with different types.
                let mut declared: HashMap<(Namespace, String), (&str, &str)> = HashMap::new();
                for symbol in &nfa.symbols {
                    let (Matcher::Names(names), Some(declaration)) =
                        (&symbol.matcher, &symbol.declaration)
                    else {
                        continue;
                    };
                    for (local, entries) in names {
                        for (namespace, _) in entries {
                            match declared.get(&(namespace.clone(), local.clone())) {
                                Some((previous, _)) if previous != declaration => {
                                    problems.push(ModelProblem::Inconsistent(format!(
                                        "the element <{local}> is declared with different types in the same content model"
                                    )));
                                }
                                Some(_) => {}
                                None => {
                                    declared.insert(
                                        (namespace.clone(), local.clone()),
                                        (declaration.as_str(), ""),
                                    );
                                }
                            }
                        }
                    }
                }
                // Determinism.
                let mut queue = vec![nfa.closure(nfa.start).to_vec()];
                let mut visited: HashSet<Vec<u32>> = HashSet::new();
                'sets: while let Some(set) = queue.pop() {
                    if !visited.insert(set.clone()) {
                        continue;
                    }
                    if visited.len() > MAX_SETS {
                        break;
                    }
                    let mut by_symbol: Vec<(u32, Vec<u32>)> = Vec::new();
                    for &state in &set {
                        let Some((symbol, target)) = nfa.edge[state as usize] else {
                            continue;
                        };
                        match by_symbol
                            .iter_mut()
                            .find(|(candidate, _)| *candidate == symbol)
                        {
                            Some((_, targets)) => {
                                targets.extend(nfa.closure(target).iter().copied())
                            }
                            None => by_symbol.push((symbol, nfa.closure(target).to_vec())),
                        }
                    }
                    for (index, (left, _)) in by_symbol.iter().enumerate() {
                        for (right, _) in &by_symbol[index + 1..] {
                            let (first, second) =
                                (&nfa.symbols[*left as usize], &nfa.symbols[*right as usize]);
                            if symbols_overlap(first, second) {
                                problems.push(ModelProblem::Ambiguous(format!(
                                    "<{}> and <{}> can match the same element (Unique Particle Attribution)",
                                    first.label, second.label
                                )));
                                break 'sets;
                            }
                        }
                    }
                    for (_, mut targets) in by_symbol {
                        targets.sort_unstable();
                        targets.dedup();
                        queue.push(targets);
                    }
                }
            }
            Kind::All { members, .. } => {
                for (index, member) in members.iter().enumerate() {
                    for other in &members[index + 1..] {
                        if member.names.iter().any(|(local, entries)| {
                            other.names.get(local).is_some_and(|others| {
                                entries.iter().any(|(namespace, _)| {
                                    others.iter().any(|(candidate, _)| candidate == namespace)
                                })
                            })
                        }) {
                            problems.push(ModelProblem::Inconsistent(format!(
                                "the element <{}> appears twice in an xs:all group",
                                member.label
                            )));
                        }
                    }
                }
            }
            Kind::Empty => {}
        }
        problems
    }

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
    expanding: Vec<(String, Namespace, usize)>,
    /// Symbol of each element or wildcard particle: the copies a counted
    /// repetition makes of a group share them.
    symbol_of: HashMap<*const XsdParticle, u32>,
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

    /// Identifies the type an element particle declares: the qualified type
    /// name, or the address of its anonymous type.
    fn declaration_id(&self, schema: usize, declaration: &XsdElementDecl) -> String {
        let target = self.models.element_target(Located {
            schema,
            item: declaration,
        });
        match (&target.item.type_name, &target.item.anonymous_type) {
            (Some(name), _) => format!(
                "{{{}}}{}",
                name.namespace.as_deref().unwrap_or_default(),
                name.local
            ),
            (None, Some(anonymous)) => format!("anonymous {:p}", &**anonymous),
            (None, None) => "anyType".to_owned(),
        }
    }

    fn particle(&mut self, schema: usize, particle: &XsdParticle, depth: usize) -> (u32, u32) {
        if depth > MAX_DEPTH || self.failed {
            self.failed = true;
            return self.empty_fragment();
        }
        let (min, max) = particle.occurs();
        match particle {
            XsdParticle::Element(declaration) => {
                let symbol = match self.symbol_of.get(&std::ptr::from_ref(particle)) {
                    Some(symbol) => *symbol,
                    None => {
                        let (names, label) = self.names_of(schema, declaration);
                        let symbol = self.symbols.len() as u32;
                        let declaration_id = self.declaration_id(schema, declaration);
                        self.symbols.push(Symbol {
                            matcher: Matcher::Names(names),
                            label,
                            process: None,
                            exclusions: false,
                            declaration: Some(declaration_id),
                        });
                        self.symbol_of.insert(std::ptr::from_ref(particle), symbol);
                        symbol
                    }
                };
                self.repeat(min, max, |builder| builder.symbol_fragment(symbol))
            }
            XsdParticle::Any(wildcard) => {
                if let Some(symbol) = self.symbol_of.get(&std::ptr::from_ref(particle)) {
                    let symbol = *symbol;
                    return self.repeat(min, max, |builder| builder.symbol_fragment(symbol));
                }
                self.wildcards.push(wildcard.namespaces.clone());
                let symbol = self.symbols.len() as u32;
                self.symbol_of.insert(std::ptr::from_ref(particle), symbol);
                self.symbols.push(Symbol {
                    matcher: Matcher::Wildcard(wildcard.namespaces.clone()),
                    process: Some(wildcard.process_contents),
                    exclusions: wildcard.has_exclusions,
                    declaration: None,
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
                // A redefined group referring to itself means the original.
                if let Some(current) = found
                    && let Some((expanding, namespace, group_schema)) = self.expanding.last()
                    && *expanding == name.local
                    && *namespace == name.namespace
                    && let Some(original) = self.models.replaced_group(
                        name.namespace.as_deref(),
                        &name.local,
                        *group_schema,
                        current.item,
                    )
                {
                    found = Some(original);
                }
                let Some(group) = found else {
                    self.failed = true;
                    return self.empty_fragment();
                };
                let Some(content) = &group.item.content else {
                    return self.empty_fragment();
                };
                let group_schema = group.schema;
                let identity = (
                    group.item.name.clone(),
                    group.item.namespace.clone(),
                    group.schema,
                );
                self.repeat(min, max, |builder| {
                    builder.expanding.push(identity.clone());
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
                    // A prohibited particle (maxOccurs 0) is no alternative.
                    if particle.occurs().1 == Some(0) {
                        continue;
                    }
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
            symbol_of: HashMap::new(),
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
