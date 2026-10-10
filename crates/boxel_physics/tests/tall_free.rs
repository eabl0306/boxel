//! Sweeps, the edge guard and collision rays read some cells only for the tall blocks they may
//! hold (fences, ADR 0026). They skip them where the source says no tall block can be
//! (`VoxelSource::tall_free`: a loaded chunk whose palette has none), and the results must not
//! change: every guarded run is compared bit for bit with the always-reach run (a source that
//! cannot tell, so every such cell is read), on random fields with and without fences, across
//! chunk borders and unloaded chunks, far from the origin too, through `TestWorld` and through
//! `ChunkSource` over a real world that is edited.
mod common;
mod sim;

use boxel_math::{DVec3, I64Vec3};
use boxel_physics::{
    ChunkSource, CollisionTable, LocalBox, Probe, RayResult, StepUp, SweepConfig, TestWorld,
    VoxelFilter, VoxelSource, guard_edge, move_box_stepping, raycast_voxels,
};
use boxel_voxel::{BlockPos, BlockRegistry, BlockState, ChunkPos, MAX_BLOCK_COORD, WorldPos};
use boxel_world::{ChunkMap, WorldSettings};
use common::{Rng, STONE, WATER, at, bottom_slab, table, top_slab};
use proptest::prelude::*;
use sim::{FRAME_144, STEP, Sim};

fn state(name: &str) -> BlockState {
    BlockRegistry::default_blocks().parse_state(name).unwrap()
}

/// A source that counts its probes and answers `tall_free` from the source it wraps when
/// `guarded`, or never (the trait's default: every cell a tall block may need is read, as
/// before the guard).
struct Counting<S> {
    src: S,
    guarded: bool,
    probes: usize,
}

impl<S> Counting<S> {
    fn new(src: S, guarded: bool) -> Self {
        Self {
            src,
            guarded,
            probes: 0,
        }
    }
}

impl<S: VoxelSource> VoxelSource for Counting<S> {
    fn probe(&mut self, p: BlockPos) -> Probe {
        self.probes += 1;
        self.src.probe(p)
    }

    fn tall_free(&mut self, chunk: ChunkPos, table: &CollisionTable) -> bool {
        self.guarded && self.src.tall_free(chunk, table)
    }
}

/// Chunk corners the fields are centered on: the origin, ±2^31, 2^40 and near the world limit.
fn bases() -> [I64Vec3; 5] {
    [
        I64Vec3::new(0, 0, 0),
        I64Vec3::new(1 << 31, -(1 << 31), 1 << 31),
        I64Vec3::new(-(1 << 31), 1 << 31, -(1 << 31)),
        I64Vec3::new(1 << 40, 32, -(1 << 40)),
        I64Vec3::new((1 << 61) - 4096, -(1 << 61) + 4096, (1 << 61) - 4096),
    ]
}

/// A chunk corner on the world limit: on X the field reaches past the world's last block
/// (`MAX_BLOCK_COORD + 31`, where positions are clamped) and rays leave the world; on Z it
/// starts at the first one.
const EDGE: I64Vec3 = I64Vec3::new(MAX_BLOCK_COORD + 32, 0, -MAX_BLOCK_COORD);

/// The eight chunks around `base`: bit `x + 2y + 4z` is the one at `(x, y, z)`, with 0 the side
/// below the corner and 1 the side above it.
fn chunk_of(base: I64Vec3, bit: u8) -> ChunkPos {
    let side = |b: u8| if b != 0 { 0 } else { -1 };
    let corner = ChunkPos::containing(BlockPos::from_i64vec3(base));
    ChunkPos::new(
        corner.x + side(bit & 1),
        corner.y + side(bit & 2),
        corner.z + side(bit & 4),
    )
}

/// A random field in `base + (-12..12)^3`, across the chunk borders at `base`: stone, slabs,
/// water and fences of every shape, the fences only in the chunks whose bit is set in `fenced`
/// ([`chunk_of`]). With `unloaded` below 8 that chunk is not loaded (it keeps its blocks).
fn field(base: I64Vec3, seed: u64, density: f64, fenced: u8, unloaded: u8) -> TestWorld {
    let fences = [
        state("boxel:oak_fence"),
        state("boxel:oak_fence[east=true,west=true]"),
        state("boxel:oak_fence[north=true,south=true]"),
        state("boxel:oak_fence[east=true,south=true]"),
    ];
    let others = [STONE, STONE, bottom_slab(), top_slab(), WATER];
    let mut world = TestWorld::new();
    let mut rng = Rng(seed);
    for y in -12..12 {
        for x in -12..12 {
            for z in -12..12 {
                if rng.unit() >= density {
                    continue;
                }
                let bit = u8::from(x >= 0) | u8::from(y >= 0) << 1 | u8::from(z >= 0) << 2;
                let fence = fenced & (1 << bit) != 0 && rng.next().is_multiple_of(3);
                let s = if fence {
                    fences[(rng.next() % fences.len() as u64) as usize]
                } else {
                    others[(rng.next() % others.len() as u64) as usize]
                };
                world.set(at(base, x, y, z), s);
            }
        }
    }
    if unloaded < 8 {
        world.set_unloaded(chunk_of(base, unloaded), true);
    }
    world
}

/// Uniform in `-r..r`, snapped to sixteenths half the time (resting on slab and fence tops,
/// touching faces).
fn coord(rng: &mut Rng, r: f64) -> f64 {
    let v = (rng.unit() * 2.0 - 1.0) * r;
    if rng.next().is_multiple_of(2) {
        (v * 16.0).round() / 16.0
    } else {
        v
    }
}

/// One random box shape: a player and an item (origin at the feet and at the center), or any.
fn shape(rng: &mut Rng) -> LocalBox {
    match rng.next() % 4 {
        0 => LocalBox::new(DVec3::new(-0.3, 0.0, -0.3), DVec3::new(0.3, 1.8, 0.3)),
        1 => LocalBox::new(DVec3::splat(-0.125), DVec3::splat(0.125)),
        2 => LocalBox::new(DVec3::new(-0.25, 0.0, -0.25), DVec3::new(0.25, 1.75, 0.25)),
        _ => {
            let half = DVec3::new(rng.unit(), rng.unit(), rng.unit()) * 1.1 + DVec3::splat(0.05);
            LocalBox::from_center_half(DVec3::ZERO, half)
        }
    }
}

/// One random displacement component: none, a walking step, gravity, or a long move.
fn component(rng: &mut Rng) -> f64 {
    match rng.next() % 6 {
        0 => 0.0,
        1 => -1.0 / 128.0,
        2 | 3 => (rng.unit() - 0.5) * 0.8,
        4 => (rng.unit() - 0.5) * 3.0,
        _ => (rng.unit() - 0.5) * 40.0,
    }
}

/// How many probes the guarded and the always-reach runs made.
#[derive(Default, Debug)]
struct Reads {
    guarded: usize,
    always: usize,
}

/// `moves` random moves (with the step-up, obstacles, a start inside blocks now and then), edge
/// guards and rays in `world` around `base`, each run guarded and always-reach: the same
/// results, and the guarded one never reads more.
fn compare<S: VoxelSource>(
    world: &mut S,
    base: I64Vec3,
    seed: u64,
    moves: usize,
    reads: &mut Reads,
) -> Result<(), TestCaseError> {
    let t = table();
    let mut rng = Rng(seed);
    let run = |world: &mut S, guarded: bool, f: &mut dyn FnMut(&mut Counting<&mut S>)| {
        let mut src = Counting::new(&mut *world, guarded);
        f(&mut src);
        src.probes
    };
    for _ in 0..moves {
        let start = WorldPos::from_block(
            BlockPos::from_i64vec3(base),
            DVec3::new(
                coord(&mut rng, 11.5),
                coord(&mut rng, 11.5),
                coord(&mut rng, 11.5),
            ),
        );
        let shape = shape(&mut rng);
        let d = DVec3::new(
            component(&mut rng),
            component(&mut rng),
            component(&mut rng),
        );
        let height = [0.0, 0.6, 0.5, 1.0, rng.unit() * 1.25][(rng.next() % 5) as usize];
        let step = StepUp {
            height,
            grounded: rng.next().is_multiple_of(2),
        };
        let obstacles: Vec<LocalBox> = (0..rng.next() % 3)
            .map(|_| {
                let min = DVec3::new(
                    coord(&mut rng, 2.0),
                    coord(&mut rng, 2.0),
                    coord(&mut rng, 2.0),
                );
                LocalBox::new(min, min + DVec3::new(0.5, 0.25 + rng.unit(), 0.5))
            })
            .collect();
        let cfg = SweepConfig {
            max_depenetration: [1.0, 0.0, 3.0][(rng.next() % 3) as usize],
            ..SweepConfig::default()
        };
        let mut outs = [None, None];
        let probes = [true, false].map(|guarded| {
            run(world, guarded, &mut |src| {
                let out = move_box_stepping(src, &t, start, shape, d, &obstacles, &cfg, step);
                outs[usize::from(!guarded)] = Some(out);
            })
        });
        prop_assert_eq!(
            outs[0],
            outs[1],
            "move from {:?} by {:?}, {:?}",
            start,
            d,
            step
        );
        prop_assert!(probes[0] <= probes[1], "{:?}", probes);
        reads.guarded += probes[0];
        reads.always += probes[1];

        // The sneak edge guard of the same move.
        let drop = [0.6, rng.unit()][(rng.next() % 2) as usize];
        let mut cuts = [None, None];
        let probes = [true, false].map(|guarded| {
            run(world, guarded, &mut |src| {
                let cut = guard_edge(src, &t, start, shape, d, &obstacles, &cfg, drop);
                cuts[usize::from(!guarded)] = Some(cut);
            })
        });
        prop_assert_eq!(cuts[0], cuts[1], "guard from {:?} by {:?}", start, d);
        prop_assert!(probes[0] <= probes[1], "{:?}", probes);

        // A collision ray (and a picking ray, which never reads under a cell).
        let origin = WorldPos::from_block(
            BlockPos::from_i64vec3(base),
            DVec3::new(
                coord(&mut rng, 12.0),
                coord(&mut rng, 12.0),
                coord(&mut rng, 12.0),
            ),
        );
        let dir = DVec3::new(rng.unit() - 0.5, rng.unit() - 0.5, rng.unit() - 0.5);
        let dir = if rng.next().is_multiple_of(4) {
            // Straight down and across: rays along a fence top, through cell bottoms.
            DVec3::new(dir.x.signum() * 0.5, -1.0, 0.0)
        } else {
            dir
        };
        let max_t = rng.unit() * 40.0;
        for picking in [false, true] {
            let mut hits = [None, None];
            let probes = [true, false].map(|guarded| {
                run(world, guarded, &mut |src| {
                    let filter = if picking {
                        VoxelFilter::Outline { fluids: false }
                    } else {
                        VoxelFilter::Collision
                    };
                    hits[usize::from(!guarded)] =
                        Some(raycast_voxels(src, &t, origin, dir, max_t, filter));
                })
            });
            prop_assert_eq!(hits[0], hits[1], "ray from {:?} along {:?}", origin, dir);
            prop_assert!(probes[0] <= probes[1], "{:?}", probes);
            reads.guarded += probes[0];
            reads.always += probes[1];
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Fields without fences, with fences everywhere, and with fences in some of the eight
    /// chunks (so moves and rays cross between both kinds), sometimes with an unloaded chunk,
    /// at the five bases and on the world limit ([`EDGE`]).
    #[test]
    fn guarded_sweeps_and_rays_match_the_always_reach_ones(
        seed in any::<u64>(),
        base in 0..6usize,
        density in 0.05..0.45f64,
        fenced in prop_oneof![Just(0u8), Just(u8::MAX), any::<u8>()],
        unloaded in 0..12u8,
    ) {
        let base = bases().get(base).copied().unwrap_or(EDGE);
        let mut world = field(base, seed, density, fenced, unloaded);
        compare(&mut world, base, seed ^ 0x7A11, 24, &mut Reads::default())?;
    }
}

#[test]
fn the_guard_reads_less_without_fences_and_the_same_with_them() {
    // Without fences the guarded runs skip the rows under the boxes and the cells under the
    // rays (and the deeper layer of a fall): fewer reads.
    let mut reads = Reads::default();
    for seed in 0..12 {
        let base = bases()[(seed % 5) as usize];
        let mut world = field(base, seed, 0.25, 0, 8);
        compare(&mut world, base, seed, 64, &mut reads).unwrap();
    }
    assert!(
        reads.guarded * 10 < reads.always * 9,
        "the guard skips reads: {reads:?}"
    );
    // A source that cannot tell reads them all, as before.
    let mut all = Reads::default();
    for seed in 0..12 {
        let base = bases()[(seed % 5) as usize];
        let mut world = field(base, seed, 0.25, 0, 8);
        let mut blind = Counting::new(&mut world, false);
        compare(&mut blind, base, seed, 64, &mut all).unwrap();
    }
    assert_eq!(all.guarded, all.always);
}

#[test]
fn a_fence_top_across_a_chunk_border_still_stops_moves_and_rays() {
    // The fence is in the chunk under the border (y = -1), its top half in the chunk above,
    // which has no tall block; a fence beside the border on X too.
    let fence = state("boxel:oak_fence[north=true,south=true]");
    for base in bases() {
        let mut world = TestWorld::new();
        for (x, z) in [(-4, -4), (4, 4)] {
            world.fill(
                at(base, x - 4, -2, z - 4),
                at(base, x + 4, -2, z + 4),
                STONE,
            );
        }
        world.set(at(base, 2, -1, 0), fence);
        world.set(at(base, -1, -1, 0), fence);
        let t = table();
        assert!(world.tall_free(chunk_of(base, 2 | 4), &t));
        assert!(!world.tall_free(chunk_of(base, 1 | 4), &t));
        assert!(!world.tall_free(chunk_of(base, 4), &t));
        // A box gliding just above the border, at y = 0.25, into the fence tops from both
        // sides: stopped at the post (x 2.375 and -0.375).
        let glide = LocalBox::new(DVec3::new(-0.25, 0.0, -0.25), DVec3::new(0.25, 0.5, 0.25));
        for (from, d, stop) in [(0.5, 3.0, 2.375 - 0.25), (0.75, -3.0, -0.375 + 0.25)] {
            let start =
                WorldPos::from_block(BlockPos::from_i64vec3(base), DVec3::new(from, 0.25, 0.5));
            let out = move_box_stepping(
                &mut world,
                &t,
                start,
                glide,
                DVec3::new(d, 0.0, 0.0),
                &[],
                &SweepConfig::default(),
                StepUp::NONE,
            )
            .expect("loaded");
            assert_eq!(
                start.delta_to(&out.end).x,
                stop - from,
                "from {from} by {d}"
            );
        }
        // Falling onto the fence top, and a ray down onto it: y = 0.5, in the upper chunk.
        let above = WorldPos::from_block(BlockPos::from_i64vec3(base), DVec3::new(2.5, 3.0, 0.5));
        let out = move_box_stepping(
            &mut world,
            &t,
            above,
            glide,
            DVec3::new(0.0, -4.0, 0.0),
            &[],
            &SweepConfig::default(),
            StepUp::NONE,
        )
        .expect("loaded");
        assert_eq!(
            out.end,
            WorldPos::from_block(BlockPos::from_i64vec3(base), DVec3::new(2.5, 0.5, 0.5))
        );
        let RayResult::Hit(hit) = raycast_voxels(
            &mut world,
            &t,
            above,
            -DVec3::Y,
            10.0,
            VoxelFilter::Collision,
        ) else {
            panic!("the ray misses the fence top");
        };
        assert_eq!((hit.block, hit.t), (at(base, 2, -1, 0), 2.5));
    }
}

/// Puts `state` at `pos` and runs steps until the world has it (a few at most: the edit
/// queue applies it in the next update).
fn place(sim: &mut Sim, pos: BlockPos, state: BlockState) {
    sim.edit(pos, state);
    for _ in 0..8 {
        sim.frame(STEP);
        if sim.app.world().resource::<ChunkMap>().block_at(pos) == Some(state) {
            return;
        }
    }
    panic!("{state:?} never landed at {pos:?}");
}

#[test]
fn chunk_sources_answer_from_the_world_and_follow_its_edits() {
    // Alone, so neighbor rules keep it as it is.
    let fence = state("boxel:oak_fence");
    let t = table();
    for base in [I64Vec3::ZERO, I64Vec3::new(1 << 40, -(1 << 30), -(1 << 40))] {
        let mut sim = Sim::new(base, FRAME_144);
        // The ground's chunk (stone and grass: packed), the air above (uniform), and a chunk
        // that is not loaded.
        let ground = ChunkPos::containing(sim.block(0, 0, 0));
        let air = ChunkPos::new(ground.x, ground.y + 1, ground.z);
        let far = ChunkPos::new(ground.x + 9, ground.y, ground.z);
        let free = |sim: &Sim, chunk: ChunkPos| {
            let world = sim.app.world();
            let mut src = ChunkSource::new(
                world.resource::<ChunkMap>(),
                world.resource::<WorldSettings>(),
            );
            // Asked twice: the second answer comes from the caches.
            let first = src.tall_free(chunk, &t);
            assert_eq!(src.tall_free(chunk, &t), first);
            first
        };
        assert!(free(&sim, ground) && free(&sim, air));
        assert!(!free(&sim, far));
        // A fence on the ground: that chunk now may hold one (the cached answer is dropped
        // with the edit), the others still cannot.
        let on_ground = sim.block(3, 4, 3);
        place(&mut sim, on_ground, fence);
        assert!(!free(&sim, ground));
        assert!(free(&sim, air));
        // A fence at the top of the ground's chunk, whose top half is in the air chunk.
        let at_top = sim.block(5, 31, -3);
        place(&mut sim, at_top, fence);
        assert!(free(&sim, air));
        // Moves and rays through the world's chunks, guarded and not: on the ground (and into
        // the chunk beside it, which has no fence), and across the border with the air chunk.
        let world = sim.app.world();
        let mut src = ChunkSource::new(
            world.resource::<ChunkMap>(),
            world.resource::<WorldSettings>(),
        );
        let mut reads = Reads::default();
        for (center, seed) in [(6, 0xC5), (30, 0x5C)] {
            let center = base + I64Vec3::new(0, center, 0);
            compare(&mut src, center, seed, 300, &mut reads).unwrap();
        }
        assert!(reads.guarded < reads.always, "{reads:?}");
    }
}
