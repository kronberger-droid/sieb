//! The line formats rows arrive in: plain text, rofi's in-band options, and
//! JSON. Shared by stdin and scripts.

use std::io::{self, BufRead};

use serde_json::{Map, Value};

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
    /// Handed back to the script in `ROFI_INFO` when this row is picked.
    pub info: Option<String>,
    /// Extra search terms, matched but not shown.
    pub meta: Option<String>,
    pub selectable: bool,
    /// The JSON object this row came from, printed back on selection so
    /// fields sieb does not know about survive the round trip.
    pub raw: Option<String>,
}

impl Row {
    pub fn plain(text: String) -> Self {
        Self {
            text,
            info: None,
            meta: None,
            selectable: true,
            raw: None,
        }
    }
}

/// Reads `reader` to the end, handing each parsed line to `sink`.
///
/// Lines that are not valid UTF-8 are kept with replacement characters
/// rather than ending the stream, since one odd filename in `fd | sieb`
/// should not truncate the list.
pub fn feed<R: BufRead>(mut reader: R, mut format: Format, mut sink: impl FnMut(Line)) -> io::Result<()> {
    if format == Format::Json && starts_with(&mut reader, b'[')? {
        // nu's `to json` writes a table as one array, not one line per row.
        let values: Vec<Value> = serde_json::from_reader(reader).map_err(io::Error::other)?;
        values.into_iter().flat_map(from_value).for_each(sink);
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
                Ok(value) => from_value(value).into_iter().for_each(&mut sink),
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
            "info" => row.info = Some(value.to_owned()),
            "meta" => row.meta = Some(value.to_owned()),
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
];

/// An object with `text` is a row; one made of option keys sets menu
/// options; a bare string or number is a row of just that.
fn from_value(value: Value) -> Vec<Line> {
    let mut object = match value {
        Value::Object(object) => object,
        Value::Null => return vec![],
        // Printed back as the same JSON value it came in as.
        scalar => {
            let raw = scalar.to_string();
            let row = Row {
                raw: Some(raw),
                ..Row::plain(as_string(scalar))
            };
            return vec![Line::Row(row)];
        }
    };
    // Only an object made of nothing but option keys sets options. Any other
    // object is a row, or `ls | to json` would vanish into ignored options.
    if !object.contains_key("text") && object.keys().all(|key| MENU_OPTIONS.contains(&key.as_str())) {
        object.remove("sieb");
        return object
            .into_iter()
            .map(|(key, value)| Line::Mode(key, as_string(value)))
            .collect();
    }
    let text = match object.get("text") {
        Some(text) => as_string(text.clone()),
        // Shown whole until there is a way to name the display field.
        None => Value::Object(object.clone()).to_string(),
    };
    let row = Row {
        text,
        info: object.get("info").cloned().map(as_string),
        meta: object.get("meta").cloned().map(as_string),
        selectable: object.get("selectable") != Some(&Value::Bool(false)),
        raw: Some(Value::Object(object).to_string()),
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
        feed(input.as_bytes(), format, |line| lines.push(line)).unwrap();
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
        assert_eq!(
            parse_rofi("Shutdown\0info\x1fpoweroff\x1fmeta\x1fhalt off\x1fnonselectable\x1ftrue\x1ficon\x1fsystem"),
            Line::Row(Row {
                text: "Shutdown".into(),
                info: Some("poweroff".into()),
                meta: Some("halt off".into()),
                selectable: false,
                raw: None,
            })
        );
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
        assert_eq!((object.text.as_str(), object.info.as_deref()), ("a.rs", Some("3")));
        // Unknown fields ride along for the output.
        assert!(object.raw.as_deref().unwrap().contains(r#""size":10"#));
        // A bare string prints back as the same JSON string.
        assert!(matches!(&lines[3], Line::Row(r) if r.text == "plain" && r.raw.as_deref() == Some("\"plain\"")));
        assert_eq!(lines[4], row("not json"));
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn records_without_text_stay_rows() {
        // `ls | to json -r`, unrenamed.
        let lines = all(r#"[{"name":"a.rs","type":"file","size":10}]"#, Format::Json);
        assert!(matches!(&lines[..], [Line::Row(r)]
            if r.text == r#"{"name":"a.rs","type":"file","size":10}"#
            && r.raw.as_deref() == Some(r.text.as_str())));
        // An unknown key next to option keys makes it a row too.
        assert!(matches!(&all(r#"{"prompt":"p","name":"x"}"#, Format::Json)[..], [Line::Row(_)]));
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
