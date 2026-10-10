//! Blocks made of boxes (ADR 0018): the collision table built from `BlockDef::collision`, the
//! sweep and the raycast against slabs and plants, checked by hand and against brute-force
//! oracles over random fields of cubes and slabs.
mod common;

use boxel_math::{DVec3, I64Vec3};
use boxel_physics::{
    CollisionShape, CollisionTable, LocalBox, RayResult, SweepConfig, TestWorld, VoxelFilter,
    move_box, raycast_voxels,
};
use boxel_voxel::{BlockPos, BlockRegistry, BlockState, Face, WorldPos};
use common::{Rng, STONE, WATER, at, fill, table};

fn state(name: &str) -> BlockState {
    BlockRegistry::default_blocks().parse_state(name).unwrap()
}

fn bottom() -> BlockState {
    state("boxel:stone_slab[type=bottom]")
}

fn top() -> BlockState {
    state("boxel:stone_slab[type=top]")
}

/// Both halves (ADR 0019): a full cube.
fn double() -> BlockState {
    state("boxel:stone_slab[type=double]")
}

/// A 0.5 x 1.75 x 0.5 box with its origin at the feet (binary fractions: exact positions).
fn body() -> LocalBox {
    LocalBox::new(DVec3::new(-0.25, 0.0, -0.25), DVec3::new(0.25, 1.75, 0.25))
}

fn sweep(world: &mut TestWorld, start: WorldPos, d: DVec3) -> boxel_physics::SweepOut {
    move_box(
        world,
        &table(),
        start,
        body(),
        d,
        &[],
        &SweepConfig::default(),
    )
    .expect("loaded")
}

#[test]
fn the_table_follows_the_block_collision_and_model() {
    let t = table();
    let half = |lo: f64, hi: f64| LocalBox::new(DVec3::new(0.0, lo, 0.0), DVec3::new(1.0, hi, 1.0));
    assert_eq!(t.shape(STONE), CollisionShape::Full);
    assert_eq!(t.shape(bottom()), CollisionShape::Boxes(&[half(0.0, 0.5)]));
    assert_eq!(t.shape(top()), CollisionShape::Boxes(&[half(0.5, 1.0)]));
    assert_eq!(
        t.outline(bottom()),
        CollisionShape::Boxes(&[half(0.0, 0.5)])
    );
    assert!(t.collides(bottom()) && !t.is_full(bottom()));
    // The double slab collides and is picked like stone.
    assert_eq!(t.shape(double()), CollisionShape::Full);
    assert_eq!(t.outline(double()), CollisionShape::Full);
    assert!(t.is_full(double()) && !t.is_fluid(double()));
    for plant in [state("boxel:short_grass"), state("boxel:poppy")] {
        assert_eq!(t.shape(plant), CollisionShape::Empty);
        assert!(!t.collides(plant) && t.has_outline(plant));
        let plant_box = LocalBox::new(
            DVec3::new(0.125, 0.0, 0.125),
            DVec3::new(0.875, 0.8125, 0.875),
        );
        assert_eq!(t.outline(plant), CollisionShape::Boxes(&[plant_box]));
    }
    let glass = state("boxel:glass");
    assert_eq!(t.shape(glass), CollisionShape::Full);
    assert!(!t.is_fluid(glass) && t.is_fluid(WATER));
    assert_eq!(t.outline(BlockState::AIR), CollisionShape::Empty);
    assert!(t.has_boxes());
}

#[test]
fn bodies_land_on_slabs_and_bump_into_their_sides() {
    let base = I64Vec3::new(-40, 7, 1000);
    let mut world = TestWorld::new();
    fill(&mut world, base, [-3, -1, -3], [3, -1, 3], STONE);
    world.set(at(base, 0, 0, 0), bottom());
    world.set(at(base, 0, 2, 0), top());
    // Falling onto the bottom slab: exactly on its top, the slab as ground.
    let start = WorldPos::from_block(at(base, 0, 0, 0), DVec3::new(0.5, 0.625, 0.5));
    let out = sweep(&mut world, start, DVec3::new(0.0, -3.0, 0.0));
    assert_eq!(
        out.end,
        WorldPos::from_block(at(base, 0, 0, 0), DVec3::new(0.5, 0.5, 0.5))
    );
    assert_eq!(out.ground, Some(at(base, 0, 0, 0)));
    assert!(out.touched(Face::NegY));
    // Next to the slab on the floor, walking into it: stopped by its side.
    let start = WorldPos::from_block(at(base, -2, 0, 0), DVec3::new(0.5, 0.0, 0.5));
    let out = sweep(&mut world, start, DVec3::new(3.0, 0.0, 0.0));
    assert_eq!(
        out.end,
        WorldPos::from_block(at(base, -1, 0, 0), DVec3::new(0.75, 0.0, 0.5))
    );
    assert_eq!(out.blocks[0], Some(at(base, 0, 0, 0)));
    // Standing on the slab's top, walking off it: free (its top face only touches the box), and
    // the top slab two blocks up stops a jump half a block early.
    let on_top = WorldPos::from_block(at(base, 0, 0, 0), DVec3::new(0.5, 0.5, 0.5));
    let out = sweep(&mut world, on_top, DVec3::new(2.0, 0.0, 0.0));
    assert_eq!(out.faces, 0);
    let out = sweep(&mut world, on_top, DVec3::new(0.0, 3.0, 0.0));
    // Head at 0.5 + 1.75 = 2.25 hits the top slab's bottom at 2.5.
    assert_eq!(
        out.end,
        WorldPos::from_block(at(base, 0, 0, 0), DVec3::new(0.5, 0.75, 0.5))
    );
    assert!(out.touched(Face::PosY));
    // Plants do not collide.
    world.set(at(base, 2, 0, 0), state("boxel:short_grass"));
    let start = WorldPos::from_block(at(base, 2, 0, 0), DVec3::new(0.5, 3.0, 0.5));
    let out = sweep(&mut world, start, DVec3::new(0.0, -5.0, 0.0));
    assert_eq!(
        out.end,
        WorldPos::from_block(at(base, 2, 0, 0), DVec3::new(0.5, 0.0, 0.5))
    );
    assert_eq!(out.ground, Some(at(base, 2, -1, 0)));
}

#[test]
fn a_body_inside_a_slab_is_pushed_out() {
    let mut world = TestWorld::new();
    world.set(BlockPos::new(0, 0, 0), bottom());
    let start = WorldPos::from_block(BlockPos::new(0, 0, 0), DVec3::new(0.5, 0.25, 0.5));
    let out = sweep(&mut world, start, DVec3::ZERO);
    assert!(out.depenetrated);
    // Onto the next block plane (the sweep only stops on integer planes).
    assert_eq!(
        out.end,
        WorldPos::from_block(BlockPos::new(0, 1, 0), DVec3::new(0.5, 0.0, 0.5))
    );
}

/// A random field of stone, bottom, top and double slabs, fences (whose collision reaches half a
/// block into the cell above, ADR 0026), walls and fence gates (the same, ADR 0027; an open gate
/// collides with nothing) in `0..size` (air elsewhere).
fn shaped_field(size: i64, density: f64, seed: u64) -> (TestWorld, Vec<(BlockPos, BlockState)>) {
    let mut world = TestWorld::new();
    let mut rng = Rng(seed);
    let mut blocks = Vec::new();
    let kinds = [
        STONE,
        bottom(),
        top(),
        double(),
        state("boxel:oak_fence"),
        state("boxel:oak_fence[east=true,south=true]"),
        state("boxel:oak_fence[north=true,west=true]"),
        state("boxel:cobblestone_wall[east=tall,north=low]"),
        state("boxel:cobblestone_wall[north=low,south=low,up=false]"),
        state("boxel:oak_fence_gate[facing=east]"),
        state("boxel:oak_fence_gate[open=true]"),
    ];
    for y in 0..size {
        for x in 0..size {
            for z in 0..size {
                if rng.unit() < density {
                    let s = kinds[(rng.next() % kinds.len() as u64) as usize];
                    let p = BlockPos::new(x, y, z);
                    world.set(p, s);
                    blocks.push((p, s));
                }
            }
        }
    }
    (world, blocks)
}

/// The world boxes of a block list.
fn world_boxes(table: &CollisionTable, blocks: &[(BlockPos, BlockState)]) -> Vec<LocalBox> {
    let mut out = Vec::new();
    for &(p, s) in blocks {
        let at = DVec3::new(p.x as f64, p.y as f64, p.z as f64);
        match table.shape(s) {
            CollisionShape::Full => out.push(LocalBox::new(at, at + DVec3::ONE)),
            CollisionShape::Boxes(boxes) => out.extend(boxes.iter().map(|b| b.translate(at))),
            _ => {}
        }
    }
    out
}

#[test]
fn single_axis_sweeps_match_a_brute_force_oracle() {
    let t = table();
    let eps = SweepConfig::default().eps;
    let mut checked = 0;
    // Sparse fields, and more dense ones where blocks of every kind share layers (a stone
    // block scanned before a fence of its layer must not hide the fence's top).
    for (density, seed) in [(0.1, 40u64), (0.3, 200)]
        .into_iter()
        .flat_map(|(density, seeds)| (0..seeds).map(move |seed| (density, seed)))
    {
        let (mut world, blocks) = shaped_field(8, density, seed);
        let boxes = world_boxes(&t, &blocks);
        let mut rng = Rng(seed ^ 0xABCD);
        for _ in 0..80 {
            let start = DVec3::new(
                0.5 + rng.unit() * 7.0,
                rng.unit() * 6.0,
                0.5 + rng.unit() * 7.0,
            );
            // Snap to 1/16 sometimes: resting on slab tops and touching faces.
            let start = if rng.next().is_multiple_of(2) {
                (start * 16.0).round() / 16.0
            } else {
                start
            };
            let me = body().translate(start);
            if boxes.iter().any(|b| b.overlaps(me, eps)) {
                continue;
            }
            let axis = (rng.next() % 3) as usize;
            let amount = (rng.unit() - 0.5) * 8.0;
            let mut d = DVec3::ZERO;
            d[axis] = amount;
            // Oracle: the nearest box face ahead that the moving box crosses.
            let (o1, o2) = match axis {
                0 => (1, 2),
                1 => (0, 2),
                _ => (0, 1),
            };
            let positive = amount > 0.0;
            let lead = if positive { me.max[axis] } else { me.min[axis] };
            let mut end = lead + amount;
            for b in &boxes {
                let across = b.min[o1] < me.max[o1] - eps
                    && b.max[o1] > me.min[o1] + eps
                    && b.min[o2] < me.max[o2] - eps
                    && b.max[o2] > me.min[o2] + eps;
                if !across {
                    continue;
                }
                if positive && b.min[axis] >= lead - eps && b.min[axis] < end {
                    end = b.min[axis];
                }
                if !positive && b.max[axis] <= lead + eps && b.max[axis] > end {
                    end = b.max[axis];
                }
            }
            let out = sweep(&mut world, WorldPos::from_dvec3(start), d);
            let got = out.end.block.as_i64vec3()[axis] as f64 + out.end.frac[axis];
            let want = start[axis] + (end - lead);
            assert!(
                (got - want).abs() < 1e-9,
                "density {density} seed {seed}: start {start} axis {axis} by {amount}: got {got}, \
                 want {want}"
            );
            checked += 1;
        }
    }
    assert!(checked > 5000);
}

#[test]
fn rays_hit_the_real_shape() {
    let mut world = TestWorld::new();
    let p = BlockPos::new(5, 0, 5);
    world.set(p, bottom());
    world.set(BlockPos::new(8, 0, 5), STONE);
    let t = table();
    let picking = || VoxelFilter::Outline { fluids: false };
    // Straight down onto the slab: its top, at y = 0.5.
    let origin = WorldPos::from_dvec3(DVec3::new(5.5, 3.0, 5.5));
    let RayResult::Hit(hit) = raycast_voxels(
        &mut world,
        &t,
        origin,
        DVec3::new(0.0, -1.0, 0.0),
        10.0,
        picking(),
    ) else {
        panic!("no hit");
    };
    assert_eq!((hit.block, hit.face, hit.t), (p, Some(Face::PosY), 2.5));
    assert_eq!(hit.point, WorldPos::from_dvec3(DVec3::new(5.5, 0.5, 5.5)));
    assert_eq!(hit.adjacent(), Some(BlockPos::new(5, 1, 5)));
    // Over the slab at y = 0.75: past it, onto the stone behind.
    let origin = WorldPos::from_dvec3(DVec3::new(3.5, 0.75, 5.5));
    let hit = raycast_voxels(&mut world, &t, origin, DVec3::X, 10.0, picking());
    let hit = *hit.hit().expect("hit");
    assert_eq!(
        (hit.block, hit.face),
        (BlockPos::new(8, 0, 5), Some(Face::NegX))
    );
    // At y = 0.25: its side, exactly on the plane x = 5.
    let origin = WorldPos::from_dvec3(DVec3::new(3.5, 0.25, 5.5));
    let hit = *raycast_voxels(&mut world, &t, origin, DVec3::X, 10.0, picking())
        .hit()
        .expect("hit");
    assert_eq!((hit.block, hit.face, hit.t), (p, Some(Face::NegX), 1.5));
    // From the empty half of the slab's own cell, looking down: its top, with a face.
    let origin = WorldPos::from_dvec3(DVec3::new(5.5, 0.875, 5.5));
    let hit = *raycast_voxels(&mut world, &t, origin, -DVec3::Y, 10.0, picking())
        .hit()
        .expect("hit");
    assert_eq!((hit.block, hit.face, hit.t), (p, Some(Face::PosY), 0.375));
    // From inside the slab: t = 0, no face.
    let origin = WorldPos::from_dvec3(DVec3::new(5.5, 0.25, 5.5));
    let hit = *raycast_voxels(&mut world, &t, origin, DVec3::X, 10.0, picking())
        .hit()
        .expect("hit");
    assert_eq!((hit.block, hit.face, hit.t), (p, None, 0.0));
    // A plant: picked by its outline box, never hit by collision rays.
    let grass = BlockPos::new(5, 0, 9);
    world.set(grass, state("boxel:short_grass"));
    let origin = WorldPos::from_dvec3(DVec3::new(5.5, 0.5, 7.5));
    let hit = *raycast_voxels(&mut world, &t, origin, DVec3::Z, 10.0, picking())
        .hit()
        .expect("hit");
    assert_eq!(
        (hit.block, hit.face, hit.t),
        (grass, Some(Face::NegZ), 1.625)
    );
    let solid = raycast_voxels(
        &mut world,
        &t,
        origin,
        DVec3::Z,
        10.0,
        VoxelFilter::Collision,
    );
    assert_eq!(solid, RayResult::Miss);
    // Over the plant's box: a miss.
    let origin = WorldPos::from_dvec3(DVec3::new(5.5, 0.9, 7.5));
    let over = raycast_voxels(&mut world, &t, origin, DVec3::Z, 10.0, picking());
    assert_eq!(over, RayResult::Miss);
}

#[test]
fn random_rays_match_a_brute_force_oracle() {
    let t = table();
    let mut checked = 0;
    for seed in 0..30u64 {
        let (mut world, blocks) = shaped_field(8, 0.15, seed);
        let boxes = world_boxes(&t, &blocks);
        let mut rng = Rng(seed ^ 0x5151);
        for _ in 0..80 {
            let origin = DVec3::new(rng.unit(), rng.unit(), rng.unit()) * 8.0;
            if boxes.iter().any(|b| b.contains(origin)) {
                continue;
            }
            let dir = DVec3::new(rng.unit() - 0.5, rng.unit() - 0.5, rng.unit() - 0.5);
            let want = boxes
                .iter()
                .filter_map(|b| b.ray(origin, dir, 30.0).map(|h| h.t))
                .fold(f64::INFINITY, f64::min);
            let got = raycast_voxels(
                &mut world,
                &t,
                WorldPos::from_dvec3(origin),
                dir,
                30.0,
                VoxelFilter::Collision,
            );
            match got {
                RayResult::Hit(hit) => {
                    assert!(
                        (hit.t - want).abs() < 1e-9,
                        "seed {seed}: {origin} {dir}: t {} vs {want}",
                        hit.t
                    );
                    checked += 1;
                }
                _ => assert_eq!(want, f64::INFINITY, "seed {seed}: {origin} {dir} missed"),
            }
        }
    }
    assert!(checked > 500);
}

/// Stairs, fences and torches (ADR 0020): stairs collide and are picked by their boxes (the slab
/// and the step), a fence collides by its post and arms 1.5 blocks tall (ADR 0026) and is
/// picked by them a block tall, a torch does not collide but is picked by a small box.
#[test]
fn stairs_fences_and_torches_collide_with_their_boxes() {
    let t = table();
    let unit = |min: [f64; 3], max: [f64; 3]| {
        LocalBox::new(DVec3::from_array(min) / 16.0, DVec3::from_array(max) / 16.0)
    };
    let stairs = state("boxel:stone_stairs[facing=east,half=top,shape=outer_right]");
    let boxes = [
        unit([0.0, 8.0, 0.0], [16.0, 16.0, 16.0]),
        unit([8.0, 0.0, 8.0], [16.0, 8.0, 16.0]),
    ];
    assert_eq!(t.shape(stairs), CollisionShape::Boxes(&boxes));
    assert_eq!(t.outline(stairs), CollisionShape::Boxes(&boxes));
    let fence = state("boxel:oak_fence[north=true]");
    let fence_boxes = [
        unit([6.0, 0.0, 6.0], [10.0, 24.0, 10.0]),
        unit([6.0, 0.0, 0.0], [10.0, 24.0, 6.0]),
    ];
    assert_eq!(t.shape(fence), CollisionShape::Boxes(&fence_boxes));
    let fence_outline = [
        unit([6.0, 0.0, 6.0], [10.0, 16.0, 10.0]),
        unit([6.0, 0.0, 0.0], [10.0, 16.0, 6.0]),
    ];
    assert_eq!(t.outline(fence), CollisionShape::Boxes(&fence_outline));
    assert!(t.is_tall(fence) && !t.is_tall(stairs) && !t.is_tall(STONE));
    assert_eq!(t.reach(), 0.5);
    for name in ["boxel:torch", "boxel:wall_torch[facing=south]"] {
        let torch = state(name);
        assert_eq!(t.shape(torch), CollisionShape::Empty);
        assert!(t.has_outline(torch) && !t.collides(torch));
    }
    assert_eq!(
        t.outline(state("boxel:torch")),
        CollisionShape::Boxes(&[unit([6.0, 0.0, 6.0], [10.0, 10.0, 10.0])])
    );
    // A box falling on bottom stairs facing east lands on the step on the east half and on
    // the slab on the west half.
    let base = I64Vec3::new(5, -3, 2_000_000_000);
    let mut world = TestWorld::new();
    world.set(at(base, 0, 0, 0), state("boxel:stone_stairs[facing=east]"));
    let drop = |world: &mut TestWorld, x: f64| {
        let start = WorldPos::from_block(BlockPos::from_i64vec3(base), DVec3::new(x, 3.0, 0.5));
        let out = sweep(world, start, DVec3::new(0.0, -5.0, 0.0));
        (out.end.block.y - base.y) as f64 + out.end.frac.y
    };
    assert_eq!(drop(&mut world, 0.875), 1.0);
    assert_eq!(drop(&mut world, 0.125), 0.5);
}
