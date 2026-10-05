use boxel_gfx::{SmokeContext, SmokeTest};

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
        todo!("WP7")
    }
}
