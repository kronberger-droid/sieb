use std::io::{self, BufRead};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Injector, Nucleo, Status};

/// One input line, with its position in the input stream.
pub struct Entry {
    pub index: u32,
    pub text: String,
    /// Script mode: handed back to the script when this entry is picked.
    pub info: Option<String>,
    pub selectable: bool,
}

/// Adds a row to the matcher. `meta` is matched but not shown, so it goes
/// into the haystack after the text, where highlight indices past the
/// visible text simply fall off the end.
pub fn push(injector: &Injector<Entry>, index: u32, row: crate::script::Row) {
    let haystack = match &row.meta {
        Some(meta) => format!("{} {meta}", row.text),
        None => row.text.clone(),
    };
    let entry = Entry {
        index,
        text: row.text,
        info: row.info,
        selectable: row.selectable,
    };
    injector.push(entry, |_, columns| columns[0] = haystack.as_str().into());
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
/// Lines that are not valid UTF-8 are kept with replacement characters
/// rather than ending the stream, since one odd filename in `fd | sieb`
/// should not truncate the list.
pub fn spawn_reader<R>(mut reader: R, injector: Injector<Entry>) -> JoinHandle<io::Result<()>>
where
    R: BufRead + Send + 'static,
{
    thread::spawn(move || {
        let mut buf = Vec::new();
        let mut index = 0;
        loop {
            buf.clear();
            if reader.read_until(b'\n', &mut buf)? == 0 {
                return Ok(());
            }
            let line = buf.strip_suffix(b"\n").unwrap_or(&buf);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let row = crate::script::Row {
                text: String::from_utf8_lossy(line).into_owned(),
                selectable: true,
                ..Default::default()
            };
            push(&injector, index, row);
            index += 1;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn filter(input: &'static [u8], query: &str) -> Vec<(u32, String)> {
        let mut matcher = Matcher::new(CaseMatching::Smart, Arc::new(|| {}));
        matcher.set_query(query);
        spawn_reader(Cursor::new(input), matcher.injector())
            .join()
            .unwrap()
            .unwrap();
        while matcher.tick(10).running {}
        matcher
            .matches()
            .map(|e| (e.index, e.text.clone()))
            .collect()
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
