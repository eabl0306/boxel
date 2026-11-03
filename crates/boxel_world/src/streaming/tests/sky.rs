//! Sky columns in the state machine (fake mesher): walks wait for the data they read instead of
//! generating it, a walk that read content an edit changed is discarded, an edited chunk that
//! unloads sends its column back to a walk (its edits are lost) unless the world is saved (then
//! walks and rescans read the saved chunk), and generators without a top or without content.
//! Beam tiles (ADR 0061): built only within reach of a layer that may hold a source, waiting for
//! the data they read, and built again when an edit changed what a running build read.
use super::*;
use crate::sky::SkyRule;
use crate::tests::support::{LitCavern, source_registry};

fn one(center: ChunkPos, radius: u32, vertical: u32) -> [(u64, Viewer); 1] {
    [(0, Viewer::new(center, radius, vertical))]
}

fn lit_world(executor: Arc<dyn JobExecutor>) -> Harness {
    Harness::new(LayerGen::new(-3..=1), executor, 256).lit()
}

fn light_counts(h: &Harness) -> WorldStats {
    let mut stats = WorldStats::default();
    h.jobs.light_stats(&mut stats);
    stats
}

/// Every mesh needs tiles here (the layers reach the void above the floor). The walks wait for
/// the generation of the layers they read, so they generate nothing, and each column is walked
/// once.
#[test]
fn walks_read_loaded_data_and_wait_for_it() {
    let hold = Arc::new(HoldExecutor::default());
    let mut h = lit_world(hold.clone());
    h.interest(&origin(1));
    h.dispatch();
    // Only generations so far: no mesh can wait for a tile before its neighbors are ready.
    assert_eq!(hold.held(), h.map.len());
    assert!(h.sky.columns.values().all(|c| !c.queued));
    while hold.held() > 0 {
        hold.run_all();
        h.collect();
        h.dispatch();
    }
    h.settle(20);
    let counts = light_counts(&h);
    assert_eq!(counts.sky_private_generations, 0);
    assert_eq!(counts.sky_walks, h.sky.walked());
    assert_eq!(h.sky.walked(), 21, "the columns around the 5 mesh columns");
    assert_eq!(counts.waiting_sky, 0);
    // Stone up to y = -1 everywhere.
    assert_eq!(h.sky.top(7, -40), Some(-1));
    assert!(h.sky.top(7, 2000).is_none(), "not loaded");
}

/// An edit to a column whose walk is running: the walk's result is discarded and the column
/// walked again with the edit.
#[test]
fn a_walk_that_read_edited_content_is_walked_again() {
    let hold = Arc::new(HoldExecutor::default());
    let mut h = lit_world(hold.clone());
    h.interest(&origin(0));
    // Generations, then the walks they made wanted, held while running.
    loop {
        h.dispatch();
        let running = h
            .sky
            .columns
            .values()
            .any(|c| matches!(c.state, SkyState::InFlight { .. }));
        if running {
            break;
        }
        assert!(hold.held() > 0, "no walk started");
        hold.run_all();
        h.collect();
    }
    let center = h.sky.id_at(0, 0).expect("column");
    assert!(matches!(
        h.sky.columns[center].state,
        SkyState::InFlight { .. }
    ));
    h.edit(BlockPos::new(3, 10, 4), STONE);
    assert_eq!(h.sky.columns[center].version, 1);
    hold.run_all();
    h.collect();
    assert!(
        !matches!(h.sky.columns[center].state, SkyState::Exact(_)),
        "the stale walk was discarded"
    );
    loop {
        h.dispatch();
        if hold.held() == 0 {
            break;
        }
        hold.run_all();
        h.collect();
    }
    h.settle(20);
    assert_eq!(h.sky.top(3, 4), Some(10));
    assert_eq!(h.sky.top(3, 5), Some(-1));
}

/// The default blocks plus `test:red_glass`: opacity 0, light filter `[10, 3, 2]`.
fn red_glass_registry() -> (Arc<BlockRegistry>, BlockState) {
    let mut b = boxel_demo::blocks::builder();
    let red = b
        .add_block(boxel_voxel::BlockDef {
            name: "test:red_glass".into(),
            shape: RenderShape::Translucent,
            model: boxel_voxel::BlockModel::CUBE,
            collision: boxel_voxel::BlockCollision::Full,
            emission: [0; 3],
            light_opacity: 0,
            textures: boxel_voxel::FaceTextures::all(boxel_voxel::TextureId(0)),
        })
        .expect("glass");
    let block = b.block_by_name("test:red_glass").expect("glass");
    b.set_light_filter(block, [10, 3, 2]).expect("filter");
    (Arc::new(b.build()), red)
}

/// Colored sky (ADR 0064): glass placed far above every blocker (and the generator's top)
/// while the column's walk runs. The walk is discarded, and the next one starts at the glass
/// (`SkyColumn::edit_top`), so its tile keeps it as a tint. Off, glass is nothing to the sky:
/// the running walk stands.
#[test]
fn a_walk_after_glass_above_every_blocker_starts_at_the_glass() {
    for colored in [false, true] {
        let (registry, red) = red_glass_registry();
        let hold = Arc::new(HoldExecutor::default());
        let h = Harness::new(LayerGen::new(-3..=1), hold.clone(), 256)
            .tall()
            .with_registry(registry)
            .lit();
        let mut h = if colored { h.colored() } else { h };
        h.interest(&origin(0));
        loop {
            h.dispatch();
            let running = h
                .sky
                .columns
                .values()
                .any(|c| matches!(c.state, SkyState::InFlight { .. }));
            if running {
                break;
            }
            assert!(hold.held() > 0, "no walk started");
            hold.run_all();
            h.collect();
        }
        let center = h.sky.id_at(0, 0).expect("column");
        assert!(matches!(
            h.sky.columns[center].state,
            SkyState::InFlight { .. }
        ));
        let glass = BlockPos::new(3, 100, 4);
        assert!(
            h.map.data_at(ChunkPos::containing(glass)).is_some(),
            "loaded"
        );
        h.edit(glass, red);
        let c = &h.sky.columns[center];
        assert_eq!(
            (c.version, c.edit_top),
            if colored { (1, 100) } else { (0, i64::MIN) }
        );
        loop {
            h.dispatch();
            if hold.held() == 0 {
                break;
            }
            hold.run_all();
            h.collect();
        }
        h.settle(20);
        let tile = h.sky.tile(0, 0).expect("walked");
        assert_eq!(tile.top(3, 4), -1);
        let tints = if colored {
            vec![boxel_light::Tint {
                y: 100,
                filter: [10, 3, 2],
            }]
        } else {
            Vec::new()
        };
        assert_eq!(tile.tints(3, 4), &tints[..]);
        assert_eq!(tile.tint_count(), tints.len());
    }
}

/// A chunk whose edit moved a sky top unloads: the edit is lost (no persistence here), so
/// the column is walked again and the top goes back to the generator's.
#[test]
fn an_edited_chunk_that_unloads_takes_its_tops_with_it() {
    let mut h = Harness::new(LayerGen::new(-2..=1), Arc::new(InlineExecutor), 64)
        .tall()
        .lit();
    h.viewers(&one(ChunkPos::new(0, 0, 0), 0, 1));
    h.settle(20);
    assert_eq!(h.sky.top(3, 4), Some(-1));
    h.edit(BlockPos::new(3, 40, 4), STONE);
    h.settle(20);
    assert_eq!(h.sky.top(3, 4), Some(40));
    let walks = light_counts(&h).sky_walks;
    // Down: layer 1 leaves, the column stays (its lower layers are loaded).
    h.viewers(&one(ChunkPos::new(0, -4, 0), 0, 1));
    h.settle(20);
    assert!(h.map.id_at(ChunkPos::new(0, 1, 0)).is_none(), "unloaded");
    assert!(h.sky.id_at(0, 0).is_some());
    assert_eq!(h.sky.top(3, 4), Some(-1), "walked again without the edit");
    assert!(light_counts(&h).sky_walks > walks);
}

/// With persistence (ADR 0024) the edits of a chunk that unloads are saved: its column keeps its
/// tops without a walk, a later walk reads the saved chunk, and a rescan does not take a saved
/// chunk that is not loaded for air above the generator.
#[test]
fn saved_edits_keep_their_tops() {
    use crate::persist::{MemoryStore, Persist, WorldStore};
    // Stone in layers -2 and -1, air in layer 0 (the generator's last), nothing above.
    let mut h = Harness::new(LayerGen::new(-2..=0), Arc::new(InlineExecutor), 64)
        .tall()
        .lit();
    let store = WorldStore::open(Arc::new(MemoryStore::new()), &h.content.registry, "layers")
        .expect("opens");
    h.jobs.persist = Some(Persist::new(store, Arc::new(InlineExecutor)));
    h.viewers(&one(ChunkPos::new(0, 0, 0), 0, 1));
    h.settle(20);
    assert_eq!(h.sky.top(3, 4), Some(-1));
    h.edit(BlockPos::new(3, 40, 4), STONE);
    h.settle(20);
    assert_eq!(h.sky.top(3, 4), Some(40));
    let walks = light_counts(&h).sky_walks;
    // Down: layer 1 leaves (saved), the column stays and keeps its tops.
    h.viewers(&one(ChunkPos::new(0, -4, 0), 0, 1));
    h.settle(20);
    assert!(h.map.id_at(ChunkPos::new(0, 1, 0)).is_none(), "unloaded");
    assert!(h.sky.id_at(0, 0).is_some());
    assert_eq!(h.sky.top(3, 4), Some(40), "kept");
    assert_eq!(light_counts(&h).sky_walks, walks, "no walk");
    // One viewer in the ground (layers -2 to 0) and one above (layers 3 to 5): layers 1 and 2
    // are not loaded. A new block on top, then removed: the next top down is the saved block in
    // layer 1, not the ground of layer -1.
    h.viewers(&[
        (0, Viewer::new(ChunkPos::new(0, -1, 0), 0, 0)),
        (1, Viewer::new(ChunkPos::new(0, 4, 0), 0, 0)),
    ]);
    h.settle(20);
    for y in [1, 2] {
        assert!(h.map.id_at(ChunkPos::new(0, y, 0)).is_none(), "layer {y}");
    }
    assert!(h.map.id_at(ChunkPos::new(0, 0, 0)).is_some());
    assert_eq!(h.sky.top(3, 4), Some(40), "walked from the saved chunk");
    h.edit(BlockPos::new(3, 130, 4), STONE);
    h.settle(20);
    assert_eq!(h.sky.top(3, 4), Some(130));
    h.edit(BlockPos::new(3, 130, 4), BlockState::AIR);
    h.settle(20);
    assert_eq!(h.sky.top(3, 4), Some(40), "the saved block, not the ground");
}

/// Without a declared top, light runs with every column open; without content, every column
/// is open until an edit places a blocker.
#[test]
fn generators_without_a_top_or_without_content() {
    struct NoTop;
    impl WorldGenerator for NoTop {
        fn generate(&self, _: ChunkKey, _: &BlockRegistry) -> ChunkData {
            ChunkData::uniform(BlockState::AIR)
        }
        fn name(&self) -> &str {
            "no top"
        }
    }
    let mut h = Harness::new(NoTop, Arc::new(InlineExecutor), 64)
        .tall()
        .lit();
    assert_eq!(h.sky.rule, SkyRule::Open);
    h.viewers(&one(ChunkPos::new(0, 0, 0), 0, 0));
    h.settle(20);
    h.edit(BlockPos::new(1, 2, 3), STONE);
    h.settle(20);
    assert!(matches!(
        h.slot(ChunkPos::new(0, 0, 0)).mesh(),
        MeshState::Ready { .. }
    ));
    assert_eq!(light_counts(&h).sky_walks, 0);
    assert!(h.sky.tile(0, 0).is_none(), "no tiles at all");

    // An empty range: the generator places nothing.
    let mut h = Harness::new(
        LayerGen::new(RangeInclusive::new(0, -1)),
        Arc::new(InlineExecutor),
        64,
    )
    .tall()
    .lit();
    let SkyRule::Walk(rule) = &h.sky.rule else {
        panic!("an empty generator is walked")
    };
    assert_eq!((rule.top, rule.floor), (None, i64::MIN));
    h.viewers(&one(ChunkPos::new(0, 0, 0), 0, 0));
    h.settle(20);
    h.edit(BlockPos::new(1, 2, 3), STONE);
    h.settle(20);
    assert_eq!(h.sky.top(1, 3), Some(2));
    assert_eq!(h.sky.top(1, 4), Some(i64::MIN), "open forever");
}

/// Walks, sky bands and rescans visit only the loaded layers of a column: viewers `2^35` layers
/// apart in the same columns (the walk floor in between, the void reachable), a block under the
/// floor next to the deepest one, and a block placed and removed far above the ground, all
/// settle at once.
#[test]
fn columns_with_layers_far_apart_settle_at_once() {
    let mut h = Harness::new(LayerGen::new(-3..=1), Arc::new(InlineExecutor), 256)
        .tall()
        .lit();
    let SkyRule::Walk(rule) = &h.sky.rule else {
        panic!("walked")
    };
    assert!(
        rule.void_reachable,
        "loaded layers under the floor are read"
    );
    let deep = ChunkPos::new(0, -(1 << 35), 0);
    let high = ChunkPos::new(0, 1 << 35, 0);
    h.viewers(&[
        (0, Viewer::new(ChunkPos::new(0, 0, 0), 1, 1)),
        (1, Viewer::new(deep, 1, 1)),
        (2, Viewer::new(high, 1, 1)),
    ]);
    h.settle(40);
    assert_eq!(h.sky.top(3, 4), Some(-1));
    // Under the floor: the column is walked again and its open columns would close at the
    // floor (here the stone closes them first).
    h.edit(BlockPos::new(3, deep.origin().y + 5, 4), STONE);
    h.settle(40);
    assert_eq!(h.sky.top(3, 4), Some(-1));
    // Far above: the band reaches from the ground to the block.
    let above = BlockPos::new(3, high.origin().y + 5, 4);
    h.edit(above, STONE);
    h.settle(40);
    assert_eq!(h.sky.top(3, 4), Some(above.y));
    // Removed: the rescan goes down to the ground in a few steps.
    h.edit(above, BlockState::AIR);
    h.settle(40);
    assert_eq!(h.sky.top(3, 4), Some(-1));
    assert!(
        h.sky
            .columns
            .values()
            .all(|c| c.layers.is_sorted() && c.layers.len() <= 15)
    );
}

/// The lit cavern, lit, with `test:sky_source` in the registry.
fn cavern(executor: Arc<dyn JobExecutor>) -> Harness {
    let mut h = Harness::new(LitCavern, executor, 256).tall();
    let (registry, _) = source_registry();
    h.content = WorldContent::new(Arc::clone(&h.content.generator), registry);
    h.lit()
}

/// A viewer far above the cavern's source layers needs no beam tile: no build, no wait. One in
/// the cavern gets a tile for every column around it, built from loaded data only (each build
/// waits for the layers it reads).
#[test]
fn beam_tiles_are_built_only_near_sources_and_wait_for_their_data() {
    let mut h = cavern(Arc::new(InlineExecutor));
    h.viewers(&one(ChunkPos::new(0, 30, 0), 1, 1));
    h.settle(40);
    let counts = light_counts(&h);
    assert_eq!((counts.beam_jobs, counts.waiting_beams), (0, 0));
    assert_eq!(h.sky.beam_stats().tiles, 0);

    let hold = Arc::new(HoldExecutor::default());
    let mut h = cavern(hold.clone());
    h.viewers(&one(ChunkPos::new(0, -3, 0), 1, 3));
    for _ in 0..200 {
        h.dispatch();
        if hold.held() == 0 {
            break;
        }
        hold.run_all();
        h.collect();
    }
    h.settle(40);
    let counts = light_counts(&h);
    assert!(counts.beam_jobs > 0);
    assert_eq!(counts.waiting_beams, 0);
    // Every column around a mesh has one.
    assert_eq!(h.sky.beam_stats().tiles, h.sky.len());
    let tile = h.sky.beam_tile(0, 0).expect("built");
    assert!(!tile.is_empty());
    assert_eq!(tile.column(10, 4).len(), 1, "the roof source of (10, 4)");
    assert_eq!(tile.column(11, 4).len(), 0);
}

/// An edit in a column whose build is running: its result is discarded, the column is built
/// again with the edit, and the edit's new beam is in the tile.
#[test]
fn a_beam_build_that_read_edited_content_is_built_again() {
    let hold = Arc::new(HoldExecutor::default());
    let mut h = cavern(hold.clone());
    let sources = crate::tests::support::Sources::of(&h.content.registry);
    h.viewers(&one(ChunkPos::new(0, -3, 0), 0, 1));
    let center = h.sky.id_at(0, 0).expect("column");
    loop {
        h.dispatch();
        if h.sky.columns[center]
            .beams
            .as_ref()
            .is_some_and(|b| b.in_flight.is_some())
        {
            break;
        }
        assert!(hold.held() > 0, "no build started");
        hold.run_all();
        h.collect();
    }
    let version = h.sky.columns[center].beams.as_ref().expect("beams").version;
    // Under the ledge, a column without a roof source.
    h.edit(BlockPos::new(31, -101, 4), sources.down[1]);
    assert_eq!(
        h.sky.columns[center].beams.as_ref().expect("beams").version,
        version + 1
    );
    hold.run_all();
    h.collect();
    assert!(
        h.sky.columns[center]
            .beams
            .as_ref()
            .expect("beams")
            .tile
            .is_none(),
        "the stale build was discarded"
    );
    loop {
        h.dispatch();
        if hold.held() == 0 {
            break;
        }
        hold.run_all();
        h.collect();
    }
    h.settle(40);
    let tile = h.sky.beam_tile(0, 0).expect("built");
    let beams: Vec<_> = tile.column(31, 4).collect();
    assert_eq!(beams.len(), 1);
    assert_eq!(
        (beams[0].source, beams[0].lo, beams[0].hi),
        (-101, -116, -101)
    );
}
