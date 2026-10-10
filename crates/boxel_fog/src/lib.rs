//! `boxel_fog`: cubic fog and cloud layers (issue #47, ADR 0030). Pure CPU and plain data: no
//! GPU, no ECS. The renderer (`boxel_render::fog`) draws them; this crate holds what it and the
//! tests share:
//!
//! - [`config`]: the style of a set of layers ([`FogLayer`]) and [`validate`]. Every value is the
//!   game's (the engine fixes no look; presets live with the content).
//! - [`noise`]: the integer noise that gives each cell its coverage, bit for bit what the shader
//!   computes, and a direct `i128` reference of it.
//! - [`layer`]: a layer reduced to small integers relative to the render origin for one frame
//!   ([`LayerParams`]), and the camera rays ([`RayBasis`]).
//! - [`march`]: the shader's march in `f32`, the reference of the GPU tests.
//! - [`pack`]: how the march stores its result in one texel.
//!
//! A cell's density is `density * coverage(cell - wind * time)`: the cells never move, the wind
//! only changes which ones are full. The time is a tick count of the fixed step (64 a second),
//! never a float, so every machine and tier sees the same cells.

pub mod config;
pub mod layer;
pub mod march;
pub mod noise;
pub mod pack;

pub use config::{
    DEFAULT_FACE_SHADE, DEFAULT_MAX_STEPS, FogError, FogFade, FogLayer, FogNoise, FogResolution,
    MAX_FOG_DISTANCE, NoiseDims, density_for_visibility, validate,
};
pub use layer::{LayerParams, RayBasis, TICKS_PER_SECOND, mean_coverage};
pub use noise::{COVERAGE_ONE, Coverage};

/// Most layers a set may have.
pub const MAX_FOG_LAYERS: usize = 4;
