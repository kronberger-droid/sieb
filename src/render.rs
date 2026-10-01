use tiny_skia::{Color, FillRule, Paint, Path, PathBuilder, PixmapMut, Stroke, Transform};

const BACKGROUND: [u8; 4] = [0x1e, 0x1e, 0x2e, 0xf2];
const BORDER: [u8; 4] = [0x58, 0x5b, 0x70, 0xff];
const RADIUS: f32 = 12.0;
const BORDER_WIDTH: f32 = 1.0;

fn color([r, g, b, a]: [u8; 4]) -> Color {
    Color::from_rgba8(r, g, b, a)
}

/// Draws the panel into a `wl_shm` ARGB8888 buffer of `width` x `height`
/// physical pixels. Lengths are given in logical pixels and multiplied by
/// `scale` here, so the result stays crisp at fractional scales.
pub fn panel(canvas: &mut [u8], width: u32, height: u32, scale: f32) {
    let mut pixmap = PixmapMut::from_bytes(canvas, width, height).expect("canvas size mismatch");
    pixmap.fill(Color::TRANSPARENT);

    // Inset by half the border so the stroke lands inside the buffer.
    let inset = BORDER_WIDTH * scale / 2.0;
    let card = rounded_rect(
        inset,
        inset,
        width as f32 - 2.0 * inset,
        height as f32 - 2.0 * inset,
        RADIUS * scale,
    );

    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    paint.set_color(color(BACKGROUND));
    pixmap.fill_path(&card, &paint, FillRule::Winding, Transform::identity(), None);

    paint.set_color(color(BORDER));
    let stroke = Stroke {
        width: BORDER_WIDTH * scale,
        ..Stroke::default()
    };
    pixmap.stroke_path(&card, &paint, &stroke, Transform::identity(), None);

    to_argb8888(canvas);
}

/// Fills the 1x1 backdrop buffer that the viewporter stretches over the
/// output. Premultiplied, like everything `wl_shm` hands to the compositor.
pub fn backdrop(canvas: &mut [u8]) {
    // A light dim, so it reads as modal without hiding what is behind it.
    canvas.copy_from_slice(&[0, 0, 0, 0x40]);
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
