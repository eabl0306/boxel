use crate::path::TierPipelines;
use crate::renderer::{DEFAULT_SKY_COLOR, RendererConfig, ViewParams, VoxelRenderer};
use crate::sort::TranslucentSort;
use crate::targets::{Blit, Targets};
use crate::textures::{generate_block_textures, linear_to_srgb};
use boxel_camera::{Camera, CameraView};
use boxel_gfx::{
    SmokeContext, SmokeLevel, SmokeTest, read_texture_rgba8, run_in_error_scopes, submit_and_wait,
};
use boxel_math::{DVec3, I64Vec3, UVec2};
use boxel_mesh::{BUCKET_COUNT, CUTOUT_BUCKET, ChunkMesh, PackedQuad, palette_entry};
use boxel_transform::Transform;
use boxel_voxel::{BlockRegistry, Face, LightValue};
use std::sync::Arc;

/// Edge of the smoke test targets.
const SIZE: u32 = 16;

/// Smoke test that builds the renderer's *real* pipelines for the candidate tier and the blit
/// pipeline for `ctx.output_format` (the exact surface format), records one pass and submits
/// it. With `SmokeLevel::Draw` it builds a real `VoxelRenderer` (1024 quads, 8 slots, 16x16),
/// draws an opaque quad lit only by blue block light (palette entry 0) under a smaller cutout
/// quad lit only by red block light (entry 1), checks that the center pixel (the cutout quad)
/// is not the clear color and is lit red and that a corner pixel (the opaque quad alone) is
/// lit blue, and blits into a 16x16 texture of `ctx.output_format`. So each quad's light entry
/// reaches the fragment shader on this tier: from the arena on High and Mid, from the second
/// vertex buffer on Low, offset by the draw's first instance like the quads (the cutout draw
/// starts at instance 1; GL emulates that offset per buffer). Passed to `boxel_gfx::select_gpu`
/// by the render plugin so a tier is only chosen if its actual shaders work on that device.
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
        translucent_sort: TranslucentSort::Inline,
    };
    let registry = BlockRegistry::default_blocks();
    let mut renderer = VoxelRenderer::new(
        ctx.device,
        ctx.queue,
        ctx.tier,
        &config,
        &generate_block_textures(registry.textures()),
        ctx.output_format,
        UVec2::splat(SIZE),
    )
    .map_err(|e| e.to_string())?;
    renderer
        .upsert_chunk(0, I64Vec3::ZERO, Arc::new(smoke_mesh(&registry)))
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
        DEFAULT_SKY_COLOR,
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
    let at = |x: u32, y: u32| {
        let i = (y * SIZE + x) as usize * 4;
        &image.data[i..i + 4]
    };
    let (center, corner) = (at(SIZE / 2, SIZE / 2), at(2, 2));
    let clear = DEFAULT_SKY_COLOR.map(linear_to_srgb);
    if (0..3).all(|c| center[c].abs_diff(clear[c]) <= 2) {
        return Err(format!(
            "smoke draw ({} tier): the center pixel {center:?} still has the clear color",
            ctx.tier
        ));
    }
    // No sky light reaches the quads and each one's block light is a pure color, so a working
    // light path leaves the other channels at the ambient floor (stone: about 125 in its
    // channel, 13 in the others).
    for (what, pixel, channel, color) in [
        ("center pixel (cutout quad)", center, 0, "red"),
        ("corner pixel (opaque quad)", corner, 2, "blue"),
    ] {
        if !lit_by(pixel, channel) {
            return Err(format!(
                "smoke draw ({} tier): the {what} {pixel:?} is not lit by its {color} block light",
                ctx.tier
            ));
        }
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

/// True if `pixel` (sRGB) is clearly lit by light of one color, channel `c` (0 red, 1 green,
/// 2 blue): that channel at least 40 and more than twice each of the other two.
fn lit_by(pixel: &[u8], c: usize) -> bool {
    let rgb = [pixel[0], pixel[1], pixel[2]].map(u16::from);
    let others = (0..3)
        .filter(|&i| i != c)
        .map(|i| rgb[i])
        .max()
        .unwrap_or(0);
    rgb[c] >= 40 && rgb[c] > 2 * others
}

/// One 32x32 +Y stone quad on the chunk floor lit only by blue block light (palette entry 0),
/// and a 16x16 stone quad in the cutout bucket above its middle lit only by red block light
/// (entry 1; stone passes the alpha test everywhere). Sky 0 in both.
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
    let stone = layer("boxel:stone");
    let mut mesh = ChunkMesh::from_quads(
        vec![
            PackedQuad::new([0, 0, 0], 32, 32, Face::PosY, stone).with_light_index(0),
            PackedQuad::new([8, 4, 8], 16, 16, Face::PosY, stone).with_light_index(1),
        ],
        bucket_counts,
    );
    mesh.palette = vec![
        palette_entry([LightValue::from_rgb([0, 0, 15]); 4]),
        palette_entry([LightValue::from_rgb([15, 0, 0]); 4]),
    ];
    mesh.hash = mesh.compute_hash();
    mesh
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_smoke_quads_have_their_own_light_entries() {
        let registry = BlockRegistry::default_blocks();
        let mesh = smoke_mesh(&registry);
        assert_eq!(mesh.palette.len(), 2);
        // Opaque bucket first (first instance 0), then the cutout one (first instance 1).
        assert_eq!(mesh.bucket_counts[Face::PosY.index()], 1);
        assert_eq!(mesh.bucket_counts[CUTOUT_BUCKET], 1);
        let lights: Vec<_> = mesh.quads.iter().map(|&q| mesh.corner_light(q)).collect();
        assert_eq!(
            lights,
            [
                [LightValue::from_rgb([0, 0, 15]); 4],
                [LightValue::from_rgb([15, 0, 0]); 4]
            ]
        );
        assert!(lit_by(&[124, 13, 13, 255], 0));
        assert!(lit_by(&[13, 13, 124, 255], 2));
        assert!(!lit_by(&[13, 13, 124, 255], 0), "blue is not red");
        assert!(!lit_by(&[120, 110, 100, 255], 0), "white light");
        assert!(!lit_by(&[13, 13, 13, 255], 2), "dark");
    }
}
