//! Dynamic lights: light that moves with an entity (a torch in the hand) and is not a block
//! (ADR 0062, 0063).
//!
//! [`DynamicLight`] on an entity with a `Transform` makes it shine. The world bakes the
//! [`WorldSettings::baked_lights`] nearest a viewer into the light of the meshes near them, as
//! an emitter placed at their voxel would light them: around walls and never through them, with
//! AO on the faces they light. Moving to another voxel remeshes the chunks they reach (an edit
//! batch). [`BakedLights`] lists them; a renderer draws the others in its shaders (ADR 0062:
//! smooth, but through walls and without AO on dark faces).
use crate::streaming::WorldJobs;
use crate::{ChunkMap, ChunkMeshEvent, ChunkViewer, SkyMap, WorldContent, WorldSettings};
use boxel_ecs::prelude::*;
use boxel_math::{DVec3, Vec3};
use boxel_transform::Transform;

/// Makes its entity a moving light (ADR 0062): it shines from the entity's `Transform`
/// translation plus `offset`, with `emission` per block light channel. An emission of 0 on every
/// channel shines nothing (and costs nothing), so a game can keep the component and change it.
///
/// The world bakes it into the meshes near it (ADR 0063, [`BakedLights`]): it lights them as an
/// emitter placed at its voxel would, and moving to another voxel remeshes the chunks it reaches.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
#[require(Transform)]
pub struct DynamicLight {
    /// Level per block light channel (red, green, blue), `0..=15` (more is 15): a block's
    /// emission (a torch is `[14, 11, 6]` in the game's colors).
    pub emission: [u8; 3],
    /// Where it shines from, relative to the translation (not turned with the entity): the eye
    /// of a player whose translation is its feet.
    pub offset: Vec3,
}

impl DynamicLight {
    /// Where it shines from, for an entity at `transform`.
    pub fn position(&self, transform: &Transform) -> DVec3 {
        transform.translation + self.offset.as_dvec3()
    }

    /// True when it shines (some channel above 0).
    pub fn is_lit(&self) -> bool {
        self.emission.iter().any(|&e| e > 0)
    }
}

/// The dynamic lights the world bakes into its meshes this frame (ADR 0063), by entity, and what
/// they cost so far. Written in `WorldSystems::Edits`. A renderer leaves these out of its own
/// dynamic lights (the meshes already show them) and draws the rest.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct BakedLights {
    entities: Vec<Entity>,
    /// Lights that moved to another voxel, changed, appeared or went, since the world opened:
    /// each one remeshed the chunks around it.
    pub moves: usize,
    /// Chunks remeshed for the lights, since the world opened.
    pub remeshes: usize,
    /// Mesh jobs that baked a light, since the world opened (a frame late).
    pub boxes: usize,
}

impl BakedLights {
    /// True if the world bakes `entity`'s light.
    pub fn contains(&self, entity: Entity) -> bool {
        self.entities.binary_search(&entity).is_ok()
    }

    /// The baked lights' entities, sorted.
    pub fn entities(&self) -> &[Entity] {
        &self.entities
    }

    /// True when the world bakes no light.
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }
}

/// `WorldSystems::Edits`, after the frame's edits: the [`WorldSettings::baked_lights`] lit
/// dynamic lights nearest a viewer (ties by entity; none with light off) go to the world's
/// jobs, which remesh around the ones that moved. Without lights it does nothing.
#[expect(
    clippy::too_many_arguments,
    reason = "a Bevy-style system: one parameter per resource it reads"
)]
pub(crate) fn bake_dynamic_lights(
    lights: Query<(Entity, &DynamicLight, &Transform)>,
    viewers: Query<&Transform, With<ChunkViewer>>,
    settings: Res<WorldSettings>,
    mut map: ResMut<ChunkMap>,
    mut sky: ResMut<SkyMap>,
    mut jobs: ResMut<WorldJobs>,
    content: Res<WorldContent>,
    mut baked: ResMut<BakedLights>,
    mut writer: MessageWriter<ChunkMeshEvent>,
) {
    if lights.is_empty() && jobs.point_lights.is_empty() {
        return;
    }
    let light_on = sky.rule != crate::sky::SkyRule::Off;
    let mut wanted: Vec<(f64, Entity, DVec3, [u8; 3])> = lights
        .iter()
        .filter(|(_, light, _)| light_on && light.is_lit())
        .filter_map(|(entity, light, transform)| {
            let at = light.position(transform);
            at.is_finite().then(|| {
                let near = viewers
                    .iter()
                    .map(|v| v.translation.distance_squared(at))
                    .fold(f64::INFINITY, f64::min);
                (near, entity, at, light.emission)
            })
        })
        .collect();
    wanted.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    wanted.truncate(settings.baked_lights);
    wanted.sort_unstable_by_key(|w| w.1);
    let points: Vec<_> = wanted
        .iter()
        .map(|&(_, entity, at, emission)| (entity.to_bits(), at, emission))
        .collect();
    let mut events = Vec::new();
    let mut cx = crate::streaming::Ctx::new(&mut map, &mut sky, &content, &mut events);
    jobs.update_point_lights(&mut cx, &points);
    writer.write_batch(events);
    let counts = jobs.point_counts;
    let now = BakedLights {
        entities: wanted.iter().map(|w| w.1).collect(),
        moves: counts.moves,
        remeshes: counts.remeshes,
        boxes: counts.boxes,
    };
    baked.set_if_neq(now);
}
