//! Swimming (ADR 0021) on the harness of `sim`: a pool of still water on the flat test world (the
//! ground at y = 4). A body sinks slowly in it, rises while it swims up, walks slower on the
//! bottom, gets its land settings back when it leaves, and does the same far from the origin.
mod common;
mod sim;

use boxel_ecs::prelude::*;
use boxel_math::{DVec3, I64Vec3, Vec2};
use boxel_physics::{
    BodySettings, CharacterController, MoveIntent, SwimIntent, SwimPlugin, SwimSettings, Swimmer,
    Teleport, Velocity,
};
use boxel_voxel::{MAX_BLOCK_COORD, WorldPos};
use common::WATER;
use sim::{STEP, Sim};

/// A world with a 9x9 pool of water 6 deep standing on the ground (y 4..=9), around `base`.
fn pool(base: I64Vec3) -> Sim {
    let mut sim = Sim::with_plugins(base, STEP, SwimPlugin);
    for x in -4..=4 {
        for z in -4..=4 {
            for y in 4..=9 {
                let pos = sim.block(x, y, z);
                sim.edit(pos, WATER);
            }
        }
    }
    sim.steps(2);
    sim
}

fn swimmer(sim: &mut Sim, pos: WorldPos) -> Entity {
    sim.spawn(
        pos,
        (
            Swimmer::default(),
            CharacterController::default(),
            MoveIntent::default(),
            SwimIntent::default(),
        ),
    )
}

/// The run of one body: dropped from y = 14 into the pool, sinks, swims up for a while, then
/// walks east along the bottom. Returns its positions (relative to the base) every step.
fn run(base: I64Vec3) -> Vec<DVec3> {
    let mut sim = pool(base);
    let start = sim.pos(0.5, 14.0, 0.5);
    let body = swimmer(&mut sim, start);
    let rel = |sim: &Sim| {
        let p = sim.position(body);
        (p.block.as_i64vec3() - base).as_dvec3() + p.frac
    };
    let mut out = Vec::new();
    for step in 0..400 {
        if step == 200 {
            sim.app.world_mut().get_mut::<SwimIntent>(body).unwrap().up = true;
        }
        if step == 260 {
            sim.app.world_mut().get_mut::<SwimIntent>(body).unwrap().up = false;
            sim.app
                .world_mut()
                .get_mut::<MoveIntent>(body)
                .unwrap()
                .wish = Vec2::X;
        }
        sim.steps(1);
        out.push(rel(&sim));
    }
    out
}

#[test]
fn a_body_sinks_slowly_swims_up_and_walks_slower_in_water() {
    let mut sim = pool(I64Vec3::ZERO);
    let start = sim.pos(0.5, 14.0, 0.5);
    let body = swimmer(&mut sim, start);
    let land = BodySettings::player();
    // It falls through the air, then the water slows it towards its sinking speed: it never
    // speeds up again, and 30 steps in it has lost half its speed.
    let sink = 32.0 * SwimSettings::default().gravity_scale / SwimSettings::default().drag;
    let mut entered: Option<(usize, f64)> = None;
    let mut last = 0.0f64;
    for step in 0..300 {
        sim.steps(1);
        let v = sim.get::<Velocity>(body).0.y;
        if sim.get::<Swimmer>(body).in_fluid && entered.is_none() {
            entered = Some((step, v));
        }
        if let Some((at, v0)) = entered {
            if step > at && v < 0.0 {
                assert!(
                    v >= last.min(-sink) - 1e-9,
                    "step {step}: sinks faster ({v} < {last})"
                );
            }
            if step == at + 30 && v < 0.0 {
                assert!(
                    v > 0.5 * v0,
                    "step {step}: still sinking at {v} (entered at {v0})"
                );
            }
        }
        last = v;
    }
    let (_, v0) = entered.expect("never reached the water");
    assert!(v0 < -10.0, "fell into the water at {v0}");
    assert_eq!(
        sim.position(body),
        sim.pos(0.5, 4.0, 0.5),
        "rests on the bottom"
    );
    let settings = sim.get::<BodySettings>(body);
    assert_eq!(settings.gravity_scale, 0.25);
    assert_eq!((settings.drag_xz, settings.drag_y), (3.0, 3.0));
    let walk = sim.get::<CharacterController>(body).walk_speed;
    assert_eq!(walk, CharacterController::default().walk_speed * 0.5);
    // Swim up: it rises to the surface (the box leaves the water and falls back: it bobs).
    sim.app.world_mut().get_mut::<SwimIntent>(body).unwrap().up = true;
    sim.steps(200);
    let y = sim.translation(body).y;
    assert!((8.0..11.0).contains(&y), "bobs at the surface: y {y}");
    // A jump press in the water also swims up (no jump off the bottom).
    sim.app.world_mut().get_mut::<SwimIntent>(body).unwrap().up = false;
    sim.steps(200);
    assert_eq!(sim.translation(body).y, 4.0, "sank back");
    sim.app
        .world_mut()
        .get_mut::<MoveIntent>(body)
        .unwrap()
        .jump = true;
    sim.steps(1);
    let v = sim.get::<Velocity>(body).0.y;
    assert!(v > 2.5 && v <= 3.0, "swims up at {v}");
    // Out of the water: the land settings come back.
    let dry = sim.pos(10.5, 4.0, 0.5);
    sim.app.world_mut().entity_mut(body).insert(Teleport(dry));
    sim.steps(2);
    assert!(!sim.get::<Swimmer>(body).in_fluid);
    assert_eq!(sim.get::<BodySettings>(body), land);
    assert_eq!(
        sim.get::<CharacterController>(body).walk_speed,
        CharacterController::default().walk_speed
    );
}

#[test]
fn swimming_is_the_same_far_from_the_origin() {
    let near = run(I64Vec3::ZERO);
    assert!(near.iter().any(|p| p.y > 7.0), "it swam up");
    assert!(near.last().unwrap().x > 2.0, "it walked on the bottom");
    for base in [
        I64Vec3::new((1 << 31) - 3, 0, -(1 << 31)),
        I64Vec3::new(1 << 40, 1 << 40, -(1 << 40)),
        // Exact in f64 (the viewer's `Transform`), so the viewer streams around the base.
        I64Vec3::new(
            MAX_BLOCK_COORD - 1024,
            -MAX_BLOCK_COORD + 1024,
            MAX_BLOCK_COORD - 1024,
        ),
    ] {
        assert_eq!(run(base), near, "base {base}");
    }
}
