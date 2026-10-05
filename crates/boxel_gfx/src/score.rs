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
    let tier_score = match tier {
        Tier::High => 3000,
        Tier::Mid => 2000,
        Tier::Low => 1000,
    };
    let low_power = power == wgpu::PowerPreference::LowPower;
    let class_score = match caps.class() {
        DeviceClass::Discrete if low_power => 300,
        DeviceClass::Discrete => 400,
        DeviceClass::Integrated if low_power => 400,
        DeviceClass::Integrated => 300,
        DeviceClass::Other => 200,
        DeviceClass::Virtual => 100,
        DeviceClass::Software => -5000,
    };
    let backend_score = match caps.info.backend {
        wgpu::Backend::Vulkan | wgpu::Backend::Metal => 100,
        wgpu::Backend::Dx12 => 90,
        _ => 0,
    };
    let lkg_score = match lkg {
        Some(LkgStatus::Good) => 50,
        Some(LkgStatus::Suspect) => -500,
        Some(LkgStatus::Pending | LkgStatus::Bad) | None => 0,
    };
    tier_score + class_score + backend_score + lkg_score
}
