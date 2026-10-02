use cosmic_text::Color as TextColor;
use tiny_skia::{Color, FillRule, Paint, Path, PathBuilder, PixmapMut, Stroke, Transform};
use unicode_segmentation::UnicodeSegmentation;

use std::ops::Range;

use crate::layout::Layout;
use crate::markup::Style;
use crate::text::{Canvas, Clip, Span, Text, mul};

/// Display cap per row. `Wrap::None` still shapes the whole line, so one
/// minified JSON blob on stdin would otherwise stall every frame.
const MAX_GRAPHEMES: usize = 300;
const CARET_WIDTH: f32 = 2.0;
const SCROLLBAR_WIDTH: f32 = 5.0;

pub const TRANSPARENT: [u8; 4] = [0; 4];

/// Colors as straight (not premultiplied) RGBA.
#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub background: [u8; 4],
    pub border: [u8; 4],
    /// Behind the selected row.
    pub selected: [u8; 4],
    /// Text of the selected row.
    pub selected_text: [u8; 4],
    pub separator: [u8; 4],
    pub text: [u8; 4],
    /// The match counter.
    pub dim: [u8; 4],
    /// The caret.
    pub accent: [u8; 4],
    /// Matched characters.
    pub matched: [u8; 4],
    /// Matched characters on the selected row.
    pub selected_match: [u8; 4],
    /// Behind every other row.
    pub row: [u8; 4],
    pub placeholder: [u8; 4],
    pub prompt: [u8; 4],
    pub prompt_background: [u8; 4],
    pub badge: [u8; 4],
    pub badge_background: [u8; 4],
    pub message: [u8; 4],
    pub message_background: [u8; 4],
    pub scrollbar: [u8; 4],
    pub scrollbar_handle: [u8; 4],
    /// Mode buttons, and the one of the mode on screen.
    pub button: [u8; 4],
    pub button_text: [u8; 4],
    pub button_selected: [u8; 4],
    pub button_selected_text: [u8; 4],
    pub backdrop: [u8; 4],
}

impl Default for Palette {
    /// The built-in colors, with every fallback applied the same way a
    /// config file's are.
    fn default() -> Self {
        crate::config::Colors::default().palette()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub colors: Palette,
    pub radius: f32,
    pub border_width: f32,
    /// Corners of rows, the message box and the prompt and badge pills.
    pub row_radius: f32,
    /// Shown in the empty query.
    pub placeholder: String,
    /// A pill left of the prompt, like a power glyph. Empty for none.
    pub badge: String,
    /// Show `matched/total` right of the query.
    pub counter: bool,
    pub scrollbar: bool,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            colors: Palette::default(),
            radius: 12.0,
            border_width: 1.0,
            row_radius: 6.0,
            placeholder: String::new(),
            badge: String::new(),
            counter: true,
            scrollbar: false,
        }
    }
}

/// Everything one frame shows, in logical terms.
pub struct View<'a> {
    pub prompt: Option<&'a str>,
    pub message: Option<&'a str>,
    pub query: &'a str,
    pub rows: Vec<RowView<'a>>,
    /// Index into `rows`.
    pub selected: Option<usize>,
    /// Rank of the first visible row.
    pub scroll: u32,
    /// Labels of the mode buttons, drawn when there are at least two.
    pub buttons: Vec<&'a str>,
    /// The button of the mode on screen.
    pub active: usize,
    pub matched: u32,
    pub total: u32,
}

/// One visible row.
pub struct RowView<'a> {
    pub text: &'a str,
    /// Styled byte ranges of `text`, from markup.
    pub styles: &'a [(Range<usize>, Style)],
    /// Grapheme indices that matched the query.
    pub indices: Vec<u32>,
}

/// A horizontal span of the input row, in physical pixels.
struct Pill {
    left: f32,
    width: f32,
}

/// Draws the panel into a `wl_shm` ARGB8888 buffer of `width` x `height`
/// physical pixels. `layout` is in logical pixels and multiplied by `scale`
/// here, so the result stays crisp at fractional scales.
#[allow(clippy::too_many_arguments)]
pub fn panel(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    scale: f32,
    layout: &Layout,
    theme: &Theme,
    text: &mut Text,
    view: &View,
) {
    let s = |v: f32| v * scale;
    let c = &theme.colors;
    let size = s(layout.font_size);
    let line = Text::line_height(size);
    let left = s(layout.padding);
    let right = width as f32 - left;
    let gap = s(layout.spacing);
    let input_padding = s(layout.input_padding);
    let row_radius = s(theme.row_radius);
    // Centers a line of text in a box starting at `top`.
    let text_top = |top: f32, height: f32| top + (height - line) / 2.0;

    // Measured before drawing, since the shapes depend on text widths.
    let mut x = left;
    let mut pill = |label: Option<&str>, text: &mut Text| {
        let label = label.filter(|label| !label.is_empty())?;
        let width = text.width(size, label) + 2.0 * input_padding;
        let pill = Pill { left: x, width };
        x += width + gap;
        Some(pill)
    };
    let badge = pill(Some(theme.badge.as_str()), text);
    let prompt = pill(view.prompt, text);
    let counter = theme
        .counter
        .then(|| format!("{}/{}", view.matched, view.total));
    let counter_width = counter.as_deref().map(|counter| text.width(size, counter));
    let query_left = x + input_padding;
    let query_right = match counter_width {
        Some(w) => right - input_padding - w - gap,
        None => right - input_padding,
    };
    // When the query is wider than the room left, it scrolls so its end,
    // where typing happens, stays in view, and the caret with it.
    let caret_width = s(CARET_WIDTH);
    let room = query_right - query_left - caret_width - s(1.0);
    let query_width = text.width(size, view.query);
    let shift = (query_width - room).max(0.0);
    let caret_x = (query_left - shift + query_width + s(1.0)).min(query_right - caret_width);

    let input_top = s(layout.input_top());
    let input_height = s(layout.input_height());
    let message_top = s(layout.message_top());
    let message_height = s(layout.message_height());
    let row_height = s(layout.row());
    let list_top = s(layout.list_top());
    let list_height = s(layout.list_height());
    let scrollbar_width = s(SCROLLBAR_WIDTH);
    let list_right = if theme.scrollbar {
        right - scrollbar_width - s(layout.row_spacing.max(4.0))
    } else {
        right
    };

    // Shapes first, with tiny-skia.
    {
        let mut pixmap =
            PixmapMut::from_bytes(canvas, width, height).expect("canvas size mismatch");
        pixmap.fill(Color::TRANSPARENT);
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        let mut fill = |x: f32, y: f32, w: f32, h: f32, r: f32, rgba: [u8; 4]| {
            if rgba[3] == 0 || w <= 0.0 || h <= 0.0 {
                return;
            }
            paint.set_color(color(rgba));
            let path = rounded_rect(x, y, w, h, r);
            pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        };

        fill(0.0, 0.0, width as f32, height as f32, s(theme.radius), c.background);
        for (pill, background) in [(&badge, c.badge_background), (&prompt, c.prompt_background)] {
            if let Some(pill) = pill {
                fill(pill.left, input_top, pill.width, input_height, row_radius, background);
            }
        }
        if caret_x >= query_left {
            let caret_height = line * 0.9;
            let caret_top = input_top + (input_height - caret_height) / 2.0;
            fill(caret_x, caret_top, caret_width, caret_height, 0.0, c.accent);
        }
        if layout.separator > 0.0 && layout.has_below() {
            let top = s(layout.separator_top());
            fill(left, top, right - left, s(layout.separator).max(1.0), 0.0, c.separator);
        }
        if view.message.is_some() {
            let background = c.message_background;
            fill(left, message_top, right - left, message_height, row_radius, background);
        }
        for i in 0..view.rows.len() {
            let background = if view.selected == Some(i) { c.selected } else { c.row };
            let top = s(layout.row_top(i));
            fill(left, top, list_right - left, row_height, row_radius, background);
        }
        if view.buttons.len() > 1 {
            let top = s(layout.buttons_top());
            for i in 0..view.buttons.len() {
                let (x, w) = layout.button(i);
                let background = if i == view.active { c.button_selected } else { c.button };
                fill(s(x), top, s(w), row_height, row_radius, background);
            }
        }
        if theme.scrollbar && layout.lines > 0 {
            let x = right - scrollbar_width;
            let radius = scrollbar_width / 2.0;
            fill(x, list_top, scrollbar_width, list_height, radius, c.scrollbar);
            let (top, length) = handle(view.scroll, layout.lines, view.matched);
            let (top, length) = (list_top + top * list_height, length * list_height);
            fill(x, top, scrollbar_width, length, radius, c.scrollbar_handle);
        }

        // Last, so it draws over whatever reaches the edge.
        if theme.border_width > 0.0 {
            let inset = s(theme.border_width) / 2.0;
            let card = rounded_rect(
                inset,
                inset,
                width as f32 - 2.0 * inset,
                height as f32 - 2.0 * inset,
                s(theme.radius),
            );
            paint.set_color(color(c.border));
            let stroke = Stroke {
                width: s(theme.border_width),
                ..Stroke::default()
            };
            pixmap.stroke_path(&card, &paint, &stroke, Transform::identity(), None);
        }
    }

    // Then text, blended straight into the same RGBA canvas.
    let mut canvas = Canvas {
        data: canvas,
        width,
        height,
    };
    let clip = |left: f32, right: f32| Clip {
        left: left as i32,
        right: right as i32,
    };

    // Input row: badge, prompt, query or placeholder, caret, counter.
    let y = text_top(input_top, input_height);
    let labels = [
        (&badge, Some(theme.badge.as_str()), c.badge),
        (&prompt, view.prompt, c.prompt),
    ];
    for (pill, label, rgba) in labels {
        if let (Some(pill), Some(label)) = (pill, label) {
            let x = pill.left + input_padding;
            let clip = clip(pill.left, pill.left + pill.width);
            text.draw(&mut canvas, x, y, size, clip, [plain(label, rgba)]);
        }
    }
    if let (Some(counter), Some(w)) = (&counter, counter_width) {
        let x = right - input_padding - w;
        let spans = [plain(counter.as_str(), c.dim)];
        text.draw(&mut canvas, x, y, size, clip(x, right), spans);
    }

    // The query gets its own clip, shifted as worked out above.
    let query_clip = clip(query_left, query_right);
    if view.query.is_empty() && !theme.placeholder.is_empty() {
        let x = query_left + caret_width + s(1.0);
        let placeholder = [plain(theme.placeholder.as_str(), c.placeholder)];
        text.draw(&mut canvas, x, y, size, query_clip, placeholder);
    } else {
        let query = [plain(view.query, c.text)];
        text.draw(&mut canvas, query_left - shift, y, size, query_clip, query);
    }

    if let Some(message) = view.message {
        let x = left + s(layout.message_padding);
        let y = text_top(message_top, message_height);
        let clip = clip(left, right - s(layout.message_padding));
        text.draw(&mut canvas, x, y, size, clip, [plain(message, c.message)]);
    }

    // Match list.
    let row_left = left + s(layout.row_padding);
    let row_clip = clip(left, list_right - s(layout.row_padding));
    for (i, row) in view.rows.iter().enumerate() {
        let y = text_top(s(layout.row_top(i)), row_height);
        let (normal, hit) = if view.selected == Some(i) {
            (c.selected_text, c.selected_match)
        } else {
            (c.text, c.matched)
        };
        let spans = highlight(row.text, &row.indices, row.styles);
        let spans = spans.iter().map(|&(span, matched, style)| {
            // A markup color yields to the match color, so matches stay
            // visible, and to the selection's text color.
            let [r, g, b, a] = match style.color {
                Some([r, g, b]) if !matched && view.selected != Some(i) => [r, g, b, 0xff],
                _ if matched => hit,
                _ => normal,
            };
            let a = style.alpha.map_or(a, |alpha| mul(a, alpha));
            (span, text_color([r, g, b, a]), style)
        });
        text.draw(&mut canvas, row_left, y, size, row_clip, spans);
    }

    if view.buttons.len() > 1 {
        let y = text_top(s(layout.buttons_top()), row_height);
        for (i, label) in view.buttons.iter().enumerate() {
            let (x, w) = layout.button(i);
            let (x, w) = (s(x), s(w));
            let centered = x + (w - text.width(size, label)).max(0.0) / 2.0;
            let rgba = if i == view.active { c.button_selected_text } else { c.button_text };
            text.draw(&mut canvas, centered, y, size, clip(x, x + w), [plain(label, rgba)]);
        }
    }
}

/// The scrollbar handle's top and length as fractions of the track, for
/// `lines` rows shown from rank `scroll` out of `matched`.
fn handle(scroll: u32, lines: u32, matched: u32) -> (f32, f32) {
    if matched <= lines {
        return (0.0, 1.0);
    }
    let length = (lines as f32 / matched as f32).max(0.05);
    let room = (matched - lines) as f32;
    (scroll as f32 / room * (1.0 - length), length)
}

/// Splits `line` into runs that share whether they matched and their
/// markup style. nucleo counts graphemes for non-ASCII haystacks, so byte
/// or char offsets would put the highlight on the wrong letters in
/// accented or emoji names.
fn highlight<'a>(
    line: &'a str,
    indices: &[u32],
    styles: &[(Range<usize>, Style)],
) -> Vec<(&'a str, bool, Style)> {
    let style_at = |offset: usize| {
        styles
            .iter()
            .find(|(range, _)| range.contains(&offset))
            .map_or_else(Style::default, |&(_, style)| style)
    };
    let mut spans = Vec::new();
    let mut hits = indices.iter().peekable();
    let mut start = 0;
    let mut end = 0;
    let mut current: Option<(bool, Style)> = None;
    for (n, (offset, grapheme)) in line.grapheme_indices(true).enumerate() {
        if n == MAX_GRAPHEMES {
            break;
        }
        let run = (hits.next_if(|&&i| i as usize == n).is_some(), style_at(offset));
        if let Some(prev) = current
            && prev != run
        {
            spans.push((&line[start..offset], prev.0, prev.1));
            start = offset;
        }
        current = Some(run);
        end = offset + grapheme.len();
    }
    if let Some((hit, style)) = current {
        spans.push((&line[start..end], hit, style));
    }
    spans
}

/// Text in one color and the default style.
fn plain(text: &str, rgba: [u8; 4]) -> Span<'_> {
    (text, text_color(rgba), Style::default())
}

/// Fills the 1x1 backdrop buffer that the viewporter stretches over the
/// output: premultiplied, and in `wl_shm` byte order.
pub fn backdrop(canvas: &mut [u8], [r, g, b, a]: [u8; 4]) {
    canvas.copy_from_slice(&[mul(b, a), mul(g, a), mul(r, a), a]);
}

// `wl_shm::Format::Argb8888` is BGRA in memory on little endian, while
// tiny-skia and the glyph blitter think in RGBA. Handing them colors with
// red and blue swapped makes them write BGRA directly, so no pass over the
// finished buffer is needed.

fn color([r, g, b, a]: [u8; 4]) -> Color {
    Color::from_rgba8(b, g, r, a)
}

fn text_color([r, g, b, a]: [u8; 4]) -> TextColor {
    TextColor::rgba(b, g, r, a)
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Path {
    let r = r.min(w / 2.0).min(h / 2.0);
    // Control point distance for a cubic approximating a quarter circle.
    let k = r * 0.552_284_8;
    let (right, bottom) = (x + w, y + h);

    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(right - r, y);
    pb.cubic_to(right - r + k, y, right, y + r - k, right, y + r);
    pb.line_to(right, bottom - r);
    pb.cubic_to(right, bottom - r + k, right - r + k, bottom, right - r, bottom);
    pb.line_to(x + r, bottom);
    pb.cubic_to(x + r - k, bottom, x, bottom - r + k, x, bottom - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish().expect("rounded rect path")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs of `highlight` without markup, as (text, matched).
    fn runs<'a>(line: &'a str, indices: &[u32]) -> Vec<(&'a str, bool)> {
        highlight(line, indices, &[])
            .into_iter()
            .map(|(text, hit, _)| (text, hit))
            .collect()
    }

    #[test]
    fn highlight_ascii() {
        assert_eq!(
            runs("foobar", &[0, 1, 4]),
            [("fo", true), ("ob", false), ("a", true), ("r", false)]
        );
    }

    #[test]
    fn highlight_counts_graphemes_not_bytes() {
        // "é" as e + combining accent is one grapheme, three bytes.
        assert_eq!(
            runs("cafe\u{301} au", &[3, 5]),
            [("caf", false), ("e\u{301}", true), (" ", false), ("a", true), ("u", false)]
        );
    }

    #[test]
    fn highlight_splits_at_style_changes() {
        let italic = Style {
            italic: true,
            ..Style::default()
        };
        let spans = highlight("ab cd", &[1, 3], &[(3..5, italic)]);
        let got: Vec<_> = spans.iter().map(|&(t, hit, s)| (t, hit, s.italic)).collect();
        assert_eq!(
            got,
            [("a", false, false), ("b", true, false), (" ", false, false), ("c", true, true), ("d", false, true)]
        );
    }

    /// `cargo test -- --ignored --nocapture` prints the cost of one frame.
    #[test]
    #[ignore = "needs system fonts"]
    fn frame_time() {
        let mut text = Text::load("sans-serif").unwrap();
        let scale = 1.5;
        let layout = Layout::default();
        let theme = Theme::default();
        let (w, h) = layout.size();
        let (w, h) = ((w as f32 * scale) as u32, (h as f32 * scale) as u32);
        let mut canvas = vec![0; (w * h * 4) as usize];
        let rows: Vec<_> = (0..10)
            .map(|i| RowView {
                text: ["src/window.rs", "Cargo.toml", "flake.nix"][i % 3],
                styles: &[],
                indices: vec![0, 2, 4],
            })
            .collect();
        let view = View {
            prompt: Some("run"),
            message: None,
            query: "swr",
            rows,
            selected: Some(2),
            scroll: 0,
            buttons: vec![],
            active: 0,
            matched: 120,
            total: 4000,
        };
        // The first frame fills the glyph cache.
        panel(&mut canvas, w, h, scale, &layout, &theme, &mut text, &view);
        let start = std::time::Instant::now();
        panel(&mut canvas, w, h, scale, &layout, &theme, &mut text, &view);
        eprintln!("frame at {w}x{h} in {:?}", start.elapsed());
    }

    #[test]
    fn backdrop_is_premultiplied_bgra() {
        let mut px = [0; 4];
        backdrop(&mut px, [0xff, 0x00, 0x80, 0x40]);
        // Red ends up in the third byte, every channel scaled by alpha.
        assert_eq!(px, [0x20, 0x00, 0x40, 0x40]);
    }

    #[test]
    fn scrollbar_handle() {
        // Everything fits: the handle fills the track.
        assert_eq!(handle(0, 10, 4), (0.0, 1.0));
        // A tenth shown, at the top and at the very end.
        assert_eq!(handle(0, 10, 100), (0.0, 0.1));
        assert_eq!(handle(90, 10, 100), (0.9, 0.1));
    }

    #[test]
    fn highlight_caps_long_lines() {
        let line = "x".repeat(MAX_GRAPHEMES * 2);
        let spans = highlight(&line, &[], &[]);
        assert_eq!(spans[0].0.len(), MAX_GRAPHEMES);
    }
}
