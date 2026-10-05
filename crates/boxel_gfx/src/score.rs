use crate::{AdapterCaps, DeviceClass, LkgStatus, Tier};

/// Candidate score; higher is tried first (frozen):
/// - tier: High 3000, Mid 2000, Low 1000;
/// - class: with `PowerPreference::HighPerformance` (default) Discrete +400, Integrated +300;
///   with `LowPower` Integrated +400, Discrete +300; Other +200 (GL adapters), Virtual +100,
///   Software -5000 (always last);
/// - backend: Vulkan +100, Metal +100, Dx12 +90, Gl +0;
/// - last-known-good: +50 if `Good`, -500 if `Suspect` (below every other adapter of the
///   same tier, still above the next tier); `Pending`/`Bad`/`None`: 0 (`Bad` pairs are
///   skipped by `select_gpu` anyway).
///
/// Ties keep enumeration order (stable sort).
pub fn score(
    caps: &AdapterCaps,
    tier: Tier,
    power: wgpu::PowerPreference,
    lkg: Option<LkgStatus>,
) -> i64 {
    let _ = DeviceClass::Other;
    todo!("WP3")
}
