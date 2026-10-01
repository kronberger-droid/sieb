//! Panel geometry in logical pixels, shared by drawing and hit-testing so
//! the two cannot drift apart.
//!
//! The panel is a vertical stack inside `padding`: the input row, an
//! optional separator, an optional message box and the list, with
//! `spacing` between neighbours. Each of them is a line of text plus its
//! own padding, so their heights follow the font.

use crate::text::Text;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub width: f32,
    pub padding: f32,
    pub font_size: f32,
    /// Gap between the input row, separator, message and list.
    pub spacing: f32,
    /// Height of the line between input and list, 0 for none.
    pub separator: f32,
    /// Around the text of the prompt, badge and query.
    pub input_padding: f32,
    /// Around the text of each row, also the room left of it.
    pub row_padding: f32,
    /// Gap between rows.
    pub row_spacing: f32,
    /// Around the text of the message.
    pub message_padding: f32,
    pub lines: u32,
    /// A script set a message, shown in a box of its own above the list.
    pub message: bool,
    /// Mode buttons under the list, one per script; none for fewer than 2.
    pub buttons: usize,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            width: 640.0,
            padding: 10.0,
            font_size: 15.0,
            spacing: 3.0,
            separator: 1.0,
            input_padding: 6.5,
            row_padding: 6.5,
            row_spacing: 0.0,
            message_padding: 6.5,
            lines: 10,
            message: false,
            buttons: 0,
        }
    }
}

impl Layout {
    /// Height of one line of text.
    pub fn line(&self) -> f32 {
        Text::line_height(self.font_size)
    }

    /// Height of a row box. Derived from the font, so large fonts do not
    /// get clipped.
    pub fn row(&self) -> f32 {
        self.line() + 2.0 * self.row_padding
    }

    fn lines(&self) -> u32 {
        self.lines.max(1)
    }

    pub fn input_top(&self) -> f32 {
        self.padding
    }

    pub fn input_height(&self) -> f32 {
        self.line() + 2.0 * self.input_padding
    }

    /// Top of the separator between input and list.
    pub fn separator_top(&self) -> f32 {
        self.input_top() + self.input_height() + self.spacing
    }

    /// Top of the message box, when there is one.
    pub fn message_top(&self) -> f32 {
        let below_input = self.input_top() + self.input_height() + self.spacing;
        if self.separator > 0.0 {
            below_input + self.separator + self.spacing
        } else {
            below_input
        }
    }

    pub fn message_height(&self) -> f32 {
        self.line() + 2.0 * self.message_padding
    }

    pub fn list_top(&self) -> f32 {
        let message = if self.message {
            self.message_height() + self.spacing
        } else {
            0.0
        };
        self.message_top() + message
    }

    pub fn list_height(&self) -> f32 {
        let n = self.lines() as f32;
        n * self.row() + (n - 1.0) * self.row_spacing
    }

    /// Top of visible row `i`.
    pub fn row_top(&self, i: usize) -> f32 {
        self.list_top() + i as f32 * (self.row() + self.row_spacing)
    }

    fn has_buttons(&self) -> bool {
        self.buttons > 1
    }

    /// Top of the mode buttons, which are as tall as a row.
    pub fn buttons_top(&self) -> f32 {
        self.list_top() + self.list_height() + self.spacing
    }

    /// Left edge and width of mode button `i`. The buttons share the
    /// width, `spacing` apart.
    pub fn button(&self, i: usize) -> (f32, f32) {
        let n = self.buttons.max(1) as f32;
        let width = (self.width - 2.0 * self.padding - (n - 1.0) * self.spacing) / n;
        (self.padding + i as f32 * (width + self.spacing), width)
    }

    /// The mode button under surface coordinate (`x`, `y`), if any.
    pub fn button_at(&self, x: f64, y: f64) -> Option<usize> {
        let (x, y) = (x as f32, y as f32);
        let top = self.buttons_top();
        if !self.has_buttons() || y < top || y >= top + self.row() {
            return None;
        }
        (0..self.buttons).find(|&i| {
            let (left, width) = self.button(i);
            (left..left + width).contains(&x)
        })
    }

    pub fn height(&self) -> f32 {
        let buttons = if self.has_buttons() {
            self.spacing + self.row()
        } else {
            0.0
        };
        self.list_top() + self.list_height() + buttons + self.padding
    }

    /// Panel size, rounded up to whole logical pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.width.ceil() as u32, self.height().ceil() as u32)
    }

    /// The visible row under surface coordinate `y`, if any. The gaps
    /// between rows belong to none.
    pub fn row_at(&self, y: f64) -> Option<usize> {
        let offset = y as f32 - self.list_top();
        if offset < 0.0 {
            return None;
        }
        let pitch = self.row() + self.row_spacing;
        let row = (offset / pitch) as usize;
        let inside = offset - row as f32 * pitch < self.row();
        (inside && row < self.lines() as usize).then_some(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout {
            lines: 5,
            ..Layout::default()
        }
    }

    #[test]
    fn row_at_matches_row_top() {
        for layout in [
            layout(),
            Layout {
                row_spacing: 5.0,
                ..layout()
            },
        ] {
            for i in 0..5 {
                let top = layout.row_top(i) as f64;
                assert_eq!(layout.row_at(top + 0.5), Some(i));
                assert_eq!(layout.row_at(top + layout.row() as f64 - 0.5), Some(i));
            }
        }
    }

    #[test]
    fn row_at_outside_the_list() {
        let layout = layout();
        assert_eq!(layout.row_at(layout.input_top() as f64 + 1.0), None);
        assert_eq!(layout.row_at(layout.row_top(5) as f64 + 1.0), None);
    }

    #[test]
    fn gaps_between_rows_hit_nothing() {
        let layout = Layout {
            row_spacing: 5.0,
            ..layout()
        };
        let gap = (layout.row_top(0) + layout.row()) as f64 + 2.0;
        assert_eq!(layout.row_at(gap), None);
    }

    #[test]
    fn message_pushes_the_list_down() {
        let mut layout = layout();
        let (top, height) = (layout.row_top(0), layout.height());
        layout.message = true;
        let shift = layout.message_height() + layout.spacing;
        assert_eq!(layout.row_top(0), top + shift);
        assert_eq!(layout.height(), height + shift);
        // The message box is not a list row.
        assert_eq!(layout.row_at(layout.message_top() as f64 + 1.0), None);
    }

    #[test]
    fn no_separator_takes_no_room() {
        let with = layout();
        let without = Layout {
            separator: 0.0,
            ..layout()
        };
        assert_eq!(with.list_top() - without.list_top(), with.separator + with.spacing);
    }

    #[test]
    fn buttons_share_the_width() {
        let layout = Layout {
            width: 400.0,
            padding: 10.0,
            spacing: 10.0,
            buttons: 4,
            ..layout()
        };
        // (380 - 3 * 10) / 4 = 87.5 each.
        assert_eq!(layout.button(0), (10.0, 87.5));
        assert_eq!(layout.button(3), (302.5, 87.5));
        let y = layout.buttons_top() as f64 + 1.0;
        assert_eq!(layout.button_at(11.0, y), Some(0));
        assert_eq!(layout.button_at(100.0, y), None, "the gap between 0 and 1");
        assert_eq!(layout.button_at(389.0, y), Some(3));
        assert_eq!(layout.height(), layout.buttons_top() + layout.row() + layout.padding);
    }

    #[test]
    fn one_script_has_no_buttons() {
        let one = Layout {
            buttons: 1,
            ..layout()
        };
        assert_eq!(one.height(), layout().height());
        assert_eq!(one.button_at(20.0, one.buttons_top() as f64 + 1.0), None);
    }

    #[test]
    fn default_row_height_is_unchanged() {
        // 15px text used to sit in hardcoded 32px rows.
        assert_eq!(Layout::default().row(), 32.0);
    }
}
