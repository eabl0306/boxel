//! Sprite scaling (stretch, tile and nine-slice, as Minecraft's `gui.scaling` in a sprite's
//! `.mcmeta`) and the pieces a sprite is cut into when drawn at a size.

use crate::layout::UiRect;

/// How a sprite fills a rectangle of another size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Scaling {
    /// The whole sprite stretched over the rectangle.
    #[default]
    Stretch,
    /// The sprite repeated from the top-left, each copy `w` by `h` GUI pixels; the last row and
    /// column are cut.
    Tile {
        /// Tile width in GUI pixels.
        w: u32,
        /// Tile height in GUI pixels.
        h: u32,
    },
    /// Corners drawn 1:1, edges and center repeated (cut at the end) or stretched. The sprite is
    /// `w` by `h` GUI pixels; `border` is left, top, right, bottom in GUI pixels.
    NineSlice {
        /// Sprite width in GUI pixels.
        w: u32,
        /// Sprite height in GUI pixels.
        h: u32,
        /// Left, top, right and bottom borders in GUI pixels.
        border: [u32; 4],
        /// Stretch the edges and the center instead of repeating them.
        stretch_inner: bool,
    },
}

/// A sprite: its rectangle in the atlas (`[x, y, w, h]` in texels) and how it scales.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Sprite {
    /// `[x, y, w, h]` in atlas texels.
    pub src: [u32; 4],
    /// How it fills a rectangle.
    pub scaling: Scaling,
}

/// One piece of a sprite drawn into a rectangle: where (GUI pixels) and from where (atlas
/// texels).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    /// Destination in GUI pixels.
    pub dst: UiRect,
    /// Source `[x, y, w, h]` in atlas texels.
    pub src: [u32; 4],
}

/// A run along one axis: destination offset and length, source offset and length, the source in
/// the sprite's GUI pixels.
type Span = (i32, i32, u32, u32);

/// The spans of a nine-slice axis: sprite length `len`, borders `a` and `b`, destination length
/// `dst` (Minecraft's `blitNineSlicedSprite`: borders clamped to half the destination, the
/// middle repeated with its last copy cut, or stretched).
fn nine_slice_axis(len: u32, a: u32, b: u32, dst: i32, stretch: bool, out: &mut Vec<Span>) {
    out.clear();
    if dst <= 0 {
        return;
    }
    if dst == len as i32 {
        out.push((0, dst, 0, len));
        return;
    }
    let a = a.min(dst as u32 / 2);
    let b = b.min(dst as u32 / 2);
    if a > 0 {
        out.push((0, a as i32, 0, a));
    }
    let middle = dst - a as i32 - b as i32;
    let tile = len.saturating_sub(a + b);
    if middle > 0 && tile > 0 {
        if stretch {
            out.push((a as i32, middle, a, tile));
        } else {
            tile_axis(a as i32, middle, a, tile, out);
        }
    }
    if b > 0 {
        out.push((dst - b as i32, b as i32, len - b, b));
    }
}

/// Copies of the source span `src..src + tile` from `at` over `dst` pixels, the last one cut.
fn tile_axis(at: i32, dst: i32, src: u32, tile: u32, out: &mut Vec<Span>) {
    let mut done = 0;
    while done < dst {
        let n = (dst - done).min(tile as i32);
        out.push((at + done, n, src, n as u32));
        done += n;
    }
}

/// Appends the pieces that draw `sprite` over `dst`. Corners and cut tiles keep one sprite
/// pixel per GUI pixel; a sprite whose texture is larger than its GUI size (a high-resolution
/// pack) keeps that ratio in every piece.
pub fn pieces(sprite: &Sprite, dst: UiRect, out: &mut Vec<Piece>) {
    if dst.is_empty() {
        return;
    }
    let [sx, sy, sw, sh] = sprite.src;
    let (gw, gh) = match sprite.scaling {
        Scaling::Stretch => {
            out.push(Piece {
                dst,
                src: sprite.src,
            });
            return;
        }
        Scaling::Tile { w, h } | Scaling::NineSlice { w, h, .. } => (w.max(1), h.max(1)),
    };
    let (mut cols, mut rows) = (Vec::new(), Vec::new());
    match sprite.scaling {
        Scaling::Tile { .. } => {
            tile_axis(0, dst.w, 0, gw, &mut cols);
            tile_axis(0, dst.h, 0, gh, &mut rows);
        }
        Scaling::NineSlice {
            border: [l, t, r, b],
            stretch_inner,
            ..
        } => {
            nine_slice_axis(gw, l, r, dst.w, stretch_inner, &mut cols);
            nine_slice_axis(gh, t, b, dst.h, stretch_inner, &mut rows);
        }
        Scaling::Stretch => {}
    }
    // Sprite GUI pixels to texels.
    let tx = |v: u32| v * sw / gw;
    let ty = |v: u32| v * sh / gh;
    for &(dy, dh, vy, vh) in &rows {
        for &(dx, dw, vx, vw) in &cols {
            out.push(Piece {
                dst: UiRect::new(dst.x + dx, dst.y + dy, dw, dh),
                src: [
                    sx + tx(vx),
                    sy + ty(vy),
                    tx(vx + vw) - tx(vx),
                    ty(vy + vh) - ty(vy),
                ],
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn button(stretch_inner: bool) -> Sprite {
        Sprite {
            src: [100, 50, 200, 20],
            scaling: Scaling::NineSlice {
                w: 200,
                h: 20,
                border: [4; 4],
                stretch_inner,
            },
        }
    }

    fn cut(sprite: &Sprite, dst: UiRect) -> Vec<Piece> {
        let mut out = Vec::new();
        pieces(sprite, dst, &mut out);
        out
    }

    #[test]
    fn nine_slice_button_150_wide() {
        // Same height as the sprite: left border, one cut middle tile, right border.
        let out = cut(&button(false), UiRect::new(10, 20, 150, 20));
        assert_eq!(
            out,
            [
                Piece {
                    dst: UiRect::new(10, 20, 4, 20),
                    src: [100, 50, 4, 20]
                },
                Piece {
                    dst: UiRect::new(14, 20, 142, 20),
                    src: [104, 50, 142, 20]
                },
                Piece {
                    dst: UiRect::new(156, 20, 4, 20),
                    src: [296, 50, 4, 20]
                },
            ]
        );
    }

    #[test]
    fn nine_slice_tiles_edges_and_center_with_a_cut_last_tile() {
        // 150 by 30: rows are top border, middle (12 + 10, cut), bottom border.
        let out = cut(&button(false), UiRect::new(0, 0, 150, 30));
        assert_eq!(out.len(), 12);
        // Corners 1:1.
        assert!(out.contains(&Piece {
            dst: UiRect::new(0, 0, 4, 4),
            src: [100, 50, 4, 4]
        }));
        assert!(out.contains(&Piece {
            dst: UiRect::new(146, 26, 4, 4),
            src: [296, 66, 4, 4]
        }));
        // The left edge: one full 12-pixel tile, then a cut one of 10.
        assert!(out.contains(&Piece {
            dst: UiRect::new(0, 4, 4, 12),
            src: [100, 54, 4, 12]
        }));
        assert!(out.contains(&Piece {
            dst: UiRect::new(0, 16, 4, 10),
            src: [100, 54, 4, 10]
        }));
        // Every pixel is covered exactly once.
        let area: i32 = out.iter().map(|p| p.dst.w * p.dst.h).sum();
        assert_eq!(area, 150 * 30);
        // Every piece maps 1:1.
        assert!(
            out.iter()
                .all(|p| p.src[2] as i32 == p.dst.w && p.src[3] as i32 == p.dst.h)
        );
    }

    #[test]
    fn nine_slice_stretch_inner_is_nine_pieces() {
        let out = cut(&button(true), UiRect::new(0, 0, 300, 40));
        assert_eq!(out.len(), 9);
        let center = out[4];
        assert_eq!(center.dst, UiRect::new(4, 4, 292, 32));
        assert_eq!(center.src, [104, 54, 192, 12]);
    }

    #[test]
    fn nine_slice_keeps_a_high_resolution_ratio_and_clamps_borders() {
        // A 2x texture (400 by 40 texels) for a 200 by 20 sprite.
        let mut sprite = button(false);
        sprite.src = [0, 0, 400, 40];
        let out = cut(&sprite, UiRect::new(0, 0, 100, 20));
        assert_eq!(out[0].src, [0, 0, 8, 40]);
        assert_eq!(out[1].src, [8, 0, 184, 40]);
        assert_eq!(out[2].src, [392, 0, 8, 40]);
        // A destination narrower than both borders: each border takes half.
        let out = cut(&button(false), UiRect::new(0, 0, 6, 20));
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].dst.w, out[1].dst.w), (3, 3));
        assert_eq!(out[1].src, [297, 50, 3, 20]);
    }

    #[test]
    fn tile_and_stretch() {
        let sprite = Sprite {
            src: [0, 0, 16, 16],
            scaling: Scaling::Tile { w: 16, h: 16 },
        };
        let out = cut(&sprite, UiRect::new(0, 0, 40, 16));
        let widths: Vec<i32> = out.iter().map(|p| p.dst.w).collect();
        assert_eq!(widths, [16, 16, 8]);
        assert_eq!(out[2].src, [0, 0, 8, 16]);
        let stretch = Sprite {
            src: [5, 5, 2, 2],
            scaling: Scaling::Stretch,
        };
        assert_eq!(
            cut(&stretch, UiRect::new(1, 1, 50, 50)),
            [Piece {
                dst: UiRect::new(1, 1, 50, 50),
                src: [5, 5, 2, 2]
            }]
        );
        assert!(cut(&stretch, UiRect::new(0, 0, 0, 5)).is_empty());
    }

    proptest::proptest! {
        /// A nine-slice with a center covers any destination exactly once, every piece inside
        /// the sprite's texels, at any texture ratio.
        #[test]
        fn nine_slice_covers_the_destination_once(
            w in 1u32..40, h in 1u32..40,
            border in proptest::array::uniform4(0u32..10),
            dw in 0i32..120, dh in 0i32..120,
            stretch_inner: bool, ratio in 1u32..4,
        ) {
            let [l, t, r, b] = border;
            proptest::prop_assume!(l + r < w && t + b < h);
            let sprite = Sprite {
                src: [10, 20, w * ratio, h * ratio],
                scaling: Scaling::NineSlice { w, h, border, stretch_inner },
            };
            let dst = UiRect::new(-7, 3, dw, dh);
            let mut count = vec![0u8; (dw * dh) as usize];
            for p in cut(&sprite, dst) {
                proptest::prop_assert!(p.src[0] >= 10 && p.src[0] + p.src[2] <= 10 + w * ratio);
                proptest::prop_assert!(p.src[1] >= 20 && p.src[1] + p.src[3] <= 20 + h * ratio);
                for y in p.dst.y..p.dst.bottom() {
                    for x in p.dst.x..p.dst.right() {
                        count[((y - dst.y) * dw + (x - dst.x)) as usize] += 1;
                    }
                }
            }
            proptest::prop_assert!(count.iter().all(|&c| c == 1));
        }
    }
}
