//! A demo figure for the examples (ADR 0040): Minecraft's biped proportions (head, body, two
//! arms, two legs; 2 blocks tall at scale 1, its feet at model y = 24) in Minecraft's box-UV
//! layout on a 64 x 64 skin, a procedural skin (a wooden artist's mannequin) and a walk cycle with
//! Minecraft's limb swing.
#![allow(dead_code, reason = "each example uses part of it")]

use boxel::math::{DVec3, Quat, Vec3};
use boxel::render::model::{
    Cuboid, Model, ModelDraw, ModelLayer, ModelTexture, PartId, PartPose, Pose,
};
use boxel::voxel::LightValue;
use std::sync::Arc;

/// The model point at the feet.
pub const FEET: Vec3 = Vec3::new(0.0, 24.0, 0.0);

/// The figure and its parts.
#[derive(Clone)]
pub struct Figure {
    pub model: Arc<Model>,
    pub head: PartId,
    pub right_arm: PartId,
    pub left_arm: PartId,
    pub right_leg: PartId,
    pub left_leg: PartId,
}

impl Figure {
    /// The model.
    pub fn new() -> Self {
        let mut b = Model::builder([64, 64]);
        let head = b.part("head", None, PartPose::default());
        b.cuboid(
            head,
            Cuboid::new([0.0, 0.0], [-4.0, -8.0, -4.0], [8.0, 8.0, 8.0]),
        );
        let body = b.part("body", None, PartPose::default());
        b.cuboid(
            body,
            Cuboid::new([16.0, 16.0], [-4.0, 0.0, -2.0], [8.0, 12.0, 4.0]),
        );
        let right_arm = b.part("right_arm", None, PartPose::at([-5.0, 2.0, 0.0]));
        b.cuboid(
            right_arm,
            Cuboid::new([40.0, 16.0], [-3.0, -2.0, -2.0], [4.0, 12.0, 4.0]),
        );
        let left_arm = b.part("left_arm", None, PartPose::at([5.0, 2.0, 0.0]));
        b.cuboid(
            left_arm,
            Cuboid::new([40.0, 16.0], [-1.0, -2.0, -2.0], [4.0, 12.0, 4.0]).mirror(),
        );
        let right_leg = b.part("right_leg", None, PartPose::at([-1.9, 12.0, 0.0]));
        b.cuboid(
            right_leg,
            Cuboid::new([0.0, 16.0], [-2.0, 0.0, -2.0], [4.0, 12.0, 4.0]),
        );
        let left_leg = b.part("left_leg", None, PartPose::at([1.9, 12.0, 0.0]));
        b.cuboid(
            left_leg,
            Cuboid::new([0.0, 16.0], [-2.0, 0.0, -2.0], [4.0, 12.0, 4.0]).mirror(),
        );
        Self {
            model: Arc::new(b.build().expect("the demo figure is valid")),
            head,
            right_arm,
            left_arm,
            right_leg,
            left_leg,
        }
    }

    /// The pose `phase` into a walk (Minecraft's `limbSwing`, about 17 a second walking) with
    /// `amount` of swing (its `limbSwingAmount`, 0 standing to 1 running), the head turned by
    /// `look` (yaw, pitch; radians).
    pub fn walk(&self, phase: f32, amount: f32, look: (f32, f32)) -> Pose {
        let mut pose = self.model.rest_pose();
        let swing = phase * 0.6662;
        pose[self.right_arm].rotation.x = (swing + std::f32::consts::PI).cos() * amount;
        pose[self.left_arm].rotation.x = swing.cos() * amount;
        pose[self.right_leg].rotation.x = swing.cos() * 1.4 * amount;
        pose[self.left_leg].rotation.x = (swing + std::f32::consts::PI).cos() * 1.4 * amount;
        pose[self.head].rotation.y = look.0;
        pose[self.head].rotation.x = look.1;
        pose
    }

    /// A world draw of the figure at `feet`, facing `yaw` (radians about +Y; 0 faces -Z).
    pub fn draw(
        &self,
        id: u64,
        skin: &ModelTexture,
        feet: DVec3,
        yaw: f32,
        pose: Pose,
        light: LightValue,
    ) -> ModelDraw {
        ModelDraw {
            id,
            model: Arc::clone(&self.model),
            texture: skin.clone(),
            pose,
            layer: ModelLayer::World,
            position: feet,
            rotation: Quat::from_rotation_y(yaw),
            origin: FEET,
            scale: 1.0,
            light,
            realm: boxel::render::model::ModelRealm::default(),
        }
    }
}

/// A small integer hash for the skin's grain.
fn hash(x: u32, y: u32) -> u32 {
    let mut h = x.wrapping_mul(0x9E37_79B9) ^ y.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 15;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^ (h >> 13)
}

/// The mannequin's skin: light wood with a grain, a darker band where the eyes would be and
/// dark hands and feet.
pub fn skin() -> ModelTexture {
    let mut rgba = Vec::with_capacity(64 * 64 * 4);
    for y in 0..64u32 {
        for x in 0..64u32 {
            let grain = (hash(x / 2, y) % 24) as i32 - 12;
            let mut c = [214 + grain, 178 + grain, 128 + grain / 2];
            if (9..15).contains(&x) && (11..13).contains(&y) {
                c = [120, 84, 52];
            }
            // The last rows of the limbs' faces (the body's layout is x 16..40).
            if (28..32).contains(&y) && !(16..40).contains(&x) {
                c = [150, 112, 72];
            }
            rgba.extend(c.map(|v| v.clamp(0, 255) as u8));
            rgba.push(255);
        }
    }
    ModelTexture::new(64, 64, rgba).expect("the demo skin is valid")
}
