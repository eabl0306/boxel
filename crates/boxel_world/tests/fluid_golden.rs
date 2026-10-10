//! G7 of #48: water and lava of the built-in content in a headless world. A flat world with
//! inline jobs and the fluid ticks, scripted pours (water into a basin with a waterlogged slab,
//! stairs and fence; lava beside water; lava falling into water; an infinite-source pool; a
//! source removed so its water drains), 400 game ticks, then the blocks of the area and every
//! `BlockChanged` frame by frame. Frozen before the content moved out of the engine: the literal
//! never changes, only the setup lines below may.

#[path = "../../boxel_voxel/tests/golden/mod.rs"]
mod golden;

use boxel_app::{App, Update};
use boxel_ecs::prelude::*;
use boxel_math::DVec3;
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, BlockRegistry, BlockState};
use boxel_world::jobs::InlineExecutor;
use boxel_world::{
    BlockChanged, BlockTickSettings, BlockTicks, ChunkMap, ChunkViewer, EditQueue, FluidPlugin,
    WorldPlugin, WorldSettings, WorldStats, WorldSystems,
};
use boxel_worldgen::{FlatGenerator, WorldGenerator};
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The registry under test.
fn registry() -> BlockRegistry {
    BlockRegistry::default_blocks()
}

/// The world: grass at y = 63 over dirt, stone and bedrock (the examples' flat world).
fn generator() -> FlatGenerator {
    FlatGenerator::new(vec![
        ("boxel:bedrock".to_string(), 1),
        ("boxel:stone".to_string(), 59),
        ("boxel:dirt".to_string(), 3),
        ("boxel:grass".to_string(), 1),
    ])
}

/// Every `BlockChanged` of a frame, read after the world's systems.
#[derive(Resource, Default)]
struct Log(Vec<BlockChanged>);

fn log(mut changes: MessageReader<BlockChanged>, mut log: ResMut<Log>) {
    log.0.extend(changes.read().copied());
}

/// Ground top.
const Y: i64 = 63;

fn pours(r: &BlockRegistry) -> Vec<(BlockPos, BlockState)> {
    let s = |text: &str| r.parse_state(text).expect(text);
    let mut out = Vec::new();
    let mut put = |x: i64, y: i64, z: i64, text: &str| out.push((BlockPos::new(x, y, z), s(text)));
    // A stone basin (x 1..=7, z 1..=7, two blocks high) with a waterlogged slab, stairs and
    // fence, and a water source poured in its corner.
    for x in 1..=7 {
        for z in 1..=7 {
            if x == 1 || x == 7 || z == 1 || z == 7 {
                put(x, Y + 1, z, "boxel:stone");
                put(x, Y + 2, z, "boxel:stone");
            }
        }
    }
    put(3, Y + 1, 3, "boxel:stone_slab[waterlogged=true]");
    put(
        5,
        Y + 1,
        3,
        "boxel:stone_stairs[facing=east,waterlogged=true]",
    );
    put(4, Y + 1, 5, "boxel:oak_fence[waterlogged=true]");
    put(3, Y + 1, 5, "boxel:stone_slab[type=top]");
    put(2, Y + 1, 2, "boxel:water");
    // Lava beside water on open ground.
    put(12, Y + 1, 4, "boxel:lava");
    put(15, Y + 1, 4, "boxel:water");
    // Lava falling into a water pool held by stone.
    for x in 19..=23 {
        for z in 2..=6 {
            if x == 19 || x == 23 || z == 2 || z == 6 {
                put(x, Y + 1, z, "boxel:stone");
            }
        }
    }
    put(21, Y + 1, 4, "boxel:water");
    put(21, Y + 5, 4, "boxel:lava");
    // An infinite-source pool: two sources in a trench, one cell apart.
    for x in 3..=7 {
        put(x, Y + 1, 11, "boxel:stone");
        put(x, Y + 1, 13, "boxel:stone");
    }
    put(3, Y + 1, 12, "boxel:stone");
    put(7, Y + 1, 12, "boxel:stone");
    put(4, Y + 1, 12, "boxel:water");
    put(6, Y + 1, 12, "boxel:water");
    // A source on open ground, removed later.
    put(16, Y + 1, 20, "boxel:water");
    out
}

fn dump(registry: BlockRegistry) -> String {
    let registry = Arc::new(registry);
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(generator()),
        registry: Arc::clone(&registry),
        executor: Arc::new(InlineExecutor),
        settings: WorldSettings {
            light: false,
            ..WorldSettings::default()
        },
    })
    .add_plugins(FluidPlugin)
    .init_resource::<Log>()
    .add_systems(Update, log.after(WorldSystems::Collect));
    *app.world_mut().resource_mut::<BlockTickSettings>() = BlockTickSettings {
        min_ticks_per_frame: usize::MAX,
        ..BlockTickSettings::default()
    };
    app.world_mut().spawn((
        Transform::from_translation(DVec3::new(16.0, (Y + 1) as f64, 16.0)),
        ChunkViewer::new(1).with_vertical_radius(1),
    ));
    let start = Instant::now();
    loop {
        app.update();
        let stats = *app.world().resource::<WorldStats>();
        if stats.chunks > 0 && stats.is_settled() {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "not settled");
    }
    app.world_mut().resource_mut::<Log>().0.clear();

    let r = &*registry;
    let mut d = String::new();
    let _ = writeln!(d, "generator {}", generator().name());
    {
        let mut queue = app.world_mut().resource_mut::<EditQueue>();
        for (p, s) in pours(r) {
            queue.set_block(p, s);
        }
    }
    let removed = BlockPos::new(16, Y + 1, 20);
    for frame in 0..=400u32 {
        if frame == 150 {
            app.world_mut()
                .resource_mut::<EditQueue>()
                .set_block(removed, BlockState::AIR);
        }
        if frame > 0 {
            app.world_mut().resource_mut::<BlockTicks>().advance(1);
        }
        app.update();
        let changes = std::mem::take(&mut app.world_mut().resource_mut::<Log>().0);
        for c in changes {
            let _ = writeln!(
                d,
                "{frame} {} {} {} {} -> {}",
                c.pos.x,
                c.pos.y,
                c.pos.z,
                r.state_name(c.old),
                r.state_name(c.new)
            );
        }
    }
    let map = app.world().resource::<ChunkMap>();
    for y in Y..=Y + 6 {
        for z in -8..40 {
            let row: Vec<String> = (-8..40)
                .map(|x| {
                    map.block_at(BlockPos::new(x, y, z))
                        .map_or_else(|| "?".to_string(), |s| s.0.to_string())
                })
                .collect();
            let _ = writeln!(d, "y={y} z={z} {}", row.join(","));
        }
    }
    d
}

#[test]
fn the_fluids_of_the_content_are_frozen() {
    golden::check("fluid", &dump(registry()), 0x30f1_bd87_1f7c_32e8);
}
