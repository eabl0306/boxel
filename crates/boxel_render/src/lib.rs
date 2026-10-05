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
//! camera, present, screenshots, device-loss recovery.
// SKELETON: remove this attribute when the crate is fully implemented (owner: WP7).
#![allow(
    unused_variables,
    unused_mut,
    dead_code,
    clippy::todo,
    reason = "skeleton stubs"
)]

mod plugin;
mod renderer;
mod smoke;
mod textures;

mod arena;
mod chunk_table;
mod path;
mod targets;

pub use plugin::{
    DebugDestroyDevice, GpuFailed, GpuReady, Render, RenderInfo, RenderPlugin, RenderSettings,
    RenderSystems, RequestScreenshot, ScreenshotSaved,
};
pub use renderer::{RenderStats, RendererConfig, RendererError, ViewParams, VoxelRenderer};
pub use smoke::RendererSmokeTest;
pub use textures::{BLOCK_TEXTURE_SIZE, BlockTextureData, generate_block_textures};

/// Format of the main color target (screenshots read it).
pub const MAIN_COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Depth format (reverse-Z, `CompareFunction::Greater`, clear 0).
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// Common items.
pub mod prelude {
    pub use crate::{RenderInfo, RenderPlugin, RenderSettings, RequestScreenshot, ScreenshotSaved};
}
