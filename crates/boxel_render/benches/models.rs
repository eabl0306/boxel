//! The CPU half of the entity models (ADR 0040, `ModelScene::update`): one frame of instances of
//! a player-like model (six parts, each with an outer layer: 12 boxes, 72 quads), as the render
//! plugin hands them to the renderer.
//!
//! - `models/animated/N`: every instance has a new pose (a walk cycle a step further): all of them
//!   posed and placed, the frame of N walking mobs.
//! - `models/camera/N`: the same poses, the render origin one block over: placed, not posed.
//! - `models/still/N`: nothing changed: nothing posed, placed or uploaded.
use boxel_math::{DVec3, I64Vec3, Quat, Vec3};
use boxel_render::model::{
    Cuboid, Model, ModelDraw, ModelLayer, ModelScene, ModelTexture, PartPose,
};
use boxel_voxel::LightValue;
use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::sync::Arc;

/// Six parts, each a box and an inflated outer box: Minecraft's player.
fn model() -> Arc<Model> {
    let mut b = Model::builder([64, 64]);
    // Name, pivot, texture origin, box origin and size.
    #[rustfmt::skip]
    let parts: [(&str, [f32; 3], [f32; 2], [f32; 3], [f32; 3]); 6] = [
        ("head", [0.0, 0.0, 0.0], [0.0, 0.0], [-4.0, -8.0, -4.0], [8.0, 8.0, 8.0]),
        ("body", [0.0, 0.0, 0.0], [16.0, 16.0], [-4.0, 0.0, -2.0], [8.0, 12.0, 4.0]),
        ("right_arm", [-5.0, 2.0, 0.0], [40.0, 16.0], [-3.0, -2.0, -2.0], [4.0, 12.0, 4.0]),
        ("left_arm", [5.0, 2.0, 0.0], [32.0, 48.0], [-1.0, -2.0, -2.0], [4.0, 12.0, 4.0]),
        ("right_leg", [-1.9, 12.0, 0.0], [0.0, 16.0], [-2.0, 0.0, -2.0], [4.0, 12.0, 4.0]),
        ("left_leg", [1.9, 12.0, 0.0], [16.0, 48.0], [-2.0, 0.0, -2.0], [4.0, 12.0, 4.0]),
    ];
    for (name, pivot, uv, origin, size) in parts {
        let part = b.part(name, None, PartPose::at(pivot));
        b.cuboid(part, Cuboid::new(uv, origin, size));
        b.cuboid(
            part,
            Cuboid::new([uv[0] + 32.0, uv[1]], origin, size)
                .inflate(0.25)
                .cutout(),
        );
    }
    Arc::new(b.build().expect("valid"))
}

/// `n` instances in a grid, walking at `phase`.
fn draws(model: &Arc<Model>, texture: &ModelTexture, n: u64, phase: f32) -> Vec<ModelDraw> {
    (0..n)
        .map(|i| {
            let mut pose = model.rest_pose();
            let swing = (phase + i as f32) * 0.6662;
            pose.parts[2].rotation.x = swing.cos() * 0.8;
            pose.parts[3].rotation.x = -swing.cos() * 0.8;
            pose.parts[4].rotation.x = -swing.cos() * 1.1;
            pose.parts[5].rotation.x = swing.cos() * 1.1;
            pose.parts[0].rotation.y = 0.3;
            ModelDraw {
                id: i,
                model: Arc::clone(model),
                texture: texture.clone(),
                pose,
                layer: ModelLayer::World,
                position: DVec3::new((i % 10) as f64 * 1.5, 64.0, (i / 10) as f64 * 1.5 + 4.0),
                rotation: Quat::from_rotation_y(i as f32),
                origin: Vec3::new(0.0, 24.0, 0.0),
                scale: 0.9375,
                light: LightValue::FULL_SKY,
                realm: boxel_render::model::ModelRealm::default(),
            }
        })
        .collect()
}

fn bench(c: &mut Criterion) {
    let model = model();
    let texture = ModelTexture::new(64, 64, vec![200; 64 * 64 * 4]).expect("valid");
    let origin = I64Vec3::new(0, 65, 0);
    let eye = Vec3::splat(0.5);
    for n in [1, 50] {
        let mut scene = ModelScene::default();
        let mut phase = 0.0;
        c.bench_function(&format!("models/animated/{n}"), |b| {
            b.iter(|| {
                phase += 0.12;
                let frame = draws(&model, &texture, n, phase);
                black_box(scene.update(&frame, origin, eye, Quat::IDENTITY));
            });
        });
        let still = draws(&model, &texture, n, 0.0);
        let mut scene = ModelScene::default();
        scene.update(&still, origin, eye, Quat::IDENTITY);
        let mut x = 0;
        c.bench_function(&format!("models/camera/{n}"), |b| {
            b.iter(|| {
                x += 1;
                let moved = origin + I64Vec3::new(x & 1, 0, 0);
                black_box(scene.update(&still, moved, eye, Quat::IDENTITY));
            });
        });
        c.bench_function(&format!("models/still/{n}"), |b| {
            b.iter(|| black_box(scene.update(&still, origin, eye, Quat::IDENTITY)));
        });
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
