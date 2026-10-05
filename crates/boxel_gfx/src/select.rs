use crate::{
    AdapterCaps, Attempt, AttemptOutcome, BasicSmokeTest, CandidateReport, CreatedInstance,
    GpuContext, GpuHealth, GpuPreferences, LkgStatus, LkgStore, SelectionReport, SmokeContext,
    SmokeLevel, SmokeTest, Tier, TierPolicy, choose_surface_format, classify,
    install_device_callbacks, one_line, run_in_error_scopes, score, tier_request,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    let SelectRequest {
        instance: created,
        surface,
        prefs,
        policy,
        mut lkg,
        exclusions,
        smoke,
        smoke_level,
        create_devices,
    } = req;
    let mut report = SelectionReport {
        backends: created.backends,
        flags: created.flags,
        candidates: Vec::new(),
        chosen: None,
        notes: created.notes.clone(),
    };
    let debug_flags = wgpu::InstanceFlags::DEBUG
        | wgpu::InstanceFlags::VALIDATION
        | wgpu::InstanceFlags::GPU_BASED_VALIDATION
        | wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL;
    if let Some(store) = lkg.as_deref_mut()
        && created.flags.intersects(debug_flags)
        && !store.read_only
    {
        store.read_only = true;
        report
            .notes
            .push("last-known-good is read-only (debug instance flags)".to_string());
    }

    // 1-2. Enumerate, snapshot, filter, classify.
    let adapters = pollster::block_on(created.instance.enumerate_adapters(wgpu::Backends::all()));
    let wanted_name = prefs.adapter.as_ref().map(|n| n.to_lowercase());
    let mut name_matched = false;
    let mut all_caps = Vec::with_capacity(adapters.len());
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let mut cand = CandidateReport {
            adapter: caps.summary(),
            classification: classify(&caps, policy),
            surface_supported: None,
            surface_format: None,
            filtered: None,
            attempts: Vec::new(),
        };
        let real = !matches!(
            caps.info.backend,
            wgpu::Backend::Noop | wgpu::Backend::BrowserWebGpu
        );
        let name_ok = wanted_name
            .as_ref()
            .is_none_or(|n| caps.info.name.to_lowercase().contains(n));
        name_matched |= real && name_ok;
        cand.filtered = if real {
            filter_reason(&caps, prefs, name_ok)
        } else {
            Some(format!(
                "backend {:?} is not a GPU backend",
                caps.info.backend
            ))
        };
        if cand.filtered.is_none()
            && let Some(surface) = surface
        {
            let supported = adapter.is_surface_supported(surface);
            cand.surface_supported = Some(supported);
            if supported {
                cand.surface_format =
                    choose_surface_format(&surface.get_capabilities(adapter).formats);
                if cand.surface_format.is_none() {
                    cand.filtered = Some("no 8-bit surface format".to_string());
                }
            } else {
                cand.filtered = Some("cannot present to the surface".to_string());
            }
        }
        report.candidates.push(cand);
        all_caps.push(caps);
    }
    if let Some(name) = &prefs.adapter
        && !name_matched
    {
        report.notes.push(format!(
            "no adapter name contains {name:?} (--adapter is explicit)"
        ));
        return Err(SelectError::NoUsableAdapter(Box::new(report)));
    }

    // 3. Candidates in score order (stable: ties keep enumeration order).
    let cap = prefs.max_tier.unwrap_or(Tier::High);
    let power = prefs.power_or_default();
    let mut pairs = Vec::new();
    for (index, cand) in report.candidates.iter().enumerate() {
        let Some(max) = cand.classification.max_tier else {
            continue;
        };
        if cand.filtered.is_some() {
            continue;
        }
        for tier in Tier::DESCENDING {
            if tier <= max && tier <= cap {
                let status = lkg.as_deref().and_then(|l| l.status(&cand.adapter, tier));
                pairs.push((index, tier, score(&all_caps[index], tier, power, status)));
            }
        }
    }
    pairs.sort_by_key(|pair| std::cmp::Reverse(pair.2));

    if !create_devices {
        for (index, tier, score) in pairs {
            report.candidates[index].attempts.push(Attempt {
                tier,
                score,
                outcome: AttemptOutcome::NotAttempted,
                elapsed: Duration::ZERO,
            });
        }
        return Err(SelectError::NoUsableAdapter(Box::new(report)));
    }

    // 4-6. Attempts.
    let mut remaining = pairs.into_iter();
    while let Some((index, tier, score)) = remaining.next() {
        let summary = report.candidates[index].adapter.clone();
        let key = summary.key();
        let record = |report: &mut SelectionReport, outcome: AttemptOutcome, elapsed: Duration| {
            report.candidates[index].attempts.push(Attempt {
                tier,
                score,
                outcome,
                elapsed,
            });
        };
        if exclusions
            .iter()
            .any(|e| e.adapter_key == key && e.max_tier.is_none_or(|max| tier > max))
        {
            record(&mut report, AttemptOutcome::SkippedExcluded, Duration::ZERO);
            continue;
        }
        let named = prefs.adapter.is_some() && prefs.max_tier == Some(tier);
        if !named && lkg.as_deref().and_then(|l| l.status(&summary, tier)) == Some(LkgStatus::Bad) {
            record(&mut report, AttemptOutcome::SkippedKnownBad, Duration::ZERO);
            continue;
        }
        if let Some(store) = lkg.as_deref_mut() {
            store.begin_attempt(&summary, tier);
        }
        let caps = &all_caps[index];
        let adapter = &adapters[index];
        let start = Instant::now();
        let request = tier_request(tier, &caps.limits);
        let device = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("boxel"),
            required_features: request.features,
            required_limits: request.limits,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }));
        let (device, queue) = match device {
            Ok(pair) => pair,
            Err(e) => {
                let message = one_line(&e.to_string());
                tracing::debug!("{} {tier}: request_device failed: {message}", summary.name);
                record(
                    &mut report,
                    AttemptOutcome::RequestDeviceFailed(message),
                    start.elapsed(),
                );
                if let Some(store) = lkg.as_deref_mut() {
                    store.cancel_pending();
                }
                continue;
            }
        };
        let health = Arc::new(GpuHealth::default());
        install_device_callbacks(&device, &health);
        let output_format = report.candidates[index]
            .surface_format
            .unwrap_or(wgpu::TextureFormat::Rgba8UnormSrgb);
        let ctx = SmokeContext {
            device: &device,
            queue: &queue,
            tier,
            caps,
            level: smoke_level.resolve(caps),
            output_format,
        };
        let mut result = run_in_error_scopes(&device, || smoke.run(&ctx));
        if result.is_ok() && health.is_lost() {
            result = Err(format!(
                "device lost during smoke test: {}",
                health.lost_reason().unwrap_or_default()
            ));
        }
        if result.is_ok() && health.error_count() > 0 {
            result = Err(format!(
                "uncaptured error: {}",
                health.last_error().unwrap_or_default()
            ));
        }
        if let Err(message) = result {
            let message = one_line(&message);
            tracing::debug!("{} {tier}: smoke test failed: {message}", summary.name);
            record(
                &mut report,
                AttemptOutcome::SmokeFailed(message),
                start.elapsed(),
            );
            if let Some(store) = lkg.as_deref_mut() {
                store.cancel_pending();
            }
            // The failed device and queue are dropped here, before the next candidate.
            continue;
        }
        record(&mut report, AttemptOutcome::Ok, start.elapsed());
        report.chosen = Some((index, tier));
        // The runner-up shows what a failure of the winner would have fallen back to.
        if let Some((next, next_tier, next_score)) = remaining.next() {
            report.candidates[next].attempts.push(Attempt {
                tier: next_tier,
                score: next_score,
                outcome: AttemptOutcome::NotAttempted,
                elapsed: Duration::ZERO,
            });
        }
        tracing::info!("{}", report.to_text());
        return Ok(GpuContext {
            instance: created.instance.clone(),
            adapter: adapter.clone(),
            device,
            queue,
            class: caps.class(),
            caps: caps.clone(),
            tier,
            output_format,
            health,
            report,
        });
    }
    Err(SelectError::NoUsableAdapter(Box::new(report)))
}

/// Pre-attempt filters (step 2, surface checks aside): `None` keeps the adapter.
fn filter_reason(caps: &AdapterCaps, prefs: &GpuPreferences, name_ok: bool) -> Option<String> {
    let backend = caps.info.backend;
    if let Some(backends) = prefs.backends
        && !backends.contains(wgpu::Backends::from(backend))
    {
        return Some(format!("backend {backend:?} not requested"));
    }
    if !name_ok {
        return Some(format!(
            "name does not contain {:?}",
            prefs.adapter.as_deref().unwrap_or_default()
        ));
    }
    if prefs.software == Some(true) && caps.class() != crate::DeviceClass::Software {
        return Some("not a software adapter (--software)".to_string());
    }
    None
}

/// Headless convenience for tools and tests: [`crate::create_headless_instance`] + `select_gpu`
/// with [`crate::BasicSmokeTest`] (`SmokeLevel::Auto`), default policy, no surface, LKG
/// disabled; if that finds nothing and `gl_deferred` is set, [`crate::create_gl_instance`] +
/// `select_gpu` again.
pub fn init_headless(prefs: &GpuPreferences) -> Result<GpuContext, SelectError> {
    let select = |created: &CreatedInstance| {
        select_gpu(SelectRequest {
            instance: created,
            surface: None,
            prefs,
            policy: TierPolicy::default(),
            lkg: None,
            exclusions: &[],
            smoke: &BasicSmokeTest,
            smoke_level: SmokeLevel::Auto,
            create_devices: true,
        })
    };
    let created = crate::create_headless_instance(prefs)?;
    match select(&created) {
        Err(SelectError::NoUsableAdapter(_)) if created.gl_deferred => {
            let gl = crate::create_gl_instance(None::<crate::instance::NoDisplay>, prefs)?;
            select(&gl)
        }
        other => other,
    }
}
