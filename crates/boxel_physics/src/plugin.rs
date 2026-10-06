//! [`PhysicsPlugin`] and the physics view of the world's blocks.

use crate::core::CollisionTable;
use boxel_app::{App, Plugin};
use boxel_ecs::prelude::*;
use boxel_world::WorldContent;
use std::sync::Arc;

/// The physics view of the world's blocks: the [`CollisionTable`] derived from
/// `WorldContent::registry`. Inserted by [`PhysicsPlugin`] in `Plugin::finish`.
#[derive(Resource, Clone, Debug)]
pub struct PhysicsContent {
    /// How each block state collides, plus the fluid and outline flags.
    pub collision: Arc<CollisionTable>,
}

/// Adds physics to an app with a voxel world (`WorldPlugin`). Not part of `DefaultPlugins`:
/// games add it.
///
/// Phase P1 of the physics plan (ADR 0016): voxel queries only. In `Plugin::finish` it builds
/// [`PhysicsContent`] from the world's registry, which [`crate::PhysicsQuery`] reads. Bodies
/// (velocity, gravity, contacts), collisions between bodies and projectiles come in later
/// phases and will run in `FixedPostUpdate`.
///
/// Runners call `App::finish` before the first update; a test that drives `App::update` by hand
/// calls it too.
///
/// # Panics
///
/// In `finish`, if the app has no `WorldContent` (add `WorldPlugin`).
#[derive(Clone, Copy, Debug, Default)]
pub struct PhysicsPlugin;

impl Plugin for PhysicsPlugin {
    fn build(&self, _app: &mut App) {}

    fn finish(&self, app: &mut App) {
        let Some(content) = app.world().get_resource::<WorldContent>() else {
            panic!("PhysicsPlugin needs the voxel world: add WorldPlugin (no WorldContent found)");
        };
        let collision = Arc::new(CollisionTable::from_registry(&content.registry));
        app.insert_resource(PhysicsContent { collision });
    }
}
