//! The UI pass (ADR 0033, feature `ui`) on every tier. Self-skips without an adapter unless
//! `BOXEL_REQUIRE_GPU=1`.
//!
//! A fixed screen declared with `boxel_ui`'s real widgets at GUI scale 3 (960x720): normal,
//! hovered, focused and disabled nine-slice buttons, a cycle and a slider, shadowed text at two
//! sizes, a tooltip, a translucent dim and panel, list rows clipped to a box, a red gradient
//! drawn as one band per GUI row, and a test sprite with translucent texels drawn stretched,
//! tiled and tinted. For every adapter (filtered by `WGPU_BACKEND` / `WGPU_ADAPTER_NAME`) and
//! every tier a local capability check allows (the one of `gpu_tiers.rs`; Low runs on GL, where
//! the pass reads an sRGB texture with `texelFetch`), `UiPass` draws it over a cleared target.
//! Asserts:
//!
//! - every pixel is `boxel_ui::raster`'s: exactly where only opaque or fully transparent
//!   fragments fell, within one step of a channel where a translucent one did;
//! - every tier of an adapter draws the same bytes;
//! - an idle frame uploads nothing, a new list uploads it once and the atlas not again, another
//!   frame size converts the list again;
//! - an empty paint list opens no render pass, and a terrain frame with it is byte-identical to
//!   the frame without any UI (the UI costs nothing when absent).
//!
//! Failing images are saved under `CARGO_TARGET_TMPDIR/gpu_ui/` (every image with
//! `GPU_UI_SAVE=1`).
#![expect(clippy::print_stderr, reason = "test progress and failure details")]
use boxel_camera::{Camera, CameraView};
use boxel_ecs::prelude::*;
use boxel_gfx::{
    AdapterCaps, GlVersion, RgbaImage, Tier, floors, gpu_tests_required, read_texture_rgba8,
    run_in_error_scopes, submit_and_wait, tier_request,
};
use boxel_math::{DVec3, I64Vec3, IVec2, UVec2};
use boxel_mesh::{ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher};
use boxel_render::{
    DEFAULT_SKY_COLOR, DistanceFog, MAIN_COLOR_FORMAT, RendererConfig, UiPass, ViewParams,
    VoxelRenderer,
};
use boxel_transform::Transform;
use boxel_ui::color::{argb, rgb, srgb_to_linear};
use boxel_ui::raster::{fragment, rasterize};
use boxel_ui::testing::UiTester;
use boxel_ui::{
    Column, Modal, Scaling, SkinSlot, SpriteId, Steps, Text, Ui, UiAtlas, UiDraw, UiLayer, UiQuad,
    UiRect,
};
use boxel_voxel::{BlockRegistry, BlockState, ChunkData, LocalPos};
use boxel_window::KeyCode;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const WIDTH: u32 = 960;
const HEIGHT: u32 = 720;
/// The target's color before the UI (sRGB bytes; cleared as their exact linear values).
const CLEAR: [u8; 4] = [40, 90, 160, 255];

// ---------------------------------------------------------------- adapters

/// Headless instance (`WGPU_BACKEND` or Vulkan + GL) and its adapters filtered by
/// `WGPU_ADAPTER_NAME` (case-insensitive substring).
fn adapters() -> (wgpu::Instance, Vec<wgpu::Adapter>) {
    let instance = wgpu::Instance::new({
        let mut d = wgpu::InstanceDescriptor::new_without_display_handle();
        d.flags = wgpu::InstanceFlags::empty();
        d.backends = wgpu::Backends::from_env()
            .filter(|b| !b.is_empty())
            .unwrap_or(wgpu::Backends::VULKAN | wgpu::Backends::GL);
        d
    });
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
    let wanted = std::env::var("WGPU_ADAPTER_NAME")
        .ok()
        .map(|n| n.to_lowercase())
        .filter(|n| !n.is_empty());
    let adapters = adapters
        .into_iter()
        .filter(|a| {
            wanted
                .as_ref()
                .is_none_or(|n| a.get_info().name.to_lowercase().contains(n))
        })
        .collect();
    (instance, adapters)
}

/// The tiers of `gpu_tiers.rs::allowed_tiers`.
fn allowed_tiers(caps: &AdapterCaps) -> Vec<Tier> {
    let mut tiers = Vec::new();
    let high = wgpu::Features::MULTI_DRAW_INDIRECT_COUNT | wgpu::Features::INDIRECT_FIRST_INSTANCE;
    let storage = caps
        .downlevel
        .flags
        .contains(wgpu::DownlevelFlags::VERTEX_STORAGE)
        && caps.limits.max_storage_buffers_per_shader_stage >= floors::STORAGE_BUFFERS_PER_STAGE
        && (caps.info.backend != wgpu::Backend::Gl
            || caps
                .gl_version
                .is_some_and(GlVersion::supports_storage_and_compute));
    if storage
        && caps.features.contains(high)
        && caps
            .downlevel
            .flags
            .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS)
        && caps.info.backend == wgpu::Backend::Vulkan
    {
        tiers.push(Tier::High);
    }
    if storage {
        tiers.push(Tier::Mid);
    }
    if caps.features.contains(wgpu::Features::IMMEDIATES)
        && caps.limits.max_immediate_size >= floors::IMMEDIATE_SIZE
    {
        tiers.push(Tier::Low);
    }
    tiers
}

/// A device shaped like the one selection would create for `tier`, collecting uncaptured
/// errors.
fn device_for(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
) -> Result<(wgpu::Device, wgpu::Queue, Arc<Mutex<Vec<String>>>), String> {
    let request = tier_request(tier, &caps.limits);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("gpu_ui"),
        required_features: request.features,
        required_limits: request.limits,
        ..Default::default()
    }))
    .map_err(|e| format!("request_device: {e}"))?;
    let errors = Arc::new(Mutex::new(Vec::new()));
    let sink = errors.clone();
    device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| {
        if let Ok(mut errors) = sink.lock() {
            errors.push(e.to_string());
        }
    }));
    Ok((device, queue, errors))
}

fn adapter_label(caps: &AdapterCaps) -> String {
    format!("{} ({:?})", caps.info.name, caps.info.backend)
}

// ---------------------------------------------------------------- the screen

/// The test sprite of the atlas.
#[derive(Resource)]
struct Pattern {
    stretched: SpriteId,
    tiled: SpriteId,
}

/// An 8x8 sprite with every kind of texel: colors in both halves of the sRGB range, opaque,
/// fully transparent and two translucent alphas.
fn pattern_texels() -> Vec<u8> {
    let mut texels = Vec::new();
    for y in 0..8u8 {
        for x in 0..8u8 {
            let alpha = match (x + y) % 5 {
                0 => 0,
                1 => 64,
                3 => 200,
                _ => 255,
            };
            texels.extend([x * 36 + 3, y * 35 + 7, (x ^ y) * 31 + 11, alpha]);
        }
    }
    texels
}

fn screen(mut ui: Ui, pattern: Res<Pattern>) {
    {
        let mut hud = ui.layer(UiLayer::Hud, Modal::No);
        hud.rect(UiRect::new(4, 210, 30, 10), rgb(0xAA2222));
        hud.text(40, 211, "HUD 20/20", Text::default());
        hud.sprite(UiRect::new(4, 222, 24, 12), pattern.tiled);
    }
    let mut p = ui.layer(UiLayer::Screen, Modal::Yes);
    // A red gradient, one band per GUI row (the death screen's background).
    for row in 0..40 {
        let red = 0x50 + row * 3;
        p.rect(
            UiRect::new(0, row, 320, 1),
            [red as u8, 0x10, 0x10, (0x60 + row * 4) as u8],
        );
    }
    p.dim(argb(0x40101010));
    let w = p.gui_size().x;
    p.text_centered(w / 2, 12, "Boxel UI ¿áéíóú ñ?", Text::title(2));
    let mut col = Column::new(w / 2 - 100, 40, 200, 24);
    p.button(col.next(), "Normal");
    p.button(col.next(), "Focused")
        .tooltip("Shown while focused");
    p.button(col.next(), "Hovered");
    p.enabled(false)
        .button(col.next(), "Disabled")
        .tooltip("Not yet, maybe later");
    let [l, r] = col.next().split(4);
    let mut mode = 1u8;
    p.cycle(l, "Mode", &mut mode, &[(0, "A"), (1, "B")]);
    let mut value = 25;
    p.slider(r, "Value", &mut value, Steps::range(0, 100), |v| {
        format!("{v}%")
    });
    // List rows scrolled inside a box: the first and the last are cut.
    p.rect(UiRect::new(8, 160, 120, 42), argb(0x80000000));
    p.with_clip(UiRect::new(10, 162, 116, 38), |p| {
        for row in 0..5 {
            let y = 155 + row * 12;
            p.rect(UiRect::new(10, y, 116, 11), argb(0x30FFFFFF));
            p.text(12, y + 2, &format!("Row {row}: list"), Text::default());
        }
    });
    p.skin(SkinSlot::MenuBackground, UiRect::new(140, 160, 40, 20));
    p.sprite(UiRect::new(190, 160, 40, 30), pattern.stretched);
    p.sprite_tinted(
        UiRect::new(240, 160, 33, 17),
        pattern.stretched,
        [255, 200, 120, 180],
    );
    p.sprite(UiRect::new(280, 160, 30, 30), pattern.tiled);
    p.label(
        UiRect::new(140, 192, 90, 12),
        "A label too long to fit in its box",
        Text::new(rgb(0xFFFF55)),
    );
}

/// The paint list and atlas of the fixed screen, with "Focused" focused (its tooltip shows) and
/// "Hovered" hovered.
fn fixed_screen() -> (UiDraw, UiAtlas) {
    let mut t = UiTester::new(UVec2::new(WIDTH, HEIGHT));
    let pattern = {
        let mut atlas = t.app.world_mut().resource_mut::<UiAtlas>();
        let texels = pattern_texels();
        let stretched = atlas
            .insert_sprite("test:pattern", 8, 8, &texels, Scaling::Stretch)
            .expect("atlas room");
        let tiled = atlas
            .insert_sprite("test:tile", 8, 8, &texels, Scaling::Tile { w: 6, h: 5 })
            .expect("atlas room");
        Pattern { stretched, tiled }
    };
    t.app.insert_resource(pattern);
    t.add_systems(screen);
    t.frame();
    assert_eq!(t.scale().scale(), 3, "GUI scale at {WIDTH}x{HEIGHT}");
    t.tap(KeyCode::Tab).tap(KeyCode::Tab);
    assert_eq!(t.focused().as_deref(), Some("Focused"));
    let hovered = t.hit("Hovered").expect("Hovered").rect;
    t.move_to(IVec2::new(hovered.x + 20, hovered.y + 10))
        .frame()
        .frame();
    assert!(
        !t.state().quads(UiLayer::Popup).is_empty(),
        "the tooltip shows"
    );
    let draw = t.draw().clone();
    let atlas = t.app.world().resource::<UiAtlas>().clone();
    (draw, atlas)
}

/// Per pixel, the translucent fragments (`0 < alpha < 1`) blended over it since the last opaque
/// one. Each such blend may round one step away from the raster's (the GPU's blender and its
/// sRGB store are not the reference's tables), so a pixel may be off by that many steps; a pixel
/// whose last fragment was opaque (or that no fragment touched) is exact.
fn translucent_blends(quads: &[UiQuad], atlas: &UiAtlas, size: UVec2) -> Vec<u8> {
    let mut blends = vec![0u8; (size.x * size.y) as usize];
    for quad in quads {
        let Some(quad) = quad.clipped([0, 0], [size.x as i32, size.y as i32]) else {
            continue;
        };
        for y in quad.min[1]..quad.max[1] {
            for x in quad.min[0]..quad.max[0] {
                let a = fragment(atlas, &quad, quad.texel_at(x, y))[3];
                let at = &mut blends[(y as u32 * size.x + x as u32) as usize];
                if a >= 1.0 {
                    *at = 0;
                } else if a > 0.0 {
                    *at += 1;
                }
            }
        }
    }
    blends
}

// ---------------------------------------------------------------- rendering

/// A [`MAIN_COLOR_FORMAT`] target the UI draws into and the test reads back.
fn target(device: &wgpu::Device, size: UVec2) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gpu_ui target"),
        size: wgpu::Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: MAIN_COLOR_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// Clears `view` to [`CLEAR`] (its exact linear values, so it stores the same bytes).
fn clear(encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
    let c = |i: usize| f64::from(srgb_to_linear(CLEAR[i]));
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("gpu_ui clear"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: c(0),
                    g: c(1),
                    b: c(2),
                    a: f64::from(CLEAR[3]) / 255.0,
                }),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
}

/// Draws `draw` with `pass` over a cleared `size` target and reads it back, with whether the
/// pass recorded a render pass.
fn draw_ui(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    pass: &mut UiPass,
    (draw, atlas): (&UiDraw, &UiAtlas),
    size: UVec2,
) -> Result<(RgbaImage, bool), String> {
    let texture = target(device, size);
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut drew = false;
    run_in_error_scopes(device, || {
        pass.prepare(device, queue, draw, atlas, size);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("gpu_ui"),
        });
        clear(&mut encoder, &view);
        drew = pass.encode(&mut encoder, &view);
        submit_and_wait(device, queue, encoder)
    })?;
    Ok((read_texture_rgba8(device, queue, &texture)?, drew))
}

/// The screens the tiers draw.
struct Screens {
    /// The fixed screen.
    screen: (UiDraw, UiAtlas),
    /// The same screen with nothing focused or hovered (a new list, the same atlas).
    unhovered: UiDraw,
}

/// The images of one tier.
struct TierImages {
    screen: RgbaImage,
    unhovered: RgbaImage,
}

/// Draws the screens with one pass on a device for `tier`, checking its uploads on the way.
fn render_tier(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    screens: &Screens,
) -> Result<TierImages, String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let size = UVec2::new(WIDTH, HEIGHT);
    let (draw, atlas) = (&screens.screen.0, &screens.screen.1);
    let mut pass = UiPass::new(&device);
    let check = |pass: &UiPass, what: &str, uploads: u64, atlas_uploads: u64| {
        let stats = pass.stats();
        if (stats.uploads, stats.atlas_uploads, stats.skipped) == (uploads, atlas_uploads, 0) {
            Ok(())
        } else {
            Err(format!(
                "{what}: {stats:?}, expected {uploads} uploads, {atlas_uploads} of the atlas"
            ))
        }
    };
    let (screen, drew) = draw_ui(&device, &queue, &mut pass, (draw, atlas), size)?;
    if !drew || pass.stats().quads as usize != draw.quads().len() {
        return Err(format!("the screen: drew {drew}, {:?}", pass.stats()));
    }
    check(&pass, "the first frame", 1, 1)?;
    let (idle, _) = draw_ui(&device, &queue, &mut pass, (draw, atlas), size)?;
    check(&pass, "an idle frame", 1, 1)?;
    if idle.data != screen.data {
        return Err("an idle frame drew other pixels".into());
    }
    let (unhovered, _) = draw_ui(
        &device,
        &queue,
        &mut pass,
        (&screens.unhovered, atlas),
        size,
    )?;
    check(&pass, "a new list", 2, 1)?;
    let wider = UVec2::new(WIDTH + 8, HEIGHT);
    draw_ui(
        &device,
        &queue,
        &mut pass,
        (&screens.unhovered, atlas),
        wider,
    )?;
    check(&pass, "a wider frame", 3, 1)?;
    let (empty, drew) = draw_ui(
        &device,
        &queue,
        &mut pass,
        (&UiDraw::default(), atlas),
        size,
    )?;
    if drew || pass.stats().quads != 0 {
        return Err(format!("an empty list: drew {drew}, {:?}", pass.stats()));
    }
    if empty.data.chunks(4).any(|px| px != CLEAR) {
        return Err("an empty list changed pixels".into());
    }
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok(TierImages { screen, unhovered })
}

// ---------------------------------------------------------------- zero cost when absent

/// A stone floor with a cube on it, meshed.
fn terrain(registry: &BlockRegistry, materials: &MaterialTable) -> ChunkMesh {
    let stone = registry.by_name("boxel:stone").expect("stone");
    let mut dense = vec![BlockState::AIR; 32 * 32 * 32];
    for x in 4..20 {
        for z in 4..20 {
            dense[LocalPos::new(x, 8, z).index()] = stone;
        }
    }
    dense[LocalPos::new(12, 9, 12).index()] = stone;
    let chunk = ChunkData::from_dense(&dense);
    let input = MeshInput {
        center: &chunk,
        neighbors: [None; 26],
        light: None,
    };
    Mesher::new().mesh_chunk(&input, materials, MeshOptions::default())
}

fn terrain_view(size: UVec2) -> ViewParams {
    let camera = Camera::default();
    let transform = Transform::from_yaw_pitch(DVec3::new(13.9, 11.3, 15.1), 0.8, -0.5);
    let view = CameraView::compute(&camera, &transform, size.x as f32 / size.y as f32);
    ViewParams::from_camera(&view, &camera, DEFAULT_SKY_COLOR, DistanceFog::default())
}

/// Terrain frames on a device for `tier`: without any UI, with a new pass given an empty list,
/// and with a pass that drew the screen and then got an empty list. All three must be the same
/// bytes, and neither pass may record a render pass for the empty list.
fn terrain_frames(
    adapter: &wgpu::Adapter,
    caps: &AdapterCaps,
    tier: Tier,
    screen: (&UiDraw, &UiAtlas),
) -> Result<[RgbaImage; 3], String> {
    let (device, queue, errors) = device_for(adapter, caps, tier)?;
    let size = UVec2::new(320, 180);
    let registry = boxel_demo::blocks::registry();
    let materials = MaterialTable::from_registry(&registry);
    let config = RendererConfig {
        arena_quads: 1 << 16,
        max_chunk_slots: 64,
        ..RendererConfig::default()
    };
    let mut renderer = VoxelRenderer::new(
        &device,
        &queue,
        tier,
        &config,
        &boxel_render::generate_block_textures(registry.textures()),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        size,
    )
    .map_err(|e| format!("VoxelRenderer::new: {e}"))?;
    renderer.set_model_boxes(&queue, materials.model_boxes());
    renderer
        .upsert_chunk(1, I64Vec3::ZERO, Arc::new(terrain(&registry, &materials)))
        .map_err(|e| e.to_string())?;
    let view = terrain_view(size);
    let empty = UiDraw::default();
    let mut frame = |ui: Option<&mut UiPass>| -> Result<RgbaImage, String> {
        let mut drew = false;
        run_in_error_scopes(&device, || {
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("gpu_ui terrain"),
            });
            renderer.render(&device, &queue, &mut encoder, &view);
            if let Some(pass) = ui {
                pass.prepare(&device, &queue, &empty, screen.1, size);
                drew = pass.encode(&mut encoder, renderer.main_color_view());
            }
            submit_and_wait(&device, &queue, encoder)
        })?;
        if drew {
            return Err("an empty list recorded a render pass".into());
        }
        read_texture_rgba8(&device, &queue, renderer.main_color())
    };
    let plain = frame(None)?;
    let mut fresh = UiPass::new(&device);
    let with_fresh = frame(Some(&mut fresh))?;
    let mut used = UiPass::new(&device);
    used.prepare(&device, &queue, screen.0, screen.1, size);
    if used.stats().quads == 0 {
        return Err("the screen has no quad on a 320x180 frame".into());
    }
    let with_used = frame(Some(&mut used))?;
    let errors = errors.lock().map(|e| e.clone()).unwrap_or_default();
    if !errors.is_empty() {
        return Err(format!("uncaptured errors: {errors:?}"));
    }
    Ok([plain, with_fresh, with_used])
}

// ---------------------------------------------------------------- checks

/// Collects failures and saves the images involved.
struct Report {
    failures: Vec<String>,
    dir: PathBuf,
}

impl Report {
    fn new() -> Self {
        Self {
            failures: Vec::new(),
            dir: PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_ui"),
        }
    }

    fn save(&self, images: &[(&str, &RgbaImage)]) {
        let _ = std::fs::create_dir_all(&self.dir);
        for (name, image) in images {
            let file: String = name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect();
            let path = self.dir.join(format!("{file}.png"));
            match image.save_png(&path) {
                Ok(()) => eprintln!("gpu_ui: saved {}", path.display()),
                Err(e) => eprintln!("gpu_ui: cannot save {}: {e}", path.display()),
            }
        }
    }

    fn fail(&mut self, message: String, images: &[(&str, &RgbaImage)]) {
        eprintln!("gpu_ui: FAIL {message}");
        self.save(images);
        self.failures.push(message);
    }

    /// `image` is the raster `reference`, a pixel off by at most its translucent `blends`
    /// (exact where the last fragment was opaque).
    fn check_raster(
        &mut self,
        what: &str,
        image: (&str, &RgbaImage),
        reference: &RgbaImage,
        blends: &[u8],
    ) {
        let (mut wrong, mut off, mut max_off) = (0, 0, 0u8);
        let mut first = None;
        let pixels = image.1.data.chunks(4).zip(reference.data.chunks(4));
        for (i, (a, b)) in pixels.enumerate() {
            let diff = (0..4).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0);
            max_off = max_off.max(diff);
            off += usize::from(diff > 0);
            if diff > blends[i] {
                wrong += 1;
                let (x, y) = (i as u32 % image.1.width, i as u32 / image.1.width);
                first.get_or_insert((x, y, a.to_vec(), b.to_vec(), blends[i]));
            }
        }
        eprintln!(
            "gpu_ui: {what}: {off} pixels off the raster after translucent blends, {wrong} \
             wrong, max channel {max_off}"
        );
        if let Some((x, y, got, want, blends)) = first {
            self.fail(
                format!(
                    "{what}: {wrong} pixels differ from the raster; first ({x}, {y}): {got:?}, \
                     raster {want:?} after {blends} translucent blends"
                ),
                &[image, ("raster", reference)],
            );
        }
    }

    fn check_identical(&mut self, what: &str, a: (&str, &RgbaImage), b: (&str, &RgbaImage)) {
        let diff = a.1.compare(b.1, 0);
        if diff.differing_pixels > 0 {
            self.fail(
                format!(
                    "{what}: {} pixels differ (max channel {})",
                    diff.differing_pixels, diff.max_channel_diff
                ),
                &[a, b],
            );
        }
    }
}

#[test]
fn the_ui_is_the_raster_on_every_tier() {
    let (_instance, adapters) = adapters();
    if adapters.is_empty() {
        assert!(!gpu_tests_required(), "BOXEL_REQUIRE_GPU=1 but no adapter");
        eprintln!("skipping: no adapter");
        return;
    }
    let (draw, atlas) = fixed_screen();
    let unhovered = {
        // The same screen in a fresh app: nothing focused or hovered.
        let mut t = UiTester::new(UVec2::new(WIDTH, HEIGHT));
        *t.app.world_mut().resource_mut::<UiAtlas>() = atlas.clone();
        let stretched = atlas.find("test:pattern").expect("pattern");
        let tiled = atlas.find("test:tile").expect("tile");
        t.app.insert_resource(Pattern { stretched, tiled });
        t.add_systems(screen);
        t.frames(2);
        t.draw().clone()
    };
    assert_ne!(
        unhovered.quads(),
        draw.quads(),
        "nothing hovered changes the list"
    );
    let size = UVec2::new(WIDTH, HEIGHT);
    let reference = |d: &UiDraw| RgbaImage {
        width: WIDTH,
        height: HEIGHT,
        data: rasterize(d.quads(), &atlas, size, CLEAR),
    };
    let (raster, raster_unhovered) = (reference(&draw), reference(&unhovered));
    let blends = translucent_blends(draw.quads(), &atlas, size);
    let blends_unhovered = translucent_blends(unhovered.quads(), &atlas, size);
    let exact = blends.iter().filter(|&&b| b == 0).count();
    assert!(
        exact > blends.len() / 20 && exact < blends.len(),
        "{exact} of {} pixels end opaque",
        blends.len()
    );
    let screens = Screens {
        screen: (draw, atlas),
        unhovered,
    };

    let mut report = Report::new();
    let mut rendered = 0;
    for adapter in &adapters {
        let caps = AdapterCaps::from_adapter(adapter);
        let label = adapter_label(&caps);
        let mut tiers = Vec::new();
        for tier in allowed_tiers(&caps) {
            match render_tier(adapter, &caps, tier, &screens) {
                Ok(images) => tiers.push((tier, images)),
                Err(e) => report.fail(format!("{label} {tier}: {e}"), &[]),
            }
            match terrain_frames(adapter, &caps, tier, (&screens.screen.0, &screens.screen.1)) {
                Ok([plain, fresh, used]) => {
                    let name = |what: &str| format!("{label}_{tier}_terrain_{what}");
                    report.check_identical(
                        &format!("{label} {tier}: terrain with a new pass and an empty list"),
                        (&name("plain"), &plain),
                        (&name("fresh"), &fresh),
                    );
                    report.check_identical(
                        &format!("{label} {tier}: terrain with a used pass and an empty list"),
                        (&name("plain"), &plain),
                        (&name("used"), &used),
                    );
                }
                Err(e) => report.fail(format!("{label} {tier}: terrain: {e}"), &[]),
            }
        }
        for (tier, i) in &tiers {
            rendered += 1;
            let name = |what: &str| format!("{label}_{tier}_{what}");
            if std::env::var_os("GPU_UI_SAVE").is_some() {
                report.save(&[(&name("screen"), &i.screen), ("raster", &raster)]);
            }
            report.check_raster(
                &format!("{label} {tier}: the screen"),
                (&name("screen"), &i.screen),
                &raster,
                &blends,
            );
            report.check_raster(
                &format!("{label} {tier}: the screen unhovered"),
                (&name("unhovered"), &i.unhovered),
                &raster_unhovered,
                &blends_unhovered,
            );
        }
        let Some(((best, b), rest)) = tiers.split_first() else {
            continue;
        };
        for (tier, t) in rest {
            report.check_identical(
                &format!("{label}: {tier} vs {best}"),
                (&format!("{label}_{best}_screen"), &b.screen),
                (&format!("{label}_{tier}_screen"), &t.screen),
            );
        }
    }
    assert!(
        report.failures.is_empty(),
        "{} failures:\n{}",
        report.failures.len(),
        report.failures.join("\n")
    );
    assert!(
        rendered > 0 || !gpu_tests_required(),
        "BOXEL_REQUIRE_GPU=1 but no tier could render"
    );
}
