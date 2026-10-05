//! GPU contract test for skeleton-implemented helpers (self-skipping).
use boxel_gfx::{RgbaImage, gpu_tests_required, read_texture_rgba8, run_in_error_scopes};

#[test]
fn readback_and_error_scopes() {
    let instance = wgpu::Instance::new({
        let mut d = wgpu::InstanceDescriptor::new_without_display_handle();
        d.flags = wgpu::InstanceFlags::empty();
        d.backends = wgpu::Backends::from_env()
            .filter(|b| !b.is_empty())
            .unwrap_or(wgpu::Backends::VULKAN | wgpu::Backends::GL);
        d
    });
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
    // Honor WGPU_ADAPTER_NAME (case-insensitive substring), like every GPU test.
    let wanted = std::env::var("WGPU_ADAPTER_NAME")
        .ok()
        .map(|n| n.to_lowercase())
        .filter(|n| !n.is_empty());
    let Some(adapter) = adapters.into_iter().find(|a| {
        wanted
            .as_ref()
            .is_none_or(|n| a.get_info().name.to_lowercase().contains(n))
    }) else {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    };
    eprintln!(
        "adapter: {:?} {:?}",
        adapter.get_info().name,
        adapter.get_info().backend
    );
    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .expect("device");
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 70,
            height: 3,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    let mut enc = device.create_command_encoder(&Default::default());
    drop(enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: 1.0,
                    g: 0.0,
                    b: 0.0,
                    a: 1.0,
                }),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    }));
    queue.submit([enc.finish()]);
    let img: RgbaImage = read_texture_rgba8(&device, &queue, &tex).expect("readback");
    assert_eq!((img.width, img.height, img.data.len()), (70, 3, 70 * 3 * 4));
    assert_eq!(&img.data[0..4], &[255, 0, 0, 255]);
    assert_eq!(img.compare(&img, 0).differing_pixels, 0);
    assert_eq!(img.non_background_fraction(), 0.0);
    // PNG round trip (save_png + load_png are part of the frozen contract).
    #[cfg(feature = "png")]
    {
        let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_contract");
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let png = dir.join("readback.png");
        img.save_png(&png).expect("save png");
        assert_eq!(RgbaImage::load_png(&png).expect("load png"), img);
    }
    // an invalid shader is reported, not panicked
    let r = run_in_error_scopes(&device, || {
        let _ = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl("this is not wgsl".into()),
        });
        Ok(())
    });
    assert!(r.is_err());
}

/// `tier_request` produces a request every real adapter accepts (Low everywhere it has
/// IMMEDIATES; High where it has the High features).
#[test]
fn tier_requests_are_accepted_by_real_adapters() {
    let instance = wgpu::Instance::new({
        let mut d = wgpu::InstanceDescriptor::new_without_display_handle();
        d.flags = wgpu::InstanceFlags::empty();
        d.backends = wgpu::Backends::from_env()
            .filter(|b| !b.is_empty())
            .unwrap_or(wgpu::Backends::VULKAN | wgpu::Backends::GL);
        d
    });
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    for adapter in adapters {
        let caps = boxel_gfx::AdapterCaps::from_adapter(&adapter);
        let high =
            wgpu::Features::MULTI_DRAW_INDIRECT_COUNT | wgpu::Features::INDIRECT_FIRST_INSTANCE;
        for tier in [boxel_gfx::Tier::Low, boxel_gfx::Tier::High] {
            let ok = match tier {
                boxel_gfx::Tier::High => {
                    caps.features.contains(high) && caps.info.backend == wgpu::Backend::Vulkan
                }
                _ => caps.features.contains(wgpu::Features::IMMEDIATES),
            };
            if !ok {
                continue;
            }
            let req = boxel_gfx::tier_request(tier, &caps.limits);
            let r = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("tier_request contract"),
                required_features: req.features,
                required_limits: req.limits,
                ..Default::default()
            }));
            eprintln!(
                "{:?} {:?} {tier}: {}",
                caps.info.name,
                caps.info.backend,
                if r.is_ok() { "ok" } else { "FAILED" }
            );
            r.unwrap_or_else(|e| panic!("{} {tier}: {e}", caps.info.name));
        }
    }
}
