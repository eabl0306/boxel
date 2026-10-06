//! Data-driven block registry. M0: one state per block (no properties yet); `BlockState(u16)` is
//! the dense runtime id (`0` = air), assigned in registration order. Frozen before the world
//! loads and shared as `Arc<BlockRegistry>`.
use crate::{Face, LightValue, OPAQUE_OPACITY};

/// Dense runtime block state id. `BlockState::AIR == BlockState(0)`. Never persisted raw
/// (persistence will store names through an id map, M1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct BlockState(pub u16);

impl BlockState {
    /// Air.
    pub const AIR: BlockState = BlockState(0);
    /// True for air.
    pub const fn is_air(self) -> bool {
        self.0 == 0
    }
}

/// How a block is drawn and how it occludes neighbors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum RenderShape {
    /// No geometry (air, invisible blocks).
    #[default]
    None,
    /// Full opaque cube: hides neighbor faces.
    Opaque,
    /// Alpha-tested cube (leaves): drawn in the opaque pass with discard; does not hide neighbors;
    /// faces between two identical cutout blocks are culled.
    Cutout,
    /// Blended cube (water, glass): meshed into the translucent bucket (drawn from M1).
    Translucent,
}

bitflags::bitflags! {
    /// Per-state flags (SoA LUT).
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct StateFlags: u16 {
        /// Collides.
        const SOLID = 1;
        /// Hides neighbor faces (shape `Opaque`).
        const OCCLUDES = 1 << 1;
        /// Liquid.
        const FLUID = 1 << 2;
        /// Emits light (some `emission` channel above 0).
        const EMISSIVE = 1 << 3;
    }
}

/// Procedural texture pattern (M0 has no texture assets; the renderer synthesizes 16x16 layers).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TexturePattern {
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
}

/// One texture layer description.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TextureSpec {
    /// Unique name (`"boxel:grass_top"`).
    pub name: String,
    /// Base sRGB color.
    pub base_color: [u8; 3],
    /// Noise amplitude (0..=64 reasonable).
    pub noise: u8,
    /// Pattern.
    pub pattern: TexturePattern,
}

/// Texture layer index in the block texture array (`0..4096`, fits `PackedQuad`'s 12 bits).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TextureId(pub u16);

/// Texture per face group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FaceTextures {
    /// +Y.
    pub top: TextureId,
    /// -Y.
    pub bottom: TextureId,
    /// ±X, ±Z.
    pub side: TextureId,
}

impl FaceTextures {
    /// Same texture on every face.
    pub const fn all(t: TextureId) -> Self {
        Self {
            top: t,
            bottom: t,
            side: t,
        }
    }
    /// Texture for `face`.
    pub const fn get(self, face: Face) -> TextureId {
        match face {
            Face::PosY => self.top,
            Face::NegY => self.bottom,
            _ => self.side,
        }
    }
}

/// Block definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockDef {
    /// Namespaced unique name (`"boxel:stone"`).
    pub name: String,
    /// Render shape.
    pub shape: RenderShape,
    /// Collides.
    pub solid: bool,
    /// Block light emission `[red, green, blue]`, each `0..=15` (white light has equal
    /// channels; `[0, 0, 0]` emits nothing). Emitters keep their emission even when opaque.
    pub emission: [u8; 3],
    /// Light opacity `0..=15`: 15 stops light (required for `RenderShape::Opaque`), 0 lets it
    /// through (air, glass), 1..=14 lets it through at a cost of `max(1, opacity)` levels per
    /// voxel (leaves and water are 1). Any value above 0 also stops direct sky.
    pub light_opacity: u8,
    /// Textures (ignored for `RenderShape::None`).
    pub textures: FaceTextures,
}

/// Registry errors.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegistryError {
    /// Name already registered.
    #[error("duplicate name {0}")]
    Duplicate(String),
    /// Name must be `namespace:path` with `[a-z0-9_]` parts.
    #[error("invalid name {0}")]
    InvalidName(String),
    /// More than 4096 textures or 65535 states.
    #[error("registry full: {0}")]
    Full(&'static str),
    /// Texture id not registered.
    #[error("unknown texture id {0}")]
    UnknownTexture(u16),
    /// Emission channel or light opacity above 15, or an `Opaque` shape whose light opacity is
    /// not 15.
    #[error("invalid light values of {0}")]
    InvalidLight(String),
}

/// Builder; `boxel:air` is pre-registered as `BlockState(0)`.
#[derive(Debug, Default)]
pub struct BlockRegistryBuilder {
    textures: Vec<TextureSpec>,
    blocks: Vec<BlockDef>,
}

impl BlockRegistryBuilder {
    /// New builder with `boxel:air` (state 0, shape `None`, texture 0 unused).
    pub fn new() -> Self {
        Self {
            textures: Vec::new(),
            blocks: vec![BlockDef {
                name: "boxel:air".to_string(),
                shape: RenderShape::None,
                solid: false,
                emission: [0; 3],
                light_opacity: 0,
                textures: FaceTextures::all(TextureId(0)),
            }],
        }
    }

    /// A builder with the built-in M0 content of [`BlockRegistry::default_blocks`], to add more
    /// blocks after it (their states follow the built-in ones).
    pub fn with_default_blocks() -> Self {
        use TexturePattern as P;
        let mut b = BlockRegistryBuilder::new();
        let mut tex = |name: &str, rgb: [u8; 3], noise: u8, pattern: TexturePattern| {
            b.add_texture(TextureSpec {
                name: format!("boxel:{name}"),
                base_color: rgb,
                noise,
                pattern,
            })
            .expect("built-in texture")
        };
        let stone = tex("stone", [125, 125, 125], 18, P::Noise);
        let dirt = tex("dirt", [134, 96, 67], 16, P::Noise);
        let grass_top = tex("grass_top", [95, 159, 53], 18, P::Noise);
        let grass_side = tex(
            "grass_side",
            [134, 96, 67],
            16,
            P::TopBand {
                color: [95, 159, 53],
                rows: 4,
            },
        );
        let sand = tex("sand", [219, 207, 163], 10, P::Noise);
        let gravel = tex(
            "gravel",
            [136, 126, 126],
            14,
            P::Speckle {
                color: [90, 85, 85],
                density: 40,
            },
        );
        let snow = tex("snow", [240, 245, 250], 6, P::Noise);
        let bedrock = tex(
            "bedrock",
            [70, 70, 70],
            24,
            P::Speckle {
                color: [30, 30, 30],
                density: 60,
            },
        );
        let log_side = tex("log_side", [102, 81, 51], 14, P::Planks);
        let log_top = tex("log_top", [156, 127, 78], 10, P::Noise);
        let leaves = tex("leaves", [60, 130, 40], 20, P::Leaves);
        let water = tex("water", [40, 90, 200], 6, P::Noise);
        let planks = tex("planks", [162, 130, 78], 10, P::Planks);
        // Light opacity: 15 for opaque cubes, 1 for leaves and water (light passes, losing one
        // level per voxel like in air, but direct sky stops at them). Nothing emits.
        let block =
            |name: &str, shape: RenderShape, solid: bool, textures: FaceTextures| BlockDef {
                name: format!("boxel:{name}"),
                shape,
                solid,
                emission: [0; 3],
                light_opacity: if shape == RenderShape::Opaque {
                    OPAQUE_OPACITY
                } else {
                    1
                },
                textures,
            };
        let all = FaceTextures::all;
        let opaque = RenderShape::Opaque;
        for def in [
            block("stone", opaque, true, all(stone)),
            block("dirt", opaque, true, all(dirt)),
            block(
                "grass",
                opaque,
                true,
                FaceTextures {
                    top: grass_top,
                    bottom: dirt,
                    side: grass_side,
                },
            ),
            block("sand", opaque, true, all(sand)),
            block("gravel", opaque, true, all(gravel)),
            block("snow", opaque, true, all(snow)),
            block("bedrock", opaque, true, all(bedrock)),
            block(
                "log",
                opaque,
                true,
                FaceTextures {
                    top: log_top,
                    bottom: log_top,
                    side: log_side,
                },
            ),
            block("leaves", RenderShape::Cutout, true, all(leaves)),
            block("water", RenderShape::Translucent, false, all(water)),
            block("planks", opaque, true, all(planks)),
        ] {
            b.add_block(def).expect("built-in block");
        }
        b
    }

    /// Registers a texture layer.
    pub fn add_texture(&mut self, spec: TextureSpec) -> Result<TextureId, RegistryError> {
        validate_name(&spec.name)?;
        if self.textures.iter().any(|t| t.name == spec.name) {
            return Err(RegistryError::Duplicate(spec.name));
        }
        if self.textures.len() >= 4096 {
            return Err(RegistryError::Full("textures"));
        }
        self.textures.push(spec);
        Ok(TextureId((self.textures.len() - 1) as u16))
    }

    /// Registers a block; returns its state.
    pub fn add_block(&mut self, def: BlockDef) -> Result<BlockState, RegistryError> {
        validate_name(&def.name)?;
        if self.blocks.iter().any(|b| b.name == def.name) {
            return Err(RegistryError::Duplicate(def.name));
        }
        if self.blocks.len() >= u16::MAX as usize {
            return Err(RegistryError::Full("block states"));
        }
        let channels_ok = def.emission.iter().all(|&c| c <= crate::MAX_LIGHT);
        let opacity_ok = def.light_opacity <= OPAQUE_OPACITY
            && (def.shape != RenderShape::Opaque || def.light_opacity == OPAQUE_OPACITY);
        if !channels_ok || !opacity_ok {
            return Err(RegistryError::InvalidLight(def.name));
        }
        if def.shape != RenderShape::None {
            for t in [def.textures.top, def.textures.bottom, def.textures.side] {
                if t.0 as usize >= self.textures.len() {
                    return Err(RegistryError::UnknownTexture(t.0));
                }
            }
        }
        self.blocks.push(def);
        Ok(BlockState((self.blocks.len() - 1) as u16))
    }

    /// Freezes the registry (builds the `SoA` lookup tables).
    pub fn build(self) -> BlockRegistry {
        let mut by_name =
            std::collections::HashMap::with_hasher(foldhash::fast::FixedState::default());
        for (i, b) in self.blocks.iter().enumerate() {
            by_name.insert(b.name.clone(), BlockState(i as u16));
        }
        BlockRegistry {
            names: self.blocks.iter().map(|b| b.name.clone()).collect(),
            shapes: self.blocks.iter().map(|b| b.shape).collect(),
            flags: self
                .blocks
                .iter()
                .map(|b| {
                    let mut f = StateFlags::empty();
                    f.set(StateFlags::SOLID, b.solid);
                    f.set(StateFlags::OCCLUDES, b.shape == RenderShape::Opaque);
                    f.set(
                        StateFlags::FLUID,
                        b.shape == RenderShape::Translucent && !b.solid,
                    );
                    f.set(StateFlags::EMISSIVE, b.emission != [0; 3]);
                    f
                })
                .collect(),
            emission: self
                .blocks
                .iter()
                .map(|b| LightValue::from_rgb(b.emission))
                .collect(),
            opacity: self.blocks.iter().map(|b| b.light_opacity).collect(),
            faces: self.blocks.iter().map(|b| b.textures).collect(),
            textures: self.textures,
            by_name,
        }
    }
}

/// `namespace:path`, both non-empty, `[a-z0-9_]` (path may contain `/`).
fn validate_name(name: &str) -> Result<(), RegistryError> {
    let ok = name.split_once(':').is_some_and(|(ns, path)| {
        !ns.is_empty()
            && !path.is_empty()
            && ns
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            && path
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '/')
    });
    if ok {
        Ok(())
    } else {
        Err(RegistryError::InvalidName(name.to_string()))
    }
}

/// Frozen registry with `SoA` lookup tables indexed by `BlockState.0`.
#[derive(Debug, Clone)]
pub struct BlockRegistry {
    names: Vec<String>,
    shapes: Vec<RenderShape>,
    flags: Vec<StateFlags>,
    emission: Vec<LightValue>,
    opacity: Vec<u8>,
    faces: Vec<FaceTextures>,
    textures: Vec<TextureSpec>,
    by_name: std::collections::HashMap<String, BlockState, foldhash::fast::FixedState>,
}

impl BlockRegistry {
    /// Built-in M0 content ([`BlockRegistryBuilder::with_default_blocks`], built), in this exact
    /// order (state ids are frozen for tests and worldgen): 0 `boxel:air`, 1 `boxel:stone`,
    /// 2 `boxel:dirt`, 3 `boxel:grass`, 4 `boxel:sand`, 5 `boxel:gravel`, 6 `boxel:snow`,
    /// 7 `boxel:bedrock`, 8 `boxel:log`, 9 `boxel:leaves` (Cutout), 10 `boxel:water`
    /// (Translucent), 11 `boxel:planks`.
    pub fn default_blocks() -> BlockRegistry {
        BlockRegistryBuilder::with_default_blocks().build()
    }
    /// Number of states.
    pub fn len(&self) -> usize {
        self.names.len()
    }
    /// True if only air.
    pub fn is_empty(&self) -> bool {
        self.names.len() <= 1
    }
    /// State by name.
    pub fn by_name(&self, name: &str) -> Option<BlockState> {
        self.by_name.get(name).copied()
    }
    /// Name of a state (`"boxel:unknown"` if out of range).
    pub fn name(&self, s: BlockState) -> &str {
        self.names
            .get(s.0 as usize)
            .map_or("boxel:unknown", String::as_str)
    }
    /// Shape (out of range -> `None`).
    pub fn shape(&self, s: BlockState) -> RenderShape {
        self.shapes.get(s.0 as usize).copied().unwrap_or_default()
    }
    /// Flags.
    pub fn flags(&self, s: BlockState) -> StateFlags {
        self.flags.get(s.0 as usize).copied().unwrap_or_default()
    }
    /// Light emission (sky 0; out of range -> none).
    pub fn emission(&self, s: BlockState) -> LightValue {
        self.emission
            .get(s.0 as usize)
            .copied()
            .unwrap_or(LightValue::DARK)
    }
    /// Light opacity `0..=15` (out of range -> 0).
    pub fn light_opacity(&self, s: BlockState) -> u8 {
        self.opacity.get(s.0 as usize).copied().unwrap_or(0)
    }
    /// Texture layer of `face` of `s`.
    pub fn face_texture(&self, s: BlockState, face: Face) -> TextureId {
        self.faces
            .get(s.0 as usize)
            .map_or(TextureId(0), |f| f.get(face))
    }
    /// Texture layers in id order (the renderer builds its texture array from this).
    pub fn textures(&self) -> &[TextureSpec] {
        &self.textures
    }
    /// Mesher voxel value of a state: `0` for `RenderShape::None`, else `s.0` (M0 identity map).
    pub fn material(&self, s: BlockState) -> u16 {
        if self.shape(s) == RenderShape::None {
            0
        } else {
            s.0
        }
    }
}
