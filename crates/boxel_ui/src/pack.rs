//! GUI sprites and fonts from a Minecraft resource pack (feature `pack`): `boxel_pack`'s
//! [`GuiPack`](boxel_pack::GuiPack) reads them, these put them in the atlas and a font.
//!
//! The game names what it reads and where it goes; the engine names no sprite:
//!
//! ```rust,no_run
//! # use boxel_ui::{FontBuilder, SkinSlot, UiAtlas, UiSkin, default_font};
//! # use boxel_pack::{GuiPack, ResourceId};
//! # fn load(dir: &std::path::Path, atlas: &mut UiAtlas, skin: &mut UiSkin) -> Option<()> {
//! let pack = GuiPack::open(dir).ok()?;
//! if let Ok(Some(button)) = pack.sprite(&ResourceId::parse("minecraft:widget/button")) {
//!     let id = atlas.insert_pack_sprite("game:button", &button).ok()?;
//!     skin.set(SkinSlot::Button, id);
//! }
//! if let Ok(Some(font)) = pack.font(&ResourceId::parse("minecraft:default")) {
//!     let fallback = default_font(atlas).ok()?;
//!     let font = FontBuilder::new().pack_font(&font).fallback(fallback).build(atlas).ok()?;
//! }
//! # Some(())
//! # }
//! ```

use crate::atlas::{AtlasError, SpriteId, UiAtlas};
use crate::font::{FontBuilder, GlyphBitmap};
use crate::sprite::Scaling;
use boxel_pack::{FontProvider, GuiScaling, GuiTexture, PackFont};
use std::collections::HashMap;

impl From<GuiScaling> for Scaling {
    fn from(scaling: GuiScaling) -> Self {
        match scaling {
            GuiScaling::Stretch => Self::Stretch,
            GuiScaling::Tile { width, height } => Self::Tile {
                w: width,
                h: height,
            },
            GuiScaling::NineSlice {
                width,
                height,
                border,
                stretch_inner,
            } => Self::NineSlice {
                w: width,
                h: height,
                border,
                stretch_inner,
            },
        }
    }
}

impl UiAtlas {
    /// Adds a pack's GUI texture under `key`, scaled as its `gui.scaling` says (stretched
    /// without one).
    pub fn insert_pack_sprite(
        &mut self,
        key: &str,
        texture: &GuiTexture,
    ) -> Result<SpriteId, AtlasError> {
        self.insert_sprite(
            key,
            texture.width,
            texture.height,
            &texture.rgba,
            texture.scaling.into(),
        )
    }
}

impl FontBuilder {
    /// Adds the glyphs of a pack's font: its bitmap providers with their `height` and `ascent`
    /// ([`FontBuilder::bitmap`]) and its space providers (advances rounded to whole GUI pixels,
    /// negative ones to 0). Within the font the first provider with a character wins, as in
    /// Minecraft; its glyphs replace those added to this builder before.
    pub fn pack_font(&mut self, font: &PackFont) -> &mut Self {
        // Providers that name the same image share it: copy it out once.
        let mut images: HashMap<(*const u8, u32, u32), GlyphBitmap> = HashMap::new();
        for provider in font.providers.iter().rev() {
            match provider {
                FontProvider::Bitmap(bitmap) => {
                    let image = images
                        .entry((bitmap.rgba.as_ptr(), bitmap.width, bitmap.height))
                        .or_insert_with(|| GlyphBitmap {
                            w: bitmap.width,
                            h: bitmap.height,
                            rgba: bitmap.rgba.to_vec(),
                        });
                    let rows: Vec<&str> = bitmap.chars.iter().map(String::as_str).collect();
                    self.bitmap(image, &rows, bitmap.glyph_height, bitmap.ascent);
                }
                FontProvider::Space(space) => {
                    for &(c, advance) in &space.advances {
                        self.space(c, advance.max(0.0).round() as u32);
                    }
                }
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::{BitmapFont, GLYPH_HEIGHT};
    use crate::sprite::pieces;
    use crate::{Piece, UiRect};
    use boxel_pack::{BitmapProvider, ResourceId, SpaceProvider};

    #[test]
    fn scalings_map_one_to_one() {
        assert_eq!(Scaling::from(GuiScaling::Stretch), Scaling::Stretch);
        assert_eq!(
            Scaling::from(GuiScaling::Tile {
                width: 16,
                height: 8
            }),
            Scaling::Tile { w: 16, h: 8 }
        );
        assert_eq!(
            Scaling::from(GuiScaling::NineSlice {
                width: 200,
                height: 20,
                border: [1, 2, 3, 4],
                stretch_inner: true,
            }),
            Scaling::NineSlice {
                w: 200,
                h: 20,
                border: [1, 2, 3, 4],
                stretch_inner: true,
            }
        );
    }

    #[test]
    fn a_pack_sprite_draws_its_nine_slice() {
        // A 2x pack button: 12 by 8 texels, 6 by 4 GUI pixels, borders of 1.
        let texture = GuiTexture {
            width: 12,
            height: 8,
            rgba: (0..12 * 8).flat_map(|i| [i as u8, 0, 0, 255]).collect(),
            scaling: GuiScaling::NineSlice {
                width: 6,
                height: 4,
                border: [1; 4],
                stretch_inner: false,
            },
        };
        let mut atlas = UiAtlas::new(64);
        let id = atlas.insert_pack_sprite("game:button", &texture).unwrap();
        assert_eq!(atlas.find("game:button"), Some(id));
        let sprite = atlas.sprite(id);
        let [x, y, w, h] = sprite.src;
        assert_eq!([w, h], [12, 8]);
        assert_eq!(atlas.texel(x + 5, y + 1), [12 + 5, 0, 0, 255]);
        // Drawn 10 by 6 GUI pixels: corners keep 2 texels per GUI pixel.
        let mut out = Vec::new();
        pieces(&sprite, UiRect::new(0, 0, 10, 6), &mut out);
        assert_eq!(
            out.first(),
            Some(&Piece {
                dst: UiRect::new(0, 0, 1, 1),
                src: [x, y, 2, 2],
            })
        );
        assert_eq!(
            out.last().map(|p| (p.dst, p.src)),
            Some((UiRect::new(9, 5, 1, 1), [x + 10, y + 6, 2, 2]))
        );
    }

    /// A white grid image whose cell `i` (row-major, `cols` per row) uses `widths[i]` columns.
    fn grid(cell: [u32; 2], cols: u32, widths: &[u32]) -> (u32, u32, Vec<u8>) {
        let rows = (widths.len() as u32).div_ceil(cols);
        let (w, h) = (cell[0] * cols, cell[1] * rows);
        let mut image = GlyphBitmap::new(w, h);
        for (i, &used) in widths.iter().enumerate() {
            let (cx, cy) = (i as u32 % cols * cell[0], i as u32 / cols * cell[1]);
            for y in 0..cell[1] {
                for x in 0..used {
                    image.set(cx + x, cy + y, [255; 4]);
                }
            }
        }
        (w, h, image.rgba)
    }

    fn bitmap(
        cell: [u32; 2],
        chars: &[&str],
        widths: &[u32],
        height: u32,
        ascent: i32,
    ) -> FontProvider {
        let cols = chars[0].chars().count() as u32;
        let (width, h, rgba) = grid(cell, cols, widths);
        FontProvider::Bitmap(BitmapProvider {
            file: ResourceId::parse("test:font/grid.png"),
            width,
            height: h,
            rgba: rgba.into(),
            chars: chars.iter().map(|s| (*s).to_owned()).collect(),
            glyph_height: height,
            ascent,
        })
    }

    #[test]
    fn a_pack_font_keeps_minecraft_order_sizes_and_spaces() {
        let font = PackFont {
            providers: vec![
                FontProvider::Space(SpaceProvider {
                    advances: vec![(' ', 4.4), ('\u{200c}', -2.0), ('t', 6.6)],
                }),
                // An accent row: 12-texel cells drawn 12 GUI pixels tall, 3 above the line.
                bitmap([9, 12], &["\u{e1}"], &[5], 12, 10),
                // A 2x ASCII grid: 16-texel cells drawn 8 GUI pixels tall.
                bitmap([16, 16], &["ab", "\u{e1}\0"], &[11, 2, 16], 8, 7),
            ],
            warnings: Vec::new(),
        };
        let mut atlas = UiAtlas::new(256);
        let mut builder = FontBuilder::new();
        builder.bits('a', [0xFF; 8]).bits('z', [0x01; 8]);
        builder.pack_font(&font);
        assert!(builder.contains('z') && builder.contains('t') && !builder.contains('c'));
        let built: BitmapFont = builder.build(&mut atlas).unwrap();

        // 11 texels at half scale: 5.5 GUI pixels drawn on 6, from 12 texels (the 12th is
        // transparent padding), advance (int) (0.5 + 5.5) + 1. The pack replaced the bits.
        let a = built.resolve('a');
        assert_eq!(
            (a.size, [a.src[2], a.src[3]], a.top, a.advance),
            ([6, GLYPH_HEIGHT], [12, 16], 0, 7)
        );
        assert_eq!(atlas.texel(a.src[0] + 10, a.src[1]), [255; 4]);
        assert_eq!(atlas.texel(a.src[0] + 11, a.src[1]), [0; 4]);
        let b = built.resolve('b');
        assert_eq!((b.size, b.advance), ([1, GLYPH_HEIGHT], 2));
        // The first provider naming 'á' wins: the accent row, not the ASCII grid's full cell.
        let accent = built.resolve('\u{e1}');
        assert_eq!((accent.size, accent.top, accent.advance), ([5, 12], -3, 6));
        // Spaces draw nothing and move the pen their rounded advance (not below 0); a space
        // provider's character replaces a glyph too.
        assert_eq!(
            (built.resolve(' ').advance, built.resolve(' ').src[2]),
            (4, 0)
        );
        assert_eq!(built.resolve('\u{200c}').advance, 0);
        assert_eq!(
            (built.resolve('t').advance, built.resolve('t').size),
            (7, [0, 0])
        );
        // What the pack lacks keeps the builder's own glyph.
        assert_eq!(built.resolve('z').size, [1, GLYPH_HEIGHT]);
    }
}
