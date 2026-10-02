use std::io::{self, BufRead};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Injector, Nucleo, Status};

use crate::format::{self, Format, Line, Row};

/// One input row, with its position in the input stream.
pub struct Entry {
    pub index: u32,
    pub row: Row,
}

/// Adds a row to the matcher. `meta` is matched but not shown, so it goes
/// into the haystack after the text, where highlight indices past the
/// visible text simply fall off the end.
pub fn push(injector: &Injector<Entry>, index: u32, row: Row) {
    injector.push(Entry { index, row }, |entry, columns| {
        let row = &entry.row;
        columns[0] = match row.meta() {
            Some(meta) => format!("{} {meta}", row.text).into(),
            None => row.text.as_str().into(),
        };
    });
}

/// What a pick prints in dmenu mode.
#[derive(Clone, Debug, PartialEq)]
pub enum Print {
    Text,
    /// The position in the input, `-1` for typed text like rofi.
    Index,
    /// The JSON value the row came from. Typed text comes back as an
    /// object with the text under `field`, `text` by default.
    Json { field: Option<String> },
}

impl Print {
    pub fn entry(&self, entry: &Entry) -> String {
        match self {
            Print::Text => entry.row.text.clone(),
            Print::Index => entry.index.to_string(),
            // The whole object, unknown fields included, so a pipeline gets
            // back the record it put in.
            Print::Json { .. } => match entry.row.raw() {
                Some(raw) => raw.to_owned(),
                None => serde_json::json!({ "text": entry.row.text }).to_string(),
            },
        }
    }

    pub fn query(&self, query: &str) -> String {
        match self {
            Print::Text => query.to_owned(),
            Print::Index => "-1".into(),
            // Under the shown key, so the record has the shape that went in.
            Print::Json { field } => {
                let key = field.as_deref().unwrap_or("text");
                serde_json::json!({ key: query }).to_string()
            }
        }
    }
}

pub struct Matcher {
    nucleo: Nucleo<Entry>,
    case: CaseMatching,
    query: String,
    /// Separate from the worker's matchers: recomputes match positions for
    /// the handful of visible rows on the UI thread.
    highlighter: nucleo::Matcher,
}

impl Matcher {
    /// `notify` fires from worker threads whenever new results are ready to
    /// be picked up with [`Matcher::tick`].
    pub fn new(case: CaseMatching, notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            nucleo: Nucleo::new(Config::DEFAULT, notify, None, 1),
            case,
            query: String::new(),
            highlighter: nucleo::Matcher::new(Config::DEFAULT),
        }
    }

    pub fn injector(&self) -> Injector<Entry> {
        self.nucleo.injector()
    }

    pub fn set_query(&mut self, query: &str) {
        // nucleo only rescores the previous matches when told the new query
        // extends the old one. Getting this wrong loses matches.
        let append = query.starts_with(self.query.as_str());
        self.nucleo
            .pattern
            .reparse(0, query, self.case, Normalization::Smart, append);
        self.query = query.to_owned();
    }

    /// Waits up to `timeout_ms` for the workers and refreshes the snapshot.
    pub fn tick(&mut self, timeout_ms: u64) -> Status {
        self.nucleo.tick(timeout_ms)
    }

    /// Waits until the worker has ranked the current query, up to `budget`.
    /// A stdin that is still streaming can keep it busy indefinitely.
    pub fn settle(&mut self, budget: Duration) {
        let start = Instant::now();
        while self.tick(10).running && start.elapsed() < budget {}
    }

    /// `(matched, total)` as of the last [`Matcher::tick`].
    pub fn counts(&self) -> (u32, u32) {
        let snapshot = self.nucleo.snapshot();
        (snapshot.matched_item_count(), snapshot.item_count())
    }

    /// Number of matches, as of the last [`Matcher::tick`].
    pub fn matched(&self) -> u32 {
        self.counts().0
    }

    /// The match at `rank`, as of the last [`Matcher::tick`].
    pub fn get(&self, rank: u32) -> Option<&Entry> {
        self.nucleo
            .snapshot()
            .get_matched_item(rank)
            .map(|item| item.data)
    }

    /// Up to `len` matches starting at `start`, each with the sorted
    /// grapheme indices that matched the query.
    pub fn window(&mut self, start: u32, len: u32) -> Vec<(&Entry, Vec<u32>)> {
        let snapshot = self.nucleo.snapshot();
        let end = (start + len).min(snapshot.matched_item_count());
        let start = start.min(end);
        let pattern = snapshot.pattern().column_pattern(0);
        snapshot
            .matched_items(start..end)
            .map(|item| {
                let mut indices = Vec::new();
                pattern.indices(
                    item.matcher_columns[0].slice(..),
                    &mut self.highlighter,
                    &mut indices,
                );
                // One run per atom, appended as is: sort and dedup.
                indices.sort_unstable();
                indices.dedup();
                (item.data, indices)
            })
            .collect()
    }

    /// Matches in rank order, as of the last [`Matcher::tick`].
    pub fn matches(&self) -> impl Iterator<Item = &Entry> {
        self.nucleo
            .snapshot()
            .matched_items(..)
            .map(|item| item.data)
    }
}

/// Reads lines from `reader` into the matcher on a background thread.
///
/// Menu options in the input (JSON only) go to `on_mode`. `field` is the
/// JSON key shown as each record's text, see [`format::feed`].
pub fn spawn_reader<R>(
    reader: R,
    injector: Injector<Entry>,
    format: Format,
    field: Option<String>,
    mut on_mode: impl FnMut(String, String) + Send + 'static,
) -> JoinHandle<io::Result<()>>
where
    R: BufRead + Send + 'static,
{
    thread::spawn(move || {
        let mut index = 0;
        format::feed(reader, format, field.as_deref(), |line| match line {
            Line::Mode(key, value) => on_mode(key, value),
            Line::Row(row) => {
                push(&injector, index, row);
                index += 1;
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn filter(input: &'static [u8], query: &str) -> Vec<(u32, String)> {
        let mut matcher = Matcher::new(CaseMatching::Smart, Arc::new(|| {}));
        matcher.set_query(query);
        spawn_reader(Cursor::new(input), matcher.injector(), Format::Plain, None, |_, _| {})
            .join()
            .unwrap()
            .unwrap();
        while matcher.tick(10).running {}
        matcher
            .matches()
            .map(|e| (e.index, e.row.text.clone()))
            .collect()
    }

    #[test]
    fn print_picks_and_typed_text() {
        let mut json = Row::plain("a.rs".into());
        json.extra_mut().raw = Some(r#"{"name":"a.rs","size":1}"#.into());
        let entry = |row| Entry { index: 4, row };
        let named = Print::Json {
            field: Some("name".into()),
        };
        assert_eq!(Print::Text.entry(&entry(Row::plain("x".into()))), "x");
        assert_eq!(Print::Index.entry(&entry(Row::plain("x".into()))), "4");
        assert_eq!(named.entry(&entry(json)), r#"{"name":"a.rs","size":1}"#);
        // A row that came in as plain text still prints as JSON.
        let plain = Print::Json { field: None }.entry(&entry(Row::plain("x".into())));
        assert_eq!(plain, r#"{"text":"x"}"#);
        assert_eq!(Print::Index.query("typed"), "-1");
        assert_eq!(named.query("typed"), r#"{"name":"typed"}"#);
    }

    #[test]
    fn empty_query_keeps_input_order() {
        let got = filter(b"c\na\nb\n", "");
        assert_eq!(got, [(0, "c".into()), (1, "a".into()), (2, "b".into())]);
    }

    #[test]
    fn invalid_utf8_does_not_end_the_stream() {
        let got = filter(b"a\n\xff\nb", "");
        assert_eq!(got.len(), 3);
        assert_eq!(got[2], (2, "b".into()));
    }

    #[test]
    fn strips_crlf() {
        let got = filter(b"one\r\ntwo\r\n", "");
        assert_eq!(got, [(0, "one".into()), (1, "two".into())]);
    }

    #[test]
    fn index_survives_reordering() {
        let got = filter(b"foobar\nbar\n", "bar");
        assert_eq!(got[0], (1, "bar".into()));
    }
}
