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
    let mut flags = wgpu::InstanceFlags::empty();
    if prefs.validation.unwrap_or(cfg!(debug_assertions)) {
        flags |= wgpu::InstanceFlags::DEBUG | wgpu::InstanceFlags::VALIDATION;
    }
    flags.with_env()
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
    let requested = prefs.backends.unwrap_or_else(default_backends);
    create_with(display, prefs, requested, cfg!(panic = "abort"))
}

/// [`create_instance`] without a display handle (tools, tests, servers). GL then uses EGL's
/// surfaceless platform and the glvnd default vendor (NVIDIA 3.3 on the dev machine).
pub fn create_headless_instance(
    prefs: &GpuPreferences,
) -> Result<CreatedInstance, crate::SelectError> {
    create_instance(None::<NoDisplay>, prefs)
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
    create_with(display, prefs, wgpu::Backends::GL, false)
}

/// Display placeholder for headless instances: never queried, because the option is `None`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NoDisplay;

impl wgpu::rwh::HasDisplayHandle for NoDisplay {
    fn display_handle(&self) -> Result<wgpu::rwh::DisplayHandle<'_>, wgpu::rwh::HandleError> {
        Err(wgpu::rwh::HandleError::Unavailable)
    }
}

/// Shared body of [`create_instance`] and [`create_gl_instance`]. `defer_gl`: leave GL out
/// when other backends are requested too (`panic = "abort"` builds).
fn create_with<D>(
    display: Option<D>,
    prefs: &GpuPreferences,
    requested: wgpu::Backends,
    defer_gl: bool,
) -> Result<CreatedInstance, crate::SelectError>
where
    D: WgpuHasDisplayHandle + Clone,
{
    let flags = instance_flags(prefs);
    let mut notes = Vec::new();
    let mut backends = requested;
    let mut gl_deferred = false;
    if backends.is_empty() {
        return Err(crate::SelectError::NoInstance(
            "no backends requested".to_string(),
        ));
    }
    if defer_gl && backends.contains(wgpu::Backends::GL) && backends != wgpu::Backends::GL {
        backends.remove(wgpu::Backends::GL);
        gl_deferred = true;
        notes.push(
            "panic = \"abort\" build: GL left out of the first instance (tried later if needed)"
                .to_string(),
        );
    }
    let first = match new_instance(display.clone(), backends, flags) {
        Ok(instance) => instance,
        Err(message) => {
            let without_gl = backends - wgpu::Backends::GL;
            if !backends.contains(wgpu::Backends::GL) || without_gl.is_empty() {
                return Err(crate::SelectError::NoInstance(format!(
                    "Instance::new panicked with backends {}: {message}",
                    backends_list(backends)
                )));
            }
            tracing::warn!("Instance::new panicked ({message}); retrying without GL");
            notes.push(format!(
                "Instance::new panicked with backends {}: {message}; retried without GL",
                backends_list(backends)
            ));
            backends = without_gl;
            new_instance(display, backends, flags).map_err(|message| {
                crate::SelectError::NoInstance(format!(
                    "Instance::new panicked again without GL: {message}"
                ))
            })?
        }
    };
    Ok(CreatedInstance {
        instance: first,
        backends,
        flags,
        notes,
        gl_deferred,
    })
}

/// One `Instance::new` attempt, panics caught silently and returned as their message.
fn new_instance<D>(
    display: Option<D>,
    backends: wgpu::Backends,
    flags: wgpu::InstanceFlags,
) -> Result<wgpu::Instance, String>
where
    D: WgpuHasDisplayHandle + Clone,
{
    let mut desc = match display {
        Some(d) => wgpu::InstanceDescriptor::new_with_display_handle(Box::new(d)),
        None => wgpu::InstanceDescriptor::new_without_display_handle(),
    };
    desc.backends = backends;
    desc.flags = flags;
    // Never `InstanceDescriptor::with_env`: an empty `WGPU_BACKEND` would disable every backend.
    desc.backend_options = desc.backend_options.with_env();
    catch_panic_silently(move || wgpu::Instance::new(desc))
}

/// `cfg(panic = "abort")`: nothing can be caught, so `f` runs directly with the installed hooks
/// (the panic message and the LKG panic hook still run before the abort; a silent hook would
/// make a broken EGL setup die without a word).
#[cfg(panic = "abort")]
pub(crate) fn catch_panic_silently<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    Ok(f())
}

/// Runs `f` inside `catch_unwind` with the panic hook replaced by a silent one (a handled
/// driver panic must not print a backtrace); the previous hook is restored right after.
///
/// Concurrent callers are serialized: two interleaved swaps would leave the silent hook
/// installed for good.
#[cfg(not(panic = "abort"))]
pub(crate) fn catch_panic_silently<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    static HOOK_SWAP: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = HOOK_SWAP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(previous);
    result.map_err(|payload| {
        payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic payload".to_string())
            .replace('\n', " ")
    })
}

/// `"VULKAN | GL"` (the inner part of the bitflags `Debug` output).
pub(crate) fn backends_list(backends: wgpu::Backends) -> String {
    let text = format!("{backends:?}");
    text.strip_prefix("Backends(")
        .and_then(|t| t.strip_suffix(')'))
        .map_or(text.clone(), str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    #[cfg(not(panic = "abort"))]
    fn panics_are_caught_silently_and_the_hook_is_restored() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let original = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        }));

        assert_eq!(catch_panic_silently(|| 7), Ok(7));
        let err = catch_panic_silently(|| -> u32 { panic!("EGL said\nno") });
        assert_eq!(err, Err("EGL said no".to_string()));
        let err = catch_panic_silently(|| -> u32 { std::panic::panic_any(3_u8) });
        assert_eq!(err, Err("unknown panic payload".to_string()));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "the silent hook replaced ours"
        );

        // Our hook is back in place afterwards.
        let _ = std::thread::spawn(|| panic!("after")).join();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        std::panic::set_hook(original);
    }

    #[test]
    fn backend_lists() {
        assert_eq!(
            backends_list(wgpu::Backends::VULKAN | wgpu::Backends::GL),
            "VULKAN | GL"
        );
        assert_eq!(backends_list(wgpu::Backends::GL), "GL");
    }

    #[test]
    fn empty_backends_are_rejected() {
        let prefs = GpuPreferences {
            backends: Some(wgpu::Backends::empty()),
            ..Default::default()
        };
        assert!(matches!(
            create_headless_instance(&prefs),
            Err(crate::SelectError::NoInstance(_))
        ));
    }
}
