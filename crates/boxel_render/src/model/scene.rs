//! The CPU half of the model pass (ADR 0040): each instance posed once per change, then every
//! instance's vertices placed relative to the render origin in draw order, one range per layer,
//! material and skin. ECS-free and GPU-free (`benches/models.rs` measures it).

use super::mesh::{InstanceFrame, model_to_local, pose_vertices};
use super::{ModelDraw, ModelLayer, ModelMaterial};
use boxel_math::glam::Affine3A;
use boxel_math::{I64Vec3, Quat, Vec3};
use boxel_shaders::gpu::ModelVertex;
use std::collections::HashMap;

/// An instance farther than this from the render origin on any axis is left out: its position
/// relative to the camera would not be exact in `f32` (the outline's rule).
const MAX_OFFSET: i64 = 1 << 20;

/// One draw call of the frame: the quads `first_quad..first_quad + quads` of the scene's
/// vertices (four each), all of one layer, material and skin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelDrawRange {
    /// The layer.
    pub layer: ModelLayer,
    /// The material (the pipeline).
    pub material: ModelMaterial,
    /// The skin ([`super::ModelTexture::id`]).
    pub texture: u64,
    /// First quad.
    pub first_quad: u32,
    /// Quads.
    pub quads: u32,
}

/// An instance as last posed.
struct Posed {
    draw: ModelDraw,
    /// World layer: the block its vertices are relative to (`floor` of its position); view
    /// layer: zero (its vertices are in the camera's frame).
    anchor: I64Vec3,
    /// Opaque and cutout vertices.
    vertices: [Vec<ModelVertex>; 2],
    /// The last update that saw it.
    seen: u64,
}

/// The camera an assembly was made for.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Camera {
    render_origin: I64Vec3,
    eye_rel: Vec3,
    rotation: Quat,
}

/// The posed instances and the frame's vertices and draw ranges ([`ModelScene::update`]).
#[derive(Default)]
pub struct ModelScene {
    posed: HashMap<u64, Posed, foldhash::fast::FixedState>,
    updates: u64,
    /// `(layer, skin, id)` of the drawn instances, sorted: the draw order.
    order: Vec<(ModelLayer, u64, u64)>,
    scratch_order: Vec<(ModelLayer, u64, u64)>,
    camera: Option<Camera>,
    vertices: Vec<ModelVertex>,
    ranges: Vec<ModelDrawRange>,
    parts: Vec<(Affine3A, bool)>,
    /// Instances posed by the last update.
    last_posed: u32,
    /// Instances the last update left out (too far from the camera).
    last_skipped: u32,
}

impl ModelScene {
    /// This frame's instances seen from a camera at `render_origin` and `eye_rel` (as in
    /// [`crate::ViewParams`]) turned by `camera` (for [`ModelLayer::View`]). Poses the instances
    /// whose draw changed (or that are new), forgets the ones that went, and places every
    /// vertex again if anything that moves them changed: a posed instance, the instances drawn,
    /// the render origin (with world instances), or the eye or the camera's rotation (with view
    /// instances). True when [`ModelScene::vertices`] changed (they need uploading).
    pub fn update(
        &mut self,
        draws: &[ModelDraw],
        render_origin: I64Vec3,
        eye_rel: Vec3,
        camera: Quat,
    ) -> bool {
        self.updates += 1;
        let seen = self.updates;
        let (mut posed, mut skipped) = (0, 0);
        self.scratch_order.clear();
        for draw in draws {
            let anchor = match draw.layer {
                ModelLayer::World => {
                    let a = draw.position.floor();
                    let anchor = I64Vec3::new(a.x as i64, a.y as i64, a.z as i64);
                    let rel = anchor.saturating_sub(render_origin);
                    let far = [rel.x, rel.y, rel.z]
                        .iter()
                        .any(|v| v.unsigned_abs() > MAX_OFFSET.unsigned_abs());
                    if far || !draw.position.is_finite() {
                        skipped += 1;
                        continue;
                    }
                    anchor
                }
                ModelLayer::View => I64Vec3::ZERO,
            };
            self.scratch_order
                .push((draw.layer, draw.texture.id(), draw.id));
            match self.posed.get_mut(&draw.id) {
                Some(p) if p.draw == *draw && p.anchor == anchor => p.seen = seen,
                _ => {
                    posed += 1;
                    let mut vertices = self
                        .posed
                        .remove(&draw.id)
                        .map(|p| p.vertices)
                        .unwrap_or_default();
                    vertices.iter_mut().for_each(Vec::clear);
                    pose_instance(draw, anchor, &mut self.parts, &mut vertices);
                    self.posed.insert(
                        draw.id,
                        Posed {
                            draw: draw.clone(),
                            anchor,
                            vertices,
                            seen,
                        },
                    );
                }
            }
        }
        let before = self.posed.len();
        self.posed.retain(|_, p| p.seen == seen);
        let went = before != self.posed.len();
        self.scratch_order.sort_unstable();
        self.scratch_order.dedup_by_key(|e| e.2);
        let order_changed = self.order != self.scratch_order;
        std::mem::swap(&mut self.order, &mut self.scratch_order);
        let now = Camera {
            render_origin,
            eye_rel,
            rotation: camera,
        };
        let has = |layer| self.order.iter().any(|e| e.0 == layer);
        let moved = match self.camera {
            None => true,
            Some(c) => {
                (has(ModelLayer::World) && c.render_origin != now.render_origin)
                    || (has(ModelLayer::View) && c != now)
            }
        };
        self.camera = Some(now);
        self.last_posed = posed;
        self.last_skipped = skipped;
        if posed == 0 && !went && !order_changed && !moved {
            return false;
        }
        self.assemble(now);
        true
    }

    /// Places every drawn instance's vertices, in draw order.
    fn assemble(&mut self, camera: Camera) {
        self.vertices.clear();
        self.ranges.clear();
        for layer in [ModelLayer::World, ModelLayer::View] {
            for material in [ModelMaterial::Opaque, ModelMaterial::Cutout] {
                let mut run: Option<ModelDrawRange> = None;
                for &(l, texture, id) in &self.order {
                    if l != layer {
                        continue;
                    }
                    let Some(p) = self.posed.get(&id) else {
                        continue;
                    };
                    let src = &p.vertices[material as usize];
                    if src.is_empty() {
                        continue;
                    }
                    if run.is_some_and(|r| r.texture != texture) {
                        self.ranges.extend(run.take());
                    }
                    let first = (self.vertices.len() / 4) as u32;
                    let r = run.get_or_insert(ModelDrawRange {
                        layer,
                        material,
                        texture,
                        first_quad: first,
                        quads: 0,
                    });
                    r.quads += (src.len() / 4) as u32;
                    match layer {
                        ModelLayer::World => {
                            let offset = (p.anchor - camera.render_origin).as_vec3();
                            self.vertices.extend(src.iter().map(|v| ModelVertex {
                                pos: (Vec3::from_array(v.pos) + offset).to_array(),
                                ..*v
                            }));
                        }
                        ModelLayer::View => {
                            let to_world = Affine3A::from_rotation_translation(
                                camera.rotation,
                                camera.eye_rel,
                            );
                            self.vertices.extend(src.iter().map(|v| {
                                ModelVertex {
                                    pos: to_world
                                        .transform_point3(Vec3::from_array(v.pos))
                                        .to_array(),
                                    ..*v
                                }
                            }));
                        }
                    }
                }
                self.ranges.extend(run);
            }
        }
    }

    /// The vertices of the last assembly: relative to its render origin, four per quad,
    /// counter-clockwise from outside.
    pub fn vertices(&self) -> &[ModelVertex] {
        &self.vertices
    }

    /// The draw calls of the last assembly, in order: the world layer, then the view layer;
    /// in each, the opaque boxes, then the cutout ones; in each, one per run of a skin.
    pub fn ranges(&self) -> &[ModelDrawRange] {
        &self.ranges
    }

    /// Instances drawn (the last update's).
    pub fn instances(&self) -> u32 {
        self.order.len() as u32
    }

    /// Instances posed by the last update.
    pub fn last_posed(&self) -> u32 {
        self.last_posed
    }

    /// Instances the last update left out (more than 2^20 blocks from the camera).
    pub fn last_skipped(&self) -> u32 {
        self.last_skipped
    }

    /// Forgets everything: the next update poses every instance again.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Poses `draw` into `out`: world instances relative to `anchor`, view instances in the
/// camera's frame.
fn pose_instance(
    draw: &ModelDraw,
    anchor: I64Vec3,
    parts: &mut Vec<(Affine3A, bool)>,
    out: &mut [Vec<ModelVertex>; 2],
) {
    let position = match draw.layer {
        ModelLayer::World => (draw.position - anchor.as_dvec3()).as_vec3(),
        ModelLayer::View => draw.position.as_vec3(),
    };
    let [tw, th] = draw.model.texture_size();
    let frame = InstanceFrame {
        to_space: Affine3A::from_rotation_translation(draw.rotation, position)
            * model_to_local(draw.origin, draw.scale),
        texel_scale: [
            draw.texture.width() as f32 / tw as f32,
            draw.texture.height() as f32 / th as f32,
        ],
        light: draw.light.0,
        realm: draw.realm,
    };
    pose_vertices(&draw.model, &draw.pose, &frame, parts, out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cuboid, Model, ModelRealm, ModelTexture, PartPose};
    use boxel_math::DVec3;
    use boxel_voxel::LightValue;
    use std::sync::Arc;

    fn model() -> Arc<Model> {
        let mut b = Model::builder([16, 16]);
        let root = b.part("root", None, PartPose::default());
        b.cuboid(
            root,
            Cuboid::new([0.0, 0.0], [-2.0, -4.0, -2.0], [4.0, 4.0, 4.0]),
        );
        b.cuboid(
            root,
            Cuboid::new([0.0, 8.0], [-2.0, -4.0, -2.0], [4.0, 4.0, 4.0])
                .inflate(0.5)
                .cutout(),
        );
        Arc::new(b.build().expect("valid"))
    }

    fn texture() -> ModelTexture {
        ModelTexture::new(16, 16, vec![255; 16 * 16 * 4]).expect("valid")
    }

    fn draw(id: u64, model: &Arc<Model>, texture: &ModelTexture, at: DVec3) -> ModelDraw {
        ModelDraw {
            id,
            model: Arc::clone(model),
            texture: texture.clone(),
            pose: model.rest_pose(),
            layer: ModelLayer::World,
            position: at,
            rotation: Quat::IDENTITY,
            origin: Vec3::ZERO,
            scale: 1.0,
            light: LightValue::FULL_SKY,
            realm: ModelRealm::default(),
        }
    }

    #[test]
    fn a_still_scene_uploads_nothing() {
        let (m, t) = (model(), texture());
        let draws = vec![draw(1, &m, &t, DVec3::new(10.5, 64.0, -3.25))];
        let mut scene = ModelScene::default();
        let origin = I64Vec3::new(8, 65, -2);
        assert!(scene.update(&draws, origin, Vec3::splat(0.5), Quat::IDENTITY));
        assert_eq!(scene.last_posed(), 1);
        assert_eq!(scene.instances(), 1);
        assert_eq!(scene.vertices().len(), 2 * 6 * 4);
        // The same draws and camera: nothing to upload, nothing posed. The eye moving inside
        // its block does not move world instances.
        assert!(!scene.update(&draws, origin, Vec3::splat(0.5), Quat::IDENTITY));
        assert!(!scene.update(
            &draws,
            origin,
            Vec3::splat(0.75),
            Quat::from_rotation_y(1.0)
        ));
        assert_eq!(scene.last_posed(), 0);
        // The render origin moving places the vertices again without posing.
        let before = scene.vertices().to_vec();
        assert!(scene.update(
            &draws,
            origin + I64Vec3::X,
            Vec3::splat(0.5),
            Quat::IDENTITY
        ));
        assert_eq!(scene.last_posed(), 0);
        for (a, b) in before.iter().zip(scene.vertices()) {
            assert!((a.pos[0] - 1.0 - b.pos[0]).abs() < 1e-6);
            assert_eq!((a.uv, a.rect, a.data), (b.uv, b.rect, b.data));
        }
        // A new pose poses that instance again.
        let mut turned = draws.clone();
        turned[0].pose.parts[0].rotation.y = 0.5;
        assert!(scene.update(&turned, origin, Vec3::splat(0.5), Quat::IDENTITY));
        assert_eq!(scene.last_posed(), 1);
        // No instances: one last change, then nothing.
        assert!(scene.update(&[], origin, Vec3::ZERO, Quat::IDENTITY));
        assert!(scene.vertices().is_empty() && scene.ranges().is_empty());
        assert!(!scene.update(&[], origin, Vec3::ZERO, Quat::IDENTITY));
    }

    #[test]
    fn instances_are_the_same_anywhere_in_the_world() {
        let (m, t) = (model(), texture());
        let at = |origin: I64Vec3| {
            let mut scene = ModelScene::default();
            let mut d = draw(7, &m, &t, origin.as_dvec3() + DVec3::new(3.25, 1.5, -2.75));
            d.rotation = Quat::from_rotation_y(0.7);
            scene.update(&[d], origin, Vec3::splat(0.5), Quat::IDENTITY);
            scene.vertices().to_vec()
        };
        let here = at(I64Vec3::ZERO);
        // Up to where the `f64` of a `Transform` keeps the quarter blocks of the position (it
        // has 1 mm steps at about 4.4e12 blocks).
        for far in [
            I64Vec3::splat(1 << 31),
            I64Vec3::new(-(1 << 40), 7, 1 << 40),
            I64Vec3::splat(-(1 << 42)),
        ] {
            assert_eq!(at(far), here, "{far}");
        }
        // Too far from the camera: left out.
        let mut scene = ModelScene::default();
        let d = draw(1, &m, &t, DVec3::new(f64::from(1 << 21), 0.0, 0.0));
        scene.update(&[d], I64Vec3::ZERO, Vec3::ZERO, Quat::IDENTITY);
        assert_eq!((scene.instances(), scene.last_skipped()), (0, 1));
    }

    #[test]
    fn draws_run_by_layer_material_and_skin() {
        let m = model();
        let (a, b) = (texture(), texture());
        let mut view = draw(4, &m, &a, DVec3::new(0.0, 0.0, -1.0));
        view.layer = View;
        let draws = vec![
            draw(1, &m, &a, DVec3::ZERO),
            draw(2, &m, &b, DVec3::X),
            draw(3, &m, &a, DVec3::Z),
            view,
        ];
        let mut scene = ModelScene::default();
        scene.update(&draws, I64Vec3::ZERO, Vec3::splat(0.5), Quat::IDENTITY);
        let ranges: Vec<_> = scene
            .ranges()
            .iter()
            .map(|r| (r.layer, r.material, r.texture == a.id(), r.quads))
            .collect();
        use ModelLayer::{View, World};
        use ModelMaterial::{Cutout, Opaque};
        // Skin a (two instances, 6 quads each) before skin b: ids are given in order.
        assert_eq!(
            ranges,
            [
                (World, Opaque, true, 12),
                (World, Opaque, false, 6),
                (World, Cutout, true, 12),
                (World, Cutout, false, 6),
                (View, Opaque, true, 6),
                (View, Cutout, true, 6),
            ]
        );
        // The ranges tile the vertices in order.
        let mut next = 0;
        for r in scene.ranges() {
            assert_eq!(r.first_quad, next);
            next += r.quads;
        }
        assert_eq!(next as usize * 4, scene.vertices().len());
        // The view instance follows the camera: turned and moved with the eye, posed once.
        assert!(scene.update(&draws, I64Vec3::ZERO, Vec3::ONE, Quat::from_rotation_y(1.0)));
        assert_eq!(scene.last_posed(), 0);
    }
}
