//! Screen-space overlay rectangles drawn on top of the frame, into the main color target and
//! before the blit (so screenshots include them). Debug overlays (`boxel_debug`'s F3 screen)
//! fill [`OverlayDraw`]; the render plugin uploads and draws it every frame.
//!
//! Works on every tier: one instance buffer (`rect` + `color` per instance), no bind groups,
//! a 4-vertex triangle strip per rectangle (`ShaderModuleId::Overlay`).

use crate::MAIN_COLOR_FORMAT;
use boxel_ecs::prelude::*;
use boxel_math::UVec2;
use boxel_shaders::{ShaderModuleId, create_module};

/// One solid rectangle in physical pixels of the frame, top-left origin.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OverlayRect {
    /// Top-left corner.
    pub min: [f32; 2],
    /// Bottom-right corner (exclusive).
    pub max: [f32; 2],
    /// Linear RGBA, alpha-blended over the frame.
    pub color: [f32; 4],
}

/// Rectangles drawn on top of the next frames, in order (later ones on top). The producer
/// replaces them when its content changes; an empty list draws nothing.
#[derive(Resource, Clone, Debug, Default)]
pub struct OverlayDraw {
    /// Rectangles to draw.
    pub rects: Vec<OverlayRect>,
}

/// GPU instance: clip-space rectangle and color.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuRect {
    rect: [f32; 4],
    color: [f32; 4],
}

/// Pipeline and instance buffer of the overlay (one per device).
pub(crate) struct OverlayPass {
    pipeline: wgpu::RenderPipeline,
    buffer: Option<wgpu::Buffer>,
    capacity: usize,
    count: u32,
    scratch: Vec<GpuRect>,
}

impl OverlayPass {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let module = create_module(device, ShaderModuleId::Overlay);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("boxel overlay"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let attributes = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("boxel overlay"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_overlay"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<GpuRect>() as u64,
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
                entry_point: Some("fs_overlay"),
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
            buffer: None,
            capacity: 0,
            count: 0,
            scratch: Vec::new(),
        }
    }

    /// Converts `rects` (pixels of a `size` target) to clip space and uploads them.
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rects: &[OverlayRect],
        size: UVec2,
    ) {
        self.scratch.clear();
        self.scratch
            .extend(rects.iter().map(|r| to_clip(r, size.max(UVec2::ONE))));
        self.count = u32::try_from(self.scratch.len()).unwrap_or(u32::MAX);
        if self.scratch.is_empty() {
            return;
        }
        if self.buffer.is_none() || self.capacity < self.scratch.len() {
            let capacity = self.scratch.len().next_power_of_two().max(256);
            self.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("boxel overlay rects"),
                size: (capacity * size_of::<GpuRect>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.capacity = capacity;
        }
        if let Some(buffer) = &self.buffer {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(&self.scratch));
        }
    }

    /// Draws the uploaded rectangles into `target` (loads and keeps its content).
    pub(crate) fn encode(&self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        let Some(buffer) = self.buffer.as_ref().filter(|_| self.count > 0) else {
            return;
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("boxel overlay"),
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
        pass.set_vertex_buffer(0, buffer.slice(..));
        pass.draw(0..4, 0..self.count);
    }
}

/// Pixel rectangle (top-left origin) to clip space (y up).
fn to_clip(r: &OverlayRect, size: UVec2) -> GpuRect {
    let (w, h) = (size.x as f32, size.y as f32);
    let x = |px: f32| px / w * 2.0 - 1.0;
    let y = |py: f32| 1.0 - py / h * 2.0;
    GpuRect {
        rect: [x(r.min[0]), y(r.min[1]), x(r.max[0]), y(r.max[1])],
        color: r.color,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_rects_map_to_clip_space_with_y_up() {
        let r = OverlayRect {
            min: [0.0, 0.0],
            max: [100.0, 50.0],
            color: [1.0; 4],
        };
        let g = to_clip(&r, UVec2::new(200, 100));
        assert_eq!(g.rect, [-1.0, 1.0, 0.0, 0.0]);
    }
}
