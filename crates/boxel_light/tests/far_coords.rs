//! Translation invariance: the same neighborhood (blocks and sky tops moved together) at the
//! origin, across +-2^31, at +-2^40 and next to the world limit (`MAX_BLOCK_COORD - 64`) gives
//! the same light, bit for bit, and takes the same path.
mod common;

use boxel_light::{LightPath, LightScratch, PaddedLight, compute};
use boxel_voxel::{CHUNK_SHIFT, ChunkPos, LightTables, MAX_BLOCK_COORD};
use common::{random_world, test_registry};

/// The center chunk's light of the 3x3x3 world with min chunk `min`.
fn light_at(seed: u64, heavy: bool, min: ChunkPos) -> (PaddedLight, LightPath) {
    let (registry, blocks) = test_registry(heavy);
    let tables = LightTables::from_registry(&registry);
    let world = random_world(seed, 3, min, &blocks, &tables);
    let tiles = world.tiles();
    let input = world.input(&tiles, &tables, (1, 1, 1));
    let mut out = PaddedLight::default();
    let path = compute(&input, &mut LightScratch::new(), &mut out);
    (out, path)
}

#[test]
fn same_neighborhood_same_light_anywhere() {
    // Chunk offsets: 2^26 chunks is 2^31 voxels, 2^35 chunks is 2^40 voxels.
    let edge = (MAX_BLOCK_COORD - 64) >> CHUNK_SHIFT;
    let near = [0, 1 << 26, -(1 << 26), (1 << 26) - 1, -(1 << 26) - 1];
    // The center chunk at +-edge holds voxel +-(MAX_BLOCK_COORD - 64).
    let far = [1 << 35, -(1 << 35), edge, -edge];
    let mut shifts: Vec<ChunkPos> = Vec::new();
    for &v in near.iter().chain(&far) {
        shifts.push(ChunkPos::new(v, 0, 0));
        shifts.push(ChunkPos::new(0, v, 0));
        shifts.push(ChunkPos::new(0, 0, v));
    }
    shifts.push(ChunkPos::new(edge, -edge, edge));
    shifts.push(ChunkPos::new(-(1 << 35), 1 << 35, -(1 << 26)));
    let mut paths = Vec::new();
    for (seed, heavy) in [(21, false), (22, true), (23, false), (24, false)] {
        let base = ChunkPos::new(-1, -1, -1);
        let (want, path) = light_at(seed, heavy, base);
        paths.push(path);
        for &s in &shifts {
            let min = ChunkPos::new(base.x + s.x, base.y + s.y, base.z + s.z);
            let (got, got_path) = light_at(seed, heavy, min);
            assert_eq!(got_path, path, "seed {seed} at {min:?}");
            assert!(got == want, "seed {seed}: light differs at {min:?}");
        }
    }
    assert!(paths.iter().any(|p| matches!(p, LightPath::Full { .. })));
}
