//! Components, messages and resources of physics bodies (phases P2 to P4 of ADR 0016).

use crate::core::{
    BodySettings, CollisionLayers, DEFAULT_GRID_SHIFT, LocalBox, Shape, StepParams, SweepConfig,
    SweepOut,
};
use boxel_ecs::component::{Mutable, StorageType};
use boxel_ecs::lifecycle::HookContext;
use boxel_ecs::prelude::*;
use boxel_ecs::world::DeferredWorld;
use boxel_math::{DVec3, Vec2};
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, BlockState, Face, WorldPos};
use std::time::Duration;

/// A physics body. Spawn it with a `Transform` (its position) and usually a [`Collider`]:
///
/// ```ignore
/// commands.spawn((
///     RigidBody::Dynamic,
///     Collider::standing(0.6, 1.8),
///     Transform::from_xyz(0.5, 80.0, 0.5),
/// ));
/// ```
///
/// Kinds:
/// - [`RigidBody::Dynamic`]: gravity, drag and collisions with the voxels move it; gameplay
///   steers it through [`Velocity`]. Solid dynamic bodies push each other apart and stand on
///   each other.
/// - [`RigidBody::Kinematic`]: moved only by its [`Velocity`], through the voxels. With a solid
///   collider it is an obstacle for dynamic bodies and pushes them out of its way.
/// - [`RigidBody::Static`]: never moved by physics. With a solid collider it is an obstacle.
///
/// It requires (and so adds with defaults) `Transform`, [`Velocity`], [`BodyPosition`],
/// [`Contacts`], [`BodySettings`] and [`PhysicsSync`]. A body without a [`Collider`] collides
/// with the voxels as a point at its position and takes no part in collisions between bodies.
///
/// Position: [`BodyPosition`] is the authoritative position, exact at any distance (a
/// [`WorldPos`]); `Transform` is a view of it that physics writes after every step (or the
/// interpolated position, with [`Interpolated`]). Writing `Transform` counts as a teleport: the
/// next step starts from the written value. Past `2^53` blocks, where an `f64` cannot hold a
/// position exactly, teleport with [`Teleport`] instead. A teleport keeps the velocity and
/// clears [`Contacts`] (the body is not on its old ground any more).
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[require(Transform, Velocity, BodyPosition, Contacts, BodySettings, PhysicsSync)]
pub enum RigidBody {
    /// Moved by gravity, drag and collisions.
    #[default]
    Dynamic,
    /// Moved only by its velocity.
    Kinematic,
    /// Never moved by physics.
    Static,
}

/// How a body's shape reacts to other bodies. Every collider of a dynamic body collides with
/// the voxels, sensors included (a dropped item is a dynamic sensor: it lands on blocks and only
/// reports when a player touches it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Response {
    /// Solid: two solid bodies never stay overlapped. Dynamic ones are pushed apart along the
    /// shortest way out, on every axis (one stands on another), and a dynamic body never
    /// enters a static or kinematic one. Add [`ReportContacts`] to get events for its contacts.
    #[default]
    Solid,
    /// Never pushes and is never pushed; overlaps with other colliders are reported as
    /// [`CollisionStarted`] and [`CollisionEnded`].
    Sensor,
}

/// The shape of a body, placed at `offset` from its position. Against the voxels (and as an
/// obstacle) a sphere collides as its bounding box (v1); between bodies spheres are exact.
///
/// A collider without a [`RigidBody`] makes a [`RigidBody::Static`] body (it requires one): a
/// wall, a trigger zone.
///
/// It is immutable: to change a body's collider, insert a new one. Physics then sees the one it
/// replaces, so the bodies asleep on the old shape wake up (a platform that shrinks or turns
/// into a sensor drops what rests on it).
#[derive(Component, Clone, Copy, Debug, PartialEq)]
#[require(RigidBody = RigidBody::Static)]
#[component(immutable, on_discard = collider_replaced)]
pub struct Collider {
    /// The shape, centered on its own origin.
    pub shape: Shape,
    /// Where the shape's center is, relative to the body's position.
    pub offset: DVec3,
    /// How it reacts to other bodies.
    pub response: Response,
    /// Which bodies it meets (and which entity rays and queries find it).
    pub layers: CollisionLayers,
}

impl Collider {
    /// A box with these half extents, centered on the body's position, solid.
    pub fn aabb(half: DVec3) -> Self {
        Self {
            shape: Shape::Aabb { half },
            offset: DVec3::ZERO,
            response: Response::Solid,
            layers: CollisionLayers::default(),
        }
    }

    /// A sphere centered on the body's position, solid.
    pub fn sphere(radius: f64) -> Self {
        Self {
            shape: Shape::Sphere { radius },
            ..Self::aabb(DVec3::ZERO)
        }
    }

    /// A box `width x height x width` standing on the body's position (the position is at the
    /// center of its bottom face, like a player's feet): a player is `standing(0.6, 1.8)`.
    pub fn standing(width: f64, height: f64) -> Self {
        Self::aabb(DVec3::new(width * 0.5, height * 0.5, width * 0.5)).with_offset(DVec3::new(
            0.0,
            height * 0.5,
            0.0,
        ))
    }

    /// The same collider with its center at `offset` from the body's position.
    pub fn with_offset(self, offset: DVec3) -> Self {
        Self { offset, ..self }
    }

    /// The same collider as a sensor.
    pub fn sensor(self) -> Self {
        Self {
            response: Response::Sensor,
            ..self
        }
    }

    /// The same collider in other layers.
    pub fn with_layers(self, layers: CollisionLayers) -> Self {
        Self { layers, ..self }
    }

    /// The box this collider uses against the voxels, relative to the body's position: the
    /// shape's bounding box moved by `offset`. Static and kinematic obstacles block dynamic
    /// bodies with this box too.
    pub fn voxel_box(&self) -> LocalBox {
        self.shape.bounds().translate(self.offset)
    }

    /// It is a sensor.
    pub fn is_sensor(&self) -> bool {
        self.response == Response::Sensor
    }
}

/// [`Collider`]'s discard hook (a new collider replaces it, it is removed, or a despawn): the
/// place a solid collider leaves is recorded, so the bodies asleep on it wake up, and a
/// sleeping body whose collider goes wakes up itself (see [`Sleeping`]). A body that never
/// stepped has no place yet.
fn collider_replaced(mut world: DeferredWorld, context: HookContext) {
    let entity = context.entity;
    if !placed(&world, entity) {
        return;
    }
    disturb(&mut world, entity);
    let (Some(collider), Some(position)) = (
        world.get::<Collider>(entity).copied(),
        world.get::<BodyPosition>(entity).copied(),
    ) else {
        return;
    };
    if collider.is_sensor() || world.get::<Projectile>(entity).is_some() {
        // Nothing rests on a sensor or a projectile.
        return;
    }
    if let Some(mut vacated) = world.get_resource_mut::<Vacated>() {
        vacated.0.push((position.0, collider.voxel_box()));
    }
}

/// A step placed the body (it read its `Transform` or [`Teleport`]): before that its
/// [`BodyPosition`] is only the default.
fn placed(world: &DeferredWorld, entity: Entity) -> bool {
    world
        .get::<PhysicsSync>(entity)
        .is_some_and(|sync| sync.placed)
}

/// A sleeping body changed in a way the wake rules cannot see from its components (its
/// collider or [`ReportContacts`] went): it wakes up in the next step.
fn disturb(world: &mut DeferredWorld, entity: Entity) {
    if world.get::<Sleeping>(entity).is_some()
        && let Some(mut disturbed) = world.get_resource_mut::<Disturbed>()
    {
        disturbed.0.push(entity);
    }
}

/// Places that solid bodies left since the latest step (a teleport, a `BodyPosition` write, a
/// changed or removed collider, a despawn): the sleeping bodies near them wake up in the next
/// step.
#[derive(Resource, Default)]
pub(crate) struct Vacated(pub(crate) Vec<(WorldPos, LocalBox)>);

/// Sleeping bodies whose collider or [`ReportContacts`] was removed or replaced since the latest
/// step: they wake up in the next one.
#[derive(Resource, Default)]
pub(crate) struct Disturbed(pub(crate) Vec<Entity>);

/// Marker: contacts of this solid body with other solid bodies are reported as
/// [`CollisionStarted`] and [`CollisionEnded`] too (sensor overlaps always are). A contact lasts
/// while the two touch or overlap at the end of a step: a crate resting on another, a player
/// standing on a static platform or pressed against a mob or a wall body. Boxes touch face to
/// face, within `PhysicsSettings::eps` (grazing an edge does not count); a static or kinematic
/// body counts as its box ([`Collider::voxel_box`]), as the sweeps of dynamic bodies see it.
///
/// Adding or removing it on a sleeping body wakes the body, so its contacts are tested again.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[component(on_add = report_changed, on_remove = report_changed)]
pub struct ReportContacts;

/// [`ReportContacts`]' add and remove hook: a sleeping body wakes up (see [`disturb`]).
fn report_changed(mut world: DeferredWorld, context: HookContext) {
    if placed(&world, context.entity) {
        disturb(&mut world, context.entity);
    }
}

/// Two colliders started to overlap in the latest fixed step (a sensor pair, or a solid pair
/// with [`ReportContacts`]). `a < b` by `Entity::to_bits`; each pair is reported once until it
/// ends. Written sorted at the end of each step, like [`VoxelImpact`].
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CollisionStarted {
    /// The entity with the lower bits.
    pub a: Entity,
    /// The entity with the higher bits.
    pub b: Entity,
}

/// Two colliders stopped overlapping in the latest fixed step (they moved apart, one was
/// despawned or lost its collider, or their layers or responses changed). Sent once for every
/// [`CollisionStarted`].
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CollisionEnded {
    /// The entity with the lower bits.
    pub a: Entity,
    /// The entity with the higher bits.
    pub b: Entity,
}

impl CollisionStarted {
    /// The other entity of the pair, if `entity` is in it.
    pub fn other(&self, entity: Entity) -> Option<Entity> {
        other_of(self.a, self.b, entity)
    }
}

impl CollisionEnded {
    /// The other entity of the pair, if `entity` is in it.
    pub fn other(&self, entity: Entity) -> Option<Entity> {
        other_of(self.a, self.b, entity)
    }
}

fn other_of(a: Entity, b: Entity, entity: Entity) -> Option<Entity> {
    if entity == a {
        Some(b)
    } else if entity == b {
        Some(a)
    } else {
        None
    }
}

/// Velocity in blocks per second. Gameplay writes it to steer a body (a jump sets `y`, a
/// knockback adds to it); physics updates it every step (gravity, drag, clips and bounces).
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct Velocity(pub DVec3);

/// The authoritative position of a body, exact at any distance (see [`RigidBody`]). Read it in
/// fixed steps; `Transform` is a view of it (interpolated with [`Interpolated`]).
///
/// To move a body somewhere else, use [`Teleport`] or write `Transform` (a teleport). A direct
/// write is kept too, but it is a nudge, not a teleport: the body moves exactly there and keeps
/// its velocity and [`Contacts`], and `Transform` follows at the end of the step. Like a
/// teleport, it wakes a sleeping body and the bodies asleep on the place it left, and a nudged
/// static or kinematic body pushes the bodies it lands on. Do it in a fixed step
/// (`FixedUpdate`); written in `Update`, the interpolated view of a frame without a step would
/// slide toward it. If `Transform` was also written since the last step, `Transform` wins.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct BodyPosition(pub WorldPos);

// `BodySettings` lives in the pure core (the integrator reads it); it is a component here.
impl Component for BodySettings {
    const STORAGE_TYPE: StorageType = StorageType::Table;
    type Mutability = Mutable;
}

/// What the latest step of a dynamic body touched, rewritten every step and cleared by a
/// teleport.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Contacts {
    /// Bit `Face::index()` for each face of the body that a clip stopped in the latest step, or
    /// that touched another solid body it was pushed apart from (`Face` order: +Y, -Y, +X, -X,
    /// +Z, -Z).
    pub faces: u8,
    /// It landed: a clip stopped it moving down, or it was pushed up off another solid body (it
    /// stands on a crate or a mob). Ground friction applies in the next step, and a
    /// [`CharacterController`] can jump.
    pub on_ground: bool,
    /// The block it stands on (under the center of its bottom when that block is full).
    pub ground_block: Option<BlockPos>,
    /// It overlaps a fluid block.
    pub in_fluid: bool,
}

impl Contacts {
    /// The contacts a sweep reports.
    pub fn from_sweep(out: &SweepOut) -> Self {
        Self {
            faces: out.faces,
            on_ground: out.on_ground(),
            ground_block: out.ground,
            in_fluid: out.in_fluid,
        }
    }

    /// A clip stopped the body's `face`.
    pub const fn touched(&self, face: Face) -> bool {
        self.faces & (1 << face.index()) != 0
    }
}

/// Marker: the latest step of this body needed a block of an unloaded chunk, so it did not move
/// (position and velocity kept, no gravity). Physics adds it and removes it when the body moves
/// again; a body retries every step (one chunk lookup). Bodies near a `ChunkViewer` never freeze:
/// the world loads data one chunk past the meshes.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Frozen;

/// Marker: this body is asleep. Physics skips it (no gravity, no sweep, no interpolation) until
/// something wakes it, so a pile of resting crates or dropped items costs almost nothing.
///
/// A dynamic body falls asleep after `PhysicsSettings::sleep_steps` steps (32 by default, half
/// a second) in a row resting on something ([`Contacts::on_ground`]: on blocks or on another
/// body), slower than `PhysicsSettings::sleep_speed` on every axis and without being moved by
/// more than that speed allows (a character whose [`MoveIntent`] wants to walk never counts as
/// resting). Its velocity is set to exactly 0 and its `Transform` to its
/// position. A projectile that sticks in a block falls asleep at once.
///
/// It wakes up (physics removes the marker) when:
/// - its [`Velocity`] is set to something other than 0 (a jump, a knockback, a
///   [`CharacterController`] that wants to walk), or its [`RigidBody`], [`Collider`],
///   [`BodySettings`] or [`ReportContacts`] change after it fell asleep;
/// - it is teleported ([`Teleport`] or a `Transform` write) or nudged (a [`BodyPosition`]
///   write);
/// - an awake body pushes it, or a solid body it touches moves (the crate under it is pushed
///   away), is despawned, teleported or nudged away, or loses or changes its collider;
/// - a block within one block of its box changes (`boxel_world::BlockChanged`), or a chunk its
///   box (plus one block) touches loads (`boxel_world::ChunkDataEvent::Loaded`: the blocks may
///   have changed while it was away). An unload leaves it asleep: it does not move, so it needs
///   no blocks;
/// - `PhysicsSettings` change (gravity, sleeping turned off).
///
/// Insert it to put a body to sleep yourself (its velocity becomes 0 then), remove it to wake
/// one. A body spawned with it (a body loaded from a save) stays asleep where its `Transform`
/// or [`Teleport`] puts it. On a kinematic body it stops it; on a static body it changes
/// nothing. A sleeping body still blocks and is found by rays, box queries and projectiles, and
/// its sensor overlaps stay as they were (no `CollisionEnded` while both bodies sleep).
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[component(storage = "SparseSet", on_add = fell_asleep, on_remove = woke_up)]
pub struct Sleeping;

/// [`Sleeping`]'s add hook: a sleeping body does not move, so its velocity becomes exactly 0 and
/// its `Transform` its position (no interpolation left to do). Physics is told (see
/// [`woke_up`]). A body that never stepped (spawned asleep) keeps its `Transform`: the next
/// step places it there.
fn fell_asleep(mut world: DeferredWorld, context: HookContext) {
    let entity = context.entity;
    if let Some(mut velocity) = world.get_mut::<Velocity>(entity)
        && velocity.0 != DVec3::ZERO
    {
        velocity.0 = DVec3::ZERO;
    }
    if placed(&world, entity)
        && let Some(position) = world.get::<BodyPosition>(entity).map(|p| p.0)
    {
        let translation = position.to_dvec3();
        if let Some(mut transform) = world.get_mut::<Transform>(entity)
            && transform.translation != translation
        {
            transform.translation = translation;
        }
        if let Some(mut sync) = world.get_mut::<PhysicsSync>(entity)
            && sync.last_written != Some(translation)
        {
            sync.last_written = Some(translation);
        }
        if let Some(mut interpolated) = world.get_mut::<Interpolated>(entity)
            && interpolated.prev != Some(position)
        {
            interpolated.prev = Some(position);
        }
    }
    woke_up(world, context);
}

/// [`Sleeping`]'s remove hook (and the end of [`fell_asleep`]): physics keeps its sleeping
/// bodies apart (they are not stepped and not gathered every step), so it is told whenever the
/// marker comes or goes, by anyone (despawns included).
fn woke_up(mut world: DeferredWorld, context: HookContext) {
    if let Some(mut log) = world.get_resource_mut::<SleepLog>() {
        log.0.push(context.entity);
    }
}

/// Entities whose [`Sleeping`] marker came or went since physics last looked.
#[derive(Resource, Default)]
pub(crate) struct SleepLog(pub(crate) Vec<Entity>);

/// Opt-in smooth rendering: physics moves bodies at the fixed rate (64 Hz), so at 144 fps a
/// body would move in steps. With this component, after the fixed steps of every frame
/// (`RunFixedMainLoopSystems::AfterFixedMainLoop`) its `Transform` is set between its position
/// before and after the latest step, by `FixedTime::overstep_fraction_f64`. `Update` and the
/// renderer see the interpolated position, up to one step (15.6 ms) behind the simulation;
/// fixed steps read [`BodyPosition`]. A teleport never smears: both ends jump.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct Interpolated {
    pub(crate) prev: Option<WorldPos>,
}

impl Interpolated {
    /// The position before the latest step (`None` before the body's first step).
    pub fn previous(&self) -> Option<WorldPos> {
        self.prev
    }
}

/// Moves a body exactly to this position at the start of the next step, then removes itself.
/// Use it past `2^53` blocks (where writing `Transform` rounds) or to spawn a body at an exact
/// far position. The velocity is kept and [`Contacts`] are cleared.
///
/// Inserting it also sets `Transform` to the position right away (rounded to `f64`), so
/// `Update` and the renderer see the body there before the step runs: a body spawned with
/// only a `Teleport` never shows at the origin.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
#[component(on_insert = show_teleport)]
pub struct Teleport(pub WorldPos);

/// [`Teleport`]'s insert hook: `Transform` shows the target at once (the next step still moves
/// [`BodyPosition`] from the exact `Teleport`).
fn show_teleport(mut world: DeferredWorld, context: HookContext) {
    let Some(target) = world
        .get::<Teleport>(context.entity)
        .map(|t| t.0.to_dvec3())
    else {
        return;
    };
    if let Some(mut transform) = world.get_mut::<Transform>(context.entity)
        && transform.translation != target
    {
        transform.translation = target;
    }
}

/// What physics last wrote to the body's `Transform` (`None` before its first step). Physics
/// compares it with `Transform` to tell a teleport (gameplay wrote `Transform`) from its own
/// write.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct PhysicsSync {
    pub(crate) last_written: Option<DVec3>,
    /// How far the body moved in the latest step before the solve (the sweep or the kinematic
    /// move): the solve uses it to tell which side another body came from.
    pub(crate) moved: DVec3,
    /// How far the body moved in the latest step in all: the sweep (or the kinematic move, or
    /// a projectile's flight) plus the pushes of the solve. Not a teleport.
    pub(crate) net: DVec3,
    /// Steps in a row the body was still (see [`Sleeping`]).
    pub(crate) still: u32,
    /// The latest step placed the body from outside physics: it was new, teleported or nudged
    /// (a [`BodyPosition`] write). Such a body can disturb others in that step.
    pub(crate) teleported: bool,
    /// A step placed the body (it read its `Transform` or [`Teleport`]): before that its
    /// [`BodyPosition`] is only the default.
    pub(crate) placed: bool,
}

impl PhysicsSync {
    /// The translation physics last wrote.
    pub fn last_written(&self) -> Option<DVec3> {
        self.last_written
    }

    /// How far the body moved in the latest step, before the pushes between bodies (zero for
    /// static, frozen and sleeping bodies).
    pub fn moved(&self) -> DVec3 {
        self.moved
    }

    /// How far the body moved in the latest step in all, pushes between bodies included (not
    /// teleports).
    pub fn net_moved(&self) -> DVec3 {
        self.net
    }
}

/// A walking character: turns [`MoveIntent`] into [`Velocity`] every fixed step, before the
/// body moves (`PhysicsSystems::Controller`). For players and mobs alike. Only
/// [`RigidBody::Dynamic`] bodies walk: on the other kinds it does nothing.
///
/// The horizontal acceleration is `walk_speed * k`, with `k` the body's ground friction on the
/// ground and its air drag in the air (times `air_control`), so the top speed with a full wish
/// is `walk_speed` on the ground and `walk_speed * air_control` in the air, for any positive
/// rates in [`BodySettings`]. A rate of 0 means no drag and so no top speed to level off at: the
/// controller does not steer at all there (with `drag_xz = 0`, no air control; with
/// `ground_friction = 0`, no walking). A jump sets the vertical velocity to `jump_speed` when the
/// body is on the ground.
///
/// On the ground, the body steps up ledges up to `step_height` high without jumping (a slab, a
/// stair of slabs; not a full block), as Minecraft's player and mobs do: the step moves it up
/// within one fixed step (see `boxel_physics::move_box_stepping`). The body's position stays
/// exact; a camera that follows it may smooth the rise for the eye.
///
/// Modes (ADR 0026), from [`MoveIntent`] and [`CharacterController::flying`]:
/// - Sneaking ([`MoveIntent::sneak`]): `sneak_factor` of the speed, and on the ground it never
///   walks off a ledge deeper than `step_height` (Minecraft's edge guard,
///   [`crate::guard_edge`]; only on blocks and static or kinematic bodies).
/// - Sprinting ([`MoveIntent::sprint`]): `sprint_factor` of the speed; sneaking wins.
/// - Flying (`flying`, Minecraft's creative flight; the game turns it on): no gravity and no
///   step-up, `fly_speed` across (twice that sprinting) with the air drag, and
///   `fly_vertical_speed` up while [`MoveIntent::up`] is held and down while sneaking, with a
///   strong vertical drag ([`FLIGHT_VERTICAL_DRAG`]: it stops soon). A jump does nothing.
///   Landing on blocks (or a static or kinematic body) turns it off, as in Minecraft.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
#[require(MoveIntent)]
pub struct CharacterController {
    /// Top walking speed, blocks per second (Minecraft: 4.317).
    pub walk_speed: f64,
    /// Steering in the air, as a fraction of `walk_speed` reached (1: as on the ground, 0:
    /// none).
    pub air_control: f64,
    /// Vertical speed of a jump, blocks per second. The default 9.5 peaks about 1.24 blocks
    /// high with gravity 32, the default drag and 64 steps per second (Minecraft's jump is 1.25
    /// blocks), so a jump clears a one-block step.
    pub jump_speed: f64,
    /// The highest ledge the body steps up onto while walking, blocks (Minecraft: 0.6, so a
    /// slab of 0.5 is climbed and a full block is not). 0 turns the step-up off. Sneaking, it is
    /// also the deepest ledge the body walks down (at least 1/16).
    pub step_height: f64,
    /// The speed while sneaking, as a fraction of the walking speed (Minecraft: 0.3).
    pub sneak_factor: f64,
    /// The speed while sprinting, as a fraction of the walking speed (Minecraft: 1.3).
    pub sprint_factor: f64,
    /// Top speed across while flying, blocks per second (Minecraft: 10.92; twice that
    /// sprinting).
    pub fly_speed: f64,
    /// Top speed up or down while flying, blocks per second (Minecraft: 7.5).
    pub fly_vertical_speed: f64,
    /// Flying (Minecraft's creative flight): set it to fly; physics clears it when the body
    /// lands.
    pub flying: bool,
}

impl Default for CharacterController {
    /// Walk 4.317 blocks/s, full air control, jump 9.5 blocks/s, step up 0.6 blocks; sneak at
    /// 0.3 and sprint at 1.3 of the walk; fly at 10.92 across and 7.5 up and down; not flying.
    fn default() -> Self {
        Self {
            walk_speed: 4.317,
            air_control: 1.0,
            jump_speed: 9.5,
            step_height: 0.6,
            sneak_factor: 0.3,
            sprint_factor: 1.3,
            fly_speed: 10.92,
            fly_vertical_speed: 7.5,
            flying: false,
        }
    }
}

impl CharacterController {
    /// The deepest ledge a sneaking body walks down: its step height, at least 1/16 (with no
    /// step-up the guard still needs something under the feet to tell an edge).
    pub fn sneak_drop(&self) -> f64 {
        // `max` takes the other side of a NaN.
        self.step_height.max(MIN_SNEAK_DROP)
    }
}

/// The least [`CharacterController::sneak_drop`]: 1/16 of a block.
const MIN_SNEAK_DROP: f64 = 0.0625;

/// The vertical drag of a flying [`CharacterController`], per second: Minecraft keeps 0.6 of
/// the vertical speed every tick (20 ticks per second), so a flyer stops climbing or sinking
/// within a fraction of a second after the key is let go.
pub const FLIGHT_VERTICAL_DRAG: f64 = 40.0 / 3.0;

/// What a [`CharacterController`] wants to do. Sample input into it once per frame, before the
/// fixed steps (`RunFixedMainLoopSystems::BeforeFixedMainLoop`); AI writes it in `FixedUpdate`.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct MoveIntent {
    /// Horizontal direction and strength: `x` along world X, `y` along world Z. Longer than 1
    /// counts as 1.
    pub wish: Vec2,
    /// A latch: set it to `true` when the jump key is pressed (never clear it yourself). The next
    /// fixed step consumes it, jumping if the body is on the ground. So a press in a frame that
    /// runs no step still jumps once, and a frame that runs two steps jumps once.
    pub jump: bool,
    /// Held: a flying body rises (Minecraft's held jump key). Every step reads it; it is never
    /// cleared.
    pub up: bool,
    /// Held: sneak (slower, never off an edge on the ground; a flying body descends).
    pub sneak: bool,
    /// Held: sprint (faster, also flying; sneaking wins).
    pub sprint: bool,
}

/// A dynamic body hit a block faster than `PhysicsSettings::impact_speed` (a landing, a wall).
/// Written at the end of each fixed step, sorted by entity; read them in `Update` (every message
/// of the frame's steps is there) or in `FixedUpdate`.
#[derive(Message, Clone, Copy, Debug, PartialEq)]
pub struct VoxelImpact {
    /// The body.
    pub entity: Entity,
    /// The face of the body that hit (`Face::NegY`: it landed).
    pub face: Face,
    /// The block it hit.
    pub block: BlockPos,
    /// Its speed along the face's axis just before the hit, blocks per second.
    pub speed: f64,
}

/// What a [`Projectile`] does when it hits a block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OnBlock {
    /// It sticks: it stops with its center on the block's face and falls asleep at once. It
    /// stays there until the block goes (then it falls), it is moved (a [`Teleport`], a
    /// `Transform` or [`BodyPosition`] write) or its [`Velocity`] is set (it flies again).
    #[default]
    Stop,
    /// It bounces off: the velocity along the hit axis turns around, scaled by
    /// `BodySettings::restitution` (and stops on that axis when it is slower than
    /// `BodySettings::bounce_min_speed` or one step of gravity, as a body's bounces do). The
    /// rest of the step goes on along the face, so a ball that lands slides on (slowed by its
    /// ground friction).
    Bounce,
}

/// What a [`Projectile`] does when it hits a body.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OnEntity {
    /// It stops where it touches the body (velocity 0) and falls from there, unless the game
    /// removes it. The body is not pushed: apply a knockback yourself on [`ProjectileHit`]. A
    /// body it stopped against does not stop it again (one that stopped on a mob's top falls
    /// through the mob).
    #[default]
    Stop,
    /// It goes through: each body it enters is hit once, and it flies on.
    Pass,
}

/// A fast small body (an arrow, a thrown snowball) that never tunnels: each step moves its
/// center along a segment that is tested against the voxels (an exact DDA, at any speed) and
/// against the solid bodies in its layers (their boxes grown by the projectile's half extents:
/// exact for boxes, a sphere counts as its bounding box). The earliest hit sends a
/// [`ProjectileHit`] and ends the step there. In the step after it is spawned or moved (a
/// teleport or a nudge), a body it starts inside or touching is hit too, at once: a point-blank
/// shot into a mob hits it.
///
/// Spawn it on a [`RigidBody::Dynamic`] body (a [`Collider`] alone makes a static one, and a
/// projectile of another kind never flies; it is never an obstacle either). Its [`Collider`]
/// (if any) gives the half extents (the shape's bounding box), its offset and its layers; a
/// projectile without one is a point.
/// Gravity and drag come from [`BodySettings`] (`BodySettings::arrow()`), with no speed limit.
/// Projectiles take no part in collisions between bodies (no pushes, no sensor events, not in
/// the body queries) and do not hit each other.
///
/// ```ignore
/// commands.spawn((
///     RigidBody::Dynamic,
///     Projectile::arrow().ignoring(shooter),
///     Collider::aabb(DVec3::splat(0.05)),
///     BodySettings::arrow(),
///     Teleport(eye),
///     Velocity(aim * 60.0),
/// ));
/// ```
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Projectile {
    /// What a block hit does.
    pub on_block: OnBlock,
    /// What a body hit does.
    pub on_entity: OnEntity,
    /// A body it never hits (its shooter).
    pub ignore: Option<Entity>,
    /// The block it is stuck in (after a [`OnBlock::Stop`] hit).
    pub(crate) stuck: Option<BlockPos>,
}

impl Projectile {
    /// A projectile that does `on_block` and `on_entity`.
    pub const fn new(on_block: OnBlock, on_entity: OnEntity) -> Self {
        Self {
            on_block,
            on_entity,
            ignore: None,
            stuck: None,
        }
    }

    /// An arrow: sticks in blocks, stops at bodies.
    pub const fn arrow() -> Self {
        Self::new(OnBlock::Stop, OnEntity::Stop)
    }

    /// The same projectile, never hitting `entity` (its shooter).
    pub const fn ignoring(self, entity: Entity) -> Self {
        Self {
            ignore: Some(entity),
            ..self
        }
    }

    /// The block it is stuck in, if it stuck in one ([`OnBlock::Stop`]).
    pub const fn stuck_in(&self) -> Option<BlockPos> {
        self.stuck
    }
}

/// What a [`Projectile`] hit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectileTarget {
    /// A full block.
    Block {
        /// The block.
        pos: BlockPos,
        /// The face it entered through; `None` when it started inside the block.
        face: Option<Face>,
        /// The block's state.
        state: BlockState,
    },
    /// A body.
    Entity(Entity),
}

/// A [`Projectile`] hit a block or a body in the latest fixed step. Written at the end of the
/// step, sorted by projectile and, for one projectile that passes through several bodies, in
/// the order it hit them. Read them in `Update` like [`VoxelImpact`].
#[derive(Message, Clone, Copy, Debug, PartialEq)]
pub struct ProjectileHit {
    /// The projectile.
    pub projectile: Entity,
    /// What it hit.
    pub target: ProjectileTarget,
    /// Where the projectile's center was when it hit.
    pub point: WorldPos,
    /// Its velocity just before the hit, blocks per second (for a knockback).
    pub velocity: DVec3,
    /// Its speed just before the hit, blocks per second.
    pub speed: f64,
}

/// World-wide physics settings. Read every step: changes apply to the next one.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct PhysicsSettings {
    /// Gravity, blocks per second squared (default `(0, -32, 0)`, Minecraft's).
    pub gravity: DVec3,
    /// Contact tolerance, blocks (default `1e-7`; see `SweepConfig::eps`).
    pub eps: f64,
    /// Speed limit per axis, blocks per second (default 100).
    pub max_speed: f64,
    /// The most a body inside blocks is pushed out per step, blocks (default 1).
    pub max_depenetration: f64,
    /// Clips faster than this (blocks per second) send a [`VoxelImpact`] (default 4).
    pub impact_speed: f64,
    /// Passes over the overlapping solid pairs per step (default 2). Each pass pushes every pair
    /// that still overlaps; an overlap left after the last pass is fixed in the next steps. 0
    /// turns pushes off (sensors and events still work).
    pub solver_passes: u32,
    /// The broadphase grid's cells are `2^grid_shift` blocks wide (default 2: 4 blocks).
    /// Bodies wider than two cells are tested against every other body, so raise it for big
    /// bodies. At most 16.
    pub grid_shift: u32,
    /// A body resting on something and slower than this on every axis (blocks per second)
    /// counts as still (default 0.05; see [`Sleeping`]).
    pub sleep_speed: f64,
    /// Steps a body stays still before it falls asleep (default 32: half a second at 64 Hz).
    /// 0 turns sleeping off (and wakes the sleeping bodies).
    pub sleep_steps: u32,
    /// The step moves the dynamic bodies (and the projectiles) on the compute threads when
    /// more of them than this are awake (default 256; static, kinematic and sleeping bodies do
    /// not count). Each body only reads the world and writes itself, so the results are
    /// bit-identical either way. The broadphase and the solve also go on the threads above it,
    /// with results that do not depend on the threads either: the broadphase from 4096 bodies
    /// in the step's list, the solve's pushes when the previous step pushed at least 128 times
    /// and no island of pairs holds more than half of them (see [`PhysicsStats::islands`]), and
    /// its write-back from 256 bodies. 0 runs every part on the threads whatever its size.
    pub parallel_threshold: usize,
}

impl Default for PhysicsSettings {
    fn default() -> Self {
        let step = StepParams::default();
        Self {
            gravity: step.gravity,
            eps: step.sweep.eps,
            max_speed: step.max_speed,
            max_depenetration: step.sweep.max_depenetration,
            impact_speed: step.impact_speed,
            solver_passes: 2,
            grid_shift: DEFAULT_GRID_SHIFT,
            sleep_speed: 0.05,
            sleep_steps: 32,
            parallel_threshold: 256,
        }
    }
}

impl PhysicsSettings {
    /// The inputs of one step of `dt` seconds (for [`crate::step_body`] and [`crate::predict`]).
    pub fn step_params(&self, dt: f64) -> StepParams {
        StepParams {
            gravity: self.gravity,
            dt,
            max_speed: self.max_speed,
            impact_speed: self.impact_speed,
            sweep: SweepConfig {
                eps: self.eps,
                max_depenetration: self.max_depenetration,
            },
        }
    }
}

/// Counters of the latest fixed step, for the `"physics"` line of `DiagnosticsBoard` and the
/// F3 overlay ([`PhysicsStats::summary`]).
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct PhysicsStats {
    /// Bodies (entities with a `RigidBody`), awake and asleep.
    pub bodies: usize,
    /// Dynamic bodies the latest step moved (awake ones, frozen and projectiles included).
    pub dynamic: usize,
    /// Dynamic bodies that did not move because of unloaded chunks.
    pub frozen: usize,
    /// Bodies asleep after the latest step ([`Sleeping`]).
    pub sleeping: usize,
    /// Bodies woken up in the latest step.
    pub woken: usize,
    /// Projectiles the latest step moved.
    pub projectiles: usize,
    /// Projectile hits of the latest step.
    pub hits: usize,
    /// The latest step moved the bodies on several threads.
    pub parallel: bool,
    /// Candidate pairs of the broadphase in the latest step.
    pub pairs: usize,
    /// Pushes between solid bodies in the latest step (one per overlapping pair and pass).
    pub pushes: usize,
    /// Islands the latest step's solve pushed on the compute threads (0: it pushed on one
    /// thread).
    pub islands: usize,
    /// Pairs in contact at the end of the latest step that report events (overlapping sensor
    /// pairs, and touching or overlapping solid pairs with [`ReportContacts`]).
    pub overlaps: usize,
    /// Impacts of the latest step.
    pub impacts: usize,
    /// Steps run since startup.
    pub steps: u64,
    /// Time the latest step spent moving dynamic bodies (`PhysicsSystems::Step`).
    pub step_time: Duration,
    /// Time the latest step spent between bodies: the broadphase and the solve
    /// (`PhysicsSystems::Broadphase` and `PhysicsSystems::Solve`).
    pub solve_time: Duration,
}

impl PhysicsStats {
    /// `bodies=12 dynamic=10 frozen=0 sleeping=2 pairs=3 pushes=1 overlaps=2 impacts=1
    /// projectiles=1 hits=0 step=0.012ms solve=0.004ms steps=640`.
    pub fn summary(&self) -> String {
        format!(
            "bodies={} dynamic={} frozen={} sleeping={} pairs={} pushes={} overlaps={} \
             impacts={} projectiles={} hits={} step={:.3}ms solve={:.3}ms steps={}",
            self.bodies,
            self.dynamic,
            self.frozen,
            self.sleeping,
            self.pairs,
            self.pushes,
            self.overlaps,
            self.impacts,
            self.projectiles,
            self.hits,
            self.step_time.as_secs_f64() * 1e3,
            self.solve_time.as_secs_f64() * 1e3,
            self.steps
        )
    }
}
