//! Optional cubic fog and clouds (issue #47, ADR 0030): a ray march over layers of cells after
//! the main pass, composited over the main color. Without fog input the renderer creates none
//! of it (no pipeline, no target, no sampled depth) and every frame is what it was before.
//!
//! - [`FogSettings`]: the layers and how they are drawn, the game's style ([`boxel_fog`] holds
//!   the layer types and their validation).
//! - [`FogInput`]: what one frame of fog needs; [`crate::VoxelRenderer::set_fog`] takes it.

mod pass;
mod plugin;

#[doc(hidden)]
pub use pass::debug_coverage;
pub use pass::frame_layers;
pub(crate) use pass::{FogPass, read_words};
pub use plugin::{FogClock, FogPlugin};

pub use boxel_fog::{
    FogError, FogFade, FogLayer, FogNoise, FogResolution, NoiseDims, density_for_visibility,
};

use boxel_ecs::prelude::*;
use std::sync::Arc;

/// The fog of an app: its layers and how they are drawn. Every value is the game's (the engine
/// fixes no look). Change it at runtime through `ResMut<FogSettings>`; an invalid change is
/// logged and the last valid settings stay.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct FogSettings {
    /// Draw the fog. Off, the passes are skipped (the renderer keeps its fog resources).
    pub enabled: bool,
    /// The layers, at most [`boxel_fog::MAX_FOG_LAYERS`], bands disjoint (any order).
    pub layers: Vec<FogLayer>,
    /// Resolution of the march (default half).
    pub resolution: FogResolution,
    /// Cells a ray visits at most over all layers (1 to 256, default 64); past it each layer
    /// uses its mean density.
    pub max_steps: u32,
}

impl Default for FogSettings {
    /// Enabled, no layers, half resolution, 64 steps.
    fn default() -> Self {
        Self {
            enabled: true,
            layers: Vec::new(),
            resolution: FogResolution::Half,
            max_steps: 64,
        }
    }
}

impl FogSettings {
    /// These layers with the default settings.
    pub fn new(layers: Vec<FogLayer>) -> Self {
        Self {
            layers,
            ..Self::default()
        }
    }

    /// Checks the layers and the step budget ([`boxel_fog::validate`]).
    pub fn validate(&self) -> Result<(), FogError> {
        boxel_fog::validate(&self.layers, self.max_steps)
    }
}

/// One frame of fog for [`crate::VoxelRenderer::set_fog`].
#[derive(Clone, Debug)]
pub struct FogInput {
    /// The settings (valid: [`FogSettings::validate`]).
    pub settings: Arc<FogSettings>,
    /// Ticks of the fog clock (the wind moves once a tick, 64 a second).
    pub tick: u64,
}
