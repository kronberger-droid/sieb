//! A password being typed: one allocation, locked out of swap, and wiped
//! whenever bytes leave it.
//!
//! The buffer is the place the secret lives, not the only place it passes
//! through. Each key press reaches us as a short `String` that sctk
//! allocates for its UTF-8, which we cannot wipe, and the pinentry protocol
//! copies the secret once more to escape it (see `assuan`). What this type
//! guarantees is that the whole password never sits in memory that is
//! reallocated, swapped or freed without being zeroed.

use std::ffi::c_void;

use unicode_segmentation::UnicodeSegmentation;
use zeroize::Zeroize;

/// Bytes the buffer holds. Typing past it is refused rather than growing
/// the buffer, since growing would leave a copy behind in the old one.
pub const CAPACITY: usize = 1024;

pub struct Secret {
    buf: Vec<u8>,
    /// Whether `mlock` took. Without it the secret may reach swap, which is
    /// worth knowing but not worth refusing to ask for a password over.
    locked: bool,
}

impl Secret {
    pub fn new() -> Self {
        let mut buf = Vec::<u8>::with_capacity(CAPACITY);
        // SAFETY: the range is the buffer's own allocation, which lives
        // until `drop` unlocks it.
        let locked = unsafe { rustix::mm::mlock(buf.as_mut_ptr().cast::<c_void>(), CAPACITY) }.is_ok();
        Self { buf, locked }
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    fn as_str(&self) -> &str {
        // Only ever filled from `&str`s and cut at grapheme boundaries.
        std::str::from_utf8(&self.buf).expect("secret holds UTF-8")
    }

    /// Characters to draw as dots, counted like the query counts them so
    /// one typed letter is one dot.
    pub fn len(&self) -> usize {
        self.as_str().graphemes(true).count()
    }

    /// Appends typed text. Returns whether it changed; text that would
    /// overflow the buffer is dropped whole.
    pub fn insert(&mut self, text: &str) -> bool {
        let before = self.buf.len();
        for c in text.chars().filter(|c| !c.is_control()) {
            let mut utf8 = [0; 4];
            let bytes = c.encode_utf8(&mut utf8).as_bytes();
            if self.buf.len() + bytes.len() > CAPACITY {
                self.truncate(before);
                utf8.zeroize();
                return false;
            }
            self.buf.extend_from_slice(bytes);
            utf8.zeroize();
        }
        debug_assert_eq!(self.buf.capacity(), CAPACITY, "the secret buffer grew");
        before != self.buf.len()
    }

    /// Deletes the last grapheme, as the query does.
    pub fn backspace(&mut self) -> bool {
        let Some((start, _)) = self.as_str().grapheme_indices(true).next_back() else {
            return false;
        };
        self.truncate(start);
        true
    }

    /// Ctrl+U.
    pub fn clear(&mut self) -> bool {
        let changed = !self.buf.is_empty();
        self.truncate(0);
        changed
    }

    /// Cuts to `len`, zeroing what is cut first: `Vec::truncate` only moves
    /// the length and leaves the bytes in the spare capacity.
    fn truncate(&mut self, len: usize) {
        self.buf[len..].zeroize();
        self.buf.truncate(len);
    }
}

impl Default for Secret {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // Zeroes the spare capacity too, then the Vec frees as usual.
        self.buf.zeroize();
        if self.locked {
            // SAFETY: the same range `new` locked, still allocated.
            let _ = unsafe { rustix::mm::munlock(self.buf.as_mut_ptr().cast::<c_void>(), CAPACITY) };
        }
    }
}

/// Keeps this process out of core dumps and away from same-user ptrace, so
/// a crash or a debugger does not hand the password to whoever asks.
pub fn harden() {
    let _ = rustix::process::set_dumpable_behavior(rustix::process::DumpableBehavior::NotDumpable);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_graphemes_not_bytes() {
        let mut secret = Secret::new();
        secret.insert("pa\u{e9}e\u{301}");
        assert_eq!(secret.len(), 4);
        assert!(secret.backspace());
        assert_eq!(secret.as_bytes(), "pa\u{e9}".as_bytes());
    }

    #[test]
    fn drops_control_characters() {
        let mut secret = Secret::new();
        assert!(!secret.insert("\t\r"));
        assert!(secret.insert("a\nb"));
        assert_eq!(secret.as_bytes(), b"ab");
    }

    #[test]
    fn refuses_to_grow() {
        let mut secret = Secret::new();
        assert!(secret.insert(&"x".repeat(CAPACITY - 1)));
        // Two bytes do not fit in one: the whole character is refused.
        assert!(!secret.insert("\u{e9}"));
        assert_eq!(secret.as_bytes().len(), CAPACITY - 1);
        assert!(secret.insert("y"));
        assert!(!secret.insert("z"));
        assert_eq!(secret.buf.capacity(), CAPACITY);
    }

    #[test]
    fn refused_insert_leaves_nothing_behind() {
        let mut secret = Secret::new();
        secret.insert(&"x".repeat(CAPACITY - 2));
        assert!(!secret.insert("abc"));
        assert_eq!(secret.as_bytes().len(), CAPACITY - 2);
        // SAFETY: inside the allocation, which is initialized up to where
        // the refused insert wrote before it was cut.
        let spare = unsafe { std::slice::from_raw_parts(secret.buf.as_ptr().add(CAPACITY - 2), 2) };
        assert_eq!(spare, [0, 0]);
    }

    #[test]
    fn clear_wipes_the_bytes() {
        let mut secret = Secret::new();
        secret.insert("hunter2");
        assert!(secret.clear());
        // SAFETY: the bytes were initialized by the insert above.
        let old = unsafe { std::slice::from_raw_parts(secret.buf.as_ptr(), 7) };
        assert_eq!(old, [0; 7]);
        assert!(!secret.clear());
    }
}
