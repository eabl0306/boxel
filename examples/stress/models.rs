//! `--models N`: N demo figures (entity models, ADR 0040) walking in front of the camera during a
//! stress run, to measure what the models add to the frame.
//!
//! The figures stand in rows of 10, 1.5 blocks apart, from 4 blocks in front of the eye and a
//! block and a half under it, facing the camera; they follow it every frame (so they stay in
//! view while it flies) and walk in place with Minecraft's limb swing, each at its own phase:
//! every figure is posed again every frame, the worst case for the model pass. Lit by the world
//! at their heads. Without the option nothing changes.

use crate::figure;
use boxel::prelude::*;
use boxel::render::model::{ModelInstance, ModelLight};

/// The option's line in `--help`.
pub const USAGE: &str = "\
  --models N          N demo figures (entity models) walking in rows in front of the camera,
                      posed every frame (default 0)";

/// Figures per row.
const ROW: u32 = 10;
/// Blocks between two figures.
const SPACING: f64 = 1.5;

/// Parses `--models`; `Ok(false)` when `flag` is not it.
pub fn parse_flag(
    count: &mut u32,
    flag: &str,
    value: &mut impl FnMut() -> Result<String, String>,
) -> Result<bool, String> {
    if flag != "--models" {
        return Ok(false);
    }
    let v = value()?;
    *count = v
        .parse::<u32>()
        .ok()
        .filter(|n| *n <= 10_000)
        .ok_or_else(|| format!("{flag}: {v:?} is not a count in 0..=10000"))?;
    Ok(true)
}

/// One walking figure: its place in the rows.
#[derive(Component)]
struct Walker {
    index: u32,
}

/// The figure and its skin, shared by every walker.
#[derive(Resource)]
struct Demo(figure::Figure);

/// Adds `count` walking figures.
pub struct ModelsPlugin {
    pub count: u32,
}

impl Plugin for ModelsPlugin {
    fn build(&self, app: &mut App) {
        let demo = figure::Figure::new();
        let skin = figure::skin();
        for index in 0..self.count {
            let mut instance = ModelInstance::new(demo.model.clone(), skin.clone());
            instance.origin = figure::FEET;
            instance.light = ModelLight::Probe(Vec3::new(0.0, 1.6, 0.0));
            app.world_mut()
                .spawn((Walker { index }, instance, Transform::default()));
        }
        app.insert_resource(Demo(demo))
            .add_systems(PostUpdate, follow_camera);
    }
}

/// The figures in their rows in front of the active camera, facing it, a step further into
/// their walk.
fn follow_camera(
    time: Res<Time>,
    demo: Res<Demo>,
    cameras: Query<(&Camera, &Transform), Without<Walker>>,
    mut walkers: Query<(&Walker, &mut Transform, &mut ModelInstance)>,
) {
    let Some((_, eye)) = cameras.iter().find(|(c, _)| c.active) else {
        return;
    };
    let forward = eye.forward().as_dvec3();
    let flat = DVec3::new(forward.x, 0.0, forward.z).normalize_or(DVec3::NEG_Z);
    let right = DVec3::new(-flat.z, 0.0, flat.x);
    // Facing the camera: their front (-Z turned by the yaw) along -flat.
    let yaw = (flat.x as f32).atan2(flat.z as f32);
    let t = (time.elapsed_secs_f64() % 3600.0) as f32;
    for (walker, mut transform, mut instance) in &mut walkers {
        let (row, column) = (walker.index / ROW, walker.index % ROW);
        let across = (f64::from(column) - f64::from(ROW - 1) / 2.0) * SPACING;
        let ahead = 4.0 + f64::from(row) * SPACING;
        transform.translation = eye.translation + flat * ahead + right * across - DVec3::Y * 1.5;
        transform.rotation = Quat::from_rotation_y(yaw);
        let phase = t * 17.0 + walker.index as f32 * 0.7;
        instance.pose = demo.0.walk(phase, 0.8, (0.0, 0.0));
    }
}
