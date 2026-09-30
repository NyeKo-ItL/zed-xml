//! XML Schema 1.0 Part 2 datatypes: the built-in type hierarchy, lexical
//! spaces, value spaces and constraining facets.
//!
//! [`BuiltinType::parse`] checks a value against the lexical space of a
//! built-in type and maps it to its value (numbers, dates, durations,
//! binary data...). [`SimpleType`] describes a simple type (atomic, list or
//! union) with the facets of each restriction step, and
//! [`SimpleType::validate`] applies, in order, whitespace processing
//! (`preserve`, `replace`, `collapse`), the lexical check and every
//! constraining facet, comparing bounds and enumerations in the value space
//! (`1.0` equals `1` for `xs:decimal`, `2026-01-01T00:00:00Z` equals
//! `2026-01-01T01:00:00+01:00`). [`XsdModelSet::simple_type`] builds a
//! [`SimpleType`] from the component model; the validator of
//! [`crate::validate_document_located`] and the language server share it.
//!
//! Errors are sentences describing the rule that failed, such as
//! `'2024-13-01' is not a valid xs:date: month must be 01-12`.

use std::{borrow::Cow, cmp::Ordering};

use xml_core::names::{is_name, is_ncname, is_nmtoken, is_qname};

use crate::{
    model::{MAX_DEPTH, XsdDerivation, XsdFacets, XsdModelSet, XsdQName, XsdTypeRef},
    pattern,
};

/// Longest value quoted in full by a message.
const MAX_QUOTED_CHARS: usize = 64;
/// Maximum number of enumeration values listed by a message.
const MAX_LISTED_VALUES: usize = 10;
/// Picoseconds per second: the resolution of dates, times and durations.
const PICOS: i128 = 1_000_000_000_000;
/// Largest absolute year mapped to the timeline (larger years are only
/// checked lexically).
const MAX_TIMELINE_YEAR: i128 = 1_000_000_000_000_000;

/// Built-in simple types of XML Schema 1.0 (`anyType` is not simple).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinType {
    AnySimpleType,
    String,
    NormalizedString,
    Token,
    Language,
    Name,
    NcName,
    Id,
    IdRef,
    IdRefs,
    Entity,
    Entities,
    NmToken,
    NmTokens,
    QName,
    Notation,
    AnyUri,
    Boolean,
    Decimal,
    Integer,
    NonPositiveInteger,
    NegativeInteger,
    Long,
    Int,
    Short,
    Byte,
    NonNegativeInteger,
    UnsignedLong,
    UnsignedInt,
    UnsignedShort,
    UnsignedByte,
    PositiveInteger,
    Float,
    Double,
    Duration,
    DateTime,
    Time,
    Date,
    GYearMonth,
    GYear,
    GMonthDay,
    GDay,
    GMonth,
    HexBinary,
    Base64Binary,
}

/// `(type, local name, base type)`.
const BUILTINS: &[(BuiltinType, &str, Option<BuiltinType>)] = {
    use BuiltinType::*;
    &[
        (AnySimpleType, "anySimpleType", None),
        (String, "string", Some(AnySimpleType)),
        (NormalizedString, "normalizedString", Some(String)),
        (Token, "token", Some(NormalizedString)),
        (Language, "language", Some(Token)),
        (Name, "Name", Some(Token)),
        (NcName, "NCName", Some(Name)),
        (Id, "ID", Some(NcName)),
        (IdRef, "IDREF", Some(NcName)),
        (IdRefs, "IDREFS", Some(AnySimpleType)),
        (Entity, "ENTITY", Some(NcName)),
        (Entities, "ENTITIES", Some(AnySimpleType)),
        (NmToken, "NMTOKEN", Some(Token)),
        (NmTokens, "NMTOKENS", Some(AnySimpleType)),
        (QName, "QName", Some(AnySimpleType)),
        (Notation, "NOTATION", Some(AnySimpleType)),
        (AnyUri, "anyURI", Some(AnySimpleType)),
        (Boolean, "boolean", Some(AnySimpleType)),
        (Decimal, "decimal", Some(AnySimpleType)),
        (Integer, "integer", Some(Decimal)),
        (NonPositiveInteger, "nonPositiveInteger", Some(Integer)),
        (NegativeInteger, "negativeInteger", Some(NonPositiveInteger)),
        (Long, "long", Some(Integer)),
        (Int, "int", Some(Long)),
        (Short, "short", Some(Int)),
        (Byte, "byte", Some(Short)),
        (NonNegativeInteger, "nonNegativeInteger", Some(Integer)),
        (UnsignedLong, "unsignedLong", Some(NonNegativeInteger)),
        (UnsignedInt, "unsignedInt", Some(UnsignedLong)),
        (UnsignedShort, "unsignedShort", Some(UnsignedInt)),
        (UnsignedByte, "unsignedByte", Some(UnsignedShort)),
        (PositiveInteger, "positiveInteger", Some(NonNegativeInteger)),
        (Float, "float", Some(AnySimpleType)),
        (Double, "double", Some(AnySimpleType)),
        (Duration, "duration", Some(AnySimpleType)),
        (DateTime, "dateTime", Some(AnySimpleType)),
        (Time, "time", Some(AnySimpleType)),
        (Date, "date", Some(AnySimpleType)),
        (GYearMonth, "gYearMonth", Some(AnySimpleType)),
        (GYear, "gYear", Some(AnySimpleType)),
        (GMonthDay, "gMonthDay", Some(AnySimpleType)),
        (GDay, "gDay", Some(AnySimpleType)),
        (GMonth, "gMonth", Some(AnySimpleType)),
        (HexBinary, "hexBinary", Some(AnySimpleType)),
        (Base64Binary, "base64Binary", Some(AnySimpleType)),
    ]
};

impl BuiltinType {
    /// Built-in type named `local` in the XML Schema namespace.
    pub fn from_local_name(local: &str) -> Option<Self> {
        BUILTINS
            .iter()
            .find(|(_, name, _)| *name == local)
            .map(|(builtin, _, _)| *builtin)
    }

    /// Local name (`dateTime`).
    pub fn name(self) -> &'static str {
        BUILTINS
            .iter()
            .find(|(builtin, _, _)| *builtin == self)
            .map_or("anySimpleType", |(_, name, _)| name)
    }

    /// Base type in the built-in hierarchy (`None` for `anySimpleType`).
    pub fn base(self) -> Option<Self> {
        BUILTINS
            .iter()
            .find(|(builtin, _, _)| *builtin == self)
            .and_then(|(_, _, base)| *base)
    }

    /// Whether the type is `ancestor` or derives from it.
    pub fn derives_from(self, ancestor: Self) -> bool {
        let mut current = Some(self);
        while let Some(builtin) = current {
            if builtin == ancestor {
                return true;
            }
            current = builtin.base();
        }
        false
    }

    /// Item type of the built-in list types (`NMTOKENS`, `IDREFS`,
    /// `ENTITIES`).
    pub fn list_item(self) -> Option<Self> {
        match self {
            Self::NmTokens => Some(Self::NmToken),
            Self::IdRefs => Some(Self::IdRef),
            Self::Entities => Some(Self::Entity),
            _ => None,
        }
    }

    /// Whitespace processing of the type (`whiteSpace` facet).
    pub fn white_space(self) -> WhiteSpace {
        match self {
            Self::AnySimpleType | Self::String => WhiteSpace::Preserve,
            Self::NormalizedString => WhiteSpace::Replace,
            _ => WhiteSpace::Collapse,
        }
    }

    /// Inclusive bounds of the integer types.
    fn integer_bounds(self) -> (Option<i128>, Option<i128>) {
        match self {
            Self::NonPositiveInteger => (None, Some(0)),
            Self::NegativeInteger => (None, Some(-1)),
            Self::Long => (Some(i64::MIN.into()), Some(i64::MAX.into())),
            Self::Int => (Some(i32::MIN.into()), Some(i32::MAX.into())),
            Self::Short => (Some(i16::MIN.into()), Some(i16::MAX.into())),
            Self::Byte => (Some(i8::MIN.into()), Some(i8::MAX.into())),
            Self::NonNegativeInteger => (Some(0), None),
            Self::UnsignedLong => (Some(0), Some(u64::MAX.into())),
            Self::UnsignedInt => (Some(0), Some(u32::MAX.into())),
            Self::UnsignedShort => (Some(0), Some(u16::MAX.into())),
            Self::UnsignedByte => (Some(0), Some(u8::MAX.into())),
            Self::PositiveInteger => (Some(1), None),
            _ => (None, None),
        }
    }

    /// Checks `value`, already processed for whitespace, against the lexical
    /// space of the type and returns its value. `Err` is the reason (without
    /// the value or the type). `namespaces` resolves the prefixes of
    /// `QName`/`NOTATION` values (`""` for the default namespace); without
    /// it, prefixes are not checked.
    pub fn parse(
        self,
        value: &str,
        namespaces: Option<&PrefixResolver<'_>>,
    ) -> Result<Value, String> {
        use BuiltinType::*;
        match self {
            AnySimpleType | String | NormalizedString | Token => {
                Ok(Value::String(value.to_owned()))
            }
            Language => {
                let valid = !value.is_empty()
                    && value.split('-').enumerate().all(|(index, part)| {
                        (1..=8).contains(&part.len())
                            && part.chars().all(|character| {
                                character.is_ascii_alphabetic()
                                    || (index > 0 && character.is_ascii_digit())
                            })
                    });
                if valid {
                    Ok(Value::String(value.to_owned()))
                } else {
                    Err("expected a language tag such as 'en' or 'en-US' (letters, then groups of 1 to 8 letters or digits separated by '-')".to_owned())
                }
            }
            Name => check_string(value, is_name(value), "expected an XML name"),
            NcName | Id | IdRef | Entity => check_string(
                value,
                is_ncname(value),
                if value.contains(':') {
                    "a non-colonized name (NCName) cannot contain ':'"
                } else {
                    "expected a non-colonized XML name (NCName)"
                },
            ),
            NmToken => check_string(
                value,
                is_nmtoken(value),
                "expected a name token (letters, digits, '.', '-', '_' or ':')",
            ),
            IdRefs | Entities | NmTokens => {
                let item = self.list_item().unwrap_or(NmToken);
                let mut items = Vec::new();
                for token in value.split(' ').filter(|token| !token.is_empty()) {
                    items.push(item.parse(token, namespaces).map_err(|reason| {
                        format!("item '{}' is invalid: {reason}", quote(token))
                    })?);
                }
                if items.is_empty() {
                    return Err("at least one item is required".to_owned());
                }
                Ok(Value::List(items))
            }
            QName | Notation => parse_qname(value, namespaces),
            AnyUri => parse_any_uri(value),
            Boolean => match value {
                "true" | "1" => Ok(Value::Boolean(true)),
                "false" | "0" => Ok(Value::Boolean(false)),
                _ => Err("expected 'true', 'false', '1' or '0'".to_owned()),
            },
            Decimal => self::Decimal::parse(value)
                .map(Value::Decimal)
                .ok_or_else(|| {
                    "expected a decimal number such as '-1.23' (no exponent)".to_owned()
                }),
            Integer | NonPositiveInteger | NegativeInteger | Long | Int | Short | Byte
            | NonNegativeInteger | UnsignedLong | UnsignedInt | UnsignedShort | UnsignedByte
            | PositiveInteger => {
                let number = self::Decimal::parse_integer(value)
                    .ok_or_else(|| "expected an integer such as '-12' (digits only)".to_owned())?;
                let (minimum, maximum) = self.integer_bounds();
                if let Some(minimum) = minimum
                    && number.cmp(&self::Decimal::from_i128(minimum)) == Ordering::Less
                {
                    return Err(format!("the value must be at least {minimum}"));
                }
                if let Some(maximum) = maximum
                    && number.cmp(&self::Decimal::from_i128(maximum)) == Ordering::Greater
                {
                    return Err(format!("the value must be at most {maximum}"));
                }
                Ok(Value::Decimal(number))
            }
            Float | Double => parse_float(value, self == Float),
            Duration => self::Duration::parse(value).map(Value::Duration),
            DateTime | Time | Date | GYearMonth | GYear | GMonthDay | GDay | GMonth => {
                Temporal::parse(self, value).map(Value::Temporal)
            }
            HexBinary => parse_hex_binary(value).map(Value::Binary),
            Base64Binary => parse_base64_binary(value).map(Value::Binary),
        }
    }
}

/// Resolves a namespace prefix (`""` for the default namespace) in the
/// context of a value: `Some(namespace)` when it is bound.
pub type PrefixResolver<'r> = dyn Fn(&str) -> Option<String> + 'r;

fn check_string(value: &str, valid: bool, reason: &str) -> Result<Value, String> {
    if valid {
        Ok(Value::String(value.to_owned()))
    } else {
        Err(reason.to_owned())
    }
}

/// Whitespace processing (`whiteSpace` facet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WhiteSpace {
    Preserve,
    Replace,
    Collapse,
}

impl WhiteSpace {
    /// Value of a `whiteSpace` facet.
    pub fn from_facet(value: &str) -> Option<Self> {
        match value.trim() {
            "preserve" => Some(Self::Preserve),
            "replace" => Some(Self::Replace),
            "collapse" => Some(Self::Collapse),
            _ => None,
        }
    }

    /// Applies the processing: `replace` turns tabs and line breaks into
    /// spaces, `collapse` also trims and merges runs of spaces.
    pub fn apply(self, value: &str) -> Cow<'_, str> {
        let is_space = |character: char| matches!(character, ' ' | '\t' | '\n' | '\r');
        match self {
            Self::Preserve => Cow::Borrowed(value),
            Self::Replace => {
                if value.contains(['\t', '\n', '\r']) {
                    Cow::Owned(value.replace(['\t', '\n', '\r'], " "))
                } else {
                    Cow::Borrowed(value)
                }
            }
            Self::Collapse => {
                let collapsed = value
                    .split(is_space)
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                if collapsed == value {
                    Cow::Borrowed(value)
                } else {
                    Cow::Owned(collapsed)
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Values
// ---------------------------------------------------------------------------

/// Value of a simple type, in the value space of its primitive type.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `anySimpleType`, `string` and the types derived from it, `anyURI`.
    String(String),
    Boolean(bool),
    /// `decimal` and the integer types.
    Decimal(Decimal),
    /// `float` (rounded to single precision) and `double`.
    Float(f64),
    Duration(Duration),
    /// `dateTime`, `time`, `date` and the `g*` types.
    Temporal(Temporal),
    /// `hexBinary` and `base64Binary`: the octets.
    Binary(Vec<u8>),
    /// `QName` and `NOTATION`.
    QName {
        prefix: Option<String>,
        /// Namespace, when the prefix could be resolved.
        namespace: Option<String>,
        local: String,
    },
    /// List types.
    List(Vec<Value>),
}

impl Value {
    /// Equality in the value space (`enumeration`, `fixed`).
    pub fn equals(&self, other: &Value) -> bool {
        match (self, other) {
            (Self::Float(left), Self::Float(right)) => {
                (left.is_nan() && right.is_nan()) || left == right
            }
            (Self::Temporal(left), Self::Temporal(right)) => left.equals(right),
            (Self::Duration(left), Self::Duration(right)) => left.equals(right),
            (
                Self::QName {
                    prefix: left_prefix,
                    namespace: left_namespace,
                    local: left_local,
                },
                Self::QName {
                    prefix: right_prefix,
                    namespace: right_namespace,
                    local: right_local,
                },
            ) => {
                left_local == right_local
                    && match (left_namespace, right_namespace) {
                        (Some(left), Some(right)) => left == right,
                        _ => left_prefix == right_prefix,
                    }
            }
            (Self::List(left), Self::List(right)) => {
                left.len() == right.len()
                    && left
                        .iter()
                        .zip(right)
                        .all(|(left, right)| left.equals(right))
            }
            (left, right) => left == right,
        }
    }

    /// Order in the value space (`None` when the values are incomparable).
    pub fn compare(&self, other: &Value) -> Option<Ordering> {
        match (self, other) {
            (Self::Decimal(left), Self::Decimal(right)) => Some(left.cmp(right)),
            (Self::Float(left), Self::Float(right)) => left.partial_cmp(right),
            (Self::Temporal(left), Self::Temporal(right)) => left.compare(right),
            (Self::Duration(left), Self::Duration(right)) => left.compare(right),
            _ => None,
        }
    }
}

/// Arbitrary precision decimal number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decimal {
    negative: bool,
    /// Integer digits without leading zeros.
    integer: String,
    /// Fraction digits without trailing zeros.
    fraction: String,
}

impl Decimal {
    /// Lexical space of `xs:decimal`: `[+-]?(\d+(\.\d*)?|\.\d+)`.
    pub fn parse(value: &str) -> Option<Self> {
        let (negative, digits) = split_sign(value);
        let (integer, fraction) = match digits.split_once('.') {
            Some((integer, fraction)) => (integer, fraction),
            None => (digits, ""),
        };
        if integer.is_empty() && fraction.is_empty()
            || !integer.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        Some(Self::new(negative, integer, fraction))
    }

    /// Lexical space of `xs:integer`: `[+-]?\d+`.
    pub fn parse_integer(value: &str) -> Option<Self> {
        let (negative, digits) = split_sign(value);
        (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| Self::new(negative, digits, ""))
    }

    fn new(negative: bool, integer: &str, fraction: &str) -> Self {
        let integer = integer.trim_start_matches('0').to_owned();
        let fraction = fraction.trim_end_matches('0').to_owned();
        let zero = integer.is_empty() && fraction.is_empty();
        Self {
            negative: negative && !zero,
            integer,
            fraction,
        }
    }

    pub fn from_i128(value: i128) -> Self {
        Self::new(value < 0, &value.unsigned_abs().to_string(), "")
    }

    /// Number of significant digits (`totalDigits`).
    pub fn total_digits(&self) -> usize {
        (self.integer.len() + self.fraction.len()).max(1)
    }

    /// Number of fraction digits (`fractionDigits`).
    pub fn fraction_digits(&self) -> usize {
        self.fraction.len()
    }

    fn cmp_magnitude(&self, other: &Self) -> Ordering {
        self.integer
            .len()
            .cmp(&other.integer.len())
            .then_with(|| self.integer.cmp(&other.integer))
            .then_with(|| self.fraction.cmp(&other.fraction))
    }
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.negative, other.negative) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => self.cmp_magnitude(other),
            (true, true) => other.cmp_magnitude(self),
        }
    }
}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn split_sign(value: &str) -> (bool, &str) {
    match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    }
}

/// `float`/`double`: decimal mantissa with an optional exponent, `INF`,
/// `-INF` or `NaN`.
fn parse_float(value: &str, single: bool) -> Result<Value, String> {
    let number = match value {
        "INF" => f64::INFINITY,
        "-INF" => f64::NEG_INFINITY,
        "NaN" => f64::NAN,
        _ => {
            let (mantissa, exponent) = match value.find(['e', 'E']) {
                Some(index) => (&value[..index], Some(&value[index + 1..])),
                None => (value, None),
            };
            let exponent_valid = exponent.is_none_or(|exponent| {
                let (_, digits) = split_sign(exponent);
                !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
            });
            if Decimal::parse(mantissa).is_none() || !exponent_valid {
                return Err(
                    if value.eq_ignore_ascii_case("inf")
                        || value.eq_ignore_ascii_case("-inf")
                        || value.eq_ignore_ascii_case("nan")
                        || value == "+INF"
                    {
                        "special values are written 'INF', '-INF' and 'NaN'".to_owned()
                    } else {
                        "expected a number such as '1.5', '-2E10', 'INF' or 'NaN'".to_owned()
                    },
                );
            }
            value.parse::<f64>().map_err(|error| error.to_string())?
        }
    };
    Ok(Value::Float(if single {
        f64::from(number as f32)
    } else {
        number
    }))
}

fn parse_qname(value: &str, namespaces: Option<&PrefixResolver<'_>>) -> Result<Value, String> {
    if !is_qname(value) {
        return Err("expected a qualified name such as 'prefix:name'".to_owned());
    }
    let (prefix, local) = match value.split_once(':') {
        Some((prefix, local)) => (Some(prefix), local),
        None => (None, value),
    };
    let namespace = match namespaces {
        Some(resolve) => {
            let namespace = resolve(prefix.unwrap_or(""));
            if let Some(prefix) = prefix
                && namespace.is_none()
            {
                return Err(format!("the prefix '{prefix}' is not bound to a namespace"));
            }
            namespace
        }
        None => None,
    };
    Ok(Value::QName {
        prefix: prefix.map(str::to_owned),
        namespace,
        local: local.to_owned(),
    })
}

/// `anyURI`: an IRI reference; only unambiguous errors are reported (bad
/// `%` escapes, several fragments, invalid scheme).
fn parse_any_uri(value: &str) -> Result<Value, String> {
    let bytes = value.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'%'
            && !(bytes.get(index + 1).is_some_and(u8::is_ascii_hexdigit)
                && bytes.get(index + 2).is_some_and(u8::is_ascii_hexdigit))
        {
            return Err("'%' must be followed by two hexadecimal digits".to_owned());
        }
    }
    if value.matches('#').count() > 1 {
        return Err("a URI cannot contain more than one '#'".to_owned());
    }
    let first_delimiter = value.find(['/', '?', '#']).unwrap_or(value.len());
    if let Some(colon) = value[..first_delimiter].find(':') {
        let scheme = &value[..colon];
        let valid = scheme
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphabetic())
            && scheme.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
            });
        if !valid {
            return Err(format!("'{}' is not a valid URI scheme", quote(scheme)));
        }
    }
    Ok(Value::String(value.to_owned()))
}

fn parse_hex_binary(value: &str) -> Result<Vec<u8>, String> {
    if !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("expected hexadecimal digits (0-9, A-F)".to_owned());
    }
    if !value.len().is_multiple_of(2) {
        return Err("expected an even number of hexadecimal digits".to_owned());
    }
    Ok(value
        .as_bytes()
        .chunks(2)
        .map(|pair| (hex_digit(pair[0]) << 4) | hex_digit(pair[1]))
        .collect())
}

fn hex_digit(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => byte - b'A' + 10,
    }
}

fn base64_digit(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// `base64Binary` (RFC 2045 alphabet, groups of four characters, single
/// spaces allowed between characters, canonical padding).
fn parse_base64_binary(value: &str) -> Result<Vec<u8>, String> {
    if value.contains("  ") || value.starts_with(' ') || value.ends_with(' ') {
        return Err("characters can only be separated by single spaces".to_owned());
    }
    let characters = value
        .bytes()
        .filter(|byte| *byte != b' ')
        .collect::<Vec<_>>();
    let padding = characters
        .iter()
        .rev()
        .take_while(|byte| **byte == b'=')
        .count();
    let data = &characters[..characters.len() - padding];
    if let Some(byte) = data.iter().find(|byte| base64_digit(**byte).is_none()) {
        return Err(if *byte == b'=' {
            "'=' can only appear at the end".to_owned()
        } else {
            format!("'{}' is not a base64 character", char::from(*byte))
        });
    }
    if characters.len() % 4 != 0 {
        return Err(format!(
            "the number of base64 characters ({}) must be a multiple of 4 (use '=' padding)",
            characters.len()
        ));
    }
    if padding > 2 {
        return Err("at most two '=' can end the value".to_owned());
    }
    let last = data
        .last()
        .and_then(|byte| base64_digit(*byte))
        .unwrap_or(0);
    if (padding == 1 && last & 0b11 != 0) || (padding == 2 && last & 0b1111 != 0) {
        return Err("the last character before the padding has unused bits set".to_owned());
    }
    let mut bytes = Vec::with_capacity(data.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in data {
        buffer = (buffer << 6) | u32::from(base64_digit(*byte).unwrap_or(0));
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// Dates, times and durations
// ---------------------------------------------------------------------------

/// Value of a date/time type: position on the timeline (picoseconds, in UTC
/// when the value has a time zone) and whether it has a time zone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Temporal {
    /// `None` for years too large for the timeline.
    instant: Option<i128>,
    timezone: bool,
    lexical: String,
}

impl Temporal {
    fn parse(kind: BuiltinType, value: &str) -> Result<Self, String> {
        let format = match kind {
            BuiltinType::DateTime => "YYYY-MM-DDThh:mm:ss",
            BuiltinType::Time => "hh:mm:ss",
            BuiltinType::Date => "YYYY-MM-DD",
            BuiltinType::GYearMonth => "YYYY-MM",
            BuiltinType::GYear => "YYYY",
            BuiltinType::GMonthDay => "--MM-DD",
            BuiltinType::GDay => "---DD",
            _ => "--MM",
        };
        let mut cursor = Cursor {
            bytes: value.as_bytes(),
            position: 0,
            format,
        };
        let has_year = matches!(
            kind,
            BuiltinType::DateTime
                | BuiltinType::Date
                | BuiltinType::GYearMonth
                | BuiltinType::GYear
        );
        let year = if has_year {
            cursor.year()?
        } else {
            // A leap year, so that --02-29 is valid.
            Some(1972)
        };
        let (mut month, mut day) = (1, 1);
        match kind {
            BuiltinType::DateTime | BuiltinType::Date => {
                cursor.expect(b'-')?;
                month = cursor.month()?;
                cursor.expect(b'-')?;
                day = cursor.two_digits("day")?;
            }
            BuiltinType::GYearMonth => {
                cursor.expect(b'-')?;
                month = cursor.month()?;
            }
            BuiltinType::GMonthDay => {
                cursor.expect(b'-')?;
                cursor.expect(b'-')?;
                month = cursor.month()?;
                cursor.expect(b'-')?;
                day = cursor.two_digits("day")?;
            }
            BuiltinType::GDay => {
                cursor.expect(b'-')?;
                cursor.expect(b'-')?;
                cursor.expect(b'-')?;
                day = cursor.two_digits("day")?;
            }
            BuiltinType::GMonth => {
                cursor.expect(b'-')?;
                cursor.expect(b'-')?;
                month = cursor.month()?;
            }
            _ => {}
        }
        if day == 0 || day > 31 {
            return Err("day must be 01-31".to_owned());
        }
        let maximum = match year {
            Some(year) if has_year => days_in_month(year, month),
            _ if kind == BuiltinType::GMonthDay => days_in_month(1972, month),
            _ => 31,
        };
        if day > maximum {
            return Err(match year {
                Some(year) if has_year => format!(
                    "day {day:02} does not exist in {}-{month:02}",
                    format_year(year)
                ),
                _ => format!("day {day:02} does not exist in month {month:02}"),
            });
        }
        let mut seconds_of_day = 0i128;
        let mut fraction = 0i128;
        if matches!(kind, BuiltinType::DateTime | BuiltinType::Time) {
            if kind == BuiltinType::DateTime {
                if cursor.peek() == Some(b' ') {
                    return Err(
                        "the date and the time must be separated by 'T', not a space".to_owned(),
                    );
                }
                cursor.expect(b'T')?;
            }
            let hour = cursor.two_digits("hour")?;
            cursor.expect(b':')?;
            let minute = cursor.two_digits("minute")?;
            cursor.expect(b':')?;
            let second = cursor.two_digits("second")?;
            let (picos, fraction_zero) = cursor.fraction()?;
            if hour > 24 {
                return Err("hour must be 00-23 (or 24:00:00)".to_owned());
            }
            if minute > 59 {
                return Err("minute must be 00-59".to_owned());
            }
            if second > 59 {
                return Err("second must be 00-59".to_owned());
            }
            if hour == 24 && (minute != 0 || second != 0 || !fraction_zero) {
                return Err("hour 24 is only allowed as 24:00:00".to_owned());
            }
            seconds_of_day = i128::from(hour) * 3600 + i128::from(minute) * 60 + i128::from(second);
            fraction = picos;
        }
        let timezone = cursor.timezone()?;
        if cursor.position != cursor.bytes.len() {
            return Err(cursor.format_error());
        }
        let instant = year.and_then(|year| {
            let days = days_from_civil(year, month, day)?;
            let seconds = days
                .checked_mul(86_400)?
                .checked_add(seconds_of_day)?
                .checked_sub(i128::from(timezone.unwrap_or(0)) * 60)?;
            seconds.checked_mul(PICOS)?.checked_add(fraction)
        });
        Ok(Self {
            instant,
            timezone: timezone.is_some(),
            lexical: value.to_owned(),
        })
    }

    fn equals(&self, other: &Self) -> bool {
        match (self.instant, other.instant) {
            (Some(left), Some(right)) => self.timezone == other.timezone && left == right,
            _ => self.lexical == other.lexical,
        }
    }

    /// Partial order of XML Schema 1.0 (§3.2.7.4): a value without time
    /// zone is compared with the range of ±14 hours.
    fn compare(&self, other: &Self) -> Option<Ordering> {
        let (left, right) = (self.instant?, other.instant?);
        if self.timezone == other.timezone {
            return Some(left.cmp(&right));
        }
        let margin = 14 * 3600 * PICOS;
        // The value without time zone may be anywhere in [t - 14h, t + 14h].
        let (low, high) = if self.timezone {
            (right - margin, right + margin)
        } else {
            (left - margin, left + margin)
        };
        let fixed = if self.timezone { left } else { right };
        let ordering = if fixed < low {
            Ordering::Less
        } else if fixed > high {
            Ordering::Greater
        } else {
            return None;
        };
        Some(if self.timezone {
            ordering
        } else {
            ordering.reverse()
        })
    }
}

/// Value of `xs:duration`: months and picoseconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Duration {
    months: Option<i128>,
    picos: Option<i128>,
    lexical: String,
}

impl Duration {
    fn parse(value: &str) -> Result<Self, String> {
        let format_error = || {
            "expected the format PnYnMnDTnHnMnS, such as 'P1Y2M3DT4H5M6.7S' or '-P10D'".to_owned()
        };
        let (negative, rest) = match value.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, value),
        };
        let rest = rest.strip_prefix('P').ok_or_else(format_error)?;
        let (date, time) = match rest.split_once('T') {
            Some((date, time)) => (date, Some(time)),
            None => (rest, None),
        };
        let mut months = Some(0i128);
        let mut picos = Some(0i128);
        let mut fields = 0;
        let add = |total: &mut Option<i128>, number: &str, factor: i128| {
            *total = total.and_then(|total| {
                number
                    .parse::<i128>()
                    .ok()?
                    .checked_mul(factor)?
                    .checked_add(total)
            });
        };
        let mut remaining = date;
        for (designator, factor) in [('Y', 12), ('M', 1)] {
            if let Some((number, tail)) = duration_field(remaining, designator)? {
                add(&mut months, number, factor);
                remaining = tail;
                fields += 1;
            }
        }
        if let Some((number, tail)) = duration_field(remaining, 'D')? {
            add(&mut picos, number, 86_400 * PICOS);
            remaining = tail;
            fields += 1;
        }
        if !remaining.is_empty() {
            return Err(if remaining.contains('.') {
                "only seconds can have a fraction".to_owned()
            } else {
                format_error()
            });
        }
        if let Some(time) = time {
            let mut remaining = time;
            let before = fields;
            for (designator, factor) in [('H', 3600 * PICOS), ('M', 60 * PICOS)] {
                if let Some((number, tail)) = duration_field(remaining, designator)? {
                    add(&mut picos, number, factor);
                    remaining = tail;
                    fields += 1;
                }
            }
            if let Some(number) = remaining.strip_suffix('S') {
                let (whole, fraction) = match number.split_once('.') {
                    Some((whole, fraction)) => (whole, Some(fraction)),
                    None => (number, None),
                };
                if whole.is_empty()
                    || !whole.bytes().all(|byte| byte.is_ascii_digit())
                    || fraction.is_some_and(|fraction| {
                        fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit())
                    })
                {
                    return Err(format_error());
                }
                add(&mut picos, whole, PICOS);
                picos =
                    picos.and_then(|picos| picos.checked_add(fraction.map_or(0, fraction_picos)));
                remaining = "";
                fields += 1;
            }
            if !remaining.is_empty() {
                return Err(if remaining.contains('.') {
                    "only seconds can have a fraction".to_owned()
                } else {
                    format_error()
                });
            }
            if fields == before {
                return Err(
                    "'T' must be followed by at least one of hours, minutes or seconds".to_owned(),
                );
            }
        }
        if fields == 0 {
            return Err(
                "at least one field (years, months, days, hours, minutes or seconds) is required"
                    .to_owned(),
            );
        }
        if negative {
            months = months.map(|months| -months);
            picos = picos.map(|picos| -picos);
        }
        Ok(Self {
            months,
            picos,
            lexical: value.to_owned(),
        })
    }

    fn equals(&self, other: &Self) -> bool {
        match (self.months, self.picos, other.months, other.picos) {
            (Some(months), Some(picos), Some(other_months), Some(other_picos)) => {
                months == other_months && picos == other_picos
            }
            _ => self.lexical == other.lexical,
        }
    }

    /// Partial order of XML Schema 1.0 (§3.2.6.2): both durations are added
    /// to four reference dates; they are ordered when all agree.
    fn compare(&self, other: &Self) -> Option<Ordering> {
        const REFERENCES: [(i128, i128); 4] = [(1696, 9), (1697, 2), (1903, 3), (1903, 7)];
        let mut result = None;
        for (year, month) in REFERENCES {
            let left = add_duration(year, month, self.months?, self.picos?)?;
            let right = add_duration(year, month, other.months?, other.picos?)?;
            let ordering = left.cmp(&right);
            match result {
                None => result = Some(ordering),
                Some(previous) if previous != ordering => return None,
                Some(_) => {}
            }
        }
        result
    }
}

/// Number and remainder of a duration field ending with `designator`.
fn duration_field(text: &str, designator: char) -> Result<Option<(&str, &str)>, String> {
    let digits = text
        .bytes()
        .take_while(|byte| byte.is_ascii_digit() || *byte == b'.')
        .count();
    if text[digits..].starts_with(designator) {
        let number = &text[..digits];
        if number.is_empty() {
            return Err(format!("'{designator}' must follow a number"));
        }
        if number.contains('.') {
            return Err("only seconds can have a fraction".to_owned());
        }
        return Ok(Some((number, &text[digits + designator.len_utf8()..])));
    }
    Ok(None)
}

/// Picoseconds of a fraction of second (digits after the dot).
fn fraction_picos(digits: &str) -> i128 {
    let mut padded = digits.chars().take(12).collect::<String>();
    while padded.len() < 12 {
        padded.push('0');
    }
    padded.parse().unwrap_or(0)
}

/// Picoseconds of `year-month-01T00:00:00Z` plus a duration.
fn add_duration(year: i128, month: i128, months: i128, picos: i128) -> Option<i128> {
    let total = year
        .checked_mul(12)?
        .checked_add(month - 1)?
        .checked_add(months)?;
    let days = days_from_civil(total.div_euclid(12), (total.rem_euclid(12) + 1) as u8, 1)?;
    days.checked_mul(86_400 * PICOS)?.checked_add(picos)
}

/// Days since 1970-01-01 of a date of the proleptic Gregorian calendar
/// (XML Schema 1.0 years: no year 0, -0001 is 1 BCE).
fn days_from_civil(year: i128, month: u8, day: u8) -> Option<i128> {
    if year.abs() > MAX_TIMELINE_YEAR {
        return None;
    }
    let year = if year < 0 { year + 1 } else { year };
    let month = i128::from(month);
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + i128::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

fn days_in_month(year: i128, month: u8) -> u8 {
    match month {
        2 => {
            let year = if year < 0 { year + 1 } else { year };
            let leap =
                year.rem_euclid(4) == 0 && (year.rem_euclid(100) != 0 || year.rem_euclid(400) == 0);
            if leap { 29 } else { 28 }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

fn format_year(year: i128) -> String {
    if year < 0 {
        format!("-{:04}", -year)
    } else {
        format!("{year:04}")
    }
}

/// Reader of the date/time lexical forms.
struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
    format: &'static str,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn format_error(&self) -> String {
        format!(
            "expected the format {} with an optional time zone",
            self.format
        )
    }

    fn expect(&mut self, expected: u8) -> Result<(), String> {
        if self.peek() == Some(expected) {
            self.position += 1;
            Ok(())
        } else {
            Err(self.format_error())
        }
    }

    fn digits(&mut self) -> &str {
        let start = self.position;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.position += 1;
        }
        std::str::from_utf8(&self.bytes[start..self.position]).unwrap_or_default()
    }

    fn two_digits(&mut self, field: &str) -> Result<u8, String> {
        let digits = self.digits();
        if digits.len() != 2 {
            return Err(if digits.is_empty() {
                self.format_error()
            } else {
                format!("{field} must have exactly two digits")
            });
        }
        Ok(digits.parse().unwrap_or(0))
    }

    fn month(&mut self) -> Result<u8, String> {
        let month = self.two_digits("month")?;
        if !(1..=12).contains(&month) {
            return Err("month must be 01-12".to_owned());
        }
        Ok(month)
    }

    /// `-?YYYY`: at least four digits, no leading zero beyond four, no
    /// year 0000. `None` for years too large to compute with.
    fn year(&mut self) -> Result<Option<i128>, String> {
        let negative = self.peek() == Some(b'-');
        if negative {
            self.position += 1;
        }
        let digits = self.digits().to_owned();
        if digits.len() < 4 {
            return Err(if digits.is_empty() {
                self.format_error()
            } else {
                "year must have at least four digits".to_owned()
            });
        }
        if digits.len() > 4 && digits.starts_with('0') {
            return Err("a year with more than four digits cannot start with 0".to_owned());
        }
        if digits.bytes().all(|byte| byte == b'0') {
            return Err("year 0000 is not allowed".to_owned());
        }
        Ok(digits
            .parse::<i128>()
            .ok()
            .map(|year| if negative { -year } else { year }))
    }

    /// Optional `.s+` after the seconds: picoseconds and whether all
    /// digits are zero.
    fn fraction(&mut self) -> Result<(i128, bool), String> {
        if self.peek() != Some(b'.') {
            return Ok((0, true));
        }
        self.position += 1;
        let digits = self.digits().to_owned();
        if digits.is_empty() {
            return Err("a '.' in the seconds must be followed by digits".to_owned());
        }
        Ok((
            fraction_picos(&digits),
            digits.bytes().all(|byte| byte == b'0'),
        ))
    }

    /// Optional time zone: `Z` or `±hh:mm` (at most 14:00); minutes east of
    /// UTC.
    fn timezone(&mut self) -> Result<Option<i32>, String> {
        match self.peek() {
            Some(b'Z') => {
                self.position += 1;
                Ok(Some(0))
            }
            Some(sign @ (b'+' | b'-')) => {
                self.position += 1;
                let hours = self.two_digits("time zone hour")?;
                self.expect(b':')?;
                let minutes = self.two_digits("time zone minute")?;
                if minutes > 59 {
                    return Err("time zone minutes must be 00-59".to_owned());
                }
                if hours > 14 || (hours == 14 && minutes != 0) {
                    return Err("time zone must be between -14:00 and +14:00".to_owned());
                }
                let offset = i32::from(hours) * 60 + i32::from(minutes);
                Ok(Some(if sign == b'-' { -offset } else { offset }))
            }
            _ => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// Simple types and facets
// ---------------------------------------------------------------------------

/// Simple type: variety and facets of each restriction step.
#[derive(Debug, Clone)]
pub struct SimpleType<'a> {
    /// Name used in messages (`xs:int`, `tns:Price`, `anonymous type`).
    pub name: String,
    pub variety: Variety<'a>,
    /// Facets of each restriction step, the most derived first.
    pub facets: Vec<&'a XsdFacets>,
}

/// Variety of a simple type.
#[derive(Debug, Clone)]
pub enum Variety<'a> {
    Atomic(BuiltinType),
    List(Box<SimpleType<'a>>),
    Union(Vec<SimpleType<'a>>),
}

/// Why a value is invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueError {
    pub message: String,
    /// The value is well-formed but outside an `enumeration`.
    pub enumeration: bool,
}

impl ValueError {
    fn new(message: String) -> Self {
        Self {
            message,
            enumeration: false,
        }
    }
}

impl<'a> SimpleType<'a> {
    /// Built-in type. The built-in list types (`NMTOKENS`, `IDREFS`,
    /// `ENTITIES`) stay atomic here: [`BuiltinType::parse`] splits them into
    /// a [`Value::List`] of at least one item.
    pub fn builtin(builtin: BuiltinType) -> Self {
        Self {
            name: format!("xs:{}", builtin.name()),
            variety: Variety::Atomic(builtin),
            facets: Vec::new(),
        }
    }

    /// Effective whitespace processing of an atomic or list type.
    pub fn white_space(&self) -> WhiteSpace {
        match &self.variety {
            Variety::Atomic(builtin) => self
                .facets
                .iter()
                .find_map(|facets| {
                    facets
                        .white_space
                        .as_deref()
                        .and_then(WhiteSpace::from_facet)
                })
                .unwrap_or_else(|| builtin.white_space()),
            Variety::List(_) => WhiteSpace::Collapse,
            Variety::Union(_) => WhiteSpace::Preserve,
        }
    }

    /// Checks `raw` (the value as written, before whitespace processing)
    /// against the type and all its facets.
    pub fn validate(
        &self,
        raw: &str,
        namespaces: Option<&PrefixResolver<'_>>,
    ) -> Result<Value, ValueError> {
        self.validate_normalized(raw, namespaces)
            .map(|(value, _)| value)
    }

    /// Whether two values of the type are equal in its value space (`fixed`
    /// values); lexical comparison after whitespace processing when one of
    /// them is invalid.
    pub fn values_equal(
        &self,
        left: &str,
        right: &str,
        namespaces: Option<&PrefixResolver<'_>>,
    ) -> bool {
        match (
            self.validate_normalized(left, namespaces),
            self.validate_normalized(right, None),
        ) {
            (Ok((left, _)), Ok((right, _))) => left.equals(&right),
            _ => {
                let white_space = self.white_space();
                white_space.apply(left) == white_space.apply(right)
            }
        }
    }

    fn validate_normalized(
        &self,
        raw: &str,
        namespaces: Option<&PrefixResolver<'_>>,
    ) -> Result<(Value, String), ValueError> {
        match &self.variety {
            Variety::Atomic(builtin) => {
                let normalized = self.white_space().apply(raw).into_owned();
                let value = builtin.parse(&normalized, namespaces).map_err(|reason| {
                    ValueError::new(format!(
                        "'{}' is not a valid {}: {reason}",
                        quote(&normalized),
                        self.type_description(*builtin)
                    ))
                })?;
                self.check_facets(&normalized, &value)?;
                Ok((value, normalized))
            }
            Variety::List(item) => {
                let normalized = WhiteSpace::Collapse.apply(raw).into_owned();
                let mut items = Vec::new();
                for (index, token) in normalized
                    .split(' ')
                    .filter(|token| !token.is_empty())
                    .enumerate()
                {
                    // Reported as invalid content: the enumeration of an
                    // item type is not the enumeration of the list.
                    let value = item.validate(token, namespaces).map_err(|error| {
                        ValueError::new(format!(
                            "{} (item {} of the list {})",
                            error.message,
                            index + 1,
                            self.name
                        ))
                    })?;
                    items.push(value);
                }
                let value = Value::List(items);
                self.check_facets(&normalized, &value)?;
                Ok((value, normalized))
            }
            Variety::Union(members) => {
                let found = members
                    .iter()
                    .find_map(|member| member.validate_normalized(raw, namespaces).ok());
                let Some((value, normalized)) = found else {
                    let names = members
                        .iter()
                        .map(|member| member.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(ValueError::new(format!(
                        "'{}' is not valid for any member type of {} ({names})",
                        quote(WhiteSpace::Collapse.apply(raw).as_ref()),
                        self.name
                    )));
                };
                self.check_facets(&normalized, &value)?;
                Ok((value, normalized))
            }
        }
    }

    /// `xs:date`, or `tns:Birthday (derived from xs:date)`.
    fn type_description(&self, builtin: BuiltinType) -> String {
        let builtin_name = format!("xs:{}", builtin.name());
        if self.name == builtin_name {
            builtin_name
        } else {
            format!("{} (derived from {builtin_name})", self.name)
        }
    }

    fn check_facets(&self, normalized: &str, value: &Value) -> Result<(), ValueError> {
        let quoted = quote(normalized);
        for facets in &self.facets {
            self.check_length(facets, &quoted, normalized, value)?;
            if !facets.patterns.is_empty() {
                let results = facets
                    .patterns
                    .iter()
                    .filter_map(|pattern| pattern::is_match(pattern, normalized))
                    .collect::<Vec<_>>();
                if !results.is_empty() && !results.contains(&true) {
                    let patterns = facets
                        .patterns
                        .iter()
                        .map(|pattern| format!("'{pattern}'"))
                        .collect::<Vec<_>>()
                        .join(" or ");
                    return Err(ValueError::new(format!(
                        "'{quoted}' does not match the pattern {patterns} of {}",
                        self.name
                    )));
                }
            }
            if !facets.enumerations.is_empty()
                && !facets.enumerations.iter().any(|enumeration| {
                    self.enumeration_matches(&enumeration.value, normalized, value)
                })
            {
                let mut listed = facets
                    .enumerations
                    .iter()
                    .take(MAX_LISTED_VALUES)
                    .map(|enumeration| format!("'{}'", enumeration.value))
                    .collect::<Vec<_>>()
                    .join(", ");
                if facets.enumerations.len() > MAX_LISTED_VALUES {
                    listed.push_str(", …");
                }
                return Err(ValueError {
                    message: format!(
                        "'{quoted}' is not one of the values allowed by {}: {listed}",
                        self.name
                    ),
                    enumeration: true,
                });
            }
            self.check_bounds(facets, &quoted, value)?;
            if let Value::Decimal(number) = value {
                if let Some(limit) = facet_count(&facets.total_digits)
                    && number.total_digits() > limit
                {
                    return Err(ValueError::new(format!(
                        "'{quoted}' has {} digits, more than the {limit} allowed by {} (totalDigits)",
                        number.total_digits(),
                        self.name
                    )));
                }
                if let Some(limit) = facet_count(&facets.fraction_digits)
                    && number.fraction_digits() > limit
                {
                    return Err(ValueError::new(format!(
                        "'{quoted}' has {} fraction digits, more than the {limit} allowed by {} (fractionDigits)",
                        number.fraction_digits(),
                        self.name
                    )));
                }
            }
        }
        Ok(())
    }

    fn check_length(
        &self,
        facets: &XsdFacets,
        quoted: &str,
        normalized: &str,
        value: &Value,
    ) -> Result<(), ValueError> {
        let (length, unit) = match (&self.variety, value) {
            (_, Value::List(items)) => (items.len(), "items"),
            (Variety::Atomic(builtin), Value::Binary(bytes))
                if !builtin.derives_from(BuiltinType::String) =>
            {
                (bytes.len(), "octets")
            }
            (Variety::Atomic(BuiltinType::QName | BuiltinType::Notation), _) => return Ok(()),
            (Variety::Atomic(builtin), _)
                if builtin.derives_from(BuiltinType::String)
                    || *builtin == BuiltinType::AnyUri
                    || *builtin == BuiltinType::AnySimpleType =>
            {
                (normalized.chars().count(), "characters")
            }
            _ => return Ok(()),
        };
        let checks = [
            (&facets.length, "exactly", "length"),
            (&facets.min_length, "at least", "minLength"),
            (&facets.max_length, "at most", "maxLength"),
        ];
        for (facet, relation, name) in checks {
            let Some(limit) = facet_count(facet) else {
                continue;
            };
            let valid = match name {
                "length" => length == limit,
                "minLength" => length >= limit,
                _ => length <= limit,
            };
            if !valid {
                return Err(ValueError::new(format!(
                    "'{quoted}' has {length} {unit}; {} requires {relation} {limit} ({name})",
                    self.name
                )));
            }
        }
        Ok(())
    }

    fn check_bounds(
        &self,
        facets: &XsdFacets,
        quoted: &str,
        value: &Value,
    ) -> Result<(), ValueError> {
        let checks = [
            (&facets.min_inclusive, "minInclusive", "at least"),
            (&facets.min_exclusive, "minExclusive", "greater than"),
            (&facets.max_inclusive, "maxInclusive", "at most"),
            (&facets.max_exclusive, "maxExclusive", "less than"),
        ];
        for (facet, name, relation) in checks {
            let Some(limit) = facet else {
                continue;
            };
            let Some(limit_value) = self.facet_value(limit) else {
                continue;
            };
            let valid = match (value.compare(&limit_value), name) {
                (Some(ordering), "minInclusive") => ordering != Ordering::Less,
                (Some(ordering), "minExclusive") => ordering == Ordering::Greater,
                (Some(ordering), "maxInclusive") => ordering != Ordering::Greater,
                (Some(ordering), _) => ordering == Ordering::Less,
                (None, _) => false,
            };
            if !valid {
                return Err(ValueError::new(format!(
                    "'{quoted}' must be {relation} {} ({name} of {})",
                    limit.trim(),
                    self.name
                )));
            }
        }
        Ok(())
    }

    /// Value of a bound facet (parsed with the primitive type).
    fn facet_value(&self, literal: &str) -> Option<Value> {
        let Variety::Atomic(builtin) = self.variety else {
            return None;
        };
        let parser = if builtin.derives_from(BuiltinType::Decimal) {
            BuiltinType::Decimal
        } else {
            builtin
        };
        parser
            .parse(WhiteSpace::Collapse.apply(literal).as_ref(), None)
            .ok()
    }

    fn enumeration_matches(&self, literal: &str, normalized: &str, value: &Value) -> bool {
        let literal = match &self.variety {
            Variety::Atomic(_) => self.white_space().apply(literal),
            Variety::List(_) => WhiteSpace::Collapse.apply(literal),
            Variety::Union(_) => Cow::Borrowed(literal),
        };
        if literal == normalized {
            return true;
        }
        let literal_value = match &self.variety {
            Variety::Atomic(builtin) => builtin.parse(&literal, None).ok(),
            Variety::List(item) => literal
                .split(' ')
                .filter(|token| !token.is_empty())
                .map(|token| item.validate(token, None).ok())
                .collect::<Option<Vec<_>>>()
                .map(Value::List),
            Variety::Union(members) => members
                .iter()
                .find_map(|member| member.validate(&literal, None).ok()),
        };
        match (literal_value, value) {
            // Prefixes of enumeration values are those of the schema: compare
            // the lexical forms.
            (Some(Value::QName { .. }), Value::QName { .. }) => false,
            (Some(literal_value), value) => literal_value.equals(value),
            (None, _) => false,
        }
    }
}

fn facet_count(facet: &Option<String>) -> Option<usize> {
    facet.as_deref().and_then(|value| value.trim().parse().ok())
}

/// Value quoted by a message, shortened when long.
fn quote(value: &str) -> String {
    if value.chars().count() <= MAX_QUOTED_CHARS {
        return value.to_owned();
    }
    let mut shortened = value.chars().take(MAX_QUOTED_CHARS).collect::<String>();
    shortened.push('…');
    shortened
}

// ---------------------------------------------------------------------------
// Component model
// ---------------------------------------------------------------------------

impl XsdModelSet {
    /// Simple type of a type definition: a simple type, or the simple
    /// content of a complex type. `None` for complex types with element or
    /// mixed content, `xs:anyType` and unresolved types.
    pub fn simple_type<'a>(&'a self, reference: XsdTypeRef<'a>) -> Option<SimpleType<'a>> {
        self.simple_type_at_depth(reference, 0)
    }

    fn simple_type_at_depth<'a>(
        &'a self,
        reference: XsdTypeRef<'a>,
        depth: usize,
    ) -> Option<SimpleType<'a>> {
        if depth > MAX_DEPTH {
            return None;
        }
        let name = reference
            .name
            .map(XsdQName::display)
            .unwrap_or_else(|| "anonymous type".to_owned());
        let mut facets = Vec::new();
        let mut current = reference;
        for _ in 0..MAX_DEPTH {
            let Some(definition) = current.definition else {
                let qname = current.name?;
                if !qname.is_builtin() {
                    return None;
                }
                let builtin = BuiltinType::from_local_name(&qname.local)?;
                return Some(SimpleType {
                    name,
                    variety: Variety::Atomic(builtin),
                    facets,
                });
            };
            if definition.complex && !definition.simple_content {
                return None;
            }
            facets.push(&definition.facets);
            match definition.derivation {
                Some(XsdDerivation::List) => {
                    let item = match &definition.item_type {
                        Some(item) => self.resolve_type(current.schema, item),
                        None => XsdTypeRef {
                            schema: current.schema,
                            name: None,
                            definition: definition.inline_types.first(),
                        },
                    };
                    let item = self.simple_type_at_depth(item, depth + 1)?;
                    return Some(SimpleType {
                        name,
                        variety: Variety::List(Box::new(item)),
                        facets,
                    });
                }
                Some(XsdDerivation::Union) => {
                    let members = definition
                        .member_types
                        .iter()
                        .map(|member| self.resolve_type(current.schema, member))
                        .chain(definition.inline_types.iter().map(|inline| XsdTypeRef {
                            schema: current.schema,
                            name: None,
                            definition: Some(inline),
                        }))
                        .map(|member| self.simple_type_at_depth(member, depth + 1))
                        .collect::<Option<Vec<_>>>()?;
                    return Some(SimpleType {
                        name,
                        variety: Variety::Union(members),
                        facets,
                    });
                }
                _ => {}
            }
            current = match self.base_type(current) {
                Some(base) => base,
                // A restriction without base: `anySimpleType`.
                None => {
                    return Some(SimpleType {
                        name,
                        variety: Variety::Atomic(BuiltinType::AnySimpleType),
                        facets,
                    });
                }
            };
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::model::{XsdEnumeration, parse_xsd_model};

    fn check(builtin: &str, value: &str) -> Result<Value, String> {
        let builtin = BuiltinType::from_local_name(builtin).expect("built-in type");
        SimpleType::builtin(builtin)
            .validate(value, None)
            .map_err(|error| error.message)
    }

    fn valid(builtin: &str, value: &str) -> bool {
        check(builtin, value).is_ok()
    }

    fn assert_valid(builtin: &str, values: &[&str]) {
        for value in values {
            assert!(
                valid(builtin, value),
                "'{value}' should be a valid xs:{builtin}: {:?}",
                check(builtin, value)
            );
        }
    }

    fn assert_invalid(builtin: &str, values: &[&str]) {
        for value in values {
            assert!(
                !valid(builtin, value),
                "'{value}' should not be a valid xs:{builtin}"
            );
        }
    }

    #[test]
    fn knows_the_builtin_hierarchy() {
        assert_eq!(
            BuiltinType::from_local_name("unsignedByte"),
            Some(BuiltinType::UnsignedByte)
        );
        assert_eq!(BuiltinType::from_local_name("anyType"), None);
        assert_eq!(BuiltinType::UnsignedByte.name(), "unsignedByte");
        assert!(BuiltinType::UnsignedByte.derives_from(BuiltinType::Decimal));
        assert!(BuiltinType::Id.derives_from(BuiltinType::String));
        assert!(!BuiltinType::Float.derives_from(BuiltinType::Decimal));
        assert_eq!(
            BuiltinType::NmTokens.list_item(),
            Some(BuiltinType::NmToken)
        );
        assert_eq!(BuiltinType::String.white_space(), WhiteSpace::Preserve);
        assert_eq!(
            BuiltinType::NormalizedString.white_space(),
            WhiteSpace::Replace
        );
        assert_eq!(BuiltinType::Token.white_space(), WhiteSpace::Collapse);
        assert_eq!(BuiltinType::Int.white_space(), WhiteSpace::Collapse);
    }

    #[test]
    fn processes_whitespace() {
        assert_eq!(WhiteSpace::Preserve.apply(" a\tb "), " a\tb ");
        assert_eq!(WhiteSpace::Replace.apply(" a\tb\r\n"), " a b  ");
        assert_eq!(WhiteSpace::Collapse.apply("\n a \t b \r\n"), "a b");
        assert_eq!(
            WhiteSpace::from_facet(" collapse "),
            Some(WhiteSpace::Collapse)
        );
        assert_eq!(WhiteSpace::from_facet("trim"), None);
        assert_valid("int", &[" 42\n", "\t-7 "]);
        assert_valid("boolean", &[" true\n"]);
    }

    #[test]
    fn validates_string_types() {
        assert_valid("string", &["", " any\ttext ", "été"]);
        assert_valid("normalizedString", &["a\tb"]);
        assert_valid("token", &["  a   b  "]);
        assert_valid(
            "language",
            &["en", "en-US", "x-klingon", "zh-Hant-TW", "i-default"],
        );
        assert_invalid(
            "language",
            &[
                "",
                "en_US",
                "englishlanguage",
                "en-",
                "-en",
                "1en",
                "en--US",
            ],
        );
        assert_valid("Name", &["a", "_a", "a:b", "é-1", ":a"]);
        assert_invalid("Name", &["1a", "-a", "a b", ""]);
        assert_valid("NCName", &["a", "a-b.c_d", "é"]);
        assert_invalid("NCName", &["a:b", "1a", ""]);
        assert_valid("ID", &["id1"]);
        assert_invalid("ID", &["1"]);
        assert_valid("IDREF", &["ref"]);
        assert_valid("ENTITY", &["logo"]);
        assert_valid("NMTOKEN", &["1a", "-.", "a:b"]);
        assert_invalid("NMTOKEN", &["a b", "", "a,b"]);
        assert_valid("NMTOKENS", &["a b  c", " 1 "]);
        assert_invalid("NMTOKENS", &["", "   ", "a ,"]);
        assert_valid("IDREFS", &["a b"]);
        assert_invalid("IDREFS", &["a 1"]);
        assert_valid("ENTITIES", &["a b"]);
        assert_invalid("ENTITIES", &["a:b"]);
        assert!(check("NCName", "a:b").unwrap_err().contains(
            "'a:b' is not a valid xs:NCName: a non-colonized name (NCName) cannot contain ':'"
        ));
    }

    #[test]
    fn validates_uris_qnames_and_notations() {
        assert_valid(
            "anyURI",
            &[
                "",
                "http://example.com/a?b=c#d",
                "../relative/path",
                "urn:isbn:0451450523",
                "%20",
                "mailto:a@b.c",
                "a b",
            ],
        );
        assert_invalid(
            "anyURI",
            &["http://a#b#c", "%zz", "%2", "1http:x", ":no-scheme"],
        );
        assert_valid("QName", &["a", "p:a"]);
        assert_invalid("QName", &["p:a:b", ":a", "a:", "1a", ""]);
        assert_valid("NOTATION", &["png"]);
        let namespaces = |prefix: &str| (prefix == "p").then(|| "urn:p".to_owned());
        let qname = SimpleType::builtin(BuiltinType::QName);
        assert_eq!(
            qname.validate("p:a", Some(&namespaces)),
            Ok(Value::QName {
                prefix: Some("p".to_owned()),
                namespace: Some("urn:p".to_owned()),
                local: "a".to_owned(),
            })
        );
        let error = qname.validate("q:a", Some(&namespaces)).unwrap_err();
        assert!(
            error
                .message
                .contains("the prefix 'q' is not bound to a namespace")
        );
        assert!(qname.validate("a", Some(&namespaces)).is_ok());
    }

    #[test]
    fn validates_booleans() {
        assert_valid("boolean", &["true", "false", "1", "0"]);
        assert_invalid("boolean", &["TRUE", "yes", "", "2", "t"]);
        assert_eq!(check("boolean", "1"), check("boolean", "true"));
    }

    #[test]
    fn validates_decimals() {
        assert_valid(
            "decimal",
            &[
                "0",
                "-1.23",
                "+100000.00",
                "210",
                ".5",
                "5.",
                "-.0",
                "000123.4500",
            ],
        );
        assert_invalid(
            "decimal",
            &["", ".", "-", "1e3", "1,5", "1.2.3", "INF", "+-1", " 1 2"],
        );
        let one = Decimal::parse("1.0").unwrap();
        assert_eq!(one, Decimal::parse("+001").unwrap());
        assert_eq!(
            Decimal::parse("-0").unwrap(),
            Decimal::parse("0.000").unwrap()
        );
        assert!(Decimal::parse("-2").unwrap() < Decimal::parse("-1.5").unwrap());
        assert!(Decimal::parse("10").unwrap() > Decimal::parse("9.99").unwrap());
        assert!(Decimal::parse("0.1").unwrap() > Decimal::parse("0.09").unwrap());
        assert_eq!(Decimal::parse("012.3400").unwrap().total_digits(), 4);
        assert_eq!(Decimal::parse("012.3400").unwrap().fraction_digits(), 2);
        assert_eq!(Decimal::parse("0").unwrap().total_digits(), 1);
        assert_eq!(Decimal::parse("0.05").unwrap().total_digits(), 2);
    }

    #[test]
    fn validates_integer_types_and_their_ranges() {
        assert_valid(
            "integer",
            &["0", "-0", "+12", "123456789012345678901234567890"],
        );
        assert_invalid("integer", &["1.0", "", "1e2", "- 1", "0x1"]);
        assert_valid("int", &["2147483647", "-2147483648"]);
        assert_invalid("int", &["2147483648", "-2147483649"]);
        assert_valid("long", &["9223372036854775807", "-9223372036854775808"]);
        assert_invalid("long", &["9223372036854775808"]);
        assert_valid("short", &["32767", "-32768"]);
        assert_invalid("short", &["32768"]);
        assert_valid("byte", &["127", "-128", "+0"]);
        assert_invalid("byte", &["128", "-129"]);
        assert_valid("unsignedLong", &["18446744073709551615", "0"]);
        assert_invalid("unsignedLong", &["18446744073709551616", "-1"]);
        assert_valid("unsignedInt", &["4294967295"]);
        assert_invalid("unsignedInt", &["4294967296"]);
        assert_valid("unsignedShort", &["65535"]);
        assert_invalid("unsignedShort", &["65536"]);
        assert_valid("unsignedByte", &["255", "-0"]);
        assert_invalid("unsignedByte", &["256", "-1"]);
        assert_valid("positiveInteger", &["1", "+99999999999999999999"]);
        assert_invalid("positiveInteger", &["0", "-1"]);
        assert_valid("nonNegativeInteger", &["0", "-0"]);
        assert_invalid("nonNegativeInteger", &["-1"]);
        assert_valid("nonPositiveInteger", &["0", "-5"]);
        assert_invalid("nonPositiveInteger", &["1"]);
        assert_valid("negativeInteger", &["-1"]);
        assert_invalid("negativeInteger", &["0", "-0"]);
        assert!(
            check("int", "2147483648")
                .unwrap_err()
                .contains("the value must be at most 2147483647")
        );
    }

    #[test]
    fn validates_floats_and_doubles() {
        assert_valid(
            "float",
            &[
                "1.5E-10", "-INF", "INF", "NaN", "-0", "12", ".5", "5.", "1e+3", "3.4e38", "1E400",
            ],
        );
        assert_invalid(
            "float",
            &[
                "inf", "+INF", "nan", "1.2.3", "E5", "1e", "1e1.5", "", "0x1p3",
            ],
        );
        assert_valid("double", &["6.02e23", "-1E-400"]);
        assert_invalid("double", &["Infinity", "1,5"]);
        assert!(
            check("float", "inf")
                .unwrap_err()
                .contains("'INF', '-INF' and 'NaN'")
        );
        assert_eq!(check("float", "0.1"), Ok(Value::Float(f64::from(0.1f32))));
        let nan = check("double", "NaN").unwrap();
        assert!(nan.equals(&nan));
        assert_eq!(nan.compare(&Value::Float(1.0)), None);
        assert!(
            check("double", "0")
                .unwrap()
                .equals(&check("double", "-0").unwrap())
        );
    }

    #[test]
    fn validates_dates_and_times() {
        assert_valid(
            "dateTime",
            &[
                "2026-09-29T21:38:46Z",
                "2026-09-29T21:38:46.123-05:00",
                "2026-09-29T24:00:00",
                "-0044-03-15T12:00:00",
                "12026-01-01T00:00:00+14:00",
                "2024-02-29T00:00:00",
            ],
        );
        assert_invalid(
            "dateTime",
            &[
                "2026-09-29 21:38:46",
                "2026-09-29T21:38",
                "2026-09-29T24:00:01",
                "2026-09-29T21:60:00",
                "2026-09-29T21:38:60",
                "2026-09-29T21:38:46.",
                "2026-09-29T21:38:46+15:00",
                "2026-09-29T21:38:46+14:30",
                "2026-09-29T21:38:46+05",
                "0000-01-01T00:00:00",
                "02026-01-01T00:00:00",
                "+2026-01-01T00:00:00",
                "2023-02-29T00:00:00",
            ],
        );
        assert_valid(
            "date",
            &[
                "2026-09-29",
                "2026-09-29+02:00",
                "2024-02-29",
                "2000-02-29",
                "-0001-02-29",
            ],
        );
        assert_invalid(
            "date",
            &[
                "2026-02-30",
                "2026-9-29",
                "2100-02-29",
                "2026-13-01",
                "2026-00-10",
                "2026-04-31",
            ],
        );
        assert_valid("time", &["13:20:00", "00:00:00.5Z", "24:00:00"]);
        assert_invalid("time", &["25:00:00", "13:20", "1:20:00", "13:20:00Z+01:00"]);
        assert_valid("gYear", &["2026", "-0001", "12345", "2026Z"]);
        assert_invalid("gYear", &["26", "0000", "2026-01"]);
        assert_valid("gYearMonth", &["2026-09", "2026-12Z"]);
        assert_invalid("gYearMonth", &["2026-13", "2026"]);
        assert_valid("gMonthDay", &["--09-29", "--02-29", "--12-31Z"]);
        assert_invalid("gMonthDay", &["--02-30", "--04-31", "09-29", "--13-01"]);
        assert_valid("gDay", &["---29", "---01", "---31+01:00"]);
        assert_invalid("gDay", &["---32", "---00", "--29"]);
        assert_valid("gMonth", &["--09", "--12Z"]);
        assert_invalid("gMonth", &["--13", "--00", "--9"]);
        assert!(
            check("date", "2024-13-01")
                .unwrap_err()
                .contains("'2024-13-01' is not a valid xs:date: month must be 01-12")
        );
        assert!(
            check("date", "2026-02-30")
                .unwrap_err()
                .contains("day 30 does not exist in 2026-02")
        );
    }

    #[test]
    fn compares_dates_on_the_timeline() {
        let date_time = |value| check("dateTime", value).unwrap();
        assert!(date_time("2026-01-01T00:00:00Z").equals(&date_time("2026-01-01T01:00:00+01:00")));
        assert!(date_time("2026-01-01T24:00:00Z").equals(&date_time("2026-01-02T00:00:00Z")));
        assert_eq!(
            date_time("2026-01-01T00:00:00Z").compare(&date_time("2025-12-31T23:59:59.5Z")),
            Some(Ordering::Greater)
        );
        // Without a time zone, values within 14 hours are incomparable.
        assert_eq!(
            date_time("2026-01-01T00:00:00Z").compare(&date_time("2026-01-01T10:00:00")),
            None
        );
        assert_eq!(
            date_time("2026-01-01T00:00:00Z").compare(&date_time("2026-01-02T00:00:00")),
            Some(Ordering::Less)
        );
        assert_eq!(
            date_time("2026-01-02T00:00:00").compare(&date_time("2026-01-01T00:00:00Z")),
            Some(Ordering::Greater)
        );
        assert!(!date_time("2026-01-01T00:00:00Z").equals(&date_time("2026-01-01T00:00:00")));
        let date = |value| check("date", value).unwrap();
        assert_eq!(
            date("-0001-12-31").compare(&date("0001-01-01")),
            Some(Ordering::Less)
        );
        assert_eq!(
            date("2026-01-01").compare(&date("2025-12-31")),
            Some(Ordering::Greater)
        );
        let time = |value| check("time", value).unwrap();
        assert_eq!(
            time("12:00:00+01:00").compare(&time("11:30:00Z")),
            Some(Ordering::Less)
        );
    }

    #[test]
    fn validates_and_compares_durations() {
        assert_valid(
            "duration",
            &[
                "P1Y2M3DT10H30M",
                "-P1D",
                "PT0.5S",
                "P0Y",
                "PT36H",
                "P1M",
                "PT1M",
                "P123456789012345678901234567890Y",
            ],
        );
        assert_invalid(
            "duration",
            &[
                "P", "P1DT", "P1.5Y", "PT1.5M", "1Y", "P-1D", "PT", "P1Y1Y", "P1D2M", "PT1S2M",
                "P1H", "-P", "P1", "PT.5S", "PT1.S",
            ],
        );
        assert!(
            check("duration", "P1DT")
                .unwrap_err()
                .contains("'T' must be followed")
        );
        let duration = |value| check("duration", value).unwrap();
        assert!(duration("P1Y").equals(&duration("P12M")));
        assert!(duration("P1D").equals(&duration("PT24H")));
        assert!(!duration("P1M").equals(&duration("P30D")));
        assert_eq!(duration("P1M").compare(&duration("P30D")), None);
        assert_eq!(
            duration("P1Y").compare(&duration("P364D")),
            Some(Ordering::Greater)
        );
        assert_eq!(
            duration("P1Y").compare(&duration("P367D")),
            Some(Ordering::Less)
        );
        assert_eq!(
            duration("-P1D").compare(&duration("PT1S")),
            Some(Ordering::Less)
        );
    }

    #[test]
    fn validates_binary_types() {
        assert_valid("hexBinary", &["", "0FB7", "0fb7"]);
        assert_invalid("hexBinary", &["0FB", "0G", "0x0F"]);
        assert_eq!(
            check("hexBinary", "0fB7"),
            Ok(Value::Binary(vec![0x0F, 0xB7]))
        );
        assert_valid(
            "base64Binary",
            &["", "SGVsbG8=", "SGVs bG8=", "SGVsbA==", "AAAA"],
        );
        assert_invalid(
            "base64Binary",
            &[
                "SGVsbG8",
                "SGVsbG8==",
                "S=Vs",
                "SGVsbG9=",
                "SGVsbB==",
                "SG!s",
                "====",
            ],
        );
        assert_eq!(
            check("base64Binary", "SGVsbG8="),
            Ok(Value::Binary(b"Hello".to_vec()))
        );
        assert!(
            check("base64Binary", "SGVsbG8")
                .unwrap_err()
                .contains("multiple of 4")
        );
    }

    fn schema_type(body: &str) -> Arc<crate::model::XsdModel> {
        Arc::new(
            parse_xsd_model(&format!(
                r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">{body}</xs:schema>"#
            ))
            .expect("schema"),
        )
    }

    fn validate_with(body: &str, value: &str) -> Result<Value, ValueError> {
        let set = XsdModelSet::new(vec![schema_type(body)]);
        let definition = set.models()[0].types.first().expect("a type");
        let simple_type = set
            .simple_type(XsdTypeRef {
                schema: 0,
                name: None,
                definition: Some(definition),
            })
            .expect("a simple type");
        simple_type.validate(value, None)
    }

    #[test]
    fn applies_length_facets() {
        let body = r#"<xs:simpleType name="t"><xs:restriction base="xs:string"><xs:minLength value="2"/><xs:maxLength value="3"/></xs:restriction></xs:simpleType>"#;
        assert!(validate_with(body, "ab").is_ok());
        assert!(validate_with(body, "été").is_ok());
        assert!(
            validate_with(body, "a")
                .unwrap_err()
                .message
                .contains("'a' has 1 characters; anonymous type requires at least 2 (minLength)")
        );
        assert!(validate_with(body, "abcd").is_err());
        let binary = r#"<xs:simpleType name="t"><xs:restriction base="xs:hexBinary"><xs:length value="2"/></xs:restriction></xs:simpleType>"#;
        assert!(validate_with(binary, "0FB7").is_ok());
        assert!(
            validate_with(binary, "0F")
                .unwrap_err()
                .message
                .contains("1 octets")
        );
        let list = r#"<xs:simpleType name="t"><xs:restriction base="xs:NMTOKENS"><xs:maxLength value="2"/></xs:restriction></xs:simpleType>"#;
        assert!(validate_with(list, "a b").is_ok());
        assert!(
            validate_with(list, "a b c")
                .unwrap_err()
                .message
                .contains("3 items")
        );
        let qname = r#"<xs:simpleType name="t"><xs:restriction base="xs:QName"><xs:length value="1"/></xs:restriction></xs:simpleType>"#;
        assert!(validate_with(qname, "p:long").is_ok());
    }

    #[test]
    fn applies_patterns_per_derivation_step() {
        let body = r#"<xs:simpleType name="t"><xs:restriction base="u"><xs:pattern value="a.*"/></xs:restriction></xs:simpleType>
            <xs:simpleType name="u"><xs:restriction base="xs:string"><xs:pattern value=".*z"/><xs:pattern value="b+"/></xs:restriction></xs:simpleType>"#;
        assert!(validate_with(body, "abz").is_ok());
        // Patterns of one step are alternatives, steps are combined.
        assert!(
            validate_with(body, "ab")
                .unwrap_err()
                .message
                .contains("pattern '.*z' or 'b+' of anonymous type")
        );
        assert!(
            validate_with(body, "bz")
                .unwrap_err()
                .message
                .contains("'a.*'")
        );
        let collapsed = r#"<xs:simpleType name="t"><xs:restriction base="xs:token"><xs:pattern value="a b"/></xs:restriction></xs:simpleType>"#;
        assert!(validate_with(collapsed, "  a\n b ").is_ok());
    }

    #[test]
    fn compares_enumerations_in_the_value_space() {
        let decimal = r#"<xs:simpleType name="t"><xs:restriction base="xs:decimal"><xs:enumeration value="1.0"/><xs:enumeration value="2.5"/></xs:restriction></xs:simpleType>"#;
        assert!(validate_with(decimal, "1").is_ok());
        assert!(validate_with(decimal, "+02.50").is_ok());
        let error = validate_with(decimal, "3").unwrap_err();
        assert!(error.enumeration);
        assert!(
            error
                .message
                .contains("'3' is not one of the values allowed by anonymous type: '1.0', '2.5'")
        );
        let date_time = r#"<xs:simpleType name="t"><xs:restriction base="xs:dateTime"><xs:enumeration value="2026-01-01T00:00:00Z"/></xs:restriction></xs:simpleType>"#;
        assert!(validate_with(date_time, "2026-01-01T02:00:00+02:00").is_ok());
        let token = r#"<xs:simpleType name="t"><xs:restriction base="xs:token"><xs:enumeration value=" red "/></xs:restriction></xs:simpleType>"#;
        assert!(validate_with(token, "red").is_ok());
        let string = r#"<xs:simpleType name="t"><xs:restriction base="xs:string"><xs:enumeration value="red"/></xs:restriction></xs:simpleType>"#;
        assert!(validate_with(string, " red").is_err());
        let list = r#"<xs:simpleType name="t"><xs:restriction base="l"><xs:enumeration value="1 2"/></xs:restriction></xs:simpleType>
            <xs:simpleType name="l"><xs:list itemType="xs:int"/></xs:simpleType>"#;
        assert!(validate_with(list, " 01  +2 ").is_ok());
        assert!(validate_with(list, "1 3").is_err());
    }

    #[test]
    fn applies_bounds_to_numbers_dates_and_durations() {
        let int = r#"<xs:simpleType name="t"><xs:restriction base="xs:int"><xs:minInclusive value="10"/><xs:maxExclusive value="20"/></xs:restriction></xs:simpleType>"#;
        assert!(validate_with(int, "10").is_ok());
        assert!(
            validate_with(int, "9")
                .unwrap_err()
                .message
                .contains("'9' must be at least 10 (minInclusive of anonymous type)")
        );
        assert!(
            validate_with(int, "20")
                .unwrap_err()
                .message
                .contains("less than 20 (maxExclusive")
        );
        let big = r#"<xs:simpleType name="t"><xs:restriction base="xs:integer"><xs:maxInclusive value="99999999999999999999999"/></xs:restriction></xs:simpleType>"#;
        assert!(validate_with(big, "99999999999999999999998").is_ok());
        assert!(validate_with(big, "100000000000000000000000").is_err());
        let float = r#"<xs:simpleType name="t"><xs:restriction base="xs:float"><xs:minExclusive value="0"/></xs:restriction></xs:simpleType>"#;
        assert!(validate_with(float, "1e-3").is_ok());
        assert!(validate_with(float, "-0").is_err());
        assert!(validate_with(float, "NaN").is_err());
        assert!(validate_with(float, "INF").is_ok());
        let date = r#"<xs:simpleType name="t"><xs:restriction base="xs:date"><xs:minInclusive value="2026-01-01"/><xs:maxInclusive value="2026-12-31"/></xs:restriction></xs:simpleType>"#;
        assert!(validate_with(date, "2026-06-15").is_ok());
        assert!(validate_with(date, "2025-12-31").is_err());
        assert!(validate_with(date, "2027-01-01").is_err());
        let duration = r#"<xs:simpleType name="t"><xs:restriction base="xs:duration"><xs:maxInclusive value="P1Y"/></xs:restriction></xs:simpleType>"#;
        assert!(validate_with(duration, "P11M").is_ok());
        assert!(validate_with(duration, "P13M").is_err());
        assert!(validate_with(duration, "P366D").is_err());
        let g_month = r#"<xs:simpleType name="t"><xs:restriction base="xs:gMonth"><xs:maxInclusive value="--06"/></xs:restriction></xs:simpleType>"#;
        assert!(validate_with(g_month, "--05").is_ok());
        assert!(validate_with(g_month, "--07").is_err());
    }

    #[test]
    fn applies_digit_facets_to_the_canonical_value() {
        let body = r#"<xs:simpleType name="t"><xs:restriction base="xs:decimal"><xs:totalDigits value="4"/><xs:fractionDigits value="2"/></xs:restriction></xs:simpleType>"#;
        assert!(validate_with(body, "12.34").is_ok());
        assert!(validate_with(body, "0012.3400").is_ok());
        assert!(
            validate_with(body, "12.345")
                .unwrap_err()
                .message
                .contains("5 digits, more than the 4 allowed by anonymous type (totalDigits)")
        );
        assert!(
            validate_with(body, "1.234")
                .unwrap_err()
                .message
                .contains("fractionDigits")
        );
    }

    #[test]
    fn honours_user_whitespace_facets() {
        let body = r#"<xs:simpleType name="t"><xs:restriction base="xs:string"><xs:whiteSpace value="collapse"/><xs:length value="3"/></xs:restriction></xs:simpleType>"#;
        assert!(validate_with(body, "  abc \n").is_ok());
        let replace = r#"<xs:simpleType name="t"><xs:restriction base="xs:normalizedString"><xs:length value="3"/></xs:restriction></xs:simpleType>"#;
        assert!(validate_with(replace, "a\tb").is_ok());
    }

    #[test]
    fn validates_lists_and_unions() {
        let list = r#"<xs:simpleType name="t"><xs:list itemType="xs:date"/></xs:simpleType>"#;
        assert!(validate_with(list, "2026-01-01  2026-01-02").is_ok());
        assert!(validate_with(list, "").is_ok());
        let error = validate_with(list, "2026-01-01 2026-02-30").unwrap_err();
        assert!(
            error
                .message
                .contains("'2026-02-30' is not a valid xs:date"),
            "{error:?}"
        );
        assert!(
            error
                .message
                .contains("(item 2 of the list anonymous type)")
        );
        let inline = r#"<xs:simpleType name="t"><xs:list><xs:simpleType><xs:restriction base="xs:int"><xs:enumeration value="1"/></xs:restriction></xs:simpleType></xs:list></xs:simpleType>"#;
        assert!(validate_with(inline, "1 1").is_ok());
        let error = validate_with(inline, "1 2").unwrap_err();
        assert!(!error.enumeration);
        let union = r#"<xs:simpleType name="t"><xs:union memberTypes="xs:int xs:boolean"><xs:simpleType><xs:restriction base="xs:token"><xs:enumeration value="none"/></xs:restriction></xs:simpleType></xs:union></xs:simpleType>"#;
        assert!(validate_with(union, "12").is_ok());
        assert!(validate_with(union, "true").is_ok());
        assert!(validate_with(union, " none ").is_ok());
        assert!(validate_with(union, "maybe").unwrap_err().message.contains(
            "'maybe' is not valid for any member type of anonymous type (xs:int, xs:boolean, anonymous type)"
        ));
        let restricted = r#"<xs:simpleType name="t"><xs:restriction base="u"><xs:enumeration value="1"/><xs:enumeration value="true"/></xs:restriction></xs:simpleType>
            <xs:simpleType name="u"><xs:union memberTypes="xs:int xs:boolean"/></xs:simpleType>"#;
        assert!(validate_with(restricted, "01").is_ok());
        assert!(validate_with(restricted, "true").is_ok());
        assert!(validate_with(restricted, "2").unwrap_err().enumeration);
    }

    #[test]
    fn resolves_simple_content_and_rejects_complex_content() {
        let set = XsdModelSet::new(vec![schema_type(
            r#"<xs:complexType name="price"><xs:simpleContent><xs:extension base="amount"><xs:attribute name="currency"/></xs:extension></xs:simpleContent></xs:complexType>
               <xs:simpleType name="amount"><xs:restriction base="xs:decimal"><xs:minInclusive value="0"/></xs:restriction></xs:simpleType>
               <xs:complexType name="box"><xs:sequence><xs:element name="a"/></xs:sequence></xs:complexType>"#,
        )]);
        let reference = |index: usize| XsdTypeRef {
            schema: 0,
            name: None,
            definition: Some(&set.models()[0].types[index]),
        };
        let price = set.simple_type(reference(0)).unwrap();
        assert!(price.validate("12.5", None).is_ok());
        assert!(price.validate("-1", None).is_err());
        assert!(
            price
                .validate("abc", None)
                .unwrap_err()
                .message
                .contains("derived from xs:decimal")
        );
        assert!(set.simple_type(reference(2)).is_none());
    }

    #[test]
    fn values_equal_compares_fixed_values() {
        let decimal = SimpleType::builtin(BuiltinType::Decimal);
        assert!(decimal.values_equal("1", "1.00", None));
        assert!(!decimal.values_equal("1", "2", None));
        let string = SimpleType::builtin(BuiltinType::String);
        assert!(!string.values_equal(" a", "a", None));
        let facets = XsdFacets {
            enumerations: vec![XsdEnumeration {
                value: "x".to_owned(),
                documentation: None,
            }],
            ..XsdFacets::default()
        };
        let restricted = SimpleType {
            name: "t".to_owned(),
            variety: Variety::Atomic(BuiltinType::Token),
            facets: vec![&facets],
        };
        assert!(restricted.validate("x", None).is_ok());
        assert!(restricted.validate("y", None).unwrap_err().enumeration);
    }

    #[test]
    fn quotes_long_values() {
        let long = "a".repeat(100);
        let message = check("int", &long).unwrap_err();
        assert!(message.contains(&format!("'{}…'", "a".repeat(64))));
    }
}
