//! Light fields of the registry, `LightTables`, and the chunk mask cache: masks equal a brute
//! force in every storage representation, the cache resets on every change, and `may_emit`
//! never misses an emitter.
use boxel_voxel::*;
use proptest::prelude::*;
use std::sync::Arc;

/// Small deterministic generator (xorshift64*).
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

/// Number of states of [`light_registry`] (more than 256, so `Direct` chunks are possible).
const STATES: u16 = 320;

/// A registry whose states cycle through every light class: opacity 0, 1, 7 and 15, solid or
/// not, and every 5th state emits a color of its own.
fn light_registry() -> BlockRegistry {
    let mut b = BlockRegistryBuilder::new();
    let t = b
        .add_texture(TextureSpec {
            name: "test:t".into(),
            base_color: [1, 2, 3],
            noise: 0,
            pattern: TexturePattern::Noise,
        })
        .unwrap();
    for i in 1..STATES {
        let opacity = [0, 1, 7, 15][usize::from(i % 4)];
        let shape = match (opacity, i % 3) {
            (15, 0) => RenderShape::Opaque,
            (_, 1) => RenderShape::Cutout,
            (_, 2) => RenderShape::Translucent,
            _ => RenderShape::None,
        };
        let emission = if i % 5 == 0 {
            [(i % 16) as u8, (i / 16 % 16) as u8, 15]
        } else {
            [0; 3]
        };
        b.add_block(BlockDef {
            name: format!("test:b{i}"),
            shape,
            solid: i % 2 == 0,
            emission,
            light_opacity: opacity,
            textures: FaceTextures::all(t),
        })
        .unwrap();
    }
    b.build()
}

/// The masks voxel by voxel, from the public table lookups.
fn brute_force(chunk: &ChunkData, tables: &LightTables) -> ChunkMasks {
    let mut opaque = [0u32; 1024];
    let mut partial = [0u32; 1024];
    let mut solid = [0u32; 1024];
    let mut emitters = Vec::new();
    for i in 0..CHUNK_VOLUME {
        let p = LocalPos::from_index(i);
        let s = chunk.get(p);
        let (word, bit) = (i >> 5, 1u32 << (i & 31));
        match tables.opacity(s) {
            0 => {}
            15 => opaque[word] |= bit,
            _ => partial[word] |= bit,
        }
        if tables.is_solid(s) {
            solid[word] |= bit;
        }
        if !tables.emission(s).is_dark() {
            emitters.push((p, tables.emission(s)));
        }
    }
    // `ChunkMasks` has a private field; compare through the public ones.
    let mut out = ChunkMasks::compute(&ChunkData::uniform(BlockState::AIR), tables);
    out.opaque = opaque;
    out.partial = partial;
    out.solid = solid;
    out.emitters = emitters;
    *out
}

fn assert_masks_eq(got: &ChunkMasks, want: &ChunkMasks, what: &str) {
    assert!(got.opaque == want.opaque, "{what}: opaque");
    assert!(got.partial == want.partial, "{what}: partial");
    assert!(got.solid == want.solid, "{what}: solid");
    assert_eq!(got.emitters, want.emitters, "{what}: emitters");
}

/// A dense chunk with `k` distinct states below `STATES` in runs of up to `run`.
fn random_dense(rng: &mut Rng, k: usize, run: usize) -> Vec<BlockState> {
    let mut ids: Vec<BlockState> = Vec::with_capacity(k);
    while ids.len() < k {
        let s = BlockState(rng.below(u64::from(STATES)) as u16);
        if !ids.contains(&s) {
            ids.push(s);
        }
    }
    let mut out = Vec::with_capacity(CHUNK_VOLUME);
    out.extend(ids.iter().copied());
    while out.len() < CHUNK_VOLUME {
        let s = ids[rng.below(k as u64) as usize];
        let len = 1 + rng.below(run as u64) as usize;
        for _ in 0..len.min(CHUNK_VOLUME - out.len()) {
            out.push(s);
        }
    }
    for i in 0..k {
        let j = rng.below(CHUNK_VOLUME as u64) as usize;
        out.swap(i, j);
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Every representation: Uniform (k = 1), Packed 1, 2, 4, 8 bits and Direct (k > 256),
    /// encoded (first-seen palette) and then edited in place (palettes with unused entries
    /// and indices out of first-seen order).
    #[test]
    fn masks_equal_brute_force(seed in any::<u64>(), k in 1usize..=300, run in 1usize..80,
                               edits in 0usize..40) {
        let registry = light_registry();
        let tables = LightTables::from_registry(&registry);
        let mut rng = Rng(seed | 1);
        let dense = random_dense(&mut rng, k, run);
        let mut chunk = ChunkData::from_dense(&dense);
        for _ in 0..edits {
            let p = LocalPos::from_index(rng.below(CHUNK_VOLUME as u64) as usize);
            chunk.set(p, BlockState(rng.below(u64::from(STATES)) as u16));
        }
        let want = brute_force(&chunk, &tables);
        assert_masks_eq(&ChunkMasks::compute(&chunk, &tables), &want, "compute");
        assert_masks_eq(chunk.masks(&tables), &want, "cached");
        // `may_emit` never misses an emitter.
        if !want.emitters.is_empty() {
            prop_assert!(chunk.may_emit(&tables));
        }
    }
}

#[test]
fn masks_of_every_representation() {
    let registry = light_registry();
    let tables = LightTables::from_registry(&registry);
    let mut rng = Rng(5);
    let mut chunks = Vec::new();
    for s in [0, 1, 2, 3, 4, 5, 15, STATES - 1, STATES + 7] {
        chunks.push(ChunkData::uniform(BlockState(s)));
    }
    for (k, run) in [
        (2, 7),
        (2, 1),
        (3, 3),
        (4, 20),
        (16, 5),
        (17, 2),
        (256, 9),
        (300, 4),
    ] {
        chunks.push(ChunkData::from_dense(&random_dense(&mut rng, k, run)));
    }
    // 1-bit chunks in both palette orders: voxel 0 set first, and set later.
    let mask: Box<Occupancy> = Box::new(std::array::from_fn(|i| {
        (i as u32).wrapping_mul(0x9E37_79B9)
    }));
    for (set, clear) in [
        (BlockState(3), BlockState(0)),
        (BlockState(0), BlockState(5)),
        (BlockState(10), BlockState(9)),
    ] {
        chunks.push(ChunkData::from_occupancy(&mask, set, clear));
    }
    let mut grown = ChunkData::uniform(BlockState(4));
    grown.set(LocalPos::from_index(0), BlockState(15));
    chunks.push(grown);
    let mut seen = std::collections::BTreeSet::new();
    for (n, chunk) in chunks.iter().enumerate() {
        seen.insert(chunk.blocks().bits_per_voxel());
        let want = brute_force(chunk, &tables);
        assert_masks_eq(
            &ChunkMasks::compute(chunk, &tables),
            &want,
            &format!("chunk {n}"),
        );
        assert_masks_eq(chunk.masks(&tables), &want, &format!("chunk {n} cached"));
        assert!(
            chunk.may_emit(&tables) || want.emitters.is_empty(),
            "chunk {n}"
        );
    }
    assert_eq!(seen.into_iter().collect::<Vec<_>>(), [0, 1, 2, 4, 8, 16]);
}

#[test]
fn uniform_chunks_share_static_masks() {
    let registry = BlockRegistry::default_blocks();
    let tables = LightTables::from_registry(&registry);
    let stone = registry.by_name("boxel:stone").unwrap();
    let dirt = registry.by_name("boxel:dirt").unwrap();
    let (a, b) = (ChunkData::uniform(stone), ChunkData::uniform(dirt));
    assert!(std::ptr::eq(a.masks(&tables), b.masks(&tables)));
    assert!(!a.has_cached_masks());
    assert!(std::ptr::eq(
        ChunkData::default().masks(&tables),
        ChunkMasks::empty()
    ));
    assert!(a.masks(&tables).opaque.iter().all(|&w| w == u32::MAX));
    assert!(a.masks(&tables).solid.iter().all(|&w| w == u32::MAX));
    let leaves = ChunkData::uniform(registry.by_name("boxel:leaves").unwrap());
    assert!(leaves.masks(&tables).partial.iter().all(|&w| w == u32::MAX));
    let water = ChunkData::uniform(registry.by_name("boxel:water").unwrap());
    assert!(water.masks(&tables).solid.iter().all(|&w| w == 0));
    // An emitting uniform chunk lists every voxel and is cached.
    let lit = light_registry();
    let lit_tables = LightTables::from_registry(&lit);
    let lamp = ChunkData::uniform(BlockState(5));
    assert_eq!(lamp.masks(&lit_tables).emitters.len(), CHUNK_VOLUME);
    assert!(lamp.has_cached_masks());
}

#[test]
fn cache_resets_after_set_and_make_mut() {
    let registry = BlockRegistry::default_blocks();
    let tables = LightTables::from_registry(&registry);
    let stone = registry.by_name("boxel:stone").unwrap();
    let leaves = registry.by_name("boxel:leaves").unwrap();
    let p = LocalPos::new(3, 4, 5);
    let bit = |m: &Occupancy| m[4 * 32 + 3] >> 5 & 1;

    let mut chunk = ChunkData::uniform(BlockState::AIR);
    chunk.set(LocalPos::new(0, 0, 0), stone);
    let first: *const ChunkMasks = chunk.masks(&tables);
    assert!(chunk.has_cached_masks());
    assert!(std::ptr::eq(first, chunk.masks(&tables)), "cached");
    assert_eq!(bit(&chunk.masks(&tables).opaque), 0);

    // No change, no reset.
    chunk.set(LocalPos::new(0, 0, 0), stone);
    assert!(chunk.has_cached_masks());
    // A change resets, and the new masks see it.
    chunk.set(p, stone);
    assert!(!chunk.has_cached_masks());
    assert_eq!(bit(&chunk.masks(&tables).opaque), 1);

    // Copy-on-write: the copy starts without masks, the shared snapshot keeps its own.
    let shared = Arc::new(chunk);
    let mut edited = Arc::clone(&shared);
    assert!(shared.has_cached_masks());
    Arc::make_mut(&mut edited).set(p, leaves);
    assert!(shared.has_cached_masks());
    assert!(!edited.has_cached_masks());
    assert_eq!(bit(&edited.masks(&tables).opaque), 0);
    assert_eq!(bit(&edited.masks(&tables).partial), 1);
    assert_eq!(bit(&shared.masks(&tables).opaque), 1);

    // A unique Arc is not copied: the masks stay until the edit.
    let mut unique = Arc::new(ChunkData::from_dense(&vec![stone; CHUNK_VOLUME]));
    Arc::make_mut(&mut unique).set(p, BlockState::AIR);
    unique.masks(&tables);
    let chunk = Arc::make_mut(&mut unique);
    assert!(chunk.has_cached_masks());
    chunk.set(p, stone);
    assert!(!chunk.has_cached_masks());

    // `blocks_mut` resets, `compact` keeps; clones start empty and equality ignores the cache.
    chunk.set(p, BlockState::AIR);
    chunk.masks(&tables);
    chunk.compact();
    assert!(chunk.has_cached_masks());
    let copy = chunk.clone();
    assert!(!copy.has_cached_masks());
    assert_eq!(&copy, &*chunk);
    chunk.blocks_mut().set(p, leaves);
    assert!(!chunk.has_cached_masks());
    assert_eq!(bit(&chunk.masks(&tables).partial), 1);
}

#[test]
fn may_emit_is_a_palette_scan() {
    let registry = light_registry();
    let tables = LightTables::from_registry(&registry);
    let (lamp, plain) = (BlockState(5), BlockState(4));
    assert!(tables.emits(lamp) && !tables.emits(plain));
    assert!(ChunkData::uniform(lamp).may_emit(&tables));
    assert!(!ChunkData::uniform(plain).may_emit(&tables));
    let mut c = ChunkData::uniform(plain);
    assert!(!c.may_emit(&tables));
    c.set(LocalPos::new(1, 2, 3), lamp);
    assert!(c.may_emit(&tables));
    // The lamp is gone but stays in the palette: a yes that is allowed to be wrong.
    c.set(LocalPos::new(1, 2, 3), plain);
    assert!(c.may_emit(&tables));
    assert!(c.masks(&tables).emitters.is_empty());
    // Without any emitter in the registry, never.
    let default = LightTables::from_registry(&BlockRegistry::default_blocks());
    assert!(!ChunkData::uniform(BlockState(1)).may_emit(&default));
}

#[test]
fn registry_light_fields_and_validation() {
    let r = BlockRegistry::default_blocks();
    for name in [
        "stone", "dirt", "grass", "sand", "gravel", "snow", "bedrock", "log", "planks",
    ] {
        let s = r.by_name(&format!("boxel:{name}")).unwrap();
        assert_eq!(r.light_opacity(s), 15, "{name}");
        assert!(r.emission(s).is_dark(), "{name}");
    }
    for name in ["leaves", "water"] {
        assert_eq!(
            r.light_opacity(r.by_name(&format!("boxel:{name}")).unwrap()),
            1,
            "{name}"
        );
    }
    assert_eq!(r.light_opacity(BlockState::AIR), 0);
    assert_eq!(r.light_opacity(BlockState(999)), 0);
    assert!(!r.flags(BlockState(1)).contains(StateFlags::EMISSIVE));
    let tables = LightTables::from_registry(&r);
    assert_eq!(tables.max_partial_opacity(), 1);
    assert!(!tables.any_emitter());
    assert!(tables.blocks_sky(r.by_name("boxel:leaves").unwrap()));
    assert!(!tables.blocks_sky(BlockState::AIR));
    assert!(tables.is_solid(r.by_name("boxel:leaves").unwrap()));
    assert!(!tables.is_solid(r.by_name("boxel:water").unwrap()));
    assert_eq!(
        tables.id(),
        LightTables::from_registry(&BlockRegistry::default_blocks()).id()
    );

    // An emitter appended after the built-in content.
    let mut b = BlockRegistryBuilder::with_default_blocks();
    let tex = TextureId(0);
    let def = |name: &str, shape, emission, light_opacity| BlockDef {
        name: format!("test:{name}"),
        shape,
        solid: true,
        emission,
        light_opacity,
        textures: FaceTextures::all(tex),
    };
    let lamp = b
        .add_block(def("lamp", RenderShape::Opaque, [14, 11, 6], 15))
        .unwrap();
    assert_eq!(lamp, BlockState(12));
    // Cutout may stop light; partial opacities and full channels are fine.
    b.add_block(def("dense_leaves", RenderShape::Cutout, [0; 3], 15))
        .unwrap();
    b.add_block(def("fog", RenderShape::None, [15, 15, 15], 9))
        .unwrap();
    for bad in [
        def("hot", RenderShape::Opaque, [16, 0, 0], 15),
        def("thick", RenderShape::None, [0; 3], 16),
        def("leaky", RenderShape::Opaque, [0; 3], 14),
    ] {
        let name = bad.name.clone();
        assert_eq!(b.add_block(bad), Err(RegistryError::InvalidLight(name)));
    }
    let r = b.build();
    assert_eq!(r.emission(lamp), LightValue::from_rgb([14, 11, 6]));
    assert!(r.flags(lamp).contains(StateFlags::EMISSIVE));
    let tables = LightTables::from_registry(&r);
    assert!(tables.any_emitter() && tables.emits(lamp));
    assert!(tables.is_opaque(lamp));
    assert_eq!(tables.max_partial_opacity(), 9);
    assert_ne!(
        tables.id(),
        LightTables::from_registry(&BlockRegistry::default_blocks()).id()
    );
}
