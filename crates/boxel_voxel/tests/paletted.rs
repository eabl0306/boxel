//! Paletted storage and `ChunkData` behavior: set/get against a dense mirror, encode/decode,
//! growth, compaction, version rules, occupancy and heap accounting.
use boxel_voxel::*;
use proptest::prelude::*;

/// Small deterministic generator (xorshift64*) for building test chunks.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn dense_of(p: &Paletted) -> Vec<BlockState> {
    let mut out = vec![BlockState::AIR; CHUNK_VOLUME];
    p.decode_into(&mut out);
    out
}

fn distinct(states: &[BlockState]) -> usize {
    states
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

/// Smallest width for `n` distinct states, as `Paletted::encode` must pick it.
fn width_for(n: usize) -> u8 {
    match n {
        1 => 0,
        2 => 1,
        3..=4 => 2,
        5..=16 => 4,
        17..=256 => 8,
        _ => 16,
    }
}

/// Structural invariants of a container (valid widths, palette fits, word count).
fn check_invariants(p: &Paletted) {
    match p {
        Paletted::Uniform(_) => {
            assert_eq!((p.palette_len(), p.bits_per_voxel()), (1, 0));
        }
        Paletted::Packed {
            palette,
            bits,
            words,
        } => {
            assert!(matches!(bits, 1 | 2 | 4 | 8), "bits {bits}");
            assert!(palette.len() >= 2 && palette.len() <= 1 << bits);
            assert_eq!(words.len(), CHUNK_VOLUME * *bits as usize / 64);
            assert_eq!(p.palette_len(), palette.len());
            assert_eq!(p.bits_per_voxel(), *bits);
        }
        Paletted::Direct(d) => {
            assert_eq!(d.len(), CHUNK_VOLUME);
            assert_eq!((p.palette_len(), p.bits_per_voxel()), (0, 16));
        }
    }
}

/// Asserts `p` is the minimal representation of its contents.
fn check_minimal(p: &Paletted) {
    let dense = dense_of(p);
    let n = distinct(&dense);
    assert_eq!(p.bits_per_voxel(), width_for(n), "{n} distinct states");
    if n <= 256 {
        assert_eq!(p.palette_len(), n);
    }
}

/// A dense chunk with `k` distinct states (ids below `max_id`) laid out in runs of up to `run`.
fn random_dense(seed: u64, k: usize, max_id: u16, run: usize) -> Vec<BlockState> {
    let mut rng = Rng(seed | 1);
    let mut ids: Vec<BlockState> = Vec::with_capacity(k);
    while ids.len() < k {
        let s = BlockState(rng.below(u64::from(max_id)) as u16);
        if !ids.contains(&s) {
            ids.push(s);
        }
    }
    let mut out = Vec::with_capacity(CHUNK_VOLUME);
    // Every state appears at least once, then random runs.
    out.extend(ids.iter().copied());
    while out.len() < CHUNK_VOLUME {
        let s = ids[rng.below(k as u64) as usize];
        let len = 1 + rng.below(run as u64) as usize;
        for _ in 0..len.min(CHUNK_VOLUME - out.len()) {
            out.push(s);
        }
    }
    // Shuffle the first k entries away from the front so first-seen order is not trivial.
    for i in 0..k {
        let j = rng.below(CHUNK_VOLUME as u64) as usize;
        out.swap(i, j);
    }
    out
}

proptest! {
    #[test]
    fn set_get_matches_dense_mirror(
        ops in prop::collection::vec((0..CHUNK_VOLUME, 0u16..300), 0..2000),
        hot in 0usize..2,
    ) {
        let mut chunk = ChunkData::uniform(BlockState::AIR);
        let mut mirror = vec![BlockState::AIR; CHUNK_VOLUME];
        let mut version = 0u32;
        for (i, s) in ops {
            // `hot == 1` folds positions into 512 voxels so states get overwritten often.
            let i = if hot == 1 { i % 512 } else { i };
            let p = LocalPos::from_index(i);
            let s = BlockState(s);
            let previous = chunk.set(p, s);
            prop_assert_eq!(previous, mirror[i]);
            if previous != s {
                version += 1;
            }
            mirror[i] = s;
            prop_assert_eq!(chunk.get(p), s);
            prop_assert_eq!(chunk.version, version);
        }
        check_invariants(chunk.blocks());
        let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
        chunk.to_dense(&mut dense);
        prop_assert!(dense == mirror);
        for i in (0..CHUNK_VOLUME).step_by(97) {
            prop_assert_eq!(chunk.get(LocalPos::from_index(i)), mirror[i]);
        }
        // Compaction keeps the contents and yields the minimal representation.
        let mut compacted = chunk.blocks().clone();
        compacted.compact();
        check_invariants(&compacted);
        prop_assert!(dense_of(&compacted) == mirror);
        check_minimal(&compacted);
        prop_assert!(compacted.heap_bytes() <= chunk.blocks().heap_bytes());
    }

    #[test]
    fn encode_decode_roundtrip(
        seed in any::<u64>(),
        k in 1usize..=300,
        max_id in 300u16..=u16::MAX,
        run in 1usize..64,
    ) {
        let dense = random_dense(seed, k, max_id, run);
        let p = Paletted::encode(&dense);
        check_invariants(&p);
        check_minimal(&p);
        prop_assert!(dense_of(&p) == dense);
        for i in (0..CHUNK_VOLUME).step_by(61) {
            prop_assert_eq!(p.get(LocalPos::from_index(i)), dense[i]);
        }
        // First-seen palette order.
        if let Paletted::Packed { palette, .. } = &p {
            let mut seen = Vec::new();
            for s in &dense {
                if !seen.contains(s) {
                    seen.push(*s);
                }
            }
            prop_assert_eq!(palette, &seen);
        }
        // Already minimal: compaction changes nothing, re-encoding gives the same value.
        let mut compacted = p.clone();
        compacted.compact();
        prop_assert_eq!(&compacted, &p);
        prop_assert_eq!(&Paletted::encode(&dense_of(&p)), &p);
        let chunk = ChunkData::from_dense(&dense);
        prop_assert_eq!(chunk.blocks(), &p);
        prop_assert_eq!(chunk.version, 0);
    }
}

#[test]
fn encode_picks_the_smallest_width() {
    for (k, bits) in [
        (1, 0),
        (2, 1),
        (3, 2),
        (4, 2),
        (5, 4),
        (16, 4),
        (17, 8),
        (256, 8),
        (257, 16),
    ] {
        let dense = random_dense(k as u64, k, 1000, 8);
        let p = Paletted::encode(&dense);
        assert_eq!(p.bits_per_voxel(), bits, "{k} states");
        assert_eq!(dense_of(&p), dense);
        match k {
            1 => assert_eq!(p.as_uniform(), Some(dense[0])),
            257 => assert!(matches!(p, Paletted::Direct(_))),
            _ => assert_eq!(p.palette_len(), k),
        }
    }
}

#[test]
fn growth_sequence_is_1_2_4_8_then_direct() {
    let mut p = Paletted::Uniform(BlockState::AIR);
    let mut mirror = vec![BlockState::AIR; CHUNK_VOLUME];
    assert_eq!(p.bits_per_voxel(), 0);
    let mut widths = vec![0];
    // Voxel i (spread over the chunk) gets the new state i + 1: distinct count = i + 2.
    for i in 0..256u16 {
        let index = usize::from(i) * 127 % CHUNK_VOLUME;
        let s = BlockState(i + 1);
        assert_eq!(p.set(LocalPos::from_index(index), s), BlockState::AIR);
        mirror[index] = s;
        let distinct = usize::from(i) + 2;
        assert_eq!(p.bits_per_voxel(), width_for(distinct), "{distinct} states");
        if widths.last() != Some(&p.bits_per_voxel()) {
            widths.push(p.bits_per_voxel());
            // Re-encoding at a new width keeps every voxel.
            assert_eq!(dense_of(&p), mirror);
        }
        check_invariants(&p);
    }
    assert_eq!(widths, [0, 1, 2, 4, 8, 16]);
    assert!(matches!(p, Paletted::Direct(_)));
    assert_eq!(dense_of(&p), mirror);
}

#[test]
fn set_reuses_palette_entries_and_appends_in_place() {
    let mut p = Paletted::Uniform(BlockState(7));
    let a = LocalPos::new(1, 2, 3);
    let b = LocalPos::new(31, 31, 31);
    assert_eq!(p.set(a, BlockState(7)), BlockState(7));
    assert_eq!(p, Paletted::Uniform(BlockState(7)), "same state: no change");
    p.set(a, BlockState(8));
    p.set(b, BlockState(9));
    assert_eq!(p.bits_per_voxel(), 2);
    // Writing a known state again does not grow the palette.
    p.set(a, BlockState(7));
    p.set(b, BlockState(8));
    assert_eq!(p.palette_len(), 3);
    // Room left at 2 bits: the 4th state is appended without widening.
    p.set(a, BlockState(10));
    assert_eq!((p.palette_len(), p.bits_per_voxel()), (4, 2));
    assert_eq!(p.get(a), BlockState(10));
    assert_eq!(p.get(b), BlockState(8));
    assert_eq!(p.get(LocalPos::new(0, 0, 0)), BlockState(7));
    // `set` never shrinks: the chunk is uniform again but stays packed until compacted.
    p.set(a, BlockState(7));
    p.set(b, BlockState(7));
    assert_eq!(p.bits_per_voxel(), 2);
    p.compact();
    assert_eq!(p, Paletted::Uniform(BlockState(7)));
}

#[test]
fn compact_collapses_to_uniform() {
    let mut c = ChunkData::uniform(BlockState::AIR);
    let p = LocalPos::new(4, 5, 6);
    c.set(p, BlockState(1));
    c.set(p, BlockState::AIR);
    assert!(matches!(c.blocks(), Paletted::Packed { .. }));
    assert_eq!(c.version, 2);
    c.compact();
    assert_eq!(c.blocks(), &Paletted::Uniform(BlockState::AIR));
    assert_eq!(c.as_uniform(), Some(BlockState::AIR));
    assert_eq!(c.heap_bytes(), 0);

    // A Direct container whose contents became uniform collapses too.
    let mut d = Paletted::encode(&random_dense(3, 300, 2000, 4));
    assert!(matches!(d, Paletted::Direct(_)));
    for i in 0..CHUNK_VOLUME {
        d.set(LocalPos::from_index(i), BlockState(2));
    }
    assert!(matches!(d, Paletted::Direct(_)), "set never narrows");
    d.compact();
    assert_eq!(d, Paletted::Uniform(BlockState(2)));
}

#[test]
fn compact_narrows_and_keeps_contents() {
    // 20 states -> 8 bits; overwrite until only 3 remain -> 2 bits.
    let dense = random_dense(11, 20, 500, 16);
    let mut p = Paletted::encode(&dense);
    assert_eq!(p.bits_per_voxel(), 8);
    let mut keep: Vec<BlockState> = Vec::new();
    for s in &dense {
        if keep.len() < 3 && !keep.contains(s) {
            keep.push(*s);
        }
    }
    let mut mirror = dense.clone();
    for (i, slot) in mirror.iter_mut().enumerate() {
        if !keep.contains(slot) {
            *slot = keep[i % 3];
            p.set(LocalPos::from_index(i), *slot);
        }
    }
    assert_eq!(p.bits_per_voxel(), 8);
    let before = p.heap_bytes();
    p.compact();
    check_invariants(&p);
    assert_eq!(
        (p.palette_len(), p.bits_per_voxel()),
        (distinct(&mirror), 2)
    );
    assert_eq!(dense_of(&p), mirror);
    assert!(p.heap_bytes() < before / 3);

    // Direct with few remaining states becomes Packed.
    let mut d = Paletted::encode(&random_dense(5, 280, 1000, 2));
    assert!(matches!(d, Paletted::Direct(_)));
    let mut mirror = dense_of(&d);
    for (i, slot) in mirror.iter_mut().enumerate() {
        let s = BlockState((i % 5) as u16);
        *slot = s;
        d.set(LocalPos::from_index(i), s);
    }
    d.compact();
    assert_eq!((d.palette_len(), d.bits_per_voxel()), (5, 4));
    assert_eq!(dense_of(&d), mirror);

    // Direct with more than 256 states stays Direct.
    let many = random_dense(6, 300, 1000, 2);
    let mut d = Paletted::encode(&many);
    d.compact();
    assert!(matches!(d, Paletted::Direct(_)));
    assert_eq!(dense_of(&d), many);
}

#[test]
fn compact_handles_hand_built_palettes() {
    // Unused and duplicate entries, wider than needed: [5, 6 (unused), 5 (dup), 9] at 8 bits.
    let mut words = vec![0u64; CHUNK_VOLUME / 8].into_boxed_slice();
    // Voxel 1 uses index 2 (state 5 again), voxel 2 uses index 3 (state 9).
    words[0] = (2 << 8) | (3 << 16);
    let mut p = Paletted::Packed {
        palette: vec![BlockState(5), BlockState(6), BlockState(5), BlockState(9)],
        bits: 8,
        words,
    };
    let before = dense_of(&p);
    p.compact();
    assert_eq!(p.palette_len(), 2);
    assert_eq!(p.bits_per_voxel(), 1);
    assert_eq!(dense_of(&p), before);
    assert_eq!(p.get(LocalPos::from_index(2)), BlockState(9));
}

#[test]
fn version_bumps_only_on_change_and_wraps() {
    let mut c = ChunkData::uniform(BlockState(3));
    let p = LocalPos::new(0, 31, 0);
    assert_eq!(c.set(p, BlockState(3)), BlockState(3));
    assert_eq!(c.version, 0, "unchanged state, no bump");
    assert_eq!(c.set(p, BlockState(4)), BlockState(3));
    assert_eq!(c.version, 1);
    assert_eq!(c.set(p, BlockState(4)), BlockState(4));
    assert_eq!(c.version, 1);
    c.version = u32::MAX;
    c.set(p, BlockState(5));
    assert_eq!(c.version, 0, "wrapping bump");
    // Direct containers follow the same rule.
    let mut d = ChunkData::from_dense(&random_dense(8, 300, 1000, 2));
    assert!(matches!(d.blocks(), Paletted::Direct(_)));
    let old = d.get(p);
    assert_eq!(d.set(p, old), old);
    assert_eq!(d.version, 0);
    d.set(p, BlockState(old.0.wrapping_add(1)));
    assert_eq!(d.version, 1);
}

fn expected_occupancy(states: &[BlockState], registry: &BlockRegistry) -> Vec<u32> {
    let mut occ = vec![0u32; 1024];
    for x in 0..32 {
        for y in 0..32 {
            for z in 0..32 {
                let s = states[LocalPos::new(x, y, z).index()];
                if registry.shape(s) == RenderShape::Opaque && registry.model(s).is_solid_cube() {
                    occ[(y * 32 + x) as usize] |= 1 << z;
                }
            }
        }
    }
    occ
}

#[test]
fn occupancy_bits_follow_opaque_shapes() {
    let r = boxel_demo::blocks::registry();
    let stone = r.by_name("boxel:stone").unwrap();
    let leaves = r.by_name("boxel:leaves").unwrap();
    let water = r.by_name("boxel:water").unwrap();
    let dirt = r.by_name("boxel:dirt").unwrap();

    assert!(
        ChunkData::uniform(stone)
            .opaque_occupancy(&r)
            .iter()
            .all(|&w| w == u32::MAX)
    );
    for s in [BlockState::AIR, leaves, water, BlockState(999)] {
        assert!(
            ChunkData::uniform(s)
                .opaque_occupancy(&r)
                .iter()
                .all(|&w| w == 0),
            "{s:?}"
        );
    }

    // Single voxel: bit z of word y * 32 + x.
    let mut c = ChunkData::uniform(BlockState::AIR);
    c.set(LocalPos::new(3, 5, 7), stone);
    let occ = c.opaque_occupancy(&r);
    assert_eq!(occ[5 * 32 + 3], 1 << 7);
    assert_eq!(occ.iter().map(|w| w.count_ones()).sum::<u32>(), 1);

    // Mixed shapes in every representation (1, 2, 4, 8 bits and Direct).
    let mut rng = Rng(77);
    let shapes = [BlockState::AIR, stone, leaves, water, dirt];
    for k in [2usize, 3, 5, 5] {
        let dense: Vec<BlockState> = (0..CHUNK_VOLUME)
            .map(|_| shapes[rng.below(k as u64) as usize])
            .collect();
        let c = ChunkData::from_dense(&dense);
        assert_eq!(
            c.opaque_occupancy(&r)[..],
            expected_occupancy(&dense, &r)[..]
        );
    }
    let mut wide: Vec<BlockState> = (0..CHUNK_VOLUME)
        .map(|i| shapes[rng.below(5) as usize].max(BlockState((i % 40) as u16 * 3)))
        .collect();
    let c = ChunkData::from_dense(&wide);
    assert_eq!(c.blocks().bits_per_voxel(), 8);
    assert_eq!(
        c.opaque_occupancy(&r)[..],
        expected_occupancy(&wide, &r)[..]
    );
    for (i, s) in wide.iter_mut().enumerate() {
        *s = BlockState((i % 400) as u16);
    }
    let c = ChunkData::from_dense(&wide);
    assert!(matches!(c.blocks(), Paletted::Direct(_)));
    assert_eq!(
        c.opaque_occupancy(&r)[..],
        expected_occupancy(&wide, &r)[..]
    );
}

/// The states of a mask, voxel by voxel: `set` where bit `z` of word `y * 32 + x` is 1.
fn expand_occupancy(occ: &Occupancy, set: BlockState, clear: BlockState) -> Vec<BlockState> {
    (0..CHUNK_VOLUME)
        .map(|i| {
            let p = LocalPos::from_index(i);
            let word = occ[(p.y() * 32 + p.x()) as usize];
            if word >> p.z() & 1 == 1 { set } else { clear }
        })
        .collect()
}

#[test]
fn from_occupancy_equals_from_dense() {
    let r = boxel_demo::blocks::registry();
    let stone = r.by_name("boxel:stone").unwrap();
    let dirt = r.by_name("boxel:dirt").unwrap();
    let mut rng = Rng(91);
    let mut masks: Vec<Box<Occupancy>> = vec![
        Box::new([0; 1024]),
        Box::new([u32::MAX; 1024]),
        // One voxel set, and one voxel clear: at voxel 0 (the palette order flips) and last.
        Box::new(std::array::from_fn(|i| u32::from(i == 0))),
        Box::new(std::array::from_fn(|i| if i == 1023 { 1 << 31 } else { 0 })),
        Box::new(std::array::from_fn(|i| if i == 0 { !1 } else { u32::MAX })),
        Box::new(std::array::from_fn(|i| {
            if i == 1023 { !(1 << 31) } else { u32::MAX }
        })),
    ];
    for density in [1, 8, 32, 64] {
        masks.push(Box::new(std::array::from_fn(|_| {
            (0..32).fold(0, |w, z| w | (u32::from(rng.below(64) < density) << z))
        })));
    }
    for occ in &masks {
        for (set, clear) in [
            (stone, BlockState::AIR),
            (BlockState::AIR, dirt),
            (stone, stone),
        ] {
            let dense = expand_occupancy(occ, set, clear);
            let from_mask = ChunkData::from_occupancy(occ, set, clear);
            assert_eq!(
                from_mask,
                ChunkData::from_dense(&dense),
                "{set:?} {clear:?}"
            );
            check_invariants(from_mask.blocks());
            check_minimal(from_mask.blocks());
            if set == stone && clear == BlockState::AIR {
                // A stone/air chunk's opaque occupancy is the mask itself.
                assert_eq!(from_mask.opaque_occupancy(&r)[..], occ[..]);
            }
        }
    }
}

#[test]
fn heap_bytes_track_the_representation() {
    assert_eq!(Paletted::Uniform(BlockState(1)).heap_bytes(), 0);
    assert_eq!(ChunkData::default().heap_bytes(), 0);

    let mut p = Paletted::Uniform(BlockState::AIR);
    p.set(LocalPos::new(0, 0, 0), BlockState(1));
    // 1 bit per voxel (4 KiB) plus the palette allocation.
    let palette_bytes = |p: &Paletted| match p {
        Paletted::Packed { palette, .. } => palette.capacity() * 2,
        _ => 0,
    };
    assert_eq!(p.heap_bytes(), CHUNK_VOLUME / 8 + palette_bytes(&p));

    let direct = Paletted::encode(&random_dense(1, 300, 1000, 3));
    assert_eq!(direct.heap_bytes(), CHUNK_VOLUME * 2);

    let mut c = ChunkData::from_dense(&random_dense(2, 9, 100, 5));
    let blocks = c.blocks().heap_bytes();
    assert_eq!(blocks, CHUNK_VOLUME / 2 + palette_bytes(c.blocks()));
    assert_eq!(c.heap_bytes(), blocks);
    // Built masks count too; a change of the blocks drops them.
    let tables = LightTables::from_registry(&boxel_demo::blocks::registry());
    c.light_masks(&tables);
    assert!(c.has_cached_light_masks());
    let masks = OwnedLightMasks::compute(&c, &tables).heap_bytes();
    assert!(masks >= 4096);
    assert_eq!(c.heap_bytes(), blocks + masks);
    c.set(LocalPos::new(0, 0, 0), BlockState(77));
    assert!(!c.has_cached_light_masks());
}
