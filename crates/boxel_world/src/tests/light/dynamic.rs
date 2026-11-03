//! Dynamic lights baked into the meshes near them (ADR 0063), with the real mesher: a light
//! meshes as a clear emitter placed at its voxel; moving it remeshes only the chunks it reached
//! and reaches; at rest nothing remeshes; edits in its reach relight; the light kept for
//! gameplay is the blocks' own; the same anywhere up to the world limit.
use super::{
    Offset, STONE, SwitchExecutor, edit, hashes, lamp_builder, lit_app, mesh_jobs, overworld,
    remeshed, settle, small, spawn, surface,
};
use crate::jobs::{InlineExecutor, JobExecutor};
use crate::reference::Reference;
use crate::streaming::{WorldJobs, box_chunks};
use crate::{
    BakedLights, ChunkMap, DynamicLight, Interest, MeshState, SkyMap, WorldContent, WorldSettings,
};
use boxel_app::App;
use boxel_ecs::prelude::*;
use boxel_light::PointEmitter;
use boxel_math::DVec3;
use boxel_mesh::MeshOptions;
use boxel_transform::Transform;
use boxel_voxel::{
    BlockCollision, BlockDef, BlockModel, BlockPos, BlockRegistry, BlockState, ChunkPos,
    FaceTextures, RenderShape, TextureId,
};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// A torch's light.
const TORCH: [u8; 3] = [14, 11, 6];

/// The lamp registry plus `test:light`, a clear emitter of [`TORCH`] that draws nothing (as
/// Minecraft's light block): what a dynamic light at its voxel must look like.
fn light_registry() -> (Arc<BlockRegistry>, BlockState) {
    let (mut b, _) = lamp_builder();
    let light = b
        .add_block(BlockDef {
            name: "test:light".to_string(),
            shape: RenderShape::None,
            model: BlockModel::CUBE,
            collision: BlockCollision::None,
            emission: TORCH,
            light_opacity: 0,
            textures: FaceTextures::all(TextureId(0)),
        })
        .expect("light block");
    (Arc::new(b.build()), light)
}

/// The center of voxel `p`.
fn center(p: BlockPos) -> DVec3 {
    p.as_i64vec3().as_dvec3() + DVec3::splat(0.5)
}

/// A dynamic light of `emission` at `at`.
fn spawn_light(app: &mut App, at: DVec3, emission: [u8; 3]) -> Entity {
    app.world_mut()
        .spawn((
            DynamicLight {
                emission,
                ..DynamicLight::default()
            },
            Transform::from_translation(at),
        ))
        .id()
}

fn move_light(app: &mut App, light: Entity, at: DVec3) {
    app.world_mut()
        .get_mut::<Transform>(light)
        .expect("light")
        .translation = at;
}

/// The lights the world bakes, as its jobs see them.
fn points(app: &App) -> Vec<PointEmitter> {
    let jobs = app.world().resource::<WorldJobs>();
    jobs.point_lights.iter().map(|b| b.light).collect()
}

/// Every settled mesh equals the from-scratch reference with the world's baked lights.
fn assert_matches_reference_with_lights(app: &App) -> usize {
    let lights = points(app);
    let map = app.world().resource::<ChunkMap>();
    let sky = app.world().resource::<SkyMap>();
    let content = app.world().resource::<WorldContent>();
    let settings = app.world().resource::<WorldSettings>();
    let options = MeshOptions::with_ao(settings.ao_policy);
    let options = if settings.edge_walls {
        options.with_walls()
    } else {
        options
    };
    let mut reference = Reference::new(map, sky, content).with_points(&lights);
    let mut compared = 0;
    for (_, slot) in map.iter() {
        if !slot.interest.contains(Interest::MESH) || map.data_at(slot.pos).is_none() {
            continue;
        }
        let direct = reference.mesh(slot.pos, options);
        match slot.mesh() {
            MeshState::Ready { mesh, .. } => {
                assert_eq!(**mesh, direct, "{:?}: streamed == from scratch", slot.pos);
            }
            MeshState::Empty => assert!(direct.is_empty(), "{:?}: Empty", slot.pos),
            other => panic!("{:?}: not settled: {other:?}", slot.pos),
        }
        compared += 1;
    }
    compared
}

/// Where these tests hang their light: 3 blocks over the surface near the origin.
fn light_voxel() -> BlockPos {
    BlockPos::new(9, surface(9, 7) + 3, 7)
}

/// The chunks a light of reach `r` at `p` may light.
fn reach(p: BlockPos, r: u8) -> BTreeSet<ChunkPos> {
    let mut out = BTreeSet::new();
    box_chunks(p, r, &mut out);
    out
}

/// A settled overworld around the origin, with the light registry.
fn world(executor: Arc<dyn JobExecutor>) -> (App, BlockState) {
    let (registry, light) = light_registry();
    let mut app = lit_app(overworld(), registry, small(), executor);
    let chunk = ChunkPos::containing(light_voxel());
    spawn(&mut app, chunk, 2, 2);
    settle(&mut app);
    (app, light)
}

/// A dynamic light gives every mesh, byte for byte, what a clear emitter of its emission placed
/// at its voxel gives; the light kept for gameplay stays the blocks' own (ADR 0059).
#[test]
fn a_dynamic_light_meshes_as_a_placed_clear_emitter() {
    let p = light_voxel();
    let (mut dark, light_block) = world(Arc::new(InlineExecutor));
    let before = hashes(&dark);
    let kept = dark.world().resource::<ChunkMap>().light_at(p);
    spawn_light(&mut dark, center(p), TORCH);
    settle(&mut dark);
    let lit = hashes(&dark);
    assert_ne!(lit, before, "the light lights its surroundings");
    assert_eq!(points(&dark), [PointEmitter::new(p, TORCH)]);
    assert!(assert_matches_reference_with_lights(&dark) > 20);
    assert_eq!(
        dark.world().resource::<ChunkMap>().light_at(p),
        kept,
        "gameplay reads the blocks' light"
    );
    let baked = dark.world().resource::<BakedLights>().clone();
    assert_eq!(baked.entities().len(), 1);
    assert_eq!(baked.moves, 1);
    assert_eq!(baked.remeshes, reach(p, 13).len());

    let (mut placed, _) = world(Arc::new(InlineExecutor));
    edit(&mut placed, &[(p, light_block)]);
    settle(&mut placed);
    assert_eq!(
        hashes(&placed),
        lit,
        "a dynamic light looks like a placed one"
    );
}

/// Moving the light to the next voxel remeshes the chunks it reached and reaches, as one batch;
/// within its voxel, or less than the hysteresis past it, nothing; at rest, nothing. Removing it
/// gives the world without it back.
#[test]
fn moving_a_light_remeshes_only_the_chunks_it_reaches() {
    let p = light_voxel();
    let (mut app, _) = world(Arc::new(InlineExecutor));
    let without = hashes(&app);
    let light = spawn_light(&mut app, center(p), TORCH);
    settle(&mut app);
    let jobs = mesh_jobs(&app);
    for _ in 0..20 {
        app.update();
    }
    assert_eq!(mesh_jobs(&app), jobs, "a light at rest remeshes nothing");
    // Inside its voxel, and up to the hysteresis past its border.
    for at in [
        center(p) + DVec3::new(0.4, -0.45, 0.3),
        center(p) + DVec3::new(0.6, 0.0, 0.0),
    ] {
        move_light(&mut app, light, at);
        settle(&mut app);
    }
    assert_eq!(mesh_jobs(&app), jobs, "small moves remesh nothing");
    // To the next voxel, past the hysteresis.
    let next = BlockPos::new(p.x + 1, p.y, p.z);
    move_light(&mut app, light, center(next) + DVec3::new(-0.3, 0.0, 0.0));
    settle(&mut app);
    assert_eq!(points(&app), [PointEmitter::new(next, TORCH)]);
    let expected: BTreeSet<ChunkPos> = reach(p, 13).union(&reach(next, 13)).copied().collect();
    let done = remeshed(&jobs, &mesh_jobs(&app));
    assert!(!done.is_empty());
    assert!(done.is_subset(&expected), "{done:?} not in {expected:?}");
    let map = app.world().resource::<ChunkMap>();
    for &pos in &expected {
        let slot = map.id_at(pos).and_then(|id| map.get(id));
        if let Some(slot) = slot.filter(|s| matches!(s.mesh(), MeshState::Ready { .. })) {
            assert!(
                done.contains(&slot.pos),
                "{pos:?} has a mesh and was not remeshed"
            );
        }
    }
    let baked = app.world().resource::<BakedLights>();
    assert_eq!(baked.moves, 2);
    assert!(assert_matches_reference_with_lights(&app) > 20);
    app.world_mut().despawn(light);
    settle(&mut app);
    assert!(points(&app).is_empty());
    assert!(app.world().resource::<BakedLights>().is_empty());
    assert_eq!(hashes(&app), without, "no light: the world as before");
}

/// An edit in a light's reach that changes what light goes through remeshes the light's chunks,
/// though the light kept around it is dark (a sealed room underground, across a chunk border):
/// every mesh stays the from-scratch one.
#[test]
fn edits_in_a_lights_reach_relight() {
    let (mut app, _) = world(Arc::new(InlineExecutor));
    // A stone shell around x 27..=37, z 5..=9, 5 high, 20 blocks under the surface.
    let y0 = surface(32, 7) - 20;
    let mut room = Vec::new();
    for x in 26..=38 {
        for y in y0 - 1..=y0 + 5 {
            for z in 4..=10 {
                let inside =
                    (27..=37).contains(&x) && (y0..y0 + 5).contains(&y) && (5..=9).contains(&z);
                room.push((
                    BlockPos::new(x, y, z),
                    if inside { BlockState::AIR } else { STONE },
                ));
            }
        }
    }
    edit(&mut app, &room);
    settle(&mut app);
    let p = BlockPos::new(28, y0 + 2, 7);
    spawn_light(&mut app, center(p), TORCH);
    settle(&mut app);
    // A pillar next to the light: its shadow falls on the far side of the chunk border at 32.
    let pillar: Vec<(BlockPos, BlockState)> = (y0..y0 + 5)
        .flat_map(|y| (5..=9).map(move |z| (BlockPos::new(29, y, z), STONE)))
        .collect();
    let map = app.world().resource::<ChunkMap>();
    assert_eq!(
        map.light_at(pillar[0].0).map(|l| l.0),
        Some(0),
        "dark around it"
    );
    edit(&mut app, &pillar);
    settle(&mut app);
    assert!(assert_matches_reference_with_lights(&app) > 20);
}

/// Without lights, with a dark one, or with `baked_lights` 0, the world meshes each chunk once
/// and as before, and bakes nothing.
#[test]
fn no_light_bakes_nothing() {
    let p = light_voxel();
    let (mut app, _) = world(Arc::new(InlineExecutor));
    let (want, jobs) = (hashes(&app), mesh_jobs(&app));
    assert!(jobs.values().all(|&n| n == 1));
    let light = spawn_light(&mut app, center(p), [0; 3]);
    settle(&mut app);
    assert!(points(&app).is_empty());
    app.world_mut().resource_mut::<WorldSettings>().baked_lights = 0;
    app.world_mut()
        .get_mut::<DynamicLight>(light)
        .expect("light")
        .emission = TORCH;
    for _ in 0..5 {
        app.update();
    }
    settle(&mut app);
    assert!(points(&app).is_empty());
    assert_eq!(
        *app.world().resource::<BakedLights>(),
        BakedLights::default()
    );
    assert_eq!((hashes(&app), mesh_jobs(&app)), (want, jobs));
}

/// A light that moves while its remesh is on its way stays where that remesh shows it until it
/// is published, then (the next frame) goes to where it is.
#[test]
fn a_light_waits_for_its_remesh() {
    let p = light_voxel();
    let executor = Arc::new(SwitchExecutor::default());
    let (mut app, _) = world(Arc::clone(&executor) as Arc<dyn JobExecutor>);
    let light = spawn_light(&mut app, center(p), TORCH);
    settle(&mut app);
    executor.hold.store(true, Ordering::Relaxed);
    let next = BlockPos::new(p.x, p.y + 1, p.z);
    move_light(&mut app, light, center(next));
    app.update();
    assert!(executor.held() > 0);
    let last = BlockPos::new(p.x, p.y + 2, p.z);
    move_light(&mut app, light, center(last));
    for _ in 0..3 {
        app.update();
    }
    assert_eq!(points(&app), [PointEmitter::new(next, TORCH)], "waits");
    executor.hold.store(false, Ordering::Relaxed);
    executor.run(usize::MAX);
    settle(&mut app);
    assert_eq!(points(&app), [PointEmitter::new(next, TORCH)], "published");
    // The next frame it goes on.
    app.update();
    settle(&mut app);
    assert_eq!(points(&app), [PointEmitter::new(last, TORCH)]);
    assert!(assert_matches_reference_with_lights(&app) > 20);
}

/// The same world and light moved anywhere (across +-2^31 and at +-2^40, ADR 0014) gives the
/// same meshes and remeshes the same chunks, relative to the light. (A `Transform` cannot hold
/// a light's voxel near the world limit; `boxel_light`'s tests flood there.)
#[test]
fn dynamic_lights_are_the_same_anywhere() {
    let run = |offset: ChunkPos| {
        let (registry, _) = light_registry();
        let generator = Offset {
            inner: overworld(),
            offset,
        };
        let mut app = lit_app(generator, registry, small(), Arc::new(InlineExecutor));
        let shift = |p: BlockPos| {
            let o = offset.origin();
            BlockPos::new(p.x + o.x, p.y + o.y, p.z + o.z)
        };
        let p = shift(light_voxel());
        let chunk = ChunkPos::containing(p);
        spawn(&mut app, chunk, 1, 1);
        settle(&mut app);
        let light = spawn_light(&mut app, center(p), TORCH);
        settle(&mut app);
        let jobs = mesh_jobs(&app);
        let next = BlockPos::new(p.x - 1, p.y, p.z + 1);
        move_light(&mut app, light, center(next));
        settle(&mut app);
        let relative = |c: ChunkPos| ChunkPos::new(c.x - offset.x, c.y - offset.y, c.z - offset.z);
        let done: BTreeSet<ChunkPos> = remeshed(&jobs, &mesh_jobs(&app))
            .into_iter()
            .map(relative)
            .collect();
        let meshes: Vec<(ChunkPos, Option<u64>)> = hashes(&app)
            .into_iter()
            .map(|(c, h)| (relative(c), h))
            .collect();
        assert!(assert_matches_reference_with_lights(&app) > 5);
        (done, meshes)
    };
    let want = run(ChunkPos::new(0, 0, 0));
    assert!(!want.0.is_empty());
    for offset in [
        ChunkPos::new(1 << 26, 0, -(1 << 26)),
        ChunkPos::new((1 << 26) - 1, 0, 0),
        ChunkPos::new(-(1 << 35), 0, 1 << 35),
    ] {
        assert!(run(offset) == want, "differs at {offset:?}");
    }
}
