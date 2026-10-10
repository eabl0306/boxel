//! Swimming (ADR 0021): a [`Swimmer`] body whose box touches a fluid (water, or a block with
//! water in it, ADR 0025) moves as in Minecraft's water: weaker gravity, strong drag in every
//! direction (it sinks slowly and stops soon), slower walking, and it rises while the game holds
//! [`SwimIntent::up`] (or presses jump; on the ground in shallow water, as on a waterlogged slab,
//! a jump is a jump). Opt in with [`SwimPlugin`]. A flying character
//! ([`CharacterController::flying`]) is not affected by the fluid, as Minecraft's creative
//! flight (ADR 0026).
//!
//! It works around the body step instead of inside it: before the controllers of each fixed step
//! ([`PhysicsSystems::Controller`]), a body entering a fluid keeps its land [`BodySettings`] and
//! [`CharacterController::walk_speed`] and gets the water ones; leaving, it gets them back. So
//! while a body is in water, changes to those two are undone when it leaves.
use crate::components::{
    BodyPosition, CharacterController, Collider, Contacts, Frozen, MoveIntent, Velocity,
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
/// A [`Swimmer`] whose box touches a fluid (a fluid block or a waterlogged one,
/// [`CollisionTable::is_wet`]) moves as in Minecraft's water: weaker gravity, strong drag in
/// every direction, slower walking, and it rises while [`SwimIntent::up`] is held or a jump is
/// pressed, except on the ground with the fluid at most [`FLUID_JUMP_THRESHOLD`] above its feet
/// (a waterlogged slab or stair), where the controller jumps as on land. Entering a fluid it
/// keeps its land `BodySettings` and walking speed and gets the
/// water ones; leaving, it gets them back (so changes to those two made while it is in water
/// are undone when it leaves). A flying character ([`CharacterController::flying`]) is not
/// affected by the fluid, as in Minecraft's creative flight: it keeps its land settings, and
/// its jump is not taken for a swim (the controller's flight goes on as in the air).
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

/// Minecraft's `getFluidJumpThreshold`, blocks: a character on the ground whose fluid reaches at
/// most this far above its feet jumps instead of swimming up (on a waterlogged slab or a stair's
/// lower step the water reaches 0.39 above the feet).
pub const FLUID_JUMP_THRESHOLD: f64 = 0.4;

/// How high in its cell a fluid reaches without the same fluid above it, blocks: a source's 8/9
/// (Minecraft's `getOwnHeight`). Physics does not read the levels of flowing water, so every
/// fluid cell counts as a source. With a wet cell above, the fluid fills the cell.
const FLUID_TOP: f64 = 8.0 / 9.0;

/// The last cell a box whose max corner is `max` reaches: a max face on a cell border does not
/// enter the next cell.
fn last_cell(max: WorldPos) -> BlockPos {
    let last = |block: i64, frac: f64| if frac == 0.0 { block - 1 } else { block };
    BlockPos::new(
        last(max.block.x, max.frac.x),
        last(max.block.y, max.frac.y),
        last(max.block.z, max.frac.z),
    )
}

/// True if the box from `min` to `max` (exclusive faces) touches a cell that holds a fluid: a
/// fluid block or a waterlogged one ([`CollisionTable::is_wet`]). A cell not loaded counts as
/// dry.
pub fn box_touches_fluid(
    source: &mut impl VoxelSource,
    table: &CollisionTable,
    min: WorldPos,
    max: WorldPos,
) -> bool {
    let hi = last_cell(max);
    for y in min.block.y..=hi.y {
        for z in min.block.z..=hi.z {
            for x in min.block.x..=hi.x {
                if let Probe::Block(state) = source.probe(BlockPos::new(x, y, z))
                    && table.is_wet(state)
                {
                    return true;
                }
            }
        }
    }
    false
}

/// How far above the bottom of the box from `min` to `max` the fluid of the cells it touches
/// reaches, blocks (Minecraft's `getFluidHeight`): a wet cell's fluid reaches [`FLUID_TOP`] of
/// it, or its top with a wet cell above. 0 when no cell is wet; a cell not loaded counts as dry.
/// Exact far from the origin: whole blocks are counted from the box's bottom cell.
fn fluid_depth(
    source: &mut impl VoxelSource,
    table: &CollisionTable,
    min: WorldPos,
    max: WorldPos,
) -> f64 {
    let mut wet = |x: i64, y: i64, z: i64| match source.probe(BlockPos::new(x, y, z)) {
        Probe::Block(state) => table.is_wet(state),
        _ => false,
    };
    let hi = last_cell(max);
    let mut depth = 0.0f64;
    for y in min.block.y..=hi.y {
        for z in min.block.z..=hi.z {
            for x in min.block.x..=hi.x {
                if wet(x, y, z) {
                    let top = if wet(x, y + 1, z) { 1.0 } else { FLUID_TOP };
                    depth = depth.max((y - min.block.y) as f64 + top - min.frac.y);
                }
            }
        }
    }
    depth
}

/// Each fixed step: every awake [`Swimmer`] checks its box against the fluids, swaps its land and
/// water settings when that changes and, in a fluid, rises while [`SwimIntent::up`] is held or
/// a jump was pressed (the jump latch is used up: no jump off the bottom). On the ground with
/// the fluid at most [`FLUID_JUMP_THRESHOLD`] above its feet (a waterlogged slab or stair) it
/// does not swim up: the jump latch stays for the controller, which jumps as on land
/// (Minecraft's `jumpFromGround` there). A flyer counts as dry for all of that (it keeps the
/// jump latch), though [`Swimmer::in_fluid`] still tells.
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
            Option<&Contacts>,
        ),
        Without<Frozen>,
    >,
) {
    let mut source = ChunkSource::new(&map, &world);
    for (
        mut swimmer,
        position,
        collider,
        mut body,
        mut velocity,
        controller,
        intent,
        up,
        contacts,
    ) in &mut bodies
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
        // Minecraft's creative flight is not affected by fluids (`isAffectedByFluids`).
        let swims = wet && !controller.as_ref().is_some_and(|c| c.flying);
        if swims && swimmer.land.is_none() {
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
        } else if !swims && let Some((land, walk)) = swimmer.land.take() {
            *body = land;
            if let Some(c) = controller.as_mut() {
                c.walk_speed = walk;
            }
        }
        if swimmer.in_fluid != wet {
            swimmer.in_fluid = wet;
        }
        if !swims {
            continue;
        }
        let jump = intent.as_ref().is_some_and(|i| i.jump);
        let held = up.is_some_and(|u| u.up);
        // Shallow fluid under a body on the ground: the jump key jumps (Minecraft's
        // `LivingEntity::aiStep`). Only looked at when a key is down.
        if (jump || held)
            && contacts.is_some_and(|c| c.on_ground)
            && fluid_depth(
                &mut source,
                &content.collision,
                pos.offset(min),
                pos.offset(max),
            ) <= FLUID_JUMP_THRESHOLD
        {
            continue;
        }
        let mut rise = held;
        if jump && let Some(mut intent) = intent {
            intent.jump = false;
            rise = true;
        }
        let up_speed = swimmer.settings.up_speed;
        if rise && velocity.0.y < up_speed {
            velocity.0.y = up_speed;
        }
    }
}
