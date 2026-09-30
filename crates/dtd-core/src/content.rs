//! Content models (`<!ELEMENT>`) and their recognition automaton.
//!
//! A `children` model is compiled into a nondeterministic finite automaton
//! (Thompson construction: one pair of states per particle, epsilon
//! transitions for `?`, `*`, `+` and choices) and then simulated with sets of
//! states. The automaton size is linear in the model size and recognition
//! is `O(states × children)`, without backtracking.

use std::fmt;

/// Cardinality of a particle (`?`, `*`, `+` or none).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occurrence {
    Once,
    Optional,
    ZeroOrMore,
    OneOrMore,
}

impl Occurrence {
    fn suffix(self) -> &'static str {
        match self {
            Occurrence::Once => "",
            Occurrence::Optional => "?",
            Occurrence::ZeroOrMore => "*",
            Occurrence::OneOrMore => "+",
        }
    }
}

/// Particle of a `children` model: name, sequence (`,`) or choice (`|`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParticleKind {
    Name(String),
    Sequence(Vec<ContentParticle>),
    Choice(Vec<ContentParticle>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentParticle {
    pub kind: ParticleKind,
    pub occurrence: Occurrence,
}

impl ContentParticle {
    /// Element names referenced by the particle, in order, without duplicates.
    pub fn names(&self) -> Vec<&str> {
        let mut names = Vec::new();
        self.collect_names(&mut names);
        names
    }

    fn collect_names<'a>(&'a self, names: &mut Vec<&'a str>) {
        match &self.kind {
            ParticleKind::Name(name) => {
                if !names.contains(&name.as_str()) {
                    names.push(name);
                }
            }
            ParticleKind::Sequence(items) | ParticleKind::Choice(items) => {
                for item in items {
                    item.collect_names(names);
                }
            }
        }
    }
}

impl fmt::Display for ContentParticle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            ParticleKind::Name(name) => formatter.write_str(name)?,
            ParticleKind::Sequence(items) | ParticleKind::Choice(items) => {
                let separator = if matches!(self.kind, ParticleKind::Choice(_)) {
                    " | "
                } else {
                    ", "
                };
                formatter.write_str("(")?;
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str(separator)?;
                    }
                    write!(formatter, "{item}")?;
                }
                formatter.write_str(")")?;
            }
        }
        formatter.write_str(self.occurrence.suffix())
    }
}

/// Content specification of an `<!ELEMENT>` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentSpec {
    /// `EMPTY`: no content.
    Empty,
    /// `ANY`: any declared element and text.
    Any,
    /// `(#PCDATA | a | b)*`: text and the listed elements, in any
    /// order.
    Mixed(Vec<String>),
    /// Element-only model (whitespace allowed between children).
    Children(ContentParticle),
}

impl ContentSpec {
    /// Element names referenced by the model.
    pub fn names(&self) -> Vec<&str> {
        match self {
            ContentSpec::Empty | ContentSpec::Any => Vec::new(),
            ContentSpec::Mixed(names) => names.iter().map(String::as_str).collect(),
            ContentSpec::Children(particle) => particle.names(),
        }
    }
}

impl fmt::Display for ContentSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContentSpec::Empty => formatter.write_str("EMPTY"),
            ContentSpec::Any => formatter.write_str("ANY"),
            ContentSpec::Mixed(names) if names.is_empty() => formatter.write_str("(#PCDATA)"),
            ContentSpec::Mixed(names) => write!(formatter, "(#PCDATA | {})*", names.join(" | ")),
            ContentSpec::Children(particle) => {
                // A single name is written between parentheses in a declaration.
                if matches!(particle.kind, ParticleKind::Name(_)) {
                    write!(formatter, "({particle})")
                } else {
                    write!(formatter, "{particle}")
                }
            }
        }
    }
}

#[derive(Debug, Default, Clone)]
struct State {
    epsilon: Vec<usize>,
    transitions: Vec<(String, usize)>,
}

/// Recognition automaton of a `children` model.
#[derive(Debug, Clone)]
pub struct ContentAutomaton {
    states: Vec<State>,
    start: usize,
    accept: usize,
}

impl ContentAutomaton {
    pub fn new(particle: &ContentParticle) -> Self {
        let mut automaton = Self {
            states: Vec::new(),
            start: 0,
            accept: 0,
        };
        let (start, accept) = automaton.build(particle);
        automaton.start = start;
        automaton.accept = accept;
        automaton
    }

    /// Number of states (linear in the size of the model).
    pub fn state_count(&self) -> usize {
        self.states.len()
    }

    fn state(&mut self) -> usize {
        self.states.push(State::default());
        self.states.len() - 1
    }

    /// Builds the fragment of `particle`; returns `(entry, exit)`.
    fn build(&mut self, particle: &ContentParticle) -> (usize, usize) {
        let (start, end) = match &particle.kind {
            ParticleKind::Name(name) => {
                let start = self.state();
                let end = self.state();
                self.states[start].transitions.push((name.clone(), end));
                (start, end)
            }
            ParticleKind::Sequence(items) => {
                let start = self.state();
                let mut current = start;
                for item in items {
                    let (item_start, item_end) = self.build(item);
                    self.states[current].epsilon.push(item_start);
                    current = item_end;
                }
                (start, current)
            }
            ParticleKind::Choice(items) => {
                let start = self.state();
                let end = self.state();
                for item in items {
                    let (item_start, item_end) = self.build(item);
                    self.states[start].epsilon.push(item_start);
                    self.states[item_end].epsilon.push(end);
                }
                (start, end)
            }
        };
        match particle.occurrence {
            Occurrence::Once => (start, end),
            Occurrence::Optional => {
                self.states[start].epsilon.push(end);
                (start, end)
            }
            Occurrence::ZeroOrMore => {
                let exit = self.state();
                self.states[start].epsilon.push(exit);
                self.states[end].epsilon.push(start);
                (start, exit)
            }
            Occurrence::OneOrMore => {
                let exit = self.state();
                self.states[end].epsilon.push(start);
                self.states[end].epsilon.push(exit);
                (start, exit)
            }
        }
    }

    /// Epsilon closure of `states`, sorted.
    fn closure(&self, states: impl IntoIterator<Item = usize>) -> Vec<usize> {
        let mut seen = vec![false; self.states.len()];
        let mut stack = states.into_iter().collect::<Vec<_>>();
        let mut closure = Vec::new();
        while let Some(state) = stack.pop() {
            if std::mem::replace(&mut seen[state], true) {
                continue;
            }
            closure.push(state);
            stack.extend(self.states[state].epsilon.iter().copied());
        }
        closure.sort_unstable();
        closure
    }

    /// Recognizer positioned at the start of the content.
    pub fn matcher(&self) -> ContentMatcher<'_> {
        ContentMatcher {
            automaton: self,
            current: self.closure([self.start]),
        }
    }

    /// Whether the sequence of names `names` is valid content.
    pub fn matches<'n>(&self, names: impl IntoIterator<Item = &'n str>) -> bool {
        let mut matcher = self.matcher();
        names.into_iter().all(|name| matcher.feed(name)) && matcher.accepts()
    }
}

/// Incremental recognition of a sequence of children.
#[derive(Debug, Clone)]
pub struct ContentMatcher<'a> {
    automaton: &'a ContentAutomaton,
    current: Vec<usize>,
}

impl ContentMatcher<'_> {
    /// Consumes the child `name`. Returns `false` (and stays in place) if the
    /// element is not allowed at this position.
    pub fn feed(&mut self, name: &str) -> bool {
        let next = self
            .current
            .iter()
            .flat_map(|&state| &self.automaton.states[state].transitions)
            .filter(|(expected, _)| expected == name)
            .map(|(_, target)| *target)
            .collect::<Vec<_>>();
        if next.is_empty() {
            return false;
        }
        self.current = self.automaton.closure(next);
        true
    }

    /// The content consumed so far is complete.
    pub fn accepts(&self) -> bool {
        self.current.binary_search(&self.automaton.accept).is_ok()
    }

    /// Element names allowed at the current position, sorted.
    pub fn expected(&self) -> Vec<String> {
        let mut names = self
            .current
            .iter()
            .flat_map(|&state| &self.automaton.states[state].transitions)
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(value: &str, occurrence: Occurrence) -> ContentParticle {
        ContentParticle {
            kind: ParticleKind::Name(value.to_owned()),
            occurrence,
        }
    }

    fn group(choice: bool, items: Vec<ContentParticle>, occurrence: Occurrence) -> ContentParticle {
        ContentParticle {
            kind: if choice {
                ParticleKind::Choice(items)
            } else {
                ParticleKind::Sequence(items)
            },
            occurrence,
        }
    }

    #[test]
    fn matches_sequences_choices_and_occurrences() {
        // (head, (p | list)*, foot?)
        let model = group(
            false,
            vec![
                name("head", Occurrence::Once),
                group(
                    true,
                    vec![name("p", Occurrence::Once), name("list", Occurrence::Once)],
                    Occurrence::ZeroOrMore,
                ),
                name("foot", Occurrence::Optional),
            ],
            Occurrence::Once,
        );
        assert_eq!(model.to_string(), "(head, (p | list)*, foot?)");
        let automaton = ContentAutomaton::new(&model);
        assert!(automaton.matches(["head"]));
        assert!(automaton.matches(["head", "p", "list", "p", "foot"]));
        assert!(!automaton.matches([]));
        assert!(!automaton.matches(["p"]));
        assert!(!automaton.matches(["head", "foot", "p"]));

        let mut matcher = automaton.matcher();
        assert_eq!(matcher.expected(), vec!["head"]);
        assert!(matcher.feed("head"));
        assert_eq!(matcher.expected(), vec!["foot", "list", "p"]);
        assert!(matcher.accepts());
        assert!(matcher.feed("foot"));
        assert!(matcher.expected().is_empty());
        assert!(!matcher.feed("p"));
        assert!(matcher.accepts());
    }

    #[test]
    fn handles_one_or_more_and_nested_optional_groups() {
        // (a, b?)+
        let model = group(
            false,
            vec![name("a", Occurrence::Once), name("b", Occurrence::Optional)],
            Occurrence::OneOrMore,
        );
        let automaton = ContentAutomaton::new(&model);
        assert!(automaton.matches(["a"]));
        assert!(automaton.matches(["a", "a", "b", "a"]));
        assert!(!automaton.matches(["b"]));
        assert!(!automaton.matches(["a", "b", "b"]));
        assert!(!automaton.matches([]));

        // ((a | b)*, c)*: ambiguities and epsilon loops without getting stuck.
        let nested = group(
            false,
            vec![
                group(
                    true,
                    vec![name("a", Occurrence::Once), name("b", Occurrence::Once)],
                    Occurrence::ZeroOrMore,
                ),
                name("c", Occurrence::Once),
            ],
            Occurrence::ZeroOrMore,
        );
        let automaton = ContentAutomaton::new(&nested);
        assert!(automaton.matches([]));
        assert!(automaton.matches(["c", "a", "b", "c"]));
        assert!(!automaton.matches(["a"]));
    }

    #[test]
    fn displays_content_specs() {
        assert_eq!(ContentSpec::Empty.to_string(), "EMPTY");
        assert_eq!(ContentSpec::Any.to_string(), "ANY");
        assert_eq!(ContentSpec::Mixed(Vec::new()).to_string(), "(#PCDATA)");
        assert_eq!(
            ContentSpec::Mixed(vec!["b".to_owned(), "i".to_owned()]).to_string(),
            "(#PCDATA | b | i)*"
        );
        assert_eq!(
            ContentSpec::Children(name("item", Occurrence::OneOrMore)).to_string(),
            "(item+)"
        );
        let spec = ContentSpec::Children(group(
            true,
            vec![name("a", Occurrence::Once), name("b", Occurrence::Once)],
            Occurrence::Once,
        ));
        assert_eq!(spec.names(), vec!["a", "b"]);
    }
}
