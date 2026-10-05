use crate::{
    CreatedInstance, GpuContext, GpuPreferences, LkgStore, SelectionReport, SmokeLevel, SmokeTest,
    Tier, TierPolicy,
};

/// Excludes an adapter (or caps its tier) for the rest of the session, after a device loss.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exclusion {
    /// `AdapterSummary::key()`.
    pub adapter_key: String,
    /// `None`: exclude the adapter entirely; `Some(t)`: allow at most `t`.
    pub max_tier: Option<Tier>,
}

/// Input of [`select_gpu`].
pub struct SelectRequest<'a> {
    /// The process instance (its backends, flags and notes go into the report).
    pub instance: &'a CreatedInstance,
    /// Windowed: adapters must pass `is_surface_supported(surface)` and offer a format accepted
    /// by [`crate::choose_surface_format`]. The surface is only queried, never configured, by
    /// `select_gpu`. Invariant (frozen): a `Surface` that was ever configured is never
    /// configured with another device; every re-selection after a configure uses a NEW surface.
    pub surface: Option<&'a wgpu::Surface<'static>>,
    /// Resolved preferences.
    pub prefs: &'a GpuPreferences,
    /// Classification policy.
    pub policy: TierPolicy,
    /// LKG store (`None` when `!prefs.lkg_enabled()`).
    pub lkg: Option<&'a mut LkgStore>,
    /// Session exclusions.
    pub exclusions: &'a [Exclusion],
    /// Tier proof.
    pub smoke: &'a dyn SmokeTest,
    /// Proof depth (`Auto` is resolved per adapter with [`SmokeLevel::resolve`]).
    pub smoke_level: SmokeLevel,
    /// False = classify and score only (`gpu_info` `--dry-run`): returns `NoUsableAdapter` with a
    /// full report and no devices are created.
    pub create_devices: bool,
}

/// Selection failure.
#[derive(Debug, thiserror::Error)]
pub enum SelectError {
    /// No instance could be created.
    #[error("no graphics backend could be initialized: {0}")]
    NoInstance(String),
    /// Every candidate was filtered out or failed. The report explains each one.
    #[error("no usable GPU adapter:\n{}", .0.to_text())]
    NoUsableAdapter(Box<SelectionReport>),
}

/// The detection algorithm (ADR 0002), frozen steps. Runs on the calling thread (error scopes
/// are thread-local).
/// 1. `enumerate_adapters(instance backends)`; snapshot `AdapterCaps`; drop `Noop`/`BrowserWebGpu`.
/// 2. Filters (recorded in `CandidateReport::filtered`): name substring (`prefs.adapter`; if it
///    matches nothing -> `NoUsableAdapter`), backend not in `prefs.backends`, `prefs.software`
///    (only Software class), and with a surface: `is_surface_supported` and
///    `choose_surface_format(&surface.get_capabilities(&adapter).formats)` (None -> filtered
///    "no 8-bit surface format").
/// 3. `classify` each; candidates = every tier `t <= min(max_tier, prefs.max_tier)` per adapter;
///    `score(caps, t, power, lkg status)`; stable sort by score descending.
/// 4. If the instance flags contain `DEBUG`, `VALIDATION`, `GPU_BASED_VALIDATION` or
///    `VALIDATION_INDIRECT_CALL`, set `lkg.read_only = true`. Skip pairs that are `Bad` in LKG
///    (unless `prefs.adapter` and `prefs.max_tier` both name them explicitly) or excluded.
/// 5. For each pair: `lkg.begin_attempt`; `request_device(tier_request(t))`; on error
///    `lkg.cancel_pending()` and continue; `install_device_callbacks`; run the smoke test with
///    `SmokeContext { level: smoke_level.resolve(caps), output_format: surface format or
///    Rgba8UnormSrgb, .. }` inside `run_in_error_scopes`; afterwards the pair also fails if
///    `health.is_lost()` or `health.error_count() > 0`. On failure: drop the device,
///    `lkg.cancel_pending()`, continue. In-process failures never write `Bad`.
/// 6. First success wins; `GpuContext::output_format` = the format the smoke test used. LKG
///    stays `Pending` until the caller calls `confirm_good` after the first presented frame
///    (or `cancel_pending` on exit). Log the report at info level.
pub fn select_gpu(req: SelectRequest<'_>) -> Result<GpuContext, SelectError> {
    todo!("WP3")
}

/// Headless convenience for tools and tests: [`crate::create_headless_instance`] + `select_gpu`
/// with [`crate::BasicSmokeTest`] (`SmokeLevel::Auto`), default policy, no surface, LKG
/// disabled; if that finds nothing and `gl_deferred` is set, [`crate::create_gl_instance`] +
/// `select_gpu` again.
pub fn init_headless(prefs: &GpuPreferences) -> Result<GpuContext, SelectError> {
    todo!("WP3")
}
