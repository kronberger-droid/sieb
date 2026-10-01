//! Panel geometry in logical pixels, shared by drawing and hit-testing so
//! the two cannot drift apart.

use crate::text::Text;

/// Vertical room around a line of text inside a row.
const ROW_PADDING: f32 = 13.0;
const SEPARATOR: f32 = 1.0;
const SEPARATOR_GAP: f32 = 6.0;

#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub width: f32,
    pub padding: f32,
    pub font_size: f32,
    pub row: f32,
    pub lines: u32,
    /// A script set a message, shown in a row of its own above the list.
    pub message: bool,
}

impl Layout {
    pub fn new(width: u32, lines: u32, font_size: f32, padding: f32) -> Self {
        Self {
            width: width as f32,
            padding,
            font_size,
            // Derived from the font, so large fonts do not get clipped.
            row: Text::line_height(font_size) + ROW_PADDING,
            lines: lines.max(1),
            message: false,
        }
    }

    /// Top of the input row.
    pub fn input_top(&self) -> f32 {
        self.padding
    }

    /// Top of the separator between input and list.
    pub fn separator_top(&self) -> f32 {
        self.padding + self.row
    }

    pub fn separator_height(&self) -> f32 {
        SEPARATOR
    }

    /// Top of the message row, when there is one.
    pub fn message_top(&self) -> f32 {
        self.separator_top() + SEPARATOR + SEPARATOR_GAP
    }

    pub fn list_top(&self) -> f32 {
        self.message_top() + if self.message { self.row } else { 0.0 }
    }

    /// Top of visible row `i`.
    pub fn row_top(&self, i: usize) -> f32 {
        self.list_top() + i as f32 * self.row
    }

    pub fn height(&self) -> f32 {
        self.list_top() + self.lines as f32 * self.row + self.padding
    }

    /// Panel size, rounded up to whole logical pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width.ceil() as u32, self.height().ceil() as u32)
    }

    /// The visible row under surface coordinate `y`, if any.
    pub fn row_at(&self, y: f64) -> Option<usize> {
        let offset = y as f32 - self.list_top();
        if offset < 0.0 {
            return None;
        }
        let row = (offset / self.row) as usize;
        (row < self.lines as usize).then_some(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_at_matches_row_top() {
        let layout = Layout::new(640, 5, 15.0, 10.0);
        for i in 0..5 {
            let top = layout.row_top(i) as f64;
            assert_eq!(layout.row_at(top + 0.5), Some(i));
            assert_eq!(layout.row_at(top + layout.row as f64 - 0.5), Some(i));
        }
    }

    #[test]
    fn row_at_outside_the_list() {
        let layout = Layout::new(640, 5, 15.0, 10.0);
        assert_eq!(layout.row_at(layout.input_top() as f64 + 1.0), None);
        assert_eq!(layout.row_at(layout.row_top(5) as f64 + 1.0), None);
    }

    #[test]
    fn message_pushes_the_list_down() {
        let mut layout = Layout::new(640, 5, 15.0, 10.0);
        let (top, height) = (layout.row_top(0), layout.height());
        layout.message = true;
        assert_eq!(layout.row_top(0), top + layout.row);
        assert_eq!(layout.height(), height + layout.row);
        // The message row is not a list row.
        assert_eq!(layout.row_at(layout.message_top() as f64 + 1.0), None);
    }

    #[test]
    fn default_row_height_is_unchanged() {
        // 15px text used to sit in hardcoded 32px rows.
        assert_eq!(Layout::new(640, 10, 15.0, 10.0).row, 32.0);
    }
}
