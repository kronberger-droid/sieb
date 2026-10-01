//! Query editing, selection and scrolling, independent of any window.

/// What Enter resolves to.
#[derive(Debug, PartialEq, Eq)]
pub enum Accept {
    /// The match at this rank.
    Match(u32),
    /// The typed query itself, for input that is not in the list.
    Query,
}

pub struct Picker {
    query: String,
    selected: u32,
    scroll: u32,
    lines: u32,
}

impl Picker {
    pub fn new(lines: u32) -> Self {
        Self {
            query: String::new(),
            selected: 0,
            scroll: 0,
            lines: lines.max(1),
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn lines(&self) -> u32 {
        self.lines
    }

    /// Rank of the selected match.
    pub fn selected(&self) -> u32 {
        self.selected
    }

    /// Rank of the first visible match.
    pub fn scroll(&self) -> u32 {
        self.scroll
    }

    /// Returns whether the query changed.
    pub fn insert(&mut self, text: &str) -> bool {
        let before = self.query.len();
        self.query.extend(text.chars().filter(|c| !c.is_control()));
        self.edited(before != self.query.len())
    }

    pub fn backspace(&mut self) -> bool {
        let changed = self.query.pop().is_some();
        self.edited(changed)
    }

    /// Ctrl+U: clear the whole query.
    pub fn clear(&mut self) -> bool {
        let changed = !self.query.is_empty();
        self.query.clear();
        self.edited(changed)
    }

    /// Ctrl+W: delete back to the previous whitespace, like a shell.
    pub fn delete_word(&mut self) -> bool {
        let trimmed = self.query.trim_end().len();
        let start = self.query[..trimmed]
            .rfind(char::is_whitespace)
            .map_or(0, |i| i + 1);
        let changed = start != self.query.len();
        self.query.truncate(start);
        self.edited(changed)
    }

    fn edited(&mut self, changed: bool) -> bool {
        if changed {
            // New query, new ranking: the old position means nothing.
            self.selected = 0;
            self.scroll = 0;
        }
        changed
    }

    /// Moves the selection by `delta` ranks within `count` matches. Stops at
    /// the ends rather than wrapping.
    pub fn move_by(&mut self, delta: i64, count: u32) {
        if count == 0 {
            return;
        }
        let target = (self.selected as i64 + delta).clamp(0, count as i64 - 1);
        self.selected = target as u32;
        self.follow();
    }

    pub fn page(&mut self, pages: i64, count: u32) {
        self.move_by(pages * self.lines as i64, count);
    }

    /// Keeps the selection valid while matches stream in or drop out.
    pub fn clamp(&mut self, count: u32) {
        self.selected = self.selected.min(count.saturating_sub(1));
        self.scroll = self.scroll.min(count.saturating_sub(self.lines));
        self.follow();
    }

    /// Scrolls just enough to keep the selection visible.
    fn follow(&mut self) {
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + self.lines {
            self.scroll = self.selected + 1 - self.lines;
        }
    }

    /// Enter selects the highlighted match, or takes the query as typed
    /// when nothing matches, like dmenu. `force_query` is Shift+Enter.
    pub fn accept(&self, count: u32, force_query: bool) -> Accept {
        if force_query || count == 0 {
            Accept::Query
        } else {
            Accept::Match(self.selected)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(query: &str) -> Picker {
        let mut picker = Picker::new(5);
        picker.insert(query);
        picker
    }

    #[test]
    fn delete_word_takes_trailing_space_with_it() {
        let mut picker = typed("open the  ");
        assert!(picker.delete_word());
        assert_eq!(picker.query(), "open ");
        picker.delete_word();
        assert_eq!(picker.query(), "");
        assert!(!picker.delete_word());
    }

    #[test]
    fn insert_drops_control_characters() {
        let picker = typed("a\tb\u{7f}c");
        assert_eq!(picker.query(), "abc");
    }

    #[test]
    fn editing_resets_selection() {
        let mut picker = typed("x");
        picker.move_by(3, 10);
        picker.backspace();
        assert_eq!((picker.selected(), picker.scroll()), (0, 0));
    }

    #[test]
    fn scroll_follows_selection() {
        let mut picker = Picker::new(5);
        picker.move_by(7, 20);
        assert_eq!((picker.selected(), picker.scroll()), (7, 3));
        picker.move_by(-6, 20);
        assert_eq!((picker.selected(), picker.scroll()), (1, 1));
        picker.move_by(100, 20);
        assert_eq!((picker.selected(), picker.scroll()), (19, 15));
    }

    #[test]
    fn clamp_when_matches_shrink() {
        let mut picker = Picker::new(5);
        picker.move_by(12, 20);
        picker.clamp(4);
        assert_eq!((picker.selected(), picker.scroll()), (3, 0));
        picker.clamp(0);
        assert_eq!((picker.selected(), picker.scroll()), (0, 0));
    }

    #[test]
    fn accept_falls_back_to_query() {
        let mut picker = typed("q");
        picker.move_by(2, 5);
        assert_eq!(picker.accept(5, false), Accept::Match(2));
        assert_eq!(picker.accept(5, true), Accept::Query);
        assert_eq!(picker.accept(0, false), Accept::Query);
    }
}
