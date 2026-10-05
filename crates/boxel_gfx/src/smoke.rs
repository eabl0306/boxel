use crate::{AdapterCaps, DeviceClass, Tier};

/// How deep a smoke test goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SmokeLevel {
    /// Resolved per adapter by [`SmokeLevel::resolve`]: `Draw` on GL backends and on
    /// Software-class adapters, `Compile` elsewhere. Default.
    #[default]
    Auto,
    /// Create every pipeline/bind group layout the tier uses (catches shader translation errors,
    /// e.g. GLSL 330 without SSBOs), record one pass, `finish` and submit it.
    Compile,
    /// `Compile` + draw one quad offscreen (16x16) and read back the center pixel. Catches silent
    /// miscompiles such as the NVIDIA GL 3.3 uniform-block collision (ADR 0011), which produce no
    /// error at all.
    Draw,
}

impl SmokeLevel {
    /// The level actually run on `caps` (never `Auto`): `Draw` on `Backend::Gl` or
    /// `DeviceClass::Software` (a 16x16 readback costs a few ms), else `Compile`.
    pub fn resolve(self, caps: &AdapterCaps) -> SmokeLevel {
        match self {
            SmokeLevel::Auto
                if caps.info.backend == wgpu::Backend::Gl
                    || caps.class() == DeviceClass::Software =>
            {
                SmokeLevel::Draw
            }
            SmokeLevel::Auto => SmokeLevel::Compile,
            other => other,
        }
    }
}

/// Inputs of a smoke test.
pub struct SmokeContext<'a> {
    /// Fresh device for the candidate.
    pub device: &'a wgpu::Device,
    /// Its queue.
    pub queue: &'a wgpu::Queue,
    /// Tier being proven.
    pub tier: Tier,
    /// Adapter capabilities.
    pub caps: &'a AdapterCaps,
    /// Depth (already resolved: never `Auto`).
    pub level: SmokeLevel,
    /// Color format the device will present to: the chosen surface format when selecting with a
    /// surface ([`crate::choose_surface_format`]), else `Rgba8UnormSrgb`. The test must build
    /// its output pipeline (the renderer's blit) for exactly this format.
    pub output_format: wgpu::TextureFormat,
}

/// Proves a tier works on a device. `select_gpu` runs it inside [`run_in_error_scopes`].
/// The renderer implements it with its *real* pipelines (`boxel_render::RendererSmokeTest`);
/// tools without a renderer use [`BasicSmokeTest`].
///
/// Contract: at every level the test records at least one pass and calls [`submit_and_wait`]
/// inside the scopes (wgpu 30 reports pass validation errors at `CommandEncoder::finish`).
/// Everything runs on the calling thread: wgpu error scopes are thread-local
/// (`ErrorScopeGuard` is `!Send`).
pub trait SmokeTest: Send + Sync {
    /// Returns `Err(message)` on any failure the test detects itself.
    fn run(&self, ctx: &SmokeContext<'_>) -> Result<(), String>;
}

/// Self-contained smoke test with its own tiny WGSL modules exercising the same capability
/// classes as the renderer: Low = instance-step `Uint32x2` attribute + immediates + one uniform
/// block + `texture_2d_array`; Mid = Low + two read-only storage buffers in the vertex stage +
/// indexed draw; High = Mid + compute with `atomicAdd` into a STORAGE|INDIRECT buffer + a render
/// pass that records `multi_draw_indexed_indirect_count`. Output pipeline in
/// `ctx.output_format`.
#[derive(Clone, Copy, Debug, Default)]
pub struct BasicSmokeTest;

impl SmokeTest for BasicSmokeTest {
    fn run(&self, ctx: &SmokeContext<'_>) -> Result<(), String> {
        todo!("WP3")
    }
}

/// Finishes `encoder`, submits it and blocks until the GPU is idle. Smoke tests call it inside
/// [`run_in_error_scopes`].
pub fn submit_and_wait(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: wgpu::CommandEncoder,
) -> Result<(), String> {
    queue.submit([encoder.finish()]);
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map(|_| ())
        .map_err(|e| format!("poll: {e}"))
}

/// Runs `f` with Validation, Internal and `OutOfMemory` error scopes pushed (popped in reverse
/// order, blocking with pollster). Returns the first error message from `f` or a scope.
/// (Internal is mandatory: shader translation failures on GL are Internal errors.)
///
/// Thread rule: wgpu 30 error scopes are thread-local and the guards are `!Send`, so `f` and
/// all GPU bring-up it covers (selection, smoke tests, `VoxelRenderer::new`) run on the calling
/// thread. Keep this in mind for async GPU init (M3).
pub fn run_in_error_scopes(
    device: &wgpu::Device,
    f: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
    let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let result = f();
    let oom = pollster::block_on(oom.pop());
    let internal = pollster::block_on(internal.pop());
    let validation = pollster::block_on(validation.pop());
    let one_line = |e: wgpu::Error| e.to_string().replace('\n', " ");
    result?;
    if let Some(e) = validation {
        return Err(format!("validation: {}", one_line(e)));
    }
    if let Some(e) = internal {
        return Err(format!("internal: {}", one_line(e)));
    }
    if let Some(e) = oom {
        return Err(format!("out of memory: {}", one_line(e)));
    }
    Ok(())
}
