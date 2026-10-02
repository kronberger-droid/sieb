//! The line formats rows arrive in: plain text, rofi's in-band options, and
//! JSON. Shared by stdin and scripts.

use std::io::{self, BufRead};
use std::ops::Range;

use serde_json::{Map, Value};

use crate::markup::{self, Style};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// dmenu: every line is a row, taken literally.
    Plain,
    /// rofi's script protocol. A first line of `{"sieb": 1}` switches the
    /// rest of the output to JSON.
    Rofi,
    /// One JSON value per line, or a single top-level array.
    Json,
}

/// One unit of input.
#[derive(Debug, PartialEq)]
pub enum Line {
    /// An option for the whole menu, like the prompt.
    Mode(String, String),
    Row(Row),
}

#[derive(Debug, PartialEq)]
pub struct Row {
    pub text: String,
    pub selectable: bool,
    /// What plain lines never have, boxed so a large stdin does not pay
    /// for four empty fields on every row.
    extra: Option<Box<Extra>>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Extra {
    /// Handed back to the script in `ROFI_INFO` when this row is picked.
    pub info: Option<String>,
    /// Extra search terms, matched but not shown.
    pub meta: Option<String>,
    /// The JSON value this row came from, printed back on selection so
    /// fields sieb does not know about survive the round trip.
    pub raw: Option<String>,
    /// Styled byte ranges of `text`, from markup.
    pub styles: Vec<(Range<usize>, Style)>,
}

impl Row {
    pub fn plain(text: String) -> Self {
        Self {
            text,
            selectable: true,
            extra: None,
        }
    }

    pub fn info(&self) -> Option<&str> {
        self.extra.as_ref()?.info.as_deref()
    }

    pub fn meta(&self) -> Option<&str> {
        self.extra.as_ref()?.meta.as_deref()
    }

    pub fn raw(&self) -> Option<&str> {
        self.extra.as_ref()?.raw.as_deref()
    }

    pub fn styles(&self) -> &[(Range<usize>, Style)] {
        self.extra.as_ref().map_or(&[], |extra| &extra.styles)
    }

    /// The extras, created on first use.
    pub fn extra_mut(&mut self) -> &mut Extra {
        self.extra.get_or_insert_default()
    }

    /// Reads the text as markup: tags become styles, entities characters.
    fn with_markup(mut self) -> Self {
        let markup = markup::parse(&self.text);
        self.text = markup.text;
        if !markup.spans.is_empty() {
            self.extra_mut().styles = markup.spans;
        }
        self
    }
}

/// Reads `reader` to the end, handing each parsed line to `sink`.
///
/// `field` names the key of a JSON record to show as its text, for input
/// that was not written with sieb in mind (`ls | to json -r`).
///
/// Lines that are not valid UTF-8 are kept with replacement characters
/// rather than ending the stream, since one odd filename in `fd | sieb`
/// should not truncate the list.
///
/// A `markup-rows` option turns on markup for the rows after it, the way
/// a rofi script sets it before printing them.
pub fn feed<R: BufRead>(
    mut reader: R,
    mut format: Format,
    field: Option<&str>,
    mut out: impl FnMut(Line),
) -> io::Result<()> {
    let mut markup = false;
    let mut sink = move |line: Line| match line {
        Line::Mode(key, value) => {
            if key == "markup-rows" {
                markup = value == "true";
            }
            out(Line::Mode(key, value));
        }
        Line::Row(row) if markup => out(Line::Row(row.with_markup())),
        row => out(row),
    };
    if format == Format::Json && starts_with(&mut reader, b'[')? {
        // nu's `to json` writes a table as one array, not one line per row.
        let values: Vec<Value> = serde_json::from_reader(reader).map_err(io::Error::other)?;
        values
            .into_iter()
            .flat_map(|value| from_value(value, field))
            .for_each(&mut sink);
        return Ok(());
    }

    let mut buf = Vec::new();
    let mut first = true;
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            return Ok(());
        }
        let line = buf.strip_suffix(b"\n").unwrap_or(&buf);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let text = String::from_utf8_lossy(line);
        if std::mem::take(&mut first) && format == Format::Rofi && is_json_header(&text) {
            format = Format::Json;
        }
        match format {
            Format::Plain => sink(Line::Row(Row::plain(text.into_owned()))),
            Format::Rofi => sink(parse_rofi(&text)),
            Format::Json if text.trim().is_empty() => {}
            Format::Json => match serde_json::from_str::<Value>(&text) {
                Ok(value) => from_value(value, field).into_iter().for_each(&mut sink),
                // Keep the line visible rather than dropping input.
                Err(_) => sink(Line::Row(Row::plain(text.into_owned()))),
            },
        }
    }
}

/// Peeks past leading whitespace for `byte`, without consuming it.
fn starts_with<R: BufRead>(reader: &mut R, byte: u8) -> io::Result<bool> {
    loop {
        let buf = reader.fill_buf()?;
        let Some(pos) = buf.iter().position(|b| !b.is_ascii_whitespace()) else {
            if buf.is_empty() {
                return Ok(false);
            }
            let len = buf.len();
            reader.consume(len);
            continue;
        };
        let found = buf[pos] == byte;
        reader.consume(pos);
        return Ok(found);
    }
}

fn is_json_header(line: &str) -> bool {
    line.starts_with('{')
        && serde_json::from_str::<Map<String, Value>>(line).is_ok_and(|m| m.contains_key("sieb"))
}

/// Splits rofi's in-band options off a line: `text\0key\x1fvalue\x1f...`,
/// or `\0key\x1fvalue` for a menu option.
pub fn parse_rofi(line: &str) -> Line {
    let (text, options) = line.split_once('\0').unwrap_or((line, ""));
    let mut pairs = options.split('\x1f');
    if text.is_empty() && !options.is_empty() {
        let key = pairs.next().unwrap_or_default().to_owned();
        let value = pairs.collect::<Vec<_>>().join("\x1f");
        return Line::Mode(key, value);
    }
    let mut row = Row::plain(text.to_owned());
    while let Some(key) = pairs.next() {
        let value = pairs.next().unwrap_or_default();
        match key {
            "info" => row.extra_mut().info = Some(value.to_owned()),
            "meta" => row.extra_mut().meta = Some(value.to_owned()),
            "nonselectable" => row.selectable = value != "true",
            // icon, urgent, active, permanent: not supported yet.
            _ => {}
        }
    }
    Line::Row(row)
}

/// Keys of an object that sets menu options instead of being a row.
const MENU_OPTIONS: &[&str] = &[
    "sieb",
    "prompt",
    "message",
    "no-custom",
    "keep-filter",
    "data",
    "new-selection",
    "markup-rows",
];

/// An object with `text` is a row; one made of option keys sets menu
/// options; a bare string or number is a row of just that.
///
/// `field` names the key shown as a record's text in place of `text`.
fn from_value(value: Value, field: Option<&str>) -> Vec<Line> {
    let mut object = match value {
        Value::Object(object) => object,
        Value::Null => return vec![],
        // Printed back as the same JSON value it came in as.
        scalar => {
            let raw = scalar.to_string();
            let mut row = Row::plain(as_string(scalar));
            row.extra_mut().raw = Some(raw);
            return vec![Line::Row(row)];
        }
    };
    // Only an object made of nothing but option keys sets options. Any other
    // object is a row, or `ls | to json` would vanish into ignored options.
    let shown = field.unwrap_or("text");
    if !object.contains_key(shown) && object.keys().all(|key| MENU_OPTIONS.contains(&key.as_str())) {
        object.remove("sieb");
        return object
            .into_iter()
            .map(|(key, value)| Line::Mode(key, as_string(value)))
            .collect();
    }
    // A record missing the named field still shows up, as its whole JSON.
    let text = match object.get(shown).or_else(|| object.get("text")) {
        Some(text) => as_string(text.clone()),
        None => Value::Object(object.clone()).to_string(),
    };
    let mut row = Row::plain(text);
    row.selectable = object.get("selectable") != Some(&Value::Bool(false));
    *row.extra_mut() = Extra {
        info: object.get("info").cloned().map(as_string),
        meta: object.get("meta").cloned().map(as_string),
        raw: Some(Value::Object(object).to_string()),
        styles: Vec::new(),
    };
    vec![Line::Row(row)]
}

/// Strings as they are, anything else as its JSON text.
fn as_string(value: Value) -> String {
    match value {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(input: &str, format: Format) -> Vec<Line> {
        let mut lines = Vec::new();
        feed(input.as_bytes(), format, None, |line| lines.push(line)).unwrap();
        lines
    }

    fn row(text: &str) -> Line {
        Line::Row(Row::plain(text.into()))
    }

    #[test]
    fn rofi_row() {
        assert_eq!(parse_rofi("Shutdown"), row("Shutdown"));
    }

    #[test]
    fn rofi_row_options() {
        let line = parse_rofi(
            "Shutdown\0info\x1fpoweroff\x1fmeta\x1fhalt off\x1fnonselectable\x1ftrue\x1ficon\x1fsystem",
        );
        let Line::Row(r) = line else { panic!("{line:?}") };
        assert_eq!(
            (r.text.as_str(), r.info(), r.meta(), r.selectable, r.raw()),
            ("Shutdown", Some("poweroff"), Some("halt off"), false, None)
        );
    }

    #[test]
    fn markup_rows_apply_to_later_rows() {
        let input = "<b>a</b>\n\0markup-rows\x1ftrue\n<b>b</b> &amp; c\n";
        let lines = all(input, Format::Rofi);
        assert_eq!(lines[0], row("<b>a</b>"));
        let Line::Row(r) = &lines[2] else { panic!() };
        assert_eq!(r.text, "b & c");
        assert_eq!(r.styles().len(), 1);
    }

    #[test]
    fn rofi_mode_option() {
        assert_eq!(
            parse_rofi("\0prompt\x1fPower"),
            Line::Mode("prompt".into(), "Power".into())
        );
    }

    #[test]
    fn rofi_empty_line_is_an_empty_row() {
        assert_eq!(parse_rofi(""), row(""));
    }

    #[test]
    fn plain_keeps_rofi_syntax_literal() {
        assert_eq!(all("a\0info\x1fb\n", Format::Plain), [row("a\0info\x1fb")]);
    }

    #[test]
    fn json_lines() {
        let input = r#"{"prompt": "files", "no-custom": true}
{"text": "a.rs", "info": 3, "size": 10}
"plain"

not json
"#;
        let lines = all(input, Format::Json);
        assert_eq!(lines[0], Line::Mode("prompt".into(), "files".into()));
        assert_eq!(lines[1], Line::Mode("no-custom".into(), "true".into()));
        let Line::Row(object) = &lines[2] else { panic!() };
        assert_eq!((object.text.as_str(), object.info()), ("a.rs", Some("3")));
        // Unknown fields ride along for the output.
        assert!(object.raw().unwrap().contains(r#""size":10"#));
        // A bare string prints back as the same JSON string.
        assert!(matches!(&lines[3], Line::Row(r) if r.text == "plain" && r.raw() == Some("\"plain\"")));
        assert_eq!(lines[4], row("not json"));
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn records_without_text_stay_rows() {
        // `ls | to json -r`, unrenamed.
        let lines = all(r#"[{"name":"a.rs","type":"file","size":10}]"#, Format::Json);
        assert!(matches!(&lines[..], [Line::Row(r)]
            if r.text == r#"{"name":"a.rs","type":"file","size":10}"#
            && r.raw() == Some(r.text.as_str())));
        // An unknown key next to option keys makes it a row too.
        assert!(matches!(&all(r#"{"prompt":"p","name":"x"}"#, Format::Json)[..], [Line::Row(_)]));
    }

    #[test]
    fn named_field_is_shown() {
        let input = r#"[{"name":"a.rs","size":10},{"text":"t","name":"b.rs"},{"size":3}]"#;
        let mut lines = Vec::new();
        feed(input.as_bytes(), Format::Json, Some("name"), |line| lines.push(line)).unwrap();
        let texts: Vec<_> = lines
            .iter()
            .map(|line| match line {
                Line::Row(r) => r.text.as_str(),
                Line::Mode(..) => panic!("{line:?}"),
            })
            .collect();
        // The named field wins over `text`; a record without it shows whole.
        assert_eq!(texts, ["a.rs", "b.rs", r#"{"size":3}"#]);
        // The whole record still goes back out on selection.
        assert!(matches!(&lines[0], Line::Row(r)
            if r.raw() == Some(r#"{"name":"a.rs","size":10}"#)));
    }

    #[test]
    fn json_array_from_nu() {
        let lines = all(r#"  [{"text":"a","selectable":false},{"text":"b"}]"#, Format::Json);
        assert!(matches!(&lines[0], Line::Row(r) if r.text == "a" && !r.selectable));
        assert!(matches!(&lines[1], Line::Row(r) if r.text == "b" && r.selectable));
    }

    #[test]
    fn script_switches_to_json_with_a_header() {
        let lines = all("{\"sieb\": 1, \"prompt\": \"p\"}\n{\"text\": \"a\"}\n", Format::Rofi);
        assert_eq!(lines[0], Line::Mode("prompt".into(), "p".into()));
        assert!(matches!(&lines[1], Line::Row(r) if r.text == "a"));
        // Without the header, a brace is just text.
        assert_eq!(all("{\"text\": \"a\"}\n", Format::Rofi), [row("{\"text\": \"a\"}")]);
    }
}
