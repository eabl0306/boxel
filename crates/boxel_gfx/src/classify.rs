use crate::{AdapterCaps, DeviceClass, Tier, floors};

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
///
/// Reasons: one line per tier above the result that fails one of its own checks, in ascending
/// tier order, as `"<Tier>: <failed check>, <failed check>"`; the software cap adds
/// `"<Tier>: software adapters capped at <cap>"` for every tier it removes.
pub fn classify(caps: &AdapterCaps, policy: TierPolicy) -> Classification {
    let per_tier = [
        (Tier::Low, low_failures(caps)),
        (Tier::Mid, mid_failures(caps)),
        (Tier::High, high_failures(caps, policy)),
    ];
    let mut max_tier = None;
    let mut reasons = Vec::new();
    for (tier, failures) in per_tier {
        if failures.is_empty() {
            // A tier only counts if every lower tier passed too (evaluated top-down).
            if max_tier == tier.lower() {
                max_tier = Some(tier);
            }
        } else {
            reasons.push(format!("{tier}: {}", failures.join(", ")));
        }
    }
    if caps.class() == DeviceClass::Software
        && let Some(max) = max_tier
        && max > policy.software_max
    {
        for tier in [Tier::Mid, Tier::High] {
            if tier > policy.software_max && tier <= max {
                reasons.push(format!(
                    "{tier}: software adapters capped at {}",
                    policy.software_max
                ));
            }
        }
        max_tier = Some(policy.software_max);
    }
    Classification { max_tier, reasons }
}

/// Failed `Low` checks (empty = passes).
fn low_failures(caps: &AdapterCaps) -> Vec<String> {
    let l = &caps.limits;
    let mut out = Vec::new();
    if matches!(
        caps.info.backend,
        wgpu::Backend::Noop | wgpu::Backend::BrowserWebGpu
    ) {
        out.push(format!(
            "backend {:?} is not a GPU backend",
            caps.info.backend
        ));
    }
    if !caps.features.contains(wgpu::Features::IMMEDIATES) {
        out.push("no IMMEDIATES".to_string());
    }
    at_least(
        &mut out,
        "max_immediate_size",
        l.max_immediate_size,
        floors::IMMEDIATE_SIZE,
    );
    at_least(
        &mut out,
        "max_vertex_buffers",
        l.max_vertex_buffers,
        floors::VERTEX_BUFFERS,
    );
    at_least(
        &mut out,
        "max_texture_array_layers",
        l.max_texture_array_layers,
        floors::TEXTURE_ARRAY_LAYERS,
    );
    out
}

/// Failed `Mid` checks on top of `Low` (empty = passes).
fn mid_failures(caps: &AdapterCaps) -> Vec<String> {
    let l = &caps.limits;
    let mut out = Vec::new();
    if !caps
        .downlevel
        .flags
        .contains(wgpu::DownlevelFlags::VERTEX_STORAGE)
    {
        out.push("no VERTEX_STORAGE".to_string());
    }
    at_least(
        &mut out,
        "max_storage_buffers_per_shader_stage",
        l.max_storage_buffers_per_shader_stage,
        floors::STORAGE_BUFFERS_PER_STAGE,
    );
    at_least(
        &mut out,
        "max_storage_buffer_binding_size",
        l.max_storage_buffer_binding_size,
        floors::STORAGE_BINDING_SIZE,
    );
    if caps.info.backend == wgpu::Backend::Gl {
        match caps.gl_version {
            None => out.push(format!(
                "GL version unknown (driver_info {:?})",
                caps.info.driver_info
            )),
            Some(v) if !v.supports_storage_and_compute() => {
                let (es, floor) = if v.es { ("ES ", "3.1") } else { ("", "4.3") };
                out.push(format!(
                    "GL {es}{}.{} < {floor} (no SSBO/compute in GLSL {es}{}{}0)",
                    v.major, v.minor, v.major, v.minor
                ));
            }
            Some(_) => {}
        }
    }
    out
}

/// Failed `High` checks on top of `Mid` (empty = passes).
fn high_failures(caps: &AdapterCaps, policy: TierPolicy) -> Vec<String> {
    let l = &caps.limits;
    let mut out = Vec::new();
    for (flag, name) in [
        (wgpu::DownlevelFlags::COMPUTE_SHADERS, "COMPUTE_SHADERS"),
        (
            wgpu::DownlevelFlags::INDIRECT_EXECUTION,
            "INDIRECT_EXECUTION",
        ),
    ] {
        if !caps.downlevel.flags.contains(flag) {
            out.push(format!("no {name}"));
        }
    }
    for (feature, name) in [
        (
            wgpu::Features::MULTI_DRAW_INDIRECT_COUNT,
            "MULTI_DRAW_INDIRECT_COUNT",
        ),
        (
            wgpu::Features::INDIRECT_FIRST_INSTANCE,
            "INDIRECT_FIRST_INSTANCE",
        ),
    ] {
        if !caps.features.contains(feature) {
            out.push(format!("no {name}"));
        }
    }
    at_least(
        &mut out,
        "max_compute_workgroup_size_x",
        l.max_compute_workgroup_size_x,
        floors::WORKGROUP_SIZE_X,
    );
    at_least(
        &mut out,
        "max_compute_invocations_per_workgroup",
        l.max_compute_invocations_per_workgroup,
        floors::INVOCATIONS_PER_WORKGROUP,
    );
    at_least(
        &mut out,
        "max_compute_workgroups_per_dimension",
        l.max_compute_workgroups_per_dimension,
        floors::WORKGROUPS_PER_DIMENSION,
    );
    let backend = caps.info.backend;
    if backend == wgpu::Backend::Dx12 && !policy.dx12_high {
        out.push("Dx12 not enabled for High (TierPolicy::dx12_high)".to_string());
    } else if !matches!(backend, wgpu::Backend::Vulkan | wgpu::Backend::Dx12) {
        out.push(format!("backend {backend:?} (High needs Vulkan)"));
    }
    out
}

/// Pushes `"{name} {value} < {floor}"` when `value < floor`.
fn at_least<T: PartialOrd + std::fmt::Display>(
    out: &mut Vec<String>,
    name: &str,
    value: T,
    floor: T,
) {
    if value < floor {
        out.push(format!("{name} {value} < {floor}"));
    }
}
