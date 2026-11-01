//! Key bindings a script declares with the `custom-keys` menu option.
//!
//! rofi sends `kb-custom-N` as `ROFI_RETV` 9+N and binds the keys on the
//! command line. Here the script names them along with its menu, so a
//! script that wants Alt+u to mean "copy the username" ships the binding
//! itself instead of asking every caller for a flag.

use smithay_client_toolkit::seat::keyboard::{Keysym, Modifiers};

/// The most `custom-keys` a menu may bind. The Nth sends `ROFI_RETV` 10+N,
/// and 20 already means a completion request.
pub const MAX_CUSTOM: usize = 9;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    ctrl: bool,
    alt: bool,
    shift: bool,
    logo: bool,
    key: Key,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    /// Lowercased, so Caps Lock and Shift do not change what matches.
    Char(char),
    Named(Keysym),
}

impl Binding {
    /// `Mod+...+Key`, modifiers out of `Control`/`Ctrl`, `Alt`, `Shift` and
    /// `Super`, the key a single character or `Return`, `Space` or `Tab`.
    pub fn parse(spec: &str) -> Option<Binding> {
        let mut parts: Vec<&str> = spec.trim().split('+').collect();
        let key = parts.pop()?;
        let mut binding = Binding {
            ctrl: false,
            alt: false,
            shift: false,
            logo: false,
            key: parse_key(key)?,
        };
        for part in parts {
            let flag = match part.to_ascii_lowercase().as_str() {
                "control" | "ctrl" => &mut binding.ctrl,
                "alt" => &mut binding.alt,
                "shift" => &mut binding.shift,
                "super" => &mut binding.logo,
                _ => return None,
            };
            *flag = true;
        }
        Some(binding)
    }

    /// Ctrl, Alt and Super must match exactly. Shift does too for letters and
    /// named keys; for other characters it is part of typing the character,
    /// so `Alt+!` matches whatever it takes to produce `!`.
    pub fn matches(&self, keysym: Keysym, modifiers: &Modifiers) -> bool {
        if self.ctrl != modifiers.ctrl || self.alt != modifiers.alt || self.logo != modifiers.logo {
            return false;
        }
        match self.key {
            Key::Named(named) => keysym == named && self.shift == modifiers.shift,
            Key::Char(want) => {
                let Some(got) = keysym.key_char().map(lower) else {
                    return false;
                };
                got == want && (!want.is_alphabetic() || self.shift == modifiers.shift)
            }
        }
    }
}

/// A `custom-keys` value: bindings separated by commas. Any that does not
/// parse makes the whole list empty, so a typo shows as keys that do
/// nothing rather than as keys shifted onto the wrong `ROFI_RETV`.
pub fn parse_list(value: &str) -> Vec<Binding> {
    let bindings: Option<Vec<Binding>> = value
        .split(',')
        .filter(|spec| !spec.trim().is_empty())
        .map(Binding::parse)
        .collect();
    let mut bindings = bindings.unwrap_or_default();
    bindings.truncate(MAX_CUSTOM);
    bindings
}

fn parse_key(key: &str) -> Option<Key> {
    let mut chars = key.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        return Some(Key::Char(lower(ch)));
    }
    let named = match key.to_ascii_lowercase().as_str() {
        "return" | "enter" => Keysym::Return,
        "space" => Keysym::space,
        "tab" => Keysym::Tab,
        _ => return None,
    };
    Some(Key::Named(named))
}

fn lower(ch: char) -> char {
    ch.to_lowercase().next().unwrap_or(ch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(ctrl: bool, alt: bool, shift: bool) -> Modifiers {
        Modifiers {
            ctrl,
            alt,
            shift,
            ..Modifiers::default()
        }
    }

    #[test]
    fn parses_modifiers_and_keys() {
        let binding = Binding::parse("Alt+Shift+p").unwrap();
        assert!(binding.alt && binding.shift && !binding.ctrl);
        assert_eq!(binding.key, Key::Char('p'));
        let binding = Binding::parse("ctrl+Return").unwrap();
        assert!(binding.ctrl);
        assert_eq!(binding.key, Key::Named(Keysym::Return));
    }

    #[test]
    fn rejects_unknown_parts() {
        assert_eq!(Binding::parse("Hyper+x"), None);
        assert_eq!(Binding::parse("Alt+"), None);
        assert_eq!(Binding::parse("Alt+Escape"), None);
    }

    #[test]
    fn letters_need_the_stated_shift() {
        let plain = Binding::parse("Alt+u").unwrap();
        let shifted = Binding::parse("Alt+Shift+u").unwrap();
        assert!(plain.matches(Keysym::u, &mods(false, true, false)));
        assert!(!plain.matches(Keysym::U, &mods(false, true, true)));
        assert!(shifted.matches(Keysym::U, &mods(false, true, true)));
        assert!(!shifted.matches(Keysym::u, &mods(false, true, false)));
        // Caps Lock uppercases the keysym without Shift.
        assert!(plain.matches(Keysym::U, &mods(false, true, false)));
    }

    #[test]
    fn other_modifiers_must_match_exactly() {
        let binding = Binding::parse("Alt+u").unwrap();
        assert!(!binding.matches(Keysym::u, &mods(false, false, false)));
        assert!(!binding.matches(Keysym::u, &mods(true, true, false)));
    }

    #[test]
    fn symbols_ignore_the_shift_that_types_them() {
        let binding = Binding::parse("Alt+!").unwrap();
        assert!(binding.matches(Keysym::exclam, &mods(false, true, true)));
    }

    #[test]
    fn one_bad_binding_empties_the_list() {
        assert_eq!(parse_list("Alt+u, Alt+p").len(), 2);
        assert!(parse_list("Alt+u,Hyper+p").is_empty());
        assert_eq!(parse_list(&"Alt+u,".repeat(12)).len(), MAX_CUSTOM);
    }
}
