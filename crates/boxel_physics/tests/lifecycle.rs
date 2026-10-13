//! Physics across a world's close (ADR 0034, test 20): the world's bodies go with it, no contact
//! of theirs ends after they are gone, and every per-world resource of physics is empty (the
//! lifecycle's checks, which see the private ones); a body that outlived its world (neither
//! `Saved` nor `WorldScoped`) still sleeps in the next one and wakes when its ground goes.
mod sim;

use boxel_math::I64Vec3;
use boxel_physics::{BodyIndex, Collider, ReportContacts};
use boxel_voxel::BlockState;
use boxel_world::lifecycle::{
    CloseOptions, WorldScope, WorldScoped, WorldSpec, close_world, finish_closing, open_world,
};
use boxel_world::{WorldContent, WorldSettings};
use sim::{Collisions, STEP, Sim};
use std::sync::Arc;
use std::time::Duration;

/// A crate: a solid cube of 0.75 standing on its position.
fn crate_box() -> Collider {
    Collider::standing(0.75, 0.75)
}

#[test]
fn a_close_ends_no_contact_and_leaves_no_body() {
    for base in [I64Vec3::ZERO, I64Vec3::new(1 << 40, 1 << 40, -(1 << 40))] {
        let mut sim = Sim::new(base, STEP);
        let lower = sim.body(sim.pos(3.5, 4.0, 0.5), (crate_box(), WorldScoped));
        let upper = sim.body(
            sim.pos(3.5, 6.0, 0.5),
            (crate_box(), ReportContacts, WorldScoped),
        );
        sim.steps(256);
        let started = std::mem::take(&mut sim.app.world_mut().resource_mut::<Collisions>().0);
        assert_eq!(started.len(), 1, "one contact started");
        assert!(sim.asleep(upper) && sim.asleep(lower), "both rest");
        assert!(close_world(sim.app.world_mut(), CloseOptions::default()));
        let world = sim.app.world();
        assert!(world.get_entity(upper).is_err() && world.get_entity(lower).is_err());
        assert!(world.resource::<BodyIndex>().is_empty());
        assert_eq!(world.resource::<WorldScope>().last_check(), Some(&[][..]));
        sim.steps(20);
        assert!(
            sim.app.world().resource::<Collisions>().0.is_empty(),
            "no contact of a dead body ends"
        );
    }
}

#[test]
fn a_body_that_outlives_its_world_still_sleeps_and_wakes() {
    let mut sim = Sim::new(I64Vec3::new(1 << 40, 0, 1 << 40), STEP);
    let body = sim.body(sim.pos(0.5, 4.0, 0.5), crate_box());
    sim.steps(300);
    assert!(sim.asleep(body));
    let generator = Arc::clone(&sim.app.world().resource::<WorldContent>().generator);
    assert!(close_world(sim.app.world_mut(), CloseOptions::default()));
    assert!(finish_closing(sim.app.world_mut(), Duration::from_secs(10)));
    assert!(
        sim.app.world().get_entity(body).is_ok(),
        "neither Saved nor WorldScoped"
    );
    // The same ground again (the test's viewer survived too, and streams it).
    open_world(
        sim.app.world_mut(),
        WorldSpec::new(generator, WorldSettings::default()),
    )
    .unwrap();
    sim.settle();
    // Its chunk loading again wakes it (it may have changed); it falls asleep again.
    sim.steps(300);
    assert!(sim.asleep(body), "it sleeps on");
    assert_eq!(sim.app.world().resource::<BodyIndex>().len(), 1);
    // Its ground goes: it wakes and falls onto the stone.
    sim.edit(sim.block(0, 3, 0), BlockState::AIR);
    sim.steps(64);
    assert_eq!(sim.position(body), sim.pos(0.5, 3.0, 0.5));
}
