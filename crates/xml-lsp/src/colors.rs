//! Document colors (`textDocument/documentColor`) and their presentations
//! (`textDocument/colorPresentation`).
//!
//! Recognized color sources:
//!
//! - unprefixed SVG presentation attributes (`fill`, `stroke`,
//!   `stop-color`, `flood-color`, `lighting-color`, `color`, `solid-color`);
//! - CSS declarations of the `style="..."` attribute;
//! - CSS content (text and CDATA sections) of `<style>` elements;
//! - Android resources: attributes of the Android namespaces
//!   (`android:textColor`, `app:tint`, `android:background`...) and text
//!   content of the `<color>`, `<item>` and `<drawable>` elements of a
//!   resource file (`<resources>` root or a file under `res/values*/`).
//!
//! Hexadecimal semantics, decided by the context:
//!
//! - CSS/SVG (presentation attributes, `style`, `<style>`): `#rgb`,
//!   `#rgba`, `#rrggbb`, `#rrggbbaa` — alpha **last**;
//! - Android (attribute whose prefix is bound to
//!   `http://schemas.android.com/apk/res/android`, `.../res-auto` or
//!   `.../tools` — or an undeclared `android`/`app`/`tools` prefix — and text
//!   of a resource file): `#RGB`, `#ARGB`, `#RRGGBB`, `#AARRGGBB` —
//!   alpha **first**. Only a fully hexadecimal value is a
//!   color (`@color/x` and `?attr/y` are references).
//!
//! In CSS, `rgb()`/`rgba()` (commas or spaces with `/ alpha`, numbers or
//! percentages), `hsl()`/`hsla()` (hue in `deg`/`rad`/`grad`/`turn`) and
//! named colors (including `transparent`) are also recognized;
//! `currentColor`, `none`, `inherit` and `url(...)` are ignored.

use std::ops::Range;

use serde_json::{Value, json};
use xml_core::tags::{
    XmlAttribute, XmlMarkupKind, XmlTagTree, resolve_namespace, scan_attributes, scan_markup,
};

use crate::selection::LineIndex;

/// Namespace of the Android platform attributes.
pub(crate) const ANDROID_NAMESPACE: &str = "http://schemas.android.com/apk/res/android";
/// Namespace of the Android application attributes (`app:`).
pub(crate) const ANDROID_AUTO_NAMESPACE: &str = "http://schemas.android.com/apk/res-auto";
/// Namespace of the Android tools attributes (`tools:`).
pub(crate) const ANDROID_TOOLS_NAMESPACE: &str = "http://schemas.android.com/tools";

/// SVG presentation attributes whose value is a color.
const SVG_COLOR_ATTRIBUTES: &[&str] = &[
    "color",
    "fill",
    "flood-color",
    "lighting-color",
    "solid-color",
    "stop-color",
    "stroke",
];

/// CSS properties whose identifiers are never colors.
const NON_COLOR_PROPERTIES: &[&str] = &[
    "animation",
    "animation-name",
    "content",
    "counter-increment",
    "counter-reset",
    "font",
    "font-family",
    "grid-area",
    "grid-template-areas",
    "quotes",
    "transition",
    "transition-property",
    "will-change",
];

/// RGBA color, components in `[0, 1]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Rgba {
    pub red: f64,
    pub green: f64,
    pub blue: f64,
    pub alpha: f64,
}

impl Rgba {
    fn from_bytes(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self {
            red: f64::from(red) / 255.0,
            green: f64::from(green) / 255.0,
            blue: f64::from(blue) / 255.0,
            alpha: f64::from(alpha) / 255.0,
        }
    }

    /// LSP `Color` extracted from `value` (clamped components).
    pub(crate) fn from_json(value: &Value) -> Option<Self> {
        let component = |name: &str| Some(value.get(name)?.as_f64()?.clamp(0.0, 1.0));
        Some(Self {
            red: component("red")?,
            green: component("green")?,
            blue: component("blue")?,
            alpha: component("alpha")?,
        })
    }

    fn to_json(self) -> Value {
        json!({
            "red": self.red,
            "green": self.green,
            "blue": self.blue,
            "alpha": self.alpha,
        })
    }

    /// 8-bit components `[r, g, b, a]`.
    fn bytes(self) -> [u8; 4] {
        [self.red, self.green, self.blue, self.alpha].map(channel)
    }
}

fn channel(value: f64) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Position of the alpha in a hexadecimal color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HexSyntax {
    /// `#rgb[a]` / `#rrggbb[aa]`: alpha last.
    Css,
    /// `#[a]rgb` / `#[aa]rrggbb`: alpha first.
    Android,
}

/// Written form of a color, reused as the first presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorFormat {
    Hex {
        syntax: HexSyntax,
        /// Number of digits (3, 4, 6 or 8).
        digits: usize,
        uppercase: bool,
    },
    Rgb {
        /// Function written as `rgba`.
        alpha_function: bool,
        /// Comma syntax (otherwise spaces and `/ alpha`).
        legacy: bool,
    },
    Hsl {
        alpha_function: bool,
        legacy: bool,
    },
    Named,
}

/// Color found in the document.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ColorMatch {
    /// Range (UTF-8 bytes) of the color text.
    pub range: Range<usize>,
    pub color: Rgba,
    pub format: ColorFormat,
}

/// JSON response to `textDocument/documentColor`.
pub(crate) fn document_colors(uri: &str, source: &str) -> Value {
    let index = LineIndex::new(source);
    Value::Array(
        find_colors(uri, source)
            .into_iter()
            .map(|found| {
                json!({
                    "range": {
                        "start": index.position(source, found.range.start),
                        "end": index.position(source, found.range.end),
                    },
                    "color": found.color.to_json(),
                })
            })
            .collect(),
    )
}

/// JSON response to `textDocument/colorPresentation`: the original form of
/// the color at `range` first, then the other forms, each with a
/// `textEdit` replacing `lsp_range`.
pub(crate) fn color_presentations(
    uri: &str,
    source: &str,
    range: Range<usize>,
    color: Rgba,
    lsp_range: &Value,
) -> Value {
    let colors = find_colors(uri, source);
    let original = colors
        .iter()
        .find(|found| found.range == range)
        .or_else(|| {
            colors
                .iter()
                .find(|found| found.range.start <= range.start && range.start < found.range.end)
        })
        .map(|found| found.format);
    Value::Array(
        presentations(color, original)
            .into_iter()
            .map(|label| {
                json!({
                    "label": label,
                    "textEdit": {"range": lsp_range, "newText": label},
                })
            })
            .collect(),
    )
}

/// Labels offered for `color`, original form first, without duplicates.
pub(crate) fn presentations(color: Rgba, original: Option<ColorFormat>) -> Vec<String> {
    let mut labels = Vec::new();
    let original = original.unwrap_or(ColorFormat::Hex {
        syntax: HexSyntax::Css,
        digits: 6,
        uppercase: false,
    });
    labels.extend(format_color(color, original));
    match original {
        ColorFormat::Hex {
            syntax: HexSyntax::Android,
            uppercase,
            ..
        } => {
            labels.push(format_hex(color, HexSyntax::Android, 8, uppercase));
            if color.bytes()[3] == 255 {
                labels.push(format_hex(color, HexSyntax::Android, 6, uppercase));
            }
        }
        _ => {
            let uppercase = matches!(
                original,
                ColorFormat::Hex {
                    uppercase: true,
                    ..
                }
            );
            labels.push(format_hex(color, HexSyntax::Css, 6, uppercase));
            labels.push(format_rgb(color, false, true));
            labels.push(format_hsl(color, false, true));
            labels.extend(color_name(color).map(str::to_owned));
        }
    }
    let mut unique = Vec::with_capacity(labels.len());
    for label in labels {
        if !unique.contains(&label) {
            unique.push(label);
        }
    }
    unique
}

/// Writes `color` in the `format` form (`None` for a named color without
/// an exact name).
fn format_color(color: Rgba, format: ColorFormat) -> Option<String> {
    Some(match format {
        ColorFormat::Hex {
            syntax,
            digits,
            uppercase,
        } => format_hex(color, syntax, digits, uppercase),
        ColorFormat::Rgb {
            alpha_function,
            legacy,
        } => format_rgb(color, alpha_function, legacy),
        ColorFormat::Hsl {
            alpha_function,
            legacy,
        } => format_hsl(color, alpha_function, legacy),
        ColorFormat::Named => color_name(color)?.to_owned(),
    })
}

/// Hexadecimal form closest to `digits`: short forms (3/4) are only kept
/// when exact, and the alpha is written if the original form had it (4/8)
/// or if the color is not opaque.
fn format_hex(color: Rgba, syntax: HexSyntax, digits: usize, uppercase: bool) -> String {
    let [red, green, blue, alpha] = color.bytes();
    let with_alpha = matches!(digits, 4 | 8) || alpha != 255;
    let rgb = [red, green, blue];
    let channels: Vec<u8> = match (syntax, with_alpha) {
        (_, false) => rgb.to_vec(),
        (HexSyntax::Css, true) => vec![red, green, blue, alpha],
        (HexSyntax::Android, true) => vec![alpha, red, green, blue],
    };
    let short = matches!(digits, 3 | 4) && channels.iter().all(|value| value % 17 == 0);
    let mut text = String::from("#");
    for value in channels {
        if short {
            text.push_str(&format!("{:x}", value / 17));
        } else {
            text.push_str(&format!("{value:02x}"));
        }
    }
    if uppercase {
        text.make_ascii_uppercase();
    }
    text
}

fn format_alpha(alpha: f64) -> String {
    let text = format!("{:.3}", alpha.clamp(0.0, 1.0));
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

fn format_rgb(color: Rgba, alpha_function: bool, legacy: bool) -> String {
    let [red, green, blue, _] = color.bytes();
    let opaque = color.bytes()[3] == 255;
    let alpha = format_alpha(color.alpha);
    match (legacy, opaque && !alpha_function) {
        (true, true) => format!("rgb({red}, {green}, {blue})"),
        (true, false) => format!("rgba({red}, {green}, {blue}, {alpha})"),
        (false, _) => {
            let name = if alpha_function { "rgba" } else { "rgb" };
            if opaque {
                format!("{name}({red} {green} {blue})")
            } else {
                format!("{name}({red} {green} {blue} / {alpha})")
            }
        }
    }
}

fn format_hsl(color: Rgba, alpha_function: bool, legacy: bool) -> String {
    let (hue, saturation, lightness) = rgb_to_hsl(color);
    let hue = (hue.round() as u32) % 360;
    let saturation = (saturation * 100.0).round() as u32;
    let lightness = (lightness * 100.0).round() as u32;
    let opaque = color.bytes()[3] == 255;
    let alpha = format_alpha(color.alpha);
    match (legacy, opaque && !alpha_function) {
        (true, true) => format!("hsl({hue}, {saturation}%, {lightness}%)"),
        (true, false) => format!("hsla({hue}, {saturation}%, {lightness}%, {alpha})"),
        (false, _) => {
            let name = if alpha_function { "hsla" } else { "hsl" };
            if opaque {
                format!("{name}({hue} {saturation}% {lightness}%)")
            } else {
                format!("{name}({hue} {saturation}% {lightness}% / {alpha})")
            }
        }
    }
}

/// Exact CSS name of `color` (`transparent` for a fully transparent
/// black), the first in alphabetical order in case of aliases.
fn color_name(color: Rgba) -> Option<&'static str> {
    let [red, green, blue, alpha] = color.bytes();
    if alpha == 0 && [red, green, blue] == [0, 0, 0] {
        return Some("transparent");
    }
    if alpha != 255 {
        return None;
    }
    let value = (u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue);
    NAMED_COLORS
        .iter()
        .find(|(_, rgb)| *rgb == value)
        .map(|(name, _)| *name)
}

/// Hue in degrees, saturation and lightness in `[0, 1]`.
fn rgb_to_hsl(color: Rgba) -> (f64, f64, f64) {
    let [red, green, blue] = [color.red, color.green, color.blue];
    let max = red.max(green).max(blue);
    let min = red.min(green).min(blue);
    let lightness = (max + min) / 2.0;
    let delta = max - min;
    if delta.abs() < f64::EPSILON {
        return (0.0, 0.0, lightness);
    }
    let saturation = delta / (1.0 - (2.0 * lightness - 1.0).abs());
    let hue = if max == red {
        ((green - blue) / delta).rem_euclid(6.0)
    } else if max == green {
        (blue - red) / delta + 2.0
    } else {
        (red - green) / delta + 4.0
    };
    (hue * 60.0, saturation.clamp(0.0, 1.0), lightness)
}

fn hsl_to_rgb(hue: f64, saturation: f64, lightness: f64, alpha: f64) -> Rgba {
    let hue = hue.rem_euclid(360.0) / 360.0;
    let (saturation, lightness) = (saturation.clamp(0.0, 1.0), lightness.clamp(0.0, 1.0));
    if saturation == 0.0 {
        return Rgba {
            red: lightness,
            green: lightness,
            blue: lightness,
            alpha,
        };
    }
    let q = if lightness < 0.5 {
        lightness * (1.0 + saturation)
    } else {
        lightness + saturation - lightness * saturation
    };
    let p = 2.0 * lightness - q;
    let component = |mut t: f64| {
        t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    Rgba {
        red: component(hue + 1.0 / 3.0),
        green: component(hue),
        blue: component(hue - 1.0 / 3.0),
        alpha,
    }
}

/// Parses a whole value (surrounding whitespace ignored): hexadecimal color
/// according to `syntax`, and in CSS also `rgb()`, `hsl()` and names.
pub(crate) fn parse_color(text: &str, syntax: HexSyntax) -> Option<(Rgba, ColorFormat)> {
    let text = text.trim();
    if let Some(digits) = text.strip_prefix('#') {
        return parse_hex(digits, syntax);
    }
    if syntax == HexSyntax::Android {
        return None;
    }
    if let Some(open) = text.find('(') {
        let arguments = text[open + 1..].strip_suffix(')')?;
        return parse_function(&text[..open], arguments);
    }
    named_color(text).map(|color| (color, ColorFormat::Named))
}

/// Digits of a hexadecimal color, `#` excluded.
fn parse_hex(digits: &str, syntax: HexSyntax) -> Option<(Rgba, ColorFormat)> {
    if !matches!(digits.len(), 3 | 4 | 6 | 8) || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let bytes = digits.as_bytes();
    let values: Vec<u8> = if digits.len() <= 4 {
        bytes.iter().map(|&b| hex_value(b) * 17).collect()
    } else {
        bytes
            .chunks(2)
            .map(|pair| hex_value(pair[0]) * 16 + hex_value(pair[1]))
            .collect()
    };
    let color = match (syntax, values.as_slice()) {
        (_, &[red, green, blue]) => Rgba::from_bytes(red, green, blue, 255),
        (HexSyntax::Css, &[red, green, blue, alpha]) => Rgba::from_bytes(red, green, blue, alpha),
        (HexSyntax::Android, &[alpha, red, green, blue]) => {
            Rgba::from_bytes(red, green, blue, alpha)
        }
        _ => return None,
    };
    let uppercase = digits.bytes().any(|b| b.is_ascii_uppercase())
        || (syntax == HexSyntax::Android && !digits.bytes().any(|b| b.is_ascii_lowercase()));
    Some((
        color,
        ColorFormat::Hex {
            syntax,
            digits: digits.len(),
            uppercase,
        },
    ))
}

fn hex_value(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        _ => byte - b'A' + 10,
    }
}

/// `rgb()`, `rgba()`, `hsl()` or `hsla()` (case-insensitive name) with its
/// arguments (parentheses excluded).
fn parse_function(name: &str, arguments: &str) -> Option<(Rgba, ColorFormat)> {
    let name = name.to_ascii_lowercase();
    let is_rgb = match name.as_str() {
        "rgb" | "rgba" => true,
        "hsl" | "hsla" => false,
        _ => return None,
    };
    let alpha_function = name.ends_with('a');
    let legacy = arguments.contains(',');
    let (channels, alpha): (Vec<&str>, Option<&str>) = if legacy {
        if arguments.contains('/') {
            return None;
        }
        let mut parts: Vec<&str> = arguments.split(',').map(str::trim).collect();
        let alpha = match parts.len() {
            3 => None,
            4 => parts.pop(),
            _ => return None,
        };
        (parts, alpha)
    } else {
        let (channels, alpha) = match arguments.split_once('/') {
            Some((channels, alpha)) => (channels, Some(alpha.trim())),
            None => (arguments, None),
        };
        (channels.split_whitespace().collect(), alpha)
    };
    if channels.len() != 3
        || channels
            .iter()
            .any(|part| part.contains(char::is_whitespace))
    {
        return None;
    }
    let alpha = match alpha {
        Some(alpha) => parse_alpha(alpha, legacy)?,
        None => 1.0,
    };
    let color = if is_rgb {
        let red = parse_rgb_channel(channels[0], legacy)?;
        let green = parse_rgb_channel(channels[1], legacy)?;
        let blue = parse_rgb_channel(channels[2], legacy)?;
        Rgba {
            red,
            green,
            blue,
            alpha,
        }
    } else {
        let hue = parse_hue(channels[0], legacy)?;
        let saturation = parse_percentage(channels[1], legacy)?;
        let lightness = parse_percentage(channels[2], legacy)?;
        hsl_to_rgb(hue, saturation, lightness, alpha)
    };
    let format = if is_rgb {
        ColorFormat::Rgb {
            alpha_function,
            legacy,
        }
    } else {
        ColorFormat::Hsl {
            alpha_function,
            legacy,
        }
    };
    Some((color, format))
}

/// CSS number (`+`/`-`, decimals, exponent); rejects `inf`/`NaN`.
fn parse_number(text: &str) -> Option<f64> {
    let valid = !text.is_empty()
        && text.bytes().any(|b| b.is_ascii_digit())
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'+' | b'-' | b'e' | b'E'));
    if !valid {
        return None;
    }
    text.parse::<f64>().ok().filter(|value| value.is_finite())
}

/// `none` counts as 0 in the modern syntax (CSS Color 4).
fn is_none(text: &str, legacy: bool) -> bool {
    !legacy && text.eq_ignore_ascii_case("none")
}

fn parse_rgb_channel(text: &str, legacy: bool) -> Option<f64> {
    if is_none(text, legacy) {
        return Some(0.0);
    }
    match text.strip_suffix('%') {
        Some(percentage) => Some((parse_number(percentage)? / 100.0).clamp(0.0, 1.0)),
        None => Some((parse_number(text)? / 255.0).clamp(0.0, 1.0)),
    }
}

fn parse_alpha(text: &str, legacy: bool) -> Option<f64> {
    if is_none(text, legacy) {
        return Some(0.0);
    }
    match text.strip_suffix('%') {
        Some(percentage) => Some((parse_number(percentage)? / 100.0).clamp(0.0, 1.0)),
        None => Some(parse_number(text)?.clamp(0.0, 1.0)),
    }
}

/// Saturation or lightness: percentage (required in comma syntax, a bare
/// number being accepted in the modern syntax).
fn parse_percentage(text: &str, legacy: bool) -> Option<f64> {
    if is_none(text, legacy) {
        return Some(0.0);
    }
    match text.strip_suffix('%') {
        Some(percentage) => Some((parse_number(percentage)? / 100.0).clamp(0.0, 1.0)),
        None if !legacy => Some((parse_number(text)? / 100.0).clamp(0.0, 1.0)),
        None => None,
    }
}

/// Hue in degrees (`deg`, `rad`, `grad`, `turn` or unitless).
fn parse_hue(text: &str, legacy: bool) -> Option<f64> {
    if is_none(text, legacy) {
        return Some(0.0);
    }
    let lower = text.to_ascii_lowercase();
    for (unit, factor) in [
        ("grad", 0.9),
        ("deg", 1.0),
        ("rad", 180.0 / std::f64::consts::PI),
        ("turn", 360.0),
    ] {
        if let Some(number) = lower.strip_suffix(unit) {
            return Some(parse_number(number)? * factor);
        }
    }
    parse_number(&lower)
}

fn named_color(name: &str) -> Option<Rgba> {
    let lower = name.to_ascii_lowercase();
    if lower == "transparent" {
        return Some(Rgba::from_bytes(0, 0, 0, 0));
    }
    let index = NAMED_COLORS
        .binary_search_by(|(candidate, _)| (*candidate).cmp(lower.as_str()))
        .ok()?;
    let rgb = NAMED_COLORS[index].1;
    Some(Rgba::from_bytes(
        (rgb >> 16) as u8,
        (rgb >> 8) as u8,
        rgb as u8,
        255,
    ))
}

// ---------------------------------------------------------------------------
// Document traversal

/// All colors of the document, in document order.
pub(crate) fn find_colors(uri: &str, source: &str) -> Vec<ColorMatch> {
    let tree = XmlTagTree::parse(source);
    let elements = tree.elements();
    let attributes: Vec<Vec<XmlAttribute>> = elements
        .iter()
        .map(|element| scan_attributes(source, &element.start_tag))
        .collect();
    let android_resources = elements
        .iter()
        .find(|element| element.parent.is_none())
        .is_some_and(|root| local_name(root.name(source)) == "resources")
        || is_android_values_uri(uri);
    let markup = scan_markup(source);
    let mut colors = Vec::new();

    for (index, element) in elements.iter().enumerate() {
        for attribute in &attributes[index] {
            let Some(value) = attribute.value.clone() else {
                continue;
            };
            let name = attribute.name(source);
            let (prefix, local) = split_name(name);
            match prefix {
                Some("xmlns") => {}
                Some(prefix) => {
                    let android =
                        match resolve_namespace(source, &tree, &attributes, index, Some(prefix)) {
                            Some(namespace) => namespace.is_some_and(is_android_namespace),
                            None => matches!(prefix, "android" | "app" | "tools"),
                        };
                    if android && is_android_color_attribute(local) {
                        push_whole(&mut colors, source, value, HexSyntax::Android);
                    }
                }
                None if name == "style" => {
                    let bytes = source.as_bytes()[value.clone()].to_vec();
                    scan_css_declarations(&bytes, value.start, false, &mut colors);
                }
                None if SVG_COLOR_ATTRIBUTES.contains(&name) => {
                    scan_css_value(&source.as_bytes()[value.clone()], value.start, &mut colors);
                }
                None => {}
            }
        }

        let Some(content) = element.content_range() else {
            continue;
        };
        let local = local_name(element.name(source));
        if local == "style" {
            let bytes = masked_content(source, &tree, &markup, index, content.clone());
            scan_css_declarations(&bytes, content.start, true, &mut colors);
        } else if android_resources && matches!(local, "color" | "item" | "drawable") {
            push_whole(&mut colors, source, content, HexSyntax::Android);
        }
    }
    colors.sort_by_key(|found| found.range.start);
    colors.dedup_by(|a, b| a.range == b.range);
    colors
}

fn split_name(name: &str) -> (Option<&str>, &str) {
    match name.split_once(':') {
        Some((prefix, local)) => (Some(prefix), local),
        None => (None, name),
    }
}

fn local_name(name: &str) -> &str {
    split_name(name).1
}

fn is_android_namespace(namespace: &str) -> bool {
    matches!(
        namespace,
        ANDROID_NAMESPACE | ANDROID_AUTO_NAMESPACE | ANDROID_TOOLS_NAMESPACE
    )
}

/// Android attributes whose value may be a literal color.
fn is_android_color_attribute(local: &str) -> bool {
    let lower = local.to_ascii_lowercase();
    lower.contains("color")
        || lower.ends_with("tint")
        || matches!(
            lower.as_str(),
            "background" | "foreground" | "src" | "drawable"
        )
}

/// Android values resource file (`.../res/values*/*.xml`).
fn is_android_values_uri(uri: &str) -> bool {
    let path = uri.replace('\\', "/");
    let mut segments = path.rsplit('/');
    let (Some(file), Some(directory), Some(parent)) =
        (segments.next(), segments.next(), segments.next())
    else {
        return false;
    };
    file.to_ascii_lowercase().ends_with(".xml")
        && parent == "res"
        && (directory == "values" || directory.starts_with("values-"))
}

/// Adds the color if the whole text of `range` (surrounding whitespace
/// excluded) is one.
fn push_whole(colors: &mut Vec<ColorMatch>, source: &str, range: Range<usize>, syntax: HexSyntax) {
    let text = &source[range.clone()];
    let trimmed = text.trim();
    let start = range.start + (text.len() - text.trim_start().len());
    if let Some((color, format)) = parse_color(trimmed, syntax) {
        colors.push(ColorMatch {
            range: start..start + trimmed.len(),
            color,
            format,
        });
    }
}

/// Content of a `<style>` element where CDATA delimiters, XML comments,
/// processing instructions and child tags are replaced by spaces (offsets
/// are preserved).
fn masked_content(
    source: &str,
    tree: &XmlTagTree,
    markup: &[xml_core::tags::XmlMarkup],
    index: usize,
    content: Range<usize>,
) -> Vec<u8> {
    let mut bytes = source.as_bytes()[content.clone()].to_vec();
    let mut mask = |range: Range<usize>| {
        let start = range.start.max(content.start);
        let end = range.end.min(content.end);
        if start < end {
            bytes[start - content.start..end - content.start].fill(b' ');
        }
    };
    for item in markup {
        if item.range.end <= content.start || item.range.start >= content.end {
            continue;
        }
        match item.kind {
            XmlMarkupKind::CData => {
                mask(item.range.start..item.content.start);
                mask(item.content.end..item.range.end);
            }
            _ => mask(item.range.clone()),
        }
    }
    for element in &tree.elements()[index + 1..] {
        if element.start_tag.range.start >= content.end {
            break;
        }
        mask(element.start_tag.range.clone());
        if let Some(end_tag) = &element.end_tag {
            mask(end_tag.range.clone());
        }
    }
    bytes
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') || byte >= 0x80
}

/// End (exclusive) of a CSS string starting with the quote at `start`.
fn skip_string(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            byte if byte == quote => return index + 1,
            _ => index += 1,
        }
    }
    bytes.len()
}

/// End (exclusive) of a CSS comment starting at `start` (`/*`).
fn skip_comment(bytes: &[u8], start: usize) -> usize {
    bytes[start + 2..]
        .windows(2)
        .position(|window| window == b"*/")
        .map_or(bytes.len(), |end| start + 2 + end + 2)
}

/// Walks a style sheet (`nested`) or a declaration list (`style="..."`)
/// and parses the value of each declaration.
/// `base` is the offset of `bytes[0]` in the document.
fn scan_css_declarations(bytes: &[u8], base: usize, nested: bool, colors: &mut Vec<ColorMatch>) {
    let mut depth = usize::from(!nested);
    let mut segment = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' | b'\'' => index = skip_string(bytes, index),
            b'/' if bytes.get(index + 1) == Some(&b'*') => index = skip_comment(bytes, index),
            b'{' => {
                depth += 1;
                index += 1;
                segment = index;
            }
            byte @ (b';' | b'}') => {
                if depth > 0 {
                    declaration(bytes, segment..index, base, colors);
                }
                if byte == b'}' {
                    depth = depth.saturating_sub(1);
                }
                index += 1;
                segment = index;
            }
            _ => index += 1,
        }
    }
    if depth > 0 {
        declaration(bytes, segment..bytes.len(), base, colors);
    }
}

/// Parses the value of the `property: value` declaration of `range`.
fn declaration(bytes: &[u8], range: Range<usize>, base: usize, colors: &mut Vec<ColorMatch>) {
    let segment = &bytes[range.clone()];
    let Some(colon) = segment.iter().position(|&byte| byte == b':') else {
        return;
    };
    let property = segment[..colon].trim_ascii();
    if property.is_empty() || !property.iter().all(|&byte| is_ident_byte(byte)) {
        return;
    }
    let property = String::from_utf8_lossy(property).to_ascii_lowercase();
    if NON_COLOR_PROPERTIES.contains(&property.as_str()) {
        return;
    }
    let value = range.start + colon + 1..range.end;
    scan_css_value(&bytes[value.clone()], base + value.start, colors);
}

/// Finds the colors of a CSS value: `#hex`, `rgb[a]()`, `hsl[a]()` and
/// named colors, outside strings, comments and `url(...)`.
fn scan_css_value(bytes: &[u8], base: usize, colors: &mut Vec<ColorMatch>) {
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let after_ident = index > 0 && is_ident_byte(bytes[index - 1]);
        match byte {
            b'"' | b'\'' => index = skip_string(bytes, index),
            b'/' if bytes.get(index + 1) == Some(&b'*') => index = skip_comment(bytes, index),
            b'#' => {
                let end = ident_end(bytes, index + 1);
                if !after_ident
                    && let Ok(digits) = std::str::from_utf8(&bytes[index + 1..end])
                    && let Some((color, format)) = parse_hex(digits, HexSyntax::Css)
                {
                    colors.push(ColorMatch {
                        range: base + index..base + end,
                        color,
                        format,
                    });
                }
                index = end;
            }
            _ if is_ident_byte(byte) && !after_ident && !byte.is_ascii_digit() => {
                let end = ident_end(bytes, index);
                let name = std::str::from_utf8(&bytes[index..end]).unwrap_or_default();
                if bytes.get(end) == Some(&b'(') {
                    let lower = name.to_ascii_lowercase();
                    if lower == "url" {
                        index =
                            closing_parenthesis(bytes, end).map_or(bytes.len(), |close| close + 1);
                        continue;
                    }
                    if matches!(lower.as_str(), "rgb" | "rgba" | "hsl" | "hsla")
                        && let Some(close) = closing_parenthesis(bytes, end)
                        && let Ok(arguments) = std::str::from_utf8(&bytes[end + 1..close])
                        && let Some((color, format)) = parse_function(name, arguments)
                    {
                        colors.push(ColorMatch {
                            range: base + index..base + close + 1,
                            color,
                            format,
                        });
                        index = close + 1;
                        continue;
                    }
                    // Other function (`var(--x, red)`...): its arguments are parsed.
                    index = end + 1;
                    continue;
                }
                if let Some(color) = named_color(name) {
                    colors.push(ColorMatch {
                        range: base + index..base + end,
                        color,
                        format: ColorFormat::Named,
                    });
                }
                index = end;
            }
            _ => index += 1,
        }
    }
}

fn ident_end(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && is_ident_byte(bytes[index]) {
        index += 1;
    }
    index
}

/// Closing parenthesis matching the one at `open`.
fn closing_parenthesis(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut index = open;
    while index < bytes.len() {
        match bytes[index] {
            b'"' | b'\'' => {
                index = skip_string(bytes, index);
                continue;
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

/// CSS named colors (CSS Color 4, excluding `transparent`), sorted.
const NAMED_COLORS: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("black", 0x000000),
    ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xadff2f),
    ("grey", 0x808080),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("navy", 0x000080),
    ("oldlace", 0xfdf5e6),
    ("olive", 0x808000),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn css(text: &str) -> Option<[u8; 4]> {
        parse_color(text, HexSyntax::Css).map(|(color, _)| color.bytes())
    }

    fn android(text: &str) -> Option<[u8; 4]> {
        parse_color(text, HexSyntax::Android).map(|(color, _)| color.bytes())
    }

    /// Colors found as `(text, [r, g, b, a])`.
    fn found(uri: &str, source: &str) -> Vec<(String, [u8; 4])> {
        find_colors(uri, source)
            .into_iter()
            .map(|found| (source[found.range].to_owned(), found.color.bytes()))
            .collect()
    }

    fn texts(uri: &str, source: &str) -> Vec<String> {
        found(uri, source)
            .into_iter()
            .map(|(text, _)| text)
            .collect()
    }

    #[test]
    fn named_colors_are_complete_and_sorted() {
        assert_eq!(NAMED_COLORS.len(), 148);
        assert!(NAMED_COLORS.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(css("RebeccaPurple"), Some([0x66, 0x33, 0x99, 255]));
        assert_eq!(css("TRANSPARENT"), Some([0, 0, 0, 0]));
        assert_eq!(css("currentColor"), None);
        assert_eq!(css("none"), None);
        assert_eq!(css("inherit"), None);
    }

    #[test]
    fn parses_css_hex_with_alpha_last() {
        assert_eq!(css("#f00"), Some([255, 0, 0, 255]));
        assert_eq!(css("#F008"), Some([255, 0, 0, 0x88]));
        assert_eq!(css("#12AbEf"), Some([0x12, 0xab, 0xef, 255]));
        assert_eq!(css("#11223380"), Some([0x11, 0x22, 0x33, 0x80]));
        for invalid in [
            "#",
            "#ff",
            "#fffff",
            "#1234567",
            "#123456789",
            "#ggg",
            "#12 3",
        ] {
            assert_eq!(css(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn parses_android_hex_with_alpha_first() {
        assert_eq!(android("#f00"), Some([255, 0, 0, 255]));
        assert_eq!(android("#8f00"), Some([255, 0, 0, 0x88]));
        assert_eq!(android("#FF0000"), Some([255, 0, 0, 255]));
        assert_eq!(android("#80112233"), Some([0x11, 0x22, 0x33, 0x80]));
        assert_eq!(android(" #80112233 "), Some([0x11, 0x22, 0x33, 0x80]));
        assert_eq!(android("red"), None);
        assert_eq!(android("@color/red"), None);
        assert_eq!(android("rgb(1, 2, 3)"), None);
    }

    #[test]
    fn parses_rgb_functions() {
        assert_eq!(css("rgb(255, 0, 0)"), Some([255, 0, 0, 255]));
        assert_eq!(css("RGBA( 0 , 128 , 255 , 0.5 )"), Some([0, 128, 255, 128]));
        assert_eq!(css("rgb(100%, 50%, 0%)"), Some([255, 128, 0, 255]));
        assert_eq!(css("rgb(0 128 255)"), Some([0, 128, 255, 255]));
        assert_eq!(css("rgb(0 128 255 / 25%)"), Some([0, 128, 255, 64]));
        assert_eq!(css("rgba(0 128 255/.5)"), Some([0, 128, 255, 128]));
        assert_eq!(css("rgb(300, -5, 1e2)"), Some([255, 0, 100, 255]));
        assert_eq!(css("rgb(none 0 0)"), Some([0, 0, 0, 255]));
        assert_eq!(css("rgb(1, 2, 3, 4)"), Some([1, 2, 3, 255]));
        for invalid in [
            "rgb(1, 2)",
            "rgb(1 2 3 4)",
            "rgb(1, 2, 3 / 1)",
            "rgb(a, b, c)",
            "rgb(inf, 0, 0)",
            "rgb(1, 2, 3",
            "rgb(none, 0, 0)",
            "rgbx(1, 2, 3)",
            "rgb()",
        ] {
            assert_eq!(css(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn parses_hsl_functions() {
        assert_eq!(css("hsl(0, 100%, 50%)"), Some([255, 0, 0, 255]));
        assert_eq!(css("hsl(120deg 100% 25%)"), Some([0, 128, 0, 255]));
        assert_eq!(css("HSLA(240, 100%, 50%, 0.5)"), Some([0, 0, 255, 128]));
        assert_eq!(css("hsl(0.5turn 100% 50% / 1)"), Some([0, 255, 255, 255]));
        assert_eq!(css("hsl(-120, 100%, 50%)"), Some([0, 0, 255, 255]));
        assert_eq!(css("hsl(3.14159rad 100% 50%)"), Some([0, 255, 255, 255]));
        assert_eq!(css("hsl(200grad 100 50)"), Some([0, 255, 255, 255]));
        assert_eq!(css("hsl(0, 0%, 50%)"), Some([128, 128, 128, 255]));
        assert_eq!(
            css("hsl(0, 100, 50)"),
            None,
            "legacy syntax needs percentages"
        );
        assert_eq!(css("hsl(0deg, 100%)"), None);
    }

    #[test]
    fn finds_svg_presentation_attributes_and_style() {
        let source = concat!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\">\n",
            "  <rect fill=\"#F00\" stroke=\"url(#red) Blue\" color=\"currentColor\"/>\n",
            "  <stop stop-color=\"rgb(0 128 0 / 50%)\" offset=\"red\"/>\n",
            "  <path style=\"fill: hsl(0,100%,50%); stroke:#00ff0080; font-family: red; --c: var(--x, navy)\"/>\n",
            "  <g fill=\"none\" flood-color=\"#abcdefg\" lighting-color=\"tomato\" solid-color=\"#1234\"/>\n",
            "</svg>"
        );
        assert_eq!(
            found("file:///a.svg", source),
            vec![
                ("#F00".to_owned(), [255, 0, 0, 255]),
                ("Blue".to_owned(), [0, 0, 255, 255]),
                ("rgb(0 128 0 / 50%)".to_owned(), [0, 128, 0, 128]),
                ("hsl(0,100%,50%)".to_owned(), [255, 0, 0, 255]),
                ("#00ff0080".to_owned(), [0, 255, 0, 128]),
                ("navy".to_owned(), [0, 0, 128, 255]),
                ("tomato".to_owned(), [255, 99, 71, 255]),
                ("#1234".to_owned(), [0x11, 0x22, 0x33, 0x44]),
            ]
        );
    }

    #[test]
    fn finds_colors_in_style_elements_and_cdata() {
        let source = concat!(
            "<svg>\n<style>\n",
            "  .red, a:hover { fill: #ff0000 } /* color: blue */\n",
            "  <!-- stroke: green -->\n",
            "  <![CDATA[ rect { stroke: rgba(0, 0, 255, .5); content: \"red\" } ]]>\n",
            "  @media (min-width: 10px) { circle { fill: Gold; background: url(white.png) } }\n",
            "</style>\n<text>red</text></svg>"
        );
        assert_eq!(
            texts("file:///a.svg", source),
            vec!["#ff0000", "rgba(0, 0, 255, .5)", "Gold"]
        );
    }

    #[test]
    fn finds_android_resources_and_attributes() {
        let values = concat!(
            "<resources>\n",
            "  <color name=\"primary\">#FF6200EE</color>\n",
            "  <color name=\"short\"> #8F00 </color>\n",
            "  <color name=\"ref\">@color/primary</color>\n",
            "  <string name=\"s\">#FF0000</string>\n",
            "  <style name=\"T\"><item name=\"colorAccent\">#03DAC5</item></style>\n",
            "</resources>"
        );
        assert_eq!(
            found("file:///app/src/main/res/values/colors.xml", values),
            vec![
                ("#FF6200EE".to_owned(), [0x62, 0x00, 0xee, 255]),
                ("#8F00".to_owned(), [255, 0, 0, 0x88]),
                ("#03DAC5".to_owned(), [0x03, 0xda, 0xc5, 255]),
            ]
        );

        let layout = concat!(
            "<LinearLayout xmlns:a=\"http://schemas.android.com/apk/res/android\"\n",
            "    xmlns:app=\"http://schemas.android.com/apk/res-auto\"\n",
            "    a:background=\"#80FF0000\" a:textColor=\"?attr/colorPrimary\">\n",
            "  <ImageView app:tint=\"#0F0\" a:layout_width=\"#FFF\" fill=\"#80FF0000\"/>\n",
            "  <color>#FF0000</color>\n",
            "</LinearLayout>"
        );
        assert_eq!(
            found("file:///res/layout/main.xml", layout),
            vec![
                ("#80FF0000".to_owned(), [255, 0, 0, 0x80]),
                ("#0F0".to_owned(), [0, 255, 0, 255]),
                // Unprefixed SVG attribute: CSS semantics (alpha last).
                ("#80FF0000".to_owned(), [0x80, 255, 0, 0]),
            ]
        );
    }

    #[test]
    fn detects_android_context_from_namespace_or_path() {
        // Undeclared `android` prefix (fragment): Android convention.
        assert_eq!(
            found("file:///x.xml", "<View android:background=\"#8000\"/>"),
            vec![("#8000".to_owned(), [0, 0, 0, 0x88])]
        );
        // `android` prefix bound to another namespace: ignored.
        assert!(
            found(
                "file:///x.xml",
                "<v xmlns:android=\"urn:x\" android:color=\"#fff\"/>"
            )
            .is_empty()
        );
        // File under res/values without a <resources> root (partial document).
        assert_eq!(
            texts(
                "file:///C:/p/res/values-night/c.xml",
                "<color>#80FFFFFF</color>"
            ),
            vec!["#80FFFFFF"]
        );
        assert!(texts("file:///p/values/c.xml", "<color>#80FFFFFF</color>").is_empty());
        assert!(is_android_values_uri("file:///p/res/values-v21/styles.XML"));
        assert!(!is_android_values_uri("file:///p/res/layout/main.xml"));
    }

    #[test]
    fn keeps_token_boundaries() {
        assert!(
            texts(
                "file:///a.svg",
                "<a fill=\"darkred-ish x-red red2 #fffz\"/>"
            )
            .is_empty()
        );
        assert_eq!(
            texts("file:///a.svg", "<a fill=\"url('#f00') #f00,red\"/>"),
            vec!["#f00", "red"]
        );
        assert_eq!(
            texts(
                "file:///a.svg",
                "<a style=\"fill:'red';stroke:/*red*/lime\"/>"
            ),
            vec!["lime"]
        );
        // Unterminated value and malformed document.
        assert_eq!(texts("file:///a.svg", "<a fill=\"red"), vec!["red"]);
        assert_eq!(
            texts("file:///a.svg", "<a><b fill='#000'></a>"),
            vec!["#000"]
        );
    }

    #[test]
    fn reports_utf16_and_crlf_ranges() {
        let source = "<svg>\r\n  <text fill=\"é😀\" style=\"x:'😀'; fill: #ABC\"/>\r\n</svg>";
        let colors = document_colors("file:///a.svg", source);
        assert_eq!(
            colors,
            json!([{
                "range": {
                    "start": {"line": 1, "character": 40},
                    "end": {"line": 1, "character": 44},
                },
                "color": {"red": 170.0 / 255.0, "green": 187.0 / 255.0, "blue": 204.0 / 255.0, "alpha": 1.0},
            }])
        );
    }

    fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> Rgba {
        Rgba::from_bytes(red, green, blue, alpha)
    }

    #[test]
    fn presents_css_colors_original_format_first() {
        let hex = |digits, uppercase| ColorFormat::Hex {
            syntax: HexSyntax::Css,
            digits,
            uppercase,
        };
        assert_eq!(
            presentations(rgba(255, 0, 0, 255), Some(hex(3, false))),
            vec![
                "#f00",
                "#ff0000",
                "rgb(255, 0, 0)",
                "hsl(0, 100%, 50%)",
                "red"
            ]
        );
        assert_eq!(
            presentations(rgba(255, 0, 0, 128), Some(hex(6, true))),
            vec![
                "#FF000080",
                "rgba(255, 0, 0, 0.502)",
                "hsla(0, 100%, 50%, 0.502)"
            ]
        );
        assert_eq!(
            presentations(rgba(0x12, 0x34, 0x56, 255), Some(hex(4, false)))[0],
            "#123456ff"
        );
        assert_eq!(
            presentations(
                rgba(0, 128, 0, 255),
                Some(ColorFormat::Rgb {
                    alpha_function: false,
                    legacy: false
                })
            ),
            vec![
                "rgb(0 128 0)",
                "#008000",
                "rgb(0, 128, 0)",
                "hsl(120, 100%, 25%)",
                "green"
            ]
        );
        assert_eq!(
            presentations(
                rgba(0, 0, 255, 64),
                Some(ColorFormat::Hsl {
                    alpha_function: false,
                    legacy: false
                })
            )[0],
            "hsl(240 100% 50% / 0.251)"
        );
        assert_eq!(
            presentations(
                rgba(0, 0, 255, 255),
                Some(ColorFormat::Rgb {
                    alpha_function: true,
                    legacy: true
                })
            )[0],
            "rgba(0, 0, 255, 1)"
        );
        // Modified named color: no exact name anymore, the other forms remain.
        assert_eq!(
            presentations(rgba(1, 2, 3, 255), Some(ColorFormat::Named)),
            vec!["#010203", "rgb(1, 2, 3)", "hsl(210, 50%, 1%)"]
        );
        assert_eq!(
            presentations(rgba(0, 0, 0, 0), Some(ColorFormat::Named))[0],
            "transparent"
        );
        assert_eq!(presentations(rgba(0, 255, 255, 255), None)[3], "aqua");
    }

    #[test]
    fn presents_android_colors_alpha_first() {
        let hex = |digits, uppercase| ColorFormat::Hex {
            syntax: HexSyntax::Android,
            digits,
            uppercase,
        };
        assert_eq!(
            presentations(rgba(255, 0, 0, 0x88), Some(hex(4, true))),
            vec!["#8F00", "#88FF0000"]
        );
        assert_eq!(
            presentations(rgba(0x62, 0, 0xee, 255), Some(hex(8, true))),
            vec!["#FF6200EE", "#6200EE"]
        );
        assert_eq!(
            presentations(rgba(0x62, 0, 0xee, 0x40), Some(hex(6, false))),
            vec!["#406200ee"]
        );
    }
}
