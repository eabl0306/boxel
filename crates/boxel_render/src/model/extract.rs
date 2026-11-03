//! `RenderSystems::Extract` of the entity models: every visible [`ModelInstance`] with its
//! `Transform` and its light this frame, as [`ModelDraw`]s for the renderer.

use super::{ModelDraw, ModelInstance, ModelLayer, ModelLight, ModelRealm};
use crate::SceneRealms;
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

/// The sky's red, green and blue at `at` where colored glass tinted them (ADR 0064): `None` where
/// they are `light`'s sky level three times (no tint, or no light there).
fn sky_rgb_at(chunks: Option<&ChunkMap>, at: DVec3, light: LightValue) -> Option<[u8; 3]> {
    if !at.is_finite() {
        return None;
    }
    chunks
        .and_then(|map| map.sky_rgb_at(BlockPos::from_world(at)))
        .filter(|&rgb| rgb != [light.sky(); 3])
}

/// The realm of the block at `at` (realm 0 where it is not finite).
fn realm_at(realms: &SceneRealms, at: DVec3) -> ModelRealm {
    if !at.is_finite() {
        return ModelRealm::default();
    }
    let index = realms
        .realm_of(BlockPos::from_world(at).y)
        .min(realms.realms.len() - 1);
    ModelRealm {
        index: index as u8,
        face_shade: realms.realms[index].face_shade,
    }
}

/// Copies the visible instances into the frame (after `extract`, which found the camera: the
/// view layer is lit at its eye, and the frame's realms are known, and after the dynamic
/// lights' extract). Each takes the realm of the block its light comes from (of its
/// translation, or the eye, with a fixed light). An instance lit at a probe takes the frame's
/// dynamic lights there too (ADR 0062), baked by the world or not (ADR 0063), by the brightest
/// per channel, and its sky tinted by colored glass there (ADR 0064, the picture's sky: its
/// light keeps the uncolored level gameplay reads); a fixed light stays as it is. Sorted by entity, so the renderer's draw order and posing cache see the same list every
/// frame.
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
        let offset = match instance.light {
            ModelLight::Fixed(_) => DVec3::ZERO,
            ModelLight::Probe(offset) => offset.as_dvec3(),
        };
        let probe = match instance.layer {
            ModelLayer::World => Some(transform.translation + offset),
            ModelLayer::View => eye.map(|eye| eye + offset),
        };
        let (light, sky_rgb) = match (instance.light, probe) {
            (ModelLight::Fixed(light), _) => (light, None),
            (ModelLight::Probe(_), Some(at)) => {
                let light = light_at(chunks, at);
                let sky_rgb = sky_rgb_at(chunks, at, light);
                (frame.model_lights.lit(light, at), sky_rgb)
            }
            (ModelLight::Probe(_), None) => (LightValue::FULL_SKY, None),
        };
        let realm = probe.map_or_else(ModelRealm::default, |at| realm_at(&frame.light.realms, at));
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
            sky_rgb,
            realm,
        });
    }
    frame.models.sort_unstable_by_key(|d| d.id);
}
