//! `boxel_gfx`: the graphics layer. wgpu 30 is the only RHI (ADR 0001); this crate is boxel's
//! *policy* on top of it, with no ECS and no voxel knowledge:
//!
//! - [`GpuPreferences`]: overrides with precedence CLI > `BOXEL_*` env > `WGPU_*` env > config file
//!   > automatic.
//! - [`create_instance`]: one `wgpu::Instance` per process (display handle for windowed apps),
//!   instance-flag policy ([`instance_flags`]), `catch_unwind` around `Instance::new` with a retry
//!   without GL.
//! - [`AdapterCaps`] + [`classify`] + [`score`]: capability tiers from features, downlevel flags,
//!   limits and the parsed GL version, never from the backend name alone (ADR 0002).
//! - [`select_gpu`]: per (adapter, tier) candidate in score order, `request_device`, install error
//!   and device-lost sinks, run a [`SmokeTest`] inside Validation + Internal + `OutOfMemory` error
//!   scopes, fall back to the next candidate. Every decision is recorded in a [`SelectionReport`].
//! - [`LkgStore`]: persisted last-known-good / known-bad (adapter, tier) pairs. Crash detection
//!   only: a pair becomes `Bad` after two unexplained process deaths during GPU bring-up.
//! - Surface helpers ([`configure_surface`], [`acquire_frame`]) and readback ([`read_texture_rgba8`]).

mod caps;
mod classify;
mod context;
mod instance;
mod lkg;
mod prefs;
mod readback;
mod report;
mod score;
mod select;
mod smoke;
mod surface;
mod tier;

/// Vulkan escape hatch. Only on targets where wgpu-hal compiles its Vulkan backend from the
/// `vulkan` feature (Windows, Linux, Android, FreeBSD); on Apple targets it would need wgpu's
/// `vulkan-portability`, which boxel does not enable.
#[cfg(all(
    feature = "hal",
    any(
        windows,
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd"
    )
))]
pub mod hal;

pub use caps::{AdapterCaps, AdapterSummary, DeviceClass, GlVersion};
pub use classify::{Classification, TierPolicy, classify};
pub use context::{GpuContext, GpuHealth, install_device_callbacks};
pub use instance::{
    CreatedInstance, create_gl_instance, create_headless_instance, create_instance,
    default_backends, instance_flags,
};
pub use lkg::{
    LastKnownGood, LkgEntry, LkgError, LkgStatus, LkgStore, PENDING_GRACE_SECS, STRIKES_FOR_BAD,
    WGPU_VERSION, build_fingerprint, install_lkg_panic_hook,
};
pub use prefs::{CLI_HELP, GpuPreferences, PRESENT_MODES, PrefsError, PresentModePref};
pub use readback::{ImageDiff, RgbaImage, read_texture_rgba8};
pub use report::{Attempt, AttemptOutcome, CandidateReport, SelectionReport};
pub use score::score;
pub use select::{Exclusion, SelectError, SelectRequest, init_headless, select_gpu};
pub use smoke::{
    BasicSmokeTest, SmokeContext, SmokeLevel, SmokeTest, run_in_error_scopes, submit_and_wait,
};
pub use surface::{
    FrameAcquire, acquire_frame, choose_alpha_mode, choose_present_mode, choose_surface_format,
    configure_surface,
};
pub use tier::{Tier, TierRequest, floors, tier_request};

pub use wgpu;

/// True when `BOXEL_REQUIRE_GPU=1`: GPU tests must fail instead of skipping when no adapter or
/// device is available (CI sets it on Linux with lavapipe/llvmpipe).
pub fn gpu_tests_required() -> bool {
    std::env::var("BOXEL_REQUIRE_GPU").is_ok_and(|v| v == "1")
}

/// Joins a multi-line message (parser errors, wgpu errors) into one trimmed line for reports.
pub(crate) fn one_line(message: &str) -> String {
    message
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
