//! Bitmap fonts: glyphs in the UI atlas, built from grids of cells (Minecraft's bitmap font
//! provider), from 8x8 bit rows (the engine's default font) or composed from other glyphs.

use crate::atlas::{AtlasError, UiAtlas};
use crate::font8x8;
use boxel_ecs::prelude::*;
use std::collections::{BTreeMap, HashMap};

/// The height of a glyph in GUI pixels at text size 1 (whatever the source cell's height).
pub const GLYPH_HEIGHT: u32 = 8;
/// The distance between two lines of text in GUI pixels at text size 1.
pub const LINE_HEIGHT: u32 = 9;
/// The advance of a space in GUI pixels at text size 1.
pub const SPACE_ADVANCE: u32 = 4;

/// A glyph of a [`BitmapFont`], sizes in GUI pixels at text size 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyph {
    /// `[x, y, w, h]` in atlas texels (`w` or `h` 0 for a glyph that draws nothing).
    pub src: [u32; 4],
    /// The drawn size.
    pub size: [u32; 2],
    /// Rows from the top of the line to the top of the glyph (negative above it, for marks
    /// over capitals).
    pub top: i32,
    /// How far the pen moves after it.
    pub advance: u32,
}

/// A bitmap font: one glyph per character, a fallback chain resolved when it was built, and
/// `'?'` for anything else. Characters are drawn one by one (no shaping, no right-to-left).
///
/// `UiPlugin` inserts the engine's default ([`default_font`]); a game replaces this resource
/// with one built by [`FontBuilder`] (from a pack's font image, for example).
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct BitmapFont {
    glyphs: HashMap<char, Glyph>,
}

impl BitmapFont {
    /// The glyph of `c`, if the font has one.
    pub fn glyph(&self, c: char) -> Option<&Glyph> {
        self.glyphs.get(&c)
    }

    /// The glyph drawn for `c`: its own, else `'?'`, else nothing (a 1-pixel advance).
    pub fn resolve(&self, c: char) -> Glyph {
        self.glyph(c)
            .or_else(|| self.glyph('?'))
            .copied()
            .unwrap_or(Glyph {
                src: [0; 4],
                size: [0, 0],
                top: 0,
                advance: 1,
            })
    }

    /// Whether `c` has its own glyph.
    pub fn has(&self, c: char) -> bool {
        self.glyphs.contains_key(&c)
    }

    /// How many characters have a glyph.
    pub fn len(&self) -> usize {
        self.glyphs.len()
    }

    /// No glyph at all.
    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }
}

/// An RGBA8 image for building glyphs (sRGB, straight alpha). Glyphs are usually white and get
/// their color from the text's tint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GlyphBitmap {
    /// Width in texels.
    pub w: u32,
    /// Height in texels.
    pub h: u32,
    /// `w * h` texels, row by row.
    pub rgba: Vec<u8>,
}

impl GlyphBitmap {
    /// A transparent image.
    pub fn new(w: u32, h: u32) -> Self {
        Self {
            w,
            h,
            rgba: vec![0; (w * h * 4) as usize],
        }
    }

    /// A white image drawn with text rows: `#` is an opaque texel, anything else transparent.
    /// Rows may differ in length; the widest sets the width.
    pub fn from_rows(rows: &[&str]) -> Self {
        let w = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0) as u32;
        let mut image = Self::new(w, rows.len() as u32);
        for (y, row) in rows.iter().enumerate() {
            for (x, c) in row.chars().enumerate() {
                if c == '#' {
                    image.set(x as u32, y as u32, [255; 4]);
                }
            }
        }
        image
    }

    /// An 8 by 8 white glyph from `font8x8` rows (bit 0 is the leftmost texel).
    pub fn from_bits(rows: [u8; 8]) -> Self {
        let mut image = Self::new(8, 8);
        for (y, bits) in rows.iter().enumerate() {
            for x in 0..8 {
                if bits & (1 << x) != 0 {
                    image.set(x, y as u32, [255; 4]);
                }
            }
        }
        image
    }

    /// The texel at `(x, y)` (transparent outside).
    pub fn get(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.w || y >= self.h {
            return [0; 4];
        }
        let i = ((y * self.w + x) * 4) as usize;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }

    /// Sets the texel at `(x, y)` (ignored outside).
    pub fn set(&mut self, x: u32, y: u32, texel: [u8; 4]) {
        if x < self.w && y < self.h {
            let i = ((y * self.w + x) * 4) as usize;
            self.rgba[i..i + 4].copy_from_slice(&texel);
        }
    }

    /// The `w` by `h` part at `(x, y)`.
    pub fn crop(&self, x: u32, y: u32, w: u32, h: u32) -> Self {
        let mut out = Self::new(w, h);
        for dy in 0..h {
            for dx in 0..w {
                out.set(dx, dy, self.get(x + dx, y + dy));
            }
        }
        out
    }

    /// One more than the rightmost column with a texel of alpha above 0 (0 when empty): the
    /// glyph's width by Minecraft's rule.
    pub fn used_width(&self) -> u32 {
        (0..self.w)
            .rev()
            .find(|&x| (0..self.h).any(|y| self.get(x, y)[3] > 0))
            .map_or(0, |x| x + 1)
    }

    fn rotated_180(&self) -> Self {
        let mut out = Self::new(self.w, self.h);
        for y in 0..self.h {
            for x in 0..self.w {
                out.set(self.w - 1 - x, self.h - 1 - y, self.get(x, y));
            }
        }
        out
    }
}

/// How [`FontBuilder::derive`] makes a glyph from another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Derive {
    /// Turned upside down (`¿` from `?`, `¡` from `!`).
    Rotate180,
}

#[derive(Clone, Debug)]
struct Pending {
    image: GlyphBitmap,
    /// Texel rows per [`GLYPH_HEIGHT`] GUI pixels (the source cell height).
    cell_h: u32,
    /// Top in texels from the line top.
    top: i32,
}

/// Builds a [`BitmapFont`]: the mechanism is the engine's, the images and character tables come
/// from the game (a pack's `font/ascii.png`, marks it draws itself).
///
/// Per character the font uses the first of: a glyph added here (grid, bits, derived or
/// composed), the fallback font, `'?'`. Every glyph is drawn [`GLYPH_HEIGHT`] GUI pixels tall
/// for its source cell height, so a 16-pixel pack font draws at the same size as an 8-pixel
/// one.
#[derive(Clone, Debug, Default)]
pub struct FontBuilder {
    glyphs: BTreeMap<char, Pending>,
    fallback: Option<BitmapFont>,
}

impl FontBuilder {
    /// An empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cuts `image` into cells of `cell` texels; row `i` of the image holds the characters of
    /// `rows[i]`, left to right (Minecraft's bitmap provider). `'\0'` marks an unused cell. Each
    /// glyph keeps its used width.
    pub fn grid(&mut self, image: &GlyphBitmap, cell: [u32; 2], rows: &[&str]) -> &mut Self {
        let [cw, ch] = cell;
        for (row, chars) in rows.iter().enumerate() {
            for (col, c) in chars.chars().enumerate() {
                if c == '\0' {
                    continue;
                }
                let glyph = image.crop(col as u32 * cw, row as u32 * ch, cw, ch);
                self.add(c, glyph, ch, 0);
            }
        }
        self
    }

    /// Adds one glyph from 8x8 bit rows, as the engine's default font.
    pub fn bits(&mut self, c: char, rows: [u8; 8]) -> &mut Self {
        self.add(c, GlyphBitmap::from_bits(rows), 8, 0)
    }

    /// Adds `c` from an image `cell_h` texels tall per 8 GUI pixels, its top `top` texels below
    /// the line's top.
    pub fn add(&mut self, c: char, image: GlyphBitmap, cell_h: u32, top: i32) -> &mut Self {
        let w = image.used_width();
        let image = image.crop(0, 0, w, image.h);
        self.glyphs.insert(
            c,
            Pending {
                image,
                cell_h: cell_h.max(1),
                top,
            },
        );
        self
    }

    /// Adds `c` made from the glyph of `from` (added earlier). Nothing happens without it.
    pub fn derive(&mut self, c: char, from: char, how: Derive) -> &mut Self {
        if let Some(source) = self.glyphs.get(&from).cloned() {
            let image = match how {
                Derive::Rotate180 => source.image.rotated_180(),
            };
            self.add(c, image, source.cell_h, source.top);
        }
        self
    }

    /// Adds `c` as the glyph of `base` with `mark` drawn over it, centered, its top `dy` texels
    /// below the base's top (negative: above it, and the composed glyph grows upwards). For
    /// accented letters a pack lacks: `mark('á', 'a', &acute, -2)`. Nothing happens without
    /// `base`.
    pub fn mark(&mut self, c: char, base: char, mark: &GlyphBitmap, dy: i32) -> &mut Self {
        let Some(source) = self.glyphs.get(&base).cloned() else {
            return self;
        };
        let base_img = &source.image;
        let above = (-dy).max(0) as u32;
        let w = base_img.w.max(mark.w);
        let h = (base_img.h + above).max((dy + mark.h as i32).max(0) as u32 + above);
        let mut image = GlyphBitmap::new(w, h);
        let paste = |image: &mut GlyphBitmap, from: &GlyphBitmap, x0: u32, y0: u32| {
            for y in 0..from.h {
                for x in 0..from.w {
                    let texel = from.get(x, y);
                    if texel[3] > 0 {
                        image.set(x0 + x, y0 + y, texel);
                    }
                }
            }
        };
        paste(&mut image, base_img, 0, above);
        let mark_x = (base_img.w.saturating_sub(mark.w)) / 2;
        paste(&mut image, mark, mark_x, (dy + above as i32) as u32);
        self.add(c, image, source.cell_h, source.top - above as i32)
    }

    /// Characters this builder has no glyph for come from `font`.
    pub fn fallback(&mut self, font: BitmapFont) -> &mut Self {
        self.fallback = Some(font);
        self
    }

    /// Places every glyph in `atlas` and returns the font.
    pub fn build(&self, atlas: &mut UiAtlas) -> Result<BitmapFont, AtlasError> {
        let mut glyphs = self
            .fallback
            .as_ref()
            .map(|font| font.glyphs.clone())
            .unwrap_or_default();
        for (&c, pending) in &self.glyphs {
            let image = &pending.image;
            let src = if image.w > 0 && image.h > 0 {
                let at = atlas.add_image(image.w, image.h, &image.rgba)?;
                [at[0], at[1], image.w, image.h]
            } else {
                [0, 0, 0, 0]
            };
            let to_gui = |texels: u32| (texels * GLYPH_HEIGHT).div_ceil(pending.cell_h);
            let width = to_gui(image.w);
            let advance = match c {
                ' ' | '\u{a0}' => SPACE_ADVANCE,
                _ => width + 1,
            };
            glyphs.insert(
                c,
                Glyph {
                    src,
                    size: [width, to_gui(image.h)],
                    top: pending.top * GLYPH_HEIGHT as i32 / pending.cell_h as i32,
                    advance,
                },
            );
        }
        Ok(BitmapFont { glyphs })
    }
}

/// The engine's default font in `atlas`: `font8x8`'s printable ASCII and Latin-1 (public
/// domain). It looks like the VGA text font, not like any game's.
pub fn default_font(atlas: &mut UiAtlas) -> Result<BitmapFont, AtlasError> {
    let mut builder = FontBuilder::new();
    for code in (0x20..=0x7E).chain(0xA0..=0xFF) {
        let c = char::from_u32(code).unwrap_or('?');
        if let Some(rows) = font8x8::glyph(c) {
            builder.bits(c, *rows);
        }
    }
    builder.build(atlas)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_font_follows_the_advance_rule() {
        let mut atlas = UiAtlas::new(256);
        let font = default_font(&mut atlas).unwrap();
        assert_eq!(font.len(), 95 + 96);
        // 'A' uses columns 0..=5: width 6, advance 7.
        let a = font.resolve('A');
        assert_eq!((a.size, a.advance, a.top), ([6, 8], 7, 0));
        // 'M' uses 0..=6.
        assert_eq!(font.resolve('M').advance, 8);
        // Space is 4 and draws nothing.
        let space = font.resolve(' ');
        assert_eq!((space.advance, space.src[2]), (4, 0));
        assert_eq!(font.resolve('\u{a0}').advance, 4);
        // Unknown characters draw '?'.
        assert_eq!(font.resolve('\u{4e2d}'), font.resolve('?'));
        // The glyph's texels are the font's bits, white.
        let [x, y, ..] = a.src;
        assert_eq!(atlas.texel(x + 2, y), [255; 4]);
        assert_eq!(atlas.texel(x, y), [0; 4]);
        for c in "áéíóúñüÁÉÍÓÚÑÜ¿¡".chars() {
            assert!(font.has(c), "{c}");
        }
    }

    #[test]
    fn grid_derive_mark_and_fallback() {
        // A 2-cell grid of 4x4 cells holding 'i' (1 column used) and 'w' (3 columns).
        let mut image = GlyphBitmap::new(8, 4);
        for y in 0..4 {
            image.set(1, y, [255; 4]);
            image.set(4, y, [255; 4]);
            image.set(6, y, [255; 4]);
        }
        image.set(0, 3, [255, 255, 255, 0]);
        let mut atlas = UiAtlas::new(64);
        let mut base = FontBuilder::new();
        base.bits('?', [0xFF; 8]);
        let fallback = base.build(&mut atlas).unwrap();

        let acute = GlyphBitmap::from_rows(&[".#", "#"]);
        let mut builder = FontBuilder::new();
        builder
            .grid(&image, [4, 4], &["i\0", "", "w"])
            .grid(&image, [4, 4], &["\0w"])
            .derive('j', 'i', Derive::Rotate180)
            .mark('í', 'i', &acute, -3)
            .fallback(fallback.clone());
        let font = builder.build(&mut atlas).unwrap();
        // 4-texel cells draw 8 GUI pixels tall: every size doubles.
        let i = font.resolve('i');
        assert_eq!((i.size, i.advance), ([4, 8], 5));
        let w = font.resolve('w');
        assert_eq!((w.size, w.advance), ([6, 8], 7));
        assert!(font.has('j'));
        let accented = font.resolve('í');
        // Width max(2, 2) texels, 3 texels taller, its top 6 GUI pixels above the line top.
        assert_eq!((accented.size, accented.top), ([4, 14], -6));
        assert_eq!(font.resolve('?'), fallback.resolve('?'));
        assert_eq!(font.resolve('z'), fallback.resolve('?'));
    }
}
