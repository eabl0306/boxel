//! Neighbor rules in the world (ADR 0020): stairs, fences and torches follow the edits around
//! them in the same edit batch, with one `BlockChanged` per block.
use super::{settings, settle, spawn_viewer, world_app};
use crate::jobs::InlineExecutor;
use crate::tests::support::LayerGen;
use crate::{BlockChanged, ChunkMap, EditQueue, NeighborRules};
use boxel_app::{App, Update};
use boxel_ecs::prelude::*;
use boxel_voxel::{BlockPos, BlockRegistry, BlockState, ChunkPos};
use std::sync::Arc;

/// Every `BlockChanged` read in `Update` after the world's systems.
#[derive(Resource, Default)]
struct ChangeLog(Vec<BlockChanged>);

fn log_changes(mut changes: MessageReader<BlockChanged>, mut log: ResMut<ChangeLog>) {
    log.0.extend(changes.read().copied());
}

/// Stone up to block y = -1, air above, loaded around the origin.
fn app() -> App {
    let mut app = world_app(
        LayerGen::new(-3..=1),
        settings(64),
        Arc::new(InlineExecutor),
    );
    app.init_resource::<ChangeLog>()
        .add_systems(Update, log_changes.after(crate::WorldSystems::Collect));
    spawn_viewer(&mut app, ChunkPos::new(0, 0, 0), 1);
    settle(&mut app, 200);
    app
}

fn state(name: &str) -> BlockState {
    BlockRegistry::default_blocks().parse_state(name).unwrap()
}

fn edit(app: &mut App, edits: &[(BlockPos, &str)]) -> Vec<BlockChanged> {
    {
        let mut queue = app.world_mut().resource_mut::<EditQueue>();
        for &(pos, name) in edits {
            queue.set_block(pos, state(name));
        }
    }
    app.world_mut().resource_mut::<ChangeLog>().0.clear();
    settle(app, 50);
    std::mem::take(&mut app.world_mut().resource_mut::<ChangeLog>().0)
}

fn at(app: &App, pos: BlockPos) -> String {
    let registry = BlockRegistry::default_blocks();
    let s = app.world().resource::<ChunkMap>().block_at(pos).unwrap();
    registry.state_name(s).to_string()
}

#[test]
fn placed_stairs_take_their_shape_and_turn_their_neighbors() {
    let mut app = app();
    let (a, b) = (BlockPos::new(3, 0, 3), BlockPos::new(4, 0, 3));
    // Stairs facing east, then stairs facing north on their high side: an outer corner.
    edit(&mut app, &[(a, "boxel:stone_stairs[facing=east]")]);
    assert_eq!(
        at(&app, a),
        "boxel:stone_stairs[facing=east,half=bottom,shape=straight,waterlogged=false]"
    );
    let changes = edit(&mut app, &[(b, "boxel:oak_stairs[facing=north]")]);
    assert_eq!(
        at(&app, a),
        "boxel:stone_stairs[facing=east,half=bottom,shape=outer_left,waterlogged=false]"
    );
    assert!(at(&app, b).contains("shape=straight"));
    // One message per block, the placed one first.
    assert_eq!(changes.iter().map(|c| c.pos).collect::<Vec<_>>(), [b, a]);
    // Gone again: straight again.
    edit(&mut app, &[(b, "boxel:air")]);
    assert!(at(&app, a).contains("shape=straight"));
}

#[test]
fn fences_connect_to_fences_and_full_faces() {
    let mut app = app();
    let (f, g, wall) = (
        BlockPos::new(5, 0, 5),
        BlockPos::new(6, 0, 5),
        BlockPos::new(5, 0, 4),
    );
    edit(
        &mut app,
        &[
            (f, "boxel:oak_fence"),
            (g, "boxel:oak_fence"),
            (wall, "boxel:stone"),
        ],
    );
    assert_eq!(
        at(&app, f),
        "boxel:oak_fence[east=true,north=true,south=false,waterlogged=false,west=false]"
    );
    assert_eq!(
        at(&app, g),
        "boxel:oak_fence[east=false,north=false,south=false,waterlogged=false,west=true]"
    );
    // A bottom slab's side is not a full face; a torch is nothing to connect to.
    edit(
        &mut app,
        &[
            (wall, "boxel:stone_slab"),
            (BlockPos::new(5, 0, 6), "boxel:torch"),
        ],
    );
    assert_eq!(
        at(&app, f),
        "boxel:oak_fence[east=true,north=false,south=false,waterlogged=false,west=false]"
    );
}

#[test]
fn waterlogged_blocks_keep_their_water_when_their_shape_changes() {
    let mut app = app();
    let (f, stairs, wall) = (
        BlockPos::new(8, 0, 8),
        BlockPos::new(8, 0, 10),
        BlockPos::new(9, 0, 8),
    );
    edit(
        &mut app,
        &[
            (f, "boxel:oak_fence[waterlogged=true]"),
            (stairs, "boxel:stone_stairs[facing=east,waterlogged=true]"),
        ],
    );
    edit(
        &mut app,
        &[
            (wall, "boxel:stone"),
            (BlockPos::new(9, 0, 10), "boxel:oak_stairs[facing=north]"),
        ],
    );
    assert_eq!(
        at(&app, f),
        "boxel:oak_fence[east=true,north=false,south=false,waterlogged=true,west=false]"
    );
    assert_eq!(
        at(&app, stairs),
        "boxel:stone_stairs[facing=east,half=bottom,shape=outer_left,waterlogged=true]"
    );
}

#[test]
fn torches_drop_when_their_support_goes() {
    let mut app = app();
    let (wall, torch, floor_torch) = (
        BlockPos::new(8, 0, 8),
        BlockPos::new(9, 0, 8),
        BlockPos::new(2, 0, 2),
    );
    edit(
        &mut app,
        &[
            (wall, "boxel:stone"),
            (torch, "boxel:wall_torch[facing=east]"),
            (floor_torch, "boxel:torch"),
            // No support: never placed.
            (BlockPos::new(2, 3, 2), "boxel:torch"),
        ],
    );
    assert_eq!(at(&app, torch), "boxel:wall_torch[facing=east]");
    assert_eq!(at(&app, floor_torch), "boxel:torch");
    assert_eq!(at(&app, BlockPos::new(2, 3, 2)), "boxel:air");
    let changes = edit(
        &mut app,
        &[(wall, "boxel:air"), (BlockPos::new(2, -1, 2), "boxel:air")],
    );
    assert_eq!(at(&app, torch), "boxel:air");
    assert_eq!(at(&app, floor_torch), "boxel:air");
    let air = BlockState::AIR;
    assert!(changes.contains(&BlockChanged {
        pos: torch,
        old: state("boxel:wall_torch[facing=east]"),
        new: air
    }));
}

#[test]
fn without_rules_the_edits_stay_as_given() {
    let mut app = app();
    *app.world_mut().resource_mut::<NeighborRules>() = NeighborRules::default();
    let p = BlockPos::new(3, 5, 3);
    edit(&mut app, &[(p, "boxel:torch")]);
    assert_eq!(at(&app, p), "boxel:torch");
}
