use crate::path::TierPipelines;
use crate::renderer::{RendererConfig, ViewParams, VoxelRenderer};
use crate::targets::{Blit, Targets};
use crate::textures::linear_to_srgb;
use boxel_camera::{Camera, CameraView};
use boxel_gfx::{
    SmokeContext, SmokeLevel, SmokeTest, read_texture_rgba8, run_in_error_scopes, submit_and_wait,
};
use boxel_math::{DVec3, I64Vec3, UVec2};
use boxel_mesh::{BUCKET_COUNT, CUTOUT_BUCKET, ChunkMesh, PackedQuad};
use boxel_transform::Transform;
use boxel_voxel::{BlockRegistry, Face};

/// Edge of the smoke test targets.
const SIZE: u32 = 16;

/// Smoke test that builds the renderer's *real* pipelines for the candidate tier and the blit
/// pipeline for `ctx.output_format` (the exact surface format), records one pass and submits
/// it. With `SmokeLevel::Draw` it builds a real `VoxelRenderer` (1024 quads, 8 slots, 16x16),
/// draws one opaque and one cutout quad, checks the center pixel against the clear color and
/// blits into a 16x16 texture of `ctx.output_format`. Passed to `boxel_gfx::select_gpu` by the
/// render plugin so a tier is only chosen if its actual shaders work on that device.
#[derive(Clone, Copy, Debug, Default)]
pub struct RendererSmokeTest;

impl SmokeTest for RendererSmokeTest {
    fn run(&self, ctx: &SmokeContext<'_>) -> Result<(), String> {
        // Own scopes too (nested inside the caller's), so direct callers also get errors back.
        run_in_error_scopes(ctx.device, || match ctx.level {
            SmokeLevel::Draw => draw(ctx),
            SmokeLevel::Auto | SmokeLevel::Compile => compile(ctx),
        })
    }
}

/// Every pipeline of the tier + the blit for the output format; one main pass and one blit.
fn compile(ctx: &SmokeContext<'_>) -> Result<(), String> {
    let pipelines = TierPipelines::new(ctx.device, ctx.tier);
    let targets = Targets::new(ctx.device, UVec2::splat(SIZE));
    let blit = Blit::new(ctx.device, ctx.output_format, &targets.color_view);
    let output = output_texture(ctx);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("boxel smoke compile"),
        });
    {
        let mut pass = targets.begin_main_pass(&mut encoder, [0.0, 0.0, 0.0, 1.0]);
        pass.set_pipeline(&pipelines.voxel.opaque);
        pass.set_pipeline(&pipelines.voxel.cutout);
    }
    blit.encode(
        &mut encoder,
        &output.create_view(&wgpu::TextureViewDescriptor::default()),
    );
    submit_and_wait(ctx.device, ctx.queue, encoder)
}

/// A real renderer drawing one opaque and one cutout quad under a camera looking down.
fn draw(ctx: &SmokeContext<'_>) -> Result<(), String> {
    let config = RendererConfig {
        arena_quads: 1024,
        max_chunk_slots: 8,
        ..RendererConfig::default()
    };
    let registry = BlockRegistry::default_blocks();
    let mut renderer = VoxelRenderer::new(
        ctx.device,
        ctx.queue,
        ctx.tier,
        &config,
        registry.textures(),
        ctx.output_format,
        UVec2::splat(SIZE),
    )
    .map_err(|e| e.to_string())?;
    renderer
        .upsert_chunk(0, I64Vec3::ZERO, &smoke_mesh(&registry))
        .map_err(|e| e.to_string())?;

    let camera = Camera::default();
    let transform = Transform::from_yaw_pitch(
        DVec3::new(16.0, 24.0, 16.0),
        0.0,
        -std::f32::consts::FRAC_PI_2,
    );
    let view = ViewParams::from_camera(
        &CameraView::compute(&camera, &transform, 1.0),
        &camera,
        config.clear_color,
        0.6,
    );
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("boxel smoke draw"),
        });
    renderer.render(ctx.device, ctx.queue, &mut encoder, &view);
    submit_and_wait(ctx.device, ctx.queue, encoder)?;

    let image = read_texture_rgba8(ctx.device, ctx.queue, renderer.main_color())?;
    let center = (SIZE / 2 * SIZE + SIZE / 2) as usize * 4;
    let pixel = &image.data[center..center + 4];
    let clear = config.clear_color.map(linear_to_srgb);
    if (0..3).all(|c| pixel[c].abs_diff(clear[c]) <= 2) {
        return Err(format!(
            "smoke draw ({} tier): the center pixel {pixel:?} still has the clear color",
            ctx.tier
        ));
    }

    let output = output_texture(ctx);
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("boxel smoke blit"),
        });
    renderer.blit(
        &mut encoder,
        &output.create_view(&wgpu::TextureViewDescriptor::default()),
    );
    submit_and_wait(ctx.device, ctx.queue, encoder)
}

/// One 32x32 +Y stone quad on the chunk floor and one 32x32 leaves quad (cutout bucket) above it.
fn smoke_mesh(registry: &BlockRegistry) -> ChunkMesh {
    let layer = |name: &str| {
        registry
            .textures()
            .iter()
            .position(|t| t.name == name)
            .unwrap_or(0) as u16
    };
    let mut bucket_counts = [0; BUCKET_COUNT];
    bucket_counts[Face::PosY.index()] = 1;
    bucket_counts[CUTOUT_BUCKET] = 1;
    ChunkMesh {
        quads: vec![
            PackedQuad::new([0, 0, 0], 32, 32, Face::PosY, layer("boxel:stone")),
            PackedQuad::new([0, 4, 0], 32, 32, Face::PosY, layer("boxel:leaves")),
        ],
        bucket_counts,
    }
}

/// 16x16 render target of the output format.
fn output_texture(ctx: &SmokeContext<'_>) -> wgpu::Texture {
    ctx.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("boxel smoke output"),
        size: wgpu::Extent3d {
            width: SIZE,
            height: SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: ctx.output_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    })
}
