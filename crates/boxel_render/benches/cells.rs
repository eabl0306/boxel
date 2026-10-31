//! The CPU half of the moving cell quads (ADR 0054) in one frame, as the render plugin hands them
//! to the renderer: the list compared with the last frame's and, when it changed, its vertices
//! written (`boxel_render::bench::MovingBench`).
//!
//! - `cells/moving/N`: `N` quads that change every frame (a lid opening: its quads turn), all
//!   written again.
//! - `cells/still/N`: the same `N` quads as the last frame: compared, nothing written.
//!
//! - `cells/sort/N`: `N` translucent quads (ADR 0055) sorted for an eye that moves every frame:
//!   their distances, the order and the indices.
//!
//! An open chest gives about 30 quads, a beacon's beam 4 per part of its color, a ringing bell
//! about 12; a stained glass block in a frame 6, a banner 6 per pattern.
use boxel_camera::{Camera, CameraView};
use boxel_math::DVec3;
use boxel_render::MovingCellQuad;
use boxel_render::bench::{MovingBench, SortBench};
use boxel_render::{DistanceFog, ViewParams};
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, CellBlend, CellImage, CellQuad, Face, LightValue};
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

/// `n` quads over cells in a row, turned by `angle`.
fn quads(n: usize, angle: f32) -> Vec<(MovingCellQuad, LightValue)> {
    (0..n)
        .map(|i| {
            let (s, c) = (angle + i as f32 * 0.1).sin_cos();
            let quad = CellQuad {
                corners: [
                    [0.0, 0.5, 0.0],
                    [c, 0.5 + s, 0.0],
                    [0.0, 0.5, 1.0],
                    [c, 0.5 + s, 1.0],
                ],
                uv: [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]],
                image: (i % 64) as u16,
                source: if i % 2 == 0 {
                    CellImage::Block
                } else {
                    CellImage::Cell
                },
                face: Face::PosY,
                full_bright: false,
                tint: [255; 3],
                blend: CellBlend::Cutout,
            };
            let cell = BlockPos::new((i / 30) as i64, 64, 0);
            (MovingCellQuad { cell, quad }, LightValue::FULL_SKY)
        })
        .collect()
}

fn bench(c: &mut Criterion) {
    for n in [30, 1000] {
        // Two frames of a turn, one after the other: each differs from the last.
        let frames = [quads(n, 0.0), quads(n, 0.01)];
        let mut moving = MovingBench::default();
        let mut k = 0;
        c.bench_function(&format!("cells/moving/{n}"), |b| {
            b.iter(|| {
                k ^= 1;
                black_box(moving.frame(&frames[k]))
            });
        });
        let still = quads(n, 0.0);
        let mut moving = MovingBench::default();
        moving.frame(&still);
        c.bench_function(&format!("cells/still/{n}"), |b| {
            b.iter(|| black_box(moving.frame(black_box(&still))));
        });
    }
}

/// A view from `eye` looking along +x.
fn view_at(eye: DVec3) -> ViewParams {
    let camera = Camera::default();
    let transform = Transform::from_yaw_pitch(eye, -std::f32::consts::FRAC_PI_2, 0.0);
    let view = CameraView::compute(&camera, &transform, 16.0 / 9.0);
    ViewParams::from_camera(&view, &camera, [0.0; 4], DistanceFog::default())
}

fn sort(c: &mut Criterion) {
    for n in [300, 3000] {
        let quads: Vec<_> = quads(n, 0.0)
            .into_iter()
            .map(|(m, light)| {
                let quad = m.quad.with_blend(CellBlend::TRANSLUCENT);
                (MovingCellQuad { quad, ..m }, light)
            })
            .collect();
        let mut sort = SortBench::new(&quads);
        // Two eyes a step apart, one after the other: each frame sorts again.
        let views = [
            view_at(DVec3::new(-4.0, 66.0, 0.5)),
            view_at(DVec3::new(-3.9, 66.0, 0.6)),
        ];
        let mut k = 0;
        c.bench_function(&format!("cells/sort/{n}"), |b| {
            b.iter(|| {
                k ^= 1;
                black_box(sort.sort(&views[k]))
            });
        });
    }
}

criterion_group!(benches, bench, sort);
criterion_main!(benches);
