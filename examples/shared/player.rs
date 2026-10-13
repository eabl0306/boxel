//! Walking for the examples (physics phase P2, ADR 0016): a player body with a
//! `CharacterController` that the camera follows at eye height.
//!
//! G switches between flying (the free fly camera) and walking. The player is a dynamic box of
//! 0.6 x 1.8 x 0.6 blocks with gravity: WASD walks along the camera's heading and Space jumps
//! (held, it jumps again on landing). It walks up slabs and stairs of slabs without jumping (the
//! controller's step-up); the body rises within one fixed step, so the eye trails the rise and
//! catches up in about a tenth of a second ([`EyeLag`], only the view). The camera keeps its
//! mouse look; its own WASD movement is paused while walking.
//!
//! The player modes of ADR 0026, as in Minecraft: Shift sneaks (slower, crouched 1.5 blocks tall
//! with the eye at [`CROUCH_EYE_HEIGHT`], and it never walks off an edge it would fall from; where
//! there is no room to stand it stays crouched), Ctrl sprints (forward only; it goes on until the
//! player stops, crouches, walks backward or runs into something, and a jump while sprinting
//! carries further), and Space twice within [`FLY_TAP`] turns creative flight on or off (no
//! gravity: Space rises, Shift descends; landing ends it; water does not slow it). Flying the
//! free camera (not walking), Shift and Ctrl keep moving it down and boosting it.
//!
//! The view, as Minecraft's first-person camera (only the camera; the body is exact): the eye
//! eases down and up when crouching or standing up, the field of view widens [`SPRINT_FOV`]
//! while sprinting, and walking on the ground bobs the view ([`view_bob`]; also wading in
//! water, not in the air or flying). [`BOB_KEY`] turns the bob off and on.
//!
//! Frame order:
//! - `RunFixedMainLoopSystems::BeforeFixedMainLoop`: the keys are sampled once per frame into
//!   the player's `MoveIntent` (the jump is a latch, so a press in a frame without a fixed step
//!   still jumps once). The walk direction uses the camera's yaw from the previous frame: mouse
//!   look stays in `Update` (`FlyCameraSystems`), not before the fixed loop as the physics plan
//!   puts it, so walking lags the view by one frame (7 ms at 144 fps), which is not noticeable.
//! - Fixed steps: the controller and the physics move the player.
//! - `Update`: [`switch_mode`] (before the fly camera) spawns or removes the player, and
//!   [`follow_player`] (after the fly camera, before `WorldSystems::Interest`) puts the camera
//!   at the player's interpolated eye and writes the F3 lines.
#![allow(dead_code, reason = "each example uses its own part")]

use boxel::app::RunFixedMainLoopSystems;
use boxel::camera::FlyCameraSystems;
use boxel::debug_overlay::OverlayLines;
use boxel::math::DVec2;
use boxel::physics::{Crouch, Frozen, PhysicsStats, Swimmer};
use boxel::prelude::*;
use boxel::world::WorldSystems;
use boxel::world::lifecycle::WorldScoped;
use std::time::Duration;

/// Eye height above the feet, blocks (Minecraft's).
pub const EYE_HEIGHT: f64 = 1.62;
/// Eye height above the feet while crouching, blocks (Minecraft's).
pub const CROUCH_EYE_HEIGHT: f64 = 1.27;
/// The field of view while sprinting, times the normal one (about Minecraft's widening).
pub const SPRINT_FOV: f64 = 1.1;
/// Turns the view bob off and on.
pub const BOB_KEY: KeyCode = KeyCode::KeyV;
/// The player's box: width and height, blocks.
pub const PLAYER_SIZE: (f64, f64) = (0.6, 1.8);
/// Switches flying and walking.
pub const WALK_KEY: KeyCode = KeyCode::KeyG;
/// How far down the first landing looks for ground, blocks.
const LAND_REACH: f64 = 512.0;
/// Sneaks while held.
pub const SNEAK_KEY: KeyCode = KeyCode::ShiftLeft;
/// Sprints while held.
pub const SPRINT_KEY: KeyCode = KeyCode::ControlLeft;
/// Two presses of Space within this time turn creative flight on or off (Minecraft's 7 ticks).
pub const FLY_TAP: Duration = Duration::from_millis(350);

/// How fast the eye catches up with a step-up, per second: the lag shrinks by `1 + rate * dt`
/// each frame (about a tenth of a second for a slab).
const EYE_CATCH_UP: f64 = 20.0;
/// A rise larger than this in one frame is a teleport, not a step: the eye goes there at once.
const MAX_EYE_LAG: f64 = 1.0;
/// How fast the eye height and the field of view ease toward theirs, per second (see [`ease`]):
/// Minecraft halves the gap every tick (20 per second), which is this rate.
const VIEW_EASE: f64 = 20.0;
/// How fast the bob follows the walking speed, per second: Minecraft keeps 0.6 of the gap
/// every tick.
const BOB_EASE: f64 = 40.0 / 3.0;
/// The strongest bob, Minecraft's (blocks per tick of walking speed, which it caps at this).
const MAX_BOB: f64 = 0.1;
/// How much of its speed across Minecraft's player keeps after a tick on dry ground (the block's
/// friction 0.6 times 0.91). Its bob reads the speed after that friction, so a body walking at
/// `v` blocks per tick bobs `v` times this.
const GROUND_KEEP: f64 = 0.546;
/// The same in water (Minecraft's water slowdown, on the ground too).
const WATER_KEEP: f64 = 0.8;

/// The player body (only while walking).
#[derive(Component, Clone, Copy, Debug, Default)]
#[require(EyeLag, PlayerView)]
pub struct Player;

/// What the camera shows of the player beyond its eye: the eased eye height, the field of view
/// and the bob. Only the view.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct PlayerView {
    /// The eye height the camera is at, blocks: it eases toward [`EYE_HEIGHT`] standing and
    /// [`CROUCH_EYE_HEIGHT`] crouching.
    pub eye: f64,
    /// The field of view, times the normal one: it eases toward [`SPRINT_FOV`] while sprinting
    /// and 1 otherwise.
    pub fov: f64,
    /// Minecraft's walk distance for the bob's phase (0.6 per block walked), kept in `0..2`
    /// (the bob's period).
    pub walked: f64,
    /// The bob's strength (Minecraft's `bob`): the speed across on the ground in blocks per
    /// tick after a tick's friction ([`Footing`]), at most [`MAX_BOB`]; 0 in the air or flying.
    pub bob: f64,
    /// The body's interpolated position across last frame.
    last: Option<DVec2>,
}

impl Default for PlayerView {
    fn default() -> Self {
        Self {
            eye: EYE_HEIGHT,
            fov: 1.0,
            walked: 0.0,
            bob: 0.0,
            last: None,
        }
    }
}

/// `value` after a frame of `dt` seconds easing toward `target` at `rate` per second: the gap
/// shrinks by `1 + rate * dt`, and within `1e-4` it lands on the target exactly.
pub fn ease(value: f64, target: f64, rate: f64, dt: f64) -> f64 {
    let v = target + (value - target) / (1.0 + rate * dt.max(0.0));
    if (v - target).abs() < 1e-4 { target } else { v }
}

/// Where the player walks, for the bob: Minecraft bobs on the ground (not swimming, a pose
/// boxel does not have yet), with the speed it keeps after the ground's or the water's
/// friction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Footing {
    /// In the air or flying: no bob.
    Air,
    /// On dry ground ([`GROUND_KEEP`]).
    Ground,
    /// On the ground in water: the bottom of a pool, a waterlogged slab ([`WATER_KEEP`]).
    Wading,
}

/// A view bob: where the camera goes and how it turns, in its own frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bob {
    /// The eye's offset, blocks: `x` to the right, `y` up.
    pub offset: DVec3,
    /// The turn around the view axis, radians.
    pub roll: f32,
    /// The pitch added to the view, radians (negative: down).
    pub pitch: f32,
}

/// Minecraft's view bob (`GameRenderer::bobView`) at walk distance `walked` with strength
/// `bob`. Minecraft moves and turns the scene in view space; this is the same as a camera that
/// goes up to `bob` up and `bob / 2` to the sides and turns up to `bob * 3` degrees around its
/// view axis and `bob * 5` degrees down, twice per stride (a full sideways sway every 2 of
/// `walked`, 3.3 blocks).
pub fn view_bob(walked: f64, bob: f64) -> Bob {
    if bob == 0.0 {
        return Bob::default();
    }
    let phase = walked * std::f64::consts::PI;
    let (sin, cos) = phase.sin_cos();
    let down = ((phase + 0.2).cos() * bob).abs() * 5.0;
    Bob {
        offset: DVec3::new(sin * bob * 0.5, (cos * bob).abs(), 0.0),
        roll: (sin * bob * 3.0).to_radians() as f32,
        pitch: -down.to_radians() as f32,
    }
}

/// How far the camera's eye is below the player's real eye after a step-up, blocks. The body
/// climbs a slab within one fixed step (15.6 ms at 64 Hz), which looks like a jolt; the eye
/// lags and catches up instead. Only the view: the body's position stays exact.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct EyeLag {
    /// Current lag, blocks (0 or more).
    pub lag: f64,
    /// The body's interpolated height last frame.
    last_y: Option<f64>,
}

/// The new eye lag after a frame of `dt` seconds in which the body rose by `rise` blocks:
/// a rise while standing on the ground (a step-up, `stepped`) adds to the lag, then the lag
/// shrinks toward 0. Anything else (falling, jumping, a lift, a teleport) leaves the eye on the
/// body.
pub fn eye_lag(lag: f64, rise: f64, stepped: bool, dt: f64) -> f64 {
    let lag = if stepped && rise > 0.0 && rise <= MAX_EYE_LAG {
        (lag + rise).min(MAX_EYE_LAG)
    } else if rise > MAX_EYE_LAG {
        0.0
    } else {
        lag
    };
    let lag = lag / (1.0 + EYE_CATCH_UP * dt.max(0.0));
    if lag < 1e-3 { 0.0 } else { lag }
}

/// Walking or flying.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct Walking {
    /// Walking (true) or flying (false). [`WALK_KEY`] flips it; the player body follows.
    pub on: bool,
    /// The next switch to walking puts the player on the ground under the camera instead of
    /// dropping it from the camera (the start of the sandbox).
    pub land: bool,
    /// The view bobs while walking on the ground (on by default; [`BOB_KEY`] flips it).
    pub view_bob: bool,
    /// The fly camera's speed, kept while walking (its WASD must not move the camera then).
    fly_speed: Option<f32>,
    /// The camera's field of view, kept while walking (sprinting widens it).
    fov: Option<f32>,
    /// The camera's rotation carries a view bob (to take away when the bob stops).
    turned: bool,
}

/// Adds walking: [`Walking`], [`switch_mode`], [`sample_input`] and [`follow_player`]. Needs
/// `FlyCameraPlugin`, `PhysicsPlugin`, `WorldPlugin` and input. With `start_walking` the player
/// lands on the ground under the camera as soon as that ground is loaded.
#[derive(Clone, Copy, Debug, Default)]
pub struct WalkPlugin {
    pub start_walking: bool,
}

impl Plugin for WalkPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Walking {
            on: self.start_walking,
            land: self.start_walking,
            view_bob: true,
            fly_speed: None,
            fov: None,
            turned: false,
        })
        .add_systems(
            RunFixedMainLoop,
            sample_input.in_set(RunFixedMainLoopSystems::BeforeFixedMainLoop),
        )
        .add_systems(
            Update,
            (
                switch_mode
                    .before(FlyCameraSystems)
                    .before(WorldSystems::Interest),
                follow_player
                    .after(FlyCameraSystems)
                    .before(WorldSystems::Interest),
            ),
        );
    }
}

/// The player body standing at `feet` (exact: through `Teleport`, which also puts its
/// `Transform` there at once).
pub fn player_body(feet: WorldPos) -> impl Bundle {
    (
        Player,
        RigidBody::Dynamic,
        Collider::standing(PLAYER_SIZE.0, PLAYER_SIZE.1),
        CharacterController::default(),
        Interpolated::default(),
        Teleport(feet),
        // In water it swims (with `SwimPlugin`): held Space keeps setting the jump latch, which
        // swims up there.
        Swimmer::default(),
        // Of the world it stands in: a close despawns it (still walking, the next world gets a
        // new one under the camera).
        WorldScoped,
    )
}

/// [`WALK_KEY`] flips [`Walking::on`] and [`BOB_KEY`] [`Walking::view_bob`] (while the fly
/// camera takes input); then the player body is spawned under the active camera's eye, or
/// removed. The camera's fly speed is 0 while
/// walking. With [`Walking::land`] the player waits for the ground under the camera to load and
/// stands on it.
pub fn switch_mode(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    physics: PhysicsQuery,
    mut walking: ResMut<Walking>,
    mut cameras: Query<(&Camera, &Transform, &mut FlyCamera)>,
    players: Query<Entity, With<Player>>,
) {
    let Some((_, eye, mut fly)) = cameras.iter_mut().find(|(camera, ..)| camera.active) else {
        return;
    };
    if fly.enabled && keys.just_pressed(WALK_KEY) {
        walking.on = !walking.on;
    }
    if fly.enabled && keys.just_pressed(BOB_KEY) {
        walking.view_bob = !walking.view_bob;
    }
    match (walking.on, players.iter().next()) {
        (true, None) => {
            let eye = WorldPos::from_dvec3(eye.translation);
            let below_eye = eye.offset(DVec3::new(0.0, -EYE_HEIGHT, 0.0));
            let feet = if walking.land {
                match physics.raycast_voxels(eye, DVec3::NEG_Y, LAND_REACH, VoxelFilter::Collision)
                {
                    RayResult::Hit(hit) => hit.point,
                    // The ground is not loaded yet: try again next frame.
                    RayResult::Unloaded { .. } => return,
                    RayResult::Miss => below_eye,
                }
            } else {
                below_eye
            };
            walking.land = false;
            commands.spawn(player_body(feet));
            walking.fly_speed = Some(fly.speed);
            fly.speed = 0.0;
        }
        (false, Some(player)) => {
            commands.entity(player).despawn();
            if let Some(speed) = walking.fly_speed.take() {
                fly.speed = speed;
            }
        }
        _ => {}
    }
}

/// +1 if only `positive` is held, -1 if only `negative` is, else 0.
fn axis(keys: &ButtonInput<KeyCode>, positive: KeyCode, negative: KeyCode) -> f32 {
    match (keys.pressed(positive), keys.pressed(negative)) {
        (true, false) => 1.0,
        (false, true) => -1.0,
        _ => 0.0,
    }
}

/// Once per frame, before the fixed steps: WASD along the active camera's heading into the
/// player's `MoveIntent::wish` (and the heading into its `facing`), Space into its jump latch
/// (held, or tapped within this frame: a press and release in one long frame still jumps) and
/// its `up` (held), Shift into `sneak` and Ctrl into `sprint`. A second press of Space within [`FLY_TAP`] of the first turns
/// `CharacterController::flying` on or off. Nothing while the fly camera takes no input
/// (automation).
pub fn sample_input(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    cameras: Query<(&Camera, &FlyCamera)>,
    mut players: Query<(&mut MoveIntent, &mut CharacterController), With<Player>>,
    mut last_tap: Local<Option<Duration>>,
) {
    let Some((_, fly)) = cameras.iter().find(|(camera, _)| camera.active) else {
        return;
    };
    let held = |key: KeyCode| fly.enabled && keys.pressed(key);
    let (forward, right) = if fly.enabled {
        (
            axis(&keys, KeyCode::KeyW, KeyCode::KeyS),
            axis(&keys, KeyCode::KeyD, KeyCode::KeyA),
        )
    } else {
        (0.0, 0.0)
    };
    let tapped = fly.enabled && keys.just_pressed(KeyCode::Space);
    let jump = held(KeyCode::Space) || tapped;
    // A double tap of Space: the second press toggles the flight (and is not a first tap).
    let now = time.elapsed();
    let toggle = tapped && last_tap.is_some_and(|last| now.saturating_sub(last) <= FLY_TAP);
    if tapped {
        *last_tap = if toggle { None } else { Some(now) };
    }
    // The fly camera's heading: -Z and +X turned by the yaw around +Y (x is world X, y world Z).
    let (sin, cos) = fly.yaw.sin_cos();
    let facing = Vec2::new(-sin, -cos);
    let wish = (facing * forward + Vec2::new(cos, -sin) * right).normalize_or_zero();
    let wanted = MoveIntent {
        wish,
        jump: false,
        up: held(KeyCode::Space),
        sneak: held(SNEAK_KEY),
        sprint: held(SPRINT_KEY),
        facing,
    };
    for (mut intent, mut controller) in &mut players {
        let latch = intent.jump || jump;
        let next = MoveIntent {
            jump: latch,
            ..wanted
        };
        if *intent != next {
            *intent = next;
        }
        if toggle {
            controller.flying = !controller.flying;
        }
    }
}

/// The active camera at the player's eye: its interpolated `Transform` plus the eased eye height
/// of [`PlayerView`] (standing or crouching), minus the [`EyeLag`] of a step-up, plus the
/// [`view_bob`] (with [`Walking::view_bob`]); the field of view widened while sprinting (the
/// camera's own one comes back when the player goes). Also the F3 lines `move` (mode, ground,
/// speed) and `physics` (`PhysicsStats`).
#[expect(
    clippy::type_complexity,
    reason = "a system's parameters: the player's state for the view"
)]
pub fn follow_player(
    time: Res<Time>,
    mut walking: ResMut<Walking>,
    stats: Res<PhysicsStats>,
    mut players: Query<
        (
            &Transform,
            &Velocity,
            &Contacts,
            Has<Frozen>,
            (&mut EyeLag, &mut PlayerView),
            &CharacterController,
            &MoveIntent,
            &Crouch,
            Option<&Swimmer>,
        ),
        (With<Player>, Without<FlyCamera>),
    >,
    mut cameras: Query<(&mut Camera, &mut Transform, &FlyCamera)>,
    lines: Option<ResMut<OverlayLines>>,
) {
    let dt = time.delta().as_secs_f64();
    let player = players.iter_mut().next();
    let player = player.map(
        |(
            body,
            velocity,
            contacts,
            frozen,
            (mut eye, mut view),
            controller,
            intent,
            crouch,
            swim,
        )| {
            let y = body.translation.y;
            let rise = eye.last_y.map_or(0.0, |last| y - last);
            // A step-up ends on the ground with no vertical speed; a jump or a lift does not.
            let stepped = contacts.on_ground && velocity.0.y <= 0.0;
            let lag = eye_lag(eye.lag, rise, stepped, dt);
            if eye.lag != lag || eye.last_y != Some(y) {
                eye.lag = lag;
                eye.last_y = Some(y);
            }
            let flying = controller.flying;
            let footing = if !contacts.on_ground || flying {
                Footing::Air
            } else if swim.is_some_and(|s| s.in_fluid) {
                Footing::Wading
            } else {
                Footing::Ground
            };
            let next = next_view(
                &view,
                body.translation,
                crouch.is_crouching(),
                controller.sprinting,
                footing,
                velocity.0,
                dt,
            );
            if *view != next {
                *view = next;
            }
            let modes = Modes {
                flying,
                sneaking: intent.sneak && !flying,
                crouching: crouch.is_crouching(),
                sprinting: controller.sprinting,
            };
            (*body, *velocity, *contacts, frozen, lag, next, modes)
        },
    );
    for (mut camera, mut transform, fly) in &mut cameras {
        if !camera.active {
            continue;
        }
        let Some((body, _, _, _, lag, view, _)) = player else {
            // The player went: the camera's own field of view comes back, and its rotation
            // without the bob.
            if let Some(fov) = walking.fov.take() {
                camera.fov_y_radians = fov;
            }
            if walking.turned {
                walking.turned = false;
                transform.rotation =
                    Quat::from_rotation_y(fly.yaw) * Quat::from_rotation_x(fly.pitch);
            }
            continue;
        };
        let base_fov = *walking.fov.get_or_insert(camera.fov_y_radians);
        let fov = (f64::from(base_fov) * view.fov) as f32;
        if camera.fov_y_radians != fov {
            camera.fov_y_radians = fov;
        }
        let bob = if walking.view_bob {
            view_bob(view.walked, view.bob)
        } else {
            Bob::default()
        };
        // The bob turns the fly camera's own rotation (its mouse look, as it writes it); with
        // no bob the rotation is left alone, after putting it back once.
        let turned = bob != Bob::default();
        if turned || walking.turned {
            let mut rotation =
                Quat::from_rotation_y(fly.yaw) * Quat::from_rotation_x(fly.pitch + bob.pitch);
            if bob.roll != 0.0 {
                rotation *= Quat::from_rotation_z(bob.roll);
            }
            if transform.rotation != rotation {
                transform.rotation = rotation;
            }
            walking.turned = turned;
        }
        let offset = (transform.rotation * bob.offset.as_vec3()).as_dvec3();
        let eye = body.translation + DVec3::new(0.0, view.eye - lag, 0.0) + offset;
        if transform.translation != eye {
            transform.translation = eye;
        }
    }
    let Some(mut lines) = lines else {
        return;
    };
    let mode = match player {
        Some((_, velocity, contacts, frozen, _, _, modes)) => {
            let state = if frozen {
                "frozen (chunk not loaded)"
            } else if modes.flying {
                "flying"
            } else if contacts.on_ground {
                "on ground"
            } else {
                "in the air"
            };
            let speed = DVec2::new(velocity.0.x, velocity.0.z).length();
            format!(
                "Walk (G flies)  {state}  speed {speed:.2} b/s{}",
                modes.suffix()
            )
        }
        None if walking.on => "Walk (G flies)  waiting for the ground".to_string(),
        None => "Fly (G walks)".to_string(),
    };
    let physics = format!("Physics {}", stats.summary());
    for (key, line) in [("move", mode), ("physics", physics)] {
        if lines.get(key) != Some(line.as_str()) {
            lines.set(key, line);
        }
    }
}

/// The view after a frame of `dt` seconds in which the body was at `at` (interpolated),
/// `crouching` or not, `sprinting` or not, with `velocity`, and its `footing`: the eye height and
/// the field of view ease toward theirs, the walk distance grows with the move across (0.6 per
/// block, Minecraft's), and the bob eases toward the speed across in blocks per tick, times what
/// Minecraft keeps of it after a tick on that footing (at most [`MAX_BOB`]; 0 in the air).
pub fn next_view(
    view: &PlayerView,
    at: DVec3,
    crouching: bool,
    sprinting: bool,
    footing: Footing,
    velocity: DVec3,
    dt: f64,
) -> PlayerView {
    let eye = if crouching {
        CROUCH_EYE_HEIGHT
    } else {
        EYE_HEIGHT
    };
    let fov = if sprinting { SPRINT_FOV } else { 1.0 };
    let across = DVec2::new(at.x, at.z);
    let moved = view.last.map_or(0.0, |last| across.distance(last));
    // A jump of more than a few blocks in one frame is a teleport, not a walk.
    let moved = if moved < 4.0 { moved } else { 0.0 };
    let per_tick = DVec2::new(velocity.x, velocity.z).length() / 20.0;
    let bob = match footing {
        Footing::Air => 0.0,
        Footing::Ground => (per_tick * GROUND_KEEP).min(MAX_BOB),
        Footing::Wading => (per_tick * WATER_KEEP).min(MAX_BOB),
    };
    PlayerView {
        eye: ease(view.eye, eye, VIEW_EASE, dt),
        fov: ease(view.fov, fov, VIEW_EASE, dt),
        walked: (view.walked + moved * 0.6).rem_euclid(2.0),
        bob: ease(view.bob, bob, BOB_EASE, dt),
        last: Some(across),
    }
}

/// The player modes F3 shows.
#[derive(Clone, Copy, Debug, Default)]
struct Modes {
    flying: bool,
    sneaking: bool,
    crouching: bool,
    sprinting: bool,
}

impl Modes {
    /// `"  sneaking"`, `"  crouching"` (crouched without sneaking: no room to stand up),
    /// `"  sprinting"` or nothing.
    fn suffix(self) -> &'static str {
        if self.sneaking {
            "  sneaking"
        } else if self.crouching {
            "  crouching"
        } else if self.sprinting {
            "  sprinting"
        } else {
            ""
        }
    }
}

#[cfg(test)]
mod eye_tests {
    use super::{
        Bob, CROUCH_EYE_HEIGHT, EYE_HEIGHT, Footing, MAX_BOB, PlayerView, SPRINT_FOV, ease,
        eye_lag, next_view, view_bob,
    };
    use boxel::prelude::*;

    #[test]
    fn the_eye_trails_a_step_up_only() {
        let dt = 1.0 / 144.0;
        // A step-up: the eye trails by the rise, shrunk by one frame of catching up.
        let lag = eye_lag(0.0, 0.5, true, dt);
        assert!(lag > 0.4 && lag < 0.5, "{lag}");
        // It catches up within a fraction of a second, exactly.
        let mut l = lag;
        for _ in 0..72 {
            l = eye_lag(l, 0.0, true, dt);
        }
        assert_eq!(l, 0.0);
        // A jump, a fall or a lift moves the eye with the body; a teleport up drops the lag.
        assert_eq!(eye_lag(0.0, 0.5, false, dt), 0.0);
        assert_eq!(eye_lag(0.0, -0.5, true, dt), 0.0);
        assert_eq!(eye_lag(0.3, 40.0, true, dt), 0.0);
        // Steps in a row never trail more than a block.
        assert!(eye_lag(0.9, 0.5, true, 0.0) <= 1.0);
    }

    #[test]
    fn easing_lands_exactly_and_never_overshoots() {
        let dt = 1.0 / 144.0;
        let mut v = EYE_HEIGHT;
        let mut frames = 0;
        while v != CROUCH_EYE_HEIGHT {
            let next = ease(v, CROUCH_EYE_HEIGHT, 20.0, dt);
            assert!(next < v && next >= CROUCH_EYE_HEIGHT, "{next}");
            v = next;
            frames += 1;
        }
        // About a third of a second, as Minecraft's halving every tick.
        assert!((30..70).contains(&frames), "{frames}");
        assert_eq!(ease(1.0, 2.0, 20.0, 0.0), 1.0);
        assert_eq!(ease(2.0, 2.0, 20.0, dt), 2.0);
    }

    #[test]
    fn the_bob_stays_small_and_repeats_every_two_of_walk() {
        assert_eq!(view_bob(0.7, 0.0), Bob::default());
        let b = MAX_BOB;
        for i in 0..200 {
            let w = f64::from(i) / 100.0;
            let bob = view_bob(w, b);
            assert!(bob.offset.x.abs() <= b * 0.5 + 1e-12);
            assert!((0.0..=b + 1e-12).contains(&bob.offset.y));
            assert_eq!(bob.offset.z, 0.0);
            assert!(bob.roll.abs() <= (b as f32 * 3.0).to_radians() + 1e-6);
            assert!(bob.pitch <= 0.0 && bob.pitch >= -(b as f32 * 5.0).to_radians() - 1e-6);
            let again = view_bob(w + 2.0, b);
            assert!((again.offset - bob.offset).length() < 1e-9);
        }
        // Up at the top of each stride (walk distance 0 and 1), level halfway; sideways the
        // other way every stride.
        assert!((view_bob(0.0, b).offset.y - b).abs() < 1e-12);
        assert!(view_bob(0.5, b).offset.y < 1e-12);
        assert!(view_bob(0.5, b).offset.x > 0.0 && view_bob(1.5, b).offset.x < 0.0);
    }

    #[test]
    fn the_view_crouches_widens_and_bobs_smoothly() {
        let dt = 1.0 / 144.0;
        let mut view = PlayerView::default();
        let at = DVec3::new(0.5, 4.0, 0.5);
        // Standing still: nothing changes.
        let still = next_view(&view, at, false, false, Footing::Ground, DVec3::ZERO, dt);
        assert_eq!((still.eye, still.fov, still.bob), (EYE_HEIGHT, 1.0, 0.0));
        // Crouching and sprinting at once (not possible in the game, but each part alone):
        // the eye goes down and the view widens, smoothly, and land exactly.
        let mut frames = 0;
        while view.eye != CROUCH_EYE_HEIGHT || view.fov != SPRINT_FOV {
            let next = next_view(&view, at, true, true, Footing::Ground, DVec3::ZERO, dt);
            assert!(next.eye < view.eye || next.eye == CROUCH_EYE_HEIGHT);
            assert!(next.fov > view.fov || next.fov == SPRINT_FOV);
            view = next;
            frames += 1;
        }
        assert!(frames > 10 && frames < 100, "{frames}");
        // Walking at 4.3 blocks per second on the ground: the bob grows to its top and the
        // walk distance grows 0.6 per block (in 0..2).
        let v = DVec3::new(0.0, 0.0, -4.317);
        let mut p = at;
        for _ in 0..144 {
            p += v * dt;
            view = next_view(&view, p, false, false, Footing::Ground, v, dt);
        }
        assert_eq!(view.bob, MAX_BOB);
        let walked = (4.317 * 0.6) % 2.0;
        assert!((view.walked - walked).abs() < 1e-9, "{}", view.walked);
        // Flying or in the air it fades out; a teleport is no walk.
        for _ in 0..144 {
            view = next_view(&view, p, false, false, Footing::Air, v, dt);
        }
        assert_eq!(view.bob, 0.0);
        let before = view.walked;
        view = next_view(
            &view,
            p + DVec3::new(100.0, 0.0, 0.0),
            false,
            false,
            Footing::Ground,
            v,
            dt,
        );
        assert_eq!(view.walked, before);
    }

    #[test]
    fn the_bob_reads_the_speed_after_a_ticks_friction_as_minecraft() {
        let dt = 1.0 / 144.0;
        // The bob a steady speed across (blocks per second) settles at on a footing.
        let settle = |speed: f64, footing: Footing| {
            let mut view = PlayerView::default();
            for _ in 0..288 {
                view = next_view(
                    &view,
                    DVec3::ZERO,
                    false,
                    false,
                    footing,
                    DVec3::new(speed, 0.0, 0.0),
                    dt,
                );
            }
            view.bob
        };
        // Walking and sprinting reach Minecraft's cap. Sneaking at 1.295 blocks per second:
        // Minecraft keeps 0.546 of the 0.065 per tick after the ground's friction, 0.035.
        assert_eq!(settle(4.317, Footing::Ground), MAX_BOB);
        assert_eq!(settle(4.317 * 1.3, Footing::Ground), MAX_BOB);
        let sneak = settle(4.317 * 0.3, Footing::Ground);
        assert!((sneak - 0.035_35).abs() < 1e-4, "{sneak}");
        // Wading on a pool's bottom (half the walk) it bobs too, with the water's 0.8.
        let wading = settle(4.317 * 0.5, Footing::Wading);
        assert!((wading - 0.086_34).abs() < 1e-4, "{wading}");
        assert_eq!(settle(4.317, Footing::Air), 0.0);
    }
}

/// A synthetic clock for headless tests: every update advances `Time` by the same frame, so
/// fixed steps happen at known updates.
#[cfg(test)]
pub mod clock {
    use boxel::app::TimeSystems;
    use boxel::prelude::*;
    use std::time::{Duration, Instant};

    /// One fixed step at 64 Hz (exactly `FixedTime::timestep()`).
    pub const STEP: Duration = Duration::from_nanos(15_625_000);

    #[derive(Resource)]
    struct Clock {
        time: Time,
        now: Instant,
        frame: Duration,
    }

    fn synthetic_time(mut clock: ResMut<Clock>, mut time: ResMut<Time>) {
        let now = clock.now + clock.frame;
        clock.now = now;
        clock.time.update_with_instant(now);
        *time = clock.time.clone();
    }

    /// Replaces the app's real clock (`TimePlugin`) with frames of `frame`.
    pub fn fixed_frames(app: &mut App, frame: Duration) {
        let start = Instant::now();
        app.insert_resource(Clock {
            time: Time::new(start),
            now: start,
            frame,
        })
        .add_systems(First, synthetic_time.after(TimeSystems));
    }
}
