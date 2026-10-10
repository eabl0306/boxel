//! Neighbor rules of an edit batch (ADR 0020, ADR 0027): `NeighborRules::expand` over one batch,
//! as the world runs it before applying the edits. Each scene places blocks over a 16x16 square
//! of an empty world in one batch:
//!
//! - `stone`: blocks without rules on every other cell (the common edit: only the lookups).
//! - `fences`: a fence on every cell, each one connecting to its neighbors: every rule changes its
//!   block, so its neighbors are looked at again (ADR 0027), to no change.
//! - `walls` (ADR 0027): a 16x16 square of walls, two high, with a torch on every other one:
//!   connections, tall sides and posts that run down a column.
//!
//! `cargo bench -p boxel_world --bench neighbor_rules` (release; never while something compiles).
#![expect(clippy::print_stderr, reason = "bench scene statistics")]

use boxel_voxel::{BlockPos, BlockRegistry, BlockState};
use boxel_world::{BlockEdit, NeighborRules};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use std::hint::black_box;

/// The batch of a scene: over a 16x16 square, the blocks `cells(x, z)` gives for each column,
/// as `(y, state)`.
fn batch(cells: impl Fn(i64, i64) -> Vec<(i64, BlockState)>) -> Vec<BlockEdit> {
    let mut edits = Vec::new();
    for x in 0..16 {
        for z in 0..16 {
            for (y, state) in cells(x, z) {
                edits.push(BlockEdit {
                    pos: BlockPos::new(x, y, z),
                    state,
                });
            }
        }
    }
    edits
}

fn bench(c: &mut Criterion) {
    let registry = BlockRegistry::default_blocks();
    let rules = NeighborRules::from_registry(&registry);
    let s = |name: &str| registry.parse_state(name).expect(name);
    let stone = s("boxel:stone");
    let fence = s("boxel:oak_fence");
    let air = |_: BlockPos| Some(BlockState::AIR);
    let checker = |state: BlockState| {
        batch(move |x, z| {
            if (x + z) % 2 == 0 {
                vec![(0, state)]
            } else {
                Vec::new()
            }
        })
    };
    let fences = batch(move |_, _| vec![(0, fence)]);
    let mut scenes = vec![("stone", checker(stone)), ("fences", fences)];
    if let Ok(wall) = registry.parse_state("boxel:cobblestone_wall") {
        let torch = s("boxel:torch");
        scenes.push((
            "walls",
            batch(move |x, z| {
                let mut column = vec![(0, wall), (1, wall)];
                if (x + z) % 2 == 0 {
                    column.push((2, torch));
                }
                column
            }),
        ));
    }
    for (name, edits) in scenes {
        let mut out = edits.clone();
        rules.expand(&registry, air, &mut out);
        eprintln!(
            "scene {name}: {} edits, {} after the rules",
            edits.len(),
            out.len()
        );
        c.bench_function(&format!("expand/{name}"), |b| {
            b.iter_batched_ref(
                || edits.clone(),
                |e| rules.expand(&registry, air, black_box(e)),
                BatchSize::SmallInput,
            );
        });
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
