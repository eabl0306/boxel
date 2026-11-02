//! Sky sources in the chunk light masks (ADR 0061), on the `test:` blocks plus
//! `test:sky_source`: a 1-bit chunk with a source builds its masks while a plain cave chunk does
//! not, `may_source` is a palette scan that never says no wrongly, and the cached list follows
//! every change of the blocks.
use boxel_voxel::testing;
use boxel_voxel::*;
use std::sync::Arc;

/// The test registry with `test:sky_source`, its tables, `test:solid` and the default source
/// (`dir=down,range=16`).
fn setup() -> (BlockRegistry, LightTables, BlockState, BlockState) {
    let mut b = testing::builder();
    let source = testing::add_sky_source(&mut b).expect("test:sky_source");
    let r = b.build();
    let tables = LightTables::from_registry(&r);
    let solid = r.by_name("test:solid").expect("test:solid");
    (r, tables, solid, source)
}

/// A 1-bit cave mask: stone where set, about half the chunk.
fn cave_mask() -> Box<Occupancy> {
    Box::new(std::array::from_fn(|i| {
        (i as u32).wrapping_mul(0x9E37_79B9) | 1
    }))
}

fn sources(chunk: &ChunkData, tables: &LightTables) -> Vec<(LocalPos, SkySourceDef)> {
    chunk.sky_sources(tables).to_vec()
}

#[test]
fn a_one_bit_chunk_with_a_source_builds_its_masks() {
    let (r, tables, solid, source) = setup();
    let def = r.sky_source(source).expect("a source");
    assert_eq!(
        def,
        SkySourceDef {
            dir: BeamDir::Down,
            range: 16
        }
    );
    // A cave of stone and air: served from its words, even with sources in the registry.
    let mask = cave_mask();
    let cave = ChunkData::from_occupancy(&mask, solid, BlockState::AIR);
    assert_eq!(cave.blocks().bits_per_voxel(), 1);
    assert!(cave.sky_sources(&tables).is_empty());
    cave.light_masks(&tables);
    assert!(!cave.has_cached_light_masks());
    assert!(!cave.may_source(&tables));

    // Sources and air, both palette orders: 1 bit, built once, every source listed in order.
    for (set, clear) in [(source, BlockState::AIR), (BlockState::AIR, source)] {
        let chunk = ChunkData::from_occupancy(&mask, set, clear);
        assert_eq!(chunk.blocks().bits_per_voxel(), 1);
        let want: Vec<(LocalPos, SkySourceDef)> = (0..CHUNK_VOLUME)
            .map(LocalPos::from_index)
            .filter(|&p| chunk.get(p) == source)
            .map(|p| (p, def))
            .collect();
        assert!(!want.is_empty());
        let m = chunk.light_masks(&tables);
        assert!(chunk.has_cached_light_masks());
        assert_eq!(chunk.sky_sources(&tables), &want[..]);
        // A source is clear: in no mask.
        assert_eq!(m.opaque.count(), 0);
        assert!(m.partial.is_empty() && m.filter.is_empty());
        assert!(m.emitters.is_empty());
        assert!(chunk.has_cached_light_masks());
        assert!(chunk.may_source(&tables));
    }

    // A cave that gets one source becomes 2-bit and builds its masks.
    let mut lit = cave.clone();
    let p = LocalPos::new(4, 20, 9);
    lit.set(p, source);
    assert_eq!(lit.blocks().bits_per_voxel(), 2);
    assert_eq!(sources(&lit, &tables), [(p, def)]);
    assert!(lit.has_cached_light_masks());

    // A chunk all of sources lists every voxel.
    let all = ChunkData::uniform(source);
    assert_eq!(all.sky_sources(&tables).len(), CHUNK_VOLUME);
    assert!(all.has_cached_light_masks());
    let heap = OwnedLightMasks::compute(&all, &tables).heap_bytes();
    assert!(heap >= CHUNK_VOLUME * size_of::<(LocalPos, SkySourceDef)>());
}

#[test]
fn may_source_never_says_no_wrongly() {
    let (r, tables, solid, source) = setup();
    let up = r.with(source, "dir", "up").expect("up");
    assert!(ChunkData::uniform(source).may_source(&tables));
    assert!(!ChunkData::uniform(solid).may_source(&tables));
    let mut c = ChunkData::uniform(solid);
    c.set(LocalPos::new(1, 2, 3), up);
    assert!(c.may_source(&tables));
    // The source is gone but stays in the palette: a yes that is allowed to be wrong.
    c.set(LocalPos::new(1, 2, 3), solid);
    assert!(c.may_source(&tables));
    assert!(sources(&c, &tables).is_empty());
    // `Direct` storage (more than 256 states in use) answers yes.
    let many: Vec<BlockState> = (0..CHUNK_VOLUME)
        .map(|i| BlockState((i % 300) as u16))
        .collect();
    let direct = ChunkData::from_dense(&many);
    assert_eq!(direct.blocks().bits_per_voxel(), 16);
    assert!(direct.may_source(&tables));
    // Without any source in the registry, never, and no list is built.
    let plain = LightTables::from_registry(&testing::registry());
    assert!(!plain.any_sky_source());
    assert!(!direct.may_source(&plain));
    assert!(!ChunkData::uniform(source).may_source(&plain));
    assert!(
        OwnedLightMasks::compute(&direct, &plain)
            .sky_sources()
            .is_empty()
    );
    assert!(direct.sky_sources(&plain).is_empty());
    assert!(!direct.has_cached_light_masks());
    // Sources move no other light datum of the test blocks.
    for s in (0..testing::registry().len() as u16).map(BlockState) {
        assert_eq!(tables.opacity(s), plain.opacity(s));
        assert_eq!(tables.emission(s), plain.emission(s));
        assert_eq!(tables.light_filter(s), plain.light_filter(s));
    }
}

#[test]
fn the_source_list_resets_after_set() {
    let (r, tables, solid, source) = setup();
    let def = r.sky_source(source).expect("a source");
    let up = r.with(source, "range", "256").expect("range 256");
    let up = r.with(up, "dir", "up").expect("up");
    let up_def = SkySourceDef {
        dir: BeamDir::Up,
        range: MAX_BEAM,
    };
    assert_eq!(r.sky_source(up), Some(up_def));
    let (a, b) = (LocalPos::new(0, 31, 0), LocalPos::new(31, 0, 31));

    let mut chunk = ChunkData::from_occupancy(&cave_mask(), solid, BlockState::AIR);
    chunk.set(a, source);
    assert_eq!(sources(&chunk, &tables), [(a, def)]);
    assert!(chunk.has_cached_light_masks());
    // No change, no reset.
    chunk.set(a, source);
    assert!(chunk.has_cached_light_masks());
    // A second source, a changed one, and a removed one: each resets, and the new list sees it.
    chunk.set(b, up);
    assert!(!chunk.has_cached_light_masks());
    assert_eq!(sources(&chunk, &tables), [(b, up_def), (a, def)]);
    chunk.set(a, up);
    assert_eq!(sources(&chunk, &tables), [(b, up_def), (a, up_def)]);
    chunk.set(b, BlockState::AIR);
    assert_eq!(sources(&chunk, &tables), [(a, up_def)]);

    // Copy-on-write: the copy starts empty, the shared snapshot keeps its own list.
    let shared = Arc::new(chunk);
    let mut edited = Arc::clone(&shared);
    Arc::make_mut(&mut edited).set(a, solid);
    assert!(sources(&edited, &tables).is_empty());
    assert_eq!(sources(&shared, &tables), [(a, up_def)]);

    // `blocks_mut` resets too.
    let mut chunk = Arc::unwrap_or_clone(shared);
    assert_eq!(sources(&chunk, &tables).len(), 1);
    chunk.blocks_mut().set(b, source);
    assert!(!chunk.has_cached_light_masks());
    assert_eq!(sources(&chunk, &tables), [(b, def), (a, up_def)]);
}
