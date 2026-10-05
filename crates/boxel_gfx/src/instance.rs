use crate::GpuPreferences;
use wgpu::wgt::WgpuHasDisplayHandle;

/// Per-OS default instance backends (frozen): Linux/BSD `VULKAN | GL`; Windows `VULKAN | DX12`
/// (GL only on request); macOS `METAL`.
pub fn default_backends() -> wgpu::Backends {
    if cfg!(target_os = "macos") {
        wgpu::Backends::METAL
    } else if cfg!(windows) {
        wgpu::Backends::VULKAN | wgpu::Backends::DX12
    } else {
        wgpu::Backends::VULKAN | wgpu::Backends::GL
    }
}

/// Instance flag policy (frozen, ADR 0010):
/// - start from `InstanceFlags::empty()`;
/// - `DEBUG | VALIDATION` if `prefs.validation` (default: `cfg!(debug_assertions)`);
/// - `VALIDATION_INDIRECT_CALL` is never set by boxel: boxel never reads draw builtins affected by
///   it and only uses the unvalidated `*_count` indirect variant; the flag breaks device creation on
///   NVIDIA GL 3.3 and costs CPU per sub-draw;
/// - finally `InstanceFlags::with_env()` so `WGPU_VALIDATION`, `WGPU_DEBUG`,
///   `WGPU_VALIDATION_INDIRECT_CALL`, `WGPU_GPU_BASED_VALIDATION`... still work for debugging.
pub fn instance_flags(prefs: &GpuPreferences) -> wgpu::InstanceFlags {
    todo!("WP3")
}

/// An instance plus what was actually used to create it.
#[derive(Debug)]
pub struct CreatedInstance {
    /// The instance (clone it freely; it is reference counted).
    pub instance: wgpu::Instance,
    /// Backends requested.
    pub backends: wgpu::Backends,
    /// Flags used.
    pub flags: wgpu::InstanceFlags,
    /// Notes for the selection report (e.g. "`Instance::new` panicked with GL; retried without GL").
    pub notes: Vec<String>,
    /// `panic = "abort"` builds only: GL was requested together with other backends but left
    /// out of this instance (a broken EGL setup can panic inside `Instance::new`, and a panic
    /// cannot be caught when unwinding is disabled). If selection on this instance finds no
    /// usable adapter, the caller retries with [`create_gl_instance`] (and a new surface).
    pub gl_deferred: bool,
}

/// Creates the process' single `wgpu::Instance`.
/// - `display`: winit's `OwnedDisplayHandle` (windowed; mandatory for GL on Wayland) or `None`
///   (headless). It is cloned for each attempt.
/// - backends = `prefs.backends` or [`default_backends`]; flags = [`instance_flags`];
///   `desc.backend_options = BackendOptions::default().with_env()` (never
///   `InstanceDescriptor::with_env`: an empty `WGPU_BACKEND` would disable every backend).
/// - `Instance::new` runs inside `catch_unwind` with the panic hook temporarily replaced by a
///   silent one (a handled EGL panic must not print a backtrace; the message goes to `notes`).
///   On panic: retry once without `GL`; if that also panics or no backend is left, return
///   `NoInstance`.
/// - `cfg(panic = "abort")`: `catch_unwind` cannot help, so when GL is requested together with
///   other backends the instance is created without GL and `gl_deferred` is set. If only GL was
///   requested it is created directly (the user asked for it).
pub fn create_instance<D>(
    display: Option<D>,
    prefs: &GpuPreferences,
) -> Result<CreatedInstance, crate::SelectError>
where
    D: WgpuHasDisplayHandle + Clone,
{
    todo!("WP3")
}

/// [`create_instance`] without a display handle (tools, tests, servers). GL then uses EGL's
/// surfaceless platform and the glvnd default vendor (NVIDIA 3.3 on the dev machine).
pub fn create_headless_instance(
    prefs: &GpuPreferences,
) -> Result<CreatedInstance, crate::SelectError> {
    todo!("WP3")
}

/// Second-stage instance with only `Backends::GL` (same flags and display rules as
/// [`create_instance`]), used when `gl_deferred` was set and the first selection found
/// nothing. May abort the process on a broken EGL setup (`panic = "abort"` builds).
pub fn create_gl_instance<D>(
    display: Option<D>,
    prefs: &GpuPreferences,
) -> Result<CreatedInstance, crate::SelectError>
where
    D: WgpuHasDisplayHandle + Clone,
{
    todo!("WP3")
}
