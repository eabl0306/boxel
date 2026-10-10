use std::sync::Arc;

use boxel_voxel::TextureSpec;

use crate::texture_animation::LayerAnimation;

/// Edge of the magenta layer of a registry without texture layers, and of the layers read from a
/// resource pack without textures. The registry's images and a resource pack give the array
/// their own edge ([`BlockTextureData::size`]).
pub const BLOCK_TEXTURE_SIZE: u32 = 16;

/// Texels of the magenta layer.
const BASE_TEXELS: usize = (BLOCK_TEXTURE_SIZE * BLOCK_TEXTURE_SIZE) as usize;
/// Layer used when the registry has no textures.
const MAGENTA: [u8; 4] = [255, 0, 255, 255];

/// CPU-side block texture array: `layers` layers of `size`^2 RGBA8 sRGB texels with a full mip
/// chain (`size`, `size / 2`, ..., 1), stored layer-major then mip-major
/// (`wgpu::util::TextureDataOrder::LayerMajor`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockTextureData {
    /// Edge of level 0 in texels (a power of two: the edge of the registry's images, or of the
    /// pack's layers).
    pub size: u32,
    /// Layer count (>= 1; a magenta layer is used when `specs` is empty).
    pub layers: u32,
    /// Mip levels (`log2(size) + 1`, 5 for 16).
    pub mip_levels: u32,
    /// Texel bytes.
    pub data: Vec<u8>,
    /// The animated layers (ADR 0028; none without a pack). `data` holds the frame each one shows
    /// at tick 0.
    pub animations: Arc<[LayerAnimation]>,
    /// Whether the flow texture of the fluids that emit no light (`[0]`) and of those that emit
    /// light (`[1]`) is one of `animations`: its frames show the fluid running, so the shaders do
    /// not scroll it (ADR 0025 and 0028).
    pub animated_flow: [bool; 2],
}

impl BlockTextureData {
    /// Bytes of one layer with its whole mip chain.
    pub fn layer_bytes(&self) -> usize {
        layer_bytes(self.size)
    }
}

/// Builds the texture array of the registry's layers: each one's image ([`TextureSpec::image`])
/// with its mips, box-filtered in linear space. A layer with binary alpha keeps it binary (0/255)
/// in every mip. No layers give one magenta layer.
///
/// # Panics
///
/// If an image is not valid or its edge differs from the first one's (a registry checks both).
pub fn generate_block_textures(specs: &[TextureSpec]) -> BlockTextureData {
    let size = specs.first().map_or(BLOCK_TEXTURE_SIZE, |s| s.image.size);
    let mut data = Vec::with_capacity(layer_bytes(size) * specs.len().max(1));
    if specs.is_empty() {
        push_layer(&mut data, &[MAGENTA; BASE_TEXELS], size, false);
    }
    for spec in specs {
        let image = &spec.image;
        assert!(
            image.is_valid() && image.size == size,
            "texture layer {}: not a {size}x{size} image",
            spec.name
        );
        push_layer(&mut data, &image.texels, size, image.binary_alpha);
    }
    BlockTextureData {
        size,
        layers: specs.len().max(1) as u32,
        mip_levels: mip_levels(size),
        data,
        animations: Arc::from([]),
        animated_flow: [false; 2],
    }
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

/// Appends level 0 (`size`^2 texels) and its mips (box filter in linear space, alpha-weighted
/// color so transparent texels do not bleed into the average). `binary_alpha` keeps alpha 0 or
/// 255 in every mip without losing coverage ([`binary_mip_alpha`]).
pub(crate) fn push_layer(out: &mut Vec<u8>, base: &[[u8; 4]], size: u32, binary_alpha: bool) {
    debug_assert_eq!(base.len(), (size * size) as usize);
    out.extend(base.iter().flatten());
    let mut level: Vec<[f32; 4]> = base.iter().map(|t| decode(*t)).collect();
    let coverage = level.iter().filter(|t| t[3] >= 0.5).count() as f64 / level.len().max(1) as f64;
    let mut size = size as usize;
    while size > 1 {
        level = downsample(&level, size);
        size /= 2;
        if binary_alpha {
            let opaque = binary_mip_alpha(&level, size, coverage);
            out.extend(level.iter().zip(opaque).flat_map(|(t, opaque)| {
                let mut texel = encode(*t, false);
                texel[3] = if opaque { 255 } else { 0 };
                texel
            }));
        } else {
            out.extend(level.iter().flat_map(|t| encode(*t, false)));
        }
    }
}

/// Which texels of a binary-alpha mip (`size`^2, alpha = the coverage of level 0 under each one)
/// are opaque. Those with alpha >= 0.5, unless fewer than `coverage` (level 0's opaque fraction)
/// of the texels pass: then the `coverage` share of the texels with the most alpha, ties in a
/// Bayer order, and never one with alpha 0. Thin cutout textures (leaves covering under half,
/// plants) keep their coverage far away instead of fading out with the 0.5 threshold (issue
/// #45); textures that keep it at 0.5 mip as before.
fn binary_mip_alpha(level: &[[f32; 4]], size: usize, coverage: f64) -> Vec<bool> {
    let at_half: Vec<bool> = level.iter().map(|t| t[3] >= 0.5).collect();
    let want = (coverage * level.len() as f64).round() as usize;
    if at_half.iter().filter(|&&o| o).count() >= want {
        return at_half;
    }
    let bits = size.trailing_zeros();
    let mut order: Vec<usize> = (0..level.len()).filter(|&i| level[i][3] > 0.0).collect();
    order.sort_by(|&a, &b| {
        level[b][3]
            .total_cmp(&level[a][3])
            .then_with(|| bayer(a % size, a / size, bits).cmp(&bayer(b % size, b / size, bits)))
    });
    let mut opaque = vec![false; level.len()];
    for &i in order.iter().take(want) {
        opaque[i] = true;
    }
    opaque
}

/// Rank of `(x, y)` in the ordered dither matrix of `2^bits` x `2^bits` texels: consecutive
/// ranks spread evenly over the area.
fn bayer(x: usize, y: usize, bits: u32) -> usize {
    (0..bits).fold(0, |v, i| {
        let (xi, yi) = ((x >> i) & 1, (y >> i) & 1);
        (v << 2) | ((xi ^ yi) << 1) | yi
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use boxel_voxel::TextureImage;

    /// A layer of one color.
    fn flat(name: &str, color: [u8; 4]) -> TextureSpec {
        TextureSpec {
            name: name.to_string(),
            image: TextureImage::solid(BLOCK_TEXTURE_SIZE, color),
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
        let registry = boxel_demo::blocks::registry();
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
    fn leaves_alpha_is_binary_in_every_mip() {
        let registry = boxel_demo::blocks::registry();
        let specs = registry.textures();
        let data = generate_block_textures(specs);
        let mut leaves_layers = 0;
        for (i, s) in specs.iter().enumerate() {
            let leaves = s.name == "boxel:leaves";
            let binary = s.image.binary_alpha;
            // Glass and water keep their alpha for blending.
            let blended = !binary && s.image.texels.iter().any(|t| t[3] != 255);
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

    /// Mip `level` of a single `size`^2 layer pushed with binary alpha.
    fn binary_mips(base: &[[u8; 4]], size: u32) -> Vec<Vec<[u8; 4]>> {
        let mut out = Vec::new();
        push_layer(&mut out, base, size, true);
        let texels: Vec<[u8; 4]> = out.as_chunks::<4>().0.to_vec();
        let mut offset = 0;
        (0..mip_levels(size))
            .map(|m| {
                let n = ((size >> m) * (size >> m)) as usize;
                offset += n;
                texels[offset - n..offset].to_vec()
            })
            .collect()
    }

    fn opaque_share(level: &[[u8; 4]]) -> f64 {
        level.iter().filter(|t| t[3] == 255).count() as f64 / level.len() as f64
    }

    /// Issue #45: a cutout texture covering under half keeps its coverage in every mip instead of
    /// vanishing at the 0.5 threshold, never turns a fully transparent area opaque, and one that
    /// keeps its coverage at 0.5 mips as before.
    #[test]
    fn binary_mips_keep_the_coverage() {
        let leaf = [40, 120, 30, 255];
        // isolated texels, one per 2x2 block: mip 1 is a uniform 0.25 coverage
        let dots: Vec<[u8; 4]> = (0..256)
            .map(|i| {
                if i % 2 == 0 && (i / 16) % 2 == 0 {
                    leaf
                } else {
                    [0; 4]
                }
            })
            .collect();
        let mips = binary_mips(&dots, 16);
        for (m, level) in mips.iter().enumerate().take(4) {
            assert!((opaque_share(level) - 0.25).abs() < 1e-9, "mip {m}");
            for t in level.iter().filter(|t| t[3] == 255) {
                assert_eq!(&t[..3], &leaf[..3], "mip {m}: only the leaf's color");
            }
        }
        // the ties of mip 1 spread out: one opaque texel in every 2x2 block of it
        let mip1 = &mips[1];
        for by in 0..4 {
            for bx in 0..4 {
                let n = (0..4)
                    .filter(|k| mip1[(2 * by + k / 2) * 8 + 2 * bx + k % 2][3] == 255)
                    .count();
                assert_eq!(n, 1, "block {bx},{by}");
            }
        }
        // an irregular sparse leaf on the right half only: the left half stays clear
        let sparse: Vec<[u8; 4]> = (0..256u32)
            .map(|i| {
                let (x, y) = (i % 16, i / 16);
                if x >= 8 && (x * 7 + y * 13) % 3 == 0 {
                    leaf
                } else {
                    [0; 4]
                }
            })
            .collect();
        let base = opaque_share(&sparse);
        for (m, level) in binary_mips(&sparse, 16).iter().enumerate() {
            let edge = 16 >> m;
            let want = (base * (edge * edge) as f64).round() / (edge * edge) as f64;
            assert!(opaque_share(level) >= want, "mip {m}");
            for (i, t) in level.iter().enumerate() {
                if i % edge < edge / 2 {
                    assert_eq!(t[3], 0, "mip {m} texel {i}");
                }
            }
        }
        // dense leaves (3 of 4 texels): the plain 0.5 threshold, as before
        let dense: Vec<[u8; 4]> = (0..256)
            .map(|i| {
                if (i * 5 + i / 16) % 4 == 0 {
                    [0; 4]
                } else {
                    leaf
                }
            })
            .collect();
        let mut out = Vec::new();
        push_layer(&mut out, &dense, 16, true);
        let mut level: Vec<[f32; 4]> = dense.iter().map(|t| decode(*t)).collect();
        let mut offset = 256 * 4;
        let mut size = 16;
        while size > 1 {
            level = downsample(&level, size);
            size /= 2;
            for t in &level {
                assert_eq!(out[offset + 3], encode(*t, true)[3]);
                offset += 4;
            }
        }
        // the procedural leaves and grass keep their coverage at 0.5: their mips do not change
        // (the poppy and the torch, thinner, gain a texel or two in their small mips)
        let registry = boxel_demo::blocks::registry();
        for spec in registry
            .textures()
            .iter()
            .filter(|s| ["boxel:leaves", "boxel:short_grass"].contains(&s.name.as_str()))
        {
            let base = &spec.image.texels;
            let mut out = Vec::new();
            push_layer(&mut out, base, 16, true);
            let mut level: Vec<[f32; 4]> = base.iter().map(|t| decode(*t)).collect();
            let (mut offset, mut size) = (256 * 4, 16);
            while size > 1 {
                level = downsample(&level, size);
                size /= 2;
                for t in &level {
                    assert_eq!(out[offset + 3], encode(*t, true)[3], "{}", spec.name);
                    offset += 4;
                }
            }
        }
        assert_eq!(bayer(0, 0, 1), 0);
        assert_eq!(bayer(1, 1, 1), 1);
        assert_eq!(bayer(1, 0, 1), 2);
        assert_eq!(bayer(0, 1, 1), 3);
    }

    #[test]
    fn mips_average_in_linear_space() {
        // a uniform color survives every mip exactly
        let data = generate_block_textures(&[flat("test:flat", [120, 90, 60, 255])]);
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
