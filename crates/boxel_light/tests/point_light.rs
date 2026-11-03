//! Point lights (ADR 0063) against placed emitters: in random 3x3x3-chunk worlds (opacity 0, 1,
//! 5 and 15, lamps, sky, with and without light filters), the center chunk's light with point
//! lights at air voxels equals, bit for bit, its light with clear emitters of the same emission
//! placed there; the same neighborhood anywhere (across +-2^31, at +-2^40, next to the world
//! limit) gives the same light; a light that cannot reach the chunk changes nothing.
mod common;

use boxel_light::{
    LightPath, LightScratch, PaddedLight, PointEmitter, PointScratch, add_point_lights, compute,
};
use boxel_voxel::{
    BlockPos, BlockState, CHUNK_SHIFT, CHUNK_SIZE, ChunkPos, LightTables, LocalPos, MAX_BLOCK_COORD,
};
use common::{Blocks, Rng, World, random_world, test_registry_full};

/// The center chunk's light of `world` (as `compute` gives it), with `lights` added.
fn center_light(world: &World, tables: &LightTables, lights: &[PointEmitter]) -> PaddedLight {
    let tiles = world.tiles();
    let input = world.input(&tiles, tables, (1, 1, 1));
    let mut out = PaddedLight::default();
    compute(&input, &mut LightScratch::new(), &mut out);
    let reached = add_point_lights(&input, lights, &mut PointScratch::new(), &mut out);
    assert_eq!(
        reached,
        lights
            .iter()
            .filter(|l| l.reaches_chunk(input.origin))
            .count()
    );
    out
}

/// The block at world voxel `p` of `world`, if inside it.
fn block_at(world: &World, p: BlockPos) -> Option<BlockState> {
    let o = world.min.origin();
    let rel = [p.x - o.x, p.y - o.y, p.z - o.z];
    let side = (world.n * CHUNK_SIZE as usize) as i64;
    if rel.iter().any(|&c| !(0..side).contains(&c)) {
        return None;
    }
    let [x, y, z] = rel.map(|c| c as usize);
    let s = CHUNK_SIZE as usize;
    Some(world.chunk(x / s, y / s, z / s).get(LocalPos::new(
        (x % s) as u32,
        (y % s) as u32,
        (z % s) as u32,
    )))
}

/// `world` with `state` at world voxel `p`.
fn place(world: &mut World, p: BlockPos, state: BlockState) {
    let o = world.min.origin();
    let [x, y, z] = [p.x - o.x, p.y - o.y, p.z - o.z].map(|c| c as usize);
    let s = CHUNK_SIZE as usize;
    let n = world.n;
    world.chunks[((y / s) * n + x / s) * n + z / s].set(
        LocalPos::new((x % s) as u32, (y % s) as u32, (z % s) as u32),
        state,
    );
}

/// Up to `count` air voxels near the center chunk (within 20 of it on each axis), picked at
/// random.
fn air_voxels(world: &World, b: &Blocks, rng: &mut Rng, count: usize) -> Vec<BlockPos> {
    let o = world.min.origin();
    let mut out = Vec::new();
    for _ in 0..2000 {
        if out.len() == count {
            break;
        }
        let pick = |rng: &mut Rng, base: i64| base + 32 - 20 + rng.below(72) as i64;
        let p = BlockPos::new(pick(rng, o.x), pick(rng, o.y), pick(rng, o.z));
        if block_at(world, p) == Some(b.air) && !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// Point lights at air voxels give the light of the clear emitters `test:glow` (opacity 0) and
/// `test:lantern` (opacity 1, entered at the cost of air) placed there.
#[test]
fn point_lights_equal_placed_emitters() {
    let mut reached = 0;
    let mut changed = 0;
    for seed in 0..40u64 {
        let (registry, b) = test_registry_full(seed % 3 == 1, seed % 2 == 0, false);
        let tables = LightTables::from_registry(&registry);
        let world = random_world(seed, 3, ChunkPos::new(-1, -1, -1), &b, &tables);
        let mut rng = Rng::new(seed ^ 0xD15C0);
        let spots = air_voxels(&world, &b, &mut rng, 1 + (seed % 3) as usize);
        let mut placed = World {
            min: world.min,
            n: world.n,
            chunks: world.chunks.clone(),
            tops: world.tops.clone(),
        };
        let mut lights = Vec::new();
        for (i, &p) in spots.iter().enumerate() {
            let emitter = b.glows[i % 2];
            place(&mut placed, p, emitter);
            lights.push(PointEmitter::new(p, tables.emission(emitter).rgb()));
        }
        let want = center_light(&placed, &tables, &[]);
        let got = center_light(&world, &tables, &lights);
        assert!(
            got.to_vec() == want.to_vec(),
            "seed {seed}: point lights at {spots:?} differ from placed emitters"
        );
        let center = ChunkPos::new(0, 0, 0);
        reached += lights.iter().filter(|l| l.reaches_chunk(center)).count();
        changed += usize::from(got.to_vec() != center_light(&world, &tables, &[]).to_vec());
    }
    assert!(reached > 30, "lights near the chunk: {reached}");
    assert!(changed > 15, "chunks the lights changed: {changed}");
}

/// A light behind a wall lights only what it can go around to; with the wall closed all around
/// it, nothing out of its room; the sky channel never changes.
#[test]
fn a_light_does_not_cross_an_opaque_wall() {
    let (registry, b) = test_registry_full(false, false, false);
    let tables = LightTables::from_registry(&registry);
    let mut world = random_world(7, 3, ChunkPos::new(-1, -1, -1), &b, &tables);
    for chunk in &mut world.chunks {
        *chunk = boxel_voxel::ChunkData::uniform(b.air);
    }
    // A closed stone box with walls 1 thick around the voxels 10..=12 of the center chunk.
    for x in 9..=13 {
        for y in 9..=13 {
            for z in 9..=13 {
                let inner = [x, y, z].iter().all(|c| (10..=12).contains(c));
                if !inner {
                    place(&mut world, BlockPos::new(x, y, z), b.stone);
                }
            }
        }
    }
    let tiles = world.tiles();
    let input = world.input(&tiles, &tables, (1, 1, 1));
    let mut base = PaddedLight::default();
    compute(&input, &mut LightScratch::new(), &mut base);
    let mut lit = base.clone();
    let light = PointEmitter::new(BlockPos::new(11, 11, 11), [15, 15, 15]);
    add_point_lights(&input, &[light], &mut PointScratch::new(), &mut lit);
    for y in -1..=32 {
        for x in -1..=32 {
            for z in -1..=32 {
                let inner = [x, y, z].iter().all(|c| (10..=12).contains(c));
                let (got, was) = (lit.get(x, y, z), base.get(x, y, z));
                assert_eq!(got.sky(), was.sky(), "sky at ({x}, {y}, {z})");
                if inner {
                    let d = (x - 11).abs() + (y - 11).abs() + (z - 11).abs();
                    let want = (15 - d) as u8;
                    assert_eq!(got.rgb(), [want; 3], "inside at ({x}, {y}, {z})");
                } else {
                    assert_eq!(got.rgb(), was.rgb(), "outside at ({x}, {y}, {z})");
                }
            }
        }
    }
    // With a hole in one wall, the light goes out through it, one level less per voxel.
    place(&mut world, BlockPos::new(13, 11, 11), b.air);
    let tiles = world.tiles();
    let input = world.input(&tiles, &tables, (1, 1, 1));
    let mut out = PaddedLight::default();
    compute(&input, &mut LightScratch::new(), &mut out);
    add_point_lights(&input, &[light], &mut PointScratch::new(), &mut out);
    assert_eq!(out.get(13, 11, 11).rgb(), [13; 3]);
    assert_eq!(out.get(14, 11, 11).rgb(), [12; 3]);
    assert_eq!(out.get(14, 12, 11).rgb(), [11; 3]);
}

/// Lights that cannot reach the padded chunk leave it as it was (uniform stays uniform), and a
/// dark light lights nothing.
#[test]
fn lights_out_of_reach_change_nothing() {
    let (registry, b) = test_registry_full(false, false, false);
    let tables = LightTables::from_registry(&registry);
    let mut world = random_world(3, 3, ChunkPos::new(-1, -1, -1), &b, &tables);
    for chunk in &mut world.chunks {
        *chunk = boxel_voxel::ChunkData::uniform(b.air);
    }
    world.tops.fill(i64::MAX);
    let tiles = world.tiles();
    let input = world.input(&tiles, &tables, (1, 1, 1));
    let mut out = PaddedLight::default();
    assert_eq!(
        compute(&input, &mut LightScratch::new(), &mut out),
        LightPath::FastDark
    );
    let far = [
        PointEmitter::new(BlockPos::new(-16, 5, 5), [15, 15, 15]),
        PointEmitter::new(BlockPos::new(5, 47, 5), [14, 0, 0]),
        PointEmitter::new(BlockPos::new(5, 5, 5), [0, 0, 0]),
    ];
    let mut scratch = PointScratch::new();
    assert_eq!(add_point_lights(&input, &far, &mut scratch, &mut out), 0);
    assert_eq!(out.as_uniform(), Some(boxel_voxel::LightValue::DARK));
    // Just in reach: one voxel of the border lit at level 1.
    let edge = PointEmitter::new(BlockPos::new(-15, 5, 5), [15, 0, 0]);
    assert_eq!(add_point_lights(&input, &[edge], &mut scratch, &mut out), 1);
    assert_eq!(out.get(-1, 5, 5).rgb(), [1, 0, 0]);
    assert_eq!(out.get(0, 5, 5).rgb(), [0; 3]);
}

/// The same neighborhood and lights moved anywhere give the same light (ADR 0014).
#[test]
fn the_same_lights_anywhere_give_the_same_light() {
    let edge = (MAX_BLOCK_COORD - 64) >> CHUNK_SHIFT;
    let shifts = [
        ChunkPos::new(1 << 26, 0, 0),
        ChunkPos::new(0, -(1 << 26), 0),
        ChunkPos::new(0, 0, (1 << 26) - 1),
        ChunkPos::new(1 << 35, -(1 << 35), 0),
        ChunkPos::new(edge, -edge, edge),
        ChunkPos::new(-edge, edge, -edge),
    ];
    let (registry, b) = test_registry_full(true, true, false);
    let tables = LightTables::from_registry(&registry);
    // The world and its light spots come from the seed, relative to the world's min corner.
    let light_at = |seed: u64, min: ChunkPos| {
        let world = random_world(seed, 3, min, &b, &tables);
        let spots = air_voxels(&world, &b, &mut Rng::new(seed), 2);
        let lights: Vec<PointEmitter> = spots
            .iter()
            .map(|&p| PointEmitter::new(p, [14, 11, 6]))
            .collect();
        (center_light(&world, &tables, &lights), lights.len())
    };
    for seed in [11, 12, 13] {
        let base = ChunkPos::new(-1, -1, -1);
        let (want, count) = light_at(seed, base);
        assert_eq!(count, 2);
        for s in shifts {
            let min = ChunkPos::new(base.x + s.x, base.y + s.y, base.z + s.z);
            assert!(
                light_at(seed, min).0 == want,
                "seed {seed}: light differs at {min:?}"
            );
        }
    }
}

/// With colored sky (ADR 0064), in worlds of glass roofs and windows: point lights at air voxels
/// still give the light of placed emitters, and leave the sky's channels as they are (block light
/// through glass is filtered on both sides; a point light never touches the sky).
#[test]
fn point_lights_in_a_colored_box_equal_placed_emitters() {
    let colored_light = |world: &World, tables: &LightTables, lights: &[PointEmitter]| {
        let tiles = world.colored_tiles(tables);
        let mut input = world.input(&tiles, tables, (1, 1, 1));
        input.colored_sky = true;
        let mut out = PaddedLight::default();
        compute(&input, &mut LightScratch::new(), &mut out);
        add_point_lights(&input, lights, &mut PointScratch::new(), &mut out);
        out
    };
    let mut colored = 0;
    for seed in 0..20u64 {
        let (registry, b) = test_registry_full(seed % 3 == 1, true, false);
        let tables = LightTables::from_registry(&registry);
        let mut world = random_world(seed, 3, ChunkPos::new(-1, -1, -1), &b, &tables);
        let mut rng = Rng::new(seed ^ 0x6C0_10E);
        world.add_glass(&mut rng, &b);
        let spots = air_voxels(&world, &b, &mut rng, 2);
        let mut placed = World {
            min: world.min,
            n: world.n,
            chunks: world.chunks.clone(),
            tops: world.tops.clone(),
        };
        let mut lights = Vec::new();
        for (i, &p) in spots.iter().enumerate() {
            let emitter = b.glows[i % 2];
            place(&mut placed, p, emitter);
            lights.push(PointEmitter::new(p, tables.emission(emitter).rgb()));
        }
        let want = colored_light(&placed, &tables, &[]);
        let got = colored_light(&world, &tables, &lights);
        assert!(got.to_vec() == want.to_vec(), "seed {seed}: the light");
        assert!(
            got.to_sky_rgb_vec() == want.to_sky_rgb_vec(),
            "seed {seed}: the sky's channels"
        );
        colored += usize::from(got.is_colored());
    }
    assert!(colored > 5, "colored results: {colored}");
}
