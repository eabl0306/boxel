//! boxel's WGSL shaders (embedded with `include_str!`) and the `#[repr(C)]` mirrors of every GPU
//! struct they read. Composition is plain concatenation: `common.wgsl` and `sun_off.wgsl` are
//! prepended to the voxel, model and cell modules (High's shadowed modules take
//! `shadow_common.wgsl` and `sun_shadow.wgsl` in place of `sun_off.wgsl`). Every module is validated by naga in `cargo test` (WGSL
//! front-end + GLSL 330 / ES 300 / 430 / ES 310 back-ends where the tier can run on GL), and every
//! mirror struct has a layout test against naga's layouter (`tests/naga.rs`).
//!
//! Shader rules (frozen, tested): no `@builtin(instance_index)`, `@builtin(draw_index)`,
//! `@builtin(num_workgroups)`; `@builtin(vertex_index)` only with zero base vertex/first vertex;
//! integer `textureLoad` mip levels; no `enable subgroups;`.

use std::sync::LazyLock;

pub mod gpu;

/// `common.wgsl`: `PackedQuad` decoding, View/ChunkGpu structs, shading helpers.
pub const COMMON_WGSL: &str = include_str!("../shaders/common.wgsl");
/// `sun_off.wgsl`: `sun_factor` without sun shadows, exactly 1.0 (ADR 0065). It follows
/// `common.wgsl` in every module that includes it, since `voxel_light` calls it. It adds no
/// binding, `View` field or vertex attribute.
pub const SUN_OFF_WGSL: &str = include_str!("../shaders/sun_off.wgsl");
/// `sun_shadow.wgsl`: `sun_factor` and `sun_factor_clip` from the sun shadow maps (ADR 0065,
/// lanes S4 and S5), High only, in place of [`SUN_OFF_WGSL`] in the shadowed modules
/// ([`ShaderModuleId::VoxelPullShadowed`], [`ShaderModuleId::ModelShadowed`],
/// [`ShaderModuleId::CellShadowed`]). It binds group 3.
pub const SUN_SHADOW_WGSL: &str = include_str!("../shaders/sun_shadow.wgsl");
/// `voxel_pull.wgsl`: High/Mid vertex pulling (`vs_pull`, `fs_opaque`).
pub const VOXEL_PULL_WGSL: &str = include_str!("../shaders/voxel_pull.wgsl");
/// `voxel_instanced.wgsl`: Low instanced quads (`vs_instanced`, `fs_opaque`).
pub const VOXEL_INSTANCED_WGSL: &str = include_str!("../shaders/voxel_instanced.wgsl");
/// `cull.wgsl`: High GPU culling (`cull_chunks`).
pub const CULL_WGSL: &str = include_str!("../shaders/cull.wgsl");
/// `blit.wgsl`: fullscreen blit (`vs_fullscreen`, `fs_blit`, `fs_blit_encode_srgb`).
pub const BLIT_WGSL: &str = include_str!("../shaders/blit.wgsl");
/// `overlay.wgsl`: screen-space overlay rectangles (`vs_overlay`, `fs_overlay`).
pub const OVERLAY_WGSL: &str = include_str!("../shaders/overlay.wgsl");
/// `outline.wgsl`: the targeted block's outline, clip-space quads (`vs_outline`, `fs_outline`).
pub const OUTLINE_WGSL: &str = include_str!("../shaders/outline.wgsl");
/// `fog.wgsl`: the fog march and its composite (`vs_fullscreen`, `fs_fog_march`,
/// `fs_fog_composite_half`, `fs_fog_composite_full`, `fs_fog_debug_coverage`,
/// `fs_fog_debug_walk`).
pub const FOG_WGSL: &str = include_str!("../shaders/fog.wgsl");
/// `lod_common.wgsl`: what both far LOD pipelines share (`lod_vertex`, `fs_lod`, ADR 0031).
pub const LOD_COMMON_WGSL: &str = include_str!("../shaders/lod_common.wgsl");
/// `lod.wgsl`: far LOD nodes, instanced, on every tier (`vs_lod`, ADR 0031).
pub const LOD_WGSL: &str = include_str!("../shaders/lod.wgsl");
/// `lod_pull.wgsl`: far LOD nodes, vertex pulling, High (`vs_lod_pull`, ADR 0031).
pub const LOD_PULL_WGSL: &str = include_str!("../shaders/lod_pull.wgsl");
/// `lod_cull.wgsl`: High GPU culling of the far LOD nodes (`cull_lod`, ADR 0031).
pub const LOD_CULL_WGSL: &str = include_str!("../shaders/lod_cull.wgsl");
/// `lod_hiz.wgsl`: the depth pyramid the far LOD's High cull tests against (`hiz_reduce`,
/// ADR 0031).
pub const LOD_HIZ_WGSL: &str = include_str!("../shaders/lod_hiz.wgsl");
/// `ui.wgsl`: the game UI's paint list, one atlas texture read with `textureLoad` (`vs_ui`,
/// `fs_ui`, ADR 0033).
pub const UI_WGSL: &str = include_str!("../shaders/ui.wgsl");
/// `model.wgsl`: entity models posed on the CPU, one skin read with `textureLoad` (`vs_model`,
/// `fs_model_opaque`, `fs_model_cutout`, ADR 0040).
pub const MODEL_WGSL: &str = include_str!("../shaders/model.wgsl");
/// `cell.wgsl`: the quads cells draw from their data, baked in the chunk meshes, one texture
/// array of the content's images (`vs_cell`, `fs_cell`, ADR 0050).
pub const CELL_WGSL: &str = include_str!("../shaders/cell.wgsl");
/// `shadow_common.wgsl`: the shadow uniform's `ShadowCascade`, shared by the sun shadow modules
/// (ADR 0065, High only).
pub const SHADOW_COMMON_WGSL: &str = include_str!("../shaders/shadow_common.wgsl");
/// `shadow_cull.wgsl`: the casters of the sun shadow cascades (`cull_shadow_casters`, ADR 0065).
pub const SHADOW_CULL_WGSL: &str = include_str!("../shaders/shadow_cull.wgsl");
/// `shadow.wgsl`: the casters' depth in a cascade's map (`vs_shadow`, `vs_shadow_cutout`,
/// `fs_shadow_cutout`, ADR 0065).
pub const SHADOW_WGSL: &str = include_str!("../shaders/shadow.wgsl");
/// `model_shadow.wgsl`: the entity models' depth in a cascade's map (`vs_model_shadow`,
/// `fs_model_shadow_cutout`, ADR 0065, lane S5).
pub const MODEL_SHADOW_WGSL: &str = include_str!("../shaders/model_shadow.wgsl");
/// `cell_shadow.wgsl`: the cutout cell quads' depth in a cascade's map (`vs_cell_shadow`,
/// `fs_cell_shadow`, ADR 0065, lane S5).
pub const CELL_SHADOW_WGSL: &str = include_str!("../shaders/cell_shadow.wgsl");

/// Shader stage of an entry point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Vertex.
    Vertex,
    /// Fragment.
    Fragment,
    /// Compute.
    Compute,
}

/// A composed shader module.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShaderModuleId {
    /// common + `sun_off` + `voxel_pull` (tiers High, Mid).
    VoxelPull,
    /// common + `shadow_common` + `sun_shadow` + `voxel_pull`: the voxels receiving sun shadows
    /// (tier High, only while sun shadows are on and a frame has some, ADR 0065).
    VoxelPullShadowed,
    /// common + `sun_off` + `voxel_instanced` (tier Low).
    VoxelInstanced,
    /// common + `sun_off` + cull (tier High).
    Cull,
    /// blit (all tiers).
    Blit,
    /// overlay rectangles (all tiers).
    Overlay,
    /// block outline (all tiers).
    Outline,
    /// fog march and composite (all tiers, only with the fog plugin).
    Fog,
    /// common + `sun_off` + `lod_common` + lod: far LOD nodes, instanced (all tiers, only with
    /// the LOD plugin).
    Lod,
    /// common + `sun_off` + `lod_common` + `lod_pull`: far LOD nodes drawn from the GPU cull's
    /// lists (tier High, only with the LOD plugin).
    LodPull,
    /// common + `sun_off` + `lod_cull`: GPU culling of the far LOD nodes (tier High, only with
    /// the LOD plugin).
    LodCull,
    /// `lod_hiz`: the depth pyramid of the far LOD's cull (tier High, only with the LOD plugin).
    LodHiz,
    /// The game UI's paint list (all tiers, only with `boxel_render`'s feature `ui` and a
    /// non-empty paint list).
    Ui,
    /// common + `sun_off` + model: entity models (all tiers, only once a model instance is drawn).
    Model,
    /// common + `shadow_common` + `sun_shadow` + model: the entity models receiving sun shadows
    /// (tier High, only while sun shadows are on and a frame has some, ADR 0065).
    ModelShadowed,
    /// common + `sun_off` + cell: cell quads (all tiers, only once a chunk mesh has some,
    /// ADR 0050).
    Cell,
    /// common + `shadow_common` + `sun_shadow` + cell: the cell quads receiving sun shadows
    /// (tier High, only while sun shadows are on and a frame has some, ADR 0065).
    CellShadowed,
    /// common + `sun_off` + `shadow_common` + `shadow_cull`: the casters of the sun shadow
    /// cascades (tier High, only while sun shadows are on, ADR 0065).
    ShadowCull,
    /// common + `sun_off` + `shadow_common` + shadow: the casters' depth in the sun shadow maps
    /// (tier High, only while sun shadows are on, ADR 0065).
    Shadow,
    /// common + `sun_off` + `shadow_common` + model + `model_shadow`: the entity models' depth in
    /// the sun shadow maps (tier High, only while sun shadows are on, ADR 0065).
    ModelShadow,
    /// common + `sun_off` + `shadow_common` + cell + `cell_shadow`: the cell quads' depth in the
    /// sun shadow maps (tier High, only while sun shadows are on, ADR 0065).
    CellShadow,
}

// `common.wgsl` and the sun factor without shadows: what every voxel module starts with.
fn common_sun_off(rest: &[&str]) -> String {
    let mut src = format!("{COMMON_WGSL}\n{SUN_OFF_WGSL}");
    for part in rest {
        src.push('\n');
        src.push_str(part);
    }
    src
}

static VOXEL_PULL: LazyLock<String> = LazyLock::new(|| common_sun_off(&[VOXEL_PULL_WGSL]));
// The shadowed twin of a module of `common_sun_off(&[rest])`: the same text but the sun factor.
fn common_sun_shadow(rest: &str) -> String {
    [COMMON_WGSL, SHADOW_COMMON_WGSL, SUN_SHADOW_WGSL, rest].join("\n")
}

static VOXEL_PULL_SHADOWED: LazyLock<String> = LazyLock::new(|| common_sun_shadow(VOXEL_PULL_WGSL));
static VOXEL_INSTANCED: LazyLock<String> =
    LazyLock::new(|| common_sun_off(&[VOXEL_INSTANCED_WGSL]));
static CULL: LazyLock<String> = LazyLock::new(|| common_sun_off(&[CULL_WGSL]));
static LOD: LazyLock<String> = LazyLock::new(|| common_sun_off(&[LOD_COMMON_WGSL, LOD_WGSL]));
static LOD_PULL: LazyLock<String> =
    LazyLock::new(|| common_sun_off(&[LOD_COMMON_WGSL, LOD_PULL_WGSL]));
static LOD_CULL: LazyLock<String> = LazyLock::new(|| common_sun_off(&[LOD_CULL_WGSL]));
static MODEL: LazyLock<String> = LazyLock::new(|| common_sun_off(&[MODEL_WGSL]));
static MODEL_SHADOWED: LazyLock<String> = LazyLock::new(|| common_sun_shadow(MODEL_WGSL));
static CELL: LazyLock<String> = LazyLock::new(|| common_sun_off(&[CELL_WGSL]));
static CELL_SHADOWED: LazyLock<String> = LazyLock::new(|| common_sun_shadow(CELL_WGSL));
static SHADOW_CULL: LazyLock<String> =
    LazyLock::new(|| common_sun_off(&[SHADOW_COMMON_WGSL, SHADOW_CULL_WGSL]));
static SHADOW: LazyLock<String> =
    LazyLock::new(|| common_sun_off(&[SHADOW_COMMON_WGSL, SHADOW_WGSL]));
static MODEL_SHADOW: LazyLock<String> =
    LazyLock::new(|| common_sun_off(&[SHADOW_COMMON_WGSL, MODEL_WGSL, MODEL_SHADOW_WGSL]));
static CELL_SHADOW: LazyLock<String> =
    LazyLock::new(|| common_sun_off(&[SHADOW_COMMON_WGSL, CELL_WGSL, CELL_SHADOW_WGSL]));

impl ShaderModuleId {
    /// All modules.
    pub const ALL: [ShaderModuleId; 21] = [
        ShaderModuleId::VoxelPull,
        ShaderModuleId::VoxelPullShadowed,
        ShaderModuleId::VoxelInstanced,
        ShaderModuleId::Cull,
        ShaderModuleId::Blit,
        ShaderModuleId::Overlay,
        ShaderModuleId::Outline,
        ShaderModuleId::Fog,
        ShaderModuleId::Lod,
        ShaderModuleId::LodPull,
        ShaderModuleId::LodCull,
        ShaderModuleId::LodHiz,
        ShaderModuleId::Ui,
        ShaderModuleId::Model,
        ShaderModuleId::ModelShadowed,
        ShaderModuleId::Cell,
        ShaderModuleId::CellShadowed,
        ShaderModuleId::ShadowCull,
        ShaderModuleId::Shadow,
        ShaderModuleId::ModelShadow,
        ShaderModuleId::CellShadow,
    ];

    /// Debug label.
    pub const fn label(self) -> &'static str {
        match self {
            ShaderModuleId::VoxelPull => "boxel voxel_pull",
            ShaderModuleId::VoxelPullShadowed => "boxel voxel_pull_shadowed",
            ShaderModuleId::VoxelInstanced => "boxel voxel_instanced",
            ShaderModuleId::Cull => "boxel cull",
            ShaderModuleId::Blit => "boxel blit",
            ShaderModuleId::Overlay => "boxel overlay",
            ShaderModuleId::Outline => "boxel outline",
            ShaderModuleId::Fog => "boxel fog",
            ShaderModuleId::Lod => "boxel lod",
            ShaderModuleId::LodPull => "boxel lod_pull",
            ShaderModuleId::LodCull => "boxel lod_cull",
            ShaderModuleId::LodHiz => "boxel lod_hiz",
            ShaderModuleId::Ui => "boxel ui",
            ShaderModuleId::Model => "boxel model",
            ShaderModuleId::ModelShadowed => "boxel model_shadowed",
            ShaderModuleId::Cell => "boxel cell",
            ShaderModuleId::CellShadowed => "boxel cell_shadowed",
            ShaderModuleId::ShadowCull => "boxel shadow_cull",
            ShaderModuleId::Shadow => "boxel shadow",
            ShaderModuleId::ModelShadow => "boxel model_shadow",
            ShaderModuleId::CellShadow => "boxel cell_shadow",
        }
    }

    /// Full WGSL source (composed once, cached).
    pub fn source(self) -> &'static str {
        match self {
            ShaderModuleId::VoxelPull => VOXEL_PULL.as_str(),
            ShaderModuleId::VoxelPullShadowed => VOXEL_PULL_SHADOWED.as_str(),
            ShaderModuleId::VoxelInstanced => VOXEL_INSTANCED.as_str(),
            ShaderModuleId::Cull => CULL.as_str(),
            ShaderModuleId::Blit => BLIT_WGSL,
            ShaderModuleId::Overlay => OVERLAY_WGSL,
            ShaderModuleId::Outline => OUTLINE_WGSL,
            ShaderModuleId::Fog => FOG_WGSL,
            ShaderModuleId::Lod => LOD.as_str(),
            ShaderModuleId::LodPull => LOD_PULL.as_str(),
            ShaderModuleId::LodCull => LOD_CULL.as_str(),
            ShaderModuleId::LodHiz => LOD_HIZ_WGSL,
            ShaderModuleId::Ui => UI_WGSL,
            ShaderModuleId::Model => MODEL.as_str(),
            ShaderModuleId::ModelShadowed => MODEL_SHADOWED.as_str(),
            ShaderModuleId::Cell => CELL.as_str(),
            ShaderModuleId::CellShadowed => CELL_SHADOWED.as_str(),
            ShaderModuleId::ShadowCull => SHADOW_CULL.as_str(),
            ShaderModuleId::Shadow => SHADOW.as_str(),
            ShaderModuleId::ModelShadow => MODEL_SHADOW.as_str(),
            ShaderModuleId::CellShadow => CELL_SHADOW.as_str(),
        }
    }

    /// Entry points and stages.
    pub const fn entry_points(self) -> &'static [(&'static str, Stage)] {
        match self {
            ShaderModuleId::VoxelPull | ShaderModuleId::VoxelPullShadowed => &[
                ("vs_pull", Stage::Vertex),
                ("fs_opaque", Stage::Fragment),
                ("fs_cutout", Stage::Fragment),
                ("fs_translucent", Stage::Fragment),
                ("fs_translucent_depth", Stage::Fragment),
            ],
            ShaderModuleId::VoxelInstanced => &[
                ("vs_instanced", Stage::Vertex),
                ("fs_opaque", Stage::Fragment),
                ("fs_cutout", Stage::Fragment),
                ("fs_translucent", Stage::Fragment),
                ("fs_translucent_depth", Stage::Fragment),
            ],
            ShaderModuleId::Cull => &[("cull_chunks", Stage::Compute)],
            ShaderModuleId::Blit => &[
                ("vs_fullscreen", Stage::Vertex),
                ("fs_blit", Stage::Fragment),
                ("fs_blit_encode_srgb", Stage::Fragment),
            ],
            ShaderModuleId::Overlay => &[
                ("vs_overlay", Stage::Vertex),
                ("fs_overlay", Stage::Fragment),
            ],
            ShaderModuleId::Outline => &[
                ("vs_outline", Stage::Vertex),
                ("fs_outline", Stage::Fragment),
            ],
            ShaderModuleId::Fog => &[
                ("vs_fullscreen", Stage::Vertex),
                ("fs_fog_march", Stage::Fragment),
                ("fs_fog_composite_half", Stage::Fragment),
                ("fs_fog_composite_full", Stage::Fragment),
                ("fs_fog_debug_coverage", Stage::Fragment),
                ("fs_fog_debug_walk", Stage::Fragment),
            ],
            ShaderModuleId::Lod => &[
                ("vs_lod", Stage::Vertex),
                ("fs_lod", Stage::Fragment),
                ("vs_lod_water", Stage::Vertex),
                ("fs_lod_water", Stage::Fragment),
                ("fs_lod_water_depth", Stage::Fragment),
            ],
            ShaderModuleId::LodPull => &[
                ("vs_lod_pull", Stage::Vertex),
                ("fs_lod", Stage::Fragment),
                ("vs_lod_pull_water", Stage::Vertex),
                ("fs_lod_water", Stage::Fragment),
                ("fs_lod_water_depth", Stage::Fragment),
            ],
            ShaderModuleId::LodCull => &[("cull_lod", Stage::Compute)],
            ShaderModuleId::LodHiz => &[("hiz_reduce", Stage::Compute)],
            ShaderModuleId::Ui => &[("vs_ui", Stage::Vertex), ("fs_ui", Stage::Fragment)],
            ShaderModuleId::Model | ShaderModuleId::ModelShadowed => &[
                ("vs_model", Stage::Vertex),
                ("fs_model_opaque", Stage::Fragment),
                ("fs_model_cutout", Stage::Fragment),
            ],
            ShaderModuleId::Cell | ShaderModuleId::CellShadowed => &[
                ("vs_cell", Stage::Vertex),
                ("fs_cell", Stage::Fragment),
                ("fs_cell_translucent", Stage::Fragment),
                ("vs_cell_pattern", Stage::Vertex),
                ("fs_cell_pattern", Stage::Fragment),
            ],
            ShaderModuleId::ShadowCull => &[("cull_shadow_casters", Stage::Compute)],
            ShaderModuleId::Shadow => &[
                ("vs_shadow", Stage::Vertex),
                ("vs_shadow_cutout", Stage::Vertex),
                ("fs_shadow_cutout", Stage::Fragment),
            ],
            // The casters' entry points after those of the module they extend (whose vertex
            // input and varyings they share).
            ShaderModuleId::ModelShadow => &[
                ("vs_model", Stage::Vertex),
                ("fs_model_opaque", Stage::Fragment),
                ("fs_model_cutout", Stage::Fragment),
                ("vs_model_shadow", Stage::Vertex),
                ("fs_model_shadow_cutout", Stage::Fragment),
            ],
            ShaderModuleId::CellShadow => &[
                ("vs_cell", Stage::Vertex),
                ("fs_cell", Stage::Fragment),
                ("fs_cell_translucent", Stage::Fragment),
                ("vs_cell_pattern", Stage::Vertex),
                ("fs_cell_pattern", Stage::Fragment),
                ("vs_cell_shadow", Stage::Vertex),
                ("fs_cell_shadow", Stage::Fragment),
            ],
        }
    }

    /// True if the module declares `var<immediate>` (needs `Features::IMMEDIATES` and the naga
    /// `IMMEDIATES` capability in validation).
    pub const fn uses_immediates(self) -> bool {
        matches!(self, ShaderModuleId::VoxelInstanced | ShaderModuleId::Lod)
    }

    /// True if only the High tier ever creates the module: the GPU cull, the far LOD's
    /// GPU-driven path and everything of sun shadows (ADR 0065). Mid and Low never see their
    /// text, bindings or layouts.
    pub const fn high_only(self) -> bool {
        matches!(
            self,
            ShaderModuleId::Cull
                | ShaderModuleId::LodPull
                | ShaderModuleId::LodCull
                | ShaderModuleId::LodHiz
                | ShaderModuleId::ShadowCull
                | ShaderModuleId::Shadow
                | ShaderModuleId::VoxelPullShadowed
                | ShaderModuleId::ModelShadowed
                | ShaderModuleId::CellShadowed
                | ShaderModuleId::ModelShadow
                | ShaderModuleId::CellShadow
        )
    }

    /// Lowest GL version the module must translate to (checked by naga tests).
    pub const fn min_gl(self) -> (u8, u8) {
        match self {
            ShaderModuleId::VoxelPull
            | ShaderModuleId::VoxelPullShadowed
            | ShaderModuleId::Cull
            | ShaderModuleId::LodPull
            | ShaderModuleId::LodCull
            | ShaderModuleId::LodHiz
            | ShaderModuleId::ShadowCull
            | ShaderModuleId::Shadow
            | ShaderModuleId::ModelShadowed
            | ShaderModuleId::CellShadowed
            | ShaderModuleId::ModelShadow
            | ShaderModuleId::CellShadow => (4, 3),
            ShaderModuleId::VoxelInstanced
            | ShaderModuleId::Blit
            | ShaderModuleId::Overlay
            | ShaderModuleId::Outline
            | ShaderModuleId::Fog
            | ShaderModuleId::Lod
            | ShaderModuleId::Ui
            | ShaderModuleId::Model
            | ShaderModuleId::Cell => (3, 3),
        }
    }
}

/// Creates the module with wgpu's default (checked) runtime checks.
pub fn create_module(device: &wgpu::Device, id: ShaderModuleId) -> wgpu::ShaderModule {
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(id.label()),
        source: wgpu::ShaderSource::Wgsl(id.source().into()),
    })
}
