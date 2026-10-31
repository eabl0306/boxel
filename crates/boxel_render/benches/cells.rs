//! The CPU half of the moving cell quads (ADR 0054) in one frame, as the render plugin hands them
//! to the renderer: the list compared with the last frame's and, when it changed, its vertices
//! written (`boxel_render::bench::MovingBench`).
//!
//! - `cells/moving/N`: `N` quads that change every frame (a lid opening: its quads turn), all
//!   written again.
//! - `cells/still/N`: the same `N` quads as the last frame: compared, nothing written.
//!
//! An open chest gives about 30 quads, a beacon's beam 4 per part of its color, a ringing bell
//! about 12.
use boxel_render::MovingCellQuad;
use boxel_render::bench::MovingBench;
use boxel_voxel::{BlockPos, CellImage, CellQuad, Face, LightValue};
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

criterion_group!(benches, bench);
criterion_main!(benches);
