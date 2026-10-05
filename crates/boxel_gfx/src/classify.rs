use crate::{AdapterCaps, Tier};

/// Policy knobs for classification (defaults are the frozen M0 policy).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TierPolicy {
    /// Allow `High` on DX12. False until a DX12 device passes the cross-tier image test
    /// (`BOXEL_DX12_HIGH=1` sets it for experiments).
    pub dx12_high: bool,
    /// Highest tier for `DeviceClass::Software` adapters (default `Mid`: GPU culling on a CPU
    /// rasterizer is slower than CPU culling).
    pub software_max: Tier,
}

impl Default for TierPolicy {
    fn default() -> Self {
        Self {
            dx12_high: false,
            software_max: Tier::Mid,
        }
    }
}

/// Result of [`classify`]: the best tier an adapter *claims*, before smoke tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Classification {
    /// Best supported tier, `None` if not even `Low`.
    pub max_tier: Option<Tier>,
    /// One human-readable line per rejected higher tier (`"High: no MULTI_DRAW_INDIRECT_COUNT"`).
    pub reasons: Vec<String>,
}

/// Classifies an adapter (frozen rules, evaluated top-down; every failed check adds a reason).
/// Every numeric threshold is a [`crate::floors`] constant, the same ones [`crate::tier_request`]
/// relies on, so classification and the device request cannot disagree.
///
/// `Low` requires: backend not `Noop`/`BrowserWebGpu`; `features ⊇ IMMEDIATES`;
/// `max_immediate_size >= floors::IMMEDIATE_SIZE`; `max_vertex_buffers >= floors::VERTEX_BUFFERS`;
/// `max_texture_array_layers >= floors::TEXTURE_ARRAY_LAYERS`.
///
/// `Mid` requires `Low` and: `downlevel.flags ⊇ VERTEX_STORAGE`;
/// `max_storage_buffers_per_shader_stage >= floors::STORAGE_BUFFERS_PER_STAGE`;
/// `max_storage_buffer_binding_size >= floors::STORAGE_BINDING_SIZE`; on GL: `gl_version`
/// parsed and `supports_storage_and_compute()` (NVIDIA's 3.3 context claims
/// `VERTEX_STORAGE/COMPUTE_SHADERS` but naga emits GLSL 330 without SSBOs).
///
/// `High` requires `Mid` and: `downlevel.flags ⊇ COMPUTE_SHADERS | INDIRECT_EXECUTION`;
/// `features ⊇ MULTI_DRAW_INDIRECT_COUNT | INDIRECT_FIRST_INSTANCE`;
/// `max_compute_workgroup_size_x >= floors::WORKGROUP_SIZE_X`;
/// `max_compute_invocations_per_workgroup >= floors::INVOCATIONS_PER_WORKGROUP`;
/// `max_compute_workgroups_per_dimension >= floors::WORKGROUPS_PER_DIMENSION`; backend
/// `Vulkan` (or `Dx12` if `policy.dx12_high`).
///
/// Finally `DeviceClass::Software` is capped at `policy.software_max`.
pub fn classify(caps: &AdapterCaps, policy: TierPolicy) -> Classification {
    todo!("WP3")
}
