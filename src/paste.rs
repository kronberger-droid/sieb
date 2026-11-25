//! Pasting into the query: which form of a selection to ask for, reading
//! it off its pipe, and what of it goes into one line of text.
//!
//! The Wayland side, which holds the offers and asks for the pipe, is in
//! `window`.

use std::io::Read;

use zeroize::{Zeroize, Zeroizing};

/// Text forms a selection is offered in, the best first. The X11 names
/// come from clients running under Xwayland.
const TEXT: &[&str] = &[
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
    "TEXT",
];

/// The most that is read of a selection. A query or a password is a line,
/// and a clipboard may hold a whole file.
const MAX: usize = 16 * 1024;

/// The best text form among the ones `offered`, if any is text.
pub fn pick_mime(offered: &[String]) -> Option<String> {
    let best = TEXT
        .iter()
        .find(|mime| offered.iter().any(|offer| offer == *mime))?;
    Some((*best).to_owned())
}

/// Reads a selection from the pipe its owner writes it to, up to [`MAX`].
/// Blocks until the owner is done, so this belongs on a thread of its own.
///
/// The text may be a password on its way into a password prompt, so the
/// buffer is sized once, never reallocated, and wiped when dropped.
pub fn read(pipe: impl Read) -> Zeroizing<String> {
    // Room to spare past the limit: `read_to_end` only grows a buffer that
    // is full, and growing would leave a copy behind in the old one.
    let mut bytes = Vec::with_capacity(MAX + 64);
    let _ = pipe.take(MAX as u64).read_to_end(&mut bytes);
    Zeroizing::new(String::from_utf8(bytes).unwrap_or_else(|err| {
        let mut bytes = err.into_bytes();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        bytes.zeroize();
        text
    }))
}

/// What of a pasted text goes into the query: one line, its line breaks
/// and tabs as spaces, without the newline most copies end in.
pub fn for_query(text: &str) -> String {
    text.trim_end_matches(['\n', '\r'])
        .chars()
        .map(|ch| {
            if matches!(ch, '\n' | '\r' | '\t') {
                ' '
            } else {
                ch
            }
        })
        .collect()
}

/// What of a pasted text goes into a password: its first line. A password
/// has no line breaks, and what follows one was not meant to be part.
pub fn for_secret(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered(mimes: &[&str]) -> Vec<String> {
        mimes.iter().map(|mime| (*mime).to_owned()).collect()
    }

    #[test]
    fn picks_the_best_text_form() {
        let firefox = offered(&["text/html", "text/plain", "text/plain;charset=utf-8"]);
        assert_eq!(
            pick_mime(&firefox).as_deref(),
            Some("text/plain;charset=utf-8")
        );
        let xwayland = offered(&["TARGETS", "STRING", "UTF8_STRING"]);
        assert_eq!(pick_mime(&xwayland).as_deref(), Some("UTF8_STRING"));
        assert_eq!(pick_mime(&offered(&["image/png"])), None);
        assert_eq!(pick_mime(&[]), None);
    }

    #[test]
    fn reads_up_to_the_limit_and_survives_bad_utf8() {
        assert_eq!(read(&b"hello\n"[..]).as_str(), "hello\n");
        assert_eq!(read(&b"a\xffb"[..]).as_str(), "a\u{fffd}b");
        let long = vec![b'x'; MAX * 2];
        assert_eq!(read(&long[..]).len(), MAX);
    }

    #[test]
    fn query_gets_one_line() {
        assert_eq!(for_query("ls -la\n"), "ls -la");
        assert_eq!(
            for_query("one\ntwo\r\nthree\tfour\n\n"),
            "one two  three four"
        );
        assert_eq!(for_query(""), "");
    }

    #[test]
    fn secret_gets_the_first_line() {
        assert_eq!(for_secret("hunter2\n"), "hunter2");
        assert_eq!(for_secret("hunter2\nnotes"), "hunter2");
        assert_eq!(for_secret(" spaced \r\n"), " spaced ");
        assert_eq!(for_secret(""), "");
    }
}
