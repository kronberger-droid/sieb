//! Appearance settings: built-in defaults, overridden by the config file,
//! overridden by flags. Every key exists in both places from one definition.

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use clap::Args;
use serde::Deserialize;

use crate::layout::Layout;
use crate::render::{Palette, Theme};

/// A color as `#rgb`, `#rrggbb` or `#rrggbbaa`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba(pub [u8; 4]);

impl FromStr for Rgba {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || format!("invalid color {s:?}, expected #rgb, #rrggbb or #rrggbbaa");
        let hex = s.strip_prefix('#').ok_or_else(invalid)?;
        if !hex.is_ascii() {
            return Err(invalid());
        }
        let byte = |i: usize, len: usize| u8::from_str_radix(&hex[i..i + len], 16);
        let rgba = match hex.len() {
            3 => [0, 1, 2, 3].map(|i| if i < 3 { byte(i, 1).map(|v| v * 0x11) } else { Ok(0xff) }),
            6 | 8 => [0, 2, 4, 6].map(|i| if i < hex.len() { byte(i, 2) } else { Ok(0xff) }),
            _ => return Err(invalid()),
        };
        let mut out = [0xff; 4];
        for (slot, value) in out.iter_mut().zip(rgba) {
            *slot = value.map_err(|_| invalid())?;
        }
        Ok(Rgba(out))
    }
}

impl<'de> Deserialize<'de> for Rgba {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Args, Deserialize, Debug, Default, Clone, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
#[command(next_help_heading = "Appearance")]
pub struct Appearance {
    /// Font family, resolved through fontconfig [default: sans-serif]
    #[arg(long)]
    pub font: Option<String>,

    /// Font size in logical pixels [default: 15]
    #[arg(long)]
    pub font_size: Option<f32>,

    /// Number of visible lines [default: 10]
    #[arg(short, long)]
    pub lines: Option<u32>,

    /// Panel width in logical pixels [default: 640]
    #[arg(long)]
    pub width: Option<u32>,

    /// Space around the content [default: 10]
    #[arg(long)]
    pub padding: Option<f32>,

    /// Gap between the input, separator, message and list [default: 3]
    #[arg(long)]
    pub spacing: Option<f32>,

    /// Corner radius [default: 12]
    #[arg(long)]
    pub radius: Option<f32>,

    /// Border width, 0 for none [default: 1]
    #[arg(long)]
    pub border_width: Option<f32>,

    /// Height of the line under the input, 0 for none [default: 1]
    #[arg(long)]
    pub separator_width: Option<f32>,

    /// Room around the prompt, badge and query [default: 6.5]
    #[arg(long)]
    pub input_padding: Option<f32>,

    /// Room around the text of each row [default: 6.5]
    #[arg(long)]
    pub row_padding: Option<f32>,

    /// Gap between rows [default: 0]
    #[arg(long)]
    pub row_spacing: Option<f32>,

    /// Corner radius of rows, the message and the pills [default: 6]
    #[arg(long)]
    pub row_radius: Option<f32>,

    /// Room around the message text [default: 6.5]
    #[arg(long)]
    pub message_padding: Option<f32>,

    /// Text shown while the query is empty
    #[arg(long, value_name = "TEXT")]
    pub placeholder: Option<String>,

    /// Text of a pill left of the prompt, such as a glyph
    #[arg(long, value_name = "TEXT")]
    pub badge: Option<String>,

    /// Show the match counter [default: true]
    #[arg(long, value_name = "BOOL")]
    pub counter: Option<bool>,

    /// Show a scrollbar right of the list [default: false]
    #[arg(long, value_name = "BOOL")]
    pub scrollbar: Option<bool>,

    #[command(flatten)]
    #[serde(default)]
    pub colors: Colors,
}

/// Colors left unset fall back to a related one, noted in brackets, so a
/// theme can stay short.
#[derive(Args, Deserialize, Debug, Default, Clone, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
#[command(next_help_heading = "Colors")]
pub struct Colors {
    /// Panel background
    #[arg(long = "color-background", value_name = "COLOR")]
    pub background: Option<Rgba>,
    /// Panel border
    #[arg(long = "color-border", value_name = "COLOR")]
    pub border: Option<Rgba>,
    /// Selected row
    #[arg(long = "color-selected", value_name = "COLOR")]
    pub selected: Option<Rgba>,
    /// Text of the selected row [text]
    #[arg(long = "color-selected-text", value_name = "COLOR")]
    pub selected_text: Option<Rgba>,
    /// Line between input and list
    #[arg(long = "color-separator", value_name = "COLOR")]
    pub separator: Option<Rgba>,
    /// Entries and query
    #[arg(long = "color-text", value_name = "COLOR")]
    pub text: Option<Rgba>,
    /// Match counter
    #[arg(long = "color-dim", value_name = "COLOR")]
    pub dim: Option<Rgba>,
    /// Caret, and what the colors below fall back to
    #[arg(long = "color-accent", value_name = "COLOR")]
    pub accent: Option<Rgba>,
    /// Matched characters [accent]
    #[arg(long = "color-match", id = "color-match", value_name = "COLOR")]
    #[serde(rename = "match")]
    pub matched: Option<Rgba>,
    /// Matched characters on the selected row [match]
    #[arg(long = "color-selected-match", value_name = "COLOR")]
    pub selected_match: Option<Rgba>,
    /// Behind the other rows [none]
    #[arg(long = "color-row", id = "color-row", value_name = "COLOR")]
    pub row: Option<Rgba>,
    /// Placeholder text [dim]
    #[arg(long = "color-placeholder", id = "color-placeholder", value_name = "COLOR")]
    pub placeholder: Option<Rgba>,
    /// Prompt text [accent]
    #[arg(long = "color-prompt", id = "color-prompt", value_name = "COLOR")]
    pub prompt: Option<Rgba>,
    /// Behind the prompt [none]
    #[arg(long = "color-prompt-background", value_name = "COLOR")]
    pub prompt_background: Option<Rgba>,
    /// Badge text [background]
    #[arg(long = "color-badge", id = "color-badge", value_name = "COLOR")]
    pub badge: Option<Rgba>,
    /// Behind the badge [accent]
    #[arg(long = "color-badge-background", value_name = "COLOR")]
    pub badge_background: Option<Rgba>,
    /// Message text [dim]
    #[arg(long = "color-message", id = "color-message", value_name = "COLOR")]
    pub message: Option<Rgba>,
    /// Behind the message [none]
    #[arg(long = "color-message-background", value_name = "COLOR")]
    pub message_background: Option<Rgba>,
    /// Scrollbar track [separator]
    #[arg(long = "color-scrollbar", id = "color-scrollbar", value_name = "COLOR")]
    pub scrollbar: Option<Rgba>,
    /// Scrollbar handle [dim]
    #[arg(long = "color-scrollbar-handle", value_name = "COLOR")]
    pub scrollbar_handle: Option<Rgba>,
    /// Mode buttons [scrollbar]
    #[arg(long = "color-button", id = "color-button", value_name = "COLOR")]
    pub button: Option<Rgba>,
    /// Mode button text [text]
    #[arg(long = "color-button-text", value_name = "COLOR")]
    pub button_text: Option<Rgba>,
    /// The button of the mode on screen [accent]
    #[arg(long = "color-button-selected", value_name = "COLOR")]
    pub button_selected: Option<Rgba>,
    /// Its text [background, opaque]
    #[arg(long = "color-button-selected-text", value_name = "COLOR")]
    pub button_selected_text: Option<Rgba>,
    /// Tint over the rest of the output, #00000000 for none
    #[arg(long = "color-backdrop", value_name = "COLOR")]
    pub backdrop: Option<Rgba>,
}

/// Fills every field `self` leaves unset from `fallback`.
macro_rules! merge {
    ($self:ident, $fallback:ident, $($field:ident),*) => {
        $( $self.$field = $self.$field.take().or($fallback.$field); )*
    };
}

impl Appearance {
    /// Flags win over the file.
    pub fn over(mut self, file: Appearance) -> Appearance {
        merge!(
            self,
            file,
            font,
            font_size,
            lines,
            width,
            padding,
            spacing,
            radius,
            border_width,
            separator_width,
            input_padding,
            row_padding,
            row_spacing,
            row_radius,
            message_padding,
            placeholder,
            badge,
            counter,
            scrollbar
        );
        let (mut colors, file) = (self.colors, file.colors);
        merge!(
            colors,
            file,
            background,
            border,
            selected,
            selected_text,
            separator,
            text,
            dim,
            accent,
            matched,
            selected_match,
            row,
            placeholder,
            prompt,
            prompt_background,
            badge,
            badge_background,
            message,
            message_background,
            scrollbar,
            scrollbar_handle,
            button,
            button_text,
            button_selected,
            button_selected_text,
            backdrop
        );
        self.colors = colors;
        self
    }

    /// Rejects values that would break drawing. Runs on the merged result,
    /// so a value from a flag and one from the file get the same rule.
    fn validate(&self) -> Result<(), String> {
        let positive = [
            ("font-size", self.font_size),
            ("width", self.width.map(|v| v as f32)),
            ("lines", self.lines.map(|v| v as f32)),
        ];
        let non_negative = [
            ("padding", self.padding),
            ("spacing", self.spacing),
            ("radius", self.radius),
            ("border-width", self.border_width),
            ("separator-width", self.separator_width),
            ("input-padding", self.input_padding),
            ("row-padding", self.row_padding),
            ("row-spacing", self.row_spacing),
            ("row-radius", self.row_radius),
            ("message-padding", self.message_padding),
        ];
        for (key, value) in positive {
            if let Some(v) = value
                && !(v.is_finite() && v > 0.0)
            {
                return Err(format!("{key} must be greater than 0, got {v}"));
            }
        }
        for (key, value) in non_negative {
            if let Some(v) = value
                && !(v.is_finite() && v >= 0.0)
            {
                return Err(format!("{key} must not be negative, got {v}"));
            }
        }
        Ok(())
    }

    /// Validates and fills the rest from the built-in defaults.
    pub fn resolve(self) -> Result<(String, Layout, Theme), String> {
        self.validate()?;
        let d = Layout::default();
        let layout = Layout {
            width: self.width.map_or(d.width, |w| w as f32),
            padding: self.padding.unwrap_or(d.padding),
            font_size: self.font_size.unwrap_or(d.font_size),
            spacing: self.spacing.unwrap_or(d.spacing),
            separator: self.separator_width.unwrap_or(d.separator),
            input_padding: self.input_padding.unwrap_or(d.input_padding),
            row_padding: self.row_padding.unwrap_or(d.row_padding),
            row_spacing: self.row_spacing.unwrap_or(d.row_spacing),
            message_padding: self.message_padding.unwrap_or(d.message_padding),
            lines: self.lines.unwrap_or(d.lines),
            message: false,
            buttons: 0,
        };

        let d = Palette::default();
        let c = self.colors;
        let pick = |color: Option<Rgba>, default: [u8; 4]| color.map_or(default, |c| c.0);
        let background = pick(c.background, d.background);
        let text = pick(c.text, d.text);
        let dim = pick(c.dim, d.dim);
        let accent = pick(c.accent, d.accent);
        let separator = pick(c.separator, d.separator);
        let matched = pick(c.matched, accent);
        let scrollbar = pick(c.scrollbar, separator);
        // The badge is text on an accent pill, so it defaults to the panel
        // color, opaque.
        let [r, g, b, _] = background;
        let colors = Palette {
            background,
            border: pick(c.border, d.border),
            selected: pick(c.selected, d.selected),
            selected_text: pick(c.selected_text, text),
            separator,
            text,
            dim,
            accent,
            matched,
            selected_match: pick(c.selected_match, matched),
            row: pick(c.row, d.row),
            placeholder: pick(c.placeholder, dim),
            prompt: pick(c.prompt, accent),
            prompt_background: pick(c.prompt_background, d.prompt_background),
            badge: pick(c.badge, [r, g, b, 0xff]),
            badge_background: pick(c.badge_background, accent),
            message: pick(c.message, dim),
            message_background: pick(c.message_background, d.message_background),
            scrollbar,
            scrollbar_handle: pick(c.scrollbar_handle, dim),
            button: pick(c.button, scrollbar),
            button_text: pick(c.button_text, text),
            button_selected: pick(c.button_selected, accent),
            button_selected_text: pick(c.button_selected_text, [r, g, b, 0xff]),
            backdrop: pick(c.backdrop, d.backdrop),
        };
        let d = Theme::default();
        let theme = Theme {
            colors,
            radius: self.radius.unwrap_or(d.radius),
            border_width: self.border_width.unwrap_or(d.border_width),
            row_radius: self.row_radius.unwrap_or(d.row_radius),
            placeholder: self.placeholder.unwrap_or(d.placeholder),
            badge: self.badge.unwrap_or(d.badge),
            counter: self.counter.unwrap_or(d.counter),
            scrollbar: self.scrollbar.unwrap_or(d.scrollbar),
        };
        Ok((self.font.unwrap_or_else(|| "sans-serif".into()), layout, theme))
    }
}

/// Reads the config file. An explicit `path` has to exist; the default
/// location is optional.
pub fn load(path: Option<&Path>) -> Result<Appearance, Box<dyn Error>> {
    let (path, required) = match path {
        Some(path) => (path.to_owned(), true),
        None => match default_path() {
            Some(path) => (path, false),
            None => return Ok(Appearance::default()),
        },
    };
    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(err) if !required && err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Appearance::default());
        }
        Err(err) => return Err(ConfigError(path, err.to_string()).into()),
    };
    toml::from_str(&source).map_err(|err| ConfigError(path, err.to_string()).into())
}

fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("sieb/config.toml"))
}

#[derive(Debug)]
struct ConfigError(PathBuf, String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}: {}", self.0.display(), self.1)
    }
}

impl Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_forms() {
        assert_eq!("#f0a".parse(), Ok(Rgba([0xff, 0x00, 0xaa, 0xff])));
        assert_eq!("#1e1e2e".parse(), Ok(Rgba([0x1e, 0x1e, 0x2e, 0xff])));
        assert_eq!("#00000040".parse(), Ok(Rgba([0, 0, 0, 0x40])));
        for bad in ["1e1e2e", "#12345", "#gggggg", "#ééé"] {
            assert!(bad.parse::<Rgba>().is_err(), "{bad}");
        }
    }

    #[test]
    fn flags_override_file_override_defaults() {
        let file: Appearance = toml::from_str(
            r##"
            lines = 5
            font-size = 18
            [colors]
            accent = "#ff0000"
            border = "#00ff00"
            "##,
        )
        .unwrap();
        let flags = Appearance {
            lines: Some(7),
            colors: Colors {
                accent: Some(Rgba([0, 0, 0xff, 0xff])),
                ..Colors::default()
            },
            ..Appearance::default()
        };
        let (font, layout, theme) = flags.over(file).resolve().unwrap();
        assert_eq!(font, "sans-serif");
        assert_eq!(layout.lines, 7);
        assert_eq!(layout.font_size, 18.0);
        assert_eq!(theme.colors.accent, [0, 0, 0xff, 0xff]);
        assert_eq!(theme.colors.border, [0, 0xff, 0, 0xff]);
        assert_eq!(theme.colors.background, Palette::default().background);
    }

    #[test]
    fn unset_colors_follow_their_fallback() {
        let file: Appearance = toml::from_str(
            r##"
            [colors]
            accent = "#ff0000"
            text = "#00ff00"
            match = "#0000ff"
            "##,
        )
        .unwrap();
        let (_, _, theme) = file.resolve().unwrap();
        let c = theme.colors;
        assert_eq!(c.prompt, [0xff, 0, 0, 0xff]);
        assert_eq!(c.selected_text, [0, 0xff, 0, 0xff]);
        assert_eq!(c.selected_match, [0, 0, 0xff, 0xff]);
    }

    #[test]
    fn bad_numbers_name_their_key() {
        for (source, key) in [
            ("width = 0", "width"),
            ("font-size = 0", "font-size"),
            ("font-size = nan", "font-size"),
            ("lines = 0", "lines"),
            ("padding = -1", "padding"),
            ("radius = -3", "radius"),
        ] {
            let file: Appearance = toml::from_str(source).unwrap();
            let err = file.resolve().unwrap_err();
            assert!(err.starts_with(key), "{source}: {err}");
        }
        // Zero is fine where it means "none".
        let file: Appearance = toml::from_str("border-width = 0\npadding = 0").unwrap();
        assert!(file.resolve().is_ok());
    }

    #[test]
    fn unknown_keys_are_errors() {
        let err = toml::from_str::<Appearance>("font-szie = 12").unwrap_err();
        assert!(err.to_string().contains("font-szie"), "{err}");
        assert!(toml::from_str::<Appearance>("[colors]\naccnet = \"#fff\"").is_err());
    }

    #[test]
    fn example_themes_resolve() {
        for theme in [
            include_str!("../examples/launcher/launcher.toml"),
            include_str!("../examples/power/power.toml"),
        ] {
            let file: Appearance = toml::from_str(theme).unwrap();
            file.resolve().unwrap();
        }
    }

    #[test]
    fn example_config_matches_defaults() {
        let file: Appearance =
            toml::from_str(include_str!("../examples/config.toml")).unwrap();
        let (font, layout, theme) = file.resolve().unwrap();
        let (dfont, dlayout, dtheme) = Appearance::default().resolve().unwrap();
        assert_eq!(font, dfont);
        assert_eq!(layout.size(), dlayout.size());
        assert_eq!(theme, dtheme);
    }
}
