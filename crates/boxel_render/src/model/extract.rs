//! `RenderSystems::Extract` of the entity models: every visible [`ModelInstance`] with its
//! `Transform` and its light this frame, as [`ModelDraw`]s for the renderer.

use super::{ModelDraw, ModelInstance, ModelLayer, ModelLight};
use crate::plugin::ExtractedFrame;
use boxel_camera::Camera;
use boxel_ecs::prelude::*;
use boxel_math::DVec3;
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, LightValue};
use boxel_world::ChunkMap;
use std::sync::Arc;

/// The light of the block at `at`: open sky where the world has none (not loaded, no light
/// computed, or no world at all).
fn light_at(chunks: Option<&ChunkMap>, at: DVec3) -> LightValue {
    if !at.is_finite() {
        return LightValue::FULL_SKY;
    }
    chunks
        .and_then(|map| map.light_at(BlockPos::from_world(at)))
        .unwrap_or(LightValue::FULL_SKY)
}

/// Copies the visible instances into the frame (after `extract`, which found the camera: the
/// view layer is lit at its eye). Sorted by entity, so the renderer's draw order and posing
/// cache see the same list every frame.
pub(crate) fn extract_models(
    mut frame: ResMut<ExtractedFrame>,
    instances: Query<(Entity, &ModelInstance, &Transform)>,
    cameras: Query<(&Camera, &Transform)>,
    chunks: Option<Res<ChunkMap>>,
) {
    let frame = &mut *frame;
    frame.models.clear();
    let eye = cameras
        .iter()
        .find(|(camera, _)| camera.active)
        .map(|(_, t)| t.translation);
    let chunks = chunks.as_deref();
    for (entity, instance, transform) in &instances {
        if !instance.visible {
            continue;
        }
        let light = match (instance.light, instance.layer) {
            (ModelLight::Fixed(light), _) => light,
            (ModelLight::Probe(offset), ModelLayer::World) => {
                light_at(chunks, transform.translation + offset.as_dvec3())
            }
            (ModelLight::Probe(offset), ModelLayer::View) => match eye {
                Some(eye) => light_at(chunks, eye + offset.as_dvec3()),
                None => LightValue::FULL_SKY,
            },
        };
        frame.models.push(ModelDraw {
            id: entity.to_bits(),
            model: Arc::clone(&instance.model),
            texture: instance.texture.clone(),
            pose: instance.pose.clone(),
            layer: instance.layer,
            position: transform.translation,
            rotation: transform.rotation,
            origin: instance.origin,
            scale: instance.scale,
            light,
        });
    }
    frame.models.sort_unstable_by_key(|d| d.id);
}
