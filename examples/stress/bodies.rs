//! `--bodies N --body-kind item|mob --projectiles M`: physics bodies during a stress run (ADR 0016
//! phase P4), to measure what they add to the frame.
//!
//! When the world first settles, `N` bodies are spawned on the ground around the start
//! (`--camera`), as spread as in the physics benches (one per 9.2 blocks of ground: 10 000 on a
//! square of 304 blocks):
//! - `item` (the default): dropped items (small sensor boxes with the item settings) a little
//!   above the ground; they land and fall asleep, the case of a field full of drops;
//! - `mob`: walkers (player-size solid boxes with a character controller) that change direction
//!   and sometimes jump every 48 steps, so they never sleep.
//!
//! `M` arrows are shot over the same square in seeded directions, all at once when the world
//! settles, and then each one again from its start every 2 s (128 steps), the shots spread
//! evenly over those 2 s: a flight lasts about 0.6 to 1.25 s, so about half of `M` are in the
//! air at any time. Bodies have no renderer: they cost CPU only. With bodies or projectiles the
//! app gets `PhysicsPlugin` (which also caps the fixed steps per frame at 3); without them
//! nothing changes.

use boxel::physics::{
    BodySettings, CharacterController, Collider, Interpolated, MoveIntent, PhysicsPlugin,
    PhysicsQuery, Projectile, RigidBody, Teleport, Velocity, VoxelFilter,
};
use boxel::prelude::*;
use boxel::world::WorldStats;

/// The options' lines in `--help`.
pub const USAGE: &str = "\
  --bodies N          physics bodies spawned on the ground around the start when the world
                      settles (default 0; see --body-kind)
  --body-kind KIND    item (dropped items that land and fall asleep, the default) or mob
                      (walkers that wander and never sleep)
  --projectiles M     arrows shot over the same area, each shot again every 2 s (default 0)";

/// Ground per body, blocks squared (the physics benches' spread).
const GROUND_PER_BODY: f64 = 9.2;
/// Steps between two shots of one arrow (2 s at 64 Hz).
const SHOT_STEPS: u64 = 128;
/// Steps between two changes of a mob's direction.
const WANDER_STEPS: u64 = 48;

/// What `--bodies` spawns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BodyKind {
    /// Dropped items: they land and fall asleep.
    #[default]
    Item,
    /// Walkers that wander: always awake.
    Mob,
}

impl BodyKind {
    pub fn name(self) -> &'static str {
        match self {
            BodyKind::Item => "item",
            BodyKind::Mob => "mob",
        }
    }
}

/// `--bodies`, `--body-kind` and `--projectiles`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BodyOptions {
    pub bodies: u32,
    pub kind: BodyKind,
    pub projectiles: u32,
}

impl BodyOptions {
    /// Parses one of the options; `Ok(false)` when `flag` is not one of them.
    pub fn parse_flag(
        &mut self,
        flag: &str,
        value: &mut impl FnMut() -> Result<String, String>,
    ) -> Result<bool, String> {
        let count = |v: String| {
            v.parse::<u32>()
                .ok()
                .filter(|n| *n <= 1_000_000)
                .ok_or_else(|| format!("{flag}: {v:?} is not a count in 0..=1000000"))
        };
        match flag {
            "--bodies" => self.bodies = count(value()?)?,
            "--projectiles" => self.projectiles = count(value()?)?,
            "--body-kind" => {
                let v = value()?;
                self.kind = match v.as_str() {
                    "item" => BodyKind::Item,
                    "mob" => BodyKind::Mob,
                    _ => return Err(format!("{flag}: {v:?} is not item or mob")),
                };
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Any body or projectile.
    pub fn enabled(&self) -> bool {
        self.bodies > 0 || self.projectiles > 0
    }

    /// `1000 item, 50 projectiles` (empty without bodies), for the summary.
    pub fn label(&self) -> String {
        if !self.enabled() {
            return String::new();
        }
        format!(
            "{} {}, {} projectiles",
            self.bodies,
            self.kind.name(),
            self.projectiles
        )
    }
}

/// Adds physics and the bodies of [`BodyOptions`] around `center` (the start of the camera).
pub struct BodiesPlugin {
    pub options: BodyOptions,
    pub center: DVec3,
}

impl Plugin for BodiesPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(PhysicsPlugin::default())
            .insert_resource(Spawner {
                options: self.options,
                center: self.center,
                spawned: false,
            })
            .init_resource::<Steps>()
            .add_systems(Update, spawn_when_settled)
            .add_systems(FixedUpdate, (count_steps, wander, shoot_again).chain());
    }
}

/// What is left to spawn.
#[derive(Resource)]
struct Spawner {
    options: BodyOptions,
    center: DVec3,
    spawned: bool,
}

/// Fixed steps since start (the clock of the mobs and the arrows).
#[derive(Resource, Default)]
struct Steps(u64);

/// A wandering mob and its seed.
#[derive(Component, Clone, Copy)]
struct Mob(u64);

/// An arrow: where it is shot from, with what velocity, and the step its 2 s period started at
/// (the arrows' periods are spread over 2 s).
#[derive(Component, Clone, Copy)]
struct Shot {
    from: WorldPos,
    velocity: DVec3,
    at: u64,
}

/// `SplitMix64`.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// `bits` as a number in `[-1, 1]`.
fn unit(bits: u64) -> f64 {
    (bits % 2001) as f64 / 1000.0 - 1.0
}

/// The side of the square `n` bodies spread over, blocks.
fn side(n: u32) -> f64 {
    (f64::from(n.max(1)) * GROUND_PER_BODY).sqrt().ceil()
}

/// The ground under `(x, z)`, looking down from `top` (`None` when nothing loaded is there).
fn ground(query: &PhysicsQuery, x: f64, top: f64, z: f64) -> Option<WorldPos> {
    query
        .raycast_voxels(
            WorldPos::from_dvec3(DVec3::new(x, top, z)),
            DVec3::NEG_Y,
            512.0,
            VoxelFilter::Collision,
        )
        .hit()
        .map(|hit| hit.point)
}

/// Spawns the bodies and arrows the first time the world is settled.
fn spawn_when_settled(
    mut commands: Commands,
    mut spawner: ResMut<Spawner>,
    stats: Res<WorldStats>,
    steps: Res<Steps>,
    query: PhysicsQuery,
) {
    if spawner.spawned || stats.chunks == 0 || !stats.is_settled() {
        return;
    }
    spawner.spawned = true;
    let Spawner {
        options, center, ..
    } = *spawner;
    let half = side(options.bodies) * 0.5;
    let top = center.y + 32.0;
    let mut placed = 0;
    for i in 0..u64::from(options.bodies) {
        let r = mix(i);
        let (x, z) = (center.x + unit(r) * half, center.z + unit(r >> 16) * half);
        let Some(at) = ground(&query, x, top, z) else {
            continue;
        };
        placed += 1;
        match options.kind {
            BodyKind::Item => {
                let height = 0.5 + (r >> 32) as f64 % 3.0;
                commands.spawn((
                    RigidBody::Dynamic,
                    Collider::aabb(DVec3::splat(0.125)).sensor(),
                    BodySettings::item(),
                    Interpolated::default(),
                    Teleport(at.offset(DVec3::new(0.0, height, 0.0))),
                ));
            }
            BodyKind::Mob => {
                commands.spawn((
                    RigidBody::Dynamic,
                    Collider::standing(0.6, 1.8),
                    CharacterController::default(),
                    MoveIntent::default(),
                    Interpolated::default(),
                    Teleport(at),
                    Mob(r),
                ));
            }
        }
    }
    let half = side(options.projectiles.max(options.bodies)) * 0.5;
    let mut shots = 0;
    for i in 0..u64::from(options.projectiles) {
        let r = mix(i ^ 0xA77A_77A7);
        let (x, z) = (center.x + unit(r) * half, center.z + unit(r >> 16) * half);
        let Some(at) = ground(&query, x, top, z) else {
            continue;
        };
        shots += 1;
        let from = at.offset(DVec3::new(0.0, 12.0, 0.0));
        let velocity = DVec3::new(
            unit(r >> 24) * 45.0,
            unit(r >> 40) * 10.0,
            unit(r >> 48) * 45.0,
        );
        commands.spawn((
            RigidBody::Dynamic,
            Projectile::arrow(),
            Collider::aabb(DVec3::splat(0.05)),
            BodySettings::arrow(),
            Interpolated::default(),
            Teleport(from),
            Velocity(velocity),
            Shot {
                from,
                velocity,
                // The first shot is now; the next ones are spread over the 2 s.
                at: steps.0 + i % SHOT_STEPS,
            },
        ));
    }
    println!(
        "stress: spawned {placed} {} bodies and {shots} projectiles around ({:.0}, {:.0})",
        options.kind.name(),
        center.x,
        center.z
    );
}

fn count_steps(mut steps: ResMut<Steps>) {
    steps.0 += 1;
}

/// Every [`WANDER_STEPS`] steps each mob picks a direction from its seed, and one in four
/// jumps.
fn wander(steps: Res<Steps>, mut mobs: Query<(&Mob, &mut MoveIntent)>) {
    if !steps.0.is_multiple_of(WANDER_STEPS) {
        return;
    }
    let turn = steps.0 / WANDER_STEPS;
    for (mob, mut intent) in &mut mobs {
        let r = mix(mob.0 ^ turn.wrapping_mul(0x1000_0001));
        intent.wish = Vec2::new(unit(r) as f32, unit(r >> 16) as f32);
        if (r >> 32).is_multiple_of(4) {
            intent.jump = true;
        }
    }
}

/// Each arrow is shot again from its start every [`SHOT_STEPS`] steps (stuck ones wake up).
fn shoot_again(
    mut commands: Commands,
    steps: Res<Steps>,
    mut shots: Query<(Entity, &mut Shot, &mut Velocity)>,
) {
    for (entity, mut shot, mut velocity) in &mut shots {
        if steps.0 < shot.at + SHOT_STEPS {
            continue;
        }
        shot.at = steps.0;
        velocity.0 = shot.velocity;
        commands.entity(entity).insert(Teleport(shot.from));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel::app::TimeSystems;
    use boxel::physics::{PhysicsStats, ProjectileHit, Sleeping};
    use boxel::world::jobs::InlineExecutor;
    use boxel::worldgen::FlatGenerator;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn parse(args: &[&str]) -> Result<BodyOptions, String> {
        let mut o = BodyOptions::default();
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            let mut value = || {
                it.next()
                    .map(ToString::to_string)
                    .ok_or_else(|| format!("{flag} needs a value"))
            };
            if !o.parse_flag(flag, &mut value)? {
                return Err(format!("unknown {flag}"));
            }
        }
        Ok(o)
    }

    #[test]
    fn options() {
        let o = parse(&[]).expect("valid");
        assert!(!o.enabled());
        assert_eq!(o.label(), "");
        let o = parse(&["--bodies", "1000", "--projectiles", "50"]).expect("valid");
        assert_eq!(
            (o.bodies, o.projectiles, o.kind),
            (1000, 50, BodyKind::Item)
        );
        assert_eq!(o.label(), "1000 item, 50 projectiles");
        let o = parse(&["--body-kind", "mob", "--bodies", "3"]).expect("valid");
        assert_eq!(o.label(), "3 mob, 0 projectiles");
        for bad in [
            &["--bodies", "-1"][..],
            &["--bodies", "x"],
            &["--projectiles", "2000000"],
            &["--body-kind", "crate"],
            &["--bodies"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
        assert_eq!(side(10_000), 304.0);
    }

    #[derive(Resource)]
    struct Clock {
        time: Time,
        now: Instant,
    }

    /// One fixed step per update.
    fn tick(mut clock: ResMut<Clock>, mut time: ResMut<Time>) {
        clock.now += Duration::from_nanos(15_625_000);
        let now = clock.now;
        clock.time.update_with_instant(now);
        *time = clock.time.clone();
    }

    #[derive(Resource, Default)]
    struct Hits(usize);

    fn count_hits(mut hits: MessageReader<ProjectileHit>, mut count: ResMut<Hits>) {
        count.0 += hits.read().count();
    }

    /// A flat world (ground at y = 4) with inline jobs, a viewer at the center and the bodies.
    fn app(options: BodyOptions) -> App {
        let mut app = App::new();
        let start = Instant::now();
        app.add_plugins((
            TimePlugin,
            WorldPlugin {
                generator: Arc::new(FlatGenerator::new(vec![
                    ("boxel:stone".to_string(), 3),
                    ("boxel:grass".to_string(), 1),
                ])),
                executor: Arc::new(InlineExecutor),
                ..WorldPlugin::default()
            },
            BodiesPlugin {
                options,
                center: DVec3::new(0.5, 10.0, 0.5),
            },
        ))
        .insert_resource(Clock {
            time: Time::new(start),
            now: start,
        })
        .init_resource::<Hits>()
        .add_systems(First, tick.after(TimeSystems))
        .add_systems(Update, count_hits);
        app.world_mut().spawn((
            Transform::from_translation(DVec3::new(0.5, 10.0, 0.5)),
            ChunkViewer::new(2).with_vertical_radius(1),
        ));
        app.finish();
        app.cleanup();
        app
    }

    fn count<F: boxel::ecs::query::QueryFilter>(app: &mut App) -> usize {
        let world = app.world_mut();
        world.query_filtered::<(), F>().iter(world).count()
    }

    #[test]
    fn items_land_and_fall_asleep() {
        let mut app = app(parse(&["--bodies", "200"]).expect("valid"));
        for _ in 0..400 {
            app.update();
        }
        assert_eq!(count::<With<RigidBody>>(&mut app), 200);
        assert_eq!(count::<With<Sleeping>>(&mut app), 200);
        let stats = *app.world().resource::<PhysicsStats>();
        assert_eq!((stats.bodies, stats.sleeping, stats.dynamic), (200, 200, 0));
    }

    #[test]
    fn mobs_keep_walking_and_arrows_keep_flying() {
        let mut app = app(parse(&[
            "--bodies",
            "60",
            "--body-kind",
            "mob",
            "--projectiles",
            "40",
        ])
        .expect("valid"));
        for _ in 0..200 {
            app.update();
        }
        let at = |app: &mut App| -> Vec<DVec3> {
            let world = app.world_mut();
            world
                .query_filtered::<&Transform, With<Mob>>()
                .iter(world)
                .map(|t| t.translation)
                .collect()
        };
        let before = at(&mut app);
        assert_eq!(before.len(), 60);
        for _ in 0..300 {
            app.update();
        }
        let after = at(&mut app);
        let moved = before.iter().zip(&after).filter(|(a, b)| a != b).count();
        assert!(moved > 50, "{moved} of 60 moved");
        assert_eq!(count::<(With<Mob>, With<Sleeping>)>(&mut app), 0);
        // 40 arrows shot every 2 s for about 8 s: they hit the ground (and mobs) over and over.
        assert_eq!(count::<With<Shot>>(&mut app), 40);
        assert!(app.world().resource::<Hits>().0 >= 80);
    }
}
