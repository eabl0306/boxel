//! Swimming (ADR 0021): a [`Swimmer`] body whose box touches a fluid (water) moves as in
//! Minecraft's water: weaker gravity, strong drag in every direction (it sinks slowly and stops
//! soon), slower walking, and it rises while the game holds [`SwimIntent::up`] (or presses
//! jump). Opt in with [`SwimPlugin`].
//!
//! It works around the body step instead of inside it: before the controllers of each fixed step
//! ([`PhysicsSystems::Controller`]), a body entering a fluid keeps its land [`BodySettings`] and
//! [`CharacterController::walk_speed`] and gets the water ones; leaving, it gets them back. So
//! while a body is in water, changes to those two are undone when it leaves.
use crate::components::{
    BodyPosition, CharacterController, Collider, Frozen, MoveIntent, Velocity,
};
use crate::core::{BodySettings, CollisionTable, Probe, VoxelSource};
use crate::plugin::PhysicsContent;
use crate::source::ChunkSource;
use crate::systems::{PhysicsSystems, drive_characters};
use boxel_app::{App, FixedPostUpdate, Plugin};
use boxel_ecs::prelude::*;
use boxel_math::DVec3;
use boxel_voxel::{BlockPos, WorldPos};
use boxel_world::{ChunkMap, WorldSettings};

/// How a [`Swimmer`] moves in a fluid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SwimSettings {
    /// Multiplies the body's land `gravity_scale` in a fluid.
    pub gravity_scale: f64,
    /// Drag in every direction in a fluid, per second (it replaces the land drags; with the
    /// gravity it gives the sinking speed `gravity * gravity_scale / drag`).
    pub drag: f64,
    /// Multiplies the walking speed in a fluid.
    pub speed: f64,
    /// Upward speed while swimming up, blocks per second.
    pub up_speed: f64,
}

impl Default for SwimSettings {
    /// About Minecraft's water: a quarter of the gravity, drag 3 /s (a player sinks at about
    /// 2.7 blocks/s with gravity 32), half the walking speed, rising at 3 blocks/s.
    fn default() -> Self {
        Self {
            gravity_scale: 0.25,
            drag: 3.0,
            speed: 0.5,
            up_speed: 3.0,
        }
    }
}

/// A body that swims in fluids (see [`SwimPlugin`]). Needs a [`BodyPosition`] (any
/// dynamic body); a [`Collider`] gives its box (a point without one).
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct Swimmer {
    /// How it moves in a fluid.
    pub settings: SwimSettings,
    /// Out: its box touched a fluid at the last fixed step.
    pub in_fluid: bool,
    /// The land settings and walking speed, kept while in a fluid.
    pub(crate) land: Option<(BodySettings, f64)>,
}

impl Swimmer {
    /// A swimmer with these settings.
    pub fn new(settings: SwimSettings) -> Self {
        Self {
            settings,
            ..Self::default()
        }
    }
}

/// The game's swim input: hold `up` to rise in a fluid (Minecraft's Space). Sample it with the
/// rest of the input; it is read every fixed step and never cleared.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SwimIntent {
    /// Swim up.
    pub up: bool,
}

/// Adds swimming (ADR 0021): [`swim_bodies`] in each fixed step, before the character
/// controllers. Needs [`PhysicsPlugin`](crate::PhysicsPlugin) first.
///
/// A [`Swimmer`] whose box touches a fluid moves as in Minecraft's water: weaker gravity, strong
/// drag in every direction, slower walking, and it rises while [`SwimIntent::up`] is held or a
/// jump is pressed. Entering a fluid it keeps its land `BodySettings` and walking speed and gets
/// the water ones; leaving, it gets them back (so changes to those two made while it is in water
/// are undone when it leaves).
pub struct SwimPlugin;

impl Plugin for SwimPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            FixedPostUpdate,
            swim_bodies
                .in_set(PhysicsSystems::Controller)
                .before(drive_characters),
        );
    }
}

/// True if the box from `min` to `max` (exclusive faces) touches a fluid cell. A cell not loaded
/// counts as dry.
pub fn box_touches_fluid(
    source: &mut impl VoxelSource,
    table: &CollisionTable,
    min: WorldPos,
    max: WorldPos,
) -> bool {
    // The last cell the box reaches: a max face on a cell border does not enter the next cell.
    let last = |block: i64, frac: f64| if frac == 0.0 { block - 1 } else { block };
    let hi = BlockPos::new(
        last(max.block.x, max.frac.x),
        last(max.block.y, max.frac.y),
        last(max.block.z, max.frac.z),
    );
    for y in min.block.y..=hi.y {
        for z in min.block.z..=hi.z {
            for x in min.block.x..=hi.x {
                if let Probe::Block(state) = source.probe(BlockPos::new(x, y, z))
                    && table.is_fluid(state)
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Each fixed step: every awake [`Swimmer`] checks its box against the fluids, swaps its land and
/// water settings when that changes and, in a fluid, rises while [`SwimIntent::up`] is held or
/// a jump was pressed (the jump latch is used up: no jump off the bottom).
pub fn swim_bodies(
    content: Res<PhysicsContent>,
    map: Res<ChunkMap>,
    world: Res<WorldSettings>,
    mut bodies: Query<
        (
            &mut Swimmer,
            &BodyPosition,
            Option<&Collider>,
            &mut BodySettings,
            &mut Velocity,
            Option<&mut CharacterController>,
            Option<&mut MoveIntent>,
            Option<&SwimIntent>,
        ),
        Without<Frozen>,
    >,
) {
    let mut source = ChunkSource::new(&map, &world);
    for (mut swimmer, position, collider, mut body, mut velocity, controller, intent, up) in
        &mut bodies
    {
        let (min, max) = match collider {
            Some(c) => {
                let b = c.shape.bounds();
                (c.offset + b.min, c.offset + b.max)
            }
            None => (DVec3::ZERO, DVec3::ZERO),
        };
        let pos = position.0;
        let wet = box_touches_fluid(
            &mut source,
            &content.collision,
            pos.offset(min),
            pos.offset(max),
        );
        let mut controller = controller;
        if wet && swimmer.land.is_none() {
            let walk = controller.as_ref().map_or(0.0, |c| c.walk_speed);
            swimmer.land = Some((*body, walk));
            let s = swimmer.settings;
            *body = BodySettings {
                gravity_scale: body.gravity_scale * s.gravity_scale,
                drag_xz: s.drag,
                drag_y: s.drag,
                ..*body
            };
            if let Some(c) = controller.as_mut() {
                c.walk_speed = walk * s.speed;
            }
        } else if !wet && let Some((land, walk)) = swimmer.land.take() {
            *body = land;
            if let Some(c) = controller.as_mut() {
                c.walk_speed = walk;
            }
        }
        if swimmer.in_fluid != wet {
            swimmer.in_fluid = wet;
        }
        if !wet {
            continue;
        }
        let mut rise = up.is_some_and(|u| u.up);
        if let Some(mut intent) = intent
            && intent.jump
        {
            intent.jump = false;
            rise = true;
        }
        let up_speed = swimmer.settings.up_speed;
        if rise && velocity.0.y < up_speed {
            velocity.0.y = up_speed;
        }
    }
}
