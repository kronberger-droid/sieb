//! Font loading through fontconfig, shaping through cosmic-text, and glyph
//! blitting into a premultiplied RGBA canvas.

use std::collections::HashSet;
use std::error::Error;
use std::ffi::CString;
use std::path::PathBuf;

use cosmic_text::fontdb::{self, FaceInfo, Language, Source, Stretch, Style, Weight};
use cosmic_text::{
    Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent, Wrap,
};
use fontconfig::{Fontconfig, ObjectSet, Pattern, UnicodeCoverage, list_fonts};

use crate::markup;

/// A borrowed premultiplied RGBA canvas in physical pixels.
pub struct Canvas<'a> {
    pub data: &'a mut [u8],
    pub width: u32,
    pub height: u32,
}

/// Horizontal clip span in physical pixels, so long lines stop at the panel
/// padding instead of running into the border.
#[derive(Clone, Copy)]
pub struct Clip {
    pub left: i32,
    pub right: i32,
}

pub struct Text {
    fonts: FontSystem,
    glyphs: SwashCache,
    family: String,
}

impl Text {
    /// Loads `family` and its fontconfig fallback chain.
    ///
    /// Only the fonts fontconfig sorts for this family get loaded, rather
    /// than every font on the system, which keeps startup cheap while still
    /// covering scripts and emoji the primary font lacks.
    pub fn load(family: &str) -> Result<Self, Box<dyn Error>> {
        let fc = Fontconfig::new().ok_or("cannot initialise fontconfig")?;
        let mut pattern = Pattern::new(&fc)?;
        pattern.add_string(c"family", &CString::new(family)?)?;
        let sorted = pattern.sort_fonts(UnicodeCoverage::Trim)?;

        // Faces are registered from fontconfig's metadata instead of being
        // parsed: the chain runs to hundreds of files, and opening each one
        // cost ~200ms at startup. cosmic-text reads a file only once it
        // actually needs a glyph from it.
        let mut db = fontdb::Database::new();
        let mut seen = HashSet::new();
        for font in sorted.iter() {
            register(&mut db, &mut seen, &font);
        }
        // The family fontconfig resolved, not the configured name: aliases
        // like "sans-serif" would otherwise hit cosmic-text's own generic
        // mapping instead of the user's fontconfig choice.
        let family = db
            .faces()
            .next()
            .and_then(|face| face.families.first())
            .map(|(name, _)| name.clone())
            .ok_or_else(|| format!("no font found for {family:?}"))?;

        // The sort trims faces that add no new characters, which drops the
        // italic and light faces of the family itself. Markup needs those.
        let mut pattern = Pattern::new(&fc)?;
        pattern.add_string(c"family", &CString::new(family.as_str())?)?;
        let mut objects = ObjectSet::new(&fc)?;
        for object in [
            c"family",
            c"file",
            c"index",
            c"slant",
            c"weight",
            c"width",
            c"spacing",
            c"postscriptname",
        ] {
            objects.add(object)?;
        }
        for font in list_fonts(&pattern, Some(&objects))?.iter() {
            register(&mut db, &mut seen, &font);
        }

        Ok(Self {
            fonts: FontSystem::new_with_locale_and_db("en-US".into(), db),
            glyphs: SwashCache::new(),
            family,
        })
    }

    fn shape<'s>(&mut self, size: f32, spans: impl IntoIterator<Item = Span<'s>>) -> Buffer {
        let metrics = Metrics::new(size, Self::line_height(size));
        let mut buffer = Buffer::new(&mut self.fonts, metrics);
        buffer.set_wrap(Wrap::None);
        let attrs = Attrs::new().family(Family::Name(&self.family));
        let spans = spans.into_iter().map(|(s, color, style)| {
            let mut span = attrs.clone().color(color).weight(Weight(style.weight));
            if style.italic {
                span = span.style(Style::Italic);
            }
            if style.scale != 1.0 {
                let size = size * style.scale;
                span = span.metrics(Metrics::new(size, Self::line_height(size)));
            }
            (s, span)
        });
        buffer.set_rich_text(spans, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fonts, false);
        buffer
    }

    /// Width of `text` in physical pixels.
    pub fn width(&mut self, size: f32, text: &str) -> f32 {
        let buffer = self.shape(size, [(text, Color(0), markup::Style::default())]);
        buffer.layout_runs().map(|run| run.line_w).fold(0.0, f32::max)
    }

    /// Height of one line box at `size`, for vertical centering.
    pub fn line_height(size: f32) -> f32 {
        (size * 1.25).ceil()
    }

    /// Draws one line of differently styled spans with its line box's top
    /// left corner at (`x`, `y`). Returns the advance width.
    pub fn draw<'s>(
        &mut self,
        canvas: &mut Canvas,
        x: f32,
        y: f32,
        size: f32,
        clip: Clip,
        spans: impl IntoIterator<Item = Span<'s>>,
    ) -> f32 {
        let buffer = self.shape(size, spans);
        let mut width: f32 = 0.0;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            for glyph in run.glyphs {
                let physical = glyph.physical((x, y + run.line_y), 1.0);
                let color = glyph.color_opt.unwrap_or(Color(0xff_ff_ff_ff));
                if let Some(image) = self.glyphs.get_image(&mut self.fonts, physical.cache_key) {
                    let left = physical.x + image.placement.left;
                    let top = physical.y - image.placement.top;
                    blit(canvas, clip, left, top, image, color);
                }
            }
        }
        width
    }
}

/// Text with the color and style to draw it in.
pub type Span<'s> = (&'s str, Color, markup::Style);

/// Adds one face fontconfig found, from its metadata alone.
fn register(db: &mut fontdb::Database, seen: &mut HashSet<(String, u32)>, font: &Pattern) {
    let (Ok(file), Ok(name)) = (font.filename(), font.get_string(c"family")) else {
        return;
    };
    let index = font.face_index().unwrap_or(0) as u32;
    if !seen.insert((file.to_owned(), index)) {
        return;
    }
    db.push_face_info(FaceInfo {
        id: fontdb::ID::dummy(),
        source: Source::File(PathBuf::from(file)),
        index,
        families: vec![(name.to_owned(), Language::English_UnitedStates)],
        post_script_name: font
            .get_string(c"postscriptname")
            .unwrap_or_default()
            .to_owned(),
        style: match font.slant() {
            Ok(FC_SLANT_ITALIC) => Style::Italic,
            Ok(FC_SLANT_OBLIQUE) => Style::Oblique,
            _ => Style::Normal,
        },
        weight: Weight(font.weight().map_or(400, opentype_weight)),
        stretch: font.width().map_or(Stretch::Normal, stretch),
        monospaced: font.get_int(c"spacing").is_ok_and(|s| s >= FC_MONO),
    });
}

const FC_SLANT_ITALIC: i32 = 100;
const FC_SLANT_OBLIQUE: i32 = 110;
const FC_MONO: i32 = 100;

/// fontconfig's weight scale to OpenType's, interpolating between the named
/// stops the way `FcWeightToOpenType` does.
fn opentype_weight(fc: i32) -> u16 {
    const STOPS: [(i32, i32); 11] = [
        (0, 100),
        (40, 200),
        (50, 300),
        (55, 350),
        (75, 380),
        (80, 400),
        (100, 500),
        (180, 600),
        (200, 700),
        (205, 800),
        (210, 900),
    ];
    let fc = fc.clamp(0, 215);
    let mut prev = STOPS[0];
    for stop in STOPS {
        if fc <= stop.0 {
            if stop.0 == prev.0 {
                return stop.1 as u16;
            }
            return (prev.1 + (fc - prev.0) * (stop.1 - prev.1) / (stop.0 - prev.0)) as u16;
        }
        prev = stop;
    }
    1000
}

/// fontconfig widths are percentages of normal.
fn stretch(width: i32) -> Stretch {
    match width {
        ..=56 => Stretch::UltraCondensed,
        57..=68 => Stretch::ExtraCondensed,
        69..=81 => Stretch::Condensed,
        82..=93 => Stretch::SemiCondensed,
        94..=106 => Stretch::Normal,
        107..=118 => Stretch::SemiExpanded,
        119..=137 => Stretch::Expanded,
        138..=175 => Stretch::ExtraExpanded,
        _ => Stretch::UltraExpanded,
    }
}

fn blit(
    canvas: &mut Canvas,
    clip: Clip,
    left: i32,
    top: i32,
    image: &cosmic_text::SwashImage,
    color: Color,
) {
    let (w, h) = (image.placement.width as i32, image.placement.height as i32);
    let x0 = left.max(clip.left).max(0);
    let x1 = (left + w).min(clip.right).min(canvas.width as i32);
    let y0 = top.max(0);
    let y1 = (top + h).min(canvas.height as i32);

    for y in y0..y1 {
        for x in x0..x1 {
            let src = ((y - top) * w + (x - left)) as usize;
            // Straight alpha source, premultiplied here.
            let (r, g, b, a) = match image.content {
                SwashContent::Mask => {
                    let a = mul(image.data[src], color.a());
                    (color.r(), color.g(), color.b(), a)
                }
                SwashContent::Color => {
                    let px = &image.data[src * 4..src * 4 + 4];
                    (px[0], px[1], px[2], px[3])
                }
                // Only produced when subpixel rendering is requested.
                SwashContent::SubpixelMask => continue,
            };
            if a == 0 {
                continue;
            }
            let dst = ((y as u32 * canvas.width + x as u32) * 4) as usize;
            let px = &mut canvas.data[dst..dst + 4];
            let inv = 255 - a;
            px[0] = mul(r, a) + mul(px[0], inv);
            px[1] = mul(g, a) + mul(px[1], inv);
            px[2] = mul(b, a) + mul(px[2], inv);
            px[3] = a + mul(px[3], inv);
        }
    }
}

/// `a * b / 255`, rounded.
fn mul(a: u8, b: u8) -> u8 {
    let t = a as u32 * b as u32 + 128;
    ((t + (t >> 8)) >> 8) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mul_is_exact_at_the_ends() {
        assert_eq!(mul(255, 255), 255);
        assert_eq!(mul(0, 255), 0);
        assert_eq!(mul(128, 255), 128);
    }

    /// `cargo test -- --ignored --nocapture` prints the startup cost.
    #[test]
    #[ignore = "needs system fonts"]
    fn font_load_time() {
        let start = std::time::Instant::now();
        let mut text = Text::load("sans-serif").unwrap();
        let faces = text.fonts.db().faces().count();
        eprintln!("loaded {:?} ({faces} faces) in {:?}", text.family, start.elapsed());

        // Fallback has to reach fonts that are registered but never parsed.
        let start = std::time::Instant::now();
        let plain = markup::Style::default();
        let buffer = text.shape(15.0, [("日本語 🦀 café", Color(0), plain)]);
        let tofu = buffer
            .layout_runs()
            .flat_map(|run| run.glyphs)
            .filter(|glyph| glyph.glyph_id == 0)
            .count();
        eprintln!("shaped CJK and emoji in {:?}", start.elapsed());
        assert_eq!(tofu, 0, "glyphs fell back to .notdef");
    }
}
