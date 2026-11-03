//! Translation invariance: the same neighborhood (blocks and sky tops moved together) at the
//! origin, across +-2^31, at +-2^40 and next to the world limit (`MAX_BLOCK_COORD - 64`) gives
//! the same light, bit for bit, and takes the same path. The same with sky sources and a realm
//! plane moved with it (ADR 0061): the beam tiles and the light they give. The same with colored
//! sky (ADR 0064): glass roofs, windows and panes, and sources through them.
mod common;

use boxel_light::{Beam, LightPath, LightScratch, PaddedLight, compute};
use boxel_voxel::{CHUNK_SHIFT, ChunkPos, LightTables, MAX_BLOCK_COORD};
use common::{Rng, random_world, test_registry, test_registry_full};

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

/// The shifts the tests move a neighborhood by, in chunks.
fn shifts() -> Vec<ChunkPos> {
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
    shifts
}

#[test]
fn same_neighborhood_same_light_anywhere() {
    let shifts = shifts();
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

/// The center chunk's light of the 3x3x3 world with min chunk `min`, with sky sources and a
/// realm plane `plane` voxels above the world's bottom, and the center column's beams relative
/// to the world's bottom.
fn beam_light_at(seed: u64, min: ChunkPos, plane: i64) -> (PaddedLight, LightPath, Vec<Beam>) {
    let (registry, blocks) = test_registry_full(seed.is_multiple_of(2), false, true);
    let tables = LightTables::from_registry(&registry);
    let mut world = random_world(seed, 3, min, &blocks, &tables);
    world.add_sources(&mut Rng::new(seed), &blocks, &tables);
    let y0 = min.origin().y;
    let tiles = world.tiles();
    let beams = world.beam_tiles(&tables, &[y0 + plane]);
    let input = world.input_with(&tiles, &beams, &tables, (1, 1, 1));
    let mut out = PaddedLight::default();
    let path = compute(&input, &mut LightScratch::new(), &mut out);
    let relative = beams[4]
        .iter()
        .map(|(_, _, b)| Beam {
            source: b.source - y0,
            lo: b.lo - y0,
            hi: b.hi - y0,
            ..b
        })
        .collect();
    (out, path, relative)
}

#[test]
fn same_sources_same_beams_and_light_anywhere() {
    let shifts = shifts();
    let mut lit = 0;
    for seed in [31, 32, 33, 34] {
        let base = ChunkPos::new(-1, -1, -1);
        let (want, path, beams) = beam_light_at(seed, base, 50);
        lit += usize::from(!beams.is_empty() && matches!(path, LightPath::Full { .. }));
        for &s in &shifts {
            let min = ChunkPos::new(base.x + s.x, base.y + s.y, base.z + s.z);
            let (got, got_path, got_beams) = beam_light_at(seed, min, 50);
            assert_eq!(got_path, path, "seed {seed} at {min:?}");
            assert!(got_beams == beams, "seed {seed}: beams differ at {min:?}");
            assert!(got == want, "seed {seed}: light differs at {min:?}");
        }
    }
    assert!(lit > 0, "no beam in any world");
}

/// The center chunk's colored light of the 3x3x3 world with min chunk `min`: glass, sky sources
/// and a realm plane `plane` voxels above the world's bottom, colored tiles and beam tiles.
fn colored_light_at(seed: u64, min: ChunkPos, plane: i64) -> (PaddedLight, LightPath) {
    let (registry, blocks) = test_registry_full(seed.is_multiple_of(2), true, true);
    let tables = LightTables::from_registry(&registry);
    let mut world = random_world(seed, 3, min, &blocks, &tables);
    let mut rng = Rng::new(seed);
    world.add_glass(&mut rng, &blocks);
    world.add_sources(&mut rng, &blocks, &tables);
    let y0 = min.origin().y;
    let tiles = world.colored_tiles(&tables);
    let beams = world.colored_beam_tiles(&tables, &[y0 + plane]);
    let mut input = world.input_with(&tiles, &beams, &tables, (1, 1, 1));
    input.colored_sky = true;
    let mut out = PaddedLight::default();
    let path = compute(&input, &mut LightScratch::new(), &mut out);
    (out, path)
}

#[test]
fn same_glass_same_colored_light_anywhere() {
    let shifts = shifts();
    let mut colored = 0;
    for seed in [41, 42, 43, 44, 45, 46] {
        let base = ChunkPos::new(-1, -1, -1);
        let (want, path) = colored_light_at(seed, base, 50);
        colored += usize::from(want.is_colored());
        for &s in &shifts {
            let min = ChunkPos::new(base.x + s.x, base.y + s.y, base.z + s.z);
            let (got, got_path) = colored_light_at(seed, min, 50);
            assert_eq!(got_path, path, "seed {seed} at {min:?}");
            assert!(got == want, "seed {seed}: colored light differs at {min:?}");
        }
    }
    assert!(colored > 0, "no colored result");
}
