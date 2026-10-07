//! Sky columns in the state machine (fake mesher): walks wait for the data they read instead of
//! generating it, a walk that read content an edit changed is discarded, an edited chunk that
//! unloads sends its column back to a walk (its edits are lost), and generators without a top
//! or without content.
use super::*;
use crate::sky::SkyRule;

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

/// A chunk whose edit moved a sky top unloads: the edit is lost (nothing is persisted yet), so
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
