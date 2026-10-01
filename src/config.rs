//! Appearance settings: built-in defaults, overridden by the config file,
//! overridden by flags. Every key exists in both places from one definition.

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use clap::Args;
use serde::Deserialize;

use crate::layout::Layout;
use crate::render::Theme;

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

    /// Corner radius [default: 12]
    #[arg(long)]
    pub radius: Option<f32>,

    /// Border width, 0 for none [default: 1]
    #[arg(long)]
    pub border_width: Option<f32>,

    #[command(flatten)]
    #[serde(default)]
    pub colors: Colors,
}

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
    /// Line between input and list
    #[arg(long = "color-separator", value_name = "COLOR")]
    pub separator: Option<Rgba>,
    /// Entries and query
    #[arg(long = "color-text", value_name = "COLOR")]
    pub text: Option<Rgba>,
    /// Match counter
    #[arg(long = "color-dim", value_name = "COLOR")]
    pub dim: Option<Rgba>,
    /// Prompt, caret and matched characters
    #[arg(long = "color-accent", value_name = "COLOR")]
    pub accent: Option<Rgba>,
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
        merge!(self, file, font, font_size, lines, width, padding, radius, border_width);
        let (mut colors, file) = (self.colors, file.colors);
        merge!(colors, file, background, border, selected, separator, text, dim, accent, backdrop);
        self.colors = colors;
        self
    }

    /// Fills the rest from the built-in defaults.
    pub fn resolve(self) -> (String, Layout, Theme) {
        let layout = Layout::new(
            self.width.unwrap_or(640),
            self.lines.unwrap_or(10),
            self.font_size.unwrap_or(15.0),
            self.padding.unwrap_or(10.0),
        );
        let d = Theme::default();
        let c = self.colors;
        let pick = |color: Option<Rgba>, default: [u8; 4]| color.map_or(default, |c| c.0);
        let theme = Theme {
            background: pick(c.background, d.background),
            border: pick(c.border, d.border),
            selected: pick(c.selected, d.selected),
            separator: pick(c.separator, d.separator),
            text: pick(c.text, d.text),
            dim: pick(c.dim, d.dim),
            accent: pick(c.accent, d.accent),
            backdrop: pick(c.backdrop, d.backdrop),
            radius: self.radius.unwrap_or(d.radius),
            border_width: self.border_width.unwrap_or(d.border_width),
        };
        (self.font.unwrap_or_else(|| "sans-serif".into()), layout, theme)
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
        let (font, layout, theme) = flags.over(file).resolve();
        assert_eq!(font, "sans-serif");
        assert_eq!(layout.lines, 7);
        assert_eq!(layout.font_size, 18.0);
        assert_eq!(theme.accent, [0, 0, 0xff, 0xff]);
        assert_eq!(theme.border, [0, 0xff, 0, 0xff]);
        assert_eq!(theme.background, Theme::default().background);
    }

    #[test]
    fn unknown_keys_are_errors() {
        let err = toml::from_str::<Appearance>("font-szie = 12").unwrap_err();
        assert!(err.to_string().contains("font-szie"), "{err}");
        assert!(toml::from_str::<Appearance>("[colors]\naccnet = \"#fff\"").is_err());
    }

    #[test]
    fn example_config_matches_defaults() {
        let file: Appearance =
            toml::from_str(include_str!("../contrib/config.toml")).unwrap();
        let (font, layout, theme) = file.resolve();
        let (dfont, dlayout, dtheme) = Appearance::default().resolve();
        assert_eq!(font, dfont);
        assert_eq!(layout.size(), dlayout.size());
        assert_eq!(theme, dtheme);
    }
}
