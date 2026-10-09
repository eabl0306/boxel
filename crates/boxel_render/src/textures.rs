use boxel_voxel::{TexturePattern, TextureSpec};

/// Edge of every procedural block texture layer (texels). A resource pack can give the array
/// another edge ([`BlockTextureData::size`]).
pub const BLOCK_TEXTURE_SIZE: u32 = 16;

/// Texels of a procedural level 0.
const BASE_TEXELS: usize = (BLOCK_TEXTURE_SIZE * BLOCK_TEXTURE_SIZE) as usize;
/// Layer used when the registry has no textures.
const MAGENTA: [u8; 4] = [255, 0, 255, 255];

/// CPU-side block texture array: `layers` layers of `size`^2 RGBA8 sRGB texels with a full mip
/// chain (`size`, `size / 2`, ..., 1), stored layer-major then mip-major
/// (`wgpu::util::TextureDataOrder::LayerMajor`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockTextureData {
    /// Edge of level 0 in texels (a power of two; [`BLOCK_TEXTURE_SIZE`] for procedural layers).
    pub size: u32,
    /// Layer count (>= 1; a magenta layer is used when `specs` is empty).
    pub layers: u32,
    /// Mip levels (`log2(size) + 1`, 5 for 16).
    pub mip_levels: u32,
    /// Texel bytes.
    pub data: Vec<u8>,
}

impl BlockTextureData {
    /// Bytes of one layer with its whole mip chain.
    pub fn layer_bytes(&self) -> usize {
        layer_bytes(self.size)
    }
}

/// Synthesizes the texture array from the registry's specs (deterministic per spec name).
/// Mips are box-filtered in linear space; `Leaves` alpha is kept binary (0/255) in every mip.
pub fn generate_block_textures(specs: &[TextureSpec]) -> BlockTextureData {
    let size = BLOCK_TEXTURE_SIZE;
    let mut data = Vec::with_capacity(layer_bytes(size) * specs.len().max(1));
    if specs.is_empty() {
        push_layer(&mut data, &[MAGENTA; BASE_TEXELS], size, false);
    }
    for spec in specs {
        let base = synthesize(spec);
        push_layer(&mut data, &base, size, is_cutout_pattern(spec));
    }
    BlockTextureData {
        size,
        layers: specs.len().max(1) as u32,
        mip_levels: mip_levels(size),
        data,
    }
}

/// Whether a procedural layer keeps binary alpha (the `Leaves`, `Plant` and `Flower` patterns).
pub(crate) fn is_cutout_pattern(spec: &TextureSpec) -> bool {
    matches!(
        spec.pattern,
        TexturePattern::Leaves
            | TexturePattern::Plant
            | TexturePattern::Flower { .. }
            | TexturePattern::Torch
    )
}

/// Mip levels of a `size`^2 layer down to 1x1.
pub(crate) fn mip_levels(size: u32) -> u32 {
    size.ilog2() + 1
}

/// Bytes of one `size`^2 layer with its whole mip chain.
pub(crate) fn layer_bytes(size: u32) -> usize {
    (0..mip_levels(size))
        .map(|mip| ((size >> mip) * (size >> mip) * 4) as usize)
        .sum()
}

/// Level 0 of one procedural layer: `BLOCK_TEXTURE_SIZE`^2 sRGB texels, row 0 = top of the
/// image.
pub(crate) fn synthesize(spec: &TextureSpec) -> [[u8; 4]; BASE_TEXELS] {
    let size = BLOCK_TEXTURE_SIZE as usize;
    let mut rng = SplitMix64(fnv1a64(spec.name.as_bytes()));
    let amplitude = i32::from(spec.noise);
    let mut texels = [[0, 0, 0, 255]; BASE_TEXELS];
    for (i, texel) in texels.iter_mut().enumerate() {
        let noise = rng.range_inclusive(-amplitude, amplitude);
        let color = match spec.pattern {
            TexturePattern::TopBand { color, rows } if i / size < usize::from(rows) => color,
            _ => spec.base_color,
        };
        *texel = with_noise(color, noise);
    }
    match spec.pattern {
        TexturePattern::Speckle { color, density } => {
            // `density` distinct texels (partial Fisher-Yates over the texel indices).
            let mut order: [u16; BASE_TEXELS] = std::array::from_fn(|i| i as u16);
            for k in 0..usize::from(density).min(BASE_TEXELS) {
                let pick = k + rng.below((BASE_TEXELS - k) as u32) as usize;
                order.swap(k, pick);
                let noise = rng.range_inclusive(-amplitude, amplitude);
                texels[usize::from(order[k])] = with_noise(color, noise);
            }
        }
        TexturePattern::Leaves => {
            for texel in &mut texels {
                if rng.below(4) == 0 {
                    texel[3] = 0;
                }
            }
        }
        TexturePattern::Planks => {
            for (i, texel) in texels.iter_mut().enumerate() {
                if i / size % 4 == 3 {
                    for c in &mut texel[..3] {
                        *c = (u16::from(*c) * 7 / 10) as u8;
                    }
                }
            }
        }
        TexturePattern::Bark => {
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
        TexturePattern::Glass => {
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
        TexturePattern::Liquid { alpha } => {
            for texel in &mut texels {
                texel[3] = alpha;
            }
        }
        TexturePattern::Plant => {
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
        TexturePattern::Flower { color } => {
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
        TexturePattern::Torch => {
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
        TexturePattern::Cobble => {
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
        TexturePattern::Flow { alpha } => {
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
        TexturePattern::Noise | TexturePattern::TopBand { .. } => {}
    }
    texels
}

/// `color + noise` per channel, clamped, alpha 255.
fn with_noise(color: [u8; 3], noise: i32) -> [u8; 4] {
    let ch = |c: u8| (i32::from(c) + noise).clamp(0, 255) as u8;
    [ch(color[0]), ch(color[1]), ch(color[2]), 255]
}

/// Appends level 0 (`size`^2 texels) and its mips (box filter in linear space, alpha-weighted
/// color so transparent texels do not bleed into the average; `binary_alpha` thresholds alpha at
/// 0.5 in every mip).
pub(crate) fn push_layer(out: &mut Vec<u8>, base: &[[u8; 4]], size: u32, binary_alpha: bool) {
    debug_assert_eq!(base.len(), (size * size) as usize);
    out.extend(base.iter().flatten());
    let mut level: Vec<[f32; 4]> = base.iter().map(|t| decode(*t)).collect();
    let mut size = size as usize;
    while size > 1 {
        level = downsample(&level, size);
        size /= 2;
        out.extend(level.iter().flat_map(|t| encode(*t, binary_alpha)));
    }
}

/// 2x2 box filter of a `size`^2 level (linear RGB + coverage in `[0, 1]`). Cascading it keeps
/// the exact coverage-weighted mean of the base texels under each output texel.
fn downsample(src: &[[f32; 4]], size: usize) -> Vec<[f32; 4]> {
    let half = size / 2;
    let mut dst = Vec::with_capacity(half * half);
    for y in 0..half {
        for x in 0..half {
            let quad = [
                src[2 * y * size + 2 * x],
                src[2 * y * size + 2 * x + 1],
                src[(2 * y + 1) * size + 2 * x],
                src[(2 * y + 1) * size + 2 * x + 1],
            ];
            let coverage: f32 = quad.iter().map(|t| t[3]).sum();
            let mut out = [0.0, 0.0, 0.0, coverage / 4.0];
            for (c, value) in out.iter_mut().take(3).enumerate() {
                *value = if coverage > 0.0 {
                    quad.iter().map(|t| t[c] * t[3]).sum::<f32>() / coverage
                } else {
                    quad.iter().map(|t| t[c]).sum::<f32>() / 4.0
                };
            }
            dst.push(out);
        }
    }
    dst
}

fn decode(t: [u8; 4]) -> [f32; 4] {
    [
        srgb_to_linear(t[0]),
        srgb_to_linear(t[1]),
        srgb_to_linear(t[2]),
        f32::from(t[3]) / 255.0,
    ]
}

fn encode(t: [f32; 4], binary_alpha: bool) -> [u8; 4] {
    let alpha = if binary_alpha {
        if t[3] >= 0.5 { 255 } else { 0 }
    } else {
        (t[3].clamp(0.0, 1.0) * 255.0).round() as u8
    };
    [
        linear_to_srgb(t[0]),
        linear_to_srgb(t[1]),
        linear_to_srgb(t[2]),
        alpha,
    ]
}

/// sRGB transfer function, decode.
pub(crate) fn srgb_to_linear(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB transfer function, encode (rounded to 8 bits).
pub(crate) fn linear_to_srgb(l: f32) -> u8 {
    let l = l.clamp(0.0, 1.0);
    let s = if l <= 0.003_130_8 {
        l * 12.92
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
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

#[cfg(test)]
mod tests {
    use super::*;
    use boxel_voxel::BlockRegistry;

    fn spec(name: &str, pattern: TexturePattern) -> TextureSpec {
        TextureSpec {
            name: name.to_string(),
            base_color: [120, 90, 60],
            noise: 16,
            pattern,
        }
    }

    /// Byte offset and edge (texels) of `level` inside a layer.
    fn mip_range(level: u32) -> (usize, usize) {
        let offset = (0..level)
            .map(|m| ((BLOCK_TEXTURE_SIZE >> m) * (BLOCK_TEXTURE_SIZE >> m) * 4) as usize)
            .sum();
        (offset, (BLOCK_TEXTURE_SIZE >> level) as usize)
    }

    fn layer(data: &BlockTextureData, index: usize) -> &[u8] {
        &data.data[index * data.layer_bytes()..(index + 1) * data.layer_bytes()]
    }

    fn mip(data: &BlockTextureData, index: usize, level: u32) -> Vec<[u8; 4]> {
        let (offset, size) = mip_range(level);
        layer(data, index)[offset..offset + size * size * 4]
            .as_chunks::<4>()
            .0
            .to_vec()
    }

    #[test]
    fn sizes_match_the_layer_major_mip_chain() {
        let registry = BlockRegistry::default_blocks();
        let specs = registry.textures();
        let data = generate_block_textures(specs);
        assert_eq!(data.layers as usize, specs.len());
        assert_eq!((data.size, data.mip_levels), (16, 5));
        assert_eq!(layer_bytes(16), 4 * (256 + 64 + 16 + 4 + 1));
        assert_eq!(layer_bytes(32), 4 * (1024 + 256 + 64 + 16 + 4 + 1));
        assert_eq!(data.data.len(), specs.len() * layer_bytes(16));
    }

    #[test]
    fn empty_input_gives_one_magenta_layer() {
        let data = generate_block_textures(&[]);
        assert_eq!((data.layers, data.mip_levels), (1, 5));
        assert_eq!(data.data.len(), layer_bytes(16));
        assert!(data.data.as_chunks::<4>().0.iter().all(|t| *t == MAGENTA));
    }

    #[test]
    fn generation_is_deterministic_per_name() {
        let a = spec("test:a", TexturePattern::Noise);
        let b = spec("test:b", TexturePattern::Noise);
        let first = generate_block_textures(&[a.clone(), b.clone()]);
        assert_eq!(first, generate_block_textures(&[a.clone(), b.clone()]));
        // a layer depends on its spec only, not on its position in the list
        let swapped = generate_block_textures(&[b, a]);
        assert_eq!(layer(&first, 0), layer(&swapped, 1));
        assert_eq!(layer(&first, 1), layer(&swapped, 0));
        assert_ne!(layer(&first, 0), layer(&first, 1));
    }

    #[test]
    fn leaves_alpha_is_binary_in_every_mip() {
        let registry = BlockRegistry::default_blocks();
        let specs = registry.textures();
        let data = generate_block_textures(specs);
        let mut leaves_layers = 0;
        for (i, s) in specs.iter().enumerate() {
            let leaves = matches!(s.pattern, TexturePattern::Leaves);
            let binary = is_cutout_pattern(s);
            // Glass and water keep their alpha for blending.
            let blended = matches!(
                s.pattern,
                TexturePattern::Glass | TexturePattern::Liquid { .. } | TexturePattern::Flow { .. }
            );
            for level in 0..data.mip_levels {
                for t in mip(&data, i, level) {
                    if binary {
                        assert!(t[3] == 0 || t[3] == 255, "{} mip {level}: {t:?}", s.name);
                    } else if !blended {
                        assert_eq!(t[3], 255, "{} mip {level}", s.name);
                    }
                }
            }
            if leaves {
                let holes = mip(&data, i, 0).iter().filter(|t| t[3] == 0).count();
                assert!((26..=102).contains(&holes), "{holes} holes of 256");
                leaves_layers += 1;
            }
        }
        assert_eq!(leaves_layers, 1, "default_blocks has one Leaves texture");
    }

    #[test]
    fn patterns_follow_the_spec() {
        let mut band = spec(
            "test:band",
            TexturePattern::TopBand {
                color: [10, 200, 10],
                rows: 4,
            },
        );
        band.noise = 0;
        let data = generate_block_textures(&[band]);
        for (i, t) in mip(&data, 0, 0).iter().enumerate() {
            let expected = if i / 16 < 4 {
                [10, 200, 10, 255]
            } else {
                [120, 90, 60, 255]
            };
            assert_eq!(*t, expected, "texel {i}");
        }

        let mut speckle = spec(
            "test:speckle",
            TexturePattern::Speckle {
                color: [0, 0, 255],
                density: 40,
            },
        );
        speckle.noise = 0;
        let data = generate_block_textures(&[speckle]);
        let blue = mip(&data, 0, 0)
            .iter()
            .filter(|t| **t == [0, 0, 255, 255])
            .count();
        assert_eq!(blue, 40);

        let mut planks = spec("test:planks", TexturePattern::Planks);
        planks.noise = 0;
        let data = generate_block_textures(&[planks]);
        let texels = mip(&data, 0, 0);
        assert_eq!(texels[3 * 16], [84, 63, 42, 255]);
        assert_eq!(texels[2 * 16], [120, 90, 60, 255]);

        // a flow texture: streaks that repeat every 8 rows, lighter and darker than the base
        let mut flow = spec("test:flow", TexturePattern::Flow { alpha: 160 });
        flow.noise = 0;
        let data = generate_block_textures(&[flow]);
        let texels = mip(&data, 0, 0);
        for (i, t) in texels.iter().enumerate() {
            assert_eq!(t[3], 160);
            assert_eq!(*t, texels[(i + 8 * 16) % 256], "texel {i}");
        }
        for x in 0..16 {
            let column: Vec<u8> = (0..16).map(|y| texels[y * 16 + x][0]).collect();
            assert!(column.contains(&150) && column.contains(&98) && column.contains(&120));
        }

        // noise stays within the amplitude
        let noisy = generate_block_textures(&[spec("test:noise", TexturePattern::Noise)]);
        for t in mip(&noisy, 0, 0) {
            assert!(t[0].abs_diff(120) <= 16 && t[1].abs_diff(90) <= 16 && t[2].abs_diff(60) <= 16);
        }
    }

    #[test]
    fn mips_average_in_linear_space() {
        // a uniform color survives every mip exactly
        let mut flat = spec("test:flat", TexturePattern::Noise);
        flat.noise = 0;
        let data = generate_block_textures(&[flat]);
        for level in 0..5 {
            assert!(
                mip(&data, 0, level)
                    .iter()
                    .all(|t| *t == [120, 90, 60, 255])
            );
        }
        // black/white checkerboard: the linear mean 0.5 is sRGB 188, not 128
        let base: Vec<[f32; 4]> = (0..256)
            .map(|i| {
                let v = if (i % 16 + i / 16) % 2 == 0 { 1.0 } else { 0.0 };
                [v, v, v, 1.0]
            })
            .collect();
        let half = downsample(&base, 16);
        assert_eq!(half.len(), 64);
        assert!(
            half.iter()
                .all(|t| encode(*t, false) == [188, 188, 188, 255])
        );
        // transparent texels do not darken the visible ones, and 50 % coverage stays opaque
        let mut cutout = base.clone();
        for (i, t) in cutout.iter_mut().enumerate() {
            if (i % 16 + i / 16) % 2 == 1 {
                *t = [0.0, 0.0, 0.0, 0.0];
            }
        }
        let half = downsample(&cutout, 16);
        assert!(
            half.iter()
                .all(|t| encode(*t, true) == [255, 255, 255, 255])
        );
    }

    #[test]
    fn srgb_round_trips_every_byte() {
        for c in 0..=255u8 {
            assert_eq!(linear_to_srgb(srgb_to_linear(c)), c);
        }
    }
}
