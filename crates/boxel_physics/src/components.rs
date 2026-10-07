//! Components, messages and resources of physics bodies (phases P2 and P3 of ADR 0016).

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
use boxel_voxel::{BlockPos, Face, WorldPos};
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
#[derive(Component, Clone, Copy, Debug, PartialEq)]
#[require(RigidBody = RigidBody::Static)]
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

/// Marker: contacts of this solid body with other solid bodies are reported as
/// [`CollisionStarted`] and [`CollisionEnded`] too (sensor overlaps always are). A contact is a
/// step where the two overlapped before being pushed apart: a crate resting on another, a
/// player walking into a mob.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReportContacts;

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
/// its velocity and [`Contacts`], and `Transform` follows at the end of the step. Do it in a
/// fixed step (`FixedUpdate`); written in `Update`, the interpolated view of a frame without a
/// step would slide toward it. If `Transform` was also written since the last step, `Transform`
/// wins.
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
}

impl PhysicsSync {
    /// The translation physics last wrote.
    pub fn last_written(&self) -> Option<DVec3> {
        self.last_written
    }

    /// How far the body moved in the latest step, before the pushes between bodies (zero for
    /// static and frozen bodies).
    pub fn moved(&self) -> DVec3 {
        self.moved
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
}

impl Default for CharacterController {
    /// Walk 4.317 blocks/s, full air control, jump 9.5 blocks/s.
    fn default() -> Self {
        Self {
            walk_speed: 4.317,
            air_control: 1.0,
            jump_speed: 9.5,
        }
    }
}

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
    /// Bodies (entities with a `RigidBody`).
    pub bodies: usize,
    /// Dynamic bodies.
    pub dynamic: usize,
    /// Dynamic bodies that did not move because of unloaded chunks.
    pub frozen: usize,
    /// Candidate pairs of the broadphase in the latest step.
    pub pairs: usize,
    /// Pushes between solid bodies in the latest step (one per overlapping pair and pass).
    pub pushes: usize,
    /// Pairs overlapping at the end of the latest step that report events (sensor pairs and
    /// solid pairs with [`ReportContacts`]).
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
    /// `bodies=12 dynamic=10 frozen=0 pairs=3 pushes=1 overlaps=2 impacts=1 step=0.012ms
    /// solve=0.004ms steps=640`.
    pub fn summary(&self) -> String {
        format!(
            "bodies={} dynamic={} frozen={} pairs={} pushes={} overlaps={} impacts={} \
             step={:.3}ms solve={:.3}ms steps={}",
            self.bodies,
            self.dynamic,
            self.frozen,
            self.pairs,
            self.pushes,
            self.overlaps,
            self.impacts,
            self.step_time.as_secs_f64() * 1e3,
            self.solve_time.as_secs_f64() * 1e3,
            self.steps
        )
    }
}
