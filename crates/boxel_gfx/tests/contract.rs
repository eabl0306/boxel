//! Contract tests for skeleton-implemented items (frozen behavior, no GPU needed).
use boxel_gfx::{
    GlVersion, PresentModePref, Tier, choose_alpha_mode, choose_present_mode,
    choose_surface_format, floors, tier_request,
};

/// Synthetic adapter limits sitting exactly at every floor of `tier` (the worst adapter that
/// still classifies for it).
fn at_floor(tier: Tier) -> wgpu::Limits {
    let mut l = match tier {
        Tier::Low => wgpu::Limits::downlevel_webgl2_defaults(),
        Tier::Mid => wgpu::Limits::downlevel_defaults(),
        Tier::High => wgpu::Limits::default(),
    };
    l.max_immediate_size = floors::IMMEDIATE_SIZE;
    l.max_vertex_buffers = floors::VERTEX_BUFFERS;
    l.max_texture_array_layers = floors::TEXTURE_ARRAY_LAYERS;
    if tier >= Tier::Mid {
        l.max_storage_buffers_per_shader_stage = floors::STORAGE_BUFFERS_PER_STAGE;
        l.max_storage_buffer_binding_size = floors::STORAGE_BINDING_SIZE;
    }
    if tier == Tier::High {
        l.max_compute_workgroup_size_x = floors::WORKGROUP_SIZE_X;
        l.max_compute_invocations_per_workgroup = floors::INVOCATIONS_PER_WORKGROUP;
        l.max_compute_workgroups_per_dimension = floors::WORKGROUPS_PER_DIMENSION;
    }
    l
}

#[test]
fn tier_request_never_exceeds_an_adapter_at_the_floors() {
    for tier in Tier::DESCENDING {
        for t in Tier::DESCENDING.into_iter().filter(|t| *t <= tier) {
            let adapter = at_floor(tier);
            let req = tier_request(t, &adapter);
            assert!(
                req.limits.check_limits(&adapter),
                "{t} on a {tier}-floor adapter"
            );
            assert!(req.features.contains(wgpu::Features::IMMEDIATES));
            assert_eq!(
                req.features
                    .contains(wgpu::Features::MULTI_DRAW_INDIRECT_COUNT),
                t == Tier::High
            );
        }
    }
    // The engine-sized limits come from the adapter itself.
    let big = wgpu::Limits {
        max_storage_buffer_binding_size: 4 << 30,
        max_buffer_size: 8 << 30,
        ..Default::default()
    };
    let req = tier_request(Tier::High, &big);
    assert_eq!(req.limits.max_storage_buffer_binding_size, 4 << 30);
    assert_eq!(req.limits.max_buffer_size, 8 << 30);
}

#[test]
fn gl_version_parsing() {
    let p = |s| GlVersion::parse(s).map(|v| (v.major, v.minor, v.es));
    assert_eq!(p("3.3.0 NVIDIA 595.91.07"), Some((3, 3, false)));
    assert_eq!(
        p("4.6 (Core Profile) Mesa 26.0.8-1ubuntu0.3"),
        Some((4, 6, false))
    );
    assert_eq!(
        p("4.5 (Core Profile) Mesa 26.0.8-1ubuntu0.3 (LLVM 21.1.8)"),
        Some((4, 5, false))
    );
    assert_eq!(p("OpenGL ES 3.2 Mesa 25.0"), Some((3, 2, true)));
    assert_eq!(p(""), None);
    assert_eq!(p("Mesa"), None);
    let v = |major, minor, es| GlVersion { major, minor, es };
    assert!(!v(3, 3, false).supports_storage_and_compute());
    assert!(v(4, 3, false).supports_storage_and_compute());
    assert!(v(3, 1, true).supports_storage_and_compute());
    assert!(!v(3, 0, true).supports_storage_and_compute());
}

#[test]
fn surface_choices() {
    use wgpu::TextureFormat as F;
    assert_eq!(
        choose_surface_format(&[
            F::Rgba16Float,
            F::Rgb10a2Unorm,
            F::Rgba8Unorm,
            F::Bgra8UnormSrgb
        ]),
        Some(F::Bgra8UnormSrgb)
    );
    assert_eq!(
        choose_surface_format(&[F::Rgba16Float, F::Rgba8Unorm]),
        Some(F::Rgba8Unorm)
    );
    assert_eq!(
        choose_surface_format(&[F::Rgba16Float, F::Rgb10a2Unorm]),
        None
    );
    use wgpu::CompositeAlphaMode as A;
    assert_eq!(choose_alpha_mode(&[A::PreMultiplied, A::Opaque]), A::Opaque);
    assert_eq!(choose_alpha_mode(&[A::PreMultiplied, A::Inherit]), A::Auto);
    use wgpu::PresentMode as P;
    assert_eq!(
        choose_present_mode(PresentModePref::Vsync, &[P::Mailbox, P::Fifo]),
        P::Fifo
    );
    assert_eq!(
        choose_present_mode(PresentModePref::Mailbox, &[P::Fifo]),
        P::Fifo
    );
    assert_eq!(
        choose_present_mode(PresentModePref::Immediate, &[P::Fifo, P::Mailbox]),
        P::Mailbox
    );
}
