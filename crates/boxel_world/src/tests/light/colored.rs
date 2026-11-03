//! Colored sky in the world (ADR 0064): with `WorldSettings::colored_sky`, light filters above
//! the sky tops tint the direct sky under them and every filter tints the sky spread through
//! it. The meshes do not show the tint yet (lane C3), so most tests mesh with [`light_probe`],
//! which turns the light a mesh job gives its mesher, sky channels included, into the mesh's
//! hash. Off (the default), or without glass, every mesh is the one of before; on, the light
//! equals the from-scratch [`Reference`] after edits above and below the tops and in beams,
//! whatever the load order, the executor, the thread count or the distance from the origin.
use super::beams::{MAIN, ShuffleExecutor, golden_digest_with};
use super::*;
use crate::tests::support::{LitCavern, Sources};
use boxel_light::{PaddedLight, PointEmitter};
use boxel_math::DVec3;
use boxel_mesh::{BUCKET_COUNT, ChunkMesh, MaterialTable, MeshInput, PackedQuad};
use boxel_voxel::{BlockRegistryBuilder, CHUNK_VOLUME, LightTables};

/// Filters of three of the game's dye colors (ADR 0064, table of sec. 1).
const RED: [u8; 3] = [10, 3, 2];
const LIGHT_BLUE: [u8; 3] = [3, 11, 13];
const BLACK: [u8; 3] = [2, 2, 2];

/// The colored glasses of these tests: opacity 0, emission 0 and a light filter each.
#[derive(Clone, Copy, Debug)]
struct Glass {
    red: BlockState,
    light_blue: BlockState,
    black: BlockState,
}

impl Glass {
    /// Adds `test:red_glass`, `test:light_blue_glass` and `test:black_glass` to `b`.
    fn add(b: &mut BlockRegistryBuilder) -> Self {
        let mut glass = |name: &str, filter| {
            let name = format!("test:{name}");
            let state = b
                .add_block(BlockDef {
                    name: name.clone(),
                    shape: RenderShape::Translucent,
                    model: BlockModel::CUBE,
                    collision: BlockCollision::Full,
                    emission: [0; 3],
                    light_opacity: 0,
                    textures: FaceTextures::all(TextureId(0)),
                })
                .expect("glass");
            let block = b.block_by_name(&name).expect("glass");
            b.set_light_filter(block, filter).expect("filter");
            state
        };
        Self {
            red: glass("red_glass", RED),
            light_blue: glass("light_blue_glass", LIGHT_BLUE),
            black: glass("black_glass", BLACK),
        }
    }
    /// The glasses of `registry`.
    fn of(registry: &BlockRegistry) -> Self {
        let state = |name| registry.by_name(name).expect("glass");
        Self {
            red: state("test:red_glass"),
            light_blue: state("test:light_blue_glass"),
            black: state("test:black_glass"),
        }
    }
}

/// The lamp registry plus [`Glass`].
fn tint_registry() -> (Arc<BlockRegistry>, Glass) {
    let (mut b, _) = lamp_builder();
    let glass = Glass::add(&mut b);
    (Arc::new(b.build()), glass)
}

/// Stone under y = 0 (layers -2 and -1), and on it the scenes of ADR 0064:
///
/// - a greenhouse: stone walls (y 0 to 8) around x 4..=40, z 4..=24, under a red glass roof
///   (y = 9) with a black skylight (x 20..=23, z 10..=13), and a light blue window in its north
///   wall (z = 4, x 10..=16, y 2..=5);
/// - a closed room: stone walls and roof (y = 9) around x 50..=62, z 40..=52, lit only through
///   a light blue window in its west wall (x = 50, z 44..=48, y 2..=5).
struct GlassHouse;

impl GlassHouse {
    fn at(x: i64, y: i64, z: i64, stone: BlockState, g: Glass) -> BlockState {
        if y < 0 {
            return stone;
        }
        if (4..=40).contains(&x) && (4..=24).contains(&z) {
            if y == 9 {
                let skylight = (20..=23).contains(&x) && (10..=13).contains(&z);
                return if skylight { g.black } else { g.red };
            }
            if (x == 4 || x == 40 || z == 4 || z == 24) && y <= 8 {
                let window = z == 4 && (10..=16).contains(&x) && (2..=5).contains(&y);
                return if window { g.light_blue } else { stone };
            }
            return BlockState::AIR;
        }
        if (50..=62).contains(&x) && (40..=52).contains(&z) && y <= 9 {
            if !(x == 50 || x == 62 || z == 40 || z == 52 || y == 9) {
                return BlockState::AIR;
            }
            let window = x == 50 && (44..=48).contains(&z) && (2..=5).contains(&y);
            return if window { g.light_blue } else { stone };
        }
        BlockState::AIR
    }
}

impl WorldGenerator for GlassHouse {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let stone = registry.by_name("boxel:stone").expect("stone");
        match key.pos.y {
            -2 | -1 => return ChunkData::uniform(stone),
            0 => {}
            _ => return ChunkData::uniform(BlockState::AIR),
        }
        let g = Glass::of(registry);
        let o = key.pos.origin();
        let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
        for (i, cell) in dense.iter_mut().enumerate() {
            let p = LocalPos::from_index(i);
            let (x, y, z) = (
                o.x + i64::from(p.x()),
                o.y + i64::from(p.y()),
                o.z + i64::from(p.z()),
            );
            *cell = Self::at(x, y, z, stone, g);
        }
        ChunkData::from_dense(&dense)
    }
    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        -2..=1
    }
    fn name(&self) -> &str {
        "glass house"
    }
}

/// A one-quad mesh whose hash is the hash of `light`: every value, uncolored and sky channels.
fn probe_mesh(light: Option<&PaddedLight>) -> ChunkMesh {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    if let Some(light) = light {
        for (v, c) in light.to_vec().iter().zip(light.to_sky_rgb_vec()) {
            let word = u64::from(v.0)
                | u64::from(c[0]) << 16
                | u64::from(c[1]) << 20
                | u64::from(c[2]) << 24;
            h = (h ^ word).wrapping_mul(0x0100_0000_01b3);
        }
    }
    let mut counts = [0; BUCKET_COUNT];
    counts[0] = 1;
    let quad = PackedQuad {
        geo: h as u32,
        mat: (h >> 32) as u32,
    };
    ChunkMesh::from_quads(vec![quad], counts)
}

/// A mesher that shows the light it is given ([`probe_mesh`]).
fn light_probe(input: &MeshInput<'_>, _: &MaterialTable, _: MeshOptions) -> ChunkMesh {
    probe_mesh(input.light)
}

/// [`lit_app`] with colored sky on (`colored`) and the [`light_probe`] mesher (`probe`).
fn colored_app(
    generator: impl WorldGenerator,
    registry: Arc<BlockRegistry>,
    colored: bool,
    probe: bool,
    executor: Arc<dyn JobExecutor>,
) -> App {
    let settings = WorldSettings {
        colored_sky: colored,
        ..small()
    };
    let mut app = lit_app(generator, registry, settings, executor);
    if probe {
        app.world_mut().resource_mut::<WorldJobs>().mesh_fn = light_probe;
    }
    app
}

/// Where the glass house views end: over the greenhouse, at the ground.
const HOUSE: ChunkPos = ChunkPos::new(1, 0, 0);

/// The glass house settled around [`HOUSE`], jobs run at once.
fn house(colored: bool, probe: bool) -> (App, Glass) {
    let (registry, glass) = tint_registry();
    let mut app = colored_app(
        GlassHouse,
        registry,
        colored,
        probe,
        Arc::new(InlineExecutor),
    );
    spawn(&mut app, HOUSE, 2, 2);
    settle(&mut app);
    (app, glass)
}

/// The lights the world bakes, as its jobs see them.
fn points(app: &App) -> Vec<PointEmitter> {
    let jobs = app.world().resource::<WorldJobs>();
    jobs.point_lights.iter().map(|b| b.light).collect()
}

/// In a world meshed with [`light_probe`], every settled mesh holds the light from scratch (with
/// the world's dynamic lights), every cached light equals the light from scratch, sky channels
/// included, and every walked tile equals a fresh walk, tints included. Returns how many meshes
/// were compared.
fn assert_colored_matches_reference(app: &App) -> usize {
    let lights = points(app);
    let map = app.world().resource::<ChunkMap>();
    let sky = app.world().resource::<SkyMap>();
    let content = app.world().resource::<WorldContent>();
    let mut reference = Reference::new(map, sky, content).with_points(&lights);
    let mut compared = 0;
    for (_, slot) in map.iter() {
        if !slot.interest.contains(Interest::MESH) || map.data_at(slot.pos).is_none() {
            continue;
        }
        match slot.mesh() {
            MeshState::Ready { mesh, .. } => {
                let direct = probe_mesh(reference.mesh_light(slot.pos).as_ref());
                assert_eq!(mesh.hash, direct.hash, "{:?}: light of the mesh", slot.pos);
                compared += 1;
            }
            MeshState::Empty => {}
            other => panic!("{:?}: not settled: {other:?}", slot.pos),
        }
    }
    for (_, slot) in map.iter() {
        let (Some(light), Some(_)) = (slot.light(), map.data_at(slot.pos)) else {
            continue;
        };
        let direct = reference.light(slot.pos).expect("light on");
        for i in 0..CHUNK_VOLUME {
            let p = LocalPos::from_index(i);
            let (x, y, z) = (p.x() as i32, p.y() as i32, p.z() as i32);
            assert_eq!(light.get(p), direct.get(x, y, z), "{:?} {p:?}", slot.pos);
            assert_eq!(
                light.sky_rgb(p),
                direct.sky_rgb(x, y, z),
                "{:?} {p:?}: sky",
                slot.pos
            );
        }
    }
    for c in sky.columns.values() {
        let Some(tile) = c.exact() else {
            continue;
        };
        let fresh = reference.tile(c.x, c.z).expect("walked");
        assert_eq!(tile.tops(), fresh.tops(), "tops of ({}, {})", c.x, c.z);
        assert_eq!(tile.colored_sky(), fresh.colored_sky());
        for x in 0..32 {
            for z in 0..32 {
                assert_eq!(
                    tile.tints(x, z),
                    fresh.tints(x, z),
                    "tints of ({}, {}) at {x} {z}",
                    c.x,
                    c.z
                );
            }
        }
    }
    compared
}

/// The cached light (uncolored and sky channels) of every chunk with mesh interest, folded into
/// one number per chunk (chunks out of the view may still be loaded, depending on the path).
fn light_hashes(app: &App) -> BTreeMap<ChunkPos, u64> {
    let map = app.world().resource::<ChunkMap>();
    map.iter()
        .filter(|(_, s)| s.interest.contains(Interest::MESH))
        .filter_map(|(_, s)| {
            let light = s.light()?;
            let mut h = 0xcbf2_9ce4_8422_2325_u64;
            for i in 0..CHUNK_VOLUME {
                let p = LocalPos::from_index(i);
                let [r, g, b] = light.sky_rgb(p);
                let word = u64::from(light.get(p).0)
                    | u64::from(r) << 16
                    | u64::from(g) << 20
                    | u64::from(b) << 24;
                h = (h ^ word).wrapping_mul(0x0100_0000_01b3);
            }
            Some((s.pos, h))
        })
        .collect()
}

fn sky_rgb_at(app: &App, p: BlockPos) -> [u8; 3] {
    app.world()
        .resource::<ChunkMap>()
        .sky_rgb_at(p)
        .unwrap_or_else(|| panic!("no cached light at {p:?}"))
}

/// The colored counters: light boxes, padded results and chunk lights.
fn colored_counts(app: &App) -> [usize; 3] {
    let s = stats(app);
    [
        s.colored_light_boxes,
        s.colored_padded,
        s.colored_chunk_light,
    ]
}

/// The glass house is tinted as from scratch: under the red roof the direct sky is
/// `(13, 7, 6)` (gameplay reads 15), under the black skylight its red neighbors win, and the
/// closed room gets light blue sky through its window only. The real mesher's meshes (which do
/// not show the tint yet) equal the reference too, and each chunk is meshed once.
#[test]
fn a_glass_house_is_tinted_as_from_scratch() {
    let (app, _) = house(true, true);
    assert!(assert_colored_matches_reference(&app) > 10);
    let jobs = mesh_jobs(&app);
    assert!(jobs.values().all(|&n| n == 1), "{jobs:?}");
    let roof = BlockPos::new(12, 3, 14);
    assert_eq!(sky_rgb_at(&app, roof), [13, 7, 6]);
    assert_eq!(
        light_at(&app, roof).sky(),
        15,
        "gameplay reads the uncolored sky"
    );
    // Two voxels from the red roof's direct sky, under black glass (6, 6, 6).
    assert_eq!(sky_rgb_at(&app, BlockPos::new(21, 3, 11)), [11, 6, 6]);
    // Two voxels behind the window: 14 entering the glass, then two steps.
    let room = BlockPos::new(52, 3, 46);
    assert_eq!(sky_rgb_at(&app, room), [4, 11, 11]);
    assert_eq!(light_at(&app, room).sky(), 12);
    let s = stats(&app);
    let [boxes, padded, chunks] = colored_counts(&app);
    println!(
        "colored: boxes={boxes} padded={padded} chunk_light={chunks} light_full={} \
         light_fast={} light_jobs={}",
        s.light_full, s.light_fast, s.light_jobs
    );
    assert!(
        boxes > 0 && padded > 0 && chunks > 0,
        "{boxes} {padded} {chunks}"
    );
    assert!(padded <= boxes);
    assert!(app.world().resource::<SkyMap>().colored_sky());

    let (real, _) = house(true, false);
    assert!(assert_matches_reference(&real) > 10);
}

/// Off, the glass is in the same place but nothing is colored: no colored box, no tint in any
/// tile, no colored light, and the sky under the roof is white as before.
#[test]
fn colored_sky_off_keeps_the_sky_white() {
    let (app, _) = house(false, true);
    assert_eq!(colored_counts(&app), [0; 3]);
    let sky = app.world().resource::<SkyMap>();
    assert!(!sky.colored_sky());
    assert!(
        sky.columns
            .values()
            .filter_map(|c| c.exact())
            .all(|t| { !t.colored_sky() && t.tint_count() == 0 })
    );
    let map = app.world().resource::<ChunkMap>();
    assert!(
        map.iter()
            .all(|(_, s)| s.light().is_none_or(|l| !l.is_colored()))
    );
    assert_eq!(sky_rgb_at(&app, BlockPos::new(12, 3, 14)), [15; 3]);
    assert!(assert_colored_matches_reference(&app) > 10);
}

/// Without filters, or with filters registered but none placed, colored sky changes nothing:
/// no colored box, and the meshes `main` gave before sky sources, bit for bit.
#[test]
fn colored_sky_without_glass_is_the_world_before() {
    let colored = WorldSettings {
        colored_sky: true,
        ..small()
    };
    let (registry, lamps) = lamp_registry();
    assert!(!LightTables::from_registry(&registry).any_filter());
    let (digest, app) = golden_digest_with(registry, lamps, colored.clone());
    assert_eq!(digest, MAIN, "{digest:#x}");
    assert_eq!(colored_counts(&app), [0; 3]);
    let (mut b, lamps) = lamp_builder();
    Glass::add(&mut b);
    let registry = Arc::new(b.build());
    assert!(LightTables::from_registry(&registry).any_filter());
    let (digest, app) = golden_digest_with(registry, lamps, colored);
    assert_eq!(digest, MAIN, "{digest:#x}");
    assert_eq!(colored_counts(&app), [0; 3]);
    let sky = app.world().resource::<SkyMap>();
    assert!(
        sky.columns
            .values()
            .filter_map(|c| c.exact())
            .all(|t| t.tint_count() == 0)
    );
}

/// A square of `state` at height `y`, x and z in `x0..x0 + n`, `z0..z0 + n`.
fn pane(x0: i64, y: i64, z0: i64, n: i64, state: BlockState) -> Vec<(BlockPos, BlockState)> {
    let mut out = Vec::new();
    for x in x0..x0 + n {
        for z in z0..z0 + n {
            out.push((BlockPos::new(x, y, z), state));
        }
    }
    out
}

/// Edits above and below the tops relight as a rebuild from scratch: a pane placed in the open
/// air, swapped for another color and removed; glass placed in the closed room (below its stone
/// roof) and its window swapped; stone placed over the glass roof and removed; the roof's own
/// voxel made stone and glass again; the whole roof swapped in one batch; glass far above every
/// blocker.
#[test]
fn glass_edits_relight_as_from_scratch() {
    let (mut app, g) = house(true, true);
    let before = hashes(&app);
    // Under the middle of a 17 x 17 pane, 9 voxels from the white sky around it.
    let under = BlockPos::new(45, 10, 33);
    assert_eq!(sky_rgb_at(&app, under), [15; 3]);
    let check = |app: &mut App| {
        settle(app);
        assert!(assert_colored_matches_reference(app) > 10);
    };
    // Above the tops, in the open air.
    edit(&mut app, &pane(37, 20, 25, 17, g.red));
    check(&mut app);
    assert_eq!(sky_rgb_at(&app, under), [13, 7, 6]);
    edit(&mut app, &pane(37, 20, 25, 17, g.light_blue));
    check(&mut app);
    assert_eq!(sky_rgb_at(&app, under), [7, 14, 14]);
    edit(&mut app, &pane(37, 20, 25, 17, BlockState::AIR));
    check(&mut app);
    assert_eq!(
        hashes(&app),
        before,
        "removing the pane equals never placing it"
    );
    // Below the tops: in the closed room, and its window.
    let inside = BlockPos::new(52, 3, 46);
    edit(&mut app, &[(BlockPos::new(51, 3, 46), g.red)]);
    check(&mut app);
    let window: Vec<_> = (44..=48)
        .flat_map(|z| (2..=5).map(move |y| (BlockPos::new(50, y, z), g.red)))
        .collect();
    edit(&mut app, &window);
    check(&mut app);
    let [r, gr, b] = sky_rgb_at(&app, inside);
    assert!(r > gr && r > b, "a red window: {r} {gr} {b}");
    // Stone over the roof, then the roof's own voxel made stone, then both back.
    let over = BlockPos::new(12, 10, 14);
    edit(&mut app, &[(over, STONE)]);
    check(&mut app);
    assert!(light_at(&app, BlockPos::new(12, 8, 14)).sky() < 15);
    edit(
        &mut app,
        &[(over, BlockState::AIR), (BlockPos::new(14, 9, 14), STONE)],
    );
    check(&mut app);
    edit(&mut app, &[(BlockPos::new(14, 9, 14), g.red)]);
    check(&mut app);
    // The whole roof light blue, in one batch.
    let mut roof = Vec::new();
    for x in 4..=40 {
        for z in 4..=24 {
            roof.push((BlockPos::new(x, 9, z), g.light_blue));
        }
    }
    edit(&mut app, &roof);
    check(&mut app);
    assert_eq!(sky_rgb_at(&app, BlockPos::new(12, 3, 14)), [7, 14, 14]);
    // Far above every blocker and the generator (its top voxel is 63): black (6) under it, but
    // white from the next column.
    let high = BlockPos::new(30, 80, 30);
    edit(&mut app, &[(high, g.black)]);
    check(&mut app);
    assert_eq!(sky_rgb_at(&app, BlockPos::new(30, 70, 30)), [14; 3]);
    assert_eq!(light_at(&app, BlockPos::new(30, 70, 30)).sky(), 15);
    let s = stats(&app);
    assert!(s.light_fanout > 0);
}

/// The glass house does not depend on the viewer's path, the executor, the thread count or the
/// order jobs finish in: the same light in every mesh and every cached chunk.
#[test]
fn a_glass_house_does_not_depend_on_the_load_order_or_the_threads() {
    let run = |executor: Arc<dyn JobExecutor>, shuffle: Option<&ShuffleExecutor>, path| {
        let (registry, _) = tint_registry();
        let mut app = colored_app(GlassHouse, registry, true, true, executor);
        let path: &[ChunkPos] = path;
        let viewer = spawn(&mut app, path[0], 2, 2);
        let step = |app: &mut App| {
            app.update();
            if let Some(s) = shuffle {
                s.run(6);
            }
        };
        for &p in path {
            move_viewer(&mut app, viewer, p);
            for _ in 0..2 {
                step(&mut app);
            }
        }
        move_viewer(&mut app, viewer, HOUSE);
        let start = Instant::now();
        loop {
            step(&mut app);
            drain_events(&mut app);
            if stats(&app).is_settled() {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(120), "not settled");
            std::thread::yield_now();
        }
        (hashes(&app), light_hashes(&app), app)
    };
    let fly: Vec<ChunkPos> = (-4..=1).map(|x| ChunkPos::new(x, 0, 0)).collect();
    let dive: Vec<ChunkPos> = (0..=6).rev().map(|y| ChunkPos::new(1, y, 0)).collect();
    let teleport = vec![ChunkPos::new(4000, 0, -900)];
    let (meshes, lights, app) = run(Arc::new(InlineExecutor), None, &fly);
    assert!(meshes.values().filter(|h| h.is_some()).count() > 10);
    assert!(lights.len() > 20);
    assert!(assert_colored_matches_reference(&app) > 10);
    for (name, executor, path) in [
        (
            "dive",
            Arc::new(InlineExecutor) as Arc<dyn JobExecutor>,
            &dive,
        ),
        ("teleport", Arc::new(InlineExecutor), &teleport),
        ("task pool", Arc::new(TaskPoolExecutor), &fly),
        ("1 thread", PoolExecutor::new(1), &dive),
        ("6 threads", PoolExecutor::new(6), &teleport),
    ] {
        let (m, l, _) = run(executor, None, path);
        assert!(m == meshes, "{name}: meshes differ");
        assert!(l == lights, "{name}: cached light differs");
    }
    for (seed, path) in [(7, &fly), (0x5eed, &dive), (99, &teleport)] {
        let shuffle = ShuffleExecutor::new(seed);
        let (m, l, _) = run(shuffle.clone(), Some(&shuffle), path);
        assert!(m == meshes, "seed {seed}: meshes differ");
        assert!(l == lights, "seed {seed}: cached light differs");
    }
}

/// The glass house at x = 2^40 and y = -2^40 (voxels) gives the light of the origin, bit for
/// bit, before and after a pane is placed over it.
#[test]
fn a_far_glass_house_is_tinted_like_the_origin() {
    let far = ChunkPos::new(1 << 35, -(1 << 35), 7);
    let run = |offset: ChunkPos| {
        let (registry, g) = tint_registry();
        let generator = Offset {
            inner: GlassHouse,
            offset,
        };
        let mut app = colored_app(generator, registry, true, true, Arc::new(InlineExecutor));
        let at = |p: ChunkPos| ChunkPos::new(p.x + offset.x, p.y + offset.y, p.z + offset.z);
        spawn(&mut app, at(HOUSE), 2, 2);
        settle(&mut app);
        let relative = |app: &App| {
            let meshes: BTreeMap<ChunkPos, Option<u64>> = hashes(app)
                .into_iter()
                .map(|(p, h)| {
                    (
                        ChunkPos::new(p.x - offset.x, p.y - offset.y, p.z - offset.z),
                        h,
                    )
                })
                .collect();
            let lights: BTreeMap<ChunkPos, u64> = light_hashes(app)
                .into_iter()
                .map(|(p, h)| {
                    (
                        ChunkPos::new(p.x - offset.x, p.y - offset.y, p.z - offset.z),
                        h,
                    )
                })
                .collect();
            (meshes, lights)
        };
        let first = relative(&app);
        let o = offset.origin();
        let edits: Vec<_> = pane(44, 20, 28, 3, g.red)
            .into_iter()
            .map(|(p, s)| (BlockPos::new(p.x + o.x, p.y + o.y, p.z + o.z), s))
            .collect();
        edit(&mut app, &edits);
        settle(&mut app);
        (first, relative(&app), app)
    };
    let (near_first, near_edited, _) = run(ChunkPos::new(0, 0, 0));
    let (far_first, far_edited, app) = run(far);
    assert_eq!(far_first, near_first);
    assert_eq!(far_edited, near_edited);
    assert_ne!(near_edited, near_first);
    assert!(assert_colored_matches_reference(&app) > 10);
    assert!(colored_counts(&app)[1] > 0);
}

/// A dynamic light under the red roof (ADR 0063) adds block light to the colored boxes: every
/// mesh holds the light from scratch with it, sky channels unchanged, as it moves and goes.
#[test]
fn a_dynamic_light_under_a_glass_roof_meshes_as_from_scratch() {
    let (mut app, _) = house(true, true);
    let before = hashes(&app);
    let p = BlockPos::new(12, 3, 14);
    let light = app
        .world_mut()
        .spawn((
            crate::DynamicLight {
                emission: [14, 11, 6],
                ..crate::DynamicLight::default()
            },
            Transform::from_translation(p.as_i64vec3().as_dvec3() + DVec3::splat(0.5)),
        ))
        .id();
    settle(&mut app);
    assert_ne!(hashes(&app), before);
    assert_eq!(points(&app).len(), 1);
    assert!(assert_colored_matches_reference(&app) > 10);
    app.world_mut()
        .get_mut::<Transform>(light)
        .expect("light")
        .translation = DVec3::new(30.5, 4.5, 20.5);
    settle(&mut app);
    assert!(assert_colored_matches_reference(&app) > 10);
    app.world_mut().despawn(light);
    settle(&mut app);
    assert_eq!(hashes(&app), before);
}

/// The lit cavern (ADR 0061) with glass: a red pane placed in a roof source's beam tints the
/// cells under it, retinted in place when swapped, white again once removed; every light equals
/// the light from scratch.
#[test]
fn glass_in_a_beam_tints_it_as_from_scratch() {
    let mut b = boxel_demo::blocks::builder();
    boxel_voxel::testing::add_sky_source(&mut b).expect("sources");
    let g = Glass::add(&mut b);
    let registry = Arc::new(b.build());
    let _ = Sources::of(&registry);
    let mut app = colored_app(LitCavern, registry, true, true, Arc::new(InlineExecutor));
    spawn(&mut app, ChunkPos::new(0, -3, 0), 2, 2);
    settle(&mut app);
    let before = hashes(&app);
    assert!(assert_colored_matches_reference(&app) > 10);
    // The beam of the roof source at (10, -60, 4), above the cavern floor.
    let p = BlockPos::new(10, -100, 4);
    let below = BlockPos::new(10, -101, 4);
    assert_eq!(sky_rgb_at(&app, below), [15; 3]);
    edit(&mut app, &[(p, g.red)]);
    settle(&mut app);
    assert!(assert_colored_matches_reference(&app) > 10);
    let tinted = sky_rgb_at(&app, below);
    assert_ne!(tinted, [15; 3]);
    assert_eq!(
        light_at(&app, below).sky(),
        15,
        "gameplay reads the uncolored beam"
    );
    let tile = app.world().resource::<SkyMap>().beam_tile(0, 0).cloned();
    assert!(tile.is_some_and(|t| t.colored_sky() && t.has_tints()));
    edit(&mut app, &[(p, g.light_blue)]);
    settle(&mut app);
    assert!(assert_colored_matches_reference(&app) > 10);
    assert_ne!(sky_rgb_at(&app, below), tinted);
    edit(&mut app, &[(p, BlockState::AIR)]);
    settle(&mut app);
    assert!(assert_colored_matches_reference(&app) > 10);
    assert_eq!(sky_rgb_at(&app, below), [15; 3]);
    assert_eq!(hashes(&app), before);
}

/// A black pane 200 voxels up (kept loaded by a second viewer) darkens the sky under it; its
/// chunk unloads (no persistence here: its edits are lost), the column is walked again without
/// the tint and the sky under it is white again.
#[test]
fn an_unloaded_pane_takes_its_tint_with_it() {
    let (registry, g) = tint_registry();
    let mut app = colored_app(GlassHouse, registry, true, true, Arc::new(InlineExecutor));
    spawn(&mut app, HOUSE, 2, 1);
    let high = spawn(&mut app, ChunkPos::new(0, 6, 0), 1, 0);
    settle(&mut app);
    let before = hashes(&app);
    let under = BlockPos::new(16, 60, 16);
    assert_eq!(sky_rgb_at(&app, under), [15; 3]);
    edit(&mut app, &pane(8, 200, 8, 17, g.black));
    settle(&mut app);
    assert_eq!(sky_rgb_at(&app, under), [6; 3]);
    assert!(assert_colored_matches_reference(&app) > 10);
    let walks = stats(&app).sky_walks;
    app.world_mut().despawn(high);
    settle(&mut app);
    let map = app.world().resource::<ChunkMap>();
    assert!(
        map.id_at(ChunkPos::new(0, 6, 0)).is_none(),
        "the pane unloaded"
    );
    assert!(stats(&app).sky_walks > walks, "walked again");
    assert_eq!(sky_rgb_at(&app, under), [15; 3]);
    assert!(assert_colored_matches_reference(&app) > 10);
    let after = hashes(&app);
    for (p, h) in &before {
        if let Some(a) = after.get(p) {
            assert_eq!(a, h, "{p:?}");
        }
    }
}
