//! Headless GPU report: every adapter, how boxel classifies it and what boxel would pick.
//!
//! `cargo run -p boxel_gfx --example gpu_info -- [GPU options] [--dry-run] [--smoke auto|compile|draw]
//! [--all-tiers] [--lkg APP] [--clear-lkg APP] [--help]`. Lives in `boxel_gfx` so it builds
//! without the rest of the engine.
//!
//! Exit codes: 0 success, 1 usage error, 2 no usable GPU.
#![expect(clippy::print_stdout, clippy::print_stderr, reason = "CLI output")]

use boxel_gfx::{
    AdapterCaps, BasicSmokeTest, CLI_HELP, CreatedInstance, DeviceClass, GpuHealth, GpuPreferences,
    LkgStore, SelectError, SelectRequest, SmokeContext, SmokeLevel, SmokeTest, Tier, TierPolicy,
    build_fingerprint, classify, create_gl_instance, create_headless_instance,
    install_device_callbacks, run_in_error_scopes, score, select_gpu, tier_request, wgpu,
};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

const USAGE: &str = "\
usage: gpu_info [GPU options] [--dry-run] [--smoke auto|compile|draw] [--all-tiers]
                [--lkg APP] [--clear-lkg APP] [--help]
  --dry-run            classify and score only (no device is created)
  --smoke <level>      smoke test depth: auto (Draw on GL and software), compile, draw
  --all-tiers          create a device and run the smoke test for every adapter and tier
  --lkg APP            print the last-known-good file of APP and exit
  --clear-lkg APP      clear the last-known-good file of APP and exit
";

/// What the tool was asked to do besides the GPU options.
#[derive(Default)]
struct Options {
    dry_run: bool,
    all_tiers: bool,
    help: bool,
    smoke: SmokeLevel,
    show_lkg: Option<String>,
    clear_lkg: Option<String>,
}

fn main() -> ExitCode {
    init_logging();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (args, show_lkg, clear_lkg) = take_lkg_app_flags(args);
    let (cli, config, rest) = match GpuPreferences::from_args(&args) {
        Ok(parsed) => parsed,
        Err(e) => return usage_error(&e.to_string()),
    };
    let mut options = match parse_options(&rest) {
        Ok(options) => options,
        Err(message) => return usage_error(&message),
    };
    options.show_lkg = show_lkg;
    options.clear_lkg = clear_lkg;
    if options.help {
        println!("{USAGE}\n{CLI_HELP}");
        return ExitCode::SUCCESS;
    }
    if let Some(app) = &options.show_lkg {
        return print_lkg(app);
    }
    if let Some(app) = &options.clear_lkg {
        return clear_lkg_file(app);
    }

    let (prefs, warnings) = GpuPreferences::resolve(cli, config.as_deref());
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }
    let policy = TierPolicy {
        dx12_high: std::env::var("BOXEL_DX12_HIGH").is_ok_and(|v| v == "1"),
        ..TierPolicy::default()
    };

    let created = match create_headless_instance(&prefs) {
        Ok(created) => created,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    print_instance(&created);
    let adapters = pollster::block_on(created.instance.enumerate_adapters(wgpu::Backends::all()));
    let caps: Vec<AdapterCaps> = adapters.iter().map(AdapterCaps::from_adapter).collect();
    for (i, c) in caps.iter().enumerate() {
        print_adapter(i, c, &prefs, policy);
    }
    if options.all_tiers {
        print_tier_matrix(&adapters, &caps, &prefs, policy, options.smoke);
    }
    select_and_report(&created, &prefs, policy, &options)
}

/// `boxel_log` is optional for this tool: a logging problem must never hide the GPU report.
fn init_logging() {
    // `AlreadySet` and `BadFilter` are ignored on purpose (RUST_LOG may be anything; an invalid
    // RUST_LOG already falls back to "warn" inside `init_logging`).
    let _ = boxel_log::init_logging("warn");
}

fn usage_error(message: &str) -> ExitCode {
    eprintln!("error: {message}\n\n{USAGE}\n{CLI_HELP}");
    ExitCode::from(1)
}

/// Removes `--lkg APP` / `--lkg=APP` and `--clear-lkg APP` / `--clear-lkg=APP` before the GPU
/// flags are parsed (`--lkg` alone stays: it is the GPU option).
fn take_lkg_app_flags(args: Vec<String>) -> (Vec<String>, Option<String>, Option<String>) {
    let mut rest = Vec::with_capacity(args.len());
    let (mut show, mut clear) = (None, None);
    let mut iter = args.into_iter().peekable();
    while let Some(arg) = iter.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) => (f.to_string(), Some(v.to_string())),
            None => (arg.clone(), None),
        };
        if flag != "--lkg" && flag != "--clear-lkg" {
            rest.push(arg);
            continue;
        }
        let app = inline.or_else(|| iter.next_if(|next| !next.starts_with("--")));
        match (flag.as_str(), app) {
            ("--lkg", Some(app)) => show = Some(app),
            ("--clear-lkg", Some(app)) => clear = Some(app),
            // `--lkg` without an app name is the GPU option; `--clear-lkg` needs one.
            _ => rest.push(arg),
        }
    }
    (rest, show, clear)
}

fn parse_options(rest: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) => (f, Some(v.to_string())),
            None => (arg.as_str(), None),
        };
        match flag {
            "--help" | "-h" => options.help = true,
            "--dry-run" => options.dry_run = true,
            "--all-tiers" => options.all_tiers = true,
            "--smoke" => {
                let value = inline
                    .or_else(|| iter.next().cloned())
                    .ok_or("missing value for --smoke")?;
                options.smoke = match value.to_ascii_lowercase().as_str() {
                    "auto" => SmokeLevel::Auto,
                    "compile" => SmokeLevel::Compile,
                    "draw" => SmokeLevel::Draw,
                    _ => {
                        return Err(format!(
                            "invalid value {value:?} for --smoke: expected auto|compile|draw"
                        ));
                    }
                };
            }
            "--clear-lkg" => return Err("missing APP for --clear-lkg".to_string()),
            _ => return Err(format!("unknown argument {arg:?}")),
        }
    }
    Ok(options)
}

fn print_lkg(app: &str) -> ExitCode {
    let Some(path) = LkgStore::default_path(app) else {
        eprintln!("error: no configuration directory for the last-known-good file");
        return ExitCode::from(1);
    };
    let (store, error) = LkgStore::open(Some(path.clone()), build_fingerprint());
    if let Some(e) = error {
        eprintln!("warning: {e}");
    }
    println!("last-known-good file: {}", path.display());
    println!("entries: {}", store.data.entries.len());
    for e in &store.data.entries {
        let reason = e
            .reason
            .as_deref()
            .map(|r| format!(" reason={r:?}"))
            .unwrap_or_default();
        println!(
            "  {:?} tier={} strikes={} adapter={:?} backend={} driver={:?} build={} boxel={} \
             wgpu={} updated_unix={}{reason}",
            e.status,
            e.tier,
            e.strikes,
            e.adapter.name,
            e.adapter.backend,
            e.adapter.driver_info,
            e.build,
            e.boxel_version,
            e.wgpu_version,
            e.updated_unix
        );
    }
    ExitCode::SUCCESS
}

fn clear_lkg_file(app: &str) -> ExitCode {
    let Some(path) = LkgStore::default_path(app) else {
        eprintln!("error: no configuration directory for the last-known-good file");
        return ExitCode::from(1);
    };
    // Not through `LkgStore::open`: an unreadable file gives a read-only store whose `save` does
    // nothing. Clearing is an explicit request, so a fresh empty store replaces whatever is
    // there, and "cleared" is printed only after a real save.
    let store = LkgStore::with_path(&path);
    match store.save() {
        Ok(()) => {
            println!("cleared {}", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

fn print_instance(created: &CreatedInstance) {
    println!(
        "compiled-in backends: {}",
        flag_names(&format!("{:?}", wgpu::Instance::enabled_backend_features()))
    );
    println!(
        "instance backends: {}  flags: {:?}{}",
        flag_names(&format!("{:?}", created.backends)),
        created.flags,
        if created.gl_deferred {
            "  (GL deferred: panic = \"abort\" build)"
        } else {
            ""
        }
    );
    for note in &created.notes {
        println!("note: {note}");
    }
}

/// `"Backends(VULKAN | GL)"` -> `"VULKAN | GL"`.
fn flag_names(debug: &str) -> String {
    debug
        .split_once('(')
        .and_then(|(_, rest)| rest.strip_suffix(')'))
        .unwrap_or(debug)
        .to_string()
}

fn yes_no(on: bool) -> &'static str {
    if on { "yes" } else { "no" }
}

fn print_adapter(index: usize, caps: &AdapterCaps, prefs: &GpuPreferences, policy: TierPolicy) {
    let info = &caps.info;
    println!();
    println!("adapter [{index}] {}", info.name);
    println!(
        "  backend={:?} class={:?} vendor=0x{:04x} device=0x{:04x} driver={:?} driver_info={:?}",
        info.backend,
        caps.class(),
        info.vendor,
        info.device,
        info.driver,
        info.driver_info
    );
    if let Some(v) = caps.gl_version {
        let es = if v.es { "ES " } else { "" };
        println!("  gl version: {es}{}.{}", v.major, v.minor);
    }
    let f = caps.features;
    let features = [
        (
            "MULTI_DRAW_INDIRECT_COUNT",
            wgpu::Features::MULTI_DRAW_INDIRECT_COUNT,
        ),
        (
            "INDIRECT_FIRST_INSTANCE",
            wgpu::Features::INDIRECT_FIRST_INSTANCE,
        ),
        ("IMMEDIATES", wgpu::Features::IMMEDIATES),
        ("TIMESTAMP_QUERY", wgpu::Features::TIMESTAMP_QUERY),
        ("SUBGROUP", wgpu::Features::SUBGROUP),
        ("SHADER_INT64", wgpu::Features::SHADER_INT64),
        (
            "TEXTURE_BINDING_ARRAY",
            wgpu::Features::TEXTURE_BINDING_ARRAY,
        ),
        ("PIPELINE_CACHE", wgpu::Features::PIPELINE_CACHE),
        (
            "EXPERIMENTAL_MESH_SHADER",
            wgpu::Features::EXPERIMENTAL_MESH_SHADER,
        ),
    ]
    .map(|(name, flag)| format!("{name}={}", yes_no(f.contains(flag))));
    println!("  features: {}", features.join(" "));
    let d = caps.downlevel.flags;
    let downlevel = [
        ("COMPUTE_SHADERS", wgpu::DownlevelFlags::COMPUTE_SHADERS),
        (
            "INDIRECT_EXECUTION",
            wgpu::DownlevelFlags::INDIRECT_EXECUTION,
        ),
        ("VERTEX_STORAGE", wgpu::DownlevelFlags::VERTEX_STORAGE),
        ("BASE_VERTEX", wgpu::DownlevelFlags::BASE_VERTEX),
    ]
    .map(|(name, flag)| format!("{name}={}", yes_no(d.contains(flag))));
    println!(
        "  downlevel: {} webgpu_compliant={}",
        downlevel.join(" "),
        yes_no(caps.downlevel.is_webgpu_compliant())
    );
    let l = &caps.limits;
    println!(
        "  limits: max_storage_buffer_binding_size={} max_storage_buffers_per_shader_stage={} \
         max_immediate_size={} max_compute_invocations_per_workgroup={} \
         max_compute_workgroup_size_x={} max_texture_array_layers={}",
        l.max_storage_buffer_binding_size,
        l.max_storage_buffers_per_shader_stage,
        l.max_immediate_size,
        l.max_compute_invocations_per_workgroup,
        l.max_compute_workgroup_size_x,
        l.max_texture_array_layers
    );
    let class = classify(caps, policy);
    match class.max_tier {
        Some(max) => println!("  max tier: {max}"),
        None => println!("  max tier: none"),
    }
    for reason in &class.reasons {
        println!("    reason: {reason}");
    }
    if let Some(max) = class.max_tier {
        let power = prefs.power_or_default();
        let scores: Vec<String> = Tier::DESCENDING
            .into_iter()
            .filter(|t| *t <= max)
            .map(|t| format!("{t}={}", score(caps, t, power, None)))
            .collect();
        println!("  scores ({power:?}): {}", scores.join(" "));
    }
}

/// Whether `caps` passes the user's backend / name / software filters (the matrix honors them).
fn wanted(caps: &AdapterCaps, prefs: &GpuPreferences) -> bool {
    let backend_ok = prefs
        .backends
        .is_none_or(|b| b.contains(wgpu::Backends::from(caps.info.backend)));
    let name_ok = prefs
        .adapter
        .as_ref()
        .is_none_or(|n| caps.info.name.to_lowercase().contains(&n.to_lowercase()));
    let software_ok = prefs.software != Some(true) || caps.class() == DeviceClass::Software;
    backend_ok && name_ok && software_ok
}

fn print_tier_matrix(
    adapters: &[wgpu::Adapter],
    caps: &[AdapterCaps],
    prefs: &GpuPreferences,
    policy: TierPolicy,
    smoke: SmokeLevel,
) {
    println!();
    println!("smoke matrix (adapter x tier, smoke {smoke:?}):");
    for (adapter, caps) in adapters.iter().zip(caps) {
        if !wanted(caps, prefs) {
            continue;
        }
        let Some(max) = classify(caps, policy).max_tier else {
            continue;
        };
        for tier in Tier::DESCENDING.into_iter().filter(|t| *t <= max) {
            let start = Instant::now();
            let level = smoke.resolve(caps);
            let result = smoke_one(adapter, caps, tier, level);
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            let outcome = match result {
                Ok(()) => format!("OK ({ms:.1} ms)"),
                Err(e) => format!("FAIL ({e})"),
            };
            println!(
                "  {} [{:?}] x {tier} ({level:?}) -> {outcome}",
                caps.info.name, caps.info.backend
            );
        }
    }
}

/// One device + [`BasicSmokeTest`] for `(adapter, tier)`, exactly as `select_gpu` runs it.
fn smoke_one(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    level: SmokeLevel,
) -> Result<(), String> {
    let request = tier_request(tier, &caps.limits);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("gpu_info"),
        required_features: request.features,
        required_limits: request.limits,
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    }))
    .map_err(|e| format!("request_device: {e}"))?;
    let health = Arc::new(GpuHealth::default());
    install_device_callbacks(&device, &health);
    let ctx = SmokeContext {
        device: &device,
        queue: &queue,
        tier,
        caps,
        level,
        output_format: wgpu::TextureFormat::Rgba8UnormSrgb,
    };
    run_in_error_scopes(&device, || BasicSmokeTest.run(&ctx))?;
    if health.is_lost() {
        return Err(format!(
            "device lost: {}",
            health.lost_reason().unwrap_or_default()
        ));
    }
    match health.last_error() {
        Some(e) => Err(format!("uncaptured error: {e}")),
        None => Ok(()),
    }
}

fn select_and_report(
    created: &CreatedInstance,
    prefs: &GpuPreferences,
    policy: TierPolicy,
    options: &Options,
) -> ExitCode {
    let select = |created: &CreatedInstance| {
        select_gpu(SelectRequest {
            instance: created,
            surface: None,
            prefs,
            policy,
            lkg: None,
            exclusions: &[],
            smoke: &BasicSmokeTest,
            smoke_level: options.smoke,
            create_devices: !options.dry_run,
        })
    };
    let mut result = select(created);
    if !options.dry_run
        && created.gl_deferred
        && matches!(result, Err(SelectError::NoUsableAdapter(_)))
    {
        match create_gl_instance(None::<NoDisplay>, prefs) {
            Ok(gl) => result = select(&gl),
            Err(e) => eprintln!("warning: GL instance: {e}"),
        }
    }
    println!();
    match result {
        Ok(gpu) => {
            println!(
                "smoke level: {:?} (requested {:?})",
                options.smoke.resolve(&gpu.caps),
                options.smoke
            );
            println!("{}", gpu.report.to_text());
            ExitCode::SUCCESS
        }
        Err(SelectError::NoUsableAdapter(report)) => {
            println!("{}", report.to_text());
            if options.dry_run {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

/// Display placeholder for the deferred GL instance (headless: never queried).
#[derive(Clone, Copy, Debug)]
struct NoDisplay;

impl wgpu::rwh::HasDisplayHandle for NoDisplay {
    fn display_handle(&self) -> Result<wgpu::rwh::DisplayHandle<'_>, wgpu::rwh::HandleError> {
        Err(wgpu::rwh::HandleError::Unavailable)
    }
}
