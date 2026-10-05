//! Vulkan escape hatch (feature `hal`). Everything here is `unsafe` and Vulkan-only; it exists so
//! future Vulkan-only features (async compute experiments, extensions wgpu lacks) do not require a
//! second RHI (ADR 0001). Uses the `wgpu::hal` re-export (same wgpu-hal =30.0.1).
#![expect(unsafe_code, reason = "raw Vulkan interop through wgpu-hal")]

/// Runs `f` with the raw `ash::Device` handle wrapper if `device` is a Vulkan device.
///
/// # Safety
/// The caller must not destroy wgpu-owned objects nor leave the device in a state wgpu does not
/// track (see `wgpu::Device::as_hal`).
pub unsafe fn with_vulkan_device<R>(
    device: &wgpu::Device,
    f: impl FnOnce(&wgpu::hal::vulkan::Device) -> R,
) -> Option<R> {
    // SAFETY: forwarded to the caller's contract above.
    let hal = unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }?;
    Some(f(&hal))
}
