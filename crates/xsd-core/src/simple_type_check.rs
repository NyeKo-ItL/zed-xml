//! Constraints on the facets of simple type restrictions (XML Schema 1.0
//! Part 2, §4.3): applicable facets, facet values in the value space of the
//! base type, consistency between facets and with the facets of the base
//! type, and valid regular expressions.

use std::cmp::Ordering;

use crate::{
    datatypes::{BuiltinType, SimpleType, Value, Variety, WhiteSpace},
    model::{XsdFacets, XsdModelSet, XsdQName},
    pattern,
    schema_check::{INVALID_VALUE, SchemaDocument, SchemaProblem},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Category {
    Text,
    Boolean,
    Decimal,
    Ordered,
    List,
    Union,
}

const TEXT_FACETS: &[&str] = &[
    "length",
    "minLength",
    "maxLength",
    "pattern",
    "enumeration",
    "whiteSpace",
];
const BOOLEAN_FACETS: &[&str] = &["pattern", "whiteSpace"];
const DECIMAL_FACETS: &[&str] = &[
    "totalDigits",
    "fractionDigits",
    "pattern",
    "whiteSpace",
    "enumeration",
    "maxInclusive",
    "maxExclusive",
    "minInclusive",
    "minExclusive",
];
const ORDERED_FACETS: &[&str] = &[
    "pattern",
    "whiteSpace",
    "enumeration",
    "maxInclusive",
    "maxExclusive",
    "minInclusive",
    "minExclusive",
];
const UNION_FACETS: &[&str] = &["pattern", "enumeration"];

fn category(simple: &SimpleType<'_>) -> Option<Category> {
    Some(match &simple.variety {
        Variety::List(_) => Category::List,
        Variety::Union(_) => Category::Union,
        Variety::Atomic(builtin) => {
            use BuiltinType::*;
            match builtin {
                AnySimpleType => return None,
                Boolean => Category::Boolean,
                Decimal | Integer | NonPositiveInteger | NegativeInteger | Long | Int | Short
                | Byte | NonNegativeInteger | UnsignedLong | UnsignedInt | UnsignedShort
                | UnsignedByte | PositiveInteger => Category::Decimal,
                Float | Double | Duration | DateTime | Time | Date | GYearMonth | GYear
                | GMonthDay | GDay | GMonth => Category::Ordered,
                IdRefs | Entities | NmTokens => Category::List,
                _ => Category::Text,
            }
        }
    })
}

fn allowed_facets(category: Category) -> &'static [&'static str] {
    match category {
        Category::Text | Category::List => TEXT_FACETS,
        Category::Boolean => BOOLEAN_FACETS,
        Category::Decimal => DECIMAL_FACETS,
        Category::Ordered => ORDERED_FACETS,
        Category::Union => UNION_FACETS,
    }
}

/// A facet of the restriction under check.
struct Facet<'a> {
    name: &'a str,
    element: usize,
    value: String,
}

/// Resolves the QName written in an attribute of `element`.
pub(crate) fn resolve_qname(
    document: &SchemaDocument<'_>,
    element: usize,
    value: &str,
) -> Option<XsdQName> {
    let value = value.trim();
    let (prefix, local) = match value.split_once(':') {
        Some((prefix, local)) => (Some(prefix), local),
        None => (None, value),
    };
    let namespace = match document.namespace(element, prefix) {
        Some(namespace) => namespace.map(str::to_owned),
        None if prefix.is_some() => return None,
        None => None,
    };
    Some(XsdQName {
        prefix: prefix.map(str::to_owned),
        namespace,
        local: local.to_owned(),
    })
}

/// Checks the facets of every simple type restriction of the document.
pub(crate) fn check_simple_types(
    document: &SchemaDocument<'_>,
    models: &XsdModelSet,
    problems: &mut Vec<SchemaProblem>,
) {
    for element in 0..document.names.len() {
        if document.names[element] != Some("restriction") {
            continue;
        }
        let Some(parent) = document.parent(element) else {
            continue;
        };
        if document.local(parent) != "simpleType" {
            continue;
        }
        let Some(base) = document.value(element, "base") else {
            continue;
        };
        let Some(base) = resolve_qname(document, element, &base) else {
            continue;
        };
        check_restriction(document, models, element, &base, problems);
    }
}

fn check_restriction(
    document: &SchemaDocument<'_>,
    models: &XsdModelSet,
    element: usize,
    base: &XsdQName,
    problems: &mut Vec<SchemaProblem>,
) {
    let reference = models.resolve_type(0, base);
    let Some(simple) = models.simple_type(reference) else {
        return;
    };
    let Some(category) = category(&simple) else {
        return;
    };
    let facets = document.children[element]
        .iter()
        .filter_map(|&child| {
            let name = document.names[child]?;
            (name != "annotation" && name != "simpleType").then(|| Facet {
                name,
                element: child,
                value: document.value(child, "value").unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    let report = |facet: &Facet<'_>, message: String, problems: &mut Vec<SchemaProblem>| {
        let range = document
            .attribute(facet.element, "value")
            .and_then(|attribute| attribute.value.clone())
            .unwrap_or_else(|| document.name_range(facet.element));
        problems.push(SchemaProblem {
            range,
            rule: INVALID_VALUE,
            message,
        });
    };

    let allowed = allowed_facets(category);
    let mut seen: Vec<&str> = Vec::new();
    for facet in &facets {
        if !allowed.contains(&facet.name) {
            problems.push(SchemaProblem {
                range: document.name_range(facet.element),
                rule: INVALID_VALUE,
                message: format!(
                    "the facet xs:{} is not applicable to the base type {}",
                    facet.name, simple.name
                ),
            });
            continue;
        }
        if !matches!(facet.name, "pattern" | "enumeration") {
            if seen.contains(&facet.name) {
                report(
                    facet,
                    format!("the facet xs:{} is specified twice", facet.name),
                    problems,
                );
            }
            seen.push(facet.name);
        }
    }

    let lookup = |name: &str| facets.iter().find(|facet| facet.name == name);
    let number = |facet: &Facet<'_>| facet.value.trim().parse::<u64>().ok();

    for facet in &facets {
        match facet.name {
            "pattern" => {
                if let Err(error) = pattern::translate(&facet.value) {
                    report(
                        facet,
                        format!("'{}' is not a valid pattern: {error}", facet.value),
                        problems,
                    );
                }
            }
            "enumeration" if allowed.contains(&"enumeration") => {
                if let Err(error) = simple.validate(&facet.value, None) {
                    report(
                        facet,
                        format!(
                            "the enumeration value '{}' is not valid for the base type {}: {}",
                            facet.value, simple.name, error.message
                        ),
                        problems,
                    );
                }
            }
            "whiteSpace" => {
                let Some(requested) = WhiteSpace::from_facet(&facet.value) else {
                    continue;
                };
                let base_white_space = simple.white_space();
                if requested < base_white_space {
                    report(
                        facet,
                        format!(
                            "whiteSpace cannot be relaxed: the base type {} already applies {}",
                            simple.name,
                            match base_white_space {
                                WhiteSpace::Preserve => "preserve",
                                WhiteSpace::Replace => "replace",
                                WhiteSpace::Collapse => "collapse",
                            }
                        ),
                        problems,
                    );
                }
            }
            "fractionDigits"
                if matches!(&simple.variety, Variety::Atomic(builtin) if *builtin != BuiltinType::Decimal)
                    && number(facet).is_some_and(|digits| digits > 0) =>
            {
                report(
                    facet,
                    "fractionDigits of an integer type must be 0".to_owned(),
                    problems,
                );
            }
            _ => {}
        }
    }

    check_lengths(&facets, &lookup, &number, &simple, &report, problems);
    check_digits(&facets, &lookup, &number, &simple, &report, problems);
    if category == Category::Decimal || category == Category::Ordered {
        check_bounds(&facets, &lookup, &simple, &report, problems);
    }
}

type Report<'r> = dyn Fn(&Facet<'_>, String, &mut Vec<SchemaProblem>) + 'r;

/// `length`, `minLength` and `maxLength`: between them and against the base.
fn check_lengths<'a>(
    facets: &[Facet<'a>],
    lookup: &dyn Fn(&str) -> Option<&'a Facet<'a>>,
    number: &dyn Fn(&Facet<'_>) -> Option<u64>,
    simple: &SimpleType<'_>,
    report: &Report<'_>,
    problems: &mut Vec<SchemaProblem>,
) {
    let _ = facets;
    let length = lookup("length").and_then(|facet| number(facet).map(|value| (facet, value)));
    let minimum = lookup("minLength").and_then(|facet| number(facet).map(|value| (facet, value)));
    let maximum = lookup("maxLength").and_then(|facet| number(facet).map(|value| (facet, value)));
    if let (Some((facet, minimum)), Some((_, maximum))) = (minimum, maximum)
        && minimum > maximum
    {
        report(
            facet,
            format!("minLength ({minimum}) is greater than maxLength ({maximum})"),
            problems,
        );
    }
    if let Some((_, length)) = length
        && let Some((facet, _)) = minimum.or(maximum)
    {
        // XML Schema 1.0 Part 2 §4.3.1.4: `length` with `minLength` or
        // `maxLength` in the same derivation step is an error.
        report(
            facet,
            format!(
                "{} cannot be combined with length ({length}) in the same restriction",
                facet.name
            ),
            problems,
        );
    }
    if let Some((facet, length)) = length {
        if let Some((_, minimum)) = minimum
            && minimum > length
        {
            report(
                facet,
                format!("length ({length}) is less than minLength ({minimum})"),
                problems,
            );
        }
        if let Some((_, maximum)) = maximum
            && length > maximum
        {
            report(
                facet,
                format!("length ({length}) is greater than maxLength ({maximum})"),
                problems,
            );
        }
    }
    let base = |select: &dyn Fn(&XsdFacets) -> Option<&String>| {
        simple
            .facets
            .iter()
            .find_map(|facets| select(facets))
            .and_then(|value| value.trim().parse::<u64>().ok())
    };
    let base_length = base(&|facets| facets.length.as_ref());
    // The built-in list types have at least one item.
    let builtin_list = matches!(simple.variety, Variety::List(_))
        && ["NMTOKENS", "IDREFS", "ENTITIES"]
            .iter()
            .any(|name| simple.name.ends_with(name));
    let base_minimum = base(&|facets| facets.min_length.as_ref()).or(builtin_list.then_some(1));
    let base_maximum = base(&|facets| facets.max_length.as_ref());
    if let Some((facet, value)) = length {
        if base_length.is_some_and(|base| base != value) {
            report(
                facet,
                format!("length ({value}) differs from the length of the base type"),
                problems,
            );
        }
        if base_minimum.is_some_and(|base| value < base)
            || base_maximum.is_some_and(|base| value > base)
        {
            report(
                facet,
                format!("length ({value}) is outside the length range of the base type"),
                problems,
            );
        }
    }
    if let Some((facet, value)) = minimum {
        if base_minimum.is_some_and(|base| value < base) {
            report(
                facet,
                format!("minLength ({value}) is less than the minLength of the base type"),
                problems,
            );
        }
        if base_maximum.is_some_and(|base| value > base) {
            report(
                facet,
                format!("minLength ({value}) is greater than the maxLength of the base type"),
                problems,
            );
        }
    }
    if let Some((facet, value)) = maximum {
        if base_maximum.is_some_and(|base| value > base) {
            report(
                facet,
                format!("maxLength ({value}) is greater than the maxLength of the base type"),
                problems,
            );
        }
        if base_minimum.is_some_and(|base| value < base) {
            report(
                facet,
                format!("maxLength ({value}) is less than the minLength of the base type"),
                problems,
            );
        }
    }
}

fn check_digits<'a>(
    facets: &[Facet<'a>],
    lookup: &dyn Fn(&str) -> Option<&'a Facet<'a>>,
    number: &dyn Fn(&Facet<'_>) -> Option<u64>,
    simple: &SimpleType<'_>,
    report: &Report<'_>,
    problems: &mut Vec<SchemaProblem>,
) {
    let _ = facets;
    let total = lookup("totalDigits").and_then(|facet| number(facet).map(|value| (facet, value)));
    let fraction =
        lookup("fractionDigits").and_then(|facet| number(facet).map(|value| (facet, value)));
    let base = |select: &dyn Fn(&XsdFacets) -> Option<&String>| {
        simple
            .facets
            .iter()
            .find_map(|facets| select(facets))
            .and_then(|value| value.trim().parse::<u64>().ok())
    };
    let base_total = base(&|facets| facets.total_digits.as_ref());
    let base_fraction = base(&|facets| facets.fraction_digits.as_ref());
    if let (Some((facet, total)), Some((_, fraction))) = (total, fraction)
        && fraction > total
    {
        report(
            facet,
            format!("fractionDigits ({fraction}) is greater than totalDigits ({total})"),
            problems,
        );
    }
    if let Some((facet, value)) = total
        && base_total.is_some_and(|base| value > base)
    {
        report(
            facet,
            format!("totalDigits ({value}) is greater than the totalDigits of the base type"),
            problems,
        );
    }
    if let Some((facet, value)) = fraction
        && base_fraction.is_some_and(|base| value > base)
    {
        report(
            facet,
            format!("fractionDigits ({value}) is greater than the fractionDigits of the base type"),
            problems,
        );
    }
}

/// Value of a bound facet in the value space of the atomic base type.
fn bound(simple: &SimpleType<'_>, raw: &str) -> Option<Value> {
    let Variety::Atomic(builtin) = &simple.variety else {
        return None;
    };
    let collapsed = WhiteSpace::Collapse.apply(raw);
    builtin.parse(&collapsed, None).ok()
}

fn check_bounds<'a>(
    facets: &[Facet<'a>],
    lookup: &dyn Fn(&str) -> Option<&'a Facet<'a>>,
    simple: &SimpleType<'_>,
    report: &Report<'_>,
    problems: &mut Vec<SchemaProblem>,
) {
    // Values must be valid in the lexical space of the base type.
    let mut parsed: Vec<(&str, &Facet<'_>, Value)> = Vec::new();
    for facet in facets {
        if !matches!(
            facet.name,
            "minInclusive" | "minExclusive" | "maxInclusive" | "maxExclusive"
        ) {
            continue;
        }
        match bound(simple, &facet.value) {
            Some(value) => parsed.push((facet.name, facet, value)),
            None => {
                let reason = match &simple.variety {
                    Variety::Atomic(builtin) => builtin
                        .parse(&WhiteSpace::Collapse.apply(&facet.value), None)
                        .err()
                        .unwrap_or_default(),
                    _ => String::new(),
                };
                report(
                    facet,
                    format!(
                        "'{}' is not a valid value for the base type {}: {reason}",
                        facet.value, simple.name
                    ),
                    problems,
                );
            }
        }
    }
    if lookup("minInclusive").is_some()
        && let Some(facet) = lookup("minExclusive")
    {
        report(
            facet,
            "minInclusive and minExclusive cannot both be specified".to_owned(),
            problems,
        );
    }
    if lookup("maxInclusive").is_some()
        && let Some(facet) = lookup("maxExclusive")
    {
        report(
            facet,
            "maxInclusive and maxExclusive cannot both be specified".to_owned(),
            problems,
        );
    }
    let value_of = |name: &str| {
        parsed
            .iter()
            .find(|(candidate, _, _)| *candidate == name)
            .map(|(_, facet, value)| (*facet, value.clone()))
    };
    // (lower bound, upper bound, whether equality is allowed)
    for (lower_name, upper_name, equal_allowed) in [
        ("minInclusive", "maxInclusive", true),
        ("minInclusive", "maxExclusive", false),
        ("minExclusive", "maxInclusive", false),
        ("minExclusive", "maxExclusive", true),
    ] {
        if let (Some((_, lower)), Some((facet, upper))) =
            (value_of(lower_name), value_of(upper_name))
        {
            let consistent = match lower.compare(&upper) {
                Some(Ordering::Less) => true,
                Some(Ordering::Equal) => equal_allowed,
                Some(Ordering::Greater) => false,
                None => true,
            };
            if !consistent {
                report(
                    facet,
                    format!("{lower_name} is greater than or equal to {upper_name}"),
                    problems,
                );
            }
        }
    }
    // Against the bounds of the base type.
    let base_bound = |select: &dyn Fn(&XsdFacets) -> Option<&String>| {
        simple
            .facets
            .iter()
            .find_map(|facets| select(facets))
            .and_then(|value| bound(simple, value))
    };
    let implicit = match &simple.variety {
        Variety::Atomic(builtin) => builtin.implicit_bounds(),
        _ => (None, None),
    };
    let base_max_inclusive = base_bound(&|facets| facets.max_inclusive.as_ref())
        .or_else(|| implicit.1.and_then(|value| bound(simple, value)));
    let base_max_exclusive = base_bound(&|facets| facets.max_exclusive.as_ref());
    let base_min_inclusive = base_bound(&|facets| facets.min_inclusive.as_ref())
        .or_else(|| implicit.0.and_then(|value| bound(simple, value)));
    let base_min_exclusive = base_bound(&|facets| facets.min_exclusive.as_ref());
    let within = |value: &Value, limit: &Option<Value>, allowed: &[Ordering]| {
        limit.as_ref().is_none_or(|limit| {
            value
                .compare(limit)
                .is_none_or(|order| allowed.contains(&order))
        })
    };
    for (name, facet, value) in &parsed {
        let (ok, what) = match *name {
            "maxInclusive" => (
                within(
                    value,
                    &base_max_inclusive,
                    &[Ordering::Less, Ordering::Equal],
                ) && within(value, &base_max_exclusive, &[Ordering::Less])
                    && within(
                        value,
                        &base_min_inclusive,
                        &[Ordering::Greater, Ordering::Equal],
                    )
                    && within(value, &base_min_exclusive, &[Ordering::Greater]),
                "maxInclusive",
            ),
            "maxExclusive" => (
                within(
                    value,
                    &base_max_inclusive,
                    &[Ordering::Less, Ordering::Equal],
                ) && within(
                    value,
                    &base_max_exclusive,
                    &[Ordering::Less, Ordering::Equal],
                ) && within(value, &base_min_inclusive, &[Ordering::Greater])
                    && within(value, &base_min_exclusive, &[Ordering::Greater]),
                "maxExclusive",
            ),
            "minInclusive" => (
                within(
                    value,
                    &base_min_inclusive,
                    &[Ordering::Greater, Ordering::Equal],
                ) && within(value, &base_min_exclusive, &[Ordering::Greater])
                    && within(
                        value,
                        &base_max_inclusive,
                        &[Ordering::Less, Ordering::Equal],
                    )
                    && within(value, &base_max_exclusive, &[Ordering::Less]),
                "minInclusive",
            ),
            _ => (
                within(
                    value,
                    &base_min_inclusive,
                    &[Ordering::Greater, Ordering::Equal],
                ) && within(
                    value,
                    &base_min_exclusive,
                    &[Ordering::Greater, Ordering::Equal],
                ) && within(value, &base_max_inclusive, &[Ordering::Less])
                    && within(value, &base_max_exclusive, &[Ordering::Less]),
                "minExclusive",
            ),
        };
        if !ok {
            report(
                facet,
                format!(
                    "{what} is outside the range of the base type {}",
                    simple.name
                ),
                problems,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::schema_check::check_schema_document;

    fn problems(restriction: &str) -> Vec<String> {
        let source = format!(
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema"><xs:simpleType name="b"><xs:restriction base="xs:int"><xs:minInclusive value="0"/><xs:maxInclusive value="100"/><xs:totalDigits value="5"/></xs:restriction></xs:simpleType><xs:simpleType name="t">{restriction}</xs:simpleType></xs:schema>"#
        );
        check_schema_document(&source)
            .into_iter()
            .map(|problem| problem.message)
            .collect()
    }

    #[test]
    fn accepts_valid_restrictions() {
        for restriction in [
            r#"<xs:restriction base="xs:string"><xs:length value="3"/><xs:pattern value="[a-z]+"/><xs:whiteSpace value="collapse"/></xs:restriction>"#,
            r#"<xs:restriction base="b"><xs:minInclusive value="10"/><xs:maxExclusive value="100"/><xs:totalDigits value="3"/></xs:restriction>"#,
            r#"<xs:restriction base="xs:decimal"><xs:fractionDigits value="2"/><xs:totalDigits value="5"/></xs:restriction>"#,
            r#"<xs:restriction base="xs:date"><xs:minInclusive value="2000-01-01"/><xs:enumeration value="2001-02-03"/></xs:restriction>"#,
            r#"<xs:restriction base="xs:string"><xs:pattern value="[a-z--[b-z]]"/></xs:restriction>"#,
            r#"<xs:restriction base="xs:token"><xs:whiteSpace value="collapse"/></xs:restriction>"#,
            r#"<xs:restriction base="xs:NMTOKENS"><xs:minLength value="1"/></xs:restriction>"#,
        ] {
            assert_eq!(problems(restriction), Vec::<String>::new(), "{restriction}");
        }
    }

    #[test]
    fn reports_facets_that_do_not_apply_or_have_invalid_values() {
        let bad = |restriction: &str, needle: &str| {
            let found = problems(restriction);
            assert!(
                found.iter().any(|message| message.contains(needle)),
                "{restriction}: {found:?}"
            );
        };
        bad(
            r#"<xs:restriction base="xs:int"><xs:length value="3"/></xs:restriction>"#,
            "not applicable",
        );
        bad(
            r#"<xs:restriction base="xs:string"><xs:fractionDigits value="1"/></xs:restriction>"#,
            "not applicable",
        );
        bad(
            r#"<xs:restriction base="xs:boolean"><xs:enumeration value="true"/></xs:restriction>"#,
            "not applicable",
        );
        bad(
            r#"<xs:restriction base="xs:gMonth"><xs:minExclusive value="--01--"/></xs:restriction>"#,
            "not a valid value",
        );
        bad(
            r#"<xs:restriction base="xs:byte"><xs:maxInclusive value="200"/></xs:restriction>"#,
            "not a valid value",
        );
        bad(
            r#"<xs:restriction base="xs:int"><xs:enumeration value="abc"/></xs:restriction>"#,
            "not valid for the base type",
        );
        bad(
            r#"<xs:restriction base="xs:string"><xs:pattern value="[a-z"/></xs:restriction>"#,
            "not a valid pattern",
        );
        bad(
            r#"<xs:restriction base="xs:string"><xs:minLength value="5"/><xs:maxLength value="2"/></xs:restriction>"#,
            "minLength",
        );
        bad(
            r#"<xs:restriction base="xs:string"><xs:length value="2"/><xs:minLength value="5"/></xs:restriction>"#,
            "length",
        );
        bad(
            r#"<xs:restriction base="xs:int"><xs:minInclusive value="5"/><xs:minExclusive value="4"/></xs:restriction>"#,
            "both",
        );
        bad(
            r#"<xs:restriction base="xs:int"><xs:minInclusive value="9"/><xs:maxInclusive value="3"/></xs:restriction>"#,
            "greater",
        );
        bad(
            r#"<xs:restriction base="xs:decimal"><xs:totalDigits value="2"/><xs:fractionDigits value="3"/></xs:restriction>"#,
            "fractionDigits",
        );
        bad(
            r#"<xs:restriction base="xs:integer"><xs:fractionDigits value="1"/></xs:restriction>"#,
            "must be 0",
        );
        bad(
            r#"<xs:restriction base="xs:token"><xs:whiteSpace value="preserve"/></xs:restriction>"#,
            "cannot be relaxed",
        );
        bad(
            r#"<xs:restriction base="xs:string"><xs:maxLength value="2"/><xs:maxLength value="3"/></xs:restriction>"#,
            "twice",
        );
        // Against the facets of the base type.
        bad(
            r#"<xs:restriction base="b"><xs:maxInclusive value="200"/></xs:restriction>"#,
            "outside the range",
        );
        bad(
            r#"<xs:restriction base="b"><xs:totalDigits value="9"/></xs:restriction>"#,
            "totalDigits",
        );
    }
}
