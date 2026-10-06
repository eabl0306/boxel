//! Walking for the examples (physics phase P2, ADR 0016): a player body with a
//! `CharacterController` that the camera follows at eye height.
//!
//! G switches between flying (the free fly camera) and walking. The player is a dynamic box of
//! 0.6 x 1.8 x 0.6 blocks with gravity: WASD walks along the camera's heading and Space jumps
//! (held, it jumps again on landing). The camera keeps its mouse look; its own WASD movement is
//! paused while walking.
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
use boxel::physics::{Frozen, PhysicsStats};
use boxel::prelude::*;
use boxel::world::WorldSystems;

/// Eye height above the feet, blocks (Minecraft's).
pub const EYE_HEIGHT: f64 = 1.62;
/// The player's box: width and height, blocks.
pub const PLAYER_SIZE: (f64, f64) = (0.6, 1.8);
/// Switches flying and walking.
pub const WALK_KEY: KeyCode = KeyCode::KeyG;
/// How far down the first landing looks for ground, blocks.
const LAND_REACH: f64 = 512.0;

/// The player body (only while walking).
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Player;

/// Walking or flying.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct Walking {
    /// Walking (true) or flying (false). [`WALK_KEY`] flips it; the player body follows.
    pub on: bool,
    /// The next switch to walking puts the player on the ground under the camera instead of
    /// dropping it from the camera (the start of the sandbox).
    pub land: bool,
    /// The fly camera's speed, kept while walking (its WASD must not move the camera then).
    fly_speed: Option<f32>,
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
            fly_speed: None,
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
    )
}

/// [`WALK_KEY`] flips [`Walking::on`] (while the fly camera takes input); then the player body
/// is spawned under the active camera's eye, or removed. The camera's fly speed is 0 while
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
/// player's `MoveIntent::wish`, and Space into its jump latch (held, or tapped within this
/// frame: a press and release in one long frame still jumps). Nothing while the fly camera
/// takes no input (automation).
pub fn sample_input(
    keys: Res<ButtonInput<KeyCode>>,
    cameras: Query<(&Camera, &FlyCamera)>,
    mut players: Query<&mut MoveIntent, With<Player>>,
) {
    let Some((_, fly)) = cameras.iter().find(|(camera, _)| camera.active) else {
        return;
    };
    let (forward, right, jump) = if fly.enabled {
        (
            axis(&keys, KeyCode::KeyW, KeyCode::KeyS),
            axis(&keys, KeyCode::KeyD, KeyCode::KeyA),
            keys.pressed(KeyCode::Space) || keys.just_pressed(KeyCode::Space),
        )
    } else {
        (0.0, 0.0, false)
    };
    // The fly camera's heading: -Z and +X turned by the yaw around +Y (x is world X, y world Z).
    let (sin, cos) = fly.yaw.sin_cos();
    let wish = (Vec2::new(-sin, -cos) * forward + Vec2::new(cos, -sin) * right).normalize_or_zero();
    for mut intent in &mut players {
        if intent.wish != wish {
            intent.wish = wish;
        }
        if jump && !intent.jump {
            intent.jump = true;
        }
    }
}

/// The active camera at the player's eye (its interpolated `Transform` plus [`EYE_HEIGHT`]),
/// and the F3 lines `move` (mode, ground, speed) and `physics` (`PhysicsStats`).
pub fn follow_player(
    walking: Res<Walking>,
    stats: Res<PhysicsStats>,
    players: Query<
        (&Transform, &Velocity, &Contacts, Has<Frozen>),
        (With<Player>, Without<FlyCamera>),
    >,
    mut cameras: Query<(&Camera, &mut Transform), With<FlyCamera>>,
    lines: Option<ResMut<OverlayLines>>,
) {
    let player = players.iter().next();
    if let Some((body, ..)) = player {
        let eye = body.translation + DVec3::new(0.0, EYE_HEIGHT, 0.0);
        for (camera, mut transform) in &mut cameras {
            if camera.active && transform.translation != eye {
                transform.translation = eye;
            }
        }
    }
    let Some(mut lines) = lines else {
        return;
    };
    let mode = match player {
        Some((_, velocity, contacts, frozen)) => {
            let state = if frozen {
                "frozen (chunk not loaded)"
            } else if contacts.on_ground {
                "on ground"
            } else {
                "in the air"
            };
            let speed = boxel::math::DVec2::new(velocity.0.x, velocity.0.z).length();
            format!("Walk (G flies)  {state}  speed {speed:.2} b/s")
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
