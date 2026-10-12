//! boxel's WGSL shaders (embedded with `include_str!`) and the `#[repr(C)]` mirrors of every GPU
//! struct they read. Composition is plain concatenation: `common.wgsl` is prepended to the voxel
//! modules. Every module is validated by naga in `cargo test` (WGSL front-end + GLSL 330 /
//! ES 300 / 430 / ES 310 back-ends where the tier can run on GL), and every mirror struct has a
//! layout test against naga's layouter (`tests/naga.rs`).
//!
//! Shader rules (frozen, tested): no `@builtin(instance_index)`, `@builtin(draw_index)`,
//! `@builtin(num_workgroups)`; `@builtin(vertex_index)` only with zero base vertex/first vertex;
//! integer `textureLoad` mip levels; no `enable subgroups;`.

use std::sync::LazyLock;

pub mod gpu;

/// `common.wgsl`: `PackedQuad` decoding, View/ChunkGpu structs, shading helpers.
pub const COMMON_WGSL: &str = include_str!("../shaders/common.wgsl");
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
    /// common + `voxel_pull` (tiers High, Mid).
    VoxelPull,
    /// common + `voxel_instanced` (tier Low).
    VoxelInstanced,
    /// common + cull (tier High).
    Cull,
    /// blit (all tiers).
    Blit,
    /// overlay rectangles (all tiers).
    Overlay,
    /// block outline (all tiers).
    Outline,
    /// fog march and composite (all tiers, only with the fog plugin).
    Fog,
    /// common + `lod_common` + lod: far LOD nodes, instanced (all tiers, only with the LOD
    /// plugin).
    Lod,
    /// common + `lod_common` + `lod_pull`: far LOD nodes drawn from the GPU cull's lists (tier
    /// High, only with the LOD plugin).
    LodPull,
    /// common + `lod_cull`: GPU culling of the far LOD nodes (tier High, only with the LOD
    /// plugin).
    LodCull,
    /// `lod_hiz`: the depth pyramid of the far LOD's cull (tier High, only with the LOD plugin).
    LodHiz,
    /// The game UI's paint list (all tiers, only with `boxel_render`'s feature `ui` and a
    /// non-empty paint list).
    Ui,
}

static VOXEL_PULL: LazyLock<String> = LazyLock::new(|| format!("{COMMON_WGSL}\n{VOXEL_PULL_WGSL}"));
static VOXEL_INSTANCED: LazyLock<String> =
    LazyLock::new(|| format!("{COMMON_WGSL}\n{VOXEL_INSTANCED_WGSL}"));
static CULL: LazyLock<String> = LazyLock::new(|| format!("{COMMON_WGSL}\n{CULL_WGSL}"));
static LOD: LazyLock<String> =
    LazyLock::new(|| format!("{COMMON_WGSL}\n{LOD_COMMON_WGSL}\n{LOD_WGSL}"));
static LOD_PULL: LazyLock<String> =
    LazyLock::new(|| format!("{COMMON_WGSL}\n{LOD_COMMON_WGSL}\n{LOD_PULL_WGSL}"));
static LOD_CULL: LazyLock<String> = LazyLock::new(|| format!("{COMMON_WGSL}\n{LOD_CULL_WGSL}"));

impl ShaderModuleId {
    /// All modules.
    pub const ALL: [ShaderModuleId; 12] = [
        ShaderModuleId::VoxelPull,
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
    ];

    /// Debug label.
    pub const fn label(self) -> &'static str {
        match self {
            ShaderModuleId::VoxelPull => "boxel voxel_pull",
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
        }
    }

    /// Full WGSL source (composed once, cached).
    pub fn source(self) -> &'static str {
        match self {
            ShaderModuleId::VoxelPull => VOXEL_PULL.as_str(),
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
        }
    }

    /// Entry points and stages.
    pub const fn entry_points(self) -> &'static [(&'static str, Stage)] {
        match self {
            ShaderModuleId::VoxelPull => &[
                ("vs_pull", Stage::Vertex),
                ("fs_opaque", Stage::Fragment),
                ("fs_cutout", Stage::Fragment),
                ("fs_translucent", Stage::Fragment),
            ],
            ShaderModuleId::VoxelInstanced => &[
                ("vs_instanced", Stage::Vertex),
                ("fs_opaque", Stage::Fragment),
                ("fs_cutout", Stage::Fragment),
                ("fs_translucent", Stage::Fragment),
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
        }
    }

    /// True if the module declares `var<immediate>` (needs `Features::IMMEDIATES` and the naga
    /// `IMMEDIATES` capability in validation).
    pub const fn uses_immediates(self) -> bool {
        matches!(self, ShaderModuleId::VoxelInstanced | ShaderModuleId::Lod)
    }

    /// Lowest GL version the module must translate to (checked by naga tests).
    pub const fn min_gl(self) -> (u8, u8) {
        match self {
            ShaderModuleId::VoxelPull
            | ShaderModuleId::Cull
            | ShaderModuleId::LodPull
            | ShaderModuleId::LodCull
            | ShaderModuleId::LodHiz => (4, 3),
            ShaderModuleId::VoxelInstanced
            | ShaderModuleId::Blit
            | ShaderModuleId::Overlay
            | ShaderModuleId::Outline
            | ShaderModuleId::Fog
            | ShaderModuleId::Lod
            | ShaderModuleId::Ui => (3, 3),
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
