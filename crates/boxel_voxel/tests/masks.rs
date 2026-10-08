//! Light fields of the registry, `LightTables`, and the chunk light masks: masks equal a brute
//! force in every storage representation, uniform and 1-bit chunks without emission are served
//! from their storage (nothing cached), the cache resets on every change, and `may_emit` never
//! misses an emitter.
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
/// not (no effect on the light masks), and every 5th state emits a color of its own.
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
            model: BlockModel::CUBE,
            collision: if i % 2 == 0 {
                BlockCollision::Full
            } else {
                BlockCollision::None
            },
            emission,
            light_opacity: opacity,
            textures: FaceTextures::all(t),
        })
        .unwrap();
    }
    b.build()
}

/// Light masks as plain data: opaque, partial, emitters.
#[derive(Debug, PartialEq, Eq)]
struct Plain {
    opaque: Box<Occupancy>,
    partial: Box<Occupancy>,
    emitters: Vec<(LocalPos, LightValue)>,
}

impl Plain {
    fn of(m: LightMasks<'_>) -> Self {
        Self {
            opaque: m.opaque.to_occupancy(),
            partial: m.partial.to_occupancy(),
            emitters: m.emitters.to_vec(),
        }
    }
}

/// The masks voxel by voxel, from the public table lookups.
fn brute_force(chunk: &ChunkData, tables: &LightTables) -> Plain {
    let mut opaque = Box::new([0u32; 1024]);
    let mut partial = Box::new([0u32; 1024]);
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
        if !tables.emission(s).is_dark() {
            emitters.push((p, tables.emission(s)));
        }
    }
    Plain {
        opaque,
        partial,
        emitters,
    }
}

fn assert_masks_eq(got: LightMasks<'_>, want: &Plain, what: &str) {
    let got = Plain::of(got);
    assert!(got.opaque == want.opaque, "{what}: opaque");
    assert!(got.partial == want.partial, "{what}: partial");
    assert_eq!(got.emitters, want.emitters, "{what}: emitters");
}

/// True if `chunk` is served from its storage: uniform or 1-bit, with no emitting palette entry
/// (a palette scan, as `may_emit`).
fn served_from_storage(chunk: &ChunkData, tables: &LightTables) -> bool {
    chunk.blocks().bits_per_voxel() <= 1 && !chunk.may_emit(tables)
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
        assert_masks_eq(OwnedLightMasks::compute(&chunk, &tables).view(), &want, "compute");
        assert_masks_eq(chunk.light_masks(&tables), &want, "chunk");
        prop_assert_eq!(chunk.has_cached_light_masks(), !served_from_storage(&chunk, &tables));
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
            OwnedLightMasks::compute(chunk, &tables).view(),
            &want,
            &format!("chunk {n}"),
        );
        let heap = chunk.heap_bytes();
        assert_masks_eq(
            chunk.light_masks(&tables),
            &want,
            &format!("chunk {n} view"),
        );
        let borrowed = served_from_storage(chunk, &tables);
        assert_eq!(chunk.has_cached_light_masks(), !borrowed, "chunk {n}");
        assert_eq!(chunk.heap_bytes() == heap, borrowed, "chunk {n}");
        assert!(
            chunk.may_emit(&tables) || want.emitters.is_empty(),
            "chunk {n}"
        );
    }
    assert_eq!(seen.into_iter().collect::<Vec<_>>(), [0, 1, 2, 4, 8, 16]);
}

#[test]
fn uniform_and_one_bit_chunks_need_no_build() {
    let registry = BlockRegistry::default_blocks();
    let tables = LightTables::from_registry(&registry);
    let stone = registry.by_name("boxel:stone").unwrap();
    let full = |m: MaskRows<'_>| m.count() == CHUNK_VOLUME as u32;
    let a = ChunkData::uniform(stone);
    assert!(full(a.light_masks(&tables).opaque));
    assert_eq!(a.light_masks(&tables).partial.count(), 0);
    assert!(!a.has_cached_light_masks());
    assert_eq!(ChunkData::default().light_masks(&tables).opaque.count(), 0);
    assert_eq!(LightMasks::EMPTY.opaque.count(), 0);
    let leaves = ChunkData::uniform(registry.by_name("boxel:leaves").unwrap());
    assert!(full(leaves.light_masks(&tables).partial));
    assert_eq!(leaves.light_masks(&tables).opaque.count(), 0);
    // A 1-bit cave chunk (stone and air, both palette orders) reads its packed words.
    let mask: Box<Occupancy> = Box::new(std::array::from_fn(|i| {
        (i as u32).wrapping_mul(0x9E37_79B9) | 1
    }));
    for (set, clear) in [(stone, BlockState::AIR), (BlockState::AIR, stone)] {
        let cave = ChunkData::from_occupancy(&mask, set, clear);
        assert_eq!(cave.blocks().bits_per_voxel(), 1);
        let heap = cave.heap_bytes();
        let m = cave.light_masks(&tables);
        let want = if set == stone {
            mask.clone()
        } else {
            Box::new(mask.map(|w| !w))
        };
        assert!(m.opaque.to_occupancy() == want);
        assert_eq!(m.partial.count(), 0);
        assert!(!cave.has_cached_light_masks());
        assert_eq!(cave.heap_bytes(), heap);
    }
    // An emitting uniform chunk lists every voxel and is cached.
    let lit = light_registry();
    let lit_tables = LightTables::from_registry(&lit);
    let lamp = ChunkData::uniform(BlockState(5));
    assert_eq!(lamp.light_masks(&lit_tables).emitters.len(), CHUNK_VOLUME);
    assert!(lamp.has_cached_light_masks());
}

#[test]
fn cache_resets_after_set_and_make_mut() {
    let registry = BlockRegistry::default_blocks();
    let tables = LightTables::from_registry(&registry);
    let stone = registry.by_name("boxel:stone").unwrap();
    let dirt = registry.by_name("boxel:dirt").unwrap();
    let leaves = registry.by_name("boxel:leaves").unwrap();
    let p = LocalPos::new(3, 4, 5);
    let bit = |m: MaskRows<'_>| m.row(4 * 32 + 3) >> 5 & 1;

    // Three states (2 bits per voxel): the masks are built and cached.
    let mut chunk = ChunkData::uniform(BlockState::AIR);
    chunk.set(LocalPos::new(0, 0, 0), stone);
    chunk.set(LocalPos::new(1, 0, 0), dirt);
    assert_eq!(chunk.blocks().bits_per_voxel(), 2);
    let heap = chunk.heap_bytes();
    assert_eq!(bit(chunk.light_masks(&tables).opaque), 0);
    assert!(chunk.has_cached_light_masks());
    assert!(chunk.heap_bytes() > heap);

    // No change, no reset.
    chunk.set(LocalPos::new(0, 0, 0), stone);
    assert!(chunk.has_cached_light_masks());
    // A change resets, and the new masks see it.
    chunk.set(p, stone);
    assert!(!chunk.has_cached_light_masks());
    assert_eq!(chunk.heap_bytes(), heap);
    assert_eq!(bit(chunk.light_masks(&tables).opaque), 1);

    // Copy-on-write: the copy starts without masks, the shared snapshot keeps its own.
    let shared = Arc::new(chunk);
    let mut edited = Arc::clone(&shared);
    assert!(shared.has_cached_light_masks());
    Arc::make_mut(&mut edited).set(p, leaves);
    assert!(shared.has_cached_light_masks());
    assert!(!edited.has_cached_light_masks());
    assert_eq!(bit(edited.light_masks(&tables).opaque), 0);
    assert_eq!(bit(edited.light_masks(&tables).partial), 1);
    assert_eq!(bit(shared.light_masks(&tables).opaque), 1);

    // A unique Arc is not copied: the masks stay until the edit.
    let mut unique = Arc::new(ChunkData::from_dense(&vec![stone; CHUNK_VOLUME]));
    Arc::make_mut(&mut unique).set(p, BlockState::AIR);
    Arc::make_mut(&mut unique).set(LocalPos::new(0, 0, 0), dirt);
    unique.light_masks(&tables);
    let chunk = Arc::make_mut(&mut unique);
    assert!(chunk.has_cached_light_masks());
    chunk.set(p, stone);
    assert!(!chunk.has_cached_light_masks());

    // `blocks_mut` resets, `compact` keeps; clones start empty and equality ignores the cache.
    chunk.set(p, BlockState::AIR);
    chunk.light_masks(&tables);
    chunk.compact();
    assert!(chunk.has_cached_light_masks());
    let copy = chunk.clone();
    assert!(!copy.has_cached_light_masks());
    assert_eq!(&copy, &*chunk);
    chunk.blocks_mut().set(p, leaves);
    assert!(!chunk.has_cached_light_masks());
    assert_eq!(bit(chunk.light_masks(&tables).partial), 1);
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
    assert!(c.light_masks(&tables).emitters.is_empty());
    // Without any emitter in the registry, never.
    let mut plain_registry = BlockRegistryBuilder::new();
    let texture = plain_registry
        .add_texture(TextureSpec {
            name: "test:stone".to_string(),
            base_color: [128; 3],
            noise: 0,
            pattern: TexturePattern::Noise,
        })
        .unwrap();
    plain_registry
        .add_block(BlockDef {
            name: "test:stone".to_string(),
            shape: RenderShape::Opaque,
            model: BlockModel::CUBE,
            collision: BlockCollision::Full,
            emission: [0; 3],
            light_opacity: 15,
            textures: FaceTextures::all(texture),
        })
        .unwrap();
    let none = LightTables::from_registry(&plain_registry.build());
    assert!(!none.any_emitter());
    assert!(!ChunkData::uniform(BlockState(1)).may_emit(&none));
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
    // The built-in emitters: four lamps and the lava placeholder, opaque and solid.
    for (name, emission) in LAMPS
        .iter()
        .map(|lamp| (lamp.name, lamp.emission))
        .chain([("boxel:lava", LAVA_EMISSION)])
    {
        let s = r.by_name(name).unwrap();
        assert_eq!(r.emission(s), LightValue::from_rgb(emission), "{name}");
        assert_eq!(r.light_opacity(s), 15, "{name}");
        assert!(
            r.flags(s)
                .contains(StateFlags::EMISSIVE | StateFlags::SOLID)
        );
        assert!(tables.emits(s) && tables.is_opaque(s), "{name}");
    }
    assert_eq!(
        r.emission(r.by_name("boxel:lamp_orange").unwrap()),
        LightValue::from_rgb([14, 11, 6])
    );
    assert_eq!(LAVA_EMISSION, [15, 7, 2]);
    assert!(tables.any_emitter());
    assert!(tables.blocks_sky(r.by_name("boxel:leaves").unwrap()));
    assert!(!tables.blocks_sky(BlockState::AIR));
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
        model: BlockModel::CUBE,
        collision: BlockCollision::Full,
        emission,
        light_opacity,
        textures: FaceTextures::all(tex),
    };
    let lamp = b
        .add_block(def("lamp", RenderShape::Opaque, [14, 11, 6], 15))
        .unwrap();
    assert_eq!(lamp, BlockState(221));
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
