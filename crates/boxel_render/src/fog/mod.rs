//! Optional cubic fog and clouds (issue #47, ADR 0030): a ray march over layers of cells after
//! the main pass, composited over the main color. Without fog input the renderer creates none
//! of it (no pipeline, no target, no sampled depth) and every frame is what it was before.
//!
//! - [`FogSettings`]: the layers and how they are drawn, the game's style ([`boxel_fog`] holds
//!   the layer types and their validation). A layer can go on past its cells as a haze of its
//!   mean density ([`FogLayer::haze`]), as far as the terrain is drawn (the far LOD's horizon
//!   while it draws, for a layer faded like the scene).
//! - [`FogInput`]: what one frame of fog needs; [`crate::VoxelRenderer::set_fog`] takes it.

mod pass;
mod plugin;

pub use pass::frame_layers;
pub(crate) use pass::{FogPass, read_words};
#[doc(hidden)]
pub use pass::{debug_coverage, debug_walk};
pub use plugin::{FogClock, FogPlugin};

pub use boxel_fog::{
    FogError, FogFade, FogLayer, FogNoise, FogResolution, FogTime, NoiseDims,
    density_for_visibility,
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
    /// Cells a ray visits at most over all layers (1 to 256, default
    /// [`boxel_fog::DEFAULT_MAX_STEPS`]); past it the rest of each layer's cells is its mean
    /// density at each height (its haze profile, [`boxel_fog::HazeProfile`]). More shows the
    /// cubes deeper into thick fog and costs more on weak GPUs.
    pub max_steps: u32,
}

impl Default for FogSettings {
    /// Enabled, no layers, half resolution, [`boxel_fog::DEFAULT_MAX_STEPS`] steps.
    fn default() -> Self {
        Self {
            enabled: true,
            layers: Vec::new(),
            resolution: FogResolution::Half,
            max_steps: boxel_fog::DEFAULT_MAX_STEPS,
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
    /// The fog clock: ticks (64 a second) and the fraction of the next one.
    pub time: FogTime,
}
