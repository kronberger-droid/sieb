use cosmic_text::Color as TextColor;
use tiny_skia::{Color, FillRule, Paint, Path, PathBuilder, PixmapMut, Stroke, Transform};
use unicode_segmentation::UnicodeSegmentation;

use crate::text::{Canvas, Clip, Text};

const BACKGROUND: [u8; 4] = [0x1e, 0x1e, 0x2e, 0xf2];
const BORDER: [u8; 4] = [0x58, 0x5b, 0x70, 0xff];
const SELECTED: [u8; 4] = [0x31, 0x32, 0x44, 0xff];
const SEPARATOR: [u8; 4] = [0x31, 0x32, 0x44, 0xff];
const TEXT: [u8; 4] = [0xcd, 0xd6, 0xf4, 0xff];
const DIM: [u8; 4] = [0x7f, 0x84, 0x9c, 0xff];
const ACCENT: [u8; 4] = [0xf5, 0xc2, 0xe7, 0xff];

const RADIUS: f32 = 12.0;
const BORDER_WIDTH: f32 = 1.0;
const FONT_SIZE: f32 = 15.0;
const PADDING: f32 = 10.0;
const ROW: f32 = 32.0;
const SEPARATOR_GAP: f32 = 6.0;
const WIDTH: u32 = 640;
/// Display cap per row. `Wrap::None` still shapes the whole line, so one
/// minified JSON blob on stdin would otherwise stall every frame.
const MAX_GRAPHEMES: usize = 300;

/// Everything one frame shows, in logical terms.
pub struct View<'a> {
    pub prompt: Option<&'a str>,
    pub query: &'a str,
    /// Visible rows with the grapheme indices that matched.
    pub rows: Vec<(&'a str, Vec<u32>)>,
    /// Index into `rows`.
    pub selected: Option<usize>,
    pub matched: u32,
    pub total: u32,
}

/// Logical panel size for `lines` visible rows. Fixed, so the card does not
/// jump around while the match count changes.
pub fn panel_size(lines: u32) -> (u32, u32) {
    let height = 2.0 * PADDING + ROW + 1.0 + SEPARATOR_GAP + lines as f32 * ROW;
    (WIDTH, height.ceil() as u32)
}

/// Draws the panel into a `wl_shm` ARGB8888 buffer of `width` x `height`
/// physical pixels. Lengths are given in logical pixels and multiplied by
/// `scale` here, so the result stays crisp at fractional scales.
pub fn panel(canvas: &mut [u8], width: u32, height: u32, scale: f32, text: &mut Text, view: &View) {
    let s = |v: f32| v * scale;
    let list_top = s(PADDING + ROW + 1.0 + SEPARATOR_GAP);

    // Shapes first, with tiny-skia.
    {
        let mut pixmap =
            PixmapMut::from_bytes(canvas, width, height).expect("canvas size mismatch");
        pixmap.fill(Color::TRANSPARENT);

        // Inset by half the border so the stroke lands inside the buffer.
        let inset = s(BORDER_WIDTH) / 2.0;
        let card = rounded_rect(
            inset,
            inset,
            width as f32 - 2.0 * inset,
            height as f32 - 2.0 * inset,
            s(RADIUS),
        );
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        paint.set_color(color(BACKGROUND));
        pixmap.fill_path(&card, &paint, FillRule::Winding, Transform::identity(), None);
        paint.set_color(color(BORDER));
        let stroke = Stroke {
            width: s(BORDER_WIDTH),
            ..Stroke::default()
        };
        pixmap.stroke_path(&card, &paint, &stroke, Transform::identity(), None);

        let separator = rounded_rect(
            s(PADDING),
            s(PADDING + ROW),
            width as f32 - s(2.0 * PADDING),
            s(1.0).max(1.0),
            0.0,
        );
        paint.set_color(color(SEPARATOR));
        pixmap.fill_path(&separator, &paint, FillRule::Winding, Transform::identity(), None);

        if let Some(selected) = view.selected {
            let row = rounded_rect(
                s(PADDING / 2.0),
                list_top + selected as f32 * s(ROW),
                width as f32 - s(PADDING),
                s(ROW),
                s(6.0),
            );
            paint.set_color(color(SELECTED));
            pixmap.fill_path(&row, &paint, FillRule::Winding, Transform::identity(), None);
        }
    }

    // Then text, blended straight into the same RGBA canvas.
    let mut canvas = Canvas {
        data: canvas,
        width,
        height,
    };
    let size = s(FONT_SIZE);
    let baseline_offset = (s(ROW) - Text::line_height(size)) / 2.0;
    let clip = Clip {
        left: s(PADDING) as i32,
        right: (width as f32 - s(PADDING)) as i32,
    };

    // Input row: prompt, query, caret, and the counter on the right.
    let counter = format!("{}/{}", view.matched, view.total);
    let counter_width = text.width(size, &counter);
    let counter_x = width as f32 - s(PADDING * 1.5) - counter_width;
    let y = s(PADDING) + baseline_offset;
    text.draw(
        &mut canvas,
        counter_x,
        y,
        size,
        clip,
        [(counter.as_str(), text_color(DIM))],
    );

    let input_clip = Clip {
        right: (counter_x - s(PADDING)) as i32,
        ..clip
    };
    let mut x = s(PADDING * 1.5);
    if let Some(prompt) = view.prompt {
        x += text.draw(&mut canvas, x, y, size, input_clip, [(prompt, text_color(ACCENT))]);
        x += s(8.0);
    }
    x += text.draw(&mut canvas, x, y, size, input_clip, [(view.query, text_color(TEXT))]);
    caret(&mut canvas, x + s(1.0), s(PADDING) + s(ROW) * 0.25, s(2.0), s(ROW) * 0.5);

    // Match list.
    for (i, (line, indices)) in view.rows.iter().enumerate() {
        let y = list_top + i as f32 * s(ROW) + baseline_offset;
        let spans = highlight(line, indices);
        text.draw(
            &mut canvas,
            s(PADDING * 1.5),
            y,
            size,
            clip,
            spans
                .iter()
                .map(|&(span, hit)| (span, text_color(if hit { ACCENT } else { TEXT }))),
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

fn caret(canvas: &mut Canvas, x: f32, y: f32, w: f32, h: f32) {
    let [r, g, b, a] = ACCENT;
    for py in y.round() as u32..(y + h).round() as u32 {
        for px in x.round() as u32..(x + w).round() as u32 {
            if px < canvas.width && py < canvas.height {
                let i = ((py * canvas.width + px) * 4) as usize;
                canvas.data[i..i + 4].copy_from_slice(&[r, g, b, a]);
            }
        }
    }
}

/// Fills the 1x1 backdrop buffer that the viewporter stretches over the
/// output. Premultiplied, like everything `wl_shm` hands to the compositor.
pub fn backdrop(canvas: &mut [u8]) {
    // A light dim, so it reads as modal without hiding what is behind it.
    canvas.copy_from_slice(&[0, 0, 0, 0x40]);
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

    #[test]
    fn highlight_caps_long_lines() {
        let line = "x".repeat(MAX_GRAPHEMES * 2);
        let spans = highlight(&line, &[]);
        assert_eq!(spans[0].0.len(), MAX_GRAPHEMES);
    }
}
