//! Tests of player edits at a distance and of the fallback (ADR 0031, milestone 2): pyramids
//! against the cell rule, overlays, rebuilds that keep the old mesh drawn, unloads with and
//! without persistence, a restart, and level-1 cells from real chunks for a generator without a
//! coarse source. The world is the toy [`Steps`] terrain of `test:` blocks, whose own source is
//! exact, so a node's expected cells are the source's outside the edited chunks and the cell
//! rule on the edited blocks inside them.
use super::edits::{self, Pyramid};
use super::jobs::{self, Built};
use super::tests::{Model, Steps, app, app_with, fill, frame, small, spawn};
use super::*;
use crate::jobs::InlineExecutor;
use crate::persist::{ChunkStore, MemoryStore, PersistPlugin, PersistSettings};
use crate::{EditQueue, WorldPlugin};
use boxel_math::DVec3;
use boxel_voxel::lod::{CellPartial, LodClasses};
use boxel_voxel::{BlockPos, BlockRegistry, BlockState, ChunkData, LocalPos};
use boxel_worldgen::WorldGenerator;
use boxel_worldgen::lod::{LodCells, LodColumn, LodNode, LodScratch, LodSource};
use foldhash::HashSet;

/// The tower: one column of `test:solid` from the ground (20 at x, z = 40) up to 95, through
/// three chunks and, at level 1, into a layer the source has as air.
const TOWER_X: i64 = 40;
const TOWER_Z: i64 = 40;
const TOWER: std::ops::RangeInclusive<i64> = 20..=95;

fn solid(registry: &BlockRegistry) -> BlockState {
    registry.by_name("test:solid").expect("test:solid")
}

/// The block of the steps world at `p`, with the tower if `tower`.
fn block_at(registry: &BlockRegistry, tower: bool, p: [i64; 3]) -> BlockState {
    let steps = Steps::new(20);
    if tower && p[0] == TOWER_X && p[2] == TOWER_Z && TOWER.contains(&p[1]) {
        return solid(registry);
    }
    if p[1] < steps.height(p[0], p[2]) {
        solid(registry)
    } else {
        BlockState::AIR
    }
}

/// The chunks the tower edits.
fn tower_chunks() -> HashSet<ChunkPos> {
    TOWER
        .map(|y| ChunkPos::containing(BlockPos::new(TOWER_X, y, TOWER_Z)))
        .collect()
}

fn edit_tower(app: &mut App) {
    let registry = Arc::clone(&app.world().resource::<WorldContent>().registry);
    let mut queue = app.world_mut().resource_mut::<EditQueue>();
    for y in TOWER {
        queue.set_block(BlockPos::new(TOWER_X, y, TOWER_Z), solid(&registry));
    }
}

/// What node `key` should be: the steps source's cells, and inside the chunks of `edited` the
/// cell rule on the blocks (`tower`: with the tower), meshed as a job does.
fn expected(key: ChunkKey, edited: &HashSet<ChunkPos>, tower: bool) -> Built {
    let registry = boxel_voxel::testing::registry();
    expected_by(key, edited, &|p| block_at(&registry, tower, p))
}

/// [`expected`] with the edited chunks' blocks from `blocks`.
fn expected_by(
    key: ChunkKey,
    edited: &HashSet<ChunkPos>,
    blocks: &dyn Fn([i64; 3]) -> BlockState,
) -> Built {
    let registry = boxel_voxel::testing::registry();
    let materials = LodMaterials::from_registry(&registry);
    let classes = LodClasses::from_registry(&registry);
    let steps = Steps::new(20);
    let req = LodRequest {
        dim: key.dim,
        level: key.lod,
        column: [key.pos.x, key.pos.z],
        layers: key.pos.y..=key.pos.y,
    };
    let mut scratch = LodScratch::default();
    let mut cells = LodCells::new();
    let mut emitted = false;
    let band = steps.column(&req, &registry, &mut scratch, &mut |_, node| {
        emitted = true;
        match node {
            LodNode::Uniform(s) => cells.fill(s),
            LodNode::Cells(c) => cells.0.copy_from_slice(&c.0[..]),
        }
    });
    if !emitted {
        cells.fill(band.uniform_at(key.pos.y).expect("outside the band"));
    }
    let s = 1i64 << key.lod;
    let o = req.origin(key.pos.y);
    for y in -1..=32i32 {
        for x in -1..=32i32 {
            for z in -1..=32i32 {
                let at = [
                    o[0] + i64::from(x) * s,
                    o[1] + i64::from(y) * s,
                    o[2] + i64::from(z) * s,
                ];
                let chunk = ChunkPos::new(at[0] >> 5, at[1] >> 5, at[2] >> 5);
                if !edited.contains(&chunk) {
                    continue;
                }
                let state = CellPartial::of_cell(&classes, at, s as u32, |bx, by, bz| {
                    blocks([
                        at[0] + i64::from(bx),
                        at[1] + i64::from(by),
                        at[2] + i64::from(bz),
                    ])
                })
                .state();
                cells.set(x, y, z, state);
            }
        }
    }
    if cells.uniform().is_some() {
        return Built::Uniform;
    }
    let mut padded = boxel_mesh::PaddedChunk::default();
    let mut mesher = boxel_mesh::Mesher::default();
    let mut mesh = ChunkMesh::default();
    jobs::mesh_cells(
        &materials,
        key.lod,
        &cells.0[..],
        &mut padded,
        &mut mesher,
        &mut mesh,
    )
}

/// The mesh the tree has for node `key` (`None`: blank, uniform or no record).
fn tree_mesh(app: &App, key: ChunkKey) -> Option<ChunkMesh> {
    let lod = app.world().resource::<LodWorld>();
    match &lod.tree.node(key)?.state {
        tree::NodeState::Ready { mesh, .. } => Some((**mesh).clone()),
        _ => None,
    }
}

/// The mesh of a [`Built`] (`None`: nothing to draw).
fn built_mesh(built: Built) -> Option<ChunkMesh> {
    match built {
        Built::Mesh(mesh, _) => Some((*mesh).clone()),
        Built::Uniform | Built::Blank => None,
        Built::TooBig => panic!("a node too big"),
    }
}

/// The node of level `k` holding block `(x, y, z)`.
fn node_at(k: u8, [x, y, z]: [i64; 3]) -> ChunkKey {
    let shift = 5 + u32::from(k);
    ChunkKey {
        dim: DimensionId(0),
        lod: k,
        pos: ChunkPos::new(x >> shift, y >> shift, z >> shift),
    }
}

// ---------------------------------------------------------------------------------------------
// Pyramids, overlays and the geometry of edits

/// A chunk of every class: ground with a slab, a cutout layer, one fluid over part of it, glass
/// and plants, from a fixed hash.
fn mixed_chunk(registry: &BlockRegistry) -> ChunkData {
    let names = [
        "test:solid",
        "test:slab",
        "test:cutout",
        "test:glass",
        "test:plant",
        "test:fluid",
    ];
    let states: Vec<BlockState> = names
        .iter()
        .map(|n| registry.by_name(n).expect("a test block"))
        .collect();
    let mut data = ChunkData::uniform(BlockState::AIR);
    for y in 0..32u32 {
        for x in 0..32u32 {
            for z in 0..32u32 {
                let h = u64::from(x * 31 + y * 17 + z * 7 + x * y * z);
                let hash = h.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 59;
                let ground = 6 + (x / 4 + z / 8) % 9;
                let state = if y < ground {
                    states[0]
                } else if y < 14 && x < 20 {
                    // A sea over part of it, with things sticking out.
                    if hash < 3 { states[1] } else { states[5] }
                } else {
                    match hash {
                        0 => states[2],
                        1 => states[3],
                        2 => states[4],
                        _ => BlockState::AIR,
                    }
                };
                data.set(LocalPos::new(x, y, z), state);
            }
        }
    }
    data
}

#[test]
fn pyramids_are_the_cell_rule_on_the_chunks_blocks() {
    let registry = boxel_voxel::testing::registry();
    let classes = LodClasses::from_registry(&registry);
    let solid = solid(&registry);
    let fluid = registry.by_name("test:fluid").expect("test:fluid");
    for data in [
        mixed_chunk(&registry),
        ChunkData::uniform(solid),
        ChunkData::uniform(fluid),
        ChunkData::uniform(BlockState::AIR),
    ] {
        let pyramid = Pyramid::of_chunk(&classes, &data);
        for level in 1..=edits::PYRAMID_LEVELS {
            let s = 1u32 << level;
            let n = 32 >> level;
            for y in 0..n {
                for z in 0..n {
                    for x in 0..n {
                        let origin = [i64::from(x * s), i64::from(y * s), i64::from(z * s)];
                        let want = CellPartial::of_cell(&classes, origin, s, |bx, by, bz| {
                            data.get(LocalPos::new(x * s + bx, y * s + by, z * s + bz))
                        })
                        .state();
                        assert_eq!(
                            pyramid.get(level, x as usize, y as usize, z as usize),
                            want,
                            "level {level} cell {x} {y} {z}"
                        );
                    }
                }
            }
        }
        let level1 = edits::Level1::of_chunk(&classes, &data);
        for i in 0..16 {
            assert_eq!(
                level1.get(i, 15 - i, i / 2),
                pyramid.get(1, i, 15 - i, i / 2)
            );
        }
    }
}

#[test]
fn nodes_touching_a_chunk_are_those_whose_padded_cells_reach_it() {
    for level in 1..=edits::PYRAMID_LEVELS {
        let k = u32::from(level);
        for c in [
            -70i64,
            -33,
            -32,
            -31,
            -17,
            -2,
            -1,
            0,
            1,
            2,
            15,
            16,
            31,
            32,
            33,
            64,
            1 << 40,
        ] {
            // Node n's padded cells span chunks (n << k) - 1 ..= (n + 1) << k.
            let brute: Vec<i64> = ((c >> k) - 2..=(c >> k) + 2)
                .filter(|&n| (n << k) - 1 <= c && c <= (n + 1) << k)
                .collect();
            let (lo, hi) = edits::nodes_touching(c, level);
            assert_eq!(
                (lo..=hi).collect::<Vec<_>>(),
                brute,
                "level {level} chunk {c}"
            );
        }
    }
}

#[test]
fn an_overlay_sets_the_cells_of_its_chunk_border_included() {
    let registry = boxel_voxel::testing::registry();
    let classes = LodClasses::from_registry(&registry);
    let stone = Arc::new(Pyramid::of_chunk(
        &classes,
        &ChunkData::uniform(solid(&registry)),
    ));
    for level in 1..=edits::PYRAMID_LEVELS {
        let per = 32i64 >> level;
        // Far from the origin too (ADR 0014): a node and the chunks around its corner.
        for node in [ChunkPos::new(0, 0, 0), ChunkPos::new(-3, 7, 1 << 30)] {
            let first = |n: i64| n << level;
            for d in [-1i64, 0, (1 << level) - 1, 1 << level] {
                let chunk = ChunkPos::new(first(node.x) + d, first(node.y), first(node.z) + d);
                let mut cells = LodCells::new();
                edits::overlay(&mut cells, level, node, &[(chunk, Arc::clone(&stone))]);
                let inside = |c: i32, d: i64| {
                    let c = i64::from(c);
                    d * per <= c && c < (d + 1) * per
                };
                for y in -1..=32i32 {
                    for x in -1..=32i32 {
                        for z in -1..=32i32 {
                            let want = inside(x, d) && inside(y, 0) && inside(z, d);
                            assert_eq!(
                                cells.get(x, y, z) != BlockState::AIR,
                                want,
                                "level {level} node {node:?} offset {d} cell {x} {y} {z}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn edits_widen_a_band_to_their_layers() {
    let stone = BlockState(1);
    let band = LodColumn {
        lo: 0,
        hi: 2,
        below: stone,
    };
    assert_eq!(edits::widen(band, None), band);
    assert_eq!(
        edits::widen(band, Some((5, 6))),
        LodColumn {
            lo: 0,
            hi: 7,
            below: stone
        }
    );
    assert_eq!(
        edits::widen(band, Some((-4, 0))),
        LodColumn {
            lo: -4,
            hi: 2,
            below: stone
        }
    );
    // A column of air takes the layers as they are, wherever its empty band sits.
    assert_eq!(
        edits::widen(LodColumn::AIR, Some((40, 41))),
        LodColumn {
            lo: 40,
            hi: 42,
            below: BlockState::AIR
        }
    );
}

#[test]
fn the_pyramid_cache_drops_the_least_recently_used() {
    let registry = boxel_voxel::testing::registry();
    let classes = LodClasses::from_registry(&registry);
    let p = Arc::new(Pyramid::of_chunk(
        &classes,
        &ChunkData::uniform(BlockState::AIR),
    ));
    // Chunks far apart along x: a level-1 column reaches one of them.
    let key = |i: i64| ChunkKey::new(ChunkPos::new(10 * i, 0, 0));
    let column = |i: i64| ColumnKey {
        level: 1,
        x: 5 * i,
        z: 0,
    };
    let mut e = Edits::new(2);
    for i in 0..3 {
        e.insert(key(i));
        e.ask(key(i), true);
    }
    // One job of the three chunks: the cache keeps the last two.
    let chunks = e.next_job(9).expect("asked");
    assert_eq!(chunks, vec![key(0), key(1), key(2)]);
    let (changed, _) = e.done(9, chunks.iter().map(|&k| (k, Arc::clone(&p))).collect());
    assert_eq!(changed, chunks);
    assert!(!e.has_pyramid(key(0)));
    assert!(e.has_pyramid(key(1)) && e.has_pyramid(key(2)));
    // A column of chunk 1 uses its pyramid, so chunk 3's evicts chunk 2's.
    let dim = DimensionId(0);
    assert!(matches!(
        e.for_column(column(1), (0, 0), dim),
        ForColumn::Ready { ref overlays, layers: Some((-1, 0)) } if overlays.len() == 1
    ));
    e.insert(key(3));
    e.ask(key(3), true);
    let chunks = e.next_job(10).expect("asked");
    e.done(10, chunks.iter().map(|&k| (k, Arc::clone(&p))).collect());
    assert!(e.has_pyramid(key(1)) && e.has_pyramid(key(3)));
    assert!(!e.has_pyramid(key(2)));
    // A column of chunk 0 waits for its pyramid; a job that fails lets it go on without it.
    assert!(matches!(
        e.for_column(column(0), (0, 0), dim),
        ForColumn::Wait
    ));
    assert!(e.busy());
    let chunks = e.next_job(11).expect("asked");
    assert_eq!(chunks, vec![key(0)]);
    let (changed, ready) = e.done(11, Vec::new());
    assert!(changed.is_empty());
    assert_eq!(ready, vec![column(0)]);
    assert!(matches!(
        e.for_column(column(0), (0, 0), dim),
        ForColumn::Ready { ref overlays, .. } if overlays.is_empty()
    ));
    assert!(!e.busy());
}

// ---------------------------------------------------------------------------------------------
// Edits in a running world

/// Small rings with `levels` levels and a short debounce.
fn edit_settings(levels: u8, debounce_ms: u64) -> LodSettings {
    LodSettings {
        levels,
        edit_debounce: Duration::from_millis(debounce_ms),
        ..small()
    }
}

/// Frames until the LOD is filled again (`max` frames at most).
fn refill(app: &mut App, model: &mut Model, max: usize) {
    for _ in 0..max {
        frame(app, model);
        let stats = app.world().resource::<LodStats>();
        if stats.fill >= 1.0 && stats.in_flight == 0 && stats.sends_waiting == 0 {
            return;
        }
    }
    panic!(
        "not filled again after {max} frames: {:?}",
        app.world().resource::<LodStats>()
    );
}

#[test]
fn a_one_block_tower_shows_up_to_level_5() {
    let mut app = app(Steps::new(20), edit_settings(5, 30));
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut model = Model::default();
    fill(&mut app, &mut model, 600);
    let top = [TOWER_X, *TOWER.end(), TOWER_Z];
    let foot = [TOWER_X, *TOWER.start(), TOWER_Z];
    // The nodes drawing the tower's place now, and their meshes.
    let before: Vec<ChunkKey> = (2..=5)
        .map(|k| node_at(k, top))
        .filter(|k| model.nodes.contains_key(k))
        .collect();
    assert!(!before.is_empty());
    edit_tower(&mut app);
    // The old meshes stay on the renderer until the new ones replace them.
    // Frames go on while the debounce waits (no sleep: a few seconds at most).
    let start = Instant::now();
    loop {
        frame(&mut app, &mut model);
        for key in &before {
            assert!(model.nodes.contains_key(key), "{key:?} left the renderer");
        }
        let stats = app.world().resource::<LodStats>();
        if stats.edited_chunks > 0
            && stats.fill >= 1.0
            && stats.in_flight == 0
            && stats.sends_waiting == 0
        {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "not rebuilt: {stats:?}"
        );
    }
    let stats = app.world().resource::<LodStats>().clone();
    assert_eq!(stats.edited_chunks, 3, "{stats:?}");
    assert_eq!(stats.pyramids, 3);
    let edited = tower_chunks();
    for k in 1..=5u8 {
        for at in [top, foot] {
            let key = node_at(k, at);
            let want = built_mesh(expected(key, &edited, true));
            let without = built_mesh(expected(key, &HashSet::default(), false));
            assert!(want.is_some(), "level {k}: the tower draws something");
            assert_ne!(
                want, without,
                "level {k} {key:?}: the tower changes the node"
            );
            assert_eq!(tree_mesh(&app, key), want, "level {k} {key:?}");
            // The renderer has it too.
            let quads = want.as_ref().map(ChunkMesh::total_quads);
            assert_eq!(model.nodes.get(&key).map(|n| n.1), quads, "level {k}");
        }
    }
    // The top of the tower lies in a level-1 layer the source has as air: the band grew.
    let lod = app.world().resource::<LodWorld>();
    let column = lod
        .tree
        .columns
        .get(&ColumnKey::of(node_at(1, top)))
        .expect("the column");
    assert!(column.band.is_some_and(|b| b.hi > node_at(1, top).pos.y));
    tests::assert_bits_from_scratch(&app);
}

/// Moves the viewer to `to`; the world settles and the LOD fills again.
fn move_away(app: &mut App, model: &mut Model, viewer: Entity, to: DVec3) {
    app.world_mut()
        .get_mut::<Transform>(viewer)
        .expect("the viewer")
        .translation = to;
    for _ in 0..600 {
        frame(app, model);
        let stats = *app.world().resource::<WorldStats>();
        let lod = app.world().resource::<LodStats>();
        if stats.is_settled() && lod.fill >= 1.0 && lod.in_flight == 0 && lod.sends_waiting == 0 {
            return;
        }
    }
    panic!("not settled");
}

#[test]
fn without_persistence_an_unloaded_edit_goes_back_to_the_generators_cells() {
    let mut app = app(Steps::new(20), edit_settings(2, 0));
    let viewer = spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut model = Model::default();
    fill(&mut app, &mut model, 600);
    edit_tower(&mut app);
    refill(&mut app, &mut model, 600);
    let edited = tower_chunks();
    let top = node_at(1, [TOWER_X, *TOWER.end(), TOWER_Z]);
    let foot = node_at(1, [TOWER_X, *TOWER.start(), TOWER_Z]);
    assert_eq!(
        tree_mesh(&app, top),
        built_mesh(expected(top, &edited, true))
    );
    assert!(model.nodes.contains_key(&top));
    // Nine chunks away the tower's chunks unload (keep band: 5), its lod-1 nodes stay wanted.
    move_away(&mut app, &mut model, viewer, DVec3::new(298.0, 60.0, 10.0));
    for c in &edited {
        assert!(app.world().resource::<ChunkMap>().id_at(*c).is_none());
    }
    let stats = app.world().resource::<LodStats>().clone();
    assert_eq!(stats.edited_chunks, 0, "{stats:?}");
    let lod = app.world().resource::<LodWorld>();
    assert!(lod.tree.wanted(foot) && lod.tree.wanted(top));
    assert_eq!(
        tree_mesh(&app, foot),
        built_mesh(expected(foot, &HashSet::default(), false))
    );
    // The air layer the tower reached is uniform again: off the renderer, no record.
    assert!(lod.tree.node(top).is_none());
    assert!(!model.nodes.contains_key(&top));
    tests::assert_bits_from_scratch(&app);
}

/// A steps world saved in `backend`, the viewer at `at`.
fn persistent_app(backend: &Arc<MemoryStore>, at: DVec3) -> (App, Entity) {
    let steps = Steps::new(20);
    let registry = Arc::new(boxel_voxel::testing::registry());
    let store = WorldStore::open(
        Arc::clone(backend) as Arc<dyn ChunkStore>,
        &registry,
        steps.name(),
    )
    .expect("the world opens");
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(steps),
        registry,
        settings: WorldSettings {
            vertical_bounds: Some(-2..=3),
            ..WorldSettings::default()
        },
        executor: Arc::new(InlineExecutor),
    })
    .add_plugins(PersistPlugin::new(store).with_executor(Arc::new(InlineExecutor)))
    .add_plugins(LodWorldPlugin {
        settings: edit_settings(2, 0),
        executor: Arc::new(InlineExecutor),
    });
    app.insert_resource(PersistSettings {
        autosave: None,
        retry_after: Duration::ZERO,
        ..PersistSettings::default()
    });
    let viewer = spawn(&mut app, at);
    (app, viewer)
}

#[test]
fn with_persistence_saved_edits_feed_the_pyramids_after_unload_and_a_restart() {
    let backend = Arc::new(MemoryStore::new());
    let near = DVec3::new(10.0, 60.0, 10.0);
    let away = DVec3::new(298.0, 60.0, 10.0);
    let edited = tower_chunks();
    let top = node_at(1, [TOWER_X, *TOWER.end(), TOWER_Z]);
    let foot = node_at(1, [TOWER_X, *TOWER.start(), TOWER_Z]);
    let level2 = node_at(2, [TOWER_X, *TOWER.end(), TOWER_Z]);
    let want = |key| built_mesh(expected(key, &edited, true));
    {
        let (mut app, viewer) = persistent_app(&backend, near);
        let mut model = Model::default();
        fill(&mut app, &mut model, 600);
        edit_tower(&mut app);
        refill(&mut app, &mut model, 600);
        for key in [top, foot, level2] {
            assert_eq!(tree_mesh(&app, key), want(key), "{key:?}");
        }
        // Away: the tower's chunks unload and are saved; they stay edited.
        move_away(&mut app, &mut model, viewer, away);
        for c in &edited {
            assert!(app.world().resource::<ChunkMap>().id_at(*c).is_none());
        }
        assert_eq!(app.world().resource::<LodStats>().edited_chunks, 3);
        // Their pyramids come from the store: forgotten, they are read again from it and the
        // nodes they reach are built again the same.
        app.world_mut()
            .resource_scope(|world, mut lod: Mut<LodWorld>| {
                lod.edits.evict_all();
                let cov = world.resource::<LodCoverage>().coverage();
                for &c in &edited {
                    lod.mark_dirty(ChunkKey::new(c), cov);
                }
            });
        refill(&mut app, &mut model, 600);
        assert_eq!(app.world().resource::<LodStats>().pyramids, 3);
        for key in [top, foot, level2] {
            assert_eq!(tree_mesh(&app, key), want(key), "{key:?} after the unload");
        }
    }
    // A restart away from the tower: the saved chunks are edited from the first frame.
    let (mut app, _) = persistent_app(&backend, away);
    app.update();
    assert_eq!(app.world().resource::<LodStats>().edited_chunks, 3);
    let mut model = Model::default();
    fill(&mut app, &mut model, 600);
    for key in [top, foot, level2] {
        assert_eq!(tree_mesh(&app, key), want(key), "{key:?} after a restart");
    }
    for c in &edited {
        assert!(app.world().resource::<ChunkMap>().id_at(*c).is_none());
    }
}

// ---------------------------------------------------------------------------------------------
// The fallback

#[test]
fn a_generator_without_a_source_gets_level_1_from_its_chunks() {
    let steps = Steps {
        no_lod: true,
        ..Steps::new(20)
    };
    let mut app = app(steps, edit_settings(3, 0));
    let viewer_at = DVec3::new(10.0, 60.0, 10.0);
    spawn(&mut app, viewer_at);
    let mut model = Model::default();
    fill(&mut app, &mut model, 2000);
    let stats = app.world().resource::<LodStats>().clone();
    assert!(stats.fallback);
    assert_eq!(stats.off, None);
    assert_eq!(stats.levels.len(), 1, "level 1 only");
    // The horizon is T_1: 64 R.
    assert_eq!(stats.horizon, 512.0);
    assert!(stats.nodes() > 0, "{stats:?}");
    // Every node from the ground up is the exact rule on the chunks (the steps source is
    // exact); under it the fallback has the world's real floor where the source has rock.
    let lod = app.world().resource::<LodWorld>();
    let mut checked = 0;
    for (c, col) in &lod.tree.columns {
        for (&y, node) in &col.nodes {
            if y < 0 {
                continue;
            }
            let key = c.node(DimensionId(0), y);
            let have = match &node.state {
                tree::NodeState::Ready { mesh, .. } => Some((**mesh).clone()),
                tree::NodeState::Blank => None,
                other => panic!("{key:?}: {other:?}"),
            };
            assert_eq!(
                have,
                built_mesh(expected(key, &HashSet::default(), false)),
                "{key:?}"
            );
            checked += 1;
        }
    }
    assert!(checked > 20, "{checked}");
    tests::assert_bits_from_scratch(&app);
    // An edit reaches the fallback's nodes once its debounce ends.
    edit_tower(&mut app);
    refill(&mut app, &mut model, 600);
    let edited = tower_chunks();
    for at in [*TOWER.start(), *TOWER.end()] {
        let key = node_at(1, [TOWER_X, at, TOWER_Z]);
        assert_eq!(
            tree_mesh(&app, key),
            built_mesh(expected(key, &edited, true)),
            "{key:?}"
        );
    }
    // Without a fallback level the LOD is off.
    let steps = Steps {
        no_lod: true,
        ..Steps::new(20)
    };
    let settings = LodSettings {
        fallback_max_level: 0,
        ..small()
    };
    let mut app = app_with(
        steps,
        settings,
        Arc::new(InlineExecutor),
        Arc::new(InlineExecutor),
    );
    spawn(&mut app, viewer_at);
    for _ in 0..3 {
        app.update();
    }
    assert_eq!(
        app.world().resource::<LodStats>().off,
        Some(LodOff::NoSource)
    );
}

#[test]
fn a_column_reaching_more_edits_than_half_the_cache_does_not_wait() {
    // Three edited chunks in one level-1 column and room for four pyramids: waiting for all of
    // them could evict the ones it has, so the column goes on with the cached ones (none here)
    // and their pyramids are still asked for.
    let key = |y: i64| ChunkKey::new(ChunkPos::new(0, y, 0));
    let mut e = Edits::new(4);
    for y in 0..3 {
        e.insert(key(y));
    }
    let c = ColumnKey {
        level: 1,
        x: 0,
        z: 0,
    };
    assert!(matches!(
        e.for_column(c, (0, 1), DimensionId(0)),
        ForColumn::Ready { ref overlays, layers: Some((-1, 1)) } if overlays.is_empty()
    ));
    assert_eq!(e.next_job(1).map(|v| v.len()), Some(3));
    // With room for six, the same column waits for them.
    let mut e = Edits::new(6);
    for y in 0..3 {
        e.insert(key(y));
    }
    assert!(matches!(
        e.for_column(c, (0, 1), DimensionId(0)),
        ForColumn::Wait
    ));
}

// ---------------------------------------------------------------------------------------------
// Edits above the generator's top

/// A platform the player built over the generator's top layer (steps: `vertical_chunks` ends at
/// layer 3, y 127): y 136..=143 over x, z 32..=47, so whole cells up to level 3 in chunk layer 4.
const PLATFORM_Y: std::ops::RangeInclusive<i64> = 136..=143;
const PLATFORM_XZ: std::ops::RangeInclusive<i64> = 32..=47;

/// The block of the steps world at `p`, with the platform.
fn platform_block(registry: &BlockRegistry, p: [i64; 3]) -> BlockState {
    if PLATFORM_XZ.contains(&p[0]) && PLATFORM_XZ.contains(&p[2]) && PLATFORM_Y.contains(&p[1]) {
        return solid(registry);
    }
    block_at(registry, false, p)
}

fn edit_platform(app: &mut App) {
    let registry = Arc::clone(&app.world().resource::<WorldContent>().registry);
    let mut queue = app.world_mut().resource_mut::<EditQueue>();
    for y in PLATFORM_Y {
        for x in PLATFORM_XZ {
            for z in PLATFORM_XZ {
                queue.set_block(BlockPos::new(x, y, z), solid(&registry));
            }
        }
    }
}

/// A world of `steps` without vertical bounds (chunks above the generator's top load as air,
/// and the player can build there), the LOD on `executor`.
fn unbounded_app(steps: Steps, settings: LodSettings, executor: Arc<dyn JobExecutor>) -> App {
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(steps),
        registry: Arc::new(boxel_voxel::testing::registry()),
        settings: WorldSettings {
            vertical_bounds: None,
            ..WorldSettings::default()
        },
        executor: Arc::new(InlineExecutor),
    })
    .add_plugins(LodWorldPlugin { settings, executor });
    app
}

#[test]
fn edits_above_the_generators_top_show_at_the_fine_levels() {
    // With a source (levels 1 and 2), and with the fallback (level 1 only).
    for no_lod in [false, true] {
        let steps = Steps {
            no_lod,
            ..Steps::new(20)
        };
        let mut app = unbounded_app(steps, edit_settings(2, 0), Arc::new(InlineExecutor));
        spawn(&mut app, DVec3::new(40.0, 150.0, 40.0));
        let mut model = Model::default();
        fill(&mut app, &mut model, 2000);
        edit_platform(&mut app);
        refill(&mut app, &mut model, 2000);
        let registry = boxel_voxel::testing::registry();
        let edited: HashSet<ChunkPos> = [ChunkPos::new(1, 4, 1)].into_iter().collect();
        let stats = app.world().resource::<LodStats>().clone();
        assert_eq!(stats.edited_chunks, 1, "{stats:?}");
        let levels = if no_lod { 1 } else { 2 };
        assert_eq!(stats.levels.len(), usize::from(levels));
        for k in 1..=levels {
            let key = node_at(k, [40, 140, 40]);
            let want = built_mesh(expected_by(key, &edited, &|p| platform_block(&registry, p)));
            assert!(want.is_some(), "level {k}: the platform draws something");
            let lod = app.world().resource::<LodWorld>();
            assert!(
                lod.tree.wanted(key),
                "no_lod {no_lod}, level {k}: {key:?} above the generator's top is wanted"
            );
            assert_eq!(tree_mesh(&app, key), want, "no_lod {no_lod}, level {k}");
        }
        tests::assert_bits_from_scratch(&app);
    }
}

// ---------------------------------------------------------------------------------------------
// Rebuilds of neighbors

#[test]
fn neighbors_built_again_change_on_the_renderer_together() {
    // The tower's chunks lie by the border of lod-1 nodes 0 and 1 (x and z): four columns are
    // built again, each by its own job. One job runs a frame, so their results land apart. A
    // node meshed from the new cells next to one still drawn from the old ones would leave its
    // seam quads out where the other's cells differ (a see-through gap): no node may change on
    // the renderer while a same-level face neighbor waits to be built again.
    let lod = Arc::new(tests::Hold::default());
    let mut app = app_with(
        Steps::new(20),
        edit_settings(2, 0),
        lod.clone(),
        Arc::new(InlineExecutor),
    );
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut model = Model::default();
    tests::fill_held(&mut app, &lod, &mut model);
    edit_tower(&mut app);
    let mut changed = 0;
    for frame_no in 0..600 {
        lod.run_some(1);
        app.update();
        let ops = app.world_mut().resource_mut::<LodOps>().take();
        let keys: Vec<ChunkKey> = ops
            .iter()
            .filter_map(|op| match op {
                LodOp::Upsert { key, .. } | LodOp::Remove { key } => Some(*key),
                LodOp::Mask { .. } => None,
            })
            .collect();
        model.apply(ops);
        tests::check_model(&app, &model);
        let state = app.world().resource::<LodWorld>();
        for &key in &keys {
            for n in tree::face_neighbors(key) {
                assert!(
                    !state.tree.node(n).is_some_and(|n| n.stale),
                    "frame {frame_no}: {key:?} changed while its neighbor {n:?} waits to be built \
                     again"
                );
            }
        }
        changed += keys.len();
        let stats = app.world().resource::<LodStats>();
        if stats.edited_chunks > 0
            && stats.fill >= 1.0
            && stats.in_flight == 0
            && stats.sends_waiting == 0
            && lod.held() == 0
        {
            break;
        }
    }
    assert!(changed > 0);
    let stats = app.world().resource::<LodStats>().clone();
    assert!(stats.fill >= 1.0 && lod.held() == 0, "{stats:?}");
    let edited = tower_chunks();
    for k in 1..=2u8 {
        for at in [
            [TOWER_X, *TOWER.end(), TOWER_Z],
            [TOWER_X, *TOWER.start(), TOWER_Z],
        ] {
            let key = node_at(k, at);
            let want = built_mesh(expected(key, &edited, true));
            assert_eq!(tree_mesh(&app, key), want, "level {k} {key:?}");
            let quads = want.as_ref().map(ChunkMesh::total_quads);
            assert_eq!(model.nodes.get(&key).map(|n| n.1), quads, "level {k}");
        }
    }
    tests::assert_bits_from_scratch(&app);
}

// ---------------------------------------------------------------------------------------------
// The fallback between frames

#[test]
fn fallback_idle_steps_do_not_spin_on_jobs_they_cannot_start() {
    // A fallback job copies the loaded chunks when it is made, so an idle step (no chunk map)
    // never starts one: once it has collected the last result, it must not ask for another
    // step at once.
    let lod = Arc::new(tests::Hold::default());
    let steps = Steps {
        no_lod: true,
        ..Steps::new(20)
    };
    let mut app = app_with(
        steps,
        edit_settings(1, 0),
        lod.clone(),
        Arc::new(InlineExecutor),
    );
    spawn(&mut app, DVec3::new(10.0, 60.0, 10.0));
    let mut ready = false;
    for _ in 0..100 {
        app.update();
        let settled = app.world().resource::<WorldStats>().is_settled();
        let state = app.world().resource::<LodWorld>();
        if settled && lod.held() == 1 && state.jobs.queue.len() > 1 {
            ready = true;
            break;
        }
        lod.run_all();
    }
    assert!(ready, "a fallback job in flight and more queued");
    assert!(app.world().resource::<IdleWanted>().wanted());
    // The job ends between frames: the first step collects it.
    lod.run_all();
    let until = Instant::now() + Duration::from_secs(5);
    let first = app.run_idle(until);
    let state = app.world().resource::<LodWorld>();
    assert_eq!(state.jobs.in_flight, 0, "the step collected the result");
    assert!(!state.jobs.queue.is_empty());
    assert_ne!(first, IdleWork::Ready, "nothing left that a step can do");
    assert_ne!(state.idle_pending(false), IdleWork::Ready);
    assert_ne!(app.run_idle(until), IdleWork::Ready);
    assert_eq!(lod.held(), 0, "no step started a job");
    // The next update starts the next job, and asks for steps only while it runs.
    app.update();
    assert_eq!(lod.held(), 1);
    assert!(app.world().resource::<IdleWanted>().wanted());
}
