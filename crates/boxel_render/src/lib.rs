//! `boxel_render`: the voxel renderer. No render graph: each tier is a fixed, ordered list of
//! passes over explicit frame resources (ADR 0007). All tiers share the mesher output
//! (`PackedQuad`), the quad arena and the WGSL decode; only submission differs:
//!
//! | Tier | Culling | Submission | Per-draw data reaches the shader via |
//! |------|---------|------------|--------------------------------------|
//! | High | compute (frustum + face buckets) | `multi_draw_indexed_indirect_count` | `DrawRef` instance attribute (`first_instance` = draw index, written by the cull shader) |
//! | Mid  | CPU (same tests) | `draw_indexed` per visible bucket | `DrawRef` instance attribute (`first_instance` = draw index, written by the CPU) |
//! | Low  | CPU | `draw(0..4, quads)` triangle strip per bucket | quad = instance attribute, chunk origin = immediates |
//!
//! [`VoxelRenderer`] is ECS-free (used directly by offscreen tests); [`RenderPlugin`] wires it
//! into the app: GPU bootstrap when the window appears, mesh extraction from `boxel_world`,
//! camera, present (vsync switchable at runtime with [`PresentSettings`]), screenshots,
//! device-loss recovery.

/// `let _span = frame_span!("name");` opens a tracing span until the end of the scope, for frame
/// breakdowns (`boxel_log::span_times`, the stress example's `--spans`). Only with feature
/// `trace`; without it the macro is a zero-sized value and no code.
#[cfg(feature = "trace")]
macro_rules! frame_span {
    ($name:literal) => {
        tracing::info_span!($name).entered()
    };
}
#[cfg(not(feature = "trace"))]
macro_rules! frame_span {
    ($name:literal) => {
        $crate::NoSpan
    };
}

/// What `frame_span!` gives without feature `trace`.
#[cfg(not(feature = "trace"))]
pub(crate) struct NoSpan;

mod light;
mod outline;
mod overlay;
mod pack_textures;
mod plugin;
mod renderer;
mod smoke;
mod textures;

mod arena;
mod chunk_table;
mod path;
mod sort;
mod targets;
mod upload;

pub use light::{LIGHT_LEVELS, SceneLight, light_curve};
pub use outline::{BlockOutline, Outline, OutlineEdge, block_outline_edges, outline_edges};
pub use overlay::{OverlayDraw, OverlayRect};
pub use pack_textures::{
    FOLIAGE_TINT, GRASS_TINT, MAX_PACK_TEXTURE_SIZE, TextureReport, TextureSource, WATER_TINT,
    load_block_textures,
};
pub use path::{CullCounts, cull_counts};
#[doc(hidden)]
pub use plugin::frame_visibility;
pub use plugin::{
    DebugDestroyDevice, GpuFailed, GpuReady, PresentSettings, Render, RenderInfo, RenderPlugin,
    RenderSettings, RenderSystems, RequestScreenshot, ScreenshotSaved, SurfaceInfo,
};
pub use renderer::{
    DEFAULT_SKY_COLOR, RenderStats, RendererConfig, RendererError, ViewParams, VoxelRenderer,
};
pub use smoke::RendererSmokeTest;
pub use sort::TranslucentSort;
pub use textures::{BLOCK_TEXTURE_SIZE, BlockTextureData, generate_block_textures};
pub use upload::arena_len;

/// Format of the main color target (screenshots read it).
pub const MAIN_COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Depth format (reverse-Z, `CompareFunction::Greater`, clear 0).
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Common items.
pub mod prelude {
    pub use crate::{
        BlockOutline, PresentSettings, RenderInfo, RenderPlugin, RenderSettings, RequestScreenshot,
        ScreenshotSaved,
    };
}
