//! How a game's texture layer finds its pixels in a pack ([`LayerRecipe`]). Plain data: the
//! renderer reads the recipes the game passes with the pack, and this crate never looks at them.
//! The [`TextureStep`]s a recipe may end with (EW-XFORM, ADR 0044) turn, mirror or crop the
//! texture on the CPU; [`apply_steps`] is public so a game paints its own layers the same way.
//! A recipe's [`LayerPiece`]s build a layer from rectangles of other textures (EW-BER, ADR
//! 0049): the faces of an entity texture placed where a block's box faces read them.

use crate::FaceName;

/// How one texture layer of a game (`game:grass_side`) is read from a Minecraft resource pack:
/// the block whose blockstate and model name the texture (Minecraft's, or one in the game's own
/// pack namespace), the model face that shows it, the biome tint baked into it, and fallback
/// texture names for packs that ship no model.
///
/// A layer without a recipe reads the texture named like it (`game:copper_ore` reads
/// `minecraft:block/copper_ore`), untinted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerRecipe {
    /// The game's texture layer name (`game:grass_side`).
    pub layer: String,
    /// Block whose blockstate and model are read: `ns:name`, or a bare name in the `minecraft`
    /// namespace (`grass_block`).
    pub block: String,
    /// A state of the game whose property values pick the blockstate variant
    /// (`game:log[axis=y]`), or `""`. Its properties are named like Minecraft's.
    pub state: String,
    /// Values of Minecraft properties the game's block lacks (`snowy=false`), added to the
    /// state's.
    pub variant: String,
    /// Model face giving the texture (`North` stands for every side face).
    pub face: FaceName,
    /// Biome tint (sRGB) baked into model faces with a `tintindex`, or `None`.
    pub tint: Option<[u8; 3]>,
    /// Texture parts (`ns:block/...`, or `block/...` in the `minecraft` namespace; bottom first,
    /// each with its tint) when the pack has no usable model.
    pub fallback: Vec<(String, Option<[u8; 3]>)>,
    /// A fluid's flow sprite: Minecraft's are twice the block size and a face shows a quarter of
    /// one, so the layer takes the top-left quarter of a frame twice the layer's edge (other
    /// sizes are scaled whole).
    pub quarter: bool,
    /// Turns, mirrors or crops applied to every texture part, in order, after `quarter` and
    /// before the part is scaled to the layer's edge ([`apply_steps`]): a rotated glazed
    /// terracotta, a mirrored door. Empty for the texture as it is.
    pub steps: Vec<TextureStep>,
    /// A texture some packs ship for this layer of their own (`block/oak_door_bottom_right`,
    /// EW-BOXTEX, ADR 0045): when the pack has it, the layer is that texture as it is, and the
    /// rest of the recipe is not read. A right-hinge door: Minecraft mirrors the door's
    /// texture (the recipe's steps), Whimscape draws its own.
    pub own: Option<String>,
    /// Pieces of textures composited bottom to top over a transparent layer (EW-BER, ADR 0049),
    /// each read through its own steps and then the recipe's `steps`: the faces of an entity
    /// texture (`entity/chest/normal`), cut out with [`TextureStep::Crop`] and set where a
    /// block's box faces read them with [`TextureStep::Place`]. Read when the pack has neither
    /// the model's textures nor the fallback (a bed's block textures in one pack, its entity
    /// texture in another), or the recipe names none; the first piece's texture is required, a
    /// missing later one is left out.
    pub pieces: Vec<LayerPiece>,
}

/// One piece of a layer ([`LayerRecipe::pieces`]): a texture of the pack (`entity/...` reads
/// `textures/entity/`, `block/...` the block textures) and the steps that turn it into its part
/// of the layer.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LayerPiece {
    /// The texture, `ns:path` or a bare path in the `minecraft` namespace
    /// (`entity/shulker/shulker_red`).
    pub texture: String,
    /// Applied in order to the texture as the pack ships it ([`apply_steps`]).
    pub steps: Vec<TextureStep>,
}

/// One step of a layer's transform ([`LayerRecipe::steps`]). Images are rows from the top; a
/// step maps every pixel of its output to one pixel of its input, so the result is exact (no
/// filtering) and keeps binary alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextureStep {
    /// Turns the image this many quarter turns clockwise (taken modulo 4: 1 is 90 degrees, 2 is
    /// 180, 3 is 270). The top row becomes the right column.
    Rotate(u8),
    /// Mirrors the image left to right.
    MirrorX,
    /// Mirrors the image top to bottom.
    MirrorY,
    /// Keeps the rectangle `[x, y, width, height]` of the image, measured in `1 / grid` of its
    /// width and height (`grid` 16: Minecraft's UVs on a block texture; 64: the texels of a
    /// 64 px entity texture), clamped to the image. The scaling to the layer's edge stretches it
    /// to fill the layer. A `grid` of 0 or an empty rectangle keeps the image.
    Crop {
        /// `[x, y, width, height]` in `1 / grid` of the edge.
        rect: [u16; 4],
        /// Units per image edge.
        grid: u16,
    },
    /// Puts the image into the rectangle `[x, y, width, height]` of a transparent image of
    /// `grid` units each way: the inverse of [`TextureStep::Crop`]. The new image has as many
    /// texels per unit as the image has across the rectangle, so a piece cut from a 64 px
    /// entity texture and placed on a 16 unit grid makes a 16 px layer (64 px for a pack
    /// four times as fine). A `grid` of 0 or an empty rectangle keeps the image.
    Place {
        /// `[x, y, width, height]` in `1 / grid` of the new image's edge.
        rect: [u16; 4],
        /// Units per new image edge.
        grid: u16,
    },
}

/// The largest edge [`TextureStep::Place`] makes: a texel stretched across a 4096 px canvas is
/// a recipe's mistake, not a texture.
const MAX_PLACED_EDGE: u64 = 4096;

impl TextureStep {
    /// The image `texels` (`width` x `height`, rows from the top) after this step, with its new
    /// size. Texels past `width * height` are ignored; missing ones read as transparent.
    pub fn apply(self, width: u32, height: u32, texels: &[[u8; 4]]) -> (u32, u32, Vec<[u8; 4]>) {
        let at = |x: u32, y: u32| {
            texels
                .get((y as usize) * (width as usize) + x as usize)
                .copied()
                .unwrap_or([0; 4])
        };
        let image = |w: u32, h: u32, f: &dyn Fn(u32, u32) -> [u8; 4]| {
            let texels = (0..h).flat_map(|y| (0..w).map(move |x| (x, y)));
            (w, h, texels.map(|(x, y)| f(x, y)).collect())
        };
        if width == 0 || height == 0 {
            return (width, height, Vec::new());
        }
        match self {
            TextureStep::Rotate(turns) => match turns % 4 {
                // Clockwise: the output's column x is the input's row height - 1 - x.
                1 => image(height, width, &|x, y| at(y, height - 1 - x)),
                2 => image(width, height, &|x, y| at(width - 1 - x, height - 1 - y)),
                3 => image(height, width, &|x, y| at(width - 1 - y, x)),
                _ => image(width, height, &|x, y| at(x, y)),
            },
            TextureStep::MirrorX => image(width, height, &|x, y| at(width - 1 - x, y)),
            TextureStep::MirrorY => image(width, height, &|x, y| at(x, height - 1 - y)),
            TextureStep::Crop { rect, grid } => {
                let grid = u64::from(grid);
                let span = |start: u16, len: u16, edge: u32| {
                    let edge = u64::from(edge);
                    let lo = (u64::from(start) * edge / grid.max(1)).min(edge);
                    let hi = (u64::from(start) + u64::from(len)) * edge / grid.max(1);
                    (lo as u32, hi.min(edge) as u32)
                };
                let (x0, x1) = span(rect[0], rect[2], width);
                let (y0, y1) = span(rect[1], rect[3], height);
                if grid == 0 || x1 <= x0 || y1 <= y0 {
                    return image(width, height, &|x, y| at(x, y));
                }
                image(x1 - x0, y1 - y0, &|x, y| at(x0 + x, y0 + y))
            }
            TextureStep::Place { rect, grid } => {
                let [rx, ry, rw, rh] = rect.map(u64::from);
                let grid = u64::from(grid);
                if grid == 0 || rw == 0 || rh == 0 {
                    return image(width, height, &|x, y| at(x, y));
                }
                // The canvas edge that gives the rectangle the image's texels, rounded.
                let edge = |texels: u32, units: u64| {
                    ((u64::from(texels) * grid + units / 2) / units).clamp(1, MAX_PLACED_EDGE)
                };
                let (cw, ch) = (edge(width, rw), edge(height, rh));
                // The rectangle in canvas texels, kept inside the canvas.
                let span = |start: u64, len: u64, edge: u64| {
                    let lo = (start * edge / grid).min(edge);
                    let hi = ((start + len) * edge / grid).min(edge);
                    (lo, hi)
                };
                let (x0, x1) = span(rx, rw, cw);
                let (y0, y1) = span(ry, rh, ch);
                let (w, h) = (u64::from(width), u64::from(height));
                image(cw as u32, ch as u32, &|x, y| {
                    let (x, y) = (u64::from(x), u64::from(y));
                    if x < x0 || x >= x1 || y < y0 || y >= y1 {
                        return [0; 4];
                    }
                    // Nearest texel of the image (exact when the sizes agree).
                    let sx = (x - x0) * w / (x1 - x0);
                    let sy = (y - y0) * h / (y1 - y0);
                    at(sx as u32, sy as u32)
                })
            }
        }
    }
}

/// [`TextureStep::apply`] of every step of `steps` in order.
pub fn apply_steps(
    steps: &[TextureStep],
    width: u32,
    height: u32,
    texels: &[[u8; 4]],
) -> (u32, u32, Vec<[u8; 4]>) {
    let mut out = (width, height, texels.to_vec());
    for step in steps {
        out = step.apply(out.0, out.1, &out.2);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 3 x 2 image whose texel at (x, y) is `[x, y, 10 * y + x, 255]`:
    /// row 0 `a b c`, row 1 `d e f`.
    fn image() -> Vec<[u8; 4]> {
        (0..2u8)
            .flat_map(|y| (0..3u8).map(move |x| [x, y, 10 * y + x, 255]))
            .collect()
    }

    /// The third channel of each texel, row by row (`10 * y + x` of the source texel).
    fn ids(out: &(u32, u32, Vec<[u8; 4]>)) -> (u32, u32, Vec<u8>) {
        (out.0, out.1, out.2.iter().map(|t| t[2]).collect())
    }

    #[test]
    fn rotations_turn_clockwise() {
        let src = image();
        // a b c       d a
        // d e f  ->   e b
        //             f c
        let r90 = TextureStep::Rotate(1).apply(3, 2, &src);
        assert_eq!(ids(&r90), (2, 3, vec![10, 0, 11, 1, 12, 2]));
        let r180 = TextureStep::Rotate(2).apply(3, 2, &src);
        assert_eq!(ids(&r180), (3, 2, vec![12, 11, 10, 2, 1, 0]));
        let r270 = TextureStep::Rotate(3).apply(3, 2, &src);
        assert_eq!(ids(&r270), (2, 3, vec![2, 12, 1, 11, 0, 10]));
        assert_eq!(
            TextureStep::Rotate(4).apply(3, 2, &src),
            (3, 2, src.clone())
        );
        // Four quarter turns and two half turns give the image back.
        let back = apply_steps(&[TextureStep::Rotate(1); 4], 3, 2, &src);
        assert_eq!(back, (3, 2, src.clone()));
        assert_eq!(
            apply_steps(
                &[TextureStep::Rotate(1), TextureStep::Rotate(1)],
                3,
                2,
                &src
            ),
            r180
        );
    }

    #[test]
    fn mirrors_flip_one_axis() {
        let src = image();
        let x = TextureStep::MirrorX.apply(3, 2, &src);
        assert_eq!(ids(&x), (3, 2, vec![2, 1, 0, 12, 11, 10]));
        let y = TextureStep::MirrorY.apply(3, 2, &src);
        assert_eq!(ids(&y), (3, 2, vec![10, 11, 12, 0, 1, 2]));
        // Both mirrors are the half turn.
        let both = apply_steps(&[TextureStep::MirrorX, TextureStep::MirrorY], 3, 2, &src);
        assert_eq!(both, TextureStep::Rotate(2).apply(3, 2, &src));
    }

    #[test]
    fn crops_keep_a_rectangle_of_the_grid() {
        // A 16 x 16 image cropped to Minecraft's door edge UVs [0, 0, 3, 16].
        let src: Vec<[u8; 4]> = (0..256u32).map(|i| [i as u8, 0, 0, 255]).collect();
        let (w, h, out) = TextureStep::Crop {
            rect: [0, 0, 3, 16],
            grid: 16,
        }
        .apply(16, 16, &src);
        assert_eq!((w, h), (3, 16));
        assert_eq!(out[0][0], 0);
        assert_eq!(out[3][0], 16, "row 1 starts at texel 16");
        // The same rectangle on a 32 px image keeps twice the texels.
        let big = vec![[1, 2, 3, 4]; 32 * 32];
        let (w, h, _) = TextureStep::Crop {
            rect: [4, 8, 8, 4],
            grid: 16,
        }
        .apply(32, 32, &big);
        assert_eq!((w, h), (16, 8));
        // A grid of 2 on the 3 x 2 image: x from 1 to 3, y from 1 to 2.
        let src = image();
        let one = TextureStep::Crop {
            rect: [1, 1, 1, 1],
            grid: 2,
        };
        assert_eq!(ids(&one.apply(3, 2, &src)), (2, 1, vec![11, 12]));
        // Clamped to the image; an empty rectangle or a zero grid keeps it.
        let clamped = TextureStep::Crop {
            rect: [1, 0, 9, 9],
            grid: 2,
        };
        assert_eq!(ids(&clamped.apply(3, 2, &src)), (2, 2, vec![1, 2, 11, 12]));
        for keep in [
            TextureStep::Crop {
                rect: [0, 0, 0, 1],
                grid: 2,
            },
            TextureStep::Crop {
                rect: [0, 0, 1, 1],
                grid: 0,
            },
        ] {
            assert_eq!(keep.apply(3, 2, &src), (3, 2, src.clone()));
        }
    }

    #[test]
    fn place_puts_a_piece_where_a_crop_took_it() {
        // A 4 x 2 texel piece of a 64 px texture placed at [1, 6, 4, 2] of a 16 unit grid: a
        // 16 x 16 image, the piece at x 1..5, y 6..8, transparent around it.
        let piece: Vec<[u8; 4]> = (0..8u8).map(|i| [i, 0, 0, 255]).collect();
        let place = TextureStep::Place {
            rect: [1, 6, 4, 2],
            grid: 16,
        };
        let (w, h, out) = place.apply(4, 2, &piece);
        assert_eq!((w, h), (16, 16));
        assert_eq!(out[6 * 16 + 1], [0, 0, 0, 255]);
        assert_eq!(out[7 * 16 + 4], [7, 0, 0, 255]);
        assert_eq!(out.iter().filter(|t| t[3] == 255).count(), 8);
        assert_eq!(out[0], [0; 4]);
        // Twice the texels (a pack twice as fine): a 32 px image, each texel 2 x 2.
        let fine: Vec<[u8; 4]> = (0..32u8).map(|i| [i, 0, 0, 255]).collect();
        let (w, h, out) = place.apply(8, 4, &fine);
        assert_eq!((w, h), (32, 32));
        assert_eq!(out.iter().filter(|t| t[3] == 255).count(), 32);
        assert_eq!(out[12 * 32 + 2], [0, 0, 0, 255]);
        // Crop then place gives the texels back at the same spot of a 16 px texture.
        let src: Vec<[u8; 4]> = (0..256u32).map(|i| [i as u8, 1, 0, 255]).collect();
        let rect = [3, 5, 7, 4];
        let back = apply_steps(
            &[
                TextureStep::Crop { rect, grid: 16 },
                TextureStep::Place { rect, grid: 16 },
            ],
            16,
            16,
            &src,
        );
        assert_eq!((back.0, back.1), (16, 16));
        for y in 0..16 {
            for x in 0..16 {
                let inside = (3..10).contains(&x) && (5..9).contains(&y);
                let want = if inside { src[y * 16 + x] } else { [0; 4] };
                assert_eq!(back.2[y * 16 + x], want, "{x} {y}");
            }
        }
        // A zero grid or an empty rectangle keeps the image.
        for keep in [
            TextureStep::Place {
                rect: [0, 0, 0, 1],
                grid: 16,
            },
            TextureStep::Place {
                rect: [0, 0, 1, 1],
                grid: 0,
            },
        ] {
            assert_eq!(keep.apply(4, 2, &piece), (4, 2, piece.clone()));
        }
    }

    #[test]
    fn steps_keep_alpha_and_handle_short_or_empty_images() {
        let src = vec![[9, 9, 9, 0], [1, 1, 1, 255]];
        let (_, _, out) = TextureStep::MirrorX.apply(2, 1, &src);
        assert_eq!(out, vec![[1, 1, 1, 255], [9, 9, 9, 0]]);
        // A missing texel reads transparent.
        let (_, _, out) = TextureStep::MirrorX.apply(3, 1, &src);
        assert_eq!(out[0], [0; 4]);
        assert_eq!(TextureStep::Rotate(1).apply(0, 4, &[]), (0, 4, vec![]));
    }
}
