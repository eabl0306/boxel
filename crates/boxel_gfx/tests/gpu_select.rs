//! Headless selection + smoke tests on real adapters. Self-skips without an adapter unless
//! `BOXEL_REQUIRE_GPU=1`. Honors `WGPU_BACKEND` and `WGPU_ADAPTER_NAME` through
//! `GpuPreferences::from_wgpu_env`, like every GPU test.
#![expect(
    clippy::print_stderr,
    reason = "GPU test diagnostics (skips, warnings)"
)]
use boxel_gfx::{
    AdapterCaps, AttemptOutcome, BasicSmokeTest, GpuHealth, GpuPreferences, SelectError,
    SelectRequest, SmokeContext, SmokeLevel, SmokeTest, Tier, TierPolicy, classify,
    create_headless_instance, gpu_tests_required, init_headless, install_device_callbacks,
    run_in_error_scopes, select_gpu, tier_request,
};
use std::sync::Arc;

/// Preferences from `WGPU_BACKEND` / `WGPU_ADAPTER_NAME` / `WGPU_POWER_PREF` (default when unset).
fn env_prefs() -> GpuPreferences {
    let (prefs, warnings) = GpuPreferences::from_wgpu_env();
    for w in warnings {
        eprintln!("warning: {w}");
    }
    prefs
}

fn skip(reason: &str) {
    assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but {reason}");
    eprintln!("skipping: {reason}");
}

#[test]
fn select_headless() {
    let prefs = env_prefs();
    let gpu = match init_headless(&prefs) {
        Ok(gpu) => gpu,
        Err(SelectError::NoUsableAdapter(report)) => {
            eprintln!("{}", report.to_text());
            return skip("no usable adapter");
        }
        Err(e) => return skip(&e.to_string()),
    };
    let text = gpu.report.to_text();
    eprintln!("{text}");
    let last = text.lines().last().unwrap_or_default();
    assert_eq!(last, format!("DECISION: {}", gpu.summary_line()));
    assert!(last.starts_with("DECISION: backend="), "{last}");
    let (index, tier) = gpu.report.chosen.expect("chosen");
    assert_eq!(tier, gpu.tier);
    let winner = &gpu.report.candidates[index];
    assert_eq!(
        winner
            .attempts
            .iter()
            .find(|a| a.tier == tier)
            .map(|a| &a.outcome),
        Some(&AttemptOutcome::Ok)
    );
    assert_eq!(winner.adapter, gpu.caps.summary());
    assert_eq!(gpu.class, gpu.caps.class());
    assert_eq!(gpu.output_format, wgpu::TextureFormat::Rgba8UnormSrgb);
    assert!(!gpu.health.is_lost());
    assert_eq!(gpu.health.error_count(), 0);
    // The tier never exceeds what classification allows with the default policy.
    let max = classify(&gpu.caps, TierPolicy::default()).max_tier;
    assert!(max.is_some_and(|m| gpu.tier <= m));
    // The device really has the tier's features.
    let req = tier_request(gpu.tier, &gpu.caps.limits);
    assert!(gpu.device.features().contains(req.features));
}

#[test]
fn dry_run_creates_no_device_and_explains_itself() {
    let prefs = env_prefs();
    let created = match create_headless_instance(&prefs) {
        Ok(created) => created,
        Err(e) => return skip(&e.to_string()),
    };
    let r = select_gpu(SelectRequest {
        instance: &created,
        surface: None,
        prefs: &prefs,
        policy: TierPolicy::default(),
        lkg: None,
        exclusions: &[],
        smoke: &BasicSmokeTest,
        smoke_level: SmokeLevel::Auto,
        create_devices: false,
    });
    let Err(SelectError::NoUsableAdapter(report)) = r else {
        panic!("a dry run never returns a device");
    };
    let text = report.to_text();
    eprintln!("{text}");
    assert!(
        text.lines()
            .last()
            .unwrap_or_default()
            .starts_with("DECISION: none (")
    );
    assert!(
        report
            .candidates
            .iter()
            .flat_map(|c| &c.attempts)
            .all(|a| a.outcome == AttemptOutcome::NotAttempted)
    );
}

/// `BasicSmokeTest` at `Draw` for every tier `classify` allows (software uncapped), with both
/// blit entry points (`Rgba8UnormSrgb`: hardware encode; `Bgra8Unorm`: manual encode).
#[test]
fn basic_smoke_test_draws_every_claimed_tier() {
    let prefs = env_prefs();
    let created = match create_headless_instance(&prefs) {
        Ok(created) => created,
        Err(e) => return skip(&e.to_string()),
    };
    let adapters = pollster::block_on(created.instance.enumerate_adapters(wgpu::Backends::all()));
    let wanted = prefs.adapter.as_ref().map(|n| n.to_lowercase());
    let adapters: Vec<_> = adapters
        .into_iter()
        .filter(|a| {
            wanted
                .as_ref()
                .is_none_or(|n| a.get_info().name.to_lowercase().contains(n))
        })
        .collect();
    if adapters.is_empty() {
        return skip("no adapter");
    }
    let policy = TierPolicy {
        software_max: Tier::High,
        ..TierPolicy::default()
    };
    let mut runs = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let class = classify(&caps, policy);
        let Some(max) = class.max_tier else {
            eprintln!("{}: no tier ({:?})", caps.info.name, class.reasons);
            continue;
        };
        for tier in Tier::DESCENDING.into_iter().filter(|t| *t <= max) {
            for format in [
                wgpu::TextureFormat::Rgba8UnormSrgb,
                wgpu::TextureFormat::Bgra8Unorm,
            ] {
                let r = smoke(adapter, &caps, tier, format);
                eprintln!(
                    "{} [{:?}] {tier} {format:?}: {r:?}",
                    caps.info.name, caps.info.backend
                );
                if let Err(e) = r {
                    panic!(
                        "{} {:?} {tier} {format:?}: {e}",
                        caps.info.name, caps.info.backend
                    );
                }
                runs += 1;
            }
        }
    }
    assert!(runs > 0, "no adapter claimed any tier");
}

fn smoke(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    output_format: wgpu::TextureFormat,
) -> Result<(), String> {
    let req = tier_request(tier, &caps.limits);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("gpu_select"),
        required_features: req.features,
        required_limits: req.limits,
        ..Default::default()
    }))
    .map_err(|e| format!("request_device: {e}"))?;
    let health = Arc::new(GpuHealth::default());
    install_device_callbacks(&device, &health);
    let ctx = SmokeContext {
        device: &device,
        queue: &queue,
        tier,
        caps,
        level: SmokeLevel::Draw,
        output_format,
    };
    run_in_error_scopes(&device, || BasicSmokeTest.run(&ctx))?;
    if health.is_lost() || health.error_count() > 0 {
        return Err(format!(
            "health: lost={:?} last_error={:?}",
            health.lost_reason(),
            health.last_error()
        ));
    }
    Ok(())
}

/// Every candidate whose smoke test fails is reported and the next one is tried; nothing
/// usable ends in `NoUsableAdapter` with a full report.
#[test]
fn failing_smoke_tests_fall_back_through_every_candidate() {
    struct AlwaysFails;
    impl SmokeTest for AlwaysFails {
        fn run(&self, _ctx: &SmokeContext<'_>) -> Result<(), String> {
            Err("simulated smoke failure".to_string())
        }
    }
    let prefs = env_prefs();
    let created = match create_headless_instance(&prefs) {
        Ok(created) => created,
        Err(e) => return skip(&e.to_string()),
    };
    let r = select_gpu(SelectRequest {
        instance: &created,
        surface: None,
        prefs: &prefs,
        policy: TierPolicy::default(),
        lkg: None,
        exclusions: &[],
        smoke: &AlwaysFails,
        smoke_level: SmokeLevel::Compile,
        create_devices: true,
    });
    let Err(SelectError::NoUsableAdapter(report)) = r else {
        panic!("every candidate must fail");
    };
    let text = report.to_text();
    eprintln!("{text}");
    let attempts: Vec<_> = report.candidates.iter().flat_map(|c| &c.attempts).collect();
    if attempts.is_empty() {
        return skip("no candidate");
    }
    assert!(attempts.iter().all(|a| matches!(
        &a.outcome,
        AttemptOutcome::SmokeFailed(m) if m == "simulated smoke failure"
    )));
    assert!(
        text.ends_with("DECISION: none (every candidate failed)"),
        "{text}"
    );
}
