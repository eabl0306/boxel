//! Translation invariance: the same scene shifted by an integer vector (the former `i32` edge,
//! `2^40`, `2^52` and the world limit, per axis and on all axes) gives bit-identical sweeps,
//! raycasts and contacts between bodies (narrowphase, broadphase pairs, rays and box queries
//! against shapes). Physics never computes with absolute coordinates, so this holds exactly, not
//! approximately.
mod common;

use boxel_math::{DVec3, I64Vec3};
use boxel_physics::core::box_box_moving;
use boxel_physics::{
    BlockBounds, CollisionLayers, Grid, LocalBox, RayResult, Shape, SweepConfig, TestWorld,
    Unloaded, VoxelFilter, move_box, raycast_voxels, shape_contact,
};
use boxel_voxel::{BlockPos, ChunkPos, MAX_BLOCK_COORD, WorldPos};
use common::{STONE, WATER, at, fill, player, table};

const SCALARS: [i64; 7] = [
    0,
    1 << 31,
    -(1 << 31),
    1 << 40,
    1 << 52,
    MAX_BLOCK_COORD - 64,
    -(MAX_BLOCK_COORD - 64),
];

/// Every shift: each scalar on x, on y, on z, and on all three with alternating signs.
fn shifts() -> Vec<I64Vec3> {
    SCALARS
        .iter()
        .flat_map(|&s| {
            [
                I64Vec3::new(s, 0, 0),
                I64Vec3::new(0, s, 0),
                I64Vec3::new(0, 0, s),
                I64Vec3::new(s, -s, s),
            ]
        })
        .collect()
}

/// A small course around `base`: a floor, a wall, a pillar, a pool and a ledge.
fn scene(base: I64Vec3) -> TestWorld {
    let mut world = TestWorld::new();
    fill(&mut world, base, [-8, -1, -8], [8, -1, 8], STONE);
    fill(&mut world, base, [4, 0, -8], [4, 2, 8], STONE);
    fill(&mut world, base, [-2, 0, -2], [-2, 1, -2], STONE);
    fill(&mut world, base, [1, 0, -5], [2, 0, -4], WATER);
    fill(&mut world, base, [-6, 0, 2], [-4, 0, 4], STONE);
    world
}

/// One step of the record: everything relative to `base`, floats as bits.
type SweepStep = (I64Vec3, [u64; 3], [u64; 3], u8, Option<I64Vec3>, bool, bool);

fn bits(v: DVec3) -> [u64; 3] {
    v.to_array().map(f64::to_bits)
}

fn sweep_course(base: I64Vec3) -> Vec<Result<SweepStep, I64Vec3>> {
    let mut world = scene(base);
    let table = table();
    let cfg = SweepConfig::default();
    let moves = [
        DVec3::new(0.0, -10.0, 0.0),
        DVec3::new(2.7, -0.0078125, 0.4),
        DVec3::new(1.3, -0.0078125, 0.0),
        DVec3::new(-5.3, -0.0078125, -3.1),
        DVec3::new(-0.6, -0.0078125, -0.9),
        DVec3::new(0.0, 1.5, 0.0),
        DVec3::new(-3.0, -2.0, 4.0),
        DVec3::new(3.25, -1.0, -8.5),
        DVec3::new(0.0, -0.5, 0.0),
        // Into the pool (x 1..3, z -5..-3).
        DVec3::new(1.0, -0.0078125, -4.0),
        DVec3::new(0.4, 0.0, 0.3),
    ];
    let mut pos = WorldPos::from_block(at(base, 0, 5, 0), DVec3::new(0.3, 0.25, 0.7));
    let mut out = Vec::new();
    for (i, d) in moves.into_iter().enumerate() {
        if i == 6 {
            // A block placed into the body: the next move starts by pushing it out.
            world.set(pos.block, STONE);
        }
        if i == 10 {
            // The body's chunk unloads: the move must stop.
            world.set_unloaded(ChunkPos::containing(pos.block), true);
        }
        let step = match move_box(&mut world, &table, pos, player(), d, &[], &cfg) {
            Ok(r) => {
                pos = r.end;
                Ok((
                    r.end.block.as_i64vec3() - base,
                    bits(r.end.frac),
                    bits(r.moved),
                    r.faces,
                    r.ground.map(|g| g.as_i64vec3() - base),
                    r.in_fluid,
                    r.depenetrated,
                ))
            }
            Err(Unloaded { block }) => Err(block.as_i64vec3() - base),
        };
        out.push(step);
    }
    out
}

#[test]
fn sweeps_are_bit_identical_at_any_shift() {
    let reference = sweep_course(I64Vec3::ZERO);
    // The course does what it says: lands, touches walls, swims, gets pushed out, freezes.
    let ok: Vec<&SweepStep> = reference.iter().filter_map(|s| s.as_ref().ok()).collect();
    assert!(ok.iter().any(|s| s.3 & 1 << 1 != 0), "landed");
    assert!(ok.iter().any(|s| s.3 & 0b11_1100 != 0), "hit a wall");
    assert!(ok.iter().any(|s| s.5), "swam");
    assert!(ok.iter().any(|s| s.6), "pushed out");
    assert!(
        reference.last().is_some_and(Result::is_err),
        "froze on the unloaded chunk"
    );
    for shift in shifts() {
        assert_eq!(sweep_course(shift), reference, "shift {shift:?}");
    }
}

/// A ray result relative to `base`, floats as bits.
type RayRecord = Option<(I64Vec3, Option<u8>, u64, I64Vec3, [u64; 3])>;

fn ray_course(base: I64Vec3) -> Vec<RayRecord> {
    let world = scene(base);
    let table = table();
    let mut out = Vec::new();
    let origins = [
        DVec3::new(0.3, 1.62, 0.7),
        DVec3::new(-3.9, 3.25, 6.1),
        DVec3::new(2.5, 0.5, -4.5),
    ];
    let dirs = [
        DVec3::new(0.8, -0.3, 0.1),
        DVec3::new(-0.2, -0.9, -0.4),
        DVec3::new(1.0, 0.0, 0.0),
        DVec3::new(-1.0, -1.0, -1.0),
        DVec3::new(0.0, 0.0, -1.0),
        DVec3::new(0.3, 0.05, -0.95),
    ];
    for o in origins {
        let origin = WorldPos::from_block(BlockPos::from_i64vec3(base), o);
        for dir in dirs {
            for fluids in [false, true] {
                let r = raycast_voxels(
                    &mut &world,
                    &table,
                    origin,
                    dir,
                    40.0,
                    VoxelFilter::Outline { fluids },
                );
                out.push(match r {
                    RayResult::Hit(h) => Some((
                        h.block.as_i64vec3() - base,
                        h.face.map(|f| f as u8),
                        h.t.to_bits(),
                        h.point.block.as_i64vec3() - base,
                        bits(h.point.frac),
                    )),
                    RayResult::Miss => None,
                    RayResult::Unloaded { .. } => unreachable!("all loaded"),
                });
            }
        }
    }
    out
}

#[test]
fn raycasts_are_bit_identical_at_any_shift() {
    let reference = ray_course(I64Vec3::ZERO);
    assert!(
        reference.iter().filter(|r| r.is_some()).count() > 20,
        "{reference:?}"
    );
    assert!(reference.iter().any(Option::is_none), "some rays miss");
    for shift in shifts() {
        assert_eq!(ray_course(shift), reference, "shift {shift:?}");
    }
}

/// Bodies of the contact course: position (relative to the base) and shape.
fn bodies() -> Vec<(DVec3, Shape)> {
    let crate_shape = Shape::Aabb {
        half: DVec3::splat(0.375),
    };
    let ball = Shape::Sphere { radius: 0.3 };
    vec![
        (DVec3::new(0.5, 0.375, 0.5), crate_shape),
        (DVec3::new(0.7, 1.05, 0.45), crate_shape),
        (DVec3::new(1.1, 0.4, 0.6), ball),
        (DVec3::new(-3.3, 2.0, 7.9), ball),
        (DVec3::new(-3.05, 2.2, 8.1), ball),
        (DVec3::new(-2.6, 1.9, 8.4), crate_shape),
        (DVec3::new(31.9, -0.2, -31.95), crate_shape),
        (DVec3::new(32.3, 0.1, -32.2), ball),
    ]
}

/// Everything between the bodies at `base`: their pairs, every pair's contact (and the
/// contact with a step's motion), a ray against each and a box query, relative and as bits.
fn contact_course(base: I64Vec3) -> Vec<Vec<u64>> {
    let at = |p: DVec3| WorldPos::from_block(BlockPos::from_i64vec3(base), p);
    let bodies: Vec<(WorldPos, Shape)> = bodies().into_iter().map(|(p, s)| (at(p), s)).collect();
    let mut grid = Grid::new(2);
    for (pos, shape) in &bodies {
        grid.insert(
            BlockBounds::of_box(*pos, shape.bounds()),
            CollisionLayers::default(),
            true,
        );
    }
    let mut pairs = Vec::new();
    grid.pairs(&mut pairs);
    let mut out = vec![
        pairs
            .iter()
            .flat_map(|&(a, b)| [u64::from(a), u64::from(b)])
            .collect(),
    ];
    for &(a, b) in &pairs {
        let ((pa, sa), (pb, sb)) = (bodies[a as usize], bodies[b as usize]);
        let rel = pa.delta_to(&pb);
        let mut row = Vec::new();
        if let Some(c) = shape_contact(sa, DVec3::ZERO, sb, rel) {
            row.extend(bits(c.normal));
            row.push(c.depth.to_bits());
        }
        if let (Shape::Aabb { half: ha }, Shape::Aabb { half: hb }) = (sa, sb)
            && let Some(c) = box_box_moving(
                LocalBox::from_center_half(DVec3::ZERO, ha),
                LocalBox::from_center_half(rel, hb),
                DVec3::new(0.01, -0.3, 0.02),
                1e-7,
            )
        {
            row.extend(bits(c.normal));
            row.push(c.depth.to_bits());
        }
        out.push(row);
    }
    // A ray from a point near each body toward its center, and a query box around it.
    for (pos, shape) in &bodies {
        let origin = pos.offset(DVec3::new(-1.3, 0.45, 0.2));
        let center = origin.delta_to(pos);
        let hit = shape.ray(center, DVec3::ZERO, center * 1.5, 1.0);
        let mut row: Vec<u64> = hit
            .map(|h| {
                [
                    h.t.to_bits(),
                    bits(h.normal)[0],
                    bits(h.normal)[1],
                    bits(h.normal)[2],
                ]
                .to_vec()
            })
            .unwrap_or_default();
        let mut found = Vec::new();
        grid.query(
            BlockBounds::of_box(
                *pos,
                LocalBox::from_center_half(DVec3::ZERO, DVec3::splat(1.0)),
            ),
            &mut found,
        );
        row.extend(found.into_iter().map(u64::from));
        out.push(row);
    }
    out
}

#[test]
fn contacts_between_bodies_are_bit_identical_at_any_shift() {
    let reference = contact_course(I64Vec3::ZERO);
    assert!(reference[0].len() >= 6, "some pairs: {:?}", reference[0]);
    assert!(reference[1..].iter().any(|row| !row.is_empty()));
    for shift in shifts() {
        assert_eq!(contact_course(shift), reference, "shift {shift:?}");
    }
}
