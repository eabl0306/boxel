//! Shared helpers of the physics tests.
#![allow(dead_code, reason = "each test file uses a different subset")]

use boxel_math::{DVec3, I64Vec3};
use boxel_physics::{CollisionTable, LocalBox, TestWorld};
use boxel_voxel::{BlockPos, BlockRegistry, BlockState};

/// `boxel:stone` in `BlockRegistry::default_blocks()`.
pub const STONE: BlockState = BlockState(1);
/// `boxel:leaves` (cutout, solid).
pub const LEAVES: BlockState = BlockState(9);
/// `boxel:water` (translucent fluid, not solid).
pub const WATER: BlockState = BlockState(10);

/// A stone slab in the lower half of its block (top at 0.5). Slabs are built only here and in
/// [`top_slab`], so a change in how slab states are named touches one place.
pub fn bottom_slab() -> BlockState {
    BlockRegistry::default_blocks()
        .by_name("boxel:stone_slab")
        .expect("bottom slab")
}

/// A stone slab in the upper half of its block (from 0.5 to 1).
pub fn top_slab() -> BlockState {
    BlockRegistry::default_blocks()
        .by_name("boxel:stone_slab_top")
        .expect("top slab")
}

/// The collision table of the built-in blocks.
pub fn table() -> CollisionTable {
    CollisionTable::from_registry(&BlockRegistry::default_blocks())
}

/// A player-size box with its origin at the feet: 0.6 x 1.8 x 0.6.
pub fn player() -> LocalBox {
    LocalBox::new(DVec3::new(-0.3, 0.0, -0.3), DVec3::new(0.3, 1.8, 0.3))
}

/// `base + (x, y, z)`.
pub fn at(base: I64Vec3, x: i64, y: i64, z: i64) -> BlockPos {
    BlockPos::from_i64vec3(base + I64Vec3::new(x, y, z))
}

/// Fills `base + min ..= base + max` with `state`.
pub fn fill(world: &mut TestWorld, base: I64Vec3, min: [i64; 3], max: [i64; 3], state: BlockState) {
    world.fill(
        at(base, min[0], min[1], min[2]),
        at(base, max[0], max[1], max[2]),
        state,
    );
}

/// `SplitMix64`: a tiny deterministic generator for random fields.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// A random field of stone with the given density in the box `base + 0 .. base + size` (air
/// elsewhere).
pub fn random_field(base: I64Vec3, size: i64, density: f64, seed: u64) -> TestWorld {
    let mut world = TestWorld::new();
    let mut rng = Rng(seed);
    for y in 0..size {
        for x in 0..size {
            for z in 0..size {
                if rng.unit() < density {
                    world.set(at(base, x, y, z), STONE);
                }
            }
        }
    }
    world
}
