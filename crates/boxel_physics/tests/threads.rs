//! The step on the compute threads changes nothing: a crowd in a pen (walkers that push into
//! each other, into crates that fall asleep and wake up, into one-block hills and slabs, items
//! that are sensors, pillars, a kinematic pusher) gives the same bodies and events, bit for bit,
//! on one thread (no tasks) and with every part of the step on a compute pool of 1, 3 or 8
//! threads. The pool is global and set once per process, so each pool size runs in a child
//! process of this test binary.
mod common;
mod sim;

use boxel_app::FixedUpdate;
use boxel_ecs::prelude::*;
use boxel_math::{DVec3, I64Vec3, Vec2};
use boxel_physics::{
    BodySettings, CharacterController, Collider, Contacts, MoveIntent, PhysicsStats,
    ReportContacts, RigidBody, Teleport, Velocity,
};
use boxel_tasks::{ComputeTaskPool, TaskPoolBuilder};
use boxel_world::ChunkMap;
use common::{STONE, bottom_slab};
use sim::{Collisions, STEP, Sim};
use std::process::Command;

/// The pool size of the child process (unset in the parent).
const THREADS: &str = "BOXEL_PHYSICS_TEST_THREADS";

#[derive(Component)]
struct Walker(u64);

#[derive(Resource, Default)]
struct Steps(u64);

fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Each walker picks a way every 40 steps, and some jump.
fn wander(mut steps: ResMut<Steps>, mut walkers: Query<(&Walker, &mut MoveIntent)>) {
    steps.0 += 1;
    for (walker, mut intent) in &mut walkers {
        let r = mix(walker.0 ^ (steps.0 / 40).wrapping_mul(0x1000_0001));
        let unit = |bits: u64| (bits & 0xff) as f32 / 127.5 - 1.0;
        intent.wish = Vec2::new(unit(r), unit(r >> 8));
        if steps.0.is_multiple_of(40) && (r >> 16).is_multiple_of(5) {
            intent.jump = true;
        }
    }
}

/// Stone at `base + min ..= base + max`, waiting for the edits.
fn build(sim: &mut Sim, min: [i64; 3], max: [i64; 3], state: boxel_voxel::BlockState) {
    for y in min[1]..=max[1] {
        for x in min[0]..=max[0] {
            for z in min[2]..=max[2] {
                sim.edit(sim.block(x, y, z), state);
            }
        }
    }
    let last = sim.block(max[0], max[1], max[2]);
    while sim.app.world().resource::<ChunkMap>().block_at(last) != Some(state) {
        sim.idle_frame();
    }
}

/// Every body's state and the collision events after `steps` steps, folded into one number,
/// with how many pushes and wake-ups happened (the scene must exercise them).
fn crowd(parallel_threshold: usize, steps: u64) -> (u64, usize, usize) {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    sim.settings().parallel_threshold = parallel_threshold;
    sim.app
        .init_resource::<Steps>()
        .add_systems(FixedUpdate, wander);
    // A pen 28 blocks wide with one-block hills and slabs inside.
    build(&mut sim, [-14, 4, -14], [14, 5, -14], STONE);
    build(&mut sim, [-14, 4, 14], [14, 5, 14], STONE);
    build(&mut sim, [-14, 4, -13], [-14, 5, 13], STONE);
    build(&mut sim, [14, 4, -13], [14, 5, 13], STONE);
    for i in 0..12 {
        let x = (mix(i) % 24) as i64 - 12;
        let z = (mix(i ^ 0xABC) % 24) as i64 - 12;
        build(&mut sim, [x, 4, z], [x + 1, 4, z], STONE);
        build(&mut sim, [z, 4, x], [z, 4, x], bottom_slab());
    }
    let mut bodies = Vec::new();
    for i in 0..360u64 {
        let r = mix(i ^ 0x5EED);
        let x = (r % 2400) as f64 / 100.0 - 12.0;
        let z = ((r >> 16) % 2400) as f64 / 100.0 - 12.0;
        let y = 5.0 + ((r >> 32) % 4) as f64;
        let pos = sim.pos(x, y, z);
        let body = match r % 8 {
            // Walkers.
            0..=3 => sim.spawn(pos, (CharacterController::default(), Walker(i))),
            // Crates, some reporting their contacts.
            4 | 5 => {
                let e = sim.body(pos, Collider::standing(0.75, 0.75));
                if r % 16 == 4 {
                    sim.app.world_mut().entity_mut(e).insert(ReportContacts);
                }
                e
            }
            // Items: sensors that fall and sleep.
            6 => sim.body(
                pos,
                (
                    Collider::aabb(DVec3::splat(0.125)).sensor(),
                    BodySettings::item(),
                    Velocity(DVec3::new(1.0, 2.0, -0.5)),
                ),
            ),
            // Balls.
            _ => sim.body(
                pos,
                (
                    Collider::sphere(0.3),
                    BodySettings::ball(),
                    Velocity(DVec3::new(2.0, 1.0, 1.5)),
                ),
            ),
        };
        bodies.push(body);
    }
    for i in 0..6u32 {
        let at = sim.pos(-9.5 + 3.5 * f64::from(i), 4.0, 0.5);
        let pillar = sim
            .app
            .world_mut()
            .spawn((
                RigidBody::Static,
                Collider::standing(1.0, 2.0),
                Teleport(at),
            ))
            .id();
        bodies.push(pillar);
    }
    let at = sim.pos(-12.5, 4.0, 6.5);
    let pusher = sim
        .app
        .world_mut()
        .spawn((
            RigidBody::Kinematic,
            Collider::standing(1.0, 1.0),
            Teleport(at),
            Velocity(DVec3::new(1.0, 0.0, 0.0)),
        ))
        .id();
    bodies.push(pusher);

    let (mut pushes, mut woken) = (0, 0);
    sim.app.world_mut().resource_mut::<Steps>().0 = 0;
    while sim.app.world().resource::<Steps>().0 < steps {
        sim.frame(STEP);
        let stats = sim.app.world().resource::<PhysicsStats>();
        pushes += stats.pushes;
        woken += stats.woken;
    }
    let mut hash = 0u64;
    let mut fold = |v: u64| hash = mix(hash ^ v);
    for &entity in &bodies {
        let p = sim.position(entity);
        for v in p.block.as_i64vec3().to_array() {
            fold(v as u64);
        }
        for v in p.frac.to_array() {
            fold(v.to_bits());
        }
        for v in sim.get::<Velocity>(entity).0.to_array() {
            fold(v.to_bits());
        }
        fold(u64::from(sim.get::<Contacts>(entity).faces));
        fold(u64::from(sim.asleep(entity)));
    }
    let index = |e: Entity| bodies.iter().position(|&b| b == e).expect("scene body") as u64;
    let events = std::mem::take(&mut sim.app.world_mut().resource_mut::<Collisions>().0);
    for (started, a, b) in events {
        fold(u64::from(started));
        fold(index(a));
        fold(index(b));
    }
    (hash, pushes, woken)
}

const STEPS: u64 = 400;

#[test]
fn a_crowd_is_bit_identical_on_one_thread_and_on_any_pool() {
    let (reference, pushes, woken) = crowd(usize::MAX, STEPS);
    assert!(pushes >= 500, "the crowd pushes: {pushes}");
    assert!(woken >= 10, "sleepers wake up: {woken}");
    let exe = std::env::current_exe().expect("test binary");
    for threads in [1, 3, 8] {
        let out = Command::new(&exe)
            .args([
                "crowd_on_a_compute_pool",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(THREADS, threads.to_string())
            .output()
            .expect("runs the child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{threads} threads: {stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let line = stdout
            .lines()
            .find_map(|l| l.split_once("CROWD ").map(|(_, result)| result))
            .unwrap_or_else(|| panic!("{threads} threads: no result in {stdout}"));
        assert_eq!(
            line,
            format!("{reference} {pushes} {woken}"),
            "{threads} threads"
        );
    }
}

/// The child of the test above: the crowd with every part of the step on the compute pool.
#[test]
#[ignore = "run by a_crowd_is_bit_identical_on_one_thread_and_on_any_pool, in its own process"]
fn crowd_on_a_compute_pool() {
    let Some(threads) = std::env::var(THREADS).ok().and_then(|t| t.parse().ok()) else {
        return;
    };
    let pool = ComputeTaskPool::get_or_init(|| TaskPoolBuilder::new().num_threads(threads).build());
    assert_eq!(pool.thread_num(), threads);
    let (hash, pushes, woken) = crowd(0, STEPS);
    println!("CROWD {hash} {pushes} {woken}");
}
