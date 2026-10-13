//! Physics across a world's close (ADR 0034): its per-world state back to a fresh process's,
//! after the world's bodies are despawned. No solver, sleep or contact logic changes here.
use crate::components::{
    CollisionEnded, CollisionStarted, Disturbed, PhysicsStats, ProjectileHit, RigidBody, SleepLog,
    Sleeping, Vacated, VoxelImpact,
};
use crate::contacts::{BodyIndex, ContactSet, ObstacleIndex};
use crate::projectile::PendingHits;
use crate::systems::{AwakeCount, PendingImpacts, WritebackTick};
use boxel_app::App;
use boxel_ecs::prelude::*;
use boxel_world::lifecycle::{TeardownSystems, WorldGarbage, WorldScopeApp, WorldTeardown};

/// Registers physics' per-world resources and messages and its teardown.
pub(crate) fn add_reset(app: &mut App) {
    app.world_resource::<PhysicsStats>(|s| *s == PhysicsStats::default())
        .world_resource::<PendingImpacts>(PendingImpacts::is_empty)
        .world_resource::<ObstacleIndex>(ObstacleIndex::is_empty)
        .world_resource::<BodyIndex>(|b| b.is_empty() && b.sleepers.len() == 0)
        .world_resource::<ContactSet>(ContactSet::is_clear)
        .world_resource::<SleepLog>(|l| l.0.is_empty())
        .world_resource::<Vacated>(|v| v.0.is_empty())
        .world_resource::<Disturbed>(|d| d.0.is_empty())
        .world_resource::<AwakeCount>(|a| a.dynamic == 0 && a.projectiles == 0)
        .world_resource::<WritebackTick>(WritebackTick::is_unset)
        .world_resource::<PendingHits>(|h| h.0.is_empty())
        .world_message::<VoxelImpact>()
        .world_message::<CollisionStarted>()
        .world_message::<CollisionEnded>()
        .world_message::<ProjectileHit>()
        .add_systems(
            WorldTeardown,
            reset_physics.in_set(TeardownSystems::Physics),
        );
}

/// `WorldTeardown`, `TeardownSystems::Physics`: after the world's bodies were despawned (their
/// despawn hooks filled `Vacated` and `SleepLog`), every per-world resource goes back to its
/// default; the indexes and the contact set are dropped off the main thread. A body that
/// survived its world (neither `Saved` nor `WorldScoped`) is a mistake of the app: it is named in
/// a warning, and the sleeping ones are logged again so the sleeper store follows them.
#[expect(
    clippy::too_many_arguments,
    reason = "a Bevy-style system: one parameter per resource it resets"
)]
fn reset_physics(
    mut stats: ResMut<PhysicsStats>,
    mut impacts: ResMut<PendingImpacts>,
    mut obstacles: ResMut<ObstacleIndex>,
    mut bodies: ResMut<BodyIndex>,
    mut contacts: ResMut<ContactSet>,
    mut sleep_log: ResMut<SleepLog>,
    mut vacated: ResMut<Vacated>,
    mut disturbed: ResMut<Disturbed>,
    mut awake: ResMut<AwakeCount>,
    mut writeback: ResMut<WritebackTick>,
    mut hits: ResMut<PendingHits>,
    mut garbage: ResMut<WorldGarbage>,
    survivors: Query<(Entity, Has<Sleeping>), With<RigidBody>>,
) {
    garbage.push(std::mem::take(&mut *obstacles));
    garbage.push(std::mem::take(&mut *bodies));
    garbage.push(std::mem::take(&mut *contacts));
    *stats = PhysicsStats::default();
    *impacts = PendingImpacts::default();
    *sleep_log = SleepLog::default();
    *vacated = Vacated::default();
    *disturbed = Disturbed::default();
    *awake = AwakeCount::default();
    *writeback = WritebackTick::default();
    *hits = PendingHits::default();
    let survived = survivors.iter().count();
    if survived > 0 {
        tracing::warn!(
            survived,
            "physics bodies outlived their world (give them Saved or WorldScoped)"
        );
        sleep_log
            .0
            .extend(survivors.iter().filter(|(_, s)| *s).map(|(e, _)| e));
    }
}
