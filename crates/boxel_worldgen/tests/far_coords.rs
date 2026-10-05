//! Terrain far from the origin: the `i64` column API keeps every height inside the former `i32`
//! range bit-identical (values recorded with the `i32` API), has no seam where `i32` used to wrap
//! (`x, z = +-2^31`), and the integer noise gives the same terrain statistics at any distance (no
//! flattening, no repetition, no seam where the lattice cells leave `i32`), deterministically.
use boxel_voxel::{BlockRegistry, BlockState, CHUNK_VOLUME, ChunkKey, ChunkPos, MAX_CHUNK_COORD};
use boxel_worldgen::{TerrainGenerator, WorldGenerator};

/// FNV-1a over the little-endian bytes of each value.
fn fnv(values: impl IntoIterator<Item = u64>) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for v in values {
        for b in v.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// Fingerprint of a generated chunk (every voxel state).
fn chunk_fingerprint(g: &TerrainGenerator, r: &BlockRegistry, pos: ChunkPos) -> u64 {
    let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
    g.generate(ChunkKey::new(pos), r).to_dense(&mut dense);
    fnv(dense.iter().map(|s| u64::from(s.0)))
}

/// Largest `|h(x + 1, z) - h(x, z)|` and `|h(x, z + 1) - h(x, z)|` over `width x depth` columns
/// from `(x0, z0)`.
fn max_step(g: &TerrainGenerator, (x0, z0): (i64, i64), width: i64, depth: i64) -> i32 {
    let mut step = 0;
    for z in z0..z0 + depth {
        for x in x0..x0 + width {
            let h = g.height_at(x, z);
            step = step
                .max((g.height_at(x + 1, z) - h).abs())
                .max((g.height_at(x, z + 1) - h).abs());
        }
    }
    step
}

#[test]
fn heights_inside_i32_are_unchanged() {
    let g = TerrainGenerator::default();
    for (x, z, h) in [
        (0, 0, 87),
        (1, -1, 87),
        (-37, 91, 80),
        (255, 256, 56),
        (-4096, 4095, 68),
        (1_000_000, -999_983, 74),
        (-123_456_789, 98_765_432, 85),
        (1_073_741_824, -1_073_741_824, 75),
        (2_147_483_647, -2_147_483_648, 47),
        (-2_147_483_648, 2_147_483_647, 50),
        (2_147_483_583, 17, 74),
        (-2_147_483_600, -5, 63),
        (999_999_937, 3, 68),
        (7, 1_500_000_001, 67),
        (-1, -1, 87),
        (65_536, -65_536, 62),
    ] {
        assert_eq!(g.height_at(x, z), h, "height_at({x}, {z})");
    }
    // 64x64 windows (x outer), heights as u32 bit patterns.
    for (x0, z0, expected) in [
        (0i64, 0i64, 0x472b_0c8f_9766_17e0),
        (-1_000_000, 777, 0xf38c_435d_ed53_f97a),
        (2_147_483_584, -2_147_483_648, 0x1e4d_4ac6_4cf1_1b96),
        (1_073_741_824, 1_073_741_824, 0x1345_ce26_d660_b2a1),
    ] {
        let g = &g;
        let heights = (0..64)
            .flat_map(|dx| (0..64).map(move |dz| u64::from(g.height_at(x0 + dx, z0 + dz) as u32)));
        assert_eq!(fnv(heights), expected, "window at ({x0}, {z0})");
    }
}

#[test]
fn chunks_inside_i32_are_unchanged() {
    let g = TerrainGenerator::default();
    let r = BlockRegistry::default_blocks();
    // Surface chunks, the last ones inside the former i32 range included.
    for ((x, y, z), expected) in [
        ((0, 2, 0), 0x236f_33b3_fdfd_8784),
        ((-3, 1, 5), 0x52bd_be93_1d8f_7765),
        ((7, 0, -9), 0x8655_f619_c7aa_4d00),
        ((31_250, 2, -31_250), 0xff90_26df_cdc6_01a5),
        ((67_108_863, 1, -67_108_864), 0x138d_dffc_25b2_b405),
        ((-67_108_864, 1, 67_108_863), 0x624b_5308_9b28_6d64),
        ((12_345_678, 2, -8_765_432), 0xf978_88a2_979a_32c4),
        ((-67_108_000, 1, -67_100_000), 0x1107_c3a4_aad5_d766),
    ] {
        let pos = ChunkPos::new(x, y, z);
        assert_eq!(chunk_fingerprint(&g, &r, pos), expected, "{pos:?}");
    }
}

#[test]
fn terrain_has_no_seam_at_the_former_i32_edge() {
    let g = TerrainGenerator::default();
    let near = max_step(&g, (-128, -8), 256, 16).max(max_step(&g, (-8, -128), 16, 256));
    assert!(near > 0, "flat terrain near the origin");
    let edge = 1i64 << 31;
    for (origin, (width, depth)) in [
        // x crosses 2^31 - 1 -> 2^31 and -2^31 - 1 -> -2^31 (z likewise).
        ((edge - 128, 5), (256, 16)),
        ((-edge - 128, 5), (256, 16)),
        ((5, edge - 128), (16, 256)),
        ((5, -edge - 128), (16, 256)),
    ] {
        let step = max_step(&g, origin, width, depth);
        assert_eq!(step, near, "window at {origin:?}");
    }
    // The former wrap glued column i32::MAX to i32::MIN; now its neighbor is 2^31.
    for z in 0..32 {
        let h = g.height_at(edge - 1, z);
        assert!((g.height_at(edge, z) - h).abs() <= near, "z {z}");
    }
}

#[test]
fn chunks_past_i32_are_generated_for_their_own_position() {
    let g = TerrainGenerator::default();
    let r = BlockRegistry::default_blocks();
    // Chunk 2^26 used to wrap onto chunk -2^26 (same origin -2^31): now they differ, and a
    // surface chunk past the edge is not air.
    for z in [-3, 7] {
        let (east, west) = (1i64 << 26, -(1i64 << 26));
        let y = i64::from(g.height_at(east << 5, z << 5) >> 5);
        let far = chunk_fingerprint(&g, &r, ChunkPos::new(east, y, z));
        let alias = chunk_fingerprint(&g, &r, ChunkPos::new(west, y, z));
        assert_ne!(far, alias, "chunk ({east}, {y}, {z})");
        let data = g.generate(ChunkKey::new(ChunkPos::new(east, y, z)), &r);
        assert_ne!(data.as_uniform(), Some(BlockState::AIR));
    }
    // Chunk y = 2^27 - 2 (world y ~4.3e9) used to alias the bedrock layer: it is air.
    let high = g.generate(ChunkKey::new(ChunkPos::new(0, (1 << 27) - 2, 0)), &r);
    assert_eq!(high.as_uniform(), Some(BlockState::AIR));
}

/// `SplitMix64` steps: a deterministic sample of columns.
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Height statistics of 4000 columns spread over `+-2^24` voxels around a center.
#[derive(Debug)]
struct Stats {
    /// Mean `|h(x + 1, z) - h(x, z)|` and `|h(x, z + 1) - h(x, z)|`.
    mean_step: f64,
    /// Largest of those steps.
    max_step: i32,
    /// Share of columns whose height equals the one `offset` voxels away along x or z, per
    /// offset of [`REPEAT_OFFSETS`].
    repeats: [f64; 3],
    /// Distinct heights seen.
    distinct: usize,
}

/// Offsets where a repeating terrain would show: one octave-0 cell (256 voxels), and `2^32`
/// lattice cells of octave 4 and of octave 0 (the period of a lattice wrapped to `i32`).
const REPEAT_OFFSETS: [i64; 3] = [256, 1 << 36, 1 << 40];

fn stats(g: &TerrainGenerator, (cx, cz): (i64, i64)) -> Stats {
    let n = 4000;
    let mut state = 7;
    let (mut sum, mut max_step) = (0, 0);
    let mut repeats = [0; 3];
    let mut seen = std::collections::BTreeSet::new();
    // Toward the origin, so the neighbors never leave i64.
    let toward = |c: i64, d: i64| if c > 0 { c - d } else { c + d };
    for _ in 0..n {
        let dx = (next(&mut state) % (1 << 25)) as i64 - (1 << 24);
        let dz = (next(&mut state) % (1 << 25)) as i64 - (1 << 24);
        let (x, z) = (toward(cx, dx.abs()), toward(cz, dz.abs()));
        let h = g.height_at(x, z);
        seen.insert(h);
        for (nx, nz) in [(toward(x, 1), z), (x, toward(z, 1))] {
            let step = (g.height_at(nx, nz) - h).abs();
            sum += step;
            max_step = max_step.max(step);
        }
        for (count, offset) in repeats.iter_mut().zip(REPEAT_OFFSETS) {
            *count += i32::from(g.height_at(toward(x, offset), z) == h);
            *count += i32::from(g.height_at(x, toward(z, offset)) == h);
        }
    }
    let pairs = f64::from(2 * n);
    Stats {
        mean_step: f64::from(sum) / pairs,
        max_step,
        repeats: repeats.map(|r| f64::from(r) / pairs),
        distinct: seen.len(),
    }
}

/// The former `f64` noise flattened from about 2^35 voxels (its lattice saturated) and repeated
/// there: the integer noise must look like the origin everywhere, up to the ends of `i64`.
#[test]
fn terrain_statistics_match_the_origin_at_any_distance() {
    let g = TerrainGenerator::default();
    let near = stats(&g, (0, 0));
    // The origin itself: real hills, a smooth surface, ~2.6% of equal heights by chance.
    assert!(near.mean_step > 0.05 && near.max_step >= 1, "{near:?}");
    assert!(near.distinct > 40, "{near:?}");
    for center in [
        1i64 << 35,
        -(1 << 35),
        1 << 39,
        -(1 << 39),
        1 << 50,
        -(1 << 50),
        1 << 62,
        -(1 << 62),
        i64::MAX,
        i64::MIN,
    ] {
        let far = stats(&g, (center, center / 3));
        let context = format!("center {center}: {far:?} vs origin {near:?}");
        assert_eq!(far.max_step, near.max_step, "{context}");
        assert!(
            (far.mean_step / near.mean_step - 1.0).abs() < 0.15,
            "{context}"
        );
        for (far, near) in far.repeats.iter().zip(near.repeats) {
            assert!((far - near).abs() < 0.01, "{context}");
        }
        assert!(far.distinct * 4 >= near.distinct * 3, "{context}");
    }
}

#[test]
fn terrain_has_no_seam_where_lattice_cells_leave_i32() {
    let g = TerrainGenerator::default();
    let near = max_step(&g, (-128, -8), 256, 16).max(max_step(&g, (-8, -128), 16, 256));
    // Octave k has 2^k / 256 cells per voxel: its cell 2^31 (out of i32) and 2^32 (the low 32
    // bits wrap) are at voxel 2^(39 - k) and 2^(40 - k). Also voxel 2^32, both signs.
    let mut edges = vec![1i64 << 32];
    for k in 0..5 {
        edges.extend([1 << (39 - k), 1 << (40 - k)]);
    }
    for edge in edges {
        for e in [edge, -edge] {
            let along_x = max_step(&g, (e - 128, 5), 256, 16);
            let along_z = max_step(&g, (5, e - 128), 16, 256);
            assert!(along_x >= 1 && along_x <= near, "x across {e}: {along_x}");
            assert!(along_z >= 1 && along_z <= near, "z across {e}: {along_z}");
        }
    }
}

#[test]
fn far_terrain_is_deterministic() {
    // Recorded with the integer noise: a change here means the far terrain changed.
    let g = TerrainGenerator::default();
    for (x, z, h) in [
        (1 << 35, -(1 << 35), 42),
        ((1 << 39) + 17, 5, 47),
        (-(1 << 50), (1 << 50) - 1, 62),
        (1 << 62, -(1 << 62), 54),
        (i64::MAX, i64::MIN, 58),
        (i64::MIN, i64::MAX, 34),
        ((1 << 61) - 1, -(1 << 61), 71),
        (-7, (1 << 44) + 3, 57),
    ] {
        assert_eq!(g.height_at(x, z), h, "height_at({x}, {z})");
    }
    let g = &g;
    let heights = (0..64).flat_map(|dx| {
        (0..64).map(move |dz| u64::from(g.height_at((1 << 40) + dx, -(1 << 40) + dz) as u32))
    });
    assert_eq!(fnv(heights), 0x58dd_393c_15ef_537a, "window at 2^40");
    // Surface chunks, at the world limit included.
    let r = BlockRegistry::default_blocks();
    let m = MAX_CHUNK_COORD;
    for ((x, y, z), expected) in [
        ((1 << 35, 2, -(1 << 35)), 0x25f0_97a7_17df_1804),
        ((-(1 << 45), 1, 1 << 45), 0x58e6_5735_836b_8726),
        ((m, 2, -m), 0xe3cd_0ca0_c028_17e5),
        ((-m, 0, m - 1), 0x5699_4c5b_ff49_4306),
    ] {
        let pos = ChunkPos::new(x, y, z);
        assert_eq!(chunk_fingerprint(g, &r, pos), expected, "{pos:?}");
        // Generated twice, by two generators: the same chunk.
        let again = TerrainGenerator::default();
        assert_eq!(
            chunk_fingerprint(&again, &r, pos),
            expected,
            "{pos:?} again"
        );
    }
}
