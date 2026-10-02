//! The part of Pango markup that rofi scripts use for rows, enabled with
//! the `markup-rows` option: `<b>`, `<i>`, `<small>`, `<big>` and `<span>`
//! with weight, style, size, color and alpha, plus XML entities.
//!
//! Rows are matched on the text without tags, so typing "web" finds
//! `Firefox <i>(Web Browser)</i>`.

use std::ops::Range;

use crate::config::Rgba;

/// How a stretch of text is drawn. Unset fields keep the row's own look.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    pub italic: bool,
    /// OpenType weight, 400 is regular.
    pub weight: u16,
    /// Font size relative to the row's.
    pub scale: f32,
    pub color: Option<[u8; 3]>,
    /// Multiplies the color's alpha.
    pub alpha: Option<u8>,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            italic: false,
            weight: 400,
            scale: 1.0,
            color: None,
            alpha: None,
        }
    }
}

/// Text without tags, and the styled byte ranges of it. Text outside every
/// range uses [`Style::default`].
#[derive(Debug, PartialEq)]
pub struct Markup {
    pub text: String,
    pub spans: Vec<(Range<usize>, Style)>,
}

/// Parses `source`. Markup that does not parse is shown as it is, tags and
/// all, rather than losing the row.
pub fn parse(source: &str) -> Markup {
    try_parse(source).unwrap_or_else(|| Markup {
        text: source.to_owned(),
        spans: Vec::new(),
    })
}

fn try_parse(source: &str) -> Option<Markup> {
    let mut text = String::new();
    let mut spans: Vec<(Range<usize>, Style)> = Vec::new();
    // Open tags with the style inside them.
    let mut stack: Vec<(String, Style)> = Vec::new();
    let mut rest = source;

    let mut push_text = |text: &mut String, piece: &str, style: Style| {
        if piece.is_empty() {
            return;
        }
        let start = text.len();
        text.push_str(piece);
        if style == Style::default() {
            return;
        }
        // Neighbours with the same style become one span.
        match spans.last_mut() {
            Some((range, last)) if range.end == start && *last == style => range.end = text.len(),
            _ => spans.push((start..text.len(), style)),
        }
    };

    while !rest.is_empty() {
        let style = stack.last().map_or_else(Style::default, |(_, style)| *style);
        let next = rest.find(['<', '&']).unwrap_or(rest.len());
        push_text(&mut text, &rest[..next], style);
        rest = &rest[next..];
        if let Some(entity) = rest.strip_prefix('&') {
            let end = entity.find(';')?;
            let decoded = decode_entity(&entity[..end])?;
            push_text(&mut text, decoded.encode_utf8(&mut [0; 4]), style);
            rest = &entity[end + 1..];
        } else if let Some(tag) = rest.strip_prefix('<') {
            let end = tag.find('>')?;
            let tag_body = &tag[..end];
            rest = &tag[end + 1..];
            if let Some(name) = tag_body.strip_prefix('/') {
                let (open, _) = stack.pop()?;
                if open != name.trim() {
                    return None;
                }
            } else {
                let (name, attributes) = tag_body
                    .split_once(char::is_whitespace)
                    .unwrap_or((tag_body, ""));
                let inner = open_tag(name, attributes, style)?;
                stack.push((name.to_owned(), inner));
            }
        }
    }
    stack.is_empty().then_some(Markup { text, spans })
}

/// The style inside `<name attributes>`, opened where `outer` applies.
fn open_tag(name: &str, attributes: &str, outer: Style) -> Option<Style> {
    let mut style = outer;
    match name {
        "b" => style.weight = 700,
        "i" => style.italic = true,
        "small" => style.scale *= SMALLER,
        "big" => style.scale /= SMALLER,
        // Drawn plain: underline, strikethrough, monospace, sub/superscript.
        "u" | "s" | "tt" | "sub" | "sup" => {}
        "span" => {
            for (key, value) in attribute_pairs(attributes)? {
                apply(&mut style, outer, key, value);
            }
        }
        _ => return None,
    }
    Some(style)
}

/// Pango's ratio between neighbouring named sizes.
const SMALLER: f32 = 1.0 / 1.2;

/// `key='value' key="value"` pairs.
fn attribute_pairs(mut s: &str) -> Option<Vec<(&str, &str)>> {
    let mut pairs = Vec::new();
    loop {
        s = s.trim_start();
        if s.is_empty() {
            return Some(pairs);
        }
        let (key, after) = s.split_once('=')?;
        let after = after.trim_start();
        let quote = after.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let after = &after[1..];
        let end = after.find(quote)?;
        pairs.push((key.trim(), &after[..end]));
        s = &after[end + 1..];
    }
}

/// Applies one `<span>` attribute. Values sieb cannot draw are ignored
/// rather than failing the row.
fn apply(style: &mut Style, outer: Style, key: &str, value: &str) {
    match key {
        "weight" | "font_weight" => {
            if let Some(weight) = weight(value) {
                style.weight = weight;
            }
        }
        "style" | "font_style" => style.italic = matches!(value, "italic" | "oblique"),
        "size" | "font_size" => {
            if let Some(scale) = scale(value, outer.scale) {
                style.scale = scale;
            }
        }
        // The same forms a config color takes; `#rrggbbaa` carries an alpha.
        "foreground" | "fgcolor" | "color" => {
            if let Ok(Rgba([r, g, b, a])) = value.parse() {
                style.color = Some([r, g, b]);
                if a != 0xff {
                    style.alpha = Some(a);
                }
            }
        }
        "alpha" | "fgalpha" => style.alpha = alpha(value),
        _ => {}
    }
}

fn weight(value: &str) -> Option<u16> {
    Some(match value {
        "thin" => 100,
        "ultralight" => 200,
        "light" => 300,
        "semilight" | "book" => 350,
        "normal" => 400,
        "medium" => 500,
        "semibold" => 600,
        "bold" => 700,
        "ultrabold" => 800,
        "heavy" => 900,
        "ultraheavy" => 1000,
        number => number.parse().ok()?,
    })
}

/// Named and relative sizes. Absolute sizes in points would need the
/// row's size in points, so they are left alone.
fn scale(value: &str, outer: f32) -> Option<f32> {
    let named = |steps: i32| SMALLER.powi(-steps);
    Some(match value {
        "xx-small" => named(-3),
        "x-small" => named(-2),
        "small" => named(-1),
        "medium" => 1.0,
        "large" => named(1),
        "x-large" => named(2),
        "xx-large" => named(3),
        "smaller" => outer * SMALLER,
        "larger" => outer / SMALLER,
        percent => percent.strip_suffix('%')?.parse::<f32>().ok()? / 100.0,
    })
}

/// `50%`, or Pango's 1 to 65535.
fn alpha(value: &str) -> Option<u8> {
    let fraction = match value.strip_suffix('%') {
        Some(percent) => percent.parse::<f32>().ok()? / 100.0,
        None => value.parse::<f32>().ok()? / 65535.0,
    };
    Some((fraction.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn decode_entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_has_no_spans() {
        assert_eq!(
            parse("Firefox"),
            Markup {
                text: "Firefox".into(),
                spans: vec![]
            }
        );
    }

    #[test]
    fn drun_display_format() {
        // What rofi's drun-display-format in the launcher theme produces.
        let m = parse("Firefox <span weight='light' size='small'><i>(Web Browser)</i></span>");
        assert_eq!(m.text, "Firefox (Web Browser)");
        let [(range, style)] = &m.spans[..] else { panic!("{m:?}") };
        assert_eq!(&m.text[range.clone()], "(Web Browser)");
        assert!(style.italic);
        assert_eq!(style.weight, 300);
        assert!((style.scale - 1.0 / 1.2).abs() < 1e-6);
    }

    #[test]
    fn entities() {
        assert_eq!(parse("Tom &amp; Jerry &lt;3 &#x41;&#66;").text, "Tom & Jerry <3 AB");
    }

    #[test]
    fn colors_and_alpha() {
        let m = parse("<span foreground=\"#f00\" alpha=\"50%\">x</span>");
        assert_eq!(m.spans[0].1.color, Some([0xff, 0, 0]));
        assert_eq!(m.spans[0].1.alpha, Some(128));
    }

    #[test]
    fn nesting_restores_the_outer_style() {
        let m = parse("<b>a<i>b</i>c</b>d");
        assert_eq!(m.text, "abcd");
        let styles: Vec<_> = m.spans.iter().map(|(r, s)| (&m.text[r.clone()], s.weight, s.italic)).collect();
        assert_eq!(styles, [("a", 700, false), ("b", 700, true), ("c", 700, false)]);
    }

    #[test]
    fn broken_markup_shows_as_is() {
        for source in ["<b>open", "a < b", "x</i>", "<blink>y</blink>", "&nope;", "<b>a</i>"] {
            assert_eq!(parse(source).text, source);
        }
    }
}
