//! The broadphase grid against brute force: random bodies (small, wide, huge), random layers
//! and active flags, any cell size, anywhere in the world. Its pairs, after moves, and its box
//! queries must equal the O(n^2) answer; the same scene shifted by any integer vector gives the
//! same pairs; the cells along a ray match the voxel DDA (1-block cells) and cover its blocks
//! (4-block cells).
mod common;

use boxel_math::{DVec3, I64Vec3};
use boxel_physics::{
    BlockBounds, CollisionLayers, Grid, LocalBox, RayResult, TestWorld, VoxelFilter, raycast_voxels,
};
use boxel_voxel::{BlockPos, BlockState, MAX_BLOCK_COORD, WorldPos};
use common::table;
use proptest::prelude::*;

/// A random body: a box at a position near the origin, its layers and whether it is active.
#[derive(Clone, Debug)]
struct Body {
    pos: DVec3,
    half: DVec3,
    layers: CollisionLayers,
    active: bool,
}

fn arb_body() -> impl Strategy<Value = Body> {
    (
        (-24.0..24.0f64, -24.0..24.0f64, -24.0..24.0f64),
        // Mostly small bodies, some wide (more than two cells of 4 blocks) or flat.
        prop_oneof![
            8 => (0.05..1.5f64, 0.05..1.5f64, 0.05..1.5f64),
            1 => (0.05..12.0f64, 0.05..1.0f64, 0.05..12.0f64),
        ],
        // Few layers, so some pairs meet and some do not.
        (1u32..4, 0u32..8),
        prop::bool::weighted(0.8),
    )
        .prop_map(|(p, h, (member, mask), active)| Body {
            pos: DVec3::new(p.0, p.1, p.2),
            half: DVec3::new(h.0, h.1, h.2),
            layers: CollisionLayers::new(member, mask | member),
            active,
        })
}

/// The bounds of `body` placed at `base + body.pos`.
fn bounds(body: &Body, base: I64Vec3) -> BlockBounds {
    let pos = WorldPos::from_block(BlockPos::from_i64vec3(base), body.pos);
    BlockBounds::of_box(pos, LocalBox::from_center_half(DVec3::ZERO, body.half))
}

fn grid(bodies: &[Body], base: I64Vec3, shift: u32) -> Grid {
    let mut grid = Grid::new(shift);
    for body in bodies {
        grid.insert(bounds(body, base), body.layers, body.active);
    }
    grid
}

/// Every candidate pair the slow way.
fn brute_pairs(grid: &Grid, bodies: &[Body]) -> Vec<(u32, u32)> {
    let mut pairs = Vec::new();
    for a in 0..bodies.len() {
        for b in a + 1..bodies.len() {
            let (first, second) = (&bodies[a], &bodies[b]);
            if (first.active || second.active)
                && first.layers.interacts(second.layers)
                && grid.bounds(a as u32).overlaps(&grid.bounds(b as u32))
            {
                pairs.push((a as u32, b as u32));
            }
        }
    }
    pairs
}

/// A base far or near the origin.
fn arb_base() -> impl Strategy<Value = I64Vec3> {
    let far = MAX_BLOCK_COORD - 64;
    prop_oneof![
        Just(I64Vec3::ZERO),
        Just(I64Vec3::new(-3, 7, 1 << 31)),
        Just(I64Vec3::new(1 << 40, -(1 << 52), 5)),
        Just(I64Vec3::splat(far)),
        Just(I64Vec3::splat(-far)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Pairs equal brute force, at any cell size and place, before and after some bodies move.
    #[test]
    fn pairs_equal_brute_force(
        mut bodies in prop::collection::vec(arb_body(), 0..80),
        moves in prop::collection::vec((0usize..80, -6.0..6.0f64, -6.0..6.0f64), 0..12),
        shift in 0u32..5,
        base in arb_base(),
    ) {
        let mut grid = grid(&bodies, base, shift);
        let mut pairs = Vec::new();
        grid.pairs(&mut pairs);
        prop_assert_eq!(&pairs, &brute_pairs(&grid, &bodies));
        prop_assert!(pairs.windows(2).all(|w| w[0] < w[1]), "sorted and unique");

        // Moves, as the solve does them: new bounds, then a rebuild when cells changed.
        for (i, dx, dz) in moves {
            if bodies.is_empty() {
                break;
            }
            let i = i % bodies.len();
            bodies[i].pos += DVec3::new(dx, 0.0, dz);
            grid.set_bounds(i as u32, bounds(&bodies[i], base));
        }
        grid.pairs(&mut pairs);
        prop_assert_eq!(&pairs, &brute_pairs(&grid, &bodies));
        // The large list holds exactly the bodies wider than two cells.
        let large: Vec<u32> = (0..bodies.len() as u32)
            .filter(|&i| {
                let (min, max) = grid.bounds(i).cells(grid.shift());
                (max - min).max_element() >= 2
            })
            .collect();
        prop_assert_eq!(grid.large(), &large[..]);
    }

    /// The same scene anywhere gives the same pairs (by index).
    #[test]
    fn pairs_do_not_depend_on_where_the_scene_is(
        bodies in prop::collection::vec(arb_body(), 0..60),
        base in arb_base(),
    ) {
        let mut here = Vec::new();
        grid(&bodies, I64Vec3::ZERO, 2).pairs(&mut here);
        let mut there = Vec::new();
        grid(&bodies, base, 2).pairs(&mut there);
        prop_assert_eq!(here, there);
    }

    /// Box queries equal brute force (through the cells when the query is small, by a scan when
    /// it is large).
    #[test]
    fn queries_equal_brute_force(
        bodies in prop::collection::vec(arb_body(), 1..80),
        center in (-30.0..30.0f64, -30.0..30.0f64, -30.0..30.0f64),
        half in (0.0..40.0f64, 0.0..6.0f64, 0.0..6.0f64),
        shift in 0u32..5,
        base in arb_base(),
    ) {
        let mut grid = grid(&bodies, base, shift);
        grid.build();
        let query = BlockBounds::of_box(
            WorldPos::from_block(BlockPos::from_i64vec3(base), DVec3::new(center.0, center.1, center.2)),
            LocalBox::from_center_half(DVec3::ZERO, DVec3::new(half.0, half.1, half.2)),
        );
        let mut found = Vec::new();
        grid.query(query, &mut found);
        let brute: Vec<u32> = (0..bodies.len() as u32)
            .filter(|&i| grid.bounds(i).overlaps(&query))
            .collect();
        prop_assert_eq!(found, brute);
    }

    /// Bodies moved after the build (as the solve moves them) are found at their new place at
    /// once, without a rebuild: box queries, the bodies of each cell plus the large list, and
    /// the pair pieces all equal brute force. A rebuild puts them back in their cells.
    #[test]
    fn moved_bodies_are_found_without_a_rebuild(
        mut bodies in prop::collection::vec(arb_body(), 1..80),
        moves in prop::collection::vec((0usize..80, -6.0..6.0f64, -2.0..2.0f64, -6.0..6.0f64), 1..16),
        center in (-30.0..30.0f64, -30.0..30.0f64, -30.0..30.0f64),
        half in (0.0..12.0f64, 0.0..6.0f64, 0.0..6.0f64),
        shift in 0u32..5,
        base in arb_base(),
    ) {
        let mut grid = grid(&bodies, base, shift);
        grid.build();
        for (i, dx, dy, dz) in moves {
            let i = i % bodies.len();
            bodies[i].pos += DVec3::new(dx, dy, dz);
            grid.set_bounds(i as u32, bounds(&bodies[i], base));
        }
        prop_assert!(grid.is_built());
        let query = BlockBounds::of_box(
            WorldPos::from_block(BlockPos::from_i64vec3(base), DVec3::new(center.0, center.1, center.2)),
            LocalBox::from_center_half(DVec3::ZERO, DVec3::new(half.0, half.1, half.2)),
        );
        let mut found = Vec::new();
        grid.query(query, &mut found);
        let brute: Vec<u32> = (0..bodies.len() as u32)
            .filter(|&i| grid.bounds(i).overlaps(&query))
            .collect();
        prop_assert_eq!(found, brute);
        // Every body is in the cells of its bounds or in the large list, never in a cell it
        // left.
        for i in 0..bodies.len() as u32 {
            let (min, max) = grid.bounds(i).cells(grid.shift());
            let large = grid.large().contains(&i);
            for x in min.x - 1..=max.x + 1 {
                for y in min.y - 1..=max.y + 1 {
                    for z in min.z - 1..=max.z + 1 {
                        let cell = I64Vec3::new(x, y, z);
                        let inside = cell.cmpge(min).all() && cell.cmple(max).all();
                        let listed = grid.cell(cell).filter(|&b| b == i).count();
                        prop_assert!(listed <= 1);
                        prop_assert!(listed == 0 || inside, "body {} in a cell it left", i);
                        prop_assert!(large || !inside || listed == 1, "body {} missing", i);
                    }
                }
            }
        }
        let mut split = Vec::new();
        for range in grid.pair_ranges(4) {
            grid.pairs_in(range, &mut split);
        }
        grid.large_pairs(&mut split);
        split.sort_unstable();
        prop_assert_eq!(&split, &brute_pairs(&grid, &bodies));
        let mut pairs = Vec::new();
        grid.pairs(&mut pairs);
        prop_assert_eq!(&pairs, &split);
        let large: Vec<u32> = (0..bodies.len() as u32)
            .filter(|&i| {
                let (min, max) = grid.bounds(i).cells(grid.shift());
                (max - min).max_element() >= 2
            })
            .collect();
        prop_assert_eq!(grid.large(), &large[..], "the rebuild put them back");
    }

    /// The pieces of the parallel broadphase (`pair_ranges`, `pairs_in` on each range in any
    /// order, then `large_pairs`) give exactly the pairs of `pairs`, each once.
    #[test]
    fn split_pairs_equal_pairs(
        bodies in prop::collection::vec(arb_body(), 0..160),
        parts in 1usize..16,
        shift in 0u32..5,
        base in arb_base(),
    ) {
        let mut grid = grid(&bodies, base, shift);
        let mut pairs = Vec::new();
        grid.pairs(&mut pairs);
        let ranges = grid.pair_ranges(parts);
        prop_assert!(ranges.len() <= parts);
        prop_assert!(ranges.windows(2).all(|w| w[0].end == w[1].start));
        prop_assert!(ranges.first().is_none_or(|r| r.start == 0));
        let mut split = Vec::new();
        for range in ranges.into_iter().rev() {
            grid.pairs_in(range, &mut split);
        }
        grid.large_pairs(&mut split);
        split.sort_unstable();
        prop_assert_eq!(split, pairs);
    }
}

#[test]
fn a_query_wider_than_the_cells_can_count_scans_the_bodies() {
    // Whole-world and very wide queries (an infinite box saturates at the `i64` ends): more
    // cells than a `u128` holds. They scan the bodies instead of walking the cells.
    let bodies: Vec<Body> = (0..20)
        .map(|i| Body {
            pos: DVec3::new(f64::from(i) * 3.0 - 30.0, 0.5, 0.5),
            half: DVec3::splat(0.4),
            layers: CollisionLayers::default(),
            active: true,
        })
        .collect();
    for base in [I64Vec3::ZERO, I64Vec3::splat(MAX_BLOCK_COORD - 64)] {
        let mut grid = grid(&bodies, base, 2);
        grid.build();
        let all: Vec<u32> = (0..bodies.len() as u32).collect();
        for query in [
            BlockBounds::new(I64Vec3::splat(i64::MIN), I64Vec3::splat(i64::MAX)),
            BlockBounds::new(I64Vec3::splat(-(1 << 61)), I64Vec3::splat(1 << 61)),
            BlockBounds::of_box(
                WorldPos::from_block(BlockPos::from_i64vec3(base), DVec3::ZERO),
                LocalBox::new(DVec3::splat(f64::NEG_INFINITY), DVec3::splat(f64::INFINITY)),
            ),
        ] {
            let mut found = Vec::new();
            grid.query(query, &mut found);
            assert_eq!(found, all, "{query:?}");
        }
    }
}

#[test]
fn bounds_reach_the_blocks_a_box_touches() {
    let at = |x, y, z| WorldPos::from_dvec3(DVec3::new(x, y, z));
    let unit = LocalBox::new(DVec3::ZERO, DVec3::ONE);
    // A box ending exactly on a block plane also reaches the next block; starting on one, it
    // does not reach the block before.
    assert_eq!(
        BlockBounds::of_box(at(0.0, 0.0, 0.0), unit),
        BlockBounds::new(I64Vec3::ZERO, I64Vec3::ONE)
    );
    let inside = LocalBox::new(DVec3::splat(0.25), DVec3::splat(0.75));
    assert_eq!(
        BlockBounds::of_box(at(-3.0, 0.0, 5.0), inside),
        BlockBounds::new(I64Vec3::new(-3, 0, 5), I64Vec3::new(-3, 0, 5))
    );
    // Cells are floor divisions, negative ones included.
    let b = BlockBounds::new(I64Vec3::new(-5, -4, 3), I64Vec3::new(-1, 0, 4));
    assert_eq!(
        b.cells(2),
        (I64Vec3::new(-2, -1, 0), I64Vec3::new(-1, 0, 1))
    );
    // A NaN box counts as the position's block; huge ones saturate.
    let nan = LocalBox::new(DVec3::splat(f64::NAN), DVec3::splat(f64::NAN));
    assert_eq!(
        BlockBounds::of_box(at(2.5, 2.5, 2.5), nan),
        BlockBounds::new(I64Vec3::splat(2), I64Vec3::splat(2))
    );
    let huge = LocalBox::new(DVec3::splat(-1e300), DVec3::splat(1e300));
    let b = BlockBounds::of_box(at(0.5, 0.5, 0.5), huge);
    assert_eq!(
        (b.min, b.max),
        (I64Vec3::splat(i64::MIN), I64Vec3::splat(i64::MAX))
    );
}

#[test]
fn an_unbuilt_grid_scans_and_a_reset_empties_it() {
    let mut grid = Grid::new(2);
    let a = grid.insert(
        BlockBounds::new(I64Vec3::ZERO, I64Vec3::ONE),
        CollisionLayers::default(),
        true,
    );
    let b = grid.insert(
        BlockBounds::new(I64Vec3::ONE, I64Vec3::splat(2)),
        CollisionLayers::default(),
        false,
    );
    assert!(!grid.is_built());
    let mut found = Vec::new();
    grid.query(BlockBounds::new(I64Vec3::ONE, I64Vec3::ONE), &mut found);
    assert_eq!(found, [a, b]);
    let mut pairs = Vec::new();
    grid.pairs(&mut pairs);
    assert_eq!(pairs, [(a, b)]);
    assert!(grid.is_built());
    assert_eq!(grid.cell(I64Vec3::ZERO).collect::<Vec<_>>(), [a, b]);
    // Two inactive bodies are never a pair.
    grid.reset(3);
    assert!(grid.is_empty() && grid.shift() == 3);
    grid.insert(BlockBounds::default(), CollisionLayers::default(), false);
    grid.insert(BlockBounds::default(), CollisionLayers::default(), false);
    grid.pairs(&mut pairs);
    assert!(pairs.is_empty());
}

/// The blocks the voxel DDA visits along a ray (a filter that never accepts records them).
fn voxel_path(origin: WorldPos, dir: DVec3, max_t: f64) -> Vec<I64Vec3> {
    let mut world = TestWorld::new();
    let mut seen = Vec::new();
    let mut record = |p: BlockPos, _: BlockState| {
        seen.push(p.as_i64vec3());
        false
    };
    let result = raycast_voxels(
        &mut world,
        &table(),
        origin,
        dir,
        max_t,
        VoxelFilter::Custom(&mut record),
    );
    assert_eq!(result, RayResult::Miss);
    seen
}

proptest! {
    /// With 1-block cells the cell walk is the voxel DDA: same blocks in the same order.
    #[test]
    fn one_block_cells_follow_the_voxel_dda(
        origin in (-20.0..20.0f64, -20.0..20.0f64, -20.0..20.0f64),
        dir in (-1.0..1.0f64, -1.0..1.0f64, -1.0..1.0f64),
        max_t in 0.0..30.0f64,
        base in arb_base(),
    ) {
        let origin = WorldPos::from_block(
            BlockPos::from_i64vec3(base),
            DVec3::new(origin.0, origin.1, origin.2),
        );
        let dir = DVec3::new(dir.0, dir.1, dir.2);
        let cells: Vec<I64Vec3> = Grid::new(0).cell_ray(origin, dir, max_t).map(|(c, _)| c).collect();
        let blocks = voxel_path(origin, dir, max_t);
        prop_assert_eq!(cells, blocks);
    }

    /// With 4-block cells: each cell is next to the previous one, the entry `t` never goes
    /// back, and every block of the voxel path is in a visited cell.
    #[test]
    fn four_block_cells_cover_the_ray(
        origin in (-20.0..20.0f64, -20.0..20.0f64, -20.0..20.0f64),
        dir in (-1.0..1.0f64, -1.0..1.0f64, -1.0..1.0f64),
        max_t in 0.0..60.0f64,
        base in arb_base(),
    ) {
        let origin = WorldPos::from_block(
            BlockPos::from_i64vec3(base),
            DVec3::new(origin.0, origin.1, origin.2),
        );
        let dir = DVec3::new(dir.0, dir.1, dir.2);
        let cells: Vec<(I64Vec3, f64)> = Grid::new(2).cell_ray(origin, dir, max_t).collect();
        let first = origin.block.as_i64vec3();
        prop_assert_eq!(cells[0], (I64Vec3::new(first.x >> 2, first.y >> 2, first.z >> 2), 0.0));
        for w in cells.windows(2) {
            let step = (w[1].0 - w[0].0).abs();
            prop_assert_eq!(step.element_sum(), 1, "{:?}", w);
            prop_assert!(w[1].1 >= w[0].1 && w[1].1 <= max_t);
        }
        for block in voxel_path(origin, dir, max_t) {
            let cell = I64Vec3::new(block.x >> 2, block.y >> 2, block.z >> 2);
            prop_assert!(cells.iter().any(|&(c, _)| c == cell), "{:?} not visited", cell);
        }
    }
}

#[test]
fn a_ray_without_direction_visits_one_cell() {
    let origin = WorldPos::from_dvec3(DVec3::new(5.5, -3.25, 9.0));
    for dir in [
        DVec3::ZERO,
        DVec3::new(f64::NAN, 1.0, 0.0),
        DVec3::splat(f64::INFINITY),
    ] {
        let cells: Vec<_> = Grid::new(2).cell_ray(origin, dir, 100.0).collect();
        assert_eq!(cells, [(I64Vec3::new(1, -1, 2), 0.0)], "{dir:?}");
    }
    // Entry `t` of a cell plane: from x = 5.5 along +X, the next 4-block plane is x = 8.
    let cells: Vec<_> = Grid::new(2).cell_ray(origin, DVec3::X, 10.0).collect();
    assert_eq!(
        cells,
        [
            (I64Vec3::new(1, -1, 2), 0.0),
            (I64Vec3::new(2, -1, 2), 2.5),
            (I64Vec3::new(3, -1, 2), 6.5)
        ]
    );
}
