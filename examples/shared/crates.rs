//! Crates the player drops (`C`, two blocks ahead of the active camera): solid dynamic boxes
//! saved with the world (`boxel::world::persist::Saved`, ADR 0024), so with `--world-dir` they
//! stay where they were left, also when their chunk unloads. Physics saves the body
//! (`boxel::physics::BodyCodec`); [`CrateCodec`] adds the game's part, the [`Crate`] marker.
//! Bodies have no renderer yet: F3 shows how many crates there are, how many sleep and the
//! nearest one.
use boxel::camera::FlyCameraSystems;
use boxel::debug_overlay::OverlayLines;
use boxel::physics::{BodyPosition, Teleport};
use boxel::prelude::*;
use boxel::voxel::BlockPos;
use boxel::world::WorldSystems;
use boxel::world::persist::{EntityCodec, Saved, SavedIds, add_entity_codec};

/// Drops a crate.
pub const CRATE_KEY: KeyCode = KeyCode::KeyC;
/// A crate's width and height, blocks.
pub const CRATE_SIZE: f64 = 0.75;

/// A dropped crate.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Crate;

/// The game's part of a saved crate: the [`Crate`] marker (no bytes). The body is physics' part.
struct CrateCodec;

impl EntityCodec for CrateCodec {
    fn name(&self) -> &str {
        "voxel_world:crate"
    }
    fn version(&self) -> u32 {
        1
    }
    fn position(&self, _world: &World, _entity: Entity) -> Option<BlockPos> {
        // Physics places it.
        None
    }
    fn save(&self, world: &World, entity: Entity, _ids: &SavedIds<'_>) -> Option<Vec<u8>> {
        world.get::<Crate>(entity).map(|_| Vec::new())
    }
    fn load(
        &self,
        world: &mut World,
        entity: Entity,
        _version: u32,
        _bytes: &[u8],
        _ids: &SavedIds<'_>,
    ) -> Result<(), String> {
        world.entity_mut(entity).insert(Crate);
        Ok(())
    }
}

/// [`CRATE_KEY`] drops crates; F3 shows them (an overlay line `"crates"`). Needs
/// `PhysicsPlugin`; with `PersistPlugin` the crates are saved.
pub struct CratesPlugin;

impl Plugin for CratesPlugin {
    fn build(&self, app: &mut App) {
        add_entity_codec(app, CrateCodec);
        app.add_systems(
            Update,
            (
                // Like picking: ahead of the view the player saw, before the camera moves.
                drop_crates
                    .before(FlyCameraSystems)
                    .before(WorldSystems::Interest),
                // Only its own overlay line: its order does not matter.
                crate_lines.ambiguous_with_all(),
            ),
        );
    }
}

/// A crate standing at `pos`: a solid dynamic box with twice a player's weight, interpolated,
/// saved with the world.
pub fn crate_body(pos: WorldPos) -> impl Bundle {
    (
        Crate,
        Saved::new(),
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

/// The active camera's transform.
fn active_eye(cameras: &Query<(&Camera, &Transform)>) -> Option<Transform> {
    cameras
        .iter()
        .find(|(camera, _)| camera.active)
        .map(|(_, t)| *t)
}

/// [`CRATE_KEY`] drops a crate two blocks ahead of the active camera (along its heading) and a
/// block above the eye.
fn drop_crates(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    cameras: Query<(&Camera, &Transform)>,
) {
    if !keys.just_pressed(CRATE_KEY) {
        return;
    }
    let Some(eye) = active_eye(&cameras) else {
        return;
    };
    let forward = eye.forward().as_dvec3();
    let ahead = DVec3::new(forward.x, 0.0, forward.z).normalize_or(DVec3::NEG_Z);
    let at = WorldPos::from_dvec3(eye.translation).offset(ahead * 2.0 + DVec3::Y);
    commands.spawn(crate_body(at));
}

/// The `"crates"` overlay line: how many, how many sleep and the nearest (when it changes).
fn crate_lines(
    crates: Query<(&BodyPosition, Has<Sleeping>), With<Crate>>,
    cameras: Query<(&Camera, &Transform)>,
    lines: Option<ResMut<OverlayLines>>,
) {
    let Some(mut lines) = lines else {
        return;
    };
    let eye = active_eye(&cameras).map(|t| WorldPos::from_dvec3(t.translation));
    let (mut total, mut asleep) = (0, 0);
    let mut nearest: Option<f64> = None;
    for (position, sleeping) in &crates {
        total += 1;
        asleep += usize::from(sleeping);
        if let Some(eye) = eye {
            let d = eye.delta_to(&position.0).length();
            nearest = Some(nearest.map_or(d, |n| n.min(d)));
        }
    }
    let line = match nearest {
        Some(d) => format!("Crates {total}  asleep {asleep}  nearest {d:.1}  (C drops)"),
        None => format!("Crates {total}  asleep {asleep}  (C drops)"),
    };
    if lines.get("crates") != Some(line.as_str()) {
        lines.set("crates", line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel::voxel::BlockRegistry;
    use boxel::world::jobs::InlineExecutor;
    use boxel::world::persist::format::decode_entity;
    use boxel::world::persist::{ChunkStore, MemoryStore, PersistPlugin, WorldStore};
    use boxel::worldgen::FlatGenerator;
    use std::sync::Arc;

    /// A headless world on a flat ground (top at y = 4) saved in `backend`, settled around the
    /// origin.
    fn app(backend: &Arc<MemoryStore>) -> App {
        let registry = Arc::new(BlockRegistry::default_blocks());
        let generator = FlatGenerator::new(vec![
            ("boxel:stone".to_string(), 3),
            ("boxel:grass".to_string(), 1),
        ]);
        let store = WorldStore::open(
            Arc::clone(backend) as Arc<dyn ChunkStore>,
            &registry,
            generator.name(),
        )
        .expect("the world opens");
        let mut app = App::new();
        app.add_plugins((
            TimePlugin,
            InputPlugin,
            WorldPlugin {
                generator: Arc::new(generator),
                registry,
                executor: Arc::new(InlineExecutor),
                ..WorldPlugin::default()
            },
            PhysicsPlugin::default(),
            CratesPlugin,
        ))
        .add_plugins(PersistPlugin {
            store,
            executor: Arc::new(InlineExecutor),
        });
        app.finish();
        app.cleanup();
        app.world_mut().spawn((
            Transform::from_translation(DVec3::new(0.5, 6.0, 0.5)),
            ChunkViewer::new(1).with_vertical_radius(1),
        ));
        for _ in 0..60 {
            app.update();
        }
        app
    }

    fn crates(app: &mut App) -> usize {
        let world = app.world_mut();
        let mut q = world.query_filtered::<&BodyPosition, With<Crate>>();
        q.iter(world).count()
    }

    /// A crate is saved on exit with its body and its marker, and comes back on the next start.
    #[test]
    fn crates_are_saved_with_the_world() {
        let backend = Arc::new(MemoryStore::new());
        let mut first = app(&backend);
        first
            .world_mut()
            .spawn(crate_body(WorldPos::from_dvec3(DVec3::new(2.5, 4.0, 2.5))));
        for _ in 0..5 {
            first.update();
        }
        first.shutdown();
        drop(first);
        let keys = backend.entity_keys().unwrap();
        assert_eq!(keys.len(), 1);
        let record = &backend.load_entities(keys[0].0).unwrap()[0].1;
        let codecs: Vec<String> = decode_entity(record)
            .unwrap()
            .into_iter()
            .map(|p| p.codec)
            .collect();
        assert_eq!(codecs, ["boxel:body", "voxel_world:crate"]);
        let mut second = app(&backend);
        assert_eq!(crates(&mut second), 1);
    }
}
