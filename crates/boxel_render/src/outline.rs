//! The outline of the targeted block (ADR 0026): thin dark lines along the edges of the block's
//! outline boxes (`BlockRegistry::outline`: the cube, a slab's half, a fence's post and arms), as in
//! Minecraft. A game sets [`BlockOutline::block`]; the render plugin reads the block's state and
//! hands its edges to [`VoxelRenderer::set_outline`](crate::VoxelRenderer::set_outline).
//!
//! Drawn the same way on every tier, so the tiers keep giving the same pixels: one pipeline with
//! no bindings (no uniform, no texture: the Low tier limits do not apply), whose vertices are
//! already in clip space. The CPU builds them every frame from the camera: each edge is a quad a
//! few pixels wide facing the screen, cut at the near plane, pulled toward the eye by a
//! 1/1024th of its distance (so it wins the depth test against the faces it lies on, as
//! Minecraft's lines do). It is drawn in the main pass after the opaque and cutout buckets and
//! before the translucent ones, with the depth test: hidden edges stay hidden, and water in
//! front tints it.

use crate::renderer::ViewParams;
use crate::{DEPTH_FORMAT, MAIN_COLOR_FORMAT};
use boxel_ecs::prelude::*;
use boxel_math::{I64Vec3, UVec2, Vec2, Vec3, Vec4};
use boxel_shaders::{ShaderModuleId, create_module};
use boxel_voxel::{BlockCollision, BlockPos, BlockRegistry, BlockState, RenderShape};
use boxel_world::ChunkMap;
use std::collections::HashMap;
use std::sync::Arc;

/// One straight edge of an outline, from one corner to the other, in sixteenths of the block
/// (`0..=16` on each axis, along one axis).
pub type OutlineEdge = [[u8; 3]; 2];

/// The block outline the render plugin draws (ADR 0026). Set [`BlockOutline::block`] to the
/// targeted block (a `PhysicsQuery::raycast_voxels` hit with `VoxelFilter::Outline`), `None`
/// to draw nothing. The plugin reads the block's state every frame, so the outline follows it
/// (a fence that connects, a slab that becomes double); blocks without geometry (air) get none.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct BlockOutline {
    /// The block to outline.
    pub block: Option<BlockPos>,
    /// Line color, linear RGBA, blended over the frame in linear space (the target is sRGB).
    /// The default, black at 67 %, darkens the picture as Minecraft's black at 40 % does in its
    /// non-sRGB framebuffer: to 0.6 of each sRGB value, within 4 levels (of 255) from 40 to 240.
    pub color: [f32; 4],
    /// Line width in physical pixels (default 2).
    pub width: f32,
}

impl Default for BlockOutline {
    fn default() -> Self {
        Self {
            block: None,
            color: [0.0, 0.0, 0.0, 0.67],
            width: 2.0,
        }
    }
}

/// What [`crate::VoxelRenderer::set_outline`] draws: the edges of one block's outline.
#[derive(Clone, Debug, PartialEq)]
pub struct Outline {
    /// The block's world position (its min corner).
    pub block: I64Vec3,
    /// The edges ([`outline_edges`]).
    pub edges: Arc<[OutlineEdge]>,
    /// Line color, linear RGBA.
    pub color: [f32; 4],
    /// Line width in physical pixels.
    pub width: f32,
}

/// The edges of the union of `boxes` (min and max corners in sixteenths, `0..=16`): where its
/// surface bends, as Minecraft draws a block's outline. Faces shared by two boxes (the step on
/// a stair's slab) and the seams of a flat face leave no line. Each edge is a maximal straight
/// run, sorted by axis, then by position; an empty or flat box adds nothing.
pub fn outline_edges(boxes: &[[[u8; 3]; 2]]) -> Vec<OutlineEdge> {
    const N: usize = 16;
    let mut solid = [[[false; N]; N]; N];
    for &[min, max] in boxes {
        let (lo, hi) = (min.map(usize::from), max.map(|v| usize::from(v).min(N)));
        let span = |a: usize| lo[a]..hi[a].max(lo[a]);
        for plane in &mut solid[span(0)] {
            for row in &mut plane[span(1)] {
                row[span(2)].fill(true);
            }
        }
    }
    // A cell, false outside the grid.
    let at = |p: [isize; 3]| {
        p.iter().all(|&v| (0..N as isize).contains(&v))
            && solid[p[0] as usize][p[1] as usize][p[2] as usize]
    };
    let mut edges = Vec::new();
    for axis in 0..3 {
        let (b, c) = match axis {
            0 => (1, 2),
            1 => (0, 2),
            _ => (0, 1),
        };
        for i in 0..=N {
            for j in 0..=N {
                let mut run: Option<usize> = None;
                for k in 0..=N {
                    // The four cells around the segment `k..k + 1` of the line at (i, j): it is
                    // an edge where one or three are solid, or two diagonal ones.
                    let bent = k < N && {
                        let cell = |di: isize, dj: isize| {
                            let mut p = [0isize; 3];
                            p[axis] = k as isize;
                            p[b] = i as isize + di;
                            p[c] = j as isize + dj;
                            at(p)
                        };
                        let (a, bb, cc, d) = (cell(-1, -1), cell(0, -1), cell(-1, 0), cell(0, 0));
                        let count = [a, bb, cc, d].iter().filter(|&&s| s).count();
                        count == 1 || count == 3 || (count == 2 && a == d)
                    };
                    match (bent, run) {
                        (true, None) => run = Some(k),
                        (false, Some(start)) => {
                            let mut from = [0u8; 3];
                            from[b] = i as u8;
                            from[c] = j as u8;
                            let mut to = from;
                            from[axis] = start as u8;
                            to[axis] = k as u8;
                            edges.push([from, to]);
                            run = None;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    edges
}

/// The outline edges of `state`: the edges of its outline boxes
/// ([`BlockRegistry::outline`]); none for a block without geometry (air, invisible blocks).
pub fn block_outline_edges(registry: &BlockRegistry, state: BlockState) -> Vec<OutlineEdge> {
    if registry.shape(state) == RenderShape::None {
        return Vec::new();
    }
    match registry.outline(state) {
        BlockCollision::None => Vec::new(),
        BlockCollision::Full => outline_edges(&[[[0; 3], [16; 3]]]),
        BlockCollision::Boxes(boxes) => {
            outline_edges(&boxes.iter().map(|b| [b.min, b.max]).collect::<Vec<_>>())
        }
    }
}

/// The outline edges of the states outlined so far, for one registry (the render plugin keeps
/// it between frames).
#[derive(Default)]
pub(crate) struct OutlineCache {
    registry: Option<Arc<BlockRegistry>>,
    edges: HashMap<BlockState, Arc<[OutlineEdge]>>,
}

/// What `request` draws in the world `map` made of `registry`: the outline of its block as it
/// is now, or `None` (no block, a block that cannot be read, one without geometry).
pub(crate) fn resolve(
    request: &BlockOutline,
    map: &ChunkMap,
    registry: &Arc<BlockRegistry>,
    cache: &mut OutlineCache,
) -> Option<Outline> {
    let block = request.block?;
    let state = map.block_at(block)?;
    if !cache
        .registry
        .as_ref()
        .is_some_and(|r| Arc::ptr_eq(r, registry))
    {
        cache.registry = Some(Arc::clone(registry));
        cache.edges.clear();
    }
    let edges = Arc::clone(
        cache
            .edges
            .entry(state)
            .or_insert_with(|| block_outline_edges(registry, state).into()),
    );
    (!edges.is_empty()).then(|| Outline {
        block: block.as_i64vec3(),
        edges,
        color: request.color,
        width: request.width,
    })
}

/// How far toward the eye the lines are pulled, as a fraction of their distance (`2^-10`).
const PULL: f32 = 1.0 / 1024.0;

/// One vertex of an outline quad: its clip-space position and color.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct OutlineVertex {
    clip: [f32; 4],
    color: [f32; 4],
}

/// The quads of `outline` for `view` and a target of `size` pixels, appended to `out` (six
/// vertices per visible edge). Plain `f32` arithmetic: the same on every tier.
fn build_quads(outline: &Outline, view: &ViewParams, size: UVec2, out: &mut Vec<OutlineVertex>) {
    // The block relative to the render origin: a target is near the camera, a far one is skipped
    // (it would not be exact in `f32`).
    let rel = outline.block.wrapping_sub(view.render_origin);
    let near = |v: i64| (-(1 << 20)..=1 << 20).contains(&v);
    if !(near(rel.x) && near(rel.y) && near(rel.z)) {
        return;
    }
    let base = rel.as_vec3();
    let eye = view.eye_rel;
    let size = size.max(UVec2::ONE).as_vec2();
    // Pixels to NDC (the y axis direction does not matter: offsets are symmetric).
    let to_ndc = Vec2::splat(2.0) / size;
    let half = outline.width.max(0.0) * 0.5;
    for edge in outline.edges.iter() {
        let corner = |p: [u8; 3]| {
            let local = Vec3::new(f32::from(p[0]), f32::from(p[1]), f32::from(p[2])) / 16.0;
            let p = base + local;
            let p = p - (p - eye) * PULL;
            view.view_proj * p.extend(1.0)
        };
        let (mut a, mut b) = (corner(edge[0]), corner(edge[1]));
        // Cut at the near plane (`z <= w` in wgpu's clip space; reverse-Z puts it at `z = w`).
        let (da, db) = (a.w - a.z, b.w - b.z);
        if da < 0.0 && db < 0.0 {
            continue;
        }
        if da < 0.0 {
            a = a + (b - a) * (da / (da - db));
        } else if db < 0.0 {
            b = b + (a - b) * (db / (db - da));
        }
        if a.w <= 0.0 || b.w <= 0.0 {
            continue;
        }
        let (sa, sb) = (a.truncate().truncate() / a.w, b.truncate().truncate() / b.w);
        let along = (sb - sa) / to_ndc;
        let length = along.length();
        let dir = if length > 1e-6 {
            along / length
        } else {
            Vec2::X
        };
        let side = Vec2::new(-dir.y, dir.x) * half;
        let ahead = dir * half;
        let offset = |clip: Vec4, px: Vec2| {
            let d = px * to_ndc * clip.w;
            Vec4::new(clip.x + d.x, clip.y + d.y, clip.z, clip.w)
        };
        let v = [
            offset(a, -ahead + side),
            offset(a, -ahead - side),
            offset(b, ahead + side),
            offset(b, ahead - side),
        ];
        for i in [0, 1, 2, 2, 1, 3] {
            out.push(OutlineVertex {
                clip: v[i].to_array(),
                color: outline.color,
            });
        }
    }
}

/// The outline pipeline, its vertex buffer and the outline to draw (one per renderer).
pub(crate) struct OutlinePass {
    pipeline: wgpu::RenderPipeline,
    buffer: Option<wgpu::Buffer>,
    capacity: usize,
    count: u32,
    scratch: Vec<OutlineVertex>,
    outline: Option<Outline>,
}

impl OutlinePass {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let module = create_module(device, ShaderModuleId::Outline);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("boxel outline"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let attributes = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("boxel outline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_outline"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: size_of::<OutlineVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &attributes,
                })],
            },
            primitive: wgpu::PrimitiveState::default(),
            // Tested like the terrain (reverse-Z), and written: water drawn after it stays
            // behind an edge in front of it.
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_outline"),
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
            outline: None,
        }
    }

    /// The outline of the next frames (`None`: none).
    pub(crate) fn set(&mut self, outline: Option<Outline>) {
        self.outline = outline;
    }

    /// Builds and uploads this frame's quads.
    pub(crate) fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &ViewParams,
        size: UVec2,
    ) {
        self.scratch.clear();
        if let Some(outline) = &self.outline {
            build_quads(outline, view, size, &mut self.scratch);
        }
        self.count = u32::try_from(self.scratch.len()).unwrap_or(0);
        if self.scratch.is_empty() {
            return;
        }
        if self.buffer.is_none() || self.capacity < self.scratch.len() {
            let capacity = self.scratch.len().next_power_of_two().max(256);
            self.buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("boxel outline"),
                size: (capacity * size_of::<OutlineVertex>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.capacity = capacity;
        }
        if let Some(buffer) = &self.buffer {
            queue.write_buffer(buffer, 0, bytemuck::cast_slice(&self.scratch));
        }
    }

    /// Draws the uploaded quads into the main pass (it leaves another pipeline and vertex
    /// buffer 0 bound: the translucent draws set their own).
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        let Some(buffer) = self.buffer.as_ref().filter(|_| self.count > 0) else {
            return;
        };
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, buffer.slice(..));
        pass.draw(0..self.count, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(mut edges: Vec<OutlineEdge>) -> Vec<OutlineEdge> {
        edges.sort_unstable();
        edges
    }

    #[test]
    fn blocks_are_outlined_by_their_model() {
        let r = boxel_demo::blocks::registry();
        let s = |name: &str| r.parse_state(name).expect(name);
        assert_eq!(block_outline_edges(&r, s("boxel:stone")).len(), 12);
        assert_eq!(block_outline_edges(&r, s("boxel:stone_slab")).len(), 12);
        assert!(block_outline_edges(&r, BlockState::AIR).is_empty());
        // A fence is picked and outlined a block tall, though it collides 1.5 tall.
        let fence = block_outline_edges(&r, s("boxel:oak_fence[north=true,east=true]"));
        assert_eq!(fence.len(), 18);
        assert!(fence.iter().all(|[a, b]| a[1] <= 16 && b[1] <= 16));
        // A stair is one shape.
        assert_eq!(block_outline_edges(&r, s("boxel:oak_stairs")).len(), 18);
    }

    #[test]
    fn a_cube_has_its_twelve_edges() {
        let edges = outline_edges(&[[[0; 3], [16; 3]]]);
        assert_eq!(edges.len(), 12);
        for [from, to] in &edges {
            let along: Vec<usize> = (0..3).filter(|&a| from[a] != to[a]).collect();
            assert_eq!(along.len(), 1, "{from:?} {to:?}");
            assert_eq!((from[along[0]], to[along[0]]), (0, 16));
            assert!(from.iter().all(|&v| v == 0 || v == 16));
        }
    }

    #[test]
    fn a_stair_is_drawn_as_one_shape() {
        // The slab and the step of an east-facing stair: the step's floor on the slab and the
        // seam across its back leave no line; the union has 18 edges (an L prism).
        let edges = outline_edges(&[[[0, 0, 0], [16, 8, 16]], [[8, 8, 0], [16, 16, 16]]]);
        assert_eq!(edges.len(), 18);
        assert!(!edges.contains(&[[8, 8, 0], [16, 8, 0]]), "inside the face");
        assert!(edges.contains(&[[0, 8, 0], [8, 8, 0]]));
        assert!(
            edges.contains(&[[16, 0, 0], [16, 16, 0]]),
            "the back is one line"
        );
        // The order of the boxes does not matter.
        assert_eq!(
            sorted(edges),
            sorted(outline_edges(&[
                [[8, 8, 0], [16, 16, 16]],
                [[0, 0, 0], [16, 8, 16]],
            ]))
        );
    }

    #[test]
    fn a_fence_post_with_an_arm_and_degenerate_boxes() {
        // A post with an arm north: an L in plan, 18 edges; a flat box adds nothing.
        let fence = [[[6, 0, 6], [10, 16, 10]], [[6, 0, 0], [10, 16, 6]]];
        assert_eq!(outline_edges(&fence).len(), 12, "a single box 4 x 16 x 10");
        let corner = [
            [[6, 0, 6], [10, 16, 10]],
            [[10, 0, 6], [16, 16, 10]],
            fence[1],
        ];
        assert_eq!(outline_edges(&corner).len(), 18);
        assert!(outline_edges(&[[[0, 4, 0], [16, 4, 16]]]).is_empty());
        assert!(outline_edges(&[]).is_empty());
    }

    /// A camera at `eye` (in `[0, 1)`) in the block `render_origin`, looking north and down:
    /// the view of the origin's block, moved there (the renderer only sees relative positions).
    fn view(eye: Vec3, render_origin: I64Vec3) -> ViewParams {
        use boxel_camera::{Camera, CameraView};
        use boxel_transform::Transform;
        let camera = Camera::default();
        let transform = Transform::from_yaw_pitch(eye.as_dvec3(), 0.0, -0.5);
        let mut view = ViewParams::from_camera(
            &CameraView::compute(&camera, &transform, 16.0 / 9.0),
            &camera,
            [0.0; 4],
            crate::DistanceFog::default(),
        );
        view.render_origin = render_origin;
        view
    }

    #[test]
    fn quads_are_the_same_anywhere_in_the_world_and_cut_at_the_near_plane() {
        let outline = |block: I64Vec3| Outline {
            block,
            edges: outline_edges(&[[[0; 3], [16; 3]]]).into(),
            color: [0.0, 0.0, 0.0, 0.4],
            width: 2.0,
        };
        let size = UVec2::new(320, 180);
        let quads = |origin: I64Vec3| {
            let mut out = Vec::new();
            let v = view(Vec3::new(0.5, 0.25, 0.75), origin);
            build_quads(
                &outline(origin + I64Vec3::new(0, -2, -3)),
                &v,
                size,
                &mut out,
            );
            out
        };
        let here = quads(I64Vec3::ZERO);
        assert_eq!(here.len(), 12 * 6, "every edge is in front of the camera");
        for far in [
            I64Vec3::splat(1 << 31),
            I64Vec3::new(-(1 << 40), 7, 1 << 40),
            I64Vec3::splat(-(1 << 61)),
        ] {
            assert_eq!(quads(far), here, "{far}");
        }
        // Every vertex is inside the near plane, and the color is the outline's.
        assert!(
            here.iter()
                .all(|v| v.clip[2] <= v.clip[3] && v.clip[3] > 0.0)
        );
        assert!(here.iter().all(|v| v.color == [0.0, 0.0, 0.0, 0.4]));
        // The camera inside the block: the edges behind it are cut or dropped, never wrapped.
        let mut inside = Vec::new();
        let v = view(Vec3::new(0.5, 0.5, 0.5), I64Vec3::ZERO);
        build_quads(&outline(I64Vec3::ZERO), &v, size, &mut inside);
        assert!(inside.len() < here.len());
        assert!(
            inside
                .iter()
                .all(|v| v.clip[2] <= v.clip[3] + 1e-6 && v.clip[3] > 0.0)
        );
        // A block far from the camera is not drawn.
        let mut none = Vec::new();
        build_quads(
            &outline(I64Vec3::new(1 << 30, 0, 0)),
            &view(Vec3::ZERO, I64Vec3::ZERO),
            size,
            &mut none,
        );
        assert!(none.is_empty());
    }
}
