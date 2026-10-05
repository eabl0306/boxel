use crate::{AdapterCaps, DeviceClass, SelectionReport, Tier};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Shared health flags set from wgpu callbacks (any thread) and polled by the renderer.
#[derive(Debug, Default)]
pub struct GpuHealth {
    lost: AtomicBool,
    lost_reason: Mutex<Option<String>>,
    errors: AtomicU32,
    last_error: Mutex<Option<String>>,
}

impl GpuHealth {
    /// Device lost has been reported.
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }
    /// Marks the device lost (first reason wins).
    pub fn mark_lost(&self, reason: String) {
        let mut r = self
            .lost_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if r.is_none() {
            *r = Some(reason);
        }
        self.lost.store(true, Ordering::Release);
    }
    /// Reason given at loss.
    pub fn lost_reason(&self) -> Option<String> {
        self.lost_reason
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    /// Records an uncaptured error.
    pub fn record_error(&self, message: String) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        *self
            .last_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(message);
    }
    /// Uncaptured errors so far.
    pub fn error_count(&self) -> u32 {
        self.errors.load(Ordering::Relaxed)
    }
    /// Last uncaptured error.
    pub fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// Installs `Device::on_uncaptured_error` (log at error level + `health.record_error`; never
/// panics, unlike wgpu's default handler) and `Device::set_device_lost_callback`
/// (`health.mark_lost`). Called by `select_gpu` right after `request_device`, before smoke tests.
pub fn install_device_callbacks(device: &wgpu::Device, health: &Arc<GpuHealth>) {
    todo!("WP3")
}

/// The selected GPU: everything the renderer needs.
#[derive(Debug)]
pub struct GpuContext {
    /// The process instance.
    pub instance: wgpu::Instance,
    /// Chosen adapter.
    pub adapter: wgpu::Adapter,
    /// Device created with [`crate::tier_request`] for `tier`.
    pub device: wgpu::Device,
    /// Its queue.
    pub queue: wgpu::Queue,
    /// Adapter capabilities.
    pub caps: AdapterCaps,
    /// Selected tier.
    pub tier: Tier,
    /// Device class.
    pub class: DeviceClass,
    /// Color format the smoke test proved for presentation: the chosen surface format
    /// ([`crate::choose_surface_format`]) when selected with a surface, else `Rgba8UnormSrgb`.
    /// `configure_surface` and the renderer's blit use exactly this format.
    pub output_format: wgpu::TextureFormat,
    /// Health flags (shared with the device callbacks).
    pub health: Arc<GpuHealth>,
    /// Full decision trace.
    pub report: SelectionReport,
}

impl GpuContext {
    /// One line for logs and scripts:
    /// `backend=Vulkan tier=High class=Discrete adapter="NVIDIA GeForce RTX 5050 Laptop GPU" driver="595.91.07"`.
    pub fn summary_line(&self) -> String {
        format!(
            "backend={:?} tier={} class={:?} adapter={:?} driver={:?}",
            self.caps.info.backend,
            self.tier,
            self.class,
            self.caps.info.name,
            self.caps.info.driver_info
        )
    }
}
