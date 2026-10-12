//! The CPU reference rasterizer: what a paint list looks like, pixel by pixel, computed the way a
//! GPU draws it (texel picked by integer arithmetic, texel times tint in linear light, straight
//! alpha blending into an sRGB target, rounded after every quad). Tests compare renderers with
//! it, and [`runs`] turns a paint list into solid rectangles for a renderer that only draws
//! those.

use crate::atlas::UiAtlas;
use crate::color::{f32_to_unorm, linear_to_srgb, srgb_to_linear, to_linear, unorm_to_f32};
use crate::paint::UiQuad;
use boxel_math::UVec2;

/// The color a quad gives a pixel before blending: the texel at `texel` times the tint, in
/// linear light with straight alpha.
pub fn fragment(atlas: &UiAtlas, quad: &UiQuad, texel: [u32; 2]) -> [f32; 4] {
    let t = atlas.texel(texel[0], texel[1]);
    let tint = to_linear(quad.tint);
    [
        srgb_to_linear(t[0]) * tint[0],
        srgb_to_linear(t[1]) * tint[1],
        srgb_to_linear(t[2]) * tint[2],
        unorm_to_f32(t[3]) * tint[3],
    ]
}

/// Blends `src` (linear, straight alpha) over the sRGB pixel `dst`, as fixed-function alpha
/// blending into an sRGB target: color `src * a + dst * (1 - a)` in linear light, alpha
/// `a + dst_a * (1 - a)`, then stored as bytes.
pub fn blend(dst: &mut [u8], src: [f32; 4]) {
    let a = src[3];
    for c in 0..3 {
        let d = srgb_to_linear(dst[c]);
        dst[c] = linear_to_srgb(src[c] * a + d * (1.0 - a));
    }
    dst[3] = f32_to_unorm(a + unorm_to_f32(dst[3]) * (1.0 - a));
}

/// Draws `quads` in order into a `size` image (sRGB RGBA8, row by row) cleared to `clear`.
pub fn rasterize(quads: &[UiQuad], atlas: &UiAtlas, size: UVec2, clear: [u8; 4]) -> Vec<u8> {
    let mut image = clear.repeat((size.x * size.y) as usize);
    let max = [size.x as i32, size.y as i32];
    for quad in quads {
        let Some(quad) = quad.clipped([0, 0], max) else {
            continue;
        };
        for y in quad.min[1]..quad.max[1] {
            for x in quad.min[0]..quad.max[0] {
                let src = fragment(atlas, &quad, quad.texel_at(x, y));
                let i = ((y as u32 * size.x + x as u32) * 4) as usize;
                blend(&mut image[i..i + 4], src);
            }
        }
    }
    image
}

/// A solid rectangle in physical pixels with the color a quad gives it (linear, straight
/// alpha, as [`fragment`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Run {
    /// Top-left pixel (inclusive).
    pub min: [i32; 2],
    /// Bottom-right pixel (exclusive).
    pub max: [i32; 2],
    /// Color before blending.
    pub linear: [f32; 4],
}

/// The paint list as solid rectangles: for each quad, the pixel rows that read the same source
/// row, cut where the color changes along it. Fully transparent pieces are dropped. Painting
/// the runs in order with [`blend`] gives exactly [`rasterize`]'s image.
pub fn runs(quads: &[UiQuad], atlas: &UiAtlas) -> Vec<Run> {
    let mut out = Vec::new();
    for quad in quads {
        if quad.min[0] >= quad.max[0] || quad.min[1] >= quad.max[1] {
            continue;
        }
        let mut y = quad.min[1];
        while y < quad.max[1] {
            // The pixel rows reading the same texel row.
            let row = quad.texel_at(quad.min[0], y)[1];
            let mut y_end = y + 1;
            while y_end < quad.max[1] && quad.texel_at(quad.min[0], y_end)[1] == row {
                y_end += 1;
            }
            let mut x = quad.min[0];
            while x < quad.max[0] {
                let color = fragment(atlas, quad, quad.texel_at(x, y));
                let mut x_end = x + 1;
                while x_end < quad.max[0] && fragment(atlas, quad, quad.texel_at(x_end, y)) == color
                {
                    x_end += 1;
                }
                if color[3] > 0.0 {
                    out.push(Run {
                        min: [x, y],
                        max: [x_end, y_end],
                        linear: color,
                    });
                }
                x = x_end;
            }
            y = y_end;
        }
    }
    out
}

/// Paints `runs` in order into a `size` image cleared to `clear`, as [`rasterize`] paints
/// quads.
pub fn paint_runs(runs: &[Run], size: UVec2, clear: [u8; 4]) -> Vec<u8> {
    let mut image = clear.repeat((size.x * size.y) as usize);
    for run in runs {
        for y in run.min[1].max(0)..run.max[1].min(size.y as i32) {
            for x in run.min[0].max(0)..run.max[0].min(size.x as i32) {
                let i = ((y as u32 * size.x + x as u32) * 4) as usize;
                blend(&mut image[i..i + 4], run.linear);
            }
        }
    }
    image
}

/// FNV-1a 64 of `bytes`: a stable fingerprint for golden images.
pub fn fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::rgb;

    #[test]
    fn opaque_white_texel_keeps_the_tint_exactly() {
        let atlas = UiAtlas::new(16);
        let quads = [UiQuad::solid([1, 1], [3, 2], rgb(0x336699))];
        let image = rasterize(&quads, &atlas, UVec2::new(4, 3), [0, 0, 0, 255]);
        let px = |x: usize, y: usize| &image[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
        assert_eq!(px(1, 1), [0x33, 0x66, 0x99, 255]);
        assert_eq!(px(2, 1), [0x33, 0x66, 0x99, 255]);
        assert_eq!(px(0, 0), [0, 0, 0, 255]);
        assert_eq!(px(1, 2), [0, 0, 0, 255]);
    }

    #[test]
    fn translucent_blends_in_linear_light() {
        let atlas = UiAtlas::new(16);
        // Half-transparent white over black: linear 0.5 is sRGB 188, not 128.
        let quads = [UiQuad::solid([0, 0], [1, 1], [255, 255, 255, 128])];
        let image = rasterize(&quads, &atlas, UVec2::new(1, 1), [0, 0, 0, 255]);
        assert_eq!(image, [188, 188, 188, 255]);
    }

    #[test]
    fn clipped_quad_paints_the_same_pixels_inside_the_clip() {
        let mut atlas = UiAtlas::new(16);
        let texels: Vec<u8> = (0..16u8)
            .flat_map(|i| [i * 16, 255 - i * 16, i, 255])
            .collect();
        let at = atlas.add_image(4, 4, &texels).unwrap();
        let quad = UiQuad::mapped([2, 1], [12, 12], at, [4, 4], [255, 200, 255, 255]);
        let size = UVec2::new(16, 16);
        let whole = rasterize(&[quad], &atlas, size, [0; 4]);
        let clip = ([5, 4], [11, 9]);
        let part = rasterize(
            &[quad.clipped(clip.0, clip.1).unwrap()],
            &atlas,
            size,
            [0; 4],
        );
        for y in 0..16 {
            for x in 0..16 {
                let i = (y * 16 + x) * 4;
                let inside = (5..11).contains(&x) && (4..9).contains(&y);
                if inside {
                    assert_eq!(part[i..i + 4], whole[i..i + 4], "({x}, {y})");
                } else {
                    assert_eq!(part[i..i + 4], [0; 4], "({x}, {y})");
                }
            }
        }
    }

    #[test]
    fn runs_paint_exactly_the_raster() {
        let mut atlas = UiAtlas::new(16);
        let texels: Vec<u8> = (0..16u8)
            .flat_map(|i| [i % 3 * 100, 50, 0, if i % 5 == 0 { 0 } else { 255 }])
            .collect();
        let at = atlas.add_image(4, 4, &texels).unwrap();
        let quads = [
            UiQuad::solid([0, 0], [20, 10], [10, 20, 30, 255]),
            UiQuad::mapped([1, 1], [12, 8], at, [4, 4], [255, 255, 255, 255]),
            UiQuad::solid([5, 2], [15, 9], [255, 0, 0, 100]),
            UiQuad::mapped([3, 0], [9, 9], at, [4, 4], [128, 255, 64, 200])
                .clipped([4, 2], [10, 7])
                .unwrap(),
        ];
        let size = UVec2::new(20, 10);
        let runs = runs(&quads, &atlas);
        assert!(runs.len() < 20 * 10);
        assert_eq!(
            paint_runs(&runs, size, [0; 4]),
            rasterize(&quads, &atlas, size, [0; 4])
        );
    }

    #[test]
    fn fingerprint_is_fnv1a() {
        assert_eq!(fingerprint(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fingerprint(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
