use std::io::{self, BufRead};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Injector, Nucleo, Status};

/// One input line, with its position in the input stream.
pub struct Entry {
    pub index: u32,
    pub text: String,
}

pub struct Matcher {
    nucleo: Nucleo<Entry>,
    case: CaseMatching,
    query: String,
}

impl Matcher {
    /// `notify` fires from worker threads whenever new results are ready to
    /// be picked up with [`Matcher::tick`].
    pub fn new(case: CaseMatching, notify: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            nucleo: Nucleo::new(Config::DEFAULT, notify, None, 1),
            case,
            query: String::new(),
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
            let entry = Entry {
                index,
                text: String::from_utf8_lossy(line).into_owned(),
            };
            injector.push(entry, |entry, columns| {
                columns[0] = entry.text.as_str().into();
            });
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
