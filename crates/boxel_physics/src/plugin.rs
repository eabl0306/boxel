//! [`PhysicsPlugin`] and the physics view of the world's blocks.

use crate::components::{
    CollisionEnded, CollisionStarted, Disturbed, PhysicsSettings, PhysicsStats, ProjectileHit,
    SleepLog, Vacated, VoxelImpact,
};
use crate::contacts::{
    BodyIndex, ContactSet, ObstacleIndex, broadphase, build_obstacles, solve, write_collisions,
};
use crate::core::{CollisionTable, Surface};
use crate::persist::BodyCodec;
use crate::projectile::{PendingHits, step_projectiles, write_hits};
use crate::sleep::{sleep_bodies, wake_bodies, wake_to_pose};
use crate::systems::{
    AwakeCount, PendingImpacts, PhysicsSystems, WritebackTick, drive_characters, interpolate,
    move_kinematic, prepare_bodies, publish_stats, step_dynamic, write_back, write_events,
};
use boxel_app::{
    App, FixedPostUpdate, FixedTime, Last, Plugin, RunFixedMainLoop, RunFixedMainLoopSystems,
};
use boxel_ecs::prelude::*;
use boxel_ecs::schedule::SingleThreadedExecutor;
use boxel_voxel::{BlockId, BlockRegistry, BlockState};
use boxel_world::{BlockChanged, ChunkDataEvent, WorldContent};
use std::sync::Arc;

/// The physics view of the world's blocks: the [`CollisionTable`] derived from
/// `WorldContent::registry`. Inserted by [`PhysicsPlugin`] in `Plugin::finish`.
#[derive(Resource, Clone, Debug)]
pub struct PhysicsContent {
    /// How each block state collides, plus the fluid and outline flags.
    pub collision: Arc<CollisionTable>,
}

/// The [`Surface`] of each block that has one (ADR 0047), by block name: slippery ground,
/// ground that slows walkers, cells that hold bodies back, cells characters climb, sides bodies
/// slide down, blocks that hold only bodies above them. The game inserts it (any time: the
/// next fixed step reads a change) and [`PhysicsPlugin`] puts the surfaces in
/// [`PhysicsContent::collision`] for every state of each block. A name with properties
/// (`game:bubble_column[drag=true]`, ADR 0057) sets the states with those values only, over the
/// block's own surface; names set later win. A block not named here is plain. Removing the
/// resource leaves the surfaces as they were: insert an empty one instead.
///
/// ```ignore
/// let mut surfaces = BlockSurfaces::default();
/// surfaces.set("game:ice", Surface { friction: -9.81, ..Surface::PLAIN });
/// app.insert_resource(surfaces);
/// ```
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct BlockSurfaces {
    blocks: Vec<(String, Surface)>,
}

impl BlockSurfaces {
    /// Gives the block named `block` (`"game:ice"`) its surface, replacing one set before; with
    /// properties (`"game:bubble_column[drag=true]"`), the states with those values.
    pub fn set(&mut self, block: impl Into<String>, surface: Surface) -> &mut Self {
        let block = block.into();
        match self.blocks.iter_mut().find(|(name, _)| *name == block) {
            Some(entry) => entry.1 = surface,
            None => self.blocks.push((block, surface)),
        }
        self
    }

    /// The surface set for the block named `block`.
    pub fn get(&self, block: &str) -> Option<&Surface> {
        self.blocks
            .iter()
            .find(|(name, _)| name == block)
            .map(|(_, surface)| surface)
    }

    /// Every block and its surface, in the order they were first set.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Surface)> {
        self.blocks
            .iter()
            .map(|(name, surface)| (name.as_str(), surface))
    }

    /// Puts the surfaces in `table`, built from `registry` ([`CollisionTable::set_surfaces`]),
    /// and returns the names `registry` does not have (they are left out), properties included
    /// (an unknown property or value).
    pub fn apply(&self, registry: &BlockRegistry, table: &mut CollisionTable) -> Vec<String> {
        let mut unknown = Vec::new();
        let mut by_block: Vec<Option<Surface>> = Vec::new();
        let mut by_state: Vec<(Vec<BlockState>, Surface)> = Vec::new();
        for (name, surface) in &self.blocks {
            let (block_name, pairs) = match name.split_once('[') {
                Some((block, rest)) => (block, rest.strip_suffix(']')),
                None => (name.as_str(), None),
            };
            let Some(block) = registry.block_by_name(block_name) else {
                unknown.push(name.clone());
                continue;
            };
            if let Some(pairs) = pairs {
                match states_with(registry, block, pairs) {
                    Some(states) => by_state.push((states, *surface)),
                    None => unknown.push(name.clone()),
                }
                continue;
            }
            if name.contains(']') {
                unknown.push(name.clone());
                continue;
            }
            let id = usize::from(block.0);
            if by_block.len() <= id {
                by_block.resize(id + 1, None);
            }
            by_block[id] = Some(*surface);
        }
        table.set_surfaces(|state| {
            let of_state = by_state
                .iter()
                .rev()
                .find(|(states, _)| states.contains(&state))
                .map(|(_, s)| *s);
            of_state.or_else(|| {
                registry
                    .block(state)
                    .and_then(|BlockId(id)| by_block.get(usize::from(id)).copied().flatten())
            })
        });
        unknown
    }
}

/// The states of `block` whose properties have the values `pairs` names (`"a=x,b=y"`); `None`
/// if it names a property the block lacks, a value it lacks, or nothing.
fn states_with(registry: &BlockRegistry, block: BlockId, pairs: &str) -> Option<Vec<BlockState>> {
    let pairs: Vec<(&str, &str)> = pairs
        .split(',')
        .map(|pair| pair.split_once('='))
        .collect::<Option<_>>()?;
    let properties = registry.properties(block);
    for (property, value) in &pairs {
        let known = properties.iter().find(|p| p.name() == *property)?;
        if !known.values().iter().any(|v| v == value) {
            return None;
        }
    }
    Some(
        registry
            .states(block)
            .filter(|&state| {
                pairs
                    .iter()
                    .all(|(p, v)| registry.property(state, p) == Some(*v))
            })
            .collect(),
    )
}

/// [`PhysicsSystems::Prepare`], first: a new or changed [`BlockSurfaces`] goes into the
/// collision table. A name the registry does not have is a warning.
pub(crate) fn apply_surfaces(
    surfaces: Option<Res<BlockSurfaces>>,
    world: Res<WorldContent>,
    mut content: ResMut<PhysicsContent>,
) {
    let Some(surfaces) = surfaces else {
        return;
    };
    if !surfaces.is_changed() {
        return;
    }
    let table = Arc::make_mut(&mut content.collision);
    for name in surfaces.apply(&world.registry, table) {
        tracing::warn!(
            block = name,
            "BlockSurfaces names a block the registry does not have"
        );
    }
}

/// Adds physics to an app with a voxel world (`WorldPlugin`) and time (`TimePlugin`). Not part
/// of `DefaultPlugins`: games add it.
///
/// - Bodies ([`crate::RigidBody`] and the other components) move in fixed steps, in
///   `FixedPostUpdate`, in the chained [`PhysicsSystems`]; interpolated `Transform`s are written
///   after the frame's steps (`RunFixedMainLoopSystems::AfterFixedMainLoop`).
/// - Block surfaces (ADR 0047): a [`BlockSurfaces`] the game inserts goes into the collision
///   table before the next step.
/// - Resources: [`PhysicsSettings`], [`PhysicsStats`] (a `"physics"` line on `DiagnosticsBoard`,
///   refreshed in `Last`), [`BodyIndex`] (the bodies for entity queries) and, in
///   `Plugin::finish`, [`PhysicsContent`] from the world's registry, which
///   [`crate::PhysicsQuery`] and the step read.
/// - Messages: [`VoxelImpact`], [`CollisionStarted`], [`CollisionEnded`] and
///   [`ProjectileHit`]. It reads the world's `BlockChanged` and `ChunkDataEvent` to wake
///   sleeping bodies.
/// - It registers [`BodyCodec`] (`boxel_world::persist::add_entity_codec`): with the world's
///   `PersistPlugin`, bodies with `boxel_world::persist::Saved` are saved with their chunk and
///   come back as they were (ADR 0024).
/// - In `Plugin::finish` it sets `FixedTime::set_max_steps_per_frame` to
///   [`PhysicsPlugin::max_steps_per_frame`] (3 by default): after a hitch, a frame runs at most
///   that many fixed steps, which bounds the cost of a catch-up frame. The cap is app-wide, see
///   the field.
/// - With [`PhysicsPlugin::single_threaded`] (the default) `FixedPostUpdate` and
///   `RunFixedMainLoop` run on the single-threaded executor: the physics systems are a chain,
///   so threads only add overhead.
///
/// Runners call `App::finish` before the first update; a test that drives `App::update` by hand
/// calls it too.
///
/// # Panics
///
/// In `finish`, if the app has no `WorldContent` (add `WorldPlugin`).
#[derive(Clone, Copy, Debug)]
pub struct PhysicsPlugin {
    /// The most fixed steps a frame runs (`FixedTime::set_max_steps_per_frame`), set in
    /// `Plugin::finish`. `None` leaves `FixedTime` as it is (the engine default is 8). 0 counts
    /// as 1.
    ///
    /// The cap is for the whole app, not only physics: every `FixedMain` schedule
    /// (`FixedUpdate` too) runs at most this many times a frame, and the time left over is
    /// dropped (an overrun, logged at most every 5 s). So it is also a frame rate floor: below
    /// `steps * rate` fps (21.3 fps with 3 steps at 64 Hz, 42.7 fps at 128 Hz) the fixed-step
    /// game runs in slow motion. A game with few bodies that must keep real time on slow
    /// machines can pass `None`. `finish` overrides a cap set earlier (in another plugin's
    /// `build`); change it after `App::finish` to keep your own.
    pub max_steps_per_frame: Option<u32>,
    /// Run `FixedPostUpdate` (the physics step and any other system there) and
    /// `RunFixedMainLoop` (input sampling, the fixed loop and the interpolation, every frame) on
    /// the single-threaded executor. The physics systems are chained, so the multi-threaded
    /// executor gains nothing and costs about 24 us per run on its own (measured on
    /// `FixedPostUpdate`: an empty step takes 0.65 us instead of 25 us, and 1000 bodies 0.22 ms
    /// instead of 0.32 ms). `RunFixedMainLoop` is a chain too (before the loop, the exclusive
    /// loop, after it). `false` leaves both executors alone.
    pub single_threaded: bool,
}

impl Default for PhysicsPlugin {
    /// At most 3 fixed steps per frame, as the physics plan recommends, and a single-threaded
    /// `FixedPostUpdate`.
    fn default() -> Self {
        Self {
            max_steps_per_frame: Some(3),
            single_threaded: true,
        }
    }
}

impl Plugin for PhysicsPlugin {
    fn build(&self, app: &mut App) {
        if self.single_threaded {
            let single = |schedule: &mut Schedule| {
                schedule.set_executor(SingleThreadedExecutor::new());
            };
            app.edit_schedule(FixedPostUpdate, single)
                .edit_schedule(RunFixedMainLoop, single);
        }
        boxel_world::persist::add_entity_codec(app, BodyCodec);
        app.init_resource::<PhysicsSettings>()
            .init_resource::<PhysicsStats>()
            .init_resource::<PendingImpacts>()
            .init_resource::<ObstacleIndex>()
            .init_resource::<BodyIndex>()
            .init_resource::<ContactSet>()
            .init_resource::<SleepLog>()
            .init_resource::<Vacated>()
            .init_resource::<Disturbed>()
            .init_resource::<AwakeCount>()
            .init_resource::<WritebackTick>()
            .init_resource::<PendingHits>()
            .add_message::<VoxelImpact>()
            .add_message::<CollisionStarted>()
            .add_message::<CollisionEnded>()
            .add_message::<ProjectileHit>()
            // The world's (also added by `WorldPlugin`, in any order).
            .add_message::<BlockChanged>()
            .add_message::<ChunkDataEvent>()
            .configure_sets(
                FixedPostUpdate,
                (
                    PhysicsSystems::Prepare,
                    PhysicsSystems::Controller,
                    PhysicsSystems::Wake,
                    PhysicsSystems::Kinematic,
                    PhysicsSystems::Step,
                    PhysicsSystems::Broadphase,
                    PhysicsSystems::Solve,
                    PhysicsSystems::Projectiles,
                    PhysicsSystems::Sleep,
                    PhysicsSystems::Events,
                    PhysicsSystems::Writeback,
                )
                    .chain(),
            )
            .add_systems(
                FixedPostUpdate,
                (
                    (apply_surfaces, prepare_bodies, wake_to_pose)
                        .chain()
                        .in_set(PhysicsSystems::Prepare),
                    drive_characters.in_set(PhysicsSystems::Controller),
                    wake_bodies.in_set(PhysicsSystems::Wake),
                    (move_kinematic, build_obstacles)
                        .chain()
                        .in_set(PhysicsSystems::Kinematic),
                    step_dynamic.in_set(PhysicsSystems::Step),
                    broadphase.in_set(PhysicsSystems::Broadphase),
                    solve.in_set(PhysicsSystems::Solve),
                    step_projectiles.in_set(PhysicsSystems::Projectiles),
                    sleep_bodies.in_set(PhysicsSystems::Sleep),
                    (write_events, write_collisions, write_hits)
                        .chain()
                        .in_set(PhysicsSystems::Events),
                    write_back.in_set(PhysicsSystems::Writeback),
                ),
            )
            .configure_sets(
                RunFixedMainLoop,
                PhysicsSystems::Interpolate.in_set(RunFixedMainLoopSystems::AfterFixedMainLoop),
            )
            .add_systems(
                RunFixedMainLoop,
                interpolate.in_set(PhysicsSystems::Interpolate),
            )
            // It writes only its own line of the board: its order against the other board
            // writers in `Last` (the world's stats) does not matter.
            .add_systems(Last, publish_stats.ambiguous_with_all());
        crate::reset::add_reset(app);
    }

    fn finish(&self, app: &mut App) {
        let Some(content) = app.world().get_resource::<WorldContent>() else {
            panic!("PhysicsPlugin needs the voxel world: add WorldPlugin (no WorldContent found)");
        };
        let collision = Arc::new(CollisionTable::from_registry(&content.registry));
        app.insert_resource(PhysicsContent { collision });
        if let Some(steps) = self.max_steps_per_frame
            && let Some(mut fixed) = app.world_mut().get_resource_mut::<FixedTime>()
        {
            fixed.set_max_steps_per_frame(steps.max(1));
        }
    }
}
