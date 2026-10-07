//! Block textures from a Minecraft Java resource pack (`boxel_pack`). The pack only replaces the
//! pixels of the registry's texture layers: layer ids, their order, the mip chain and the upload
//! path stay the ones of the procedural textures, so every tier samples it the same way.
//!
//! Each built-in layer has a [`Recipe`]: the Minecraft block whose blockstate and model name the
//! texture (`cube_column` end/side, the grass overlay...), and fallback texture names for packs
//! that ship no model. Grayscale textures Minecraft tints by biome (grass, foliage, water) get a
//! fixed plains tint baked in. Layers the pack cannot fill keep their procedural pixels and add a
//! warning to the [`TextureReport`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use boxel_pack::{BlockState as PackState, FaceName, ResourceId, ResourcePack, Texture};
use boxel_voxel::{BlockRegistry, BlockState, Face, RenderShape, TextureSpec};

use crate::textures::{
    BLOCK_TEXTURE_SIZE, BlockTextureData, generate_block_textures, is_cutout_pattern, layer_bytes,
    mip_levels, push_layer, synthesize,
};

/// Largest layer edge taken from a pack (bigger packs are scaled down to it).
pub const MAX_PACK_TEXTURE_SIZE: u32 = 512;
/// Minecraft's plains grass color (sRGB), baked into grass textures with a tint index.
pub const GRASS_TINT: [u8; 3] = [0x91, 0xBD, 0x59];
/// Minecraft's plains foliage color (sRGB), baked into leaves.
pub const FOLIAGE_TINT: [u8; 3] = [0x77, 0xAB, 0x2F];
/// Minecraft's default water color (sRGB), baked into the grayscale water texture.
pub const WATER_TINT: [u8; 3] = [0x3F, 0x76, 0xE4];

/// Where the pixels of the block texture layers come from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TextureSource {
    /// Synthesized from each [`TextureSpec`] (no files; the default).
    #[default]
    Procedural,
    /// A Minecraft Java resource pack directory (the one holding `pack.mcmeta`), read at startup.
    Pack(PathBuf),
}

impl TextureSource {
    /// `procedural`, or the pack directory's last component.
    pub fn label(&self) -> String {
        match self {
            Self::Procedural => "procedural".to_string(),
            Self::Pack(path) => path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
        }
    }
}

/// What building the block texture array did (also in `RenderInfo::textures`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextureReport {
    /// [`TextureSource::label`] of the source used (`procedural` when a pack failed to open).
    pub source: String,
    /// Layer edge in texels.
    pub size: u32,
    /// Layers in the array.
    pub layers: u32,
    /// Layers whose pixels came from the pack.
    pub from_pack: u32,
    /// Wall time of reading the pack and building the array (CPU only, no upload).
    pub load_time: Duration,
    /// Non-fatal warnings of the pack reader itself (skipped assets, odd sizes...).
    pub pack_warnings: usize,
    /// Mapping problems: layers that kept their procedural pixels, missing overlays, a pack that
    /// failed to open.
    pub warnings: Vec<String>,
}

/// Builds the block texture array of `registry` from `source`. A pack that cannot be opened is
/// an error; missing textures inside a pack are warnings (the layer stays procedural).
pub fn load_block_textures(
    registry: &BlockRegistry,
    source: &TextureSource,
) -> Result<(BlockTextureData, TextureReport), String> {
    let started = Instant::now();
    let (data, from_pack, pack_warnings, warnings) = match source {
        TextureSource::Procedural => (generate_block_textures(registry.textures()), 0, 0, vec![]),
        TextureSource::Pack(path) => {
            let pack = open_pack(path)?;
            let mut warnings = Vec::new();
            let (data, from_pack) = pack_block_textures(registry, &pack, &mut warnings);
            (data, from_pack, pack.warnings().len(), warnings)
        }
    };
    let report = TextureReport {
        source: source.label(),
        size: data.size,
        layers: data.layers,
        from_pack,
        load_time: started.elapsed(),
        pack_warnings,
        warnings,
    };
    Ok((data, report))
}

fn open_pack(path: &Path) -> Result<ResourcePack, String> {
    ResourcePack::open(path).map_err(|e| format!("resource pack {}: {e}", path.display()))
}

/// Biome tint of a texture part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tint {
    None,
    Grass,
    Foliage,
    Water,
}

impl Tint {
    fn rgb(self) -> Option<[u8; 3]> {
        match self {
            Self::None => None,
            Self::Grass => Some(GRASS_TINT),
            Self::Foliage => Some(FOLIAGE_TINT),
            Self::Water => Some(WATER_TINT),
        }
    }
}

/// How one engine layer finds its pack pixels.
struct Recipe {
    /// Engine texture name (`boxel:grass_side`).
    layer: &'static str,
    /// Minecraft block whose blockstate and model are read (`grass_block`).
    block: &'static str,
    /// Preferred variant key of the blockstate (`snowy=false`); then `""`, then the first one.
    variant: &'static str,
    /// Model face giving the texture (`North` stands for every side face).
    face: FaceName,
    /// Tint of model faces with a `tintindex`.
    tint: Tint,
    /// Texture parts (`block/...`, bottom first) when the pack has no usable model.
    fallback: &'static [(&'static str, Tint)],
}

const fn recipe(
    layer: &'static str,
    block: &'static str,
    face: FaceName,
    fallback: &'static [(&'static str, Tint)],
) -> Recipe {
    Recipe {
        layer,
        block,
        variant: "",
        face,
        tint: Tint::None,
        fallback,
    }
}

/// The built-in layers of `BlockRegistry::default_blocks`. Other layers try `block/<path>` of
/// their name.
const RECIPES: &[Recipe] = &[
    recipe(
        "boxel:stone",
        "stone",
        FaceName::North,
        &[("block/stone", Tint::None)],
    ),
    recipe(
        "boxel:dirt",
        "dirt",
        FaceName::North,
        &[("block/dirt", Tint::None)],
    ),
    Recipe {
        variant: "snowy=false",
        tint: Tint::Grass,
        ..recipe(
            "boxel:grass_top",
            "grass_block",
            FaceName::Up,
            &[("block/grass_block_top", Tint::Grass)],
        )
    },
    Recipe {
        variant: "snowy=false",
        tint: Tint::Grass,
        ..recipe(
            "boxel:grass_side",
            "grass_block",
            FaceName::North,
            &[
                ("block/grass_block_side", Tint::None),
                ("block/grass_block_side_overlay", Tint::Grass),
            ],
        )
    },
    recipe(
        "boxel:sand",
        "sand",
        FaceName::North,
        &[("block/sand", Tint::None)],
    ),
    recipe(
        "boxel:gravel",
        "gravel",
        FaceName::North,
        &[("block/gravel", Tint::None)],
    ),
    recipe(
        "boxel:snow",
        "snow_block",
        FaceName::North,
        &[("block/snow", Tint::None)],
    ),
    recipe(
        "boxel:bedrock",
        "bedrock",
        FaceName::North,
        &[("block/bedrock", Tint::None)],
    ),
    Recipe {
        variant: "axis=y",
        ..recipe(
            "boxel:log_side",
            "oak_log",
            FaceName::North,
            &[("block/oak_log", Tint::None)],
        )
    },
    Recipe {
        variant: "axis=y",
        ..recipe(
            "boxel:log_top",
            "oak_log",
            FaceName::Up,
            &[("block/oak_log_top", Tint::None)],
        )
    },
    Recipe {
        tint: Tint::Foliage,
        ..recipe(
            "boxel:leaves",
            "oak_leaves",
            FaceName::North,
            &[("block/oak_leaves", Tint::Foliage)],
        )
    },
    Recipe {
        tint: Tint::Water,
        ..recipe(
            "boxel:water",
            "water",
            FaceName::Up,
            &[("block/water_still", Tint::Water)],
        )
    },
    recipe(
        "boxel:planks",
        "oak_planks",
        FaceName::North,
        &[("block/oak_planks", Tint::None)],
    ),
    recipe(
        "boxel:lamp_orange",
        "glowstone",
        FaceName::North,
        &[("block/glowstone", Tint::None)],
    ),
    recipe(
        "boxel:lamp_red",
        "shroomlight",
        FaceName::North,
        &[("block/shroomlight", Tint::None)],
    ),
    recipe(
        "boxel:lamp_blue",
        "sea_lantern",
        FaceName::North,
        &[("block/sea_lantern", Tint::None)],
    ),
    Recipe {
        variant: "lit=true",
        ..recipe(
            "boxel:lamp_white",
            "redstone_lamp",
            FaceName::North,
            &[("block/redstone_lamp_on", Tint::None)],
        )
    },
    recipe(
        "boxel:lava",
        "lava",
        FaceName::Up,
        &[("block/lava_still", Tint::None)],
    ),
    // ADR 0018: glass (translucent, its alpha kept), and the cross plants (cutout). Slabs use
    // the stone layer and the turned logs the log layers.
    recipe(
        "boxel:glass",
        "glass",
        FaceName::North,
        &[("block/glass", Tint::None)],
    ),
    Recipe {
        tint: Tint::Grass,
        ..recipe(
            "boxel:short_grass",
            "short_grass",
            FaceName::North,
            &[("block/short_grass", Tint::Grass)],
        )
    },
    recipe(
        "boxel:poppy",
        "poppy",
        FaceName::North,
        &[("block/poppy", Tint::None)],
    ),
];

/// One texture of a layer, composited bottom to top.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Part {
    id: ResourceId,
    tint: Tint,
}

/// The array of `registry`'s layers at the pack's most common texture size; returns it and how
/// many layers came from the pack.
fn pack_block_textures(
    registry: &BlockRegistry,
    pack: &ResourcePack,
    warnings: &mut Vec<String>,
) -> (BlockTextureData, u32) {
    let size = pack_texture_size(pack, warnings);
    let specs = registry.textures();
    let shapes = layer_shapes(registry);
    let mut data = Vec::with_capacity(layer_bytes(size) * specs.len().max(1));
    let mut from_pack = 0;
    for (index, spec) in specs.iter().enumerate() {
        let shape = shapes.get(index).copied().flatten();
        match pack_layer(pack, spec, size, warnings) {
            Some(mut texels) => {
                let cutout = finish_alpha(&mut texels, shape);
                push_layer(&mut data, &texels, size, cutout);
                from_pack += 1;
            }
            None => {
                let texels = scale_nearest(
                    BLOCK_TEXTURE_SIZE,
                    BLOCK_TEXTURE_SIZE,
                    &synthesize(spec),
                    size,
                );
                push_layer(&mut data, &texels, size, is_cutout_pattern(spec));
            }
        }
    }
    if specs.is_empty() {
        // The single magenta layer of the procedural array.
        let magenta = vec![[255, 0, 255, 255]; (size * size) as usize];
        push_layer(&mut data, &magenta, size, false);
    }
    let layers = specs.len().max(1) as u32;
    let data = BlockTextureData {
        size,
        layers,
        mip_levels: mip_levels(size),
        data,
    };
    (data, from_pack)
}

/// The most common frame width of the pack's block textures (ties: the smaller one), rounded up
/// to a power of two and capped at [`MAX_PACK_TEXTURE_SIZE`]; 16 for a pack without textures.
fn pack_texture_size(pack: &ResourcePack, warnings: &mut Vec<String>) -> u32 {
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for (_, texture) in pack.block_textures() {
        let width = frame_size(texture).0;
        if width > 0 {
            *counts.entry(width).or_default() += 1;
        }
    }
    let common = counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(&a.0)))
        .map_or(BLOCK_TEXTURE_SIZE, |(width, _)| width);
    let size = common.next_power_of_two().min(MAX_PACK_TEXTURE_SIZE);
    if size != common {
        warnings.push(format!(
            "most common texture width {common} is not a power of two up to \
             {MAX_PACK_TEXTURE_SIZE}; layers use {size}"
        ));
    }
    size
}

/// Shape of the first block drawing each layer (`None`: no block uses it).
fn layer_shapes(registry: &BlockRegistry) -> Vec<Option<RenderShape>> {
    let mut shapes = vec![None; registry.textures().len()];
    for state in 0..registry.len() {
        let state = BlockState(state as u16);
        let shape = registry.shape(state);
        if shape == RenderShape::None {
            continue;
        }
        for face in [Face::PosY, Face::NegY, Face::PosX] {
            let layer = registry.face_texture(state, face).0 as usize;
            if let Some(slot) = shapes.get_mut(layer)
                && slot.is_none()
            {
                *slot = Some(shape);
            }
        }
    }
    shapes
}

/// Opaque layers get alpha 255 (Minecraft's solid layer ignores it), cutout layers a binary alpha
/// (threshold 0.5, kept in every mip); returns whether the layer is cutout.
fn finish_alpha(texels: &mut [[u8; 4]], shape: Option<RenderShape>) -> bool {
    match shape {
        Some(RenderShape::Opaque) => {
            for t in texels.iter_mut() {
                t[3] = 255;
            }
            false
        }
        Some(RenderShape::Cutout) => {
            for t in texels.iter_mut() {
                t[3] = if t[3] >= 128 { 255 } else { 0 };
            }
            true
        }
        _ => false,
    }
}

/// Level 0 of `spec`'s layer from the pack (`size`^2 texels), or `None` (with a warning) when
/// the pack lacks its textures.
fn pack_layer(
    pack: &ResourcePack,
    spec: &TextureSpec,
    size: u32,
    warnings: &mut Vec<String>,
) -> Option<Vec<[u8; 4]>> {
    let Some(recipe) = RECIPES.iter().find(|r| r.layer == spec.name) else {
        // Not built in: try the texture named like the layer (`game:copper_ore` ->
        // `minecraft:block/copper_ore`).
        let path = spec
            .name
            .split_once(':')
            .map_or(spec.name.as_str(), |(_, p)| p);
        let parts = [Part {
            id: ResourceId::new(ResourceId::DEFAULT_NAMESPACE, format!("block/{path}")),
            tint: Tint::None,
        }];
        return match compose(pack, &parts, size, warnings) {
            Ok(texels) => Some(texels),
            Err(missing) => {
                warnings.push(format!("{}: no texture {missing}; procedural", spec.name));
                None
            }
        };
    };
    if let Some(parts) = model_parts(pack, recipe) {
        match compose(pack, &parts, size, warnings) {
            Ok(texels) => return Some(texels),
            Err(missing) => warnings.push(format!(
                "{}: model of {} names missing texture {missing}; trying default names",
                spec.name, recipe.block
            )),
        }
    }
    let parts: Vec<Part> = recipe
        .fallback
        .iter()
        .map(|&(path, tint)| Part {
            id: ResourceId::new(ResourceId::DEFAULT_NAMESPACE, path),
            tint,
        })
        .collect();
    match compose(pack, &parts, size, warnings) {
        Ok(texels) => Some(texels),
        Err(missing) => {
            warnings.push(format!("{}: no texture {missing}; procedural", spec.name));
            None
        }
    }
}

/// The textures of `recipe.face` in the pack's model of `recipe.block` (first model of the
/// preferred variant, every element that has the face, in order). `None` when the pack ships no
/// blockstate or model for it (vanilla models are not built in), or the face is missing.
/// Variant rotations and face UVs are ignored: built-in blocks are full cubes.
fn model_parts(pack: &ResourcePack, recipe: &Recipe) -> Option<Vec<Part>> {
    let state = pack.blockstate(&ResourceId::new(
        ResourceId::DEFAULT_NAMESPACE,
        recipe.block,
    ))?;
    let model = match state {
        PackState::Variants(variants) => variants
            .get(recipe.variant)
            .or_else(|| variants.get(""))
            .or_else(|| variants.values().next())?
            .0
            .first()?
            .model
            .clone(),
        PackState::Multipart(cases) => cases.first()?.apply.0.first()?.model.clone(),
    };
    let model = pack.model(&model)?;
    let parts: Vec<Part> = model
        .elements
        .iter()
        .filter_map(|element| element.faces.get(&recipe.face))
        .map(|face| Part {
            id: ResourceId::parse(&face.texture),
            tint: if face.tintindex.is_some() {
                recipe.tint
            } else {
                Tint::None
            },
        })
        .collect();
    (!parts.is_empty()).then_some(parts)
}

/// Composites `parts` bottom to top at `size`^2 (first frame, nearest scaling, tint baked in,
/// "over" blending in sRGB like Minecraft's overlay quads). The first part is required (`Err`
/// names it when missing); a missing later part is a warning.
fn compose(
    pack: &ResourcePack,
    parts: &[Part],
    size: u32,
    warnings: &mut Vec<String>,
) -> Result<Vec<[u8; 4]>, String> {
    let mut out: Option<Vec<[u8; 4]>> = None;
    for part in parts {
        let Some(texture) = pack.texture(&part.id) else {
            if out.is_none() {
                return Err(part.id.to_string());
            }
            warnings.push(format!("overlay {} missing; drawn without it", part.id));
            continue;
        };
        let (width, height, frame) = first_frame(texture);
        let mut texels = scale_nearest(width, height, &frame, size);
        if let Some(tint) = part.tint.rgb() {
            for t in &mut texels {
                for c in 0..3 {
                    t[c] = ((u32::from(t[c]) * u32::from(tint[c]) + 127) / 255) as u8;
                }
            }
        }
        out = Some(match out {
            None => texels,
            Some(mut base) => {
                for (b, t) in base.iter_mut().zip(&texels) {
                    *b = over(*t, *b);
                }
                base
            }
        });
    }
    out.ok_or_else(|| "(no texture parts)".to_string())
}

/// `top` over `base` (straight alpha, sRGB bytes).
fn over(top: [u8; 4], base: [u8; 4]) -> [u8; 4] {
    let top_a = u32::from(top[3]);
    let base_a = u32::from(base[3]) * (255 - top_a) / 255;
    let alpha = top_a + base_a;
    if alpha == 0 {
        return [0; 4];
    }
    let mut out = [0, 0, 0, alpha as u8];
    for c in 0..3 {
        out[c] =
            ((u32::from(top[c]) * top_a + u32::from(base[c]) * base_a + alpha / 2) / alpha) as u8;
    }
    out
}

/// Frame edge of a texture: the animation's frame size, else a square of its width when it is a
/// vertical strip, else its size.
fn frame_size(texture: &Texture) -> (u32, u32) {
    match &texture.animation {
        Some(anim) => (
            anim.width.unwrap_or(texture.width),
            anim.height.unwrap_or(texture.width),
        ),
        None if texture.height > texture.width => (texture.width, texture.width),
        None => (texture.width, texture.height),
    }
}

/// First displayed frame of a texture (RGBA texels). Animation is a follow-up: animated
/// textures (water, lava, sea lantern) show this frame only.
fn first_frame(texture: &Texture) -> (u32, u32, Vec<[u8; 4]>) {
    let (width, height) = frame_size(texture);
    let index = texture
        .animation
        .as_ref()
        .and_then(|anim| anim.frames.first())
        .map_or(0, |frame| frame.index as usize);
    let bytes = match texture.frames.get(index).or_else(|| texture.frames.first()) {
        Some(frame) => frame.as_slice(),
        // No split frames: the top `width` x `height` of the image (the strip's first frame).
        None => {
            let len = (texture.width * height.min(texture.height) * 4) as usize;
            &texture.rgba[..len.min(texture.rgba.len())]
        }
    };
    let width = if texture.frames.is_empty() {
        texture.width
    } else {
        width
    };
    let height = (bytes.len() / 4) as u32 / width.max(1);
    (width, height, bytes.as_chunks::<4>().0.to_vec())
}

/// Nearest-neighbor scaling of a `width` x `height` image to `size`^2.
fn scale_nearest(width: u32, height: u32, src: &[[u8; 4]], size: u32) -> Vec<[u8; 4]> {
    if width == 0 || height == 0 || src.len() < (width * height) as usize {
        return vec![[255, 0, 255, 255]; (size * size) as usize];
    }
    if width == size && height == size {
        return src[..(size * size) as usize].to_vec();
    }
    (0..size * size)
        .map(|i| {
            let (x, y) = (i % size, i / size);
            let (sx, sy) = (x * width / size, y * height / size);
            src[(sy * width + sx) as usize]
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use boxel_gfx::RgbaImage;

    /// A temporary resource pack of generated PNG files (no third-party content), removed on drop.
    pub(crate) struct TempPack(pub(crate) PathBuf);

    impl TempPack {
        pub(crate) fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "boxel_packtex_{}_{name}_{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("temp pack dir");
            std::fs::write(
                dir.join("pack.mcmeta"),
                r#"{"pack":{"pack_format":34,"description":"boxel test pack"}}"#,
            )
            .expect("pack.mcmeta");
            Self(dir)
        }

        /// Writes `assets/minecraft/<rel>` (parents created).
        pub(crate) fn file(&self, rel: &str, contents: &str) {
            let path = self.0.join("assets/minecraft").join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
            std::fs::write(path, contents).expect("write");
        }

        /// Writes `textures/block/<name>.png`, `width` x `height`, every texel `pixel(x, y)`.
        pub(crate) fn png(
            &self,
            name: &str,
            width: u32,
            height: u32,
            pixel: impl Fn(u32, u32) -> [u8; 4],
        ) {
            let path = self
                .0
                .join("assets/minecraft/textures/block")
                .join(format!("{name}.png"));
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
            let data = (0..width * height)
                .flat_map(|i| pixel(i % width, i / width))
                .collect();
            RgbaImage {
                width,
                height,
                data,
            }
            .save_png(&path)
            .expect("png");
        }
    }

    impl Drop for TempPack {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn layer_of(registry: &BlockRegistry, name: &str) -> usize {
        registry
            .textures()
            .iter()
            .position(|t| t.name == name)
            .expect("layer")
    }

    /// Level 0 texel `(x, y)` of `layer`.
    fn texel(data: &BlockTextureData, layer: usize, x: u32, y: u32) -> [u8; 4] {
        let offset = layer * data.layer_bytes() + ((y * data.size + x) * 4) as usize;
        data.data[offset..offset + 4].try_into().expect("texel")
    }

    #[test]
    fn procedural_source_is_the_generated_array() {
        let registry = BlockRegistry::default_blocks();
        let (data, report) =
            load_block_textures(&registry, &TextureSource::Procedural).expect("procedural");
        assert_eq!(data, generate_block_textures(registry.textures()));
        assert_eq!(report.source, "procedural");
        assert_eq!((report.size, report.from_pack), (16, 0));
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn a_missing_pack_is_an_error() {
        let registry = BlockRegistry::default_blocks();
        let source = TextureSource::Pack(PathBuf::from("/nonexistent/boxel/pack"));
        assert!(load_block_textures(&registry, &source).is_err());
        assert_eq!(source.label(), "pack");
    }

    #[test]
    fn pack_textures_replace_layers_with_tints_overlays_and_fallbacks() {
        let pack = TempPack::new("replace");
        pack.png("stone", 16, 16, |x, _| [10, 20, x as u8 * 10, 255]);
        pack.png("grass_block_top", 16, 16, |_, _| [255, 255, 255, 255]);
        pack.png("grass_block_side", 16, 16, |_, _| [100, 50, 0, 255]);
        // overlay: top row opaque gray, the rest transparent
        pack.png("grass_block_side_overlay", 16, 16, |_, y| {
            if y == 0 {
                [255, 255, 255, 255]
            } else {
                [0, 0, 0, 0]
            }
        });
        // leaves: a hole on even x, half alpha elsewhere becomes opaque
        pack.png("oak_leaves", 16, 16, |x, _| {
            if x % 2 == 0 {
                [0, 0, 0, 0]
            } else {
                [255, 255, 255, 200]
            }
        });
        // water: a two-frame strip, frame 0 white, frame 1 black
        pack.png("water_still", 16, 32, |_, y| {
            if y < 16 {
                [255, 255, 255, 180]
            } else {
                [0, 0, 0, 255]
            }
        });
        std::fs::write(
            pack.0
                .join("assets/minecraft/textures/block/water_still.png.mcmeta"),
            r#"{"animation":{"frametime":2}}"#,
        )
        .expect("mcmeta");
        // glowstone (the orange lamp): 8x8, not the common size
        pack.png("glowstone", 8, 8, |x, y| {
            [x as u8 * 30, y as u8 * 30, 7, 255]
        });

        let registry = BlockRegistry::default_blocks();
        let (data, report) =
            load_block_textures(&registry, &TextureSource::Pack(pack.0.clone())).expect("pack");
        assert_eq!((data.size, data.mip_levels), (16, 5));
        assert_eq!(data.layers as usize, registry.textures().len());
        assert_eq!(data.data.len(), data.layers as usize * data.layer_bytes());
        assert_eq!(report.from_pack, 6);

        let stone = layer_of(&registry, "boxel:stone");
        assert_eq!(texel(&data, stone, 3, 5), [10, 20, 30, 255]);
        // grass top: white tinted to plains grass
        let top = layer_of(&registry, "boxel:grass_top");
        let [r, g, b] = GRASS_TINT;
        assert_eq!(texel(&data, top, 0, 0), [r, g, b, 255]);
        // grass side: tinted overlay on the top row only
        let side = layer_of(&registry, "boxel:grass_side");
        assert_eq!(texel(&data, side, 4, 0), [r, g, b, 255]);
        assert_eq!(texel(&data, side, 4, 1), [100, 50, 0, 255]);
        // leaves: binary alpha, foliage tint
        let leaves = layer_of(&registry, "boxel:leaves");
        assert_eq!(texel(&data, leaves, 0, 0)[3], 0);
        let [r, g, b] = FOLIAGE_TINT;
        assert_eq!(texel(&data, leaves, 1, 0), [r, g, b, 255]);
        // water: first frame, tinted, translucent alpha kept
        let water = layer_of(&registry, "boxel:water");
        let [r, g, b] = WATER_TINT;
        assert_eq!(texel(&data, water, 0, 15), [r, g, b, 180]);
        // glowstone (orange lamp) 8x8 scaled to 16 by nearest
        let lamp = layer_of(&registry, "boxel:lamp_orange");
        assert_eq!(texel(&data, lamp, 5, 3), [60, 30, 7, 255]);
        // missing textures keep the procedural pixels and warn
        let dirt = layer_of(&registry, "boxel:dirt");
        let procedural = generate_block_textures(registry.textures());
        assert_eq!(texel(&data, dirt, 2, 2), texel(&procedural, dirt, 2, 2));
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("boxel:dirt") && w.contains("minecraft:block/dirt")),
            "{:?}",
            report.warnings
        );
    }

    #[test]
    fn models_name_the_textures_and_32px_packs_scale_the_rest() {
        let pack = TempPack::new("models");
        pack.file(
            "blockstates/oak_log.json",
            r#"{"variants":{"axis=x":{"model":"block/oak_log_x"},"axis=y":{"model":"block/oak_log"}}}"#,
        );
        pack.file(
            "models/block/oak_log.json",
            r#"{"parent":"block/cube_column","textures":{"end":"block/my_end","side":"block/my_bark"}}"#,
        );
        pack.png("my_end", 32, 32, |_, _| [1, 2, 3, 255]);
        pack.png("my_bark", 32, 32, |x, _| [x as u8, 0, 0, 255]);
        pack.png("stone", 32, 32, |_, _| [9, 9, 9, 255]);

        let registry = BlockRegistry::default_blocks();
        let (data, report) =
            load_block_textures(&registry, &TextureSource::Pack(pack.0.clone())).expect("pack");
        assert_eq!((data.size, data.mip_levels), (32, 6));
        assert_eq!(report.from_pack, 3);
        let top = layer_of(&registry, "boxel:log_top");
        let side = layer_of(&registry, "boxel:log_side");
        assert_eq!(texel(&data, top, 7, 7), [1, 2, 3, 255]);
        assert_eq!(texel(&data, side, 31, 0), [31, 0, 0, 255]);
        // a procedural layer is upscaled 2x by nearest
        let dirt = layer_of(&registry, "boxel:dirt");
        let procedural = generate_block_textures(registry.textures());
        assert_eq!(texel(&data, dirt, 7, 9), texel(&procedural, dirt, 3, 4));
    }

    #[test]
    fn over_and_scaling() {
        assert_eq!(over([0, 0, 0, 0], [5, 6, 7, 255]), [5, 6, 7, 255]);
        assert_eq!(over([200, 0, 0, 255], [5, 6, 7, 255]), [200, 0, 0, 255]);
        assert_eq!(over([255, 0, 0, 128], [0, 0, 255, 255]), [128, 0, 127, 255]);
        let src: Vec<[u8; 4]> = (0..4).map(|i| [i as u8, 0, 0, 255]).collect();
        let up = scale_nearest(2, 2, &src, 4);
        assert_eq!(up[0][0], 0);
        assert_eq!(up[1][0], 0);
        assert_eq!(up[2][0], 1);
        assert_eq!(up[15][0], 3);
        let down = scale_nearest(2, 2, &src, 1);
        assert_eq!(down, vec![[0, 0, 0, 255]]);
    }
}
