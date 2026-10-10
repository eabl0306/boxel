//! Cave culling (ADR 0022) against what the renderer draws, not against the blocks the world
//! has: the meshes the world published, as they were built.
//!
//! A mesh shows a face of an occluding cube where the voxel in front of it did not occlude
//! when the mesh was built. A chunk with blocks but no mesh (not meshed yet, not wanted) shows
//! nothing, and a mesh built before an edit shows the old blocks. Where faces are missing, a line
//! of sight goes on into the rock of the neighbors, whose faces toward it are not meshed either,
//! and shows the far walls of caves the face links do not reach. These tests keep a copy of what
//! each published mesh was built from, cast voxel rays from the camera through it (a ray stops at
//! the first drawn face it sees from the front) and check that every chunk a ray crosses is in
//! the world's visible set:
//!
//! - a chunk with data and no mesh between the eye and meshed chunks (two viewers with a gap);
//! - the meshes of an edit batch published apart (a late member), in both orders;
//! - every step of a world streaming in, with its jobs finished in a scrambled order.
//!
//! The scene is designed: stone, a tunnel along `+x` from the eye's chunk and a closed pocket two
//! chunks past where the tunnel ends. The pocket's far wall shows through the rock only where a
//! chunk on the way is not drawn as it is. Without [`XRAY`] (a search that only opens such a
//! chunk) rays reach hidden chunks: the tests check that too, so they prove something.
use boxel_app::App;
use boxel_demo::worldgen::OverworldGenerator;
use boxel_ecs::prelude::Messages;
use boxel_math::projection::{look_to, perspective_infinite_reverse};
use boxel_math::{DVec3, Frustum, I64Vec3, Vec3};
use boxel_transform::Transform;
use boxel_voxel::{
    ALL_LINKS, BlockPos, BlockRegistry, BlockState, ChunkData, ChunkKey, ChunkPos, LocalPos,
};
use boxel_world::jobs::JobExecutor;
use boxel_world::visibility::{XRAY, eye_in_open, visible_chunks};
use boxel_world::{
    ChunkMap, ChunkMeshEvent, ChunkViewer, ChunkVisibility, DataState, EditQueue, MeshState,
    VisibilityView, VisibleSet, WorldPlugin, WorldSettings, WorldStats,
};
use boxel_worldgen::WorldGenerator;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ---------------------------------------------------------------- the scene

/// Tunnel cross-section (y and z, world voxels).
const TUNNEL: RangeInclusive<i64> = 12..=20;
/// Where the tunnel starts along x.
const TUNNEL_START: i64 = -24;
/// The closed pocket in chunk (4, 0, 0): x, then y and z.
const POCKET_X: RangeInclusive<i64> = 136..=150;
const POCKET_YZ: RangeInclusive<i64> = 4..=28;
/// The eye: in the tunnel, in chunk (0, 0, 0), looking along `+x`.
const EYE: DVec3 = DVec3::new(8.5, 16.5, 16.5);

/// Stone everywhere but the tunnel (to `end` along x) and the pocket.
struct Tunnel {
    end: i64,
}

impl WorldGenerator for Tunnel {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let stone = registry.by_name("boxel:stone").expect("stone");
        let mut data = ChunkData::uniform(stone);
        let o = key.pos.origin();
        let mut carve = |x: RangeInclusive<i64>,
                         y: &RangeInclusive<i64>,
                         z: &RangeInclusive<i64>| {
            for wy in o.y.max(*y.start())..=(o.y + 31).min(*y.end()) {
                for wz in o.z.max(*z.start())..=(o.z + 31).min(*z.end()) {
                    for wx in o.x.max(*x.start())..=(o.x + 31).min(*x.end()) {
                        let local =
                            LocalPos::new((wx - o.x) as u32, (wy - o.y) as u32, (wz - o.z) as u32);
                        data.set(local, BlockState::AIR);
                    }
                }
            }
        };
        carve(TUNNEL_START..=self.end, &TUNNEL, &TUNNEL);
        carve(POCKET_X, &POCKET_YZ, &POCKET_YZ);
        data
    }

    fn name(&self) -> &str {
        "tunnel"
    }
}

// ---------------------------------------------------------------- jobs on demand

/// Holds the world's jobs until the test runs them: all at once, the newest first or in a
/// scrambled order (so meshes are published in an order the test chooses).
#[derive(Default)]
struct Steps {
    jobs: Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
}

impl Steps {
    fn held(&self) -> usize {
        self.jobs.lock().expect("lock").len()
    }
    /// Runs held job `i` (`None`: the newest); false if none is held.
    fn run(&self, i: Option<usize>) -> bool {
        let job = {
            let mut jobs = self.jobs.lock().expect("lock");
            match i {
                Some(i) if i < jobs.len() => Some(jobs.remove(i)),
                Some(_) => None,
                None => jobs.pop(),
            }
        };
        job.map(|job| job()).is_some()
    }
    fn run_all(&self) {
        let jobs = std::mem::take(&mut *self.jobs.lock().expect("lock"));
        for job in jobs {
            job();
        }
    }
}

impl JobExecutor for Steps {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>) {
        self.jobs.lock().expect("lock").push(job);
    }
}

/// Small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}

// ---------------------------------------------------------------- what is drawn

/// Side of the padded box a mesh is built from: the chunk and one voxel of its neighbors.
const PAD: i64 = 34;

/// Per published mesh, which voxels of its padded box occluded when it was published (the mesh
/// shows a face of an occluding cube toward a voxel that did not). A mesh is published in the
/// frame its job's result is applied, and only if the chunk and its neighbors' borders did not
/// change since the job read them, so the map's blocks at the end of that frame are the ones it
/// was built from.
#[derive(Default)]
struct Drawn {
    chunks: HashMap<ChunkPos, Vec<u64>>,
}

fn pad_index(x: i64, y: i64, z: i64) -> usize {
    (((y + 1) * PAD + (z + 1)) * PAD + (x + 1)) as usize
}

impl Drawn {
    fn apply(&mut self, map: &ChunkMap, registry: &BlockRegistry, events: &[ChunkMeshEvent]) {
        for event in events {
            match event {
                ChunkMeshEvent::Upsert { key, .. } => {
                    self.chunks
                        .insert(key.pos, Self::snapshot(map, registry, key.pos));
                }
                ChunkMeshEvent::Remove { key, .. } => {
                    self.chunks.remove(&key.pos);
                }
            }
        }
    }

    fn snapshot(map: &ChunkMap, registry: &BlockRegistry, pos: ChunkPos) -> Vec<u64> {
        let mut bits = vec![0u64; (PAD * PAD * PAD) as usize / 64 + 1];
        let mut near: HashMap<ChunkPos, Option<Arc<ChunkData>>> = HashMap::new();
        let o = pos.origin();
        for y in -1..=32 {
            for z in -1..=32 {
                for x in -1..=32 {
                    let p = BlockPos::new(o.x + x, o.y + y, o.z + z);
                    let (chunk, local) = p.split();
                    let data = near
                        .entry(chunk)
                        .or_insert_with(|| map.data_at(chunk).cloned());
                    // A neighbor without data (`Missing`) is air to the mesher.
                    let occludes = data
                        .as_ref()
                        .is_some_and(|d| registry.occludes(d.get(local)));
                    if occludes {
                        let i = pad_index(x, y, z);
                        bits[i >> 6] |= 1 << (i & 63);
                    }
                }
            }
        }
        bits
    }

    /// True if the renderer draws the face of voxel `b` toward its face neighbor `a`.
    fn face(&self, a: BlockPos, b: BlockPos) -> bool {
        let chunk = ChunkPos::containing(b);
        let Some(bits) = self.chunks.get(&chunk) else {
            return false;
        };
        let o = chunk.origin();
        let occ = |p: BlockPos| {
            let i = pad_index(p.x - o.x, p.y - o.y, p.z - o.z);
            bits[i >> 6] >> (i & 63) & 1 != 0
        };
        occ(b) && !occ(a)
    }
}

/// Walks the voxels of a ray from `eye` along `dir` through what is drawn, calling `visit` with
/// the chunk of every voxel it enters, until a drawn face stops it (its chunk visited too) or
/// after `steps` voxels.
fn walk(drawn: &Drawn, eye: DVec3, dir: DVec3, steps: usize, mut visit: impl FnMut(ChunkPos)) {
    let mut cell = eye.floor().as_i64vec3();
    let step = I64Vec3::new(
        dir.x.signum() as i64,
        dir.y.signum() as i64,
        dir.z.signum() as i64,
    );
    let frac = eye - eye.floor();
    let next = |f: f64, d: f64| {
        if d > 0.0 {
            (1.0 - f) / d
        } else if d < 0.0 {
            f / -d
        } else {
            f64::INFINITY
        }
    };
    let mut t = DVec3::new(
        next(frac.x, dir.x),
        next(frac.y, dir.y),
        next(frac.z, dir.z),
    );
    let delta = DVec3::new(1.0 / dir.x.abs(), 1.0 / dir.y.abs(), 1.0 / dir.z.abs());
    visit(ChunkPos::containing(BlockPos::from_i64vec3(cell)));
    for _ in 0..steps {
        let from = BlockPos::from_i64vec3(cell);
        if t.x <= t.y && t.x <= t.z {
            cell.x += step.x;
            t.x += delta.x;
        } else if t.y <= t.z {
            cell.y += step.y;
            t.y += delta.y;
        } else {
            cell.z += step.z;
            t.z += delta.z;
        }
        let to = BlockPos::from_i64vec3(cell);
        visit(ChunkPos::containing(to));
        if drawn.face(from, to) {
            return;
        }
    }
}

// ---------------------------------------------------------------- the camera

/// A 16:9 camera with a 70 degree vertical field of view.
struct Camera {
    eye: DVec3,
    dir: DVec3,
    /// Half extents of the searched box (chunks), around and up and down.
    radius: u32,
    vertical: u32,
}

impl Camera {
    fn new(eye: DVec3, dir: DVec3, radius: u32, vertical: u32) -> Self {
        Self {
            eye,
            dir: dir.normalize(),
            radius,
            vertical,
        }
    }

    fn render_origin(&self) -> I64Vec3 {
        self.eye.floor().as_i64vec3()
    }

    fn eye_rel(&self) -> Vec3 {
        (self.eye - self.eye.floor()).as_vec3()
    }

    fn up(&self) -> DVec3 {
        if self.dir.y.abs() > 0.99 {
            DVec3::Z
        } else {
            DVec3::Y
        }
    }

    fn view(&self) -> VisibilityView {
        let eye_rel = self.eye_rel();
        let view_proj = perspective_infinite_reverse(70f32.to_radians(), 16.0 / 9.0, 0.1)
            * look_to(eye_rel, self.dir.as_vec3(), self.up().as_vec3());
        let mut f = Frustum::from_view_proj_reverse_z(view_proj);
        f.set_max_distance(eye_rel, self.dir.as_vec3(), 4096.0);
        VisibilityView::new(self.render_origin(), Some(f), self.radius, self.vertical)
    }

    /// Ray directions on a `w` x `h` grid over the image, a hair inside its edges.
    fn rays(&self, w: usize, h: usize) -> Vec<DVec3> {
        let right = self.dir.cross(self.up()).normalize();
        let up = right.cross(self.dir);
        let tan_v = (35f64).to_radians().tan() * 0.999;
        let tan_h = tan_v * 16.0 / 9.0;
        let mut out = Vec::with_capacity(w * h);
        for j in 0..h {
            for i in 0..w {
                let u = (i as f64 + 0.5) / w as f64 * 2.0 - 1.0;
                let v = (j as f64 + 0.5) / h as f64 * 2.0 - 1.0;
                out.push((self.dir + right * (u * tan_h) + up * (v * tan_v)).normalize());
            }
        }
        out
    }
}

/// The near plane's reach of the camera above (`boxel_camera::Camera::near_reach`).
fn near_reach() -> f32 {
    let tan_v = 35f32.to_radians().tan();
    let tan_h = tan_v * 16.0 / 9.0;
    0.1 * (1.0 + tan_v * tan_v + tan_h * tan_h).sqrt()
}

/// What one check found: rays cast, chunks they crossed that the world's set hides (must be
/// none), and those a set without [`XRAY`] would hide.
#[derive(Clone, Copy, Debug, Default)]
struct Found {
    rays: usize,
    culled: usize,
    lost_without_xray: usize,
    /// The eye did not see from open space: the renderer culls nothing (no check).
    blocked: bool,
}

// ---------------------------------------------------------------- the world

struct Rig {
    app: App,
    steps: Arc<Steps>,
    drawn: Drawn,
    registry: BlockRegistry,
}

impl Rig {
    fn new(
        generator: impl WorldGenerator,
        settings: WorldSettings,
        viewers: &[(DVec3, ChunkViewer)],
    ) -> Self {
        let steps = Arc::new(Steps::default());
        let mut app = App::new();
        app.add_plugins(WorldPlugin {
            generator: Arc::new(generator),
            registry: Arc::new(boxel_demo::blocks::registry()),
            settings: WorldSettings {
                max_jobs_in_flight: 4096,
                interest_budget: Duration::MAX,
                job_budget: Duration::MAX,
                ..settings
            },
            executor: steps.clone(),
        });
        for &(at, viewer) in viewers {
            app.world_mut()
                .spawn((Transform::from_translation(at), viewer));
        }
        Self {
            app,
            steps,
            drawn: Drawn::default(),
            registry: boxel_demo::blocks::registry(),
        }
    }

    /// One frame; what it published goes to the copy of what is drawn.
    fn update(&mut self) {
        self.app.update();
        let events: Vec<ChunkMeshEvent> = self
            .app
            .world_mut()
            .resource_mut::<Messages<ChunkMeshEvent>>()
            .drain()
            .collect();
        let map = self.app.world().resource::<ChunkMap>();
        self.drawn.apply(map, &self.registry, &events);
    }

    fn settled(&self) -> bool {
        let stats = self.app.world().resource::<WorldStats>();
        stats.chunks > 0 && stats.is_settled() && self.steps.held() == 0
    }

    /// Frames with every job run at once, until the world settles.
    fn settle(&mut self) {
        for _ in 0..500 {
            self.update();
            if self.settled() {
                return;
            }
            self.steps.run_all();
        }
        panic!(
            "not settled: {:?}",
            self.app.world().resource::<WorldStats>()
        );
    }

    fn slot_links(&self, pos: ChunkPos) -> (bool, u16) {
        let map = self.app.world().resource::<ChunkMap>();
        let slot = map.get(map.id_at(pos).expect("loaded")).expect("slot");
        (slot.shown(), slot.visibility_links())
    }

    /// Rays of `camera` on a `w` x `h` grid through what is drawn, against the visible set the
    /// renderer would use this frame (the world's [`ChunkVisibility`], as the render plugin
    /// calls it), a fresh search (that set once the links that closed are searched again, after
    /// `TIGHTEN_DELAY`) and a fresh search without [`XRAY`].
    fn check(&mut self, camera: &Camera, (w, h): (usize, usize)) -> Found {
        let view = camera.view();
        let world = self.app.world_mut();
        let (set, open) =
            world.resource_scope(|world, mut vis: boxel_ecs::world::Mut<ChunkVisibility>| {
                let map = world.resource::<ChunkMap>();
                let registry = &self.registry;
                let open = eye_in_open(
                    camera.render_origin(),
                    camera.eye_rel(),
                    near_reach(),
                    |p| map.block_at(p).map(|s| registry.occludes(s)),
                );
                (vis.update(&view, |p| map.visibility_links_at(p)), open)
            });
        if !open {
            return Found {
                blocked: true,
                ..Found::default()
            };
        }
        let map = self.app.world().resource::<ChunkMap>();
        let fresh: VisibleSet = visible_chunks(&view, |p| map.visibility_links_at(p));
        let without: VisibleSet = visible_chunks(&view, |p| map.visibility_links_at(p) & !XRAY);
        let mut found = Found::default();
        let mut culled: Vec<ChunkPos> = Vec::new();
        for dir in camera.rays(w, h) {
            found.rays += 1;
            let (mut lost, mut hidden) = (false, false);
            walk(&self.drawn, camera.eye, dir, 400, |chunk| {
                if !set.is_visible(chunk) || !fresh.is_visible(chunk) {
                    hidden = true;
                    if culled.len() < 8 && !culled.contains(&chunk) {
                        culled.push(chunk);
                    }
                }
                lost |= !without.is_visible(chunk);
            });
            found.culled += usize::from(hidden);
            found.lost_without_xray += usize::from(lost);
        }
        assert!(
            found.culled == 0,
            "{} of {} rays from {} cross chunks the visible set hides (some: {culled:?})",
            found.culled,
            found.rays,
            camera.eye
        );
        found
    }
}

/// The camera in the tunnel, with the box of a viewer of radius `radius` and vertical radius 1.
fn tunnel_camera(radius: u32) -> Camera {
    Camera::new(EYE, DVec3::X, radius, 1)
}

/// A viewer at the eye that meshes the tunnel and the pocket.
fn tunnel_viewer() -> (DVec3, ChunkViewer) {
    (EYE, ChunkViewer::new(5).with_vertical_radius(1))
}

// ---------------------------------------------------------------- tests

/// Two viewers four chunks apart: between their mesh regions, chunk (2, 0, 0) has data but no
/// mesh (nobody wants one). The tunnel ends inside it, so nothing stops the rays from the eye
/// there: they cross its rock and that of chunk (3, 0, 0), whose faces toward it are not meshed,
/// and show the far wall of the pocket in chunk (4, 0, 0). Opening chunk (2, 0, 0) is not enough:
/// the search enters chunk (3, 0, 0), which links nothing, and stops. (The box is that of a
/// camera whose viewer reaches chunk 4.)
#[test]
fn a_chunk_with_data_and_no_mesh_is_seen_through() {
    let viewer = ChunkViewer::new(1).with_vertical_radius(1);
    let mut rig = Rig::new(
        Tunnel { end: 80 },
        WorldSettings::default(),
        &[(EYE, viewer), (EYE + DVec3::new(128.0, 0.0, 0.0), viewer)],
    );
    rig.settle();
    {
        let map = rig.app.world().resource::<ChunkMap>();
        let gap = map
            .get(map.id_at(ChunkPos::new(2, 0, 0)).expect("loaded"))
            .expect("slot");
        assert!(matches!(gap.data(), DataState::Ready(_)));
        assert!(matches!(gap.mesh(), MeshState::None));
        for x in [0, 1, 3, 4] {
            let id = map.id_at(ChunkPos::new(x, 0, 0)).expect("loaded");
            let slot = map.get(id).expect("slot");
            assert!(
                matches!(slot.mesh(), MeshState::Ready { .. } | MeshState::Empty),
                "{x}: {:?}",
                slot.mesh()
            );
            assert!(slot.shown(), "{x}");
        }
    }
    let found = rig.check(&tunnel_camera(6), (160, 90));
    eprintln!("gap: {found:?}");
    assert_eq!(
        rig.slot_links(ChunkPos::new(2, 0, 0)),
        (false, ALL_LINKS | XRAY)
    );
    assert!(
        found.lost_without_xray > 0,
        "no ray reaches a chunk a search without XRAY hides (the test proves nothing)"
    );
}

/// The tunnel reaches the border of chunk (2, 0, 0), so chunk (3, 0, 0) draws its end wall. An
/// edit closes the tunnel's last voxel layer (in chunk 2): chunk 3 loses its end wall faces and
/// chunk 2 gets its own. Each mesh is published on its own (no batch hold), in the order the
/// test runs their jobs. Chunk 3 first: chunk 2 still shows the open tunnel, and the rays go
/// through both rocks to the pocket. Then the reverse edit (chunk 3 must show the wall again):
/// chunk 2 first shows the hole before chunk 3 has the faces. Both edits are checked after every
/// job, with the jobs run newest first and oldest first.
#[test]
fn edit_batch_members_published_apart_hide_nothing() {
    let edits: [(&str, BlockState); 2] = [
        (
            "close",
            boxel_demo::blocks::registry()
                .by_name("boxel:stone")
                .expect("stone"),
        ),
        ("open", BlockState::AIR),
    ];
    let mut lost = 0;
    for newest_first in [true, false] {
        let mut rig = Rig::new(
            Tunnel { end: 95 },
            WorldSettings {
                edit_batch_hold: Duration::ZERO,
                ..WorldSettings::default()
            },
            &[tunnel_viewer()],
        );
        rig.settle();
        rig.check(&tunnel_camera(5), (64, 36));
        for (name, state) in edits {
            {
                let mut queue = rig.app.world_mut().resource_mut::<EditQueue>();
                for y in TUNNEL {
                    for z in TUNNEL {
                        queue.set_block(BlockPos::new(95, y, z), state);
                    }
                }
            }
            rig.update();
            // Checked after the rays (which say more when they fail).
            let edited = [2, 3].map(|x| rig.slot_links(ChunkPos::new(x, 0, 0)));
            let mut step = 0;
            while !rig.settled() {
                if !rig.steps.run(if newest_first { None } else { Some(0) }) {
                    rig.update();
                    continue;
                }
                rig.update();
                let found = rig.check(&tunnel_camera(5), (160, 90));
                eprintln!("{name} newest_first={newest_first} step {step}: {found:?}");
                lost += found.lost_without_xray;
                step += 1;
                assert!(step < 100, "{name}: not settled");
            }
            assert_eq!(
                edited,
                [(false, ALL_LINKS | XRAY); 2],
                "{name}: chunks 2 and 3 right after the edit"
            );
            for x in [2, 3] {
                assert!(
                    rig.slot_links(ChunkPos::new(x, 0, 0)).0,
                    "{name}: chunk {x}"
                );
            }
        }
    }
    assert!(
        lost > 0,
        "no ray reaches a chunk a search without XRAY hides (the test proves nothing)"
    );
}

/// Streams the tunnel world in with every job run in a scrambled order, one per frame, and
/// checks the rays after every frame: chunks whose first mesh is not published yet sit between
/// the eye and chunks already drawn.
#[test]
fn streaming_in_any_order_hides_nothing() {
    let mut lost = 0;
    for seed in 1..=2u64 {
        let mut rig = Rig::new(
            Tunnel { end: 80 },
            WorldSettings::default(),
            &[tunnel_viewer()],
        );
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let (mut checks, mut frames) = (0, 0);
        while !rig.settled() {
            let held = rig.steps.held();
            if held > 0 {
                rig.steps.run(Some((rng.next() % held as u64) as usize));
            }
            frames += 1;
            assert!(frames < 20_000, "not settled");
            rig.update();
            let found = rig.check(&tunnel_camera(5), (40, 22));
            lost += found.lost_without_xray;
            checks += usize::from(!found.blocked);
        }
        eprintln!("seed {seed}: {frames} frames, {checks} checks");
    }
    eprintln!("streaming: {lost} rays lost without XRAY");
    assert!(
        lost > 0,
        "no ray reaches a chunk a search without XRAY hides (the test proves nothing)"
    );
}

/// The same in the overworld's caves: every frame of a world streaming in with its jobs run in a
/// scrambled order, from an open voxel deep in a cave, looking along the axes and up and down.
#[test]
fn streaming_overworld_caves_hides_nothing() {
    let registry = boxel_demo::blocks::registry();
    let generator = OverworldGenerator::default();
    // An open voxel with open neighbors in chunk (0, -10, 0), a cave chunk deep under the
    // surface (the benches use it).
    let chunk = ChunkPos::new(0, -10, 0);
    let data = generator.generate(ChunkKey::new(chunk), &registry);
    let open = |q: BlockPos| {
        let (c, l) = q.split();
        c == chunk && !registry.occludes(data.get(l))
    };
    let eye = (1..=30)
        .flat_map(|y| (1..=30).flat_map(move |z| (1..=30).map(move |x| (x, y, z))))
        .map(|(x, y, z)| BlockPos::new(x, -320 + y, z))
        .find(|p| {
            (-1..=1).all(|d| {
                open(BlockPos::new(p.x + d, p.y, p.z))
                    && open(BlockPos::new(p.x, p.y + d, p.z))
                    && open(BlockPos::new(p.x, p.y, p.z + d))
            })
        })
        .expect("an open voxel in the cave chunk");
    let eye = eye.as_i64vec3().as_dvec3() + DVec3::new(0.5, 0.5, 0.5);
    let viewer = ChunkViewer::new(2).with_vertical_radius(1);
    let mut rig = Rig::new(
        OverworldGenerator::default(),
        WorldSettings::default(),
        &[(eye, viewer)],
    );
    let cameras: Vec<Camera> = [
        DVec3::X,
        DVec3::NEG_X,
        DVec3::Z,
        DVec3::NEG_Z,
        DVec3::new(0.3, -1.0, 0.2),
        DVec3::new(-0.2, 1.0, 0.3),
    ]
    .into_iter()
    .map(|dir| Camera::new(eye, dir, 2, 1))
    .collect();
    let mut rng = Rng(7);
    let (mut frames, mut checks) = (0, 0);
    while !rig.settled() {
        let held = rig.steps.held();
        if held > 0 {
            rig.steps.run(Some((rng.next() % held as u64) as usize));
        }
        frames += 1;
        assert!(frames < 20_000, "not settled");
        rig.update();
        let camera = &cameras[frames % cameras.len()];
        let found = rig.check(camera, (32, 18));
        checks += usize::from(!found.blocked);
    }
    assert!(checks > 100, "too few checks: {checks}");
    eprintln!("overworld: {frames} frames, {checks} checks from {eye}");
}

/// The eye is in the tunnel, which ends where the tests say, and the pocket is closed.
#[test]
fn the_scene_is_where_the_tests_think() {
    let registry = boxel_demo::blocks::registry();
    let tunnel = Tunnel { end: 80 };
    let at = |p: BlockPos| {
        let (chunk, local) = p.split();
        tunnel.generate(ChunkKey::new(chunk), &registry).get(local)
    };
    assert_eq!(at(BlockPos::from_world(EYE)), BlockState::AIR);
    assert_eq!(at(BlockPos::new(80, 16, 16)), BlockState::AIR);
    assert_ne!(at(BlockPos::new(81, 16, 16)), BlockState::AIR);
    assert_eq!(at(BlockPos::new(140, 16, 16)), BlockState::AIR);
    assert_ne!(at(BlockPos::new(151, 16, 16)), BlockState::AIR);
}
