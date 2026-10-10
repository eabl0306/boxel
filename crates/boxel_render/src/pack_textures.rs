//! Block textures from a Minecraft Java resource pack (`boxel_pack`). The pack only replaces the
//! pixels of the registry's texture layers: layer ids, their order, the mip chain and the upload
//! path stay the ones of the procedural textures, so every tier samples it the same way.
//!
//! The game passes a [`LayerRecipe`] per layer with the pack ([`PackSource::recipes`]): the
//! Minecraft block whose blockstate and model name the texture (`cube_column` end/side, the grass
//! overlay...), fallback texture names for packs that ship no model, and the biome tint baked
//! into the grayscale textures Minecraft tints (grass, foliage, water). The blockstate variant is
//! picked by the property values of one of the game's states (ADR 0019): `game:log[axis=y]` reads
//! the pack's `axis=y` variant ([`pack_variant`]). A layer without a recipe reads the texture
//! named like it (`game:copper_ore` -> `minecraft:block/copper_ore`). Layers the pack cannot fill
//! keep their procedural pixels and add a warning to the [`TextureReport`], as do recipes the
//! registry cannot use.
//!
//! A layer whose texture is animated (`.png.mcmeta`, water, lava, the sea lantern...) also keeps
//! the finished layer of every frame its animation shows ([`LayerAnimation`], ADR 0028); the
//! array holds the frame of tick 0.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use std::sync::Arc;

use boxel_pack::{
    BlockState as PackState, LayerRecipe, ResourceId, ResourcePack, Texture, Timeline, VariantList,
    condition_matches, parse_variant_key,
};
use boxel_voxel::{
    BlockModel, BlockRegistry, BlockState, FLUID_LEVEL, Face, RenderShape, TextureSpec,
};

use crate::texture_animation::LayerAnimation;
use crate::textures::{
    BLOCK_TEXTURE_SIZE, BlockTextureData, generate_block_textures, is_cutout_pattern, layer_bytes,
    mip_levels, push_layer, synthesize,
};

/// Largest layer edge taken from a pack (bigger packs are scaled down to it).
pub const MAX_PACK_TEXTURE_SIZE: u32 = 512;
/// Most bytes the frames of animated layers may take ([`LayerAnimation::memory`]); a layer past
/// it shows its first frame only, with a warning. Every animated layer of a 16 px pack takes a
/// few hundred KB; a 512 px layer with 32 frames, 45 MB.
pub const MAX_ANIMATION_BYTES: usize = 64 << 20;

/// Where the pixels of the block texture layers come from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TextureSource {
    /// Synthesized from each [`TextureSpec`] (no files; the default).
    #[default]
    Procedural,
    /// A Minecraft Java resource pack, read at startup.
    Pack(PackSource),
}

impl TextureSource {
    /// `procedural`, or the pack directory's last component.
    pub fn label(&self) -> String {
        match self {
            Self::Procedural => "procedural".to_string(),
            Self::Pack(pack) => pack.dir.file_name().map_or_else(
                || pack.dir.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            ),
        }
    }
}

/// A Minecraft Java resource pack and how the game's texture layers read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackSource {
    /// The pack directory (the one holding `pack.mcmeta`).
    pub dir: PathBuf,
    /// The recipe of each layer that has one (the first one naming a layer counts); a layer
    /// without one reads the texture named like it ([`LayerRecipe`]).
    pub recipes: Arc<[LayerRecipe]>,
}

impl PackSource {
    /// The pack at `dir` with no recipes: every layer reads the texture named like it.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            recipes: Arc::from([]),
        }
    }

    /// The same pack with `recipes` (replacing any earlier ones).
    pub fn with_recipes(mut self, recipes: impl Into<Arc<[LayerRecipe]>>) -> Self {
        self.recipes = recipes.into();
        self
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
    /// Animated layers among them (ADR 0028).
    pub animated: u32,
    /// Bytes the frames of the animated layers take.
    pub animation_bytes: usize,
    /// Wall time of reading the pack and building the array (CPU only, no upload).
    pub load_time: Duration,
    /// Non-fatal warnings of the pack reader itself (skipped assets, odd sizes...).
    pub pack_warnings: usize,
    /// Mapping problems: layers that kept their procedural pixels, missing overlays, recipes the
    /// registry cannot use, a pack that failed to open.
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
        TextureSource::Pack(source) => {
            let pack = open_pack(&source.dir)?;
            let mut warnings = Vec::new();
            let (data, from_pack) =
                pack_block_textures(registry, &pack, &source.recipes, &mut warnings);
            (data, from_pack, pack.warnings().len(), warnings)
        }
    };
    let report = TextureReport {
        source: source.label(),
        size: data.size,
        layers: data.layers,
        from_pack,
        animated: data.animations.len() as u32,
        animation_bytes: data.animations.iter().map(LayerAnimation::memory).sum(),
        load_time: started.elapsed(),
        pack_warnings,
        warnings,
    };
    Ok((data, report))
}

fn open_pack(path: &Path) -> Result<ResourcePack, String> {
    ResourcePack::open(path).map_err(|e| format!("resource pack {}: {e}", path.display()))
}

/// One texture of a layer, composited bottom to top, with the tint baked into it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Part {
    id: ResourceId,
    tint: Option<[u8; 3]>,
}

/// A layer the pack fills: its parts and level 0 at tick 0.
struct PackLayer {
    parts: Vec<Part>,
    quarter: bool,
    texels: Vec<[u8; 4]>,
}

/// The array of `registry`'s layers at the pack's most common texture size, each read through its
/// recipe in `recipes`; returns it and how many layers came from the pack.
fn pack_block_textures(
    registry: &BlockRegistry,
    pack: &ResourcePack,
    recipes: &[LayerRecipe],
    warnings: &mut Vec<String>,
) -> (BlockTextureData, u32) {
    let size = pack_texture_size(pack, warnings);
    check_recipes(registry, recipes, warnings);
    let specs = registry.textures();
    let shapes = layer_shapes(registry);
    let mut data = Vec::with_capacity(layer_bytes(size) * specs.len().max(1));
    let mut from_pack = 0;
    let mut animations = Vec::new();
    let mut animation_bytes = 0;
    for (index, spec) in specs.iter().enumerate() {
        let shape = shapes.get(index).copied().flatten();
        match pack_layer(registry, pack, recipes, spec, size, warnings) {
            Some(PackLayer {
                parts,
                quarter,
                mut texels,
            }) => {
                let cutout = finish_alpha(&mut texels, shape);
                push_layer(&mut data, &texels, size, cutout);
                from_pack += 1;
                if let Some((part, timeline)) = animated_part(pack, &parts, &spec.name, warnings) {
                    let bytes = timeline.indices().len() * layer_bytes(size);
                    if animation_bytes + bytes > MAX_ANIMATION_BYTES {
                        warnings.push(format!(
                            "{}: animation frames ({bytes} bytes) past the \
                             {MAX_ANIMATION_BYTES} byte cap; first frame only",
                            spec.name
                        ));
                        continue;
                    }
                    animation_bytes += bytes;
                    let frames = timeline
                        .indices()
                        .into_iter()
                        .map(|frame| {
                            let pick = |i: usize, texture: &Texture| {
                                if i == part {
                                    frame
                                } else {
                                    start_index(texture)
                                }
                            };
                            let mut texels =
                                compose(pack, &parts, size, quarter, &mut Vec::new(), &pick)
                                    .expect("the layer composed at tick 0");
                            finish_alpha(&mut texels, shape);
                            let mut bytes = Vec::with_capacity(layer_bytes(size));
                            push_layer(&mut bytes, &texels, size, cutout);
                            (frame, bytes)
                        })
                        .collect();
                    animations.push(LayerAnimation::new(index as u32, timeline, frames));
                }
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
        animated_flow: animated_flow(registry, &animations),
        data,
        animations: Arc::from(animations),
    };
    (data, from_pack)
}

/// Warns about the recipes `registry` cannot use (they are the game's input): one for a layer it
/// lacks, a second one for a layer (the first one counts), and a state that does not parse (the
/// blockstate variant is then picked without its values).
fn check_recipes(registry: &BlockRegistry, recipes: &[LayerRecipe], warnings: &mut Vec<String>) {
    let layers: HashSet<&str> = registry
        .textures()
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    let mut seen = HashSet::new();
    for recipe in recipes {
        let layer = recipe.layer.as_str();
        if !layers.contains(layer) {
            warnings.push(format!("{layer}: recipe for no texture layer; ignored"));
        } else if !seen.insert(layer) {
            warnings.push(format!("{layer}: second recipe for the layer; ignored"));
        } else if !recipe.state.is_empty()
            && let Err(e) = registry.parse_state(&recipe.state)
        {
            warnings.push(format!("{layer}: recipe state {}: {e}", recipe.state));
        }
    }
}

/// Which flow layers are animated ([`BlockTextureData::animated_flow`]): slot 0 for fluids that
/// emit no light, slot 1 for those that do. That is the key the shaders use, as they pick a fluid
/// quad's flow speed and still bit by its emissive flag. A fluid's flow layer is its side
/// texture (the fluid mesher draws its sides and sloping tops with it). Every level of a fluid
/// is drawn as its source (level 0, ADR 0021), so the source's emission and side texture count,
/// whatever its levels say.
fn animated_flow(registry: &BlockRegistry, animations: &[LayerAnimation]) -> [bool; 2] {
    let mut flow = [false; 2];
    for state in 0..registry.len() {
        let state = BlockState(state as u16);
        if registry.model(state) != BlockModel::Fluid {
            continue;
        }
        // The state the mesher draws (`MaterialTable::from_registry`).
        let source = registry.with(state, FLUID_LEVEL, "0").unwrap_or(state);
        let layer = u32::from(registry.face_texture(source, Face::PosX).0);
        let slot = usize::from(!registry.emission(source).is_dark());
        flow[slot] |= animations.iter().any(|a| a.layer == layer);
    }
    flow
}

/// The part of `parts` that animates the layer `name` and its timeline: the first part whose
/// pack texture has a valid animation (warnings name dropped frames). Layers composite one
/// animation: a later animated part shows its tick 0 frame, with a warning.
fn animated_part(
    pack: &ResourcePack,
    parts: &[Part],
    name: &str,
    warnings: &mut Vec<String>,
) -> Option<(usize, Timeline)> {
    let mut found: Option<(usize, Timeline)> = None;
    for (i, part) in parts.iter().enumerate() {
        let Some(texture) = pack.texture(&part.id) else {
            continue;
        };
        let mut problems = Vec::new();
        let timeline = timeline(texture, &mut problems);
        warnings.extend(
            problems
                .into_iter()
                .map(|p| format!("{name}: {}: {p}", part.id)),
        );
        if let Some(timeline) = timeline {
            if found.is_some() {
                warnings.push(format!(
                    "{name}: {} is animated too; only the first animated part moves",
                    part.id
                ));
            } else {
                found = Some((i, timeline));
            }
        }
    }
    found
}

/// The animation of `texture` resolved against its strip (`None`: static).
fn timeline(texture: &Texture, warnings: &mut Vec<String>) -> Option<Timeline> {
    let anim = texture.animation.as_ref()?;
    if texture.frames.is_empty() {
        return None;
    }
    anim.timeline(texture.frames.len() as u32, warnings)
}

/// The strip index `texture` shows at tick 0: its timeline's first frame, else 0 (a static
/// texture, or an animation Minecraft rejects, shows the top of the strip).
fn start_index(texture: &Texture) -> u32 {
    timeline(texture, &mut Vec::new()).map_or(0, |t| t.at(0).current)
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
    registry: &BlockRegistry,
    pack: &ResourcePack,
    recipes: &[LayerRecipe],
    spec: &TextureSpec,
    size: u32,
    warnings: &mut Vec<String>,
) -> Option<PackLayer> {
    let Some(recipe) = recipes.iter().find(|r| r.layer == spec.name) else {
        // No recipe: try the texture named like the layer (`game:copper_ore` ->
        // `minecraft:block/copper_ore`).
        let path = spec
            .name
            .split_once(':')
            .map_or(spec.name.as_str(), |(_, p)| p);
        let parts = vec![Part {
            id: ResourceId::new(ResourceId::DEFAULT_NAMESPACE, format!("block/{path}")),
            tint: None,
        }];
        return match compose(pack, &parts, size, false, warnings, &tick0) {
            Ok(texels) => Some(PackLayer {
                parts,
                quarter: false,
                texels,
            }),
            Err(missing) => {
                warnings.push(format!("{}: no texture {missing}; procedural", spec.name));
                None
            }
        };
    };
    if let Some(parts) = model_parts(pack, recipe, &recipe_values(registry, recipe)) {
        match compose(pack, &parts, size, recipe.quarter, warnings, &tick0) {
            Ok(texels) => {
                return Some(PackLayer {
                    parts,
                    quarter: recipe.quarter,
                    texels,
                });
            }
            Err(missing) => warnings.push(format!(
                "{}: model of {} names missing texture {missing}; trying default names",
                spec.name, recipe.block
            )),
        }
    }
    let parts: Vec<Part> = recipe
        .fallback
        .iter()
        .map(|(path, tint)| Part {
            id: ResourceId::new(ResourceId::DEFAULT_NAMESPACE, path.as_str()),
            tint: *tint,
        })
        .collect();
    match compose(pack, &parts, size, recipe.quarter, warnings, &tick0) {
        Ok(texels) => Some(PackLayer {
            parts,
            quarter: recipe.quarter,
            texels,
        }),
        Err(missing) => {
            warnings.push(format!("{}: no texture {missing}; procedural", spec.name));
            None
        }
    }
}

/// The property values `recipe` asks the blockstate for: those of its state (when `registry`
/// has it) and its extra `variant` ones.
fn recipe_values(registry: &BlockRegistry, recipe: &LayerRecipe) -> BTreeMap<String, String> {
    let mut values = parse_variant_key(&recipe.variant);
    if let Ok(state) = registry.parse_state(&recipe.state) {
        for (property, value) in registry.values(state).iter() {
            values.insert(property.to_string(), value.to_string());
        }
    }
    values
}

/// The models a Minecraft blockstate gives the state with property `values` (one of the game's,
/// by its property string: `game:stone_slab[type=top]` -> `type=top`). Variants: the key whose
/// conditions all hold and name the most properties (`""` holds always), else the first one.
/// Multipart: the first case whose condition holds. Properties the key names and `values` lack
/// do not hold.
pub(crate) fn pack_variant<'a>(
    state: &'a PackState,
    values: &BTreeMap<String, String>,
) -> Option<&'a VariantList> {
    match state {
        PackState::Variants(variants) => {
            let holds = |key: &str| {
                let conditions = parse_variant_key(key);
                conditions
                    .iter()
                    .all(|(k, v)| values.get(k) == Some(v))
                    .then_some(conditions.len())
            };
            let mut best: Option<(usize, &VariantList)> = None;
            for (key, list) in variants {
                if let Some(n) = holds(key)
                    && best.is_none_or(|(m, _)| n > m)
                {
                    best = Some((n, list));
                }
            }
            best.map(|(_, list)| list)
                .or_else(|| variants.values().next())
        }
        PackState::Multipart(cases) => cases
            .iter()
            .find(|case| {
                case.when
                    .as_ref()
                    .is_none_or(|when| condition_matches(when, values))
            })
            .map(|case| &case.apply),
    }
}

/// The textures of `recipe.face` in the pack's model of `recipe.block` (first model of the
/// variant [`pack_variant`] picks for `values`, every element that has the face, in order).
/// `None` when the pack ships no blockstate or model for it (vanilla models are not built in),
/// or the face is missing. Variant rotations and face UVs are ignored: a layer is one full face,
/// and a turned block turns its texture itself.
fn model_parts(
    pack: &ResourcePack,
    recipe: &LayerRecipe,
    values: &BTreeMap<String, String>,
) -> Option<Vec<Part>> {
    let state = pack.blockstate(&ResourceId::new(
        ResourceId::DEFAULT_NAMESPACE,
        recipe.block.as_str(),
    ))?;
    let model = pack_variant(state, values)?.0.first()?.model.clone();
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
                None
            },
        })
        .collect();
    (!parts.is_empty()).then_some(parts)
}

/// The strip frame of each part at tick 0 ([`compose`]'s `pick`).
fn tick0(_: usize, texture: &Texture) -> u32 {
    start_index(texture)
}

/// Composites `parts` bottom to top at `size`^2 (part `i` shows strip frame `pick(i, texture)`,
/// nearest scaling, tint baked in, "over" blending in sRGB like Minecraft's overlay quads). The
/// first part is required (`Err` names it when missing); a missing later part is a warning.
/// `quarter`: a frame of twice `size` gives its top-left quarter ([`LayerRecipe::quarter`]).
fn compose(
    pack: &ResourcePack,
    parts: &[Part],
    size: u32,
    quarter: bool,
    warnings: &mut Vec<String>,
    pick: &dyn Fn(usize, &Texture) -> u32,
) -> Result<Vec<[u8; 4]>, String> {
    let mut out: Option<Vec<[u8; 4]>> = None;
    for (i, part) in parts.iter().enumerate() {
        let Some(texture) = pack.texture(&part.id) else {
            if out.is_none() {
                return Err(part.id.to_string());
            }
            warnings.push(format!("overlay {} missing; drawn without it", part.id));
            continue;
        };
        let (mut width, mut height, mut frame) = frame_texels(texture, pick(i, texture));
        if quarter && width == 2 * size && height == 2 * size {
            frame = (0..size * size)
                .map(|i| frame[((i / size) * width + i % size) as usize])
                .collect();
            (width, height) = (size, size);
        }
        let mut texels = scale_nearest(width, height, &frame, size);
        if let Some(tint) = part.tint {
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

/// Frame edge of a texture: the animation's frame size (Minecraft's rule), else a square of its
/// width when it is a vertical strip, else its size.
fn frame_size(texture: &Texture) -> (u32, u32) {
    match &texture.animation {
        Some(anim) => anim.frame_size(texture.width, texture.height),
        None if texture.height > texture.width => (texture.width, texture.width),
        None => (texture.width, texture.height),
    }
}

/// Strip frame `index` of a texture (RGBA texels; the first frame when the strip was not split).
fn frame_texels(texture: &Texture, index: u32) -> (u32, u32, Vec<[u8; 4]>) {
    let (width, height) = frame_size(texture);
    let bytes = match texture
        .frames
        .get(index as usize)
        .or_else(|| texture.frames.first())
    {
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
    use boxel_demo::pack::{FOLIAGE_TINT, GRASS_TINT, WATER_TINT};
    use boxel_gfx::RgbaImage;

    /// The pack at `dir` read with the demo recipes.
    fn demo_pack(dir: impl Into<PathBuf>) -> TextureSource {
        TextureSource::Pack(PackSource::new(dir).with_recipes(boxel_demo::pack::recipes()))
    }

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
        let registry = boxel_demo::blocks::registry();
        let (data, report) =
            load_block_textures(&registry, &TextureSource::Procedural).expect("procedural");
        assert_eq!(data, generate_block_textures(registry.textures()));
        assert_eq!(report.source, "procedural");
        assert_eq!((report.size, report.from_pack), (16, 0));
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn a_missing_pack_is_an_error() {
        let registry = boxel_demo::blocks::registry();
        let source = demo_pack("/nonexistent/boxel/pack");
        assert!(load_block_textures(&registry, &source).is_err());
        assert_eq!(source.label(), "pack");
    }

    /// The recipes are the game's (#48): without any, every layer reads the texture named like
    /// it, untinted; a recipe the game passes is the one read.
    #[test]
    fn layers_read_the_recipes_the_game_passes() {
        let pack = TempPack::new("recipes");
        pack.png("stone", 16, 16, |_, _| [9, 9, 9, 255]);
        pack.png("grass_top", 16, 16, |_, _| [200, 200, 200, 255]);
        pack.png("gravel", 16, 16, |_, _| [255, 255, 255, 255]);
        let registry = boxel_demo::blocks::registry();
        let procedural = generate_block_textures(registry.textures());
        let (stone, grass, dirt, gravel) = (
            layer_of(&registry, "boxel:stone"),
            layer_of(&registry, "boxel:grass_top"),
            layer_of(&registry, "boxel:dirt"),
            layer_of(&registry, "boxel:gravel"),
        );

        let bare = TextureSource::Pack(PackSource::new(pack.0.clone()));
        let (data, report) = load_block_textures(&registry, &bare).expect("pack");
        assert_eq!(report.from_pack, 3, "{:?}", report.warnings);
        assert_eq!(texel(&data, stone, 1, 1), [9, 9, 9, 255]);
        assert_eq!(texel(&data, grass, 1, 1), [200, 200, 200, 255]);
        assert_eq!(texel(&data, gravel, 1, 1), [255, 255, 255, 255]);
        assert_eq!(texel(&data, dirt, 1, 1), texel(&procedural, dirt, 1, 1));
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w == "boxel:dirt: no texture minecraft:block/dirt; procedural"),
            "{:?}",
            report.warnings
        );

        // The dirt layer reads the gravel texture, tinted red; the other layers keep their names.
        let recipe = LayerRecipe {
            layer: "boxel:dirt".to_string(),
            block: "no_such_block".to_string(),
            state: String::new(),
            variant: String::new(),
            face: boxel_pack::FaceName::North,
            tint: None,
            fallback: vec![("block/gravel".to_string(), Some([255, 0, 0]))],
            quarter: false,
        };
        let source =
            TextureSource::Pack(PackSource::new(pack.0.clone()).with_recipes(vec![recipe]));
        let (data, report) = load_block_textures(&registry, &source).expect("pack");
        assert_eq!(report.from_pack, 4, "{:?}", report.warnings);
        assert_eq!(texel(&data, dirt, 1, 1), [255, 0, 0, 255]);
        assert_eq!(texel(&data, gravel, 1, 1), [255, 255, 255, 255]);
        assert_eq!(source.label(), bare.label());
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
        // flow sprites (ADR 0025): water's twice the block size, gray in its top-left quarter
        // only; lava's at the block size, taken whole
        pack.png("water_flow", 32, 32, |x, y| {
            if x < 16 && y < 16 {
                [255, 255, 255, 200]
            } else {
                [0, 0, 0, 255]
            }
        });
        pack.png("lava_flow", 16, 16, |x, _| [200, x as u8, 0, 255]);

        let registry = boxel_demo::blocks::registry();
        let (data, report) =
            load_block_textures(&registry, &demo_pack(pack.0.clone())).expect("pack");
        assert_eq!((data.size, data.mip_levels), (16, 5));
        assert_eq!(data.layers as usize, registry.textures().len());
        assert_eq!(data.data.len(), data.layers as usize * data.layer_bytes());
        assert_eq!(report.from_pack, 8);

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
        // the flow sprites: the quarter a face shows, tinted; lava's whole
        let flow = layer_of(&registry, "boxel:water_flow");
        for (x, y) in [(0, 0), (15, 15), (3, 12)] {
            assert_eq!(texel(&data, flow, x, y), [r, g, b, 200]);
        }
        let lava_flow = layer_of(&registry, "boxel:lava_flow");
        assert_eq!(texel(&data, lava_flow, 9, 4), [200, 9, 0, 255]);
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

    /// #48: the flow layers that do not scroll are found by fluid, not by name. A fluid's flow
    /// layer is its side texture; slot 0 holds the fluids that emit no light, slot 1 those that
    /// do (the shaders' key). Every level of a fluid is drawn as its source (ADR 0021), so only
    /// the source counts.
    #[test]
    fn animated_flow_follows_the_fluids_side_texture() {
        use boxel_voxel::{
            BlockCollision, BlockDef, BlockModel, BlockRegistryBuilder, FLUID_LEVEL, FaceTextures,
            Property, TexturePattern,
        };
        let mut b = BlockRegistryBuilder::new();
        let mut layer = |name: &str| {
            b.add_texture(TextureSpec {
                name: format!("game:{name}"),
                base_color: [90, 90, 90],
                noise: 4,
                pattern: TexturePattern::Noise,
            })
            .expect("layer")
        };
        const LAYERS: [&str; 8] = [
            "goo",
            "goo_side",
            "glow",
            "glow_side",
            "rock",
            "magma",
            "magma_side",
            "crust",
        ];
        let [
            goo,
            goo_side,
            glow,
            glow_side,
            rock,
            magma,
            magma_side,
            crust,
        ] = LAYERS.map(&mut layer);
        let fluid = |name: &str, still, side, emission| BlockDef {
            name: format!("game:{name}"),
            shape: RenderShape::Translucent,
            model: BlockModel::Fluid,
            collision: BlockCollision::None,
            emission,
            light_opacity: 1,
            textures: FaceTextures {
                top: still,
                bottom: still,
                side,
            },
        };
        b.add_block(fluid("goo", goo, goo_side, [0; 3]))
            .expect("goo");
        b.add_block(fluid("glow", glow, glow_side, [9, 9, 9]))
            .expect("glow");
        // A cube showing an animated layer on its side is no fluid.
        b.add_block(BlockDef {
            name: "game:rock".to_string(),
            shape: RenderShape::Opaque,
            model: BlockModel::CUBE,
            collision: BlockCollision::Full,
            emission: [0; 3],
            light_opacity: 15,
            textures: FaceTextures::all(rock),
        })
        .expect("rock");
        // An emissive fluid whose levels look otherwise: 8..=11 dark, 12..=15 dark with another
        // side texture. The mesher draws them all as the emissive source.
        b.add_block_with_states(
            fluid("magma", magma, magma_side, [12, 6, 0]),
            vec![Property::int(FLUID_LEVEL, 0..=15)],
            |values, def| {
                let level = values.index(FLUID_LEVEL).expect("level");
                if level >= 8 {
                    def.emission = [0; 3];
                }
                if level >= 12 {
                    def.textures.side = crust;
                }
            },
        )
        .expect("magma");
        let registry = b.build();

        let flow_of = |animated: &[&str]| {
            let pack = TempPack::new("flow_by_fluid");
            for name in LAYERS {
                pack.png(name, 16, 32, |_, y| [(y * 8) as u8, 0, 0, 255]);
                if animated.contains(&name) {
                    pack.file(
                        &format!("textures/block/{name}.png.mcmeta"),
                        r#"{"animation":{}}"#,
                    );
                }
            }
            let source = TextureSource::Pack(PackSource::new(pack.0.clone()));
            let (data, report) = load_block_textures(&registry, &source).expect("pack");
            assert_eq!(report.animated as usize, animated.len(), "{report:?}");
            data.animated_flow
        };
        assert_eq!(flow_of(&[]), [false, false]);
        assert_eq!(flow_of(&["goo_side"]), [true, false]);
        assert_eq!(flow_of(&["glow_side"]), [false, true]);
        assert_eq!(flow_of(&["goo_side", "glow_side"]), [true, true]);
        // Still layers and non-fluid layers do not count.
        assert_eq!(flow_of(&["goo", "glow", "rock", "magma"]), [false, false]);
        // The magma's dark levels and the side texture only they name are never drawn.
        assert_eq!(flow_of(&["magma_side"]), [false, true]);
        assert_eq!(flow_of(&["crust"]), [false, false]);
        assert_eq!(flow_of(&["goo_side", "magma_side"]), [true, true]);
    }

    /// #48: the recipes are the game's input, so the ones the registry cannot use warn: a layer
    /// it lacks, a second recipe for a layer (the first one counts) and a state that does not
    /// parse (the variant is then picked without its values).
    #[test]
    fn recipes_the_registry_cannot_use_warn() {
        let pack = TempPack::new("bad_recipes");
        pack.png("stone", 16, 16, |_, _| [9, 9, 9, 255]);
        let registry = boxel_demo::blocks::registry();
        let recipe = |layer: &str, state: &str| LayerRecipe {
            layer: layer.to_string(),
            block: "stone".to_string(),
            state: state.to_string(),
            variant: String::new(),
            face: boxel_pack::FaceName::North,
            tint: None,
            fallback: vec![("block/stone".to_string(), None)],
            quarter: false,
        };
        let source = TextureSource::Pack(PackSource::new(pack.0.clone()).with_recipes(vec![
            recipe("boxel:stone", ""),
            recipe("boxel:grass_top_typo", ""),
            recipe("boxel:stone", "boxel:no_such_block"),
            recipe("boxel:dirt", "boxel:logs[axis=y]"),
            recipe("boxel:sand", "boxel:log[axis=up]"),
        ]));
        let (data, report) = load_block_textures(&registry, &source).expect("pack");
        let recipe_warnings: Vec<&str> = report
            .warnings
            .iter()
            .map(String::as_str)
            .filter(|w| w.contains("recipe"))
            .collect();
        assert_eq!(
            recipe_warnings,
            [
                "boxel:grass_top_typo: recipe for no texture layer; ignored",
                "boxel:stone: second recipe for the layer; ignored",
                "boxel:dirt: recipe state boxel:logs[axis=y]: unknown block boxel:logs",
                "boxel:sand: recipe state boxel:log[axis=up]: property axis of boxel:log has no \
                 value up",
            ]
        );
        // The recipes still read their textures.
        for layer in ["boxel:stone", "boxel:dirt", "boxel:sand"] {
            assert_eq!(
                texel(&data, layer_of(&registry, layer), 1, 1),
                [9, 9, 9, 255]
            );
        }
        // The demo's recipes are all usable.
        let (_, report) = load_block_textures(&registry, &demo_pack(pack.0.clone())).expect("pack");
        assert!(
            report.warnings.iter().all(|w| !w.contains("recipe")),
            "{:?}",
            report.warnings
        );
    }

    /// ADR 0028: animated pack textures keep the finished layer of each frame they show (tint,
    /// flow quarter and alpha baked in, every mip), picked by Minecraft's rules.
    #[test]
    fn animated_textures_keep_every_frame_they_show() {
        let pack = TempPack::new("animated");
        let mcmeta = |name: &str, json: &str| {
            pack.file(&format!("textures/block/{name}.png.mcmeta"), json);
        };
        // water: 4 gray frames (0, 60, 120, 180), 2 ticks each, interpolated
        pack.png("water_still", 16, 64, |_, y| {
            let v = (y / 16) as u8 * 60;
            [v, v, v, 200]
        });
        mcmeta(
            "water_still",
            r#"{"animation":{"frametime":2,"interpolate":true}}"#,
        );
        // lava: an explicit list over a 3-frame strip, red channel = frame index * 100
        pack.png("lava_still", 16, 48, |_, y| {
            [(y / 16) as u8 * 100, 0, 0, 255]
        });
        mcmeta(
            "lava_still",
            r#"{"animation":{"frametime":3,"frames":[2,{"index":0,"time":1},2]}}"#,
        );
        // water flow: two 32x32 frames, a face shows their top-left quarter
        pack.png("water_flow", 32, 64, |x, y| {
            let v = if x < 16 && y % 32 < 16 { 255 } else { 0 };
            [v, (y / 32) as u8 * 255, 0, 255]
        });
        mcmeta("water_flow", r#"{"animation":{}}"#);
        // sea lantern: every listed frame is invalid, so it stays still on strip frame 0
        pack.png("sea_lantern", 16, 32, |_, y| {
            [0, 0, if y < 16 { 50 } else { 250 }, 255]
        });
        mcmeta("sea_lantern", r#"{"animation":{"frames":[5,9]}}"#);

        let registry = boxel_demo::blocks::registry();
        let (data, report) =
            load_block_textures(&registry, &demo_pack(pack.0.clone())).expect("pack");
        assert_eq!(report.animated, 3, "{:?}", data.animations);
        // the water flow moves by its frames and does not scroll; the pack has no lava flow
        assert_eq!(data.animated_flow, [true, false]);
        assert_eq!(report.animation_bytes, (4 + 2 + 2) * data.layer_bytes());
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("boxel:lamp_blue") && w.contains("index 5")),
            "{:?}",
            report.warnings
        );
        let animation = |name: &str| {
            let layer = layer_of(&registry, name) as u32;
            data.animations
                .iter()
                .find(|a| a.layer == layer)
                .unwrap_or_else(|| panic!("{name} is not animated"))
        };
        let level0 = |bytes: &[u8], x: u32, y: u32| -> [u8; 4] {
            let i = ((y * data.size + x) * 4) as usize;
            bytes[i..i + 4].try_into().expect("texel")
        };
        let tinted = |v: u8| WATER_TINT.map(|t| ((u32::from(v) * u32::from(t) + 127) / 255) as u8);

        // water: frame k is gray k * 60 tinted; tick 3 blends frame 1 halfway into frame 2
        let water = animation("boxel:water");
        assert_eq!(water.timeline().period(), 8);
        let mut scratch = Vec::new();
        for (tick, frame) in [(0, 0u8), (2, 1), (4, 2), (6, 3), (8, 0)] {
            let key = water.key(tick);
            assert!(!key.blends(), "tick {tick}");
            let [r, g, b] = tinted(frame * 60);
            assert_eq!(
                level0(water.pixels(key, &mut scratch), 5, 5),
                [r, g, b, 200]
            );
        }
        let (one, two) = (tinted(60), tinted(120));
        let mixed: Vec<u8> = (0..3)
            .map(|c| boxel_pack::mix_channel(0.5, one[c], two[c]))
            .collect();
        let blended = level0(water.pixels(water.key(3), &mut scratch), 9, 2);
        assert_eq!(blended, [mixed[0], mixed[1], mixed[2], 200]);
        // the array holds tick 0
        let layer = layer_of(&registry, "boxel:water");
        assert_eq!(texel(&data, layer, 0, 0), level0(water.frame(0), 0, 0));
        // every mip of a frame is the frame's own chain
        let mut chain = Vec::new();
        let gray = tinted(120);
        let frame2 = vec![[gray[0], gray[1], gray[2], 200]; 256];
        push_layer(&mut chain, &frame2, 16, false);
        assert_eq!(water.frame(2), chain.as_slice());

        // lava: frames 2 (3 ticks), 0 (1 tick), 2 (3 ticks); not interpolated
        let lava = animation("boxel:lava");
        assert_eq!(lava.timeline().period(), 7);
        let shown: Vec<u32> = (0..8).map(|t| lava.key(t).current).collect();
        assert_eq!(shown, [2, 2, 2, 0, 2, 2, 2, 2]);
        assert_eq!(level0(lava.frame(2), 1, 1), [200, 0, 0, 255]);
        let lava_layer = layer_of(&registry, "boxel:lava");
        assert_eq!(texel(&data, lava_layer, 1, 1), [200, 0, 0, 255]);

        // flow: each frame gives its own top-left quarter
        let flow = animation("boxel:water_flow");
        let [r, g, b] = WATER_TINT;
        assert_eq!(level0(flow.frame(0), 15, 15), [r, 0, 0, 255]);
        assert_eq!(level0(flow.frame(1), 15, 15), [r, g, 0, 255]);
        let _ = b;

        // the still sea lantern shows strip frame 0
        let lamp = layer_of(&registry, "boxel:lamp_blue");
        assert_eq!(texel(&data, lamp, 3, 3), [0, 0, 50, 255]);
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

        let registry = boxel_demo::blocks::registry();
        let (data, report) =
            load_block_textures(&registry, &demo_pack(pack.0.clone())).expect("pack");
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

    /// ADR 0019: every state of ours reads the pack variant of its property string, so the logs
    /// and slabs map onto Minecraft's `oak_log` and `stone_slab` blockstates as they are.
    #[test]
    fn each_state_picks_the_variant_of_its_properties() {
        let pack = TempPack::new("variants");
        pack.file(
            "blockstates/oak_log.json",
            r#"{"variants":{"axis=x":{"model":"block/log_x"},"axis=y":{"model":"block/log_y"},
                "axis=z":{"model":"block/log_z"}}}"#,
        );
        pack.file(
            "blockstates/stone_slab.json",
            r#"{"variants":{"type=bottom":{"model":"block/slab"},
                "type=double":{"model":"block/stone"},"type=top":{"model":"block/slab_top"}}}"#,
        );
        // A key over two properties, one we lack: only a state that has both holds it.
        pack.file(
            "blockstates/grass_block.json",
            r#"{"variants":{"":{"model":"block/any"},"snowy=false":{"model":"block/grass"},
                "snowy=true":{"model":"block/snowy"}}}"#,
        );
        pack.file(
            "blockstates/oak_fence.json",
            r#"{"multipart":[{"when":{"north":"true"},"apply":{"model":"block/fence_side"}},
                {"apply":{"model":"block/fence_post"}}]}"#,
        );
        let pack = ResourcePack::open(&pack.0).expect("pack");
        let registry = boxel_demo::blocks::registry();
        let model_of = |block: &str, values: &BTreeMap<String, String>| {
            let state = pack
                .blockstate(&ResourceId::new(ResourceId::DEFAULT_NAMESPACE, block))
                .expect("blockstate");
            pack_variant(state, values).map(|list| list.0[0].model.to_string())
        };
        let values_of = |state| {
            registry
                .values(state)
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>()
        };
        for (block, mc, want) in [
            ("boxel:log", "oak_log", &["log_x", "log_y", "log_z"][..]),
            (
                "boxel:stone_slab",
                "stone_slab",
                // each type dry and waterlogged (ADR 0025)
                &["slab", "slab", "slab_top", "slab_top", "stone", "stone"],
            ),
        ] {
            let block = registry.block_by_name(block).expect("block");
            let models: Vec<String> = registry
                .states(block)
                .map(|s| model_of(mc, &values_of(s)).expect("variant"))
                .collect();
            let want: Vec<String> = want
                .iter()
                .map(|m| format!("minecraft:block/{m}"))
                .collect();
            assert_eq!(models, want);
        }
        let grass = |key: &str| model_of("grass_block", &parse_variant_key(key));
        assert_eq!(
            grass("snowy=false").as_deref(),
            Some("minecraft:block/grass")
        );
        assert_eq!(grass("").as_deref(), Some("minecraft:block/any"));
        let fence = |key: &str| model_of("oak_fence", &parse_variant_key(key));
        assert_eq!(
            fence("north=true").as_deref(),
            Some("minecraft:block/fence_side")
        );
        assert_eq!(
            fence("north=false").as_deref(),
            Some("minecraft:block/fence_post")
        );
        // The recipes ask with the state's values: the log layers read `axis=y`.
        let recipes = boxel_demo::pack::recipes();
        let log_top = recipes
            .iter()
            .find(|r| r.layer == "boxel:log_top")
            .expect("recipe");
        assert_eq!(
            model_of("oak_log", &recipe_values(&registry, log_top)).as_deref(),
            Some("minecraft:block/log_y")
        );
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
