//! Physics sandbox (phases P2 and P3 of ADR 0016): walk on a generated world with gravity, jump,
//! drop items that fall, come to rest and get picked up, and stack crates.
//!
//! `cargo run --release --example physics_sandbox -- [GPU options] [options]` (see `--help`).
//! It starts walking on the ground at the origin. Left click grabs the mouse; WASD walks along
//! the view, Space jumps (hold it to keep jumping), G switches to flying (the free camera, as in
//! `voxel_world`) and back, Q drops an item in front of you (a small dynamic sensor box with the
//! item settings: half gravity, more drag), C drops a crate two blocks ahead (a solid box of
//! 0.75: crates stack, and you can push them and stand on them). Walking into an item picks it up
//! (the player's `CollisionStarted` with the item's sensor; a thrown item only after a second).
//! F3 shows the debug overlay with the player's state, a physics line (bodies, pairs, pushes,
//! impacts, step time), the items and the crates. Bodies have no renderer yet: items and crates
//! show on F3 only.
//!
//! `--drop-test` drops 100 items from 40 blocks above the ground once the player stands, and
//! exits 0 when all of them rest on something, or 3 if any still moves after 20 s of simulated
//! time. `--frames N` exits after N rendered frames.
//!
//! Exit codes: 0 success, 1 usage error, 2 no usable GPU, 3 drop test failed, 4 timeout.
#![expect(clippy::print_stdout, clippy::print_stderr, reason = "CLI output")]

#[path = "shared/player.rs"]
mod player;
#[path = "shared/world.rs"]
mod world;

use boxel::camera::FlyCameraSystems;
use boxel::debug_overlay::OverlayLines;
use boxel::gfx::CLI_HELP;
use boxel::physics::{Frozen, PhysicsStats};
use boxel::prelude::*;
use boxel::render::GpuFailed;
use boxel::world::WorldSystems;
use player::{Player, WalkPlugin, follow_player};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};
use world::{World, WorldChoice};

const USAGE: &str = "\
usage: physics_sandbox [GPU options] [options]
  --drop-test             drop 100 items from 40 blocks once the player stands; exit 0 when all
                          rest, 3 if any still moves after 20 s of simulated time
  --frames N              exit after N rendered frames (renders while hidden)
  --timeout SECS          wall-clock limit, exit 4 (default 120 with --drop-test or --frames)
  --fly                   start flying instead of walking (G switches)
  --seed N                terrain seed
  --view-distance N       mesh radius in chunks (default 8)
  --debug-overlay         start with the F3 debug overlay visible (F3 toggles it)
  --help                  this text
keys: WASD walk, Space jump, G fly/walk, Q drop an item, C drop a crate, F3 overlay,
  Escape frees the mouse
exit codes: 0 ok, 1 usage error, 2 no usable GPU, 3 drop test failed, 4 timeout";

/// Default `--timeout` for automation runs.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
/// Extra time the watchdog thread waits after `--timeout` before killing the process.
const WATCHDOG_GRACE: Duration = Duration::from_secs(15);
/// Default mesh radius, chunks.
const DEFAULT_RADIUS: u32 = 8;
/// Drops an item.
const DROP_KEY: KeyCode = KeyCode::KeyQ;
/// Drops a crate.
const CRATE_KEY: KeyCode = KeyCode::KeyC;
/// Half the edge of an item's box, blocks (a quarter-block cube, Minecraft's item size).
const ITEM_HALF: f64 = 0.125;
/// The edge of a crate, blocks.
const CRATE_SIZE: f64 = 0.75;
/// A thrown item can be picked up this many physics steps after it was thrown (1 s).
const PICKUP_DELAY: u64 = 64;
/// `--drop-test`: items per side of the square, their spacing and the drop height above the
/// player's feet, blocks.
const DROP_GRID: u32 = 10;
const DROP_SPACING: f64 = 1.5;
const DROP_HEIGHT: f64 = 40.0;
/// `--drop-test`: fixed steps the items get to come to rest (20 s at 64 Hz).
const DROP_STEPS: u64 = 20 * 64;
/// An item resting on something moves slower than this, blocks per second.
const REST_SPEED: f64 = 0.05;

// ---------------------------------------------------------------------------------------------
// Options

#[derive(Clone, Debug, PartialEq)]
struct Options {
    frames: Option<u64>,
    drop_test: bool,
    timeout: Option<Duration>,
    fly: bool,
    seed: u64,
    view_distance: u32,
    world: WorldChoice,
    debug_overlay: bool,
}

impl Options {
    /// Scripted runs: fly-camera input is off, frames render while hidden and `--timeout`
    /// defaults to [`DEFAULT_TIMEOUT`].
    fn automation(&self) -> bool {
        self.frames.is_some() || self.drop_test
    }
}

fn number(flag: &str, value: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("{flag}: {value:?} is not a non-negative integer"))
}

/// `Ok(None)` for `--help`.
fn parse_options(args: &[String]) -> Result<Option<Options>, String> {
    let mut o = Options {
        frames: None,
        drop_test: false,
        timeout: None,
        fly: false,
        seed: TerrainGenerator::default().seed,
        view_distance: DEFAULT_RADIUS,
        world: WorldChoice::default(),
        debug_overlay: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        if o.world.parse_flag(flag, &mut value)? {
            continue;
        }
        match flag {
            "--help" | "-h" => return Ok(None),
            "--drop-test" => o.drop_test = true,
            "--frames" => o.frames = Some(number(flag, &value()?)?),
            "--timeout" => o.timeout = Some(Duration::from_secs(number(flag, &value()?)?)),
            "--fly" => o.fly = true,
            "--seed" => o.seed = number(flag, &value()?)?,
            "--view-distance" => {
                let radius = number(flag, &value()?)?;
                o.view_distance = u32::try_from(radius)
                    .ok()
                    .filter(|r| (1..=64).contains(r))
                    .ok_or_else(|| format!("{flag}: {radius} is not in 1..=64"))?;
            }
            "--debug-overlay" => o.debug_overlay = true,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    o.world.validate(false)?;
    if o.drop_test && o.fly {
        return Err("--drop-test needs the player walking (no --fly)".to_string());
    }
    if o.timeout.is_none() && o.automation() {
        o.timeout = Some(DEFAULT_TIMEOUT);
    }
    Ok(Some(o))
}

// ---------------------------------------------------------------------------------------------
// Items and the drop test

/// A dropped item.
#[derive(Component, Clone, Copy, Debug, Default)]
struct Item {
    /// The physics step (`PhysicsStats::steps`) from which the player can pick it up.
    pickable_at: u64,
}

/// An item at `pos` moving at `velocity`, pickable from step `pickable_at`: a dynamic sensor box
/// (it lands on blocks and pushes nothing) with the item settings, interpolated.
fn item(pos: WorldPos, velocity: DVec3, pickable_at: u64) -> impl Bundle {
    (
        Item { pickable_at },
        RigidBody::Dynamic,
        Collider::aabb(DVec3::splat(ITEM_HALF)).sensor(),
        BodySettings::item(),
        Interpolated::default(),
        Teleport(pos),
        Velocity(velocity),
    )
}

/// A crate.
#[derive(Component, Clone, Copy, Debug, Default)]
struct Crate;

/// A crate standing at `pos`: a solid dynamic box with twice a player's weight (a player pushing
/// it moves at a third of its speed), interpolated.
fn crate_body(pos: WorldPos) -> impl Bundle {
    (
        Crate,
        RigidBody::Dynamic,
        Collider::standing(CRATE_SIZE, CRATE_SIZE),
        BodySettings {
            push_weight: 2.0,
            ..BodySettings::player()
        },
        Interpolated::default(),
        Teleport(pos),
    )
}

/// The item rests on something: on the ground, not frozen and nearly still.
fn at_rest(velocity: &Velocity, contacts: &Contacts, frozen: bool) -> bool {
    !frozen && contacts.on_ground && velocity.0.length() < REST_SPEED
}

/// `--drop-test`.
#[derive(Resource, Debug, Default, PartialEq)]
enum DropTest {
    /// Not asked for.
    #[default]
    Off,
    /// Waiting for the player to stand on the ground.
    Waiting,
    /// The items fell at physics step `since`.
    Falling { since: u64, items: Vec<Entity> },
    /// Finished with this exit code (0 all at rest, 3 not).
    Done(u8),
}

/// The sandbox systems in `Update`, after the camera follows the player.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
struct SandboxSystems;

/// Items the player touches and how many it picked up.
#[derive(Resource, Debug, Default)]
struct Pickup {
    /// Items overlapping the player (between their `CollisionStarted` and `CollisionEnded`)
    /// that were too fresh to pick up.
    touching: Vec<Entity>,
    /// Items picked up so far.
    picked: u32,
}

/// Items ([`DROP_KEY`] drops one, the player picks them up), crates ([`CRATE_KEY`]), the drop
/// test and the F3 `items` and `crates` lines. Needs `WalkPlugin`.
struct SandboxPlugin {
    drop_test: bool,
}

impl Plugin for SandboxPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(if self.drop_test {
            DropTest::Waiting
        } else {
            DropTest::Off
        })
        .init_resource::<Pickup>()
        .configure_sets(Update, SandboxSystems.after(follow_player))
        .add_systems(
            Update,
            (drop_items, drop_crates, pick_up, drop_test, item_lines)
                .chain()
                .in_set(SandboxSystems),
        );
    }
}

/// The active camera's eye while the fly camera takes input.
fn active_eye<'a>(
    cameras: impl IntoIterator<Item = (&'a Camera, &'a Transform, &'a FlyCamera)>,
) -> Option<&'a Transform> {
    cameras
        .into_iter()
        .find(|(camera, _, fly)| camera.active && fly.enabled)
        .map(|(_, eye, _)| eye)
}

/// [`DROP_KEY`] throws an item from below the active camera's eye along its view, as Minecraft
/// does (6 blocks/s forward, 2 up). The player can pick it up after [`PICKUP_DELAY`] steps.
fn drop_items(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    stats: Res<PhysicsStats>,
    cameras: Query<(&Camera, &Transform, &FlyCamera)>,
) {
    if !keys.just_pressed(DROP_KEY) {
        return;
    }
    let Some(eye) = active_eye(cameras) else {
        return;
    };
    let forward = eye.forward().as_dvec3();
    let from = WorldPos::from_dvec3(eye.translation).offset(forward * 0.5 - DVec3::Y * 0.3);
    commands.spawn(item(
        from,
        forward * 6.0 + DVec3::Y * 2.0,
        stats.steps + PICKUP_DELAY,
    ));
}

/// [`CRATE_KEY`] drops a crate two blocks ahead of the active camera (along its heading) and a
/// block above the eye: crates dropped in one place stack.
fn drop_crates(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    cameras: Query<(&Camera, &Transform, &FlyCamera)>,
) {
    if !keys.just_pressed(CRATE_KEY) {
        return;
    }
    let Some(eye) = active_eye(cameras) else {
        return;
    };
    let forward = eye.forward().as_dvec3();
    let ahead = DVec3::new(forward.x, 0.0, forward.z).normalize_or(DVec3::NEG_Z);
    let at = WorldPos::from_dvec3(eye.translation).offset(ahead * 2.0 + DVec3::Y);
    commands.spawn(crate_body(at));
}

/// The player picks up the items it touches: a `CollisionStarted` between the player's solid box
/// and an item's sensor, at once, or once the item is old enough while they still touch.
fn pick_up(
    mut commands: Commands,
    mut started: MessageReader<CollisionStarted>,
    mut ended: MessageReader<CollisionEnded>,
    stats: Res<PhysicsStats>,
    players: Query<Entity, With<Player>>,
    items: Query<&Item>,
    mut pickup: ResMut<Pickup>,
) {
    let Some(player) = players.iter().next() else {
        started.clear();
        ended.clear();
        pickup.touching.clear();
        return;
    };
    let pickup = &mut *pickup;
    for event in started.read() {
        if let Some(other) = event.other(player)
            && items.contains(other)
            && !pickup.touching.contains(&other)
        {
            pickup.touching.push(other);
        }
    }
    for event in ended.read() {
        if let Some(other) = event.other(player) {
            pickup.touching.retain(|&item| item != other);
        }
    }
    let picked = &mut pickup.picked;
    pickup.touching.retain(|&entity| match items.get(entity) {
        Ok(item) if stats.steps >= item.pickable_at => {
            commands.entity(entity).despawn();
            *picked += 1;
            false
        }
        Ok(_) => true,
        Err(_) => false,
    });
}

/// Drops the test's items once the player stands, then waits for all of them to rest.
fn drop_test(
    mut commands: Commands,
    mut test: ResMut<DropTest>,
    stats: Res<PhysicsStats>,
    players: Query<(&BodyPosition, &Contacts), With<Player>>,
    items: Query<(&BodyPosition, &Velocity, &Contacts, Has<Frozen>), With<Item>>,
) {
    match &*test {
        DropTest::Waiting => {
            let Some((feet, contacts)) = players.iter().next() else {
                return;
            };
            if !contacts.on_ground {
                return;
            }
            let half = f64::from(DROP_GRID - 1) * 0.5;
            let mut dropped = Vec::new();
            for i in 0..DROP_GRID {
                for j in 0..DROP_GRID {
                    let offset = DVec3::new(
                        (f64::from(i) - half) * DROP_SPACING,
                        DROP_HEIGHT,
                        (f64::from(j) - half) * DROP_SPACING,
                    );
                    dropped.push(
                        commands
                            .spawn(item(feet.0.offset(offset), DVec3::ZERO, 0))
                            .id(),
                    );
                }
            }
            println!(
                "physics_sandbox: drop test: {} items dropped from {DROP_HEIGHT} blocks",
                dropped.len()
            );
            *test = DropTest::Falling {
                since: stats.steps,
                items: dropped,
            };
        }
        DropTest::Falling {
            since,
            items: dropped,
        } => {
            let steps = stats.steps.saturating_sub(*since);
            let moving = dropped.iter().find(|&&entity| {
                !items
                    .get(entity)
                    .is_ok_and(|(_, velocity, contacts, frozen)| {
                        at_rest(velocity, contacts, frozen)
                    })
            });
            match moving {
                None => {
                    println!(
                        "physics_sandbox: drop test: all {} items at rest after {steps} steps \
                         ({:.2} s simulated)",
                        dropped.len(),
                        steps as f64 / 64.0
                    );
                    *test = DropTest::Done(0);
                }
                Some(&entity) if steps > DROP_STEPS => {
                    let state = items.get(entity).ok().map(|(p, v, c, f)| (p.0, v.0, *c, f));
                    eprintln!(
                        "physics_sandbox: drop test failed: item {entity} still moving after \
                         {steps} steps: {state:?}"
                    );
                    *test = DropTest::Done(3);
                }
                Some(_) => {}
            }
        }
        DropTest::Off | DropTest::Done(_) => {}
    }
}

/// The F3 lines `items` (how many there are, how many rest, how many were picked up) and
/// `crates` (how many).
fn item_lines(
    items: Query<(&Velocity, &Contacts, Has<Frozen>), With<Item>>,
    crates: Query<(), With<Crate>>,
    pickup: Res<Pickup>,
    lines: Option<ResMut<OverlayLines>>,
) {
    let Some(mut lines) = lines else {
        return;
    };
    let total = items.iter().count();
    let resting = items
        .iter()
        .filter(|(velocity, contacts, frozen)| at_rest(velocity, contacts, *frozen))
        .count();
    let items_line = format!(
        "Items {total}  at rest {resting}  picked {}  (Q drops)",
        pickup.picked
    );
    let crates_line = format!("Crates {}  (C drops)", crates.iter().count());
    for (key, line) in [("items", items_line), ("crates", crates_line)] {
        if lines.get(key) != Some(line.as_str()) {
            lines.set(key, line);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The windowed run

#[derive(Resource)]
struct Run {
    options: Options,
    started: Instant,
    exiting: bool,
}

fn spawn_camera(mut commands: Commands, run: Res<Run>) {
    let radius = run.options.view_distance;
    commands.spawn((
        Camera {
            view_distance: radius as f32 * 32.0,
            ..Camera::default()
        },
        Transform::from_yaw_pitch(DVec3::new(0.5, 120.0, 0.5), 0.0, -0.2),
        FlyCamera {
            enabled: !run.options.automation(),
            pitch: -0.2,
            ..FlyCamera::default()
        },
        ChunkViewer::new(radius),
    ));
}

/// The exit: no GPU (2), the drop test's result, `--frames` reached (0, or 3 if the drop test
/// is still running), or the timeout (4).
fn finish_run(
    mut run: ResMut<Run>,
    test: Res<DropTest>,
    info: Res<RenderInfo>,
    mut failed: MessageReader<GpuFailed>,
    mut exit: MessageWriter<AppExit>,
) {
    let pending = matches!(*test, DropTest::Waiting | DropTest::Falling { .. });
    let code = if let Some(event) = failed.read().last() {
        eprintln!("physics_sandbox: no usable GPU: {}", event.error);
        Some(2)
    } else if let DropTest::Done(code) = *test {
        Some(code)
    } else if run
        .options
        .frames
        .is_some_and(|frames| info.frames_rendered >= frames)
    {
        Some(if pending { 3 } else { 0 })
    } else if run
        .options
        .timeout
        .is_some_and(|timeout| run.started.elapsed() >= timeout)
    {
        eprintln!("physics_sandbox: timeout");
        Some(4)
    } else {
        None
    };
    if let Some(code) = code
        && !run.exiting
    {
        run.exiting = true;
        println!(
            "physics_sandbox: frames_rendered={} elapsed={:.1}s exit={code}",
            info.frames_rendered,
            run.started.elapsed().as_secs_f64()
        );
        exit.write(AppExit::from_code(code));
    }
}

/// Kills the process if `main` has not returned `limit` after start (the event loop stopped
/// updating, so the in-app timeout cannot fire). Dropping the returned sender disarms it.
fn spawn_watchdog(limit: Duration) -> Sender<()> {
    let (disarm, armed) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        if armed.recv_timeout(limit) == Err(RecvTimeoutError::Timeout) {
            eprintln!("physics_sandbox: watchdog: still running after {limit:?}, exiting with 4");
            std::process::exit(4);
        }
    });
    disarm
}

fn main() -> AppExit {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let help = || format!("{USAGE}\nworld options:\n{}", world::usage(World::Forest));
    let (gpu, gpu_config, rest) = match GpuPreferences::from_args(&args) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("physics_sandbox: {e}\n\n{}\n\n{CLI_HELP}", help());
            return AppExit::from_code(1);
        }
    };
    let options = match parse_options(&rest) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!("{}\n\n{CLI_HELP}", help());
            return AppExit::Success;
        }
        Err(message) => {
            eprintln!("physics_sandbox: {message}\n\n{}\n\n{CLI_HELP}", help());
            return AppExit::from_code(1);
        }
    };
    let _watchdog = options
        .timeout
        .map(|timeout| spawn_watchdog(timeout.saturating_add(WATCHDOG_GRACE)));

    let (arena_quads, max_chunk_slots) = options.world.render_capacity();
    let plugins = DefaultPlugins
        .set(WindowPlugin {
            primary: WindowSettings {
                title: "boxel physics_sandbox".to_string(),
                ..WindowSettings::default()
            },
            ..WindowPlugin::default()
        })
        .set(RenderPlugin {
            settings: RenderSettings {
                app_name: "physics_sandbox".to_string(),
                gpu,
                gpu_config,
                render_when_hidden: options.automation(),
                arena_quads,
                max_chunk_slots,
                ..RenderSettings::default()
            },
        })
        .set(WorldPlugin {
            generator: options.world.generator(options.seed),
            ..WorldPlugin::default()
        })
        .set(DebugOverlayPlugin {
            visible: options.debug_overlay,
            frame_keys: None,
        });
    let walk = WalkPlugin {
        start_walking: !options.fly,
    };
    let sandbox = SandboxPlugin {
        drop_test: options.drop_test,
    };
    let mut app = App::new();
    app.add_plugins(plugins)
        .add_plugins((
            LogDiagnosticsPlugin::default(),
            FlyCameraPlugin,
            PhysicsPlugin::default(),
            walk,
            sandbox,
        ))
        // The camera moves first; the world then computes interest from the new position.
        .configure_sets(Update, FlyCameraSystems.before(WorldSystems::Interest))
        .insert_resource(Run {
            options,
            started: Instant::now(),
            exiting: false,
        })
        .add_systems(Startup, spawn_camera)
        .add_systems(Update, finish_run.after(SandboxSystems));
    app.run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel::ecs::schedule::{LogLevel, ScheduleBuildSettings};
    use boxel::world::jobs::InlineExecutor;
    use boxel::worldgen::FlatGenerator;
    use std::sync::Arc;

    fn parse(args: &[&str]) -> Result<Option<Options>, String> {
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        parse_options(&args)
    }

    #[test]
    fn options() {
        let o = parse(&[]).expect("valid").expect("not --help");
        assert!(!o.automation() && !o.fly && o.timeout.is_none());
        assert_eq!(o.world.label(), "forest");
        let o = parse(&["--drop-test", "--world", "flat"])
            .expect("valid")
            .expect("not --help");
        assert!(o.automation());
        assert_eq!(o.timeout, Some(DEFAULT_TIMEOUT));
        let o = parse(&["--frames=30", "--timeout", "9", "--fly"])
            .expect("valid")
            .expect("not --help");
        assert_eq!(
            (o.frames, o.timeout, o.fly),
            (Some(30), Some(Duration::from_secs(9)), true)
        );
        assert_eq!(parse(&["--help"]), Ok(None));
        for bad in [
            &["--drop-test", "--fly"][..],
            &["--view-distance", "0"],
            &["--bodies", "3"],
            &["--caves", "off"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    /// The sandbox without a window: flat world (stone y 0..=2, grass at y = 3, inline jobs),
    /// physics, walking (from the start), the sandbox systems, a fly camera at (0.5, 20, 0.5)
    /// looking north, a synthetic clock (one fixed step per update) and a strict `Update`.
    fn sandbox_app(drop_test: bool) -> App {
        let mut app = App::new();
        app.add_plugins((
            TimePlugin,
            InputPlugin,
            FlyCameraPlugin,
            WorldPlugin {
                generator: Arc::new(FlatGenerator::new(vec![
                    ("boxel:stone".to_string(), 3),
                    ("boxel:grass".to_string(), 1),
                ])),
                executor: Arc::new(InlineExecutor),
                ..WorldPlugin::default()
            },
            PhysicsPlugin::default(),
            WalkPlugin {
                start_walking: true,
            },
            SandboxPlugin { drop_test },
        ))
        .init_resource::<OverlayLines>()
        .configure_sets(Update, FlyCameraSystems.before(WorldSystems::Interest))
        .edit_schedule(Update, |schedule| {
            schedule.set_build_settings(ScheduleBuildSettings {
                ambiguity_detection: LogLevel::Error,
                ..ScheduleBuildSettings::default()
            });
        });
        player::clock::fixed_frames(&mut app, player::clock::STEP);
        app.world_mut().spawn((
            Camera::default(),
            FlyCamera::default(),
            Transform::from_translation(DVec3::new(0.5, 20.0, 0.5)),
            ChunkViewer::new(1).with_vertical_radius(1),
        ));
        app.finish();
        app.cleanup();
        app
    }

    fn items(app: &mut App) -> Vec<(WorldPos, DVec3)> {
        let mut items = app
            .world_mut()
            .query_filtered::<(&BodyPosition, &Velocity), With<Item>>();
        items.iter(app.world()).map(|(p, v)| (p.0, v.0)).collect()
    }

    #[test]
    fn the_drop_test_brings_every_item_to_rest_on_the_ground() {
        let mut app = sandbox_app(true);
        for _ in 0..64 * 30 {
            app.update();
            if matches!(*app.world().resource::<DropTest>(), DropTest::Done(_)) {
                break;
            }
        }
        assert_eq!(*app.world().resource::<DropTest>(), DropTest::Done(0));
        let items = items(&mut app);
        assert_eq!(items.len(), (DROP_GRID * DROP_GRID) as usize);
        // Each rests exactly on the grass (top at y = 4), its center half an item above.
        for (pos, velocity) in items {
            assert_eq!((pos.block.y, pos.frac.y), (4, ITEM_HALF), "{pos:?}");
            assert_eq!(velocity, DVec3::ZERO);
        }
        assert_eq!(
            app.world().resource::<OverlayLines>().get("items"),
            Some("Items 100  at rest 100  picked 0  (Q drops)")
        );
    }

    /// Presses or releases `key` (seen by the next update).
    fn key(app: &mut App, key: KeyCode, pressed: bool) {
        use boxel::window::{ButtonState, KeyboardInput};
        let state = if pressed {
            ButtonState::Pressed
        } else {
            ButtonState::Released
        };
        app.world_mut()
            .resource_mut::<Messages<KeyboardInput>>()
            .write(KeyboardInput {
                key,
                state,
                repeat: false,
            });
    }

    /// Taps `k`: pressed for one update, released in the next.
    fn tap(app: &mut App, k: KeyCode) {
        key(app, k, true);
        app.update();
        key(app, k, false);
        app.update();
    }

    fn line(app: &App, key: &str) -> String {
        app.world()
            .resource::<OverlayLines>()
            .get(key)
            .unwrap_or_default()
            .to_string()
    }

    #[test]
    fn crates_dropped_in_one_place_stack() {
        let mut app = sandbox_app(false);
        for _ in 0..64 {
            app.update();
        }
        // The player stands at (0.5, 4, 0.5) looking north: each crate falls 2 blocks ahead.
        for _ in 0..3 {
            tap(&mut app, CRATE_KEY);
            for _ in 0..64 {
                app.update();
            }
        }
        let mut crates = app
            .world_mut()
            .query_filtered::<(&BodyPosition, &Velocity, &Contacts), With<Crate>>();
        let mut stack: Vec<_> = crates
            .iter(app.world())
            .map(|(p, v, c)| (p.0, v.0, *c))
            .collect();
        stack.sort_by(|a, b| a.0.to_dvec3().y.total_cmp(&b.0.to_dvec3().y));
        assert_eq!(stack.len(), 3);
        for (level, (pos, velocity, contacts)) in stack.into_iter().enumerate() {
            // Exactly on the grass (top at y = 4) and on each other.
            assert_eq!(
                pos.to_dvec3(),
                DVec3::new(0.5, 4.0 + CRATE_SIZE * level as f64, -1.5),
                "level {level}"
            );
            assert_eq!(velocity, DVec3::ZERO);
            assert!(contacts.on_ground);
        }
        assert_eq!(line(&app, "crates"), "Crates 3  (C drops)");
    }

    #[test]
    fn walking_into_an_item_picks_it_up() {
        let mut app = sandbox_app(false);
        for _ in 0..64 {
            app.update();
        }
        // An item lying 3 blocks north of the player, pickable at once.
        app.world_mut().spawn(item(
            WorldPos::from_dvec3(DVec3::new(0.5, 4.0 + ITEM_HALF, -2.5)),
            DVec3::ZERO,
            0,
        ));
        for _ in 0..8 {
            app.update();
        }
        assert_eq!(items(&mut app).len(), 1);
        assert_eq!(
            line(&app, "items"),
            "Items 1  at rest 1  picked 0  (Q drops)"
        );
        // W walks north into it: the player picks it up.
        key(&mut app, KeyCode::KeyW, true);
        for _ in 0..64 {
            app.update();
        }
        key(&mut app, KeyCode::KeyW, false);
        app.update();
        assert!(items(&mut app).is_empty());
        assert_eq!(app.world().resource::<Pickup>().picked, 1);
        assert_eq!(
            line(&app, "items"),
            "Items 0  at rest 0  picked 1  (Q drops)"
        );
    }

    #[test]
    fn q_throws_an_item_along_the_view() {
        use boxel::window::{ButtonState, KeyboardInput};
        let mut app = sandbox_app(false);
        for _ in 0..64 {
            app.update();
        }
        let mut players = app
            .world_mut()
            .query_filtered::<&BodyPosition, With<Player>>();
        let feet = players.single(app.world()).expect("walking").0;
        assert_eq!(feet, WorldPos::from_dvec3(DVec3::new(0.5, 4.0, 0.5)));
        for state in [ButtonState::Pressed, ButtonState::Released] {
            app.world_mut()
                .resource_mut::<Messages<KeyboardInput>>()
                .write(KeyboardInput {
                    key: DROP_KEY,
                    state,
                    repeat: false,
                });
            app.update();
        }
        for _ in 0..128 {
            app.update();
        }
        // Thrown away from the player: not picked up.
        let items = items(&mut app);
        assert_eq!(items.len(), 1);
        let (pos, velocity) = items[0];
        // Thrown north (-Z) from the eye, it lands on the grass a few blocks away.
        assert_eq!((pos.block.y, pos.frac.y), (4, ITEM_HALF), "{pos:?}");
        assert!(pos.to_dvec3().z < -1.0, "{pos:?}");
        assert!((pos.to_dvec3().x - 0.5).abs() < 1e-9, "{pos:?}");
        assert!(velocity.length() < REST_SPEED);
    }
}
