use crate::{GpuContext, PresentModePref};

/// Result of [`acquire_frame`].
#[derive(Debug)]
pub enum FrameAcquire {
    /// Render into it, then `window.pre_present_notify()` and `queue.present(texture)`.
    Frame {
        /// The texture.
        texture: wgpu::SurfaceTexture,
        /// `Suboptimal`: present, then reconfigure.
        suboptimal: bool,
    },
    /// `Timeout` / `Occluded`: skip this frame.
    Skip,
    /// `Outdated`: reconfigure with the current size, skip this frame.
    Reconfigure,
    /// `Lost`: recreate the surface. Rules (frozen): never while `GpuHealth::is_lost()` (then the
    /// renderer re-selects a device instead); DROP the old `Surface` first, then create a new one
    /// from the window with the same instance and configure it with the SAME device (a second
    /// live swapchain on one window fails with `VK_ERROR_NATIVE_WINDOW_IN_USE_KHR`).
    Recreate,
}

/// Surface format choice (frozen): the first of `Bgra8UnormSrgb`, `Rgba8UnormSrgb`,
/// `Bgra8Unorm`, `Rgba8Unorm` present in `formats` (capability order ignored). Never a float,
/// 10-bit or HDR format: the blit's manual sRGB encode would be wrong there. `None` = the
/// adapter cannot present to this surface for boxel (selection filters it out).
pub fn choose_surface_format(formats: &[wgpu::TextureFormat]) -> Option<wgpu::TextureFormat> {
    use wgpu::TextureFormat as F;
    [
        F::Bgra8UnormSrgb,
        F::Rgba8UnormSrgb,
        F::Bgra8Unorm,
        F::Rgba8Unorm,
    ]
    .into_iter()
    .find(|f| formats.contains(f))
}

/// `Opaque` if supported, else `Auto` (the blit always writes alpha = 1).
pub fn choose_alpha_mode(modes: &[wgpu::CompositeAlphaMode]) -> wgpu::CompositeAlphaMode {
    if modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
        wgpu::CompositeAlphaMode::Opaque
    } else {
        wgpu::CompositeAlphaMode::Auto
    }
}

/// `Vsync` -> `Fifo`; `Mailbox` -> `Mailbox` if supported else `Fifo`; `Immediate` ->
/// `Immediate`, else `Mailbox`, else `Fifo` (`Fifo` is always supported).
pub fn choose_present_mode(
    pref: PresentModePref,
    modes: &[wgpu::PresentMode],
) -> wgpu::PresentMode {
    use wgpu::PresentMode as P;
    let wanted: &[P] = match pref {
        PresentModePref::Vsync => &[P::Fifo],
        PresentModePref::Mailbox => &[P::Mailbox, P::Fifo],
        PresentModePref::Immediate => &[P::Immediate, P::Mailbox, P::Fifo],
    };
    wanted
        .iter()
        .copied()
        .find(|m| modes.contains(m))
        .unwrap_or(P::Fifo)
}

/// Configures `surface` for the selected device (inside [`crate::run_in_error_scopes`]):
/// format = `gpu.output_format` (the one the smoke test proved; error if the surface no longer
/// offers it), present mode = [`choose_present_mode`], alpha = [`choose_alpha_mode`],
/// `desired_maximum_frame_latency = 2`, usage `RENDER_ATTACHMENT`, `view_formats = []`.
/// Returns the configuration used. Never call it on a surface that was configured with another
/// device: create a new `Surface` instead (Wayland segfault, measured).
pub fn configure_surface(
    surface: &wgpu::Surface<'_>,
    gpu: &GpuContext,
    width: u32,
    height: u32,
    present: PresentModePref,
) -> Result<wgpu::SurfaceConfiguration, String> {
    let caps = surface.get_capabilities(&gpu.adapter);
    if !caps.formats.contains(&gpu.output_format) {
        return Err(format!(
            "surface no longer offers {:?} (offers {:?})",
            gpu.output_format, caps.formats
        ));
    }
    // A zero-sized (minimized, not yet mapped) or oversized surface is a validation error:
    // clamp to 1..=max_texture_dimension_2d; the caller reconfigures on the next resize.
    let max = gpu.device.limits().max_texture_dimension_2d.max(1);
    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: gpu.output_format,
        color_space: wgpu::SurfaceColorSpace::default(),
        width: width.clamp(1, max),
        height: height.clamp(1, max),
        present_mode: choose_present_mode(present, &caps.present_modes),
        desired_maximum_frame_latency: 2,
        alpha_mode: choose_alpha_mode(&caps.alpha_modes),
        view_formats: Vec::new(),
    };
    crate::run_in_error_scopes(&gpu.device, || {
        surface.configure(&gpu.device, &config);
        Ok(())
    })?;
    Ok(config)
}

/// Maps `Surface::get_current_texture()` (wgpu 30 `CurrentSurfaceTexture`) to [`FrameAcquire`];
/// a `Validation` result logs at warn and becomes `Skip`.
pub fn acquire_frame(surface: &wgpu::Surface<'_>) -> FrameAcquire {
    match surface.get_current_texture() {
        wgpu::CurrentSurfaceTexture::Success(texture) => FrameAcquire::Frame {
            texture,
            suboptimal: false,
        },
        wgpu::CurrentSurfaceTexture::Suboptimal(texture) => FrameAcquire::Frame {
            texture,
            suboptimal: true,
        },
        wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
            FrameAcquire::Skip
        }
        wgpu::CurrentSurfaceTexture::Outdated => FrameAcquire::Reconfigure,
        wgpu::CurrentSurfaceTexture::Lost => FrameAcquire::Recreate,
        wgpu::CurrentSurfaceTexture::Validation => {
            tracing::warn!("get_current_texture: validation error; frame skipped");
            FrameAcquire::Skip
        }
    }
}
