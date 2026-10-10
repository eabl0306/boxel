//! The procedural painter of the demo layers: the pattern of each layer of [`crate::blocks`],
//! synthesized from its name, base color and noise. Moved verbatim from the renderer (#48): the
//! seed is the FNV-1a hash of the layer name, so every texel stays the same.
use std::sync::Arc;

use boxel_voxel::{TextureImage, TextureSpec};

/// Edge of every demo layer (texels).
pub const SIZE: u32 = 16;

/// Texels of a layer.
const BASE_TEXELS: usize = (SIZE * SIZE) as usize;

/// The pattern of a procedural layer ([`paint`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Pattern {
    /// Base color with per-texel noise of amplitude `noise`.
    Noise,
    /// Noise plus a top band of `color` `rows` texels tall (grass side).
    TopBand {
        /// Band color.
        color: [u8; 3],
        /// Band height in texels (of 16).
        rows: u8,
    },
    /// Noise plus random speckles of `color`.
    Speckle {
        /// Speckle color.
        color: [u8; 3],
        /// Speckles per 256 texels.
        density: u8,
    },
    /// Leaves: noise with ~25% transparent texels (alpha 0), for cutout tests.
    Leaves,
    /// Horizontal planks.
    Planks,
    /// Bark: dark vertical cracks, broken here and there (log sides; a turned log runs them
    /// along its axis).
    Bark,
    /// Glass: a clear inside (alpha 0) with a light frame and a few streaks (translucent).
    Glass,
    /// Noise with a constant alpha (water; translucent).
    Liquid {
        /// Alpha of every texel.
        alpha: u8,
    },
    /// Grass blades on a clear background, for cross plants (binary alpha, like `Leaves`).
    Plant,
    /// A flower on a clear background: a stem of the base color and a head of `color` (binary
    /// alpha).
    Flower {
        /// Petal color.
        color: [u8; 3],
    },
    /// A torch on a clear background, laid out like Minecraft's: a stick of the base color in
    /// columns 7 and 8 from row 8 down, under a flame in rows 6 and 7 (binary alpha).
    Torch,
    /// Cobblestone: rounded stones of the base color, each a little lighter or darker, between
    /// dark joints.
    Cobble,
    /// A fluid's flow texture (ADR 0025): noise with streaks running down its columns, lighter
    /// and darker, that tile along v; the shader scrolls it along the flow. Every texel has
    /// `alpha`.
    Flow {
        /// Alpha of every texel.
        alpha: u8,
    },
}

/// The image of a layer named `name`: `pattern` over `base_color` with per-texel noise of
/// amplitude `noise`, seeded by the name. Binary alpha for the cutout patterns (`Leaves`,
/// `Plant`, `Flower` and `Torch`).
pub fn paint(name: &str, base_color: [u8; 3], noise: u8, pattern: Pattern) -> TextureImage {
    TextureImage {
        size: SIZE,
        texels: Arc::from(synthesize(name, base_color, noise, pattern).as_slice()),
        binary_alpha: matches!(
            pattern,
            Pattern::Leaves | Pattern::Plant | Pattern::Flower { .. } | Pattern::Torch
        ),
    }
}

/// The layer named `name`, painted by [`paint`].
pub fn spec(name: &str, base_color: [u8; 3], noise: u8, pattern: Pattern) -> TextureSpec {
    TextureSpec {
        name: name.to_string(),
        image: paint(name, base_color, noise, pattern),
    }
}

/// The texels of one layer: [`SIZE`]^2 sRGB texels, row 0 = top of the image.
fn synthesize(
    name: &str,
    base_color: [u8; 3],
    noise: u8,
    pattern: Pattern,
) -> [[u8; 4]; BASE_TEXELS] {
    let size = SIZE as usize;
    let mut rng = SplitMix64(fnv1a64(name.as_bytes()));
    let amplitude = i32::from(noise);
    let mut texels = [[0, 0, 0, 255]; BASE_TEXELS];
    for (i, texel) in texels.iter_mut().enumerate() {
        let noise = rng.range_inclusive(-amplitude, amplitude);
        let color = match pattern {
            Pattern::TopBand { color, rows } if i / size < usize::from(rows) => color,
            _ => base_color,
        };
        *texel = with_noise(color, noise);
    }
    match pattern {
        Pattern::Speckle { color, density } => {
            // `density` distinct texels (partial Fisher-Yates over the texel indices).
            let mut order: [u16; BASE_TEXELS] = std::array::from_fn(|i| i as u16);
            for k in 0..usize::from(density).min(BASE_TEXELS) {
                let pick = k + rng.below((BASE_TEXELS - k) as u32) as usize;
                order.swap(k, pick);
                let noise = rng.range_inclusive(-amplitude, amplitude);
                texels[usize::from(order[k])] = with_noise(color, noise);
            }
        }
        Pattern::Leaves => {
            for texel in &mut texels {
                if rng.below(4) == 0 {
                    texel[3] = 0;
                }
            }
        }
        Pattern::Planks => {
            for (i, texel) in texels.iter_mut().enumerate() {
                if i / size % 4 == 3 {
                    for c in &mut texel[..3] {
                        *c = (u16::from(*c) * 7 / 10) as u8;
                    }
                }
            }
        }
        Pattern::Bark => {
            // Cracks on 5 of the 16 columns (spread unevenly so no grid shows), each a run of
            // texels at 6/10 brightness with a gap of 1 to 3 texels somewhere along it.
            for x in [1, 4, 8, 11, 14].map(|x| x * size / 16) {
                let gap = rng.below(size as u32) as usize;
                let gap_len = 1 + rng.below(3) as usize;
                for y in (0..size).filter(|y| (y + size - gap) % size >= gap_len) {
                    for c in &mut texels[y * size + x][..3] {
                        *c = (u16::from(*c) * 6 / 10) as u8;
                    }
                }
            }
        }
        Pattern::Glass => {
            // A light frame, two short streaks and an almost clear inside.
            for (i, texel) in texels.iter_mut().enumerate() {
                let (x, y) = (i % size, i / size);
                let frame = x == 0 || y == 0 || x == size - 1 || y == size - 1;
                let streak = (x + 3 == y + 6 && (3..=7).contains(&x))
                    || (x + 3 == y && (8..=11).contains(&x));
                if streak {
                    *texel = [250, 252, 255, 120];
                } else {
                    texel[3] = if frame { 200 } else { 24 };
                }
            }
        }
        Pattern::Liquid { alpha } => {
            for texel in &mut texels {
                texel[3] = alpha;
            }
        }
        Pattern::Plant => {
            // Blades of random height from the bottom row, darker near the ground.
            for texel in &mut texels {
                texel[3] = 0;
            }
            for x in 1..size - 1 {
                if rng.below(3) == 0 {
                    continue;
                }
                let height = 5 + rng.below(10) as usize;
                for y in size - height..size {
                    let t = &mut texels[y * size + x];
                    t[3] = 255;
                    if y + 3 >= size {
                        for c in &mut t[..3] {
                            *c = (u16::from(*c) * 3 / 4) as u8;
                        }
                    }
                }
            }
        }
        Pattern::Flower { color } => {
            // A stem with two leaves (base color) under a round head of `color` with a light
            // center.
            for texel in &mut texels {
                texel[3] = 0;
            }
            let stem = [
                (7, 8),
                (7, 9),
                (7, 10),
                (7, 11),
                (7, 12),
                (7, 13),
                (7, 14),
                (7, 15),
            ];
            for (x, y) in stem.into_iter().chain([(6, 11), (8, 12), (5, 10), (9, 13)]) {
                texels[y * size + x][3] = 255;
            }
            for y in 3..8usize {
                for x in 5..10usize {
                    let (dx, dy) = (x.abs_diff(7), y.abs_diff(5));
                    if dx + dy <= 3 {
                        let noise = rng.range_inclusive(-amplitude, amplitude);
                        let center = dx + dy == 0;
                        texels[y * size + x] =
                            with_noise(if center { [240, 220, 90] } else { color }, noise);
                    }
                }
            }
        }
        Pattern::Torch => {
            // Minecraft's layout: a 2 texel stick (columns 7 and 8) from row 8 down, under a
            // flame in rows 6 and 7. The model boxes of the torches sample exactly these texels
            // (ADR 0020).
            for (i, texel) in texels.iter_mut().enumerate() {
                let (x, y) = (i % size, i / size);
                if !(7..9).contains(&x) || y < 6 {
                    texel[3] = 0;
                } else if y == 6 {
                    *texel = [255, 248, 200, 255];
                } else if y == 7 {
                    *texel = [255, 200, 80, 255];
                }
            }
        }
        Pattern::Cobble => {
            // Stones around 7 fixed centers (the nearest center, wrapping, so the layer tiles),
            // each a little lighter or darker and lit toward its middle; dark joints where two
            // centers are about as near.
            const CENTERS: [(i32, i32); 7] =
                [(3, 3), (10, 2), (14, 9), (7, 8), (2, 12), (11, 14), (6, 15)];
            const TONE: [u16; 7] = [100, 90, 108, 96, 104, 88, 112];
            let n = size as i32;
            for (i, texel) in texels.iter_mut().enumerate() {
                let (x, y) = ((i % size) as i32, (i / size) as i32);
                let wrap = |d: i32| d.abs().min(n - d.abs());
                let mut near = [(i32::MAX, 0usize); 2];
                for (k, &(cx, cy)) in CENTERS.iter().enumerate() {
                    let d = wrap(x - cx).pow(2) + wrap(y - cy).pow(2);
                    if d < near[0].0 {
                        near = [(d, k), near[0]];
                    } else if d < near[1].0 {
                        near[1] = (d, k);
                    }
                }
                let factor = if near[1].0 - near[0].0 <= 4 {
                    55
                } else {
                    TONE[near[0].1] + if near[0].0 <= 2 { 8 } else { 0 }
                };
                for c in &mut texel[..3] {
                    *c = (u16::from(*c) * factor / 100).min(255) as u8;
                }
            }
        }
        Pattern::Flow { alpha } => {
            // Streaks down every column (ADR 0025): each column has its own phase and a light
            // dash of 3 texels and a dark one of 2 every 8 rows, so the layer tiles along v and
            // a scroll along v reads as fluid running.
            for x in 0..size {
                let phase = rng.below(8) as usize;
                for y in 0..size {
                    let factor = match (y + phase) % 8 {
                        0..=2 => 125,
                        4..=5 => 82,
                        _ => 100,
                    };
                    let texel = &mut texels[y * size + x];
                    for c in &mut texel[..3] {
                        *c = (u16::from(*c) * factor / 100).min(255) as u8;
                    }
                    texel[3] = alpha;
                }
            }
        }
        Pattern::Noise | Pattern::TopBand { .. } => {}
    }
    texels
}

/// `color + noise` per channel, clamped, alpha 255.
fn with_noise(color: [u8; 3], noise: i32) -> [u8; 4] {
    let ch = |c: u8| (i32::from(c) + noise).clamp(0, 255) as u8;
    [ch(color[0]), ch(color[1]), ch(color[2]), 255]
}

/// FNV-1a, 64 bit (texture seeds).
fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// `SplitMix64`: small, fast and identical on every platform.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n` (`n > 0`; the modulo bias is irrelevant at these sizes).
    fn below(&mut self, n: u32) -> u32 {
        (self.next_u64() % u64::from(n)) as u32
    }

    /// Uniform in `lo..=hi`.
    fn range_inclusive(&mut self, lo: i32, hi: i32) -> i32 {
        lo + self.below((hi - lo + 1) as u32) as i32
    }
}
