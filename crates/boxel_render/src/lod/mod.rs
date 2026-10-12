//! Far level of detail, the render half (ADR 0031): coarse terrain nodes drawn beyond the
//! full-detail view distance, like the Distant Horizons mod. The world side
//! ([`boxel_world::lod`]) plans the nodes, builds their meshes and decides which octants of each
//! node finer detail draws; the renderer keeps them in an arena of its own and draws them in the
//! main pass, right after the opaque and cutout terrain: on Mid and Low (and integrated GPUs)
//! one instanced draw per run of the face buckets of a visible node that can face the camera
//! (`lod.wgsl`), on High from lists the GPU culls (`lod_cull.wgsl`, against the depth of the
//! terrain drawn before it while that hides something, and `lod_pull.wgsl`), with the same
//! pixels. Both leave out the skirts no seam shows.
//!
//! Without LOD input the renderer creates none of it (no pipeline, no arena) and every frame is
//! what it was before. With it ([`crate::VoxelRenderer::set_lod`]) and the LOD on, the main
//! frustum's far plane and the terrain's distance fog move out to the horizon (the same rule as
//! at the view distance, [`crate::DistanceFog`]), so full detail and LOD fade together into the
//! sky; the fog layers keep their own ranges.
//!
//! - [`LodPlugin`]: the far LOD of an app, world side included (not in any default set of
//!   plugins).
//! - [`LodInput`]: one frame of LOD for the renderer: the settings, the horizon and the world
//!   side's [`LodOp`]s.
//! - [`LodReport`]: what the renderer sends back (refused meshes and the free arena).

mod pass;
mod plugin;

pub(crate) use pass::{GpuInputs, LodPass};
pub use plugin::LodPlugin;
pub(crate) use plugin::{LodRenderer, lod_far};

pub use boxel_world::lod::{
    LodFeedback, LodFocus, LodOp, LodOps, LodResync, LodSettings, LodStats, LodSystems,
};

use crate::DistanceFog;
use boxel_voxel::ChunkKey;

/// One frame of far LOD for [`crate::VoxelRenderer::set_lod`].
#[derive(Clone, Debug)]
pub struct LodInput {
    /// The settings (valid: [`LodSettings::validate`]). The renderer reads `enabled`,
    /// `arena_quads` (a change rebuilds the LOD pass empty) and `debug_tint`.
    pub settings: LodSettings,
    /// The terrain's distance fog while the LOD draws, at the horizon instead of the view
    /// distance ([`DistanceFog::range`]): the render plugin's `RenderSettings::distance_fog`.
    /// [`LodInput::new`] gives the default.
    pub fog: DistanceFog,
    /// The horizon in blocks ([`LodStats::horizon`]), `None` while the world side draws nothing
    /// (it is off, or the keep region reaches the preset's horizon). Without it, or with
    /// `settings.enabled` false, the renderer draws no node and frames are what they are
    /// without LOD.
    pub horizon: Option<f32>,
    /// The frame's ops, in order ([`boxel_world::lod::LodOps`]).
    pub ops: Vec<LodOp>,
    /// Drop every node before the ops: the world side sends them all again (it was told with a
    /// [`LodResync`]).
    pub resync: bool,
}

impl LodInput {
    /// Settings and horizon with no op, and the default distance fog.
    pub fn new(settings: LodSettings, horizon: Option<f32>) -> Self {
        Self {
            settings,
            fog: DistanceFog::default(),
            horizon,
            ops: Vec::new(),
            resync: false,
        }
    }

    /// The horizon when this input draws LOD: `settings.enabled` and a positive horizon.
    pub fn far(&self) -> Option<f32> {
        far(&self.settings, self.horizon)
    }
}

/// The horizon when `settings` and `horizon` draw LOD.
fn far(settings: &LodSettings, horizon: Option<f32>) -> Option<f32> {
    horizon.filter(|h| settings.enabled && h.is_finite() && *h > 0.0)
}

/// What the LOD pass reports after applying an input, for [`LodFeedback`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LodReport {
    /// Node meshes refused since the last report: the LOD arena or its node table was full (or
    /// the key was no LOD node). A refused node is not on the renderer, its older mesh included;
    /// later `Mask` ops for it are ignored.
    pub refused: Vec<ChunkKey>,
    /// Free quads in the LOD arena.
    pub arena_free_quads: u64,
}

/// True if the far LOD can be drawn on an adapter with `info`: not on GL backends (no clip
/// control, so far depth z-fights, ADR 0007).
pub fn supported(info: &wgpu::AdapterInfo) -> bool {
    info.backend != wgpu::Backend::Gl
}

/// True if High's GPU-driven LOD path (the node cull, the depth pyramid, one indirect draw list
/// per segment) is worth it on the adapter of `info`: not on an integrated GPU. On the Radeon
/// 780M (RADV) it measured slower than the instanced path, about 0.2 ms for the indirect draws
/// and as much for the pyramid; on the discrete RTX it saves most of the LOD's cost. Both draw
/// the same pixels.
pub fn gpu_driven_pays(info: &wgpu::AdapterInfo) -> bool {
    info.device_type != wgpu::DeviceType::IntegratedGpu
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lod_draws_only_when_enabled_with_a_horizon() {
        let on = LodSettings::default();
        let off = LodSettings {
            enabled: false,
            ..LodSettings::default()
        };
        assert_eq!(
            LodInput::new(on.clone(), Some(12288.0)).far(),
            Some(12288.0)
        );
        assert_eq!(LodInput::new(on.clone(), None).far(), None);
        assert_eq!(LodInput::new(on.clone(), Some(0.0)).far(), None);
        assert_eq!(LodInput::new(on, Some(f32::NAN)).far(), None);
        assert_eq!(LodInput::new(off, Some(12288.0)).far(), None);
    }

    #[test]
    fn lod_is_off_on_gl() {
        let info = |backend| wgpu::AdapterInfo::new(wgpu::DeviceType::Other, backend);
        assert!(!supported(&info(wgpu::Backend::Gl)));
        assert!(supported(&info(wgpu::Backend::Vulkan)));
        assert!(supported(&info(wgpu::Backend::Metal)));
    }

    #[test]
    fn integrated_gpus_draw_the_lod_instanced() {
        let info = |kind| wgpu::AdapterInfo::new(kind, wgpu::Backend::Vulkan);
        assert!(!gpu_driven_pays(&info(wgpu::DeviceType::IntegratedGpu)));
        assert!(gpu_driven_pays(&info(wgpu::DeviceType::DiscreteGpu)));
        assert!(gpu_driven_pays(&info(wgpu::DeviceType::Other)));
    }
}
