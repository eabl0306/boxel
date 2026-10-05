//! Selection policy without a GPU: `classify` on synthetic adapters, `score`
//! ordering, `SmokeLevel::resolve`, `instance_flags`, `GpuPreferences::from_args`, config files
//! and the LKG default. Environment-driven parts are unit-tested in `src/prefs.rs` (injected
//! lookups, the process environment is never mutated).
use boxel_gfx::{
    AdapterCaps, Classification, DeviceClass, GlVersion, GpuPreferences, LkgStatus, PrefsError,
    PresentModePref, SmokeLevel, Tier, TierPolicy, classify, floors, instance_flags, score,
};
use wgpu::{Backend, DeviceType, DownlevelFlags, Features};

const HIGH_FEATURES: Features = Features::MULTI_DRAW_INDIRECT_COUNT
    .union(Features::INDIRECT_FIRST_INSTANCE)
    .union(Features::IMMEDIATES);

/// Synthetic adapter: `limits` start from `Limits::default()` plus 256 bytes of immediates.
fn adapter(
    name: &str,
    backend: Backend,
    device_type: DeviceType,
    features: Features,
    driver_info: &str,
) -> AdapterCaps {
    let mut info = wgpu::AdapterInfo::new(device_type, backend);
    info.name = name.to_string();
    info.driver_info = driver_info.to_string();
    AdapterCaps {
        gl_version: (backend == Backend::Gl)
            .then(|| GlVersion::parse(driver_info))
            .flatten(),
        info,
        features,
        limits: wgpu::Limits {
            max_immediate_size: 256,
            max_texture_array_layers: 2048,
            max_storage_buffer_binding_size: 2 << 30,
            ..wgpu::Limits::default()
        },
        downlevel: wgpu::DownlevelCapabilities::default(),
    }
}

fn radv() -> AdapterCaps {
    adapter(
        "AMD Radeon 780M Graphics (RADV PHOENIX)",
        Backend::Vulkan,
        DeviceType::IntegratedGpu,
        HIGH_FEATURES,
        "Mesa 26.0.8",
    )
}

fn rtx() -> AdapterCaps {
    adapter(
        "NVIDIA GeForce RTX 5050 Laptop GPU",
        Backend::Vulkan,
        DeviceType::DiscreteGpu,
        HIGH_FEATURES,
        "595.91.07",
    )
}

fn lavapipe() -> AdapterCaps {
    adapter(
        "llvmpipe (LLVM 21.1.8, 256 bits)",
        Backend::Vulkan,
        DeviceType::Cpu,
        HIGH_FEATURES,
        "Mesa 26.0.8 (LLVM 21.1.8)",
    )
}

/// NVIDIA's GL 3.3 context claims `VERTEX_STORAGE | COMPUTE_SHADERS` (all downlevel flags).
fn nvidia_gl() -> AdapterCaps {
    adapter(
        "NVIDIA GeForce RTX 5050 Laptop GPU/PCIe/SSE2",
        Backend::Gl,
        DeviceType::Other,
        Features::IMMEDIATES,
        "3.3.0 NVIDIA 595.91.07",
    )
}

fn radeonsi_gl() -> AdapterCaps {
    adapter(
        "AMD Radeon 780M Graphics (radeonsi, phoenix)",
        Backend::Gl,
        DeviceType::Other,
        Features::IMMEDIATES,
        "4.6 (Core Profile) Mesa 26.0.8",
    )
}

fn max(caps: &AdapterCaps) -> Option<Tier> {
    classify(caps, TierPolicy::default()).max_tier
}

fn has_reason(c: &Classification, text: &str) -> bool {
    c.reasons.iter().any(|r| r.contains(text))
}

#[test]
fn classify_real_adapter_shapes() {
    let c = classify(&radv(), TierPolicy::default());
    assert_eq!(c.max_tier, Some(Tier::High));
    assert!(c.reasons.is_empty(), "{:?}", c.reasons);
    assert_eq!(max(&rtx()), Some(Tier::High));

    let c = classify(&lavapipe(), TierPolicy::default());
    assert_eq!(c.max_tier, Some(Tier::Mid));
    assert_eq!(c.reasons, ["High: software adapters capped at Mid"]);
    let uncapped = TierPolicy {
        software_max: Tier::High,
        ..TierPolicy::default()
    };
    assert_eq!(classify(&lavapipe(), uncapped).max_tier, Some(Tier::High));
    let low_cap = TierPolicy {
        software_max: Tier::Low,
        ..TierPolicy::default()
    };
    let c = classify(&lavapipe(), low_cap);
    assert_eq!(c.max_tier, Some(Tier::Low));
    assert_eq!(
        c.reasons,
        [
            "Mid: software adapters capped at Low",
            "High: software adapters capped at Low"
        ]
    );

    let c = classify(&nvidia_gl(), TierPolicy::default());
    assert_eq!(c.max_tier, Some(Tier::Low));
    assert!(
        c.reasons
            .iter()
            .any(|r| r == "Mid: GL 3.3 < 4.3 (no SSBO/compute in GLSL 330)"),
        "{:?}",
        c.reasons
    );
    assert!(has_reason(&c, "High: no MULTI_DRAW_INDIRECT_COUNT"));

    let c = classify(&radeonsi_gl(), TierPolicy::default());
    assert_eq!(c.max_tier, Some(Tier::Mid));
    assert!(has_reason(&c, "High: no MULTI_DRAW_INDIRECT_COUNT"));

    // Even with MDIC, GL never gets High (the indirect draw is a CPU loop there).
    let mut gl_mdic = radeonsi_gl();
    gl_mdic.features = HIGH_FEATURES;
    let c = classify(&gl_mdic, TierPolicy::default());
    assert_eq!(c.max_tier, Some(Tier::Mid));
    assert!(has_reason(&c, "backend Gl"));

    // GLES 3.0 cannot do SSBOs in the vertex stage; ES 3.1 can.
    let es30 = adapter(
        "es",
        Backend::Gl,
        DeviceType::Other,
        Features::IMMEDIATES,
        "OpenGL ES 3.0 Mesa",
    );
    let c = classify(&es30, TierPolicy::default());
    assert_eq!(c.max_tier, Some(Tier::Low));
    assert!(has_reason(&c, "Mid: GL ES 3.0 < 3.1"), "{:?}", c.reasons);
    let es31 = adapter(
        "es",
        Backend::Gl,
        DeviceType::Other,
        Features::IMMEDIATES,
        "OpenGL ES 3.1 Mesa",
    );
    assert_eq!(max(&es31), Some(Tier::Mid));
    // Unparsable GL version: no Mid.
    let unknown = adapter(
        "gl",
        Backend::Gl,
        DeviceType::Other,
        Features::IMMEDIATES,
        "weird",
    );
    assert_eq!(max(&unknown), Some(Tier::Low));
}

#[test]
fn classify_metal_and_dx12() {
    let metal = adapter(
        "Apple M3",
        Backend::Metal,
        DeviceType::IntegratedGpu,
        Features::IMMEDIATES | Features::INDIRECT_FIRST_INSTANCE,
        "",
    );
    let c = classify(&metal, TierPolicy::default());
    assert_eq!(c.max_tier, Some(Tier::Mid));
    assert!(has_reason(&c, "High: no MULTI_DRAW_INDIRECT_COUNT"));

    let dx12 = adapter(
        "NVIDIA GeForce RTX 5050",
        Backend::Dx12,
        DeviceType::DiscreteGpu,
        HIGH_FEATURES,
        "",
    );
    let c = classify(&dx12, TierPolicy::default());
    assert_eq!(c.max_tier, Some(Tier::Mid));
    assert!(has_reason(&c, "dx12_high"), "{:?}", c.reasons);
    let policy = TierPolicy {
        dx12_high: true,
        ..TierPolicy::default()
    };
    assert_eq!(classify(&dx12, policy).max_tier, Some(Tier::High));
}

#[test]
fn classify_without_immediates_is_none() {
    let mut caps = radv();
    caps.features = Features::MULTI_DRAW_INDIRECT_COUNT | Features::INDIRECT_FIRST_INSTANCE;
    let c = classify(&caps, TierPolicy::default());
    assert_eq!(c.max_tier, None);
    assert!(has_reason(&c, "Low: no IMMEDIATES"), "{:?}", c.reasons);

    let mut noop = radv();
    noop.info.backend = Backend::Noop;
    assert_eq!(max(&noop), None);
}

/// The worst adapter that still classifies for `tier`: every limit at its floor, no feature or
/// downlevel flag beyond what the tier needs.
fn at_floor(tier: Tier) -> AdapterCaps {
    let mut limits = match tier {
        Tier::Low => wgpu::Limits::downlevel_webgl2_defaults(),
        Tier::Mid => wgpu::Limits::downlevel_defaults(),
        Tier::High => wgpu::Limits::default(),
    };
    limits.max_immediate_size = floors::IMMEDIATE_SIZE;
    limits.max_vertex_buffers = floors::VERTEX_BUFFERS;
    limits.max_texture_array_layers = floors::TEXTURE_ARRAY_LAYERS;
    let mut flags = DownlevelFlags::empty();
    let mut features = Features::IMMEDIATES;
    if tier >= Tier::Mid {
        limits.max_storage_buffers_per_shader_stage = floors::STORAGE_BUFFERS_PER_STAGE;
        limits.max_storage_buffer_binding_size = floors::STORAGE_BINDING_SIZE;
        flags |= DownlevelFlags::VERTEX_STORAGE;
    }
    if tier == Tier::High {
        limits.max_compute_workgroup_size_x = floors::WORKGROUP_SIZE_X;
        limits.max_compute_invocations_per_workgroup = floors::INVOCATIONS_PER_WORKGROUP;
        limits.max_compute_workgroups_per_dimension = floors::WORKGROUPS_PER_DIMENSION;
        flags |= DownlevelFlags::COMPUTE_SHADERS | DownlevelFlags::INDIRECT_EXECUTION;
        features = HIGH_FEATURES;
    }
    let mut caps = adapter(
        "floor",
        Backend::Vulkan,
        DeviceType::DiscreteGpu,
        features,
        "",
    );
    caps.limits = limits;
    caps.downlevel.flags = flags;
    caps
}

#[test]
fn classify_exactly_at_and_below_every_floor() {
    for tier in Tier::DESCENDING {
        let caps = at_floor(tier);
        assert_eq!(max(&caps), Some(tier), "{tier} floor adapter");
        // The device request for a classified tier never exceeds the adapter.
        let req = boxel_gfx::tier_request(tier, &caps.limits);
        assert!(req.limits.check_limits(&caps.limits));
        assert!(caps.features.contains(req.features));
    }
    type Tweak = fn(&mut AdapterCaps);
    let below: [(Tier, &str, Tweak); 11] = [
        (Tier::Low, "max_immediate_size", |c| {
            c.limits.max_immediate_size = floors::IMMEDIATE_SIZE - 1;
        }),
        (Tier::Low, "max_vertex_buffers", |c| {
            c.limits.max_vertex_buffers = floors::VERTEX_BUFFERS - 1;
        }),
        (Tier::Low, "max_texture_array_layers", |c| {
            c.limits.max_texture_array_layers = floors::TEXTURE_ARRAY_LAYERS - 1;
        }),
        (Tier::Mid, "max_storage_buffers_per_shader_stage", |c| {
            c.limits.max_storage_buffers_per_shader_stage = floors::STORAGE_BUFFERS_PER_STAGE - 1;
        }),
        (Tier::Mid, "max_storage_buffer_binding_size", |c| {
            c.limits.max_storage_buffer_binding_size = floors::STORAGE_BINDING_SIZE - 1;
        }),
        (Tier::Mid, "VERTEX_STORAGE", |c| {
            c.downlevel.flags.remove(DownlevelFlags::VERTEX_STORAGE);
        }),
        (Tier::High, "max_compute_workgroup_size_x", |c| {
            c.limits.max_compute_workgroup_size_x = floors::WORKGROUP_SIZE_X - 1;
        }),
        (Tier::High, "max_compute_invocations_per_workgroup", |c| {
            c.limits.max_compute_invocations_per_workgroup = floors::INVOCATIONS_PER_WORKGROUP - 1;
        }),
        (Tier::High, "max_compute_workgroups_per_dimension", |c| {
            c.limits.max_compute_workgroups_per_dimension = floors::WORKGROUPS_PER_DIMENSION - 1;
        }),
        (Tier::High, "INDIRECT_EXECUTION", |c| {
            c.downlevel.flags.remove(DownlevelFlags::INDIRECT_EXECUTION);
        }),
        (Tier::High, "INDIRECT_FIRST_INSTANCE", |c| {
            c.features.remove(Features::INDIRECT_FIRST_INSTANCE);
        }),
    ];
    for (tier, what, tweak) in below {
        let mut caps = at_floor(tier);
        tweak(&mut caps);
        let c = classify(&caps, TierPolicy::default());
        assert_eq!(
            c.max_tier,
            tier.lower(),
            "{tier} with {what} below its floor"
        );
        assert!(
            c.reasons
                .iter()
                .any(|r| r.starts_with(&format!("{tier}: ")) && r.contains(what)),
            "{tier}/{what}: {:?}",
            c.reasons
        );
    }
}

#[test]
fn score_ordering() {
    let high_perf = wgpu::PowerPreference::HighPerformance;
    let low_power = wgpu::PowerPreference::LowPower;
    let s = |caps: &AdapterCaps, tier, power, lkg| score(caps, tier, power, lkg);
    // Documented values (`docs/architecture.md` section 5.4).
    assert_eq!(s(&rtx(), Tier::High, high_perf, None), 3500);
    assert_eq!(s(&radv(), Tier::High, high_perf, None), 3400);
    assert_eq!(s(&nvidia_gl(), Tier::Low, high_perf, None), 1200);
    assert_eq!(s(&lavapipe(), Tier::Mid, high_perf, None), -2900);
    // The tier dominates.
    for caps in [rtx(), radv(), lavapipe()] {
        assert!(s(&caps, Tier::High, high_perf, None) > s(&caps, Tier::Mid, high_perf, None));
        assert!(s(&caps, Tier::Mid, high_perf, None) > s(&caps, Tier::Low, high_perf, None));
    }
    assert!(s(&radv(), Tier::Mid, high_perf, None) > s(&rtx(), Tier::Low, high_perf, None));
    // Power preference picks discrete or integrated.
    assert!(s(&rtx(), Tier::High, high_perf, None) > s(&radv(), Tier::High, high_perf, None));
    assert!(s(&radv(), Tier::High, low_power, None) > s(&rtx(), Tier::High, low_power, None));
    // `None` behaves like the default.
    assert_eq!(
        s(&rtx(), Tier::High, wgpu::PowerPreference::None, None),
        s(&rtx(), Tier::High, high_perf, None)
    );
    // Software is always last, below every hardware pair of any tier.
    assert!(
        s(&lavapipe(), Tier::High, high_perf, None) < s(&nvidia_gl(), Tier::Low, high_perf, None)
    );
    // Vulkan beats GL on the same class and tier.
    let mut vk_other = nvidia_gl();
    vk_other.info.backend = Backend::Vulkan;
    assert!(s(&vk_other, Tier::Low, high_perf, None) > s(&nvidia_gl(), Tier::Low, high_perf, None));
    // LKG: Good +50; Suspect below the other adapters of its tier, above the next tier.
    let good = s(&radv(), Tier::High, high_perf, Some(LkgStatus::Good));
    assert_eq!(good, s(&radv(), Tier::High, high_perf, None) + 50);
    let suspect = s(&rtx(), Tier::High, high_perf, Some(LkgStatus::Suspect));
    assert!(suspect < s(&radv(), Tier::High, high_perf, None));
    assert!(suspect > s(&rtx(), Tier::Mid, high_perf, None));
    assert!(suspect > s(&radv(), Tier::Mid, high_perf, Some(LkgStatus::Good)));
    for neutral in [LkgStatus::Pending, LkgStatus::Bad] {
        assert_eq!(
            s(&rtx(), Tier::High, high_perf, Some(neutral)),
            s(&rtx(), Tier::High, high_perf, None)
        );
    }
}

#[test]
fn smoke_level_resolution() {
    use SmokeLevel::{Auto, Compile, Draw};
    assert_eq!(Auto.resolve(&nvidia_gl()), Draw);
    assert_eq!(Auto.resolve(&radeonsi_gl()), Draw);
    assert_eq!(Auto.resolve(&lavapipe()), Draw);
    assert_eq!(Auto.resolve(&rtx()), Compile);
    assert_eq!(Auto.resolve(&radv()), Compile);
    assert_eq!(Compile.resolve(&nvidia_gl()), Compile);
    assert_eq!(Draw.resolve(&rtx()), Draw);
    assert_eq!(lavapipe().class(), DeviceClass::Software);
}

#[test]
fn instance_flags_policy() {
    use wgpu::InstanceFlags as F;
    let env_set = |key: &str| std::env::var_os(key).is_some();
    let on = GpuPreferences {
        validation: Some(true),
        ..Default::default()
    };
    let off = GpuPreferences {
        validation: Some(false),
        ..Default::default()
    };
    for prefs in [GpuPreferences::default(), on.clone(), off.clone()] {
        let flags = instance_flags(&prefs);
        if !env_set("WGPU_VALIDATION_INDIRECT_CALL") {
            assert!(
                !flags.contains(F::VALIDATION_INDIRECT_CALL),
                "{prefs:?}: {flags:?}"
            );
        }
        if !env_set("WGPU_GPU_BASED_VALIDATION") {
            assert!(!flags.contains(F::GPU_BASED_VALIDATION));
        }
    }
    if !env_set("WGPU_VALIDATION") && !env_set("WGPU_DEBUG") {
        assert!(instance_flags(&on).contains(F::DEBUG | F::VALIDATION));
        assert!(!instance_flags(&off).intersects(F::DEBUG | F::VALIDATION));
        assert_eq!(
            instance_flags(&GpuPreferences::default()).contains(F::VALIDATION),
            cfg!(debug_assertions)
        );
    }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn from_args_both_syntaxes_and_leftovers() {
    let (p, config, rest) = GpuPreferences::from_args(&args(&[
        "--frames",
        "10",
        "--backend",
        "vulkan,gl",
        "--adapter=NVIDIA GeForce",
        "--power",
        "low",
        "--tier=mid",
        "--software",
        "--validation",
        "--no-lkg",
        "--present",
        "mailbox",
        "--help",
        "--gpu-config=gpu.toml",
        "positional",
    ]))
    .unwrap();
    assert_eq!(
        p.backends,
        Some(wgpu::Backends::VULKAN | wgpu::Backends::GL)
    );
    assert_eq!(p.adapter.as_deref(), Some("NVIDIA GeForce"));
    assert_eq!(p.power, Some(wgpu::PowerPreference::LowPower));
    assert_eq!(p.max_tier, Some(Tier::Mid));
    assert_eq!(p.software, Some(true));
    assert_eq!(p.validation, Some(true));
    assert_eq!(p.use_lkg, Some(false));
    assert_eq!(p.present_mode, Some(PresentModePref::Mailbox));
    assert_eq!(config, Some(std::path::PathBuf::from("gpu.toml")));
    assert_eq!(rest, args(&["--frames", "10", "--help", "positional"]));

    let (p, config, rest) = GpuPreferences::from_args(&args(&[
        "--lkg",
        "--gpu-config",
        "a.toml",
        "--tier",
        "HIGH",
    ]))
    .unwrap();
    assert_eq!(p.use_lkg, Some(true));
    assert_eq!(p.max_tier, Some(Tier::High));
    assert_eq!(config, Some(std::path::PathBuf::from("a.toml")));
    assert!(rest.is_empty());

    let (p, _, rest) = GpuPreferences::from_args(&[]).unwrap();
    assert_eq!(p, GpuPreferences::default());
    assert!(rest.is_empty());
}

#[test]
fn from_args_errors() {
    let err = |list: &[&str]| GpuPreferences::from_args(&args(list)).unwrap_err();
    assert!(matches!(err(&["--tier"]), PrefsError::MissingValue(k) if k == "--tier"));
    assert!(matches!(
        err(&["--adapter", "--help"]),
        PrefsError::MissingValue(_)
    ));
    assert!(matches!(err(&["--backend="]), PrefsError::MissingValue(_)));
    assert!(matches!(
        err(&["--tier", "ultra"]),
        PrefsError::BadValue { key, .. } if key == "--tier"
    ));
    assert!(matches!(
        err(&["--backend", "vulkan,glide"]),
        PrefsError::BadValue { .. }
    ));
    assert!(matches!(
        err(&["--power=turbo"]),
        PrefsError::BadValue { .. }
    ));
    assert!(matches!(
        err(&["--present", "tearing"]),
        PrefsError::BadValue { .. }
    ));
    assert!(matches!(
        err(&["--software=perhaps"]),
        PrefsError::BadValue { .. }
    ));
    // Error messages name the flag and the accepted values.
    let message = err(&["--tier", "ultra"]).to_string();
    assert!(
        message.contains("--tier") && message.contains("high|mid|low"),
        "{message}"
    );
}

#[test]
fn config_file_parsing() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("policy_config");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("gpu.toml");
    std::fs::write(
        &path,
        "title = \"my game\"\n\n[gpu]\nbackends = \"vulkan,gl\"\nadapter = \"radeon\"\n\
         power = \"low\"\nmax_tier = \"mid\"\nsoftware = false\nvalidation = true\n\
         use_lkg = false\npresent_mode = \"immediate\"\n",
    )
    .unwrap();
    let p = GpuPreferences::from_config_file(&path).unwrap();
    assert_eq!(
        p,
        GpuPreferences {
            backends: Some(wgpu::Backends::VULKAN | wgpu::Backends::GL),
            adapter: Some("radeon".to_string()),
            power: Some(wgpu::PowerPreference::LowPower),
            max_tier: Some(Tier::Mid),
            software: Some(false),
            validation: Some(true),
            use_lkg: Some(false),
            present_mode: Some(PresentModePref::Immediate),
        }
    );
    std::fs::write(&path, "[gpu]\nmax_tier = \"mid\"\ntypo = 1\n").unwrap();
    assert!(matches!(
        GpuPreferences::from_config_file(&path),
        Err(PrefsError::Parse { .. })
    ));
    std::fs::write(&path, "[gpu]\npower = \"turbo\"\n").unwrap();
    assert!(matches!(
        GpuPreferences::from_config_file(&path),
        Err(PrefsError::Parse { .. })
    ));
    assert!(matches!(
        GpuPreferences::from_config_file(&dir.join("missing.toml")),
        Err(PrefsError::Io { .. })
    ));
}

#[test]
fn lkg_default_follows_the_build() {
    assert_eq!(
        GpuPreferences::default().lkg_enabled(),
        !cfg!(debug_assertions)
    );
    let on = GpuPreferences {
        use_lkg: Some(true),
        ..Default::default()
    };
    let off = GpuPreferences {
        use_lkg: Some(false),
        ..Default::default()
    };
    assert!(on.lkg_enabled());
    assert!(!off.lkg_enabled());
}
