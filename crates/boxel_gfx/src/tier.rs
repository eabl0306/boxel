use serde::{Deserialize, Serialize};

/// Render-path capability tier (ADR 0002). Ordered: `Low < Mid < High`.
///
/// - `High`: GPU-driven. Compute frustum/bucket culling writes indirect args;
///   `multi_draw_indexed_indirect_count`; vertex pulling from storage buffers.
/// - `Mid`: vertex pulling from storage buffers, CPU culling, one direct draw per visible bucket.
/// - `Low`: no storage buffers in any stage. Quads are instance-step vertex attributes, chunk
///   origin via immediates. Desktop GL 3.3 / GLES 3.0 class devices.
///
/// "Software" is not a tier: it is a [`crate::DeviceClass`] (CPU rasterizers) that ranks last
/// and is capped at `Mid` by default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Instance-attribute quads, immediates, CPU culling.
    Low = 1,
    /// Vertex pulling, CPU culling, direct draws.
    Mid = 2,
    /// GPU culling + multi-draw-indirect-count.
    High = 3,
}

impl Tier {
    /// All tiers, best first.
    pub const DESCENDING: [Tier; 3] = [Tier::High, Tier::Mid, Tier::Low];

    /// Next lower tier.
    pub const fn lower(self) -> Option<Tier> {
        match self {
            Tier::High => Some(Tier::Mid),
            Tier::Mid => Some(Tier::Low),
            Tier::Low => None,
        }
    }

    /// `"high" | "mid" | "low"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Tier::High => "high",
            Tier::Mid => "mid",
            Tier::Low => "low",
        }
    }

    /// Case-insensitive parse of `high|mid|low`.
    pub fn parse(s: &str) -> Option<Tier> {
        match s.trim().to_ascii_lowercase().as_str() {
            "high" => Some(Tier::High),
            "mid" => Some(Tier::Mid),
            "low" => Some(Tier::Low),
            _ => None,
        }
    }
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Tier::High => "High",
            Tier::Mid => "Mid",
            Tier::Low => "Low",
        })
    }
}

/// Exact features and limits requested from `request_device` for a tier.
#[derive(Clone, Debug)]
pub struct TierRequest {
    /// Required features.
    pub features: wgpu::Features,
    /// Required limits.
    pub limits: wgpu::Limits,
}

/// Engine floors per tier (frozen). [`crate::classify`] checks exactly these against the
/// adapter and [`tier_request`] never asks for more than the adapter has, so a tier that
/// classifies always gets a device request the adapter can satisfy.
pub mod floors {
    /// All tiers: immediates bytes (Low passes the chunk origin through them).
    pub const IMMEDIATE_SIZE: u32 = 16;
    /// All tiers: vertex buffer slots. High and Mid use one (the instance-step `DrawRef`); Low
    /// uses two, both instance-step: the quad and its light palette entry (ADR 0015; an
    /// attribute, not a binding, so ADR 0011 holds).
    pub const VERTEX_BUFFERS: u32 = 2;
    /// All tiers: layers of the block texture array.
    pub const TEXTURE_ARRAY_LAYERS: u32 = 256;
    /// Mid/High: storage buffers per stage (quad arena + chunk table in the vertex stage; the
    /// cull shader binds 4).
    pub const STORAGE_BUFFERS_PER_STAGE: u32 = 4;
    /// Mid/High: storage binding size (quad arena).
    pub const STORAGE_BINDING_SIZE: u64 = 128 << 20;
    /// High: `cull.wgsl` uses `@workgroup_size(64)`.
    pub const WORKGROUP_SIZE_X: u32 = 64;
    /// High: invocations per workgroup.
    pub const INVOCATIONS_PER_WORKGROUP: u32 = 64;
    /// High: workgroups per dispatch dimension (1024 x 64 = 65536 chunk slots).
    pub const WORKGROUPS_PER_DIMENSION: u32 = 1024;
}

/// Features/limits for `tier` on an adapter with `adapter_limits` (frozen rules):
/// - limits = `Limits::default().or_worse_values_from(adapter)` (never better than the adapter
///   in any field), then the fields the engine sizes from the device are raised to the
///   adapter's own value: `max_buffer_size`, `max_texture_array_layers`,
///   `max_immediate_size`, and for Mid/High `max_storage_buffer_binding_size` and
///   `max_storage_buffers_per_shader_stage`;
/// - features: `IMMEDIATES` on every tier (one device shape per tier); High adds
///   `MULTI_DRAW_INDIRECT_COUNT | INDIRECT_FIRST_INSTANCE`.
///
/// Postcondition: `limits.check_limits(adapter_limits)` (debug-asserted; unit-tested with
/// synthetic adapters sitting exactly at every [`floors`] value). The renderer reads the
/// granted values back with `Device::limits()`.
pub fn tier_request(tier: Tier, adapter_limits: &wgpu::Limits) -> TierRequest {
    let a = adapter_limits;
    let mut limits = wgpu::Limits::default().or_worse_values_from(a);
    limits.max_buffer_size = a.max_buffer_size;
    limits.max_texture_array_layers = a.max_texture_array_layers;
    limits.max_immediate_size = a.max_immediate_size;
    if tier >= Tier::Mid {
        limits.max_storage_buffer_binding_size = a.max_storage_buffer_binding_size;
        limits.max_storage_buffers_per_shader_stage = a.max_storage_buffers_per_shader_stage;
    }
    let mut features = wgpu::Features::IMMEDIATES;
    if tier == Tier::High {
        features |=
            wgpu::Features::MULTI_DRAW_INDIRECT_COUNT | wgpu::Features::INDIRECT_FIRST_INSTANCE;
    }
    debug_assert!(
        limits.check_limits(a),
        "tier_request exceeds the adapter limits"
    );
    TierRequest { features, limits }
}
