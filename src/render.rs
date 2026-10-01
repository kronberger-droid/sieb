use cosmic_text::Color as TextColor;
use tiny_skia::{Color, FillRule, Paint, Path, PathBuilder, PixmapMut, Stroke, Transform};
use unicode_segmentation::UnicodeSegmentation;

use crate::layout::Layout;
use crate::text::{Canvas, Clip, Text};

/// Display cap per row. `Wrap::None` still shapes the whole line, so one
/// minified JSON blob on stdin would otherwise stall every frame.
const MAX_GRAPHEMES: usize = 300;
const CARET_WIDTH: f32 = 2.0;
const PROMPT_GAP: f32 = 8.0;

/// Colors as straight (not premultiplied) RGBA.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub background: [u8; 4],
    pub border: [u8; 4],
    pub selected: [u8; 4],
    pub separator: [u8; 4],
    pub text: [u8; 4],
    pub dim: [u8; 4],
    pub accent: [u8; 4],
    pub backdrop: [u8; 4],
    pub radius: f32,
    pub border_width: f32,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            background: [0x1e, 0x1e, 0x2e, 0xf2],
            border: [0x58, 0x5b, 0x70, 0xff],
            selected: [0x31, 0x32, 0x44, 0xff],
            separator: [0x31, 0x32, 0x44, 0xff],
            text: [0xcd, 0xd6, 0xf4, 0xff],
            dim: [0x7f, 0x84, 0x9c, 0xff],
            accent: [0xf5, 0xc2, 0xe7, 0xff],
            // A light dim, so it reads as modal without hiding what is behind.
            backdrop: [0x00, 0x00, 0x00, 0x40],
            radius: 12.0,
            border_width: 1.0,
        }
    }
}

/// Everything one frame shows, in logical terms.
pub struct View<'a> {
    pub prompt: Option<&'a str>,
    pub message: Option<&'a str>,
    pub query: &'a str,
    /// Visible rows with the grapheme indices that matched.
    pub rows: Vec<(&'a str, Vec<u32>)>,
    /// Index into `rows`.
    pub selected: Option<usize>,
    pub matched: u32,
    pub total: u32,
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
    let pad = s(layout.padding);
    let row = s(layout.row);

    // Shapes first, with tiny-skia.
    {
        let mut pixmap =
            PixmapMut::from_bytes(canvas, width, height).expect("canvas size mismatch");
        pixmap.fill(Color::TRANSPARENT);

        // Inset by half the border so the stroke lands inside the buffer.
        let inset = s(theme.border_width) / 2.0;
        let card = rounded_rect(
            inset,
            inset,
            width as f32 - 2.0 * inset,
            height as f32 - 2.0 * inset,
            s(theme.radius),
        );
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(color(theme.background));
        pixmap.fill_path(&card, &paint, FillRule::Winding, Transform::identity(), None);
        if theme.border_width > 0.0 {
            paint.set_color(color(theme.border));
            let stroke = Stroke {
                width: s(theme.border_width),
                ..Stroke::default()
            };
            pixmap.stroke_path(&card, &paint, &stroke, Transform::identity(), None);
        }

        let separator = rounded_rect(
            pad,
            s(layout.separator_top()),
            width as f32 - 2.0 * pad,
            s(layout.separator_height()).max(1.0),
            0.0,
        );
        paint.set_color(color(theme.separator));
        pixmap.fill_path(&separator, &paint, FillRule::Winding, Transform::identity(), None);

        if let Some(selected) = view.selected {
            let highlight = rounded_rect(
                pad / 2.0,
                s(layout.row_top(selected)),
                width as f32 - pad,
                row,
                s(theme.radius / 2.0),
            );
            paint.set_color(color(theme.selected));
            pixmap.fill_path(&highlight, &paint, FillRule::Winding, Transform::identity(), None);
        }
    }

    // Then text, blended straight into the same RGBA canvas.
    let mut canvas = Canvas {
        data: canvas,
        width,
        height,
    };
    let size = s(layout.font_size);
    let baseline_offset = (row - Text::line_height(size)) / 2.0;
    let text_left = pad * 1.5;
    let clip = Clip {
        left: pad as i32,
        right: (width as f32 - pad) as i32,
    };

    // Input row: prompt, query, caret, and the counter on the right.
    let y = s(layout.input_top()) + baseline_offset;
    let counter = format!("{}/{}", view.matched, view.total);
    let counter_x = width as f32 - text_left - text.width(size, &counter);
    text.draw(
        &mut canvas,
        counter_x,
        y,
        size,
        clip,
        [(counter.as_str(), text_color(theme.dim))],
    );

    let mut query_left = text_left;
    if let Some(prompt) = view.prompt {
        let prompt_clip = Clip {
            right: (counter_x - pad) as i32,
            ..clip
        };
        query_left +=
            text.draw(&mut canvas, text_left, y, size, prompt_clip, [(prompt, text_color(theme.accent))]);
        query_left += s(PROMPT_GAP);
    }
    // The query gets its own clip starting after the prompt. When it is wider
    // than the room left, it scrolls so its end, where typing happens, stays
    // in view.
    let query_right = counter_x - pad;
    let caret_width = s(CARET_WIDTH);
    let room = query_right - query_left - caret_width - s(1.0);
    let query_width = text.width(size, view.query);
    let shift = (query_width - room).max(0.0);
    let query_clip = Clip {
        left: query_left as i32,
        right: query_right as i32,
    };
    text.draw(
        &mut canvas,
        query_left - shift,
        y,
        size,
        query_clip,
        [(view.query, text_color(theme.text))],
    );
    let caret_x = (query_left - shift + query_width + s(1.0)).min(query_right - caret_width);
    if caret_x >= query_left {
        fill(
            &mut canvas,
            theme.accent,
            caret_x,
            s(layout.input_top()) + row * 0.25,
            caret_width,
            row * 0.5,
        );
    }

    if let Some(message) = view.message {
        let y = s(layout.message_top()) + baseline_offset;
        text.draw(&mut canvas, text_left, y, size, clip, [(message, text_color(theme.dim))]);
    }

    // Match list.
    for (i, (line, indices)) in view.rows.iter().enumerate() {
        let y = s(layout.row_top(i)) + baseline_offset;
        let spans = highlight(line, indices);
        text.draw(
            &mut canvas,
            text_left,
            y,
            size,
            clip,
            spans.iter().map(|&(span, hit)| {
                (span, text_color(if hit { theme.accent } else { theme.text }))
            }),
        );
    }

    to_argb8888(canvas.data);
}

/// Splits `line` into runs of matched and unmatched graphemes. nucleo counts
/// graphemes for non-ASCII haystacks, so byte or char offsets would put the
/// highlight on the wrong letters in accented or emoji names.
fn highlight<'a>(line: &'a str, indices: &[u32]) -> Vec<(&'a str, bool)> {
    let mut spans = Vec::new();
    let mut hits = indices.iter().peekable();
    let mut start = 0;
    let mut end = 0;
    let mut current = None;
    for (n, (offset, grapheme)) in line.grapheme_indices(true).enumerate() {
        if n == MAX_GRAPHEMES {
            break;
        }
        let hit = hits.next_if(|&&i| i as usize == n).is_some();
        if let Some(prev) = current
            && prev != hit
        {
            spans.push((&line[start..offset], prev));
            start = offset;
        }
        current = Some(hit);
        end = offset + grapheme.len();
    }
    if let Some(hit) = current {
        spans.push((&line[start..end], hit));
    }
    spans
}

/// Replaces a rectangle of the RGBA canvas with `color`.
fn fill(canvas: &mut Canvas, color: [u8; 4], x: f32, y: f32, w: f32, h: f32) {
    let px = premultiply(color);
    for py in y.round() as u32..(y + h).round() as u32 {
        for pxx in x.round() as u32..(x + w).round() as u32 {
            if pxx < canvas.width && py < canvas.height {
                let i = ((py * canvas.width + pxx) * 4) as usize;
                canvas.data[i..i + 4].copy_from_slice(&px);
            }
        }
    }
}

/// Fills the 1x1 backdrop buffer that the viewporter stretches over the
/// output: premultiplied, and in `wl_shm` byte order.
pub fn backdrop(canvas: &mut [u8], color: [u8; 4]) {
    canvas.copy_from_slice(&premultiply(color));
    to_argb8888(canvas);
}

fn premultiply([r, g, b, a]: [u8; 4]) -> [u8; 4] {
    let m = |c: u8| ((c as u32 * a as u32 + 127) / 255) as u8;
    [m(r), m(g), m(b), a]
}

fn color([r, g, b, a]: [u8; 4]) -> Color {
    Color::from_rgba8(r, g, b, a)
}

fn text_color([r, g, b, a]: [u8; 4]) -> TextColor {
    TextColor::rgba(r, g, b, a)
}

/// tiny-skia writes premultiplied RGBA, while `wl_shm::Format::Argb8888` is
/// BGRA in memory on little endian.
fn to_argb8888(canvas: &mut [u8]) {
    for px in canvas.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
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

    #[test]
    fn highlight_ascii() {
        assert_eq!(
            highlight("foobar", &[0, 1, 4]),
            [("fo", true), ("ob", false), ("a", true), ("r", false)]
        );
    }

    #[test]
    fn highlight_counts_graphemes_not_bytes() {
        // "é" as e + combining accent is one grapheme, three bytes.
        assert_eq!(
            highlight("cafe\u{301} au", &[3, 5]),
            [("caf", false), ("e\u{301}", true), (" ", false), ("a", true), ("u", false)]
        );
    }

    /// `cargo test -- --ignored --nocapture` prints the cost of one frame.
    #[test]
    #[ignore = "needs system fonts"]
    fn frame_time() {
        let mut text = Text::load("sans-serif").unwrap();
        let scale = 1.5;
        let layout = Layout::new(640, 10, 15.0, 10.0);
        let theme = Theme::default();
        let (w, h) = layout.size();
        let (w, h) = ((w as f32 * scale) as u32, (h as f32 * scale) as u32);
        let mut canvas = vec![0; (w * h * 4) as usize];
        let rows: Vec<_> = (0..10)
            .map(|i| (["src/window.rs", "Cargo.toml", "flake.nix"][i % 3], vec![0, 2, 4]))
            .collect();
        let view = View {
            prompt: Some("run"),
            message: None,
            query: "swr",
            rows,
            selected: Some(2),
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
    fn highlight_caps_long_lines() {
        let line = "x".repeat(MAX_GRAPHEMES * 2);
        let spans = highlight(&line, &[]);
        assert_eq!(spans[0].0.len(), MAX_GRAPHEMES);
    }
}
