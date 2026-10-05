//! Terrain far from the origin: the `i64` column API keeps every height inside the former `i32`
//! range bit-identical (values recorded with the `i32` API) and has no seam where `i32` used to
//! wrap (`x, z = +-2^31`).
use boxel_voxel::{BlockRegistry, BlockState, CHUNK_VOLUME, ChunkKey, ChunkPos};
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
