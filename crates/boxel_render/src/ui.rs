//! The UI pass (feature `ui`, ADR 0033): draws `boxel_ui`'s paint list ([`UiDraw`] over its
//! [`UiAtlas`]) on top of the frame, into the main color target right after the overlay and
//! before the blit, so screenshots include it and menus cover the F3 screen and the crosshair.
//!
//! Every tier draws the same pixels as `boxel_ui::raster`, the CPU reference: one texture read
//! with `textureLoad` at a texel picked by integer arithmetic, a flat tint computed on the CPU
//! from an exact table, fixed-function alpha blending (`ShaderModuleId::Ui`). It binds one
//! texture and nothing else, so it runs on the Low tier and on GL 3.3.
//!
//! Zero cost while unused: [`crate::RenderPlugin`] creates the pass on the first non-empty
//! paint list, opens no render pass while the list is empty, and uploads the list and the atlas
//! only when their generation (or the target size) changes. A device loss drops the pass with
//! the device; the next non-empty list creates it again and uploads everything.

use crate::MAIN_COLOR_FORMAT;
use crate::plugin::{ExtractedFrame, GpuState};
use boxel_ecs::prelude::*;
use boxel_math::UVec2;
use boxel_shaders::{ShaderModuleId, create_module};
use boxel_ui::color::to_linear;
use boxel_ui::{UiAtlas, UiDraw, UiQuad};
use std::sync::Arc;

/// GPU instance of one quad (64 bytes, one instance-step vertex buffer).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct GpuUiQuad {
    /// The drawn rectangle in clip space (min x, min y, max x, max y).
    rect: [f32; 4],
    /// Linear RGBA with straight alpha.
    tint: [f32; 4],
    /// The mapping's size in pixels (x, y), then its remainder at the drawn corner (x, y).
    scale: [u32; 4],
    /// Source texel at the drawn corner | source size << 16 (x, then y); the drawn corner,
    /// x | y << 16; 0.
    src: [u32; 4],
}

/// The quad as the shader draws it on a `target` sized frame, or `None` when nothing of it
/// shows. Only the part inside both the target and the mapping (`origin`..`origin + size`) is
/// drawn, which is all of every quad `boxel_ui` builds.
///
/// The shader picks `s + ((p - corner) * src_size + rem) / size` per axis, where `corner` is
/// the drawn rectangle's top-left pixel, `s` the texel there and `rem` the remainder of the
/// mapping at it: exactly [`UiQuad::texel_at`]'s `src + (p - origin) * src_size / size`, with
/// small numbers wherever the mapping starts. Also `None` for the (absurd) quads whose numbers
/// do not fit the instance: a source past texel 65535 or an intermediate product past 2^32.
pub(crate) fn gpu_quad(quad: &UiQuad, target: UVec2) -> Option<GpuUiQuad> {
    let target = [target.x, target.y];
    let mut corner = [0u32; 2];
    let mut end = [0u32; 2];
    let mut scale = [0u32; 4];
    let mut src = [0u32; 2];
    for axis in 0..2 {
        let size = quad.size[axis];
        if size == 0 {
            return None;
        }
        let origin = i64::from(quad.origin[axis]);
        let lo = i64::from(quad.min[axis]).max(origin).max(0);
        let hi = i64::from(quad.max[axis])
            .min(origin + i64::from(size))
            .min(i64::from(target[axis]));
        if lo >= hi {
            return None;
        }
        let sn = u64::from(quad.src_size[axis]);
        let skipped = (lo - origin) as u64 * sn;
        let (q, rem) = (skipped / u64::from(size), skipped % u64::from(size));
        let s = u64::from(quad.src[axis]) + q;
        let last = (hi - lo - 1) as u64 * sn + rem;
        if lo > 0xffff || s > 0xffff || sn > 0xffff || last > u64::from(u32::MAX) {
            return None;
        }
        corner[axis] = lo as u32;
        end[axis] = hi as u32;
        scale[axis] = size;
        scale[axis + 2] = rem as u32;
        src[axis] = s as u32 | (sn as u32) << 16;
    }
    let (w, h) = (target[0] as f32, target[1] as f32);
    let x = |px: u32| px as f32 / w * 2.0 - 1.0;
    let y = |py: u32| 1.0 - py as f32 / h * 2.0;
    Some(GpuUiQuad {
        rect: [x(corner[0]), y(corner[1]), x(end[0]), y(end[1])],
        tint: to_linear(quad.tint),
        scale,
        src: [src[0], src[1], corner[0] | corner[1] << 16, 0],
    })
}

/// What the shader computes for pixel `(x, y)` of `quad`, in the same 32-bit arithmetic (the
/// instance's mirror, for tests).
#[cfg(test)]
fn shader_texel(quad: &GpuUiQuad, x: u32, y: u32) -> [u32; 2] {
    let corner = [quad.src[2] & 0xffff, quad.src[2] >> 16];
    let pick = |axis: usize, p: u32| {
        let s0 = quad.src[axis] & 0xffff;
        let sn = quad.src[axis] >> 16;
        let e = p.wrapping_sub(corner[axis]);
        s0.wrapping_add(e.wrapping_mul(sn).wrapping_add(quad.scale[axis + 2]) / quad.scale[axis])
    };
    [pick(0, x), pick(1, y)]
}

/// The atlas page as `RenderSystems::Extract` copies it (only when the pass lacks it).
#[derive(Clone, Debug)]
pub(crate) struct AtlasPage {
    size: u32,
    rgba: Arc<Vec<u8>>,
    generation: u64,
}

impl AtlasPage {
    fn of(atlas: &UiAtlas) -> Self {
        Self {
            size: atlas.size(),
            rgba: Arc::clone(atlas.rgba()),
            generation: atlas.generation(),
        }
    }
}

/// What the UI pass needs this frame, copied by `RenderSystems::Extract` only when the pass
/// does not have it yet (taken by `Prepare`, so the main world's atlas is never shared for long
/// and growing it never copies the page).
#[derive(Default)]
pub(crate) struct UiExtract {
    /// A new paint list and its generation (`None`: the app has no `UiDraw` any more).
    list: Option<(Arc<Vec<UiQuad>>, Option<u64>)>,
    /// A new atlas page.
    atlas: Option<AtlasPage>,
}

/// Counters of a [`UiPass`] (published in [`crate::RenderInfo`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UiPassStats {
    /// Quads the pass draws each frame (0: it opens no render pass).
    pub quads: u32,
    /// Paint list uploads since the pass was created.
    pub uploads: u64,
    /// Atlas page uploads since the pass was created.
    pub atlas_uploads: u64,
    /// Quads left out because their numbers do not fit the instance (see
    /// [`UiPass::prepare`]); 0 for every list `boxel_ui` builds.
    pub skipped: u32,
}

/// The atlas page on the GPU.
struct AtlasTexture {
    texture: wgpu::Texture,
    group: wgpu::BindGroup,
}

/// The UI's pipeline, atlas texture and instance buffer (one per device). ECS-free:
/// [`crate::RenderPlugin`] drives it from the extracted frame, and a test or a tool can draw a
/// paint list into any [`MAIN_COLOR_FORMAT`] target with [`UiPass::prepare`] and
/// [`UiPass::encode`].
pub struct UiPass {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    atlas: Option<AtlasTexture>,
    /// Generation of the atlas page last given (uploaded, or refused as too big).
    atlas_generation: Option<u64>,
    /// The paint list last given and its generation.
    quads: Arc<Vec<UiQuad>>,
    draw_generation: Option<u64>,
    /// The instances need converting again (a new list).
    stale: bool,
    /// Target size the instances were converted for.
    target: UVec2,
    buffer: Option<wgpu::Buffer>,
    capacity: usize,
    scratch: Vec<GpuUiQuad>,
    stats: UiPassStats,
    /// A refused atlas or skipped quads were logged.
    warned: bool,
}

impl UiPass {
    /// The pipeline for `device`. Nothing is uploaded until [`UiPass::prepare`].
    pub fn new(device: &wgpu::Device) -> Self {
        let module = create_module(device, ShaderModuleId::Ui);
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("boxel ui atlas"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("boxel ui"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let attributes = wgpu::vertex_attr_array![
            0 => Float32x4, 1 => Float32x4, 2 => Uint32x4, 3 => Uint32x4
        ];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("boxel ui"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_ui"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<GpuUiQuad>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &attributes,
                })],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_ui"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: MAIN_COLOR_FORMAT,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            layout,
            atlas: None,
            atlas_generation: None,
            quads: Arc::default(),
            draw_generation: None,
            stale: false,
            target: UVec2::ZERO,
            buffer: None,
            capacity: 0,
            scratch: Vec::new(),
            stats: UiPassStats::default(),
            warned: false,
        }
    }

    /// Takes `draw` and `atlas` for a `target` sized frame, uploading only what changed since
    /// the last call: the atlas page when its generation changed, the instances when the list's
    /// generation or `target` changed. An idle UI uploads nothing.
    ///
    /// Quads whose numbers do not fit the instance (a source past texel 65535, or a mapping so
    /// large that its texel arithmetic would pass 2^32) are left out and counted in
    /// [`UiPassStats::skipped`]; `boxel_ui` builds none. An atlas larger than the device's
    /// largest texture is refused (logged once): nothing draws until a smaller one comes.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        draw: &UiDraw,
        atlas: &UiAtlas,
        target: UVec2,
    ) {
        if self.atlas_generation != Some(atlas.generation()) {
            self.set_atlas(device, queue, &AtlasPage::of(atlas));
        }
        if self.draw_generation != Some(draw.generation()) {
            self.set_list(Arc::clone(draw.quads()), Some(draw.generation()));
        }
        self.upload(device, queue, target);
    }

    /// Records the draw into `target` (loading and keeping its content). Returns whether it did:
    /// with no quad, or no atlas yet, it opens no render pass at all.
    pub fn encode(&self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) -> bool {
        let (Some(buffer), Some(atlas)) = (self.buffer.as_ref(), self.atlas.as_ref()) else {
            return false;
        };
        if self.stats.quads == 0 {
            return false;
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("boxel ui"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &atlas.group, &[]);
        pass.set_vertex_buffer(0, buffer.slice(..));
        pass.draw(0..4, 0..self.stats.quads);
        true
    }

    /// The pass's counters.
    pub fn stats(&self) -> UiPassStats {
        self.stats
    }

    /// Generation of the paint list last given (`None`: none yet, or the app has no `UiDraw`).
    pub(crate) fn draw_generation(&self) -> Option<u64> {
        self.draw_generation
    }

    /// Generation of the atlas page last given.
    pub(crate) fn atlas_generation(&self) -> Option<u64> {
        self.atlas_generation
    }

    /// Uploads the page (a new texture only when its size changed).
    fn set_atlas(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, page: &AtlasPage) {
        self.atlas_generation = Some(page.generation);
        let max = device.limits().max_texture_dimension_2d;
        if page.size > max {
            if !self.warned {
                self.warned = true;
                tracing::warn!(
                    "the UI atlas is {0}x{0} but this device's largest texture is {max}: the UI \
                     is not drawn",
                    page.size
                );
            }
            self.atlas = None;
            return;
        }
        let extent = wgpu::Extent3d {
            width: page.size,
            height: page.size,
            depth_or_array_layers: 1,
        };
        if self
            .atlas
            .as_ref()
            .is_none_or(|a| a.texture.size() != extent)
        {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("boxel ui atlas"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("boxel ui atlas"),
                layout: &self.layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                }],
            });
            self.atlas = Some(AtlasTexture { texture, group });
        }
        if let Some(atlas) = &self.atlas {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &atlas.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &page.rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * page.size),
                    rows_per_image: Some(page.size),
                },
                extent,
            );
            self.stats.atlas_uploads += 1;
        }
    }

    /// Takes a new paint list (converted at the next [`UiPass::upload`]).
    fn set_list(&mut self, quads: Arc<Vec<UiQuad>>, generation: Option<u64>) {
        self.quads = quads;
        self.draw_generation = generation;
        self.stale = true;
    }

    /// Converts and uploads the instances when the list or the target size changed.
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, target: UVec2) {
        if !self.stale && self.target == target {
            return;
        }
        self.stale = false;
        self.target = target;
        self.scratch.clear();
        self.scratch
            .extend(self.quads.iter().filter_map(|q| gpu_quad(q, target)));
        let drawn = self.quads.iter().filter(|q| shows(q, target)).count();
        self.stats.skipped = u32::try_from(drawn - self.scratch.len()).unwrap_or(u32::MAX);
        if self.stats.skipped > 0 && !self.warned {
            self.warned = true;
            tracing::warn!(
                "{} UI quads do not fit the UI pass's instances and are not drawn",
                self.stats.skipped
            );
        }
        self.stats.quads = u32::try_from(self.scratch.len()).unwrap_or(u32::MAX);
        if self.scratch.is_empty() {
            return;
        }
        if self.buffer.is_none() || self.capacity < self.scratch.len() {
            let capacity = self.scratch.len().next_power_of_two().max(256);
            self.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("boxel ui quads"),
                size: (capacity * size_of::<GpuUiQuad>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.capacity = capacity;
        }
        if let Some(buffer) = &self.buffer {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(&self.scratch));
            self.stats.uploads += 1;
        }
    }
}

/// Whether some pixel of `quad` inside its mapping lands on a `target` sized frame.
fn shows(quad: &UiQuad, target: UVec2) -> bool {
    let target = [target.x, target.y];
    (0..2).all(|axis| {
        let origin = i64::from(quad.origin[axis]);
        let lo = i64::from(quad.min[axis]).max(origin).max(0);
        let hi = i64::from(quad.max[axis])
            .min(origin + i64::from(quad.size[axis]))
            .min(i64::from(target[axis]));
        lo < hi
    })
}

/// `RenderSystems::Extract`: copies the paint list and the atlas page when the pass does not
/// have their generation yet (an `Arc` each, never the data), see [`UiExtract::wanted`].
pub(crate) fn extract_ui(
    draw: Option<Res<UiDraw>>,
    atlas: Option<Res<UiAtlas>>,
    gpu: Res<GpuState>,
    mut frame: ResMut<ExtractedFrame>,
) {
    frame.ui = match gpu.ui_pass() {
        Some(pass) => UiExtract::wanted(
            pass.map(|p| PassHas {
                draw: p.draw_generation(),
                atlas: p.atlas_generation(),
            }),
            draw.as_deref().zip(atlas.as_deref()),
        ),
        // No usable device: nothing to give.
        None => UiExtract::default(),
    };
}

/// The generations a pass has (`None`: never given, or the UI went away).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PassHas {
    draw: Option<u64>,
    atlas: Option<u64>,
}

impl UiExtract {
    /// What a pass in state `pass` (`None`: no pass yet on this device) needs from the app's
    /// `ui` resources: nothing while there is no pass and the list is empty (the pass is
    /// created on the first non-empty one); else the list and the page whose generation the
    /// pass lacks, all of them for a pass recreated after a device loss; an empty list for a
    /// pass whose app has no UI any more.
    fn wanted(pass: Option<PassHas>, ui: Option<(&UiDraw, &UiAtlas)>) -> Self {
        let mut out = Self::default();
        let Some((draw, atlas)) = ui else {
            if pass.is_some_and(|p| p.draw.is_some()) {
                out.list = Some((Arc::default(), None));
            }
            return out;
        };
        if pass.is_none() && draw.is_empty() {
            return out;
        }
        let has = pass.unwrap_or_default();
        if has.draw != Some(draw.generation()) {
            out.list = Some((Arc::clone(draw.quads()), Some(draw.generation())));
        }
        if has.atlas != Some(atlas.generation()) {
            out.atlas = Some(AtlasPage::of(atlas));
        }
        out
    }
}

/// `RenderSystems::Prepare`: creates the pass on the first non-empty list, then uploads what
/// `extract_ui` copied and converts the instances for a `target` sized frame when needed.
pub(crate) fn prepare_ui(
    pass: &mut Option<UiPass>,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    ui: &mut UiExtract,
    target: UVec2,
) {
    let list = ui.list.take();
    let atlas = ui.atlas.take();
    if pass.is_none() && list.as_ref().is_none_or(|(quads, _)| quads.is_empty()) {
        return;
    }
    let pass = pass.get_or_insert_with(|| UiPass::new(device));
    if let Some(page) = atlas {
        pass.set_atlas(device, queue, &page);
    }
    if let Some((quads, generation)) = list {
        pass.set_list(quads, generation);
    }
    pass.upload(device, queue, target);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every drawn pixel of `quad` gets `texel_at`'s texel from the shader's arithmetic.
    fn assert_same_texels(quad: &UiQuad, target: UVec2) {
        let gpu = gpu_quad(quad, target).expect("drawable");
        let corner = [gpu.src[2] & 0xffff, gpu.src[2] >> 16];
        let x0 = quad.min[0].max(quad.origin[0]).max(0);
        let y0 = quad.min[1].max(quad.origin[1]).max(0);
        assert_eq!(corner, [x0 as u32, y0 as u32]);
        let x1 = quad.max[0].min(target.x as i32);
        let y1 = quad.max[1].min(target.y as i32);
        for y in y0..y1 {
            for x in x0..x1 {
                assert_eq!(
                    shader_texel(&gpu, x as u32, y as u32),
                    quad.texel_at(x, y),
                    "pixel ({x}, {y}) of {quad:?}"
                );
            }
        }
    }

    #[test]
    fn instances_are_64_bytes() {
        assert_eq!(size_of::<GpuUiQuad>(), 64);
    }

    #[test]
    fn the_drawn_rect_maps_to_clip_space_with_y_up() {
        let quad = UiQuad::solid([0, 0], [100, 50], [255; 4]);
        let gpu = gpu_quad(&quad, UVec2::new(200, 100)).unwrap();
        assert_eq!(gpu.rect, [-1.0, 1.0, 0.0, 0.0]);
        assert_eq!(gpu.tint, [1.0; 4]);
        // Only what lands on the target is drawn.
        let quad = UiQuad::solid([-10, 75], [10, 120], [255; 4]);
        let gpu = gpu_quad(&quad, UVec2::new(200, 100)).unwrap();
        assert_eq!(gpu.rect, [-1.0, -0.5, -0.9, -1.0]);
        assert!(gpu_quad(&quad, UVec2::new(200, 75)).is_none());
        assert!(gpu_quad(&UiQuad::solid([5, 5], [5, 9], [255; 4]), UVec2::new(9, 9)).is_none());
    }

    #[test]
    fn the_tint_is_the_exact_linear_table() {
        let quad = UiQuad::solid([0, 0], [1, 1], [0x33, 0x80, 0xff, 128]);
        let gpu = gpu_quad(&quad, UVec2::new(4, 4)).unwrap();
        assert_eq!(gpu.tint, to_linear([0x33, 0x80, 0xff, 128]));
    }

    #[test]
    fn stretched_tiled_and_clipped_quads_pick_texel_at() {
        let target = UVec2::new(64, 48);
        // 3 texels over 10 pixels, an 8-pixel tile at 1:1, a 4x upscale, a 16 -> 5 shrink.
        let quads = [
            UiQuad::mapped([5, 2], [10, 7], [7, 3], [3, 2], [255; 4]),
            UiQuad::mapped([0, 0], [8, 8], [100, 40], [8, 8], [255; 4]),
            UiQuad::mapped([20, 10], [32, 32], [9, 9], [8, 8], [255; 4]),
            UiQuad::mapped([1, 30], [5, 5], [0, 0], [16, 16], [255; 4]),
        ];
        for quad in quads {
            assert_same_texels(&quad, target);
            for clip in [([7, 3], [13, 8]), ([22, 12], [40, 30]), ([2, 31], [4, 33])] {
                if let Some(clipped) = quad.clipped(clip.0, clip.1) {
                    assert_same_texels(&clipped, target);
                }
            }
        }
    }

    #[test]
    fn a_mapping_that_starts_far_off_screen_stays_exact() {
        // A huge stretched quad whose origin is a million pixels up and left: only its corner
        // lands on the frame, and the shader's numbers stay small.
        let quad = UiQuad::mapped(
            [-1_000_000, -999_983],
            [1_000_640, 1_000_480],
            [3, 5],
            [7, 11],
            [255; 4],
        );
        let quad = quad.clipped([0, 0], [640, 480]).unwrap();
        assert_same_texels(&quad, UVec2::new(640, 480));
    }

    #[test]
    fn numbers_past_the_instance_are_refused() {
        let target = UVec2::new(64, 64);
        let far_source = UiQuad::mapped([0, 0], [4, 4], [70_000, 0], [4, 4], [255; 4]);
        assert!(gpu_quad(&far_source, target).is_none());
        assert!(shows(&far_source, target));
        let zero = UiQuad {
            size: [0, 4],
            ..UiQuad::solid([0, 0], [4, 4], [255; 4])
        };
        assert!(gpu_quad(&zero, target).is_none());
        // Drawn outside its own mapping: only the mapped part draws.
        let quad = UiQuad {
            min: [0, 0],
            max: [20, 20],
            ..UiQuad::mapped([5, 5], [4, 4], [1, 1], [2, 2], [255; 4])
        };
        let gpu = gpu_quad(&quad, target).unwrap();
        assert_eq!(gpu.src[2], 5 | 5 << 16);
        assert_eq!(
            gpu.rect,
            gpu_quad(
                &UiQuad::mapped([5, 5], [4, 4], [1, 1], [2, 2], [255; 4]),
                target
            )
            .unwrap()
            .rect
        );
    }

    /// A UI app with one button; `hover` puts the cursor on it (another list).
    fn ui_app() -> boxel_ui::testing::UiTester {
        use boxel_ui::{Modal, Ui, UiLayer, UiRect};
        let mut t = boxel_ui::testing::UiTester::new(UVec2::new(640, 480));
        t.add_systems(|mut ui: Ui| {
            let mut p = ui.layer(UiLayer::Screen, Modal::Yes);
            p.button(UiRect::new(10, 10, 100, 20), "test:play");
        });
        t.frame();
        t
    }

    fn resources(t: &boxel_ui::testing::UiTester) -> (UiDraw, UiAtlas) {
        let world = t.app.world();
        (
            world.resource::<UiDraw>().clone(),
            world.resource::<UiAtlas>().clone(),
        )
    }

    fn generations(e: &UiExtract) -> (Option<Option<u64>>, Option<u64>) {
        (
            e.list.as_ref().map(|(_, generation)| *generation),
            e.atlas.as_ref().map(|page| page.generation),
        )
    }

    #[test]
    fn the_extract_gives_a_pass_only_what_it_lacks() {
        let mut t = ui_app();
        let (draw, atlas) = resources(&t);
        let (d, a) = (draw.generation(), atlas.generation());
        assert!(!draw.is_empty());
        let has = |draw, atlas| Some(PassHas { draw, atlas });
        let wanted = |pass, ui| generations(&UiExtract::wanted(pass, ui));

        // No pass yet: an empty list creates none, the first non-empty one gets everything
        // (also after a device loss dropped the pass).
        let empty = UiDraw::default();
        assert_eq!(wanted(None, Some((&empty, &atlas))), (None, None));
        assert_eq!(
            wanted(None, Some((&draw, &atlas))),
            (Some(Some(d)), Some(a))
        );
        // A pass with everything: an idle frame copies nothing.
        assert_eq!(
            wanted(has(Some(d), Some(a)), Some((&draw, &atlas))),
            (None, None)
        );
        // A new list, or a new atlas, alone.
        let hit = t.hit("test:play").unwrap().rect;
        t.move_to(boxel_math::IVec2::new(hit.x + 1, hit.y + 1))
            .frame();
        let (hovered, _) = resources(&t);
        assert_ne!(hovered.generation(), d);
        assert_eq!(
            wanted(has(Some(d), Some(a)), Some((&hovered, &atlas))),
            (Some(Some(hovered.generation())), None)
        );
        assert_eq!(
            wanted(has(Some(d), Some(a - 1)), Some((&draw, &atlas))),
            (None, Some(a))
        );
        // An existing pass follows the list down to empty (it then draws nothing).
        assert_eq!(
            wanted(has(Some(d), Some(a)), Some((&empty, &atlas))),
            (Some(Some(0)), None)
        );
        // The UI resources went away: a pass with a list gets an empty one, once.
        assert_eq!(wanted(has(Some(d), Some(a)), None), (Some(None), None));
        assert_eq!(wanted(has(None, Some(a)), None), (None, None));
        assert_eq!(wanted(None, None), (None, None));
    }

    proptest::proptest! {
        /// Any mapping, clipped anywhere on a frame up to 2048 pixels wide: the shader's 32-bit
        /// arithmetic picks `texel_at`'s texel for every drawn pixel.
        #[test]
        fn the_shader_arithmetic_is_texel_at(
            origin in -3_000_000i32..3000, size in 1u32..4_000_000, src in 0u32..4096,
            src_size in 1u32..4096, clip_lo in 0i32..2048, clip_len in 1i32..2048,
            width in 1u32..2048,
        ) {
            let target = UVec2::new(width, 1);
            let quad = UiQuad::mapped([origin, 0], [size, 1], [src, 0], [src_size, 1], [255; 4]);
            if let Some(quad) = quad.clipped([clip_lo, 0], [clip_lo + clip_len, 1]) {
                proptest::prop_assume!(shows(&quad, target));
                assert_same_texels(&quad, target);
            }
        }
    }
}
