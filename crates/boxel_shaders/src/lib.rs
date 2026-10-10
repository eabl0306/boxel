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
}

static VOXEL_PULL: LazyLock<String> = LazyLock::new(|| format!("{COMMON_WGSL}\n{VOXEL_PULL_WGSL}"));
static VOXEL_INSTANCED: LazyLock<String> =
    LazyLock::new(|| format!("{COMMON_WGSL}\n{VOXEL_INSTANCED_WGSL}"));
static CULL: LazyLock<String> = LazyLock::new(|| format!("{COMMON_WGSL}\n{CULL_WGSL}"));

impl ShaderModuleId {
    /// All modules.
    pub const ALL: [ShaderModuleId; 6] = [
        ShaderModuleId::VoxelPull,
        ShaderModuleId::VoxelInstanced,
        ShaderModuleId::Cull,
        ShaderModuleId::Blit,
        ShaderModuleId::Overlay,
        ShaderModuleId::Outline,
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
        }
    }

    /// True if the module declares `var<immediate>` (needs `Features::IMMEDIATES` and the naga
    /// `IMMEDIATES` capability in validation).
    pub const fn uses_immediates(self) -> bool {
        matches!(self, ShaderModuleId::VoxelInstanced)
    }

    /// Lowest GL version the module must translate to (checked by naga tests).
    pub const fn min_gl(self) -> (u8, u8) {
        match self {
            ShaderModuleId::VoxelPull | ShaderModuleId::Cull => (4, 3),
            ShaderModuleId::VoxelInstanced
            | ShaderModuleId::Blit
            | ShaderModuleId::Overlay
            | ShaderModuleId::Outline => (3, 3),
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
