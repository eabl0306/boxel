//! Cave culling (ADR 0022) on real overworld chunks:
//!
//! - Soundness by ray casting: from open voxels in caves, at the surface and far from the origin
//!   (`2^40` voxels in x, `-2^40` in y, near the world limit), voxel rays inside a camera's
//!   frustum walk until they hit an occluding voxel; every chunk a ray crosses (the one it hits
//!   included) must be in the visible set, since any of them may show a face along the ray.
//! - The world keeps every settled chunk's face info equal to a fresh computation from its data,
//!   sees through an edited chunk at once and has exact links again after its remesh, and its
//!   incremental [`ChunkVisibility`] agrees with a search from scratch. (What the renderer draws
//!   while meshes are missing or stale: `cave_culling_drawn.rs`.)
#![expect(clippy::print_stderr, reason = "test progress")]
use boxel_app::App;
use boxel_math::projection::{look_to, perspective_infinite_reverse};
use boxel_math::{DVec3, Frustum, I64Vec3, Vec3};
use boxel_transform::Transform;
use boxel_voxel::{
    ALL_LINKS, BlockPos, BlockRegistry, CHUNK_SHIFT, ChunkData, ChunkFaceInfo, ChunkKey, ChunkPos,
    MAX_CHUNK_COORD,
};
use boxel_world::jobs::InlineExecutor;
use boxel_world::visibility::{TIGHTEN_DELAY, XRAY, visible_chunks};
use boxel_world::{
    ChunkMap, ChunkViewer, ChunkVisibility, DataState, EditQueue, MeshState, VisibilityView,
    WorldPlugin, WorldSettings, WorldStats,
};
use boxel_worldgen::{OverworldGenerator, WorldGenerator};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Small deterministic generator (xorshift64*).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// A random direction (uniform on the sphere).
    fn direction(&mut self) -> DVec3 {
        loop {
            let v = DVec3::new(
                self.unit() * 2.0 - 1.0,
                self.unit() * 2.0 - 1.0,
                self.unit() * 2.0 - 1.0,
            );
            let l = v.length();
            if (0.1..=1.0).contains(&l) {
                return v / l;
            }
        }
    }
}

/// Generated chunks of a box around a center, with their face info.
struct Region {
    registry: BlockRegistry,
    chunks: HashMap<ChunkPos, (ChunkData, ChunkFaceInfo)>,
}

impl Region {
    fn generate(center: ChunkPos, r: i64, v: i64) -> Self {
        let registry = BlockRegistry::default_blocks();
        let generator = OverworldGenerator::default();
        let mut chunks = HashMap::new();
        for y in -v..=v {
            for z in -r..=r {
                for x in -r..=r {
                    let pos = ChunkPos::new(center.x + x, center.y + y, center.z + z);
                    let data = generator.generate(ChunkKey::new(pos), &registry);
                    let faces = ChunkFaceInfo::compute(&data, &registry);
                    chunks.insert(pos, (data, faces));
                }
            }
        }
        Self { registry, chunks }
    }

    fn links(&self, pos: ChunkPos) -> u16 {
        self.chunks.get(&pos).map_or(ALL_LINKS, |(_, f)| f.links)
    }

    /// `None` outside the region.
    fn occludes(&self, p: BlockPos) -> Option<bool> {
        let (chunk, local) = p.split();
        let (data, _) = self.chunks.get(&chunk)?;
        Some(self.registry.occludes(data.get(local)))
    }

    /// Up to `n` open voxels of chunk `pos`.
    fn open_voxels(&self, pos: ChunkPos, n: usize, rng: &mut Rng) -> Vec<BlockPos> {
        let origin = pos.origin();
        let mut out = Vec::new();
        for _ in 0..20_000 {
            let p = BlockPos::new(
                origin.x + (rng.next() % 32) as i64,
                origin.y + (rng.next() % 32) as i64,
                origin.z + (rng.next() % 32) as i64,
            );
            if self.occludes(p) == Some(false) {
                out.push(p);
                if out.len() == n {
                    break;
                }
            }
        }
        out
    }
}

/// A camera at `eye_rel` (relative to the render origin) looking along `dir`.
fn frustum(eye_rel: Vec3, dir: Vec3) -> Frustum {
    let up = if dir.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let view_proj = perspective_infinite_reverse(70f32.to_radians(), 16.0 / 9.0, 0.1)
        * look_to(eye_rel, dir, up);
    let mut f = Frustum::from_view_proj_reverse_z(view_proj);
    f.set_max_distance(eye_rel, dir, 4096.0);
    f
}

fn inside(f: &Frustum, p: Vec3) -> bool {
    f.planes.iter().all(|pl| pl.truncate().dot(p) + pl.w >= 0.0)
}

/// Casts voxel rays (DDA) from `eye` (world, relative to `origin` for precision) along `dir`
/// until an occluding voxel or the region's end; calls `visit` with every chunk crossed.
fn cast(
    region: &Region,
    origin: I64Vec3,
    eye_rel: DVec3,
    dir: DVec3,
    mut visit: impl FnMut(ChunkPos),
) {
    let mut cell = origin + eye_rel.floor().as_i64vec3();
    let step = I64Vec3::new(
        dir.x.signum() as i64,
        dir.y.signum() as i64,
        dir.z.signum() as i64,
    );
    let frac = eye_rel - eye_rel.floor();
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
    for _ in 0..2000 {
        let p = BlockPos::from_i64vec3(cell);
        match region.occludes(p) {
            None => return,
            Some(occludes) => {
                visit(ChunkPos::containing(p));
                if occludes {
                    return;
                }
            }
        }
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
    }
}

/// Every chunk a ray inside the frustum crosses is visible, from `eyes` open voxels of
/// `center`'s chunk, looking in random directions.
fn assert_rays_see_visible_chunks(center: ChunkPos, seed: u64) {
    let (r, v) = (3, 2);
    let region = Region::generate(center, r, v);
    let mut rng = Rng(seed | 1);
    let eyes = region.open_voxels(center, 4, &mut rng);
    assert!(!eyes.is_empty(), "no open voxel in {center:?}");
    let mut checked = 0;
    let mut hidden_boxes = 0;
    for eye in eyes {
        for _ in 0..3 {
            let jitter = DVec3::new(rng.unit(), rng.unit(), rng.unit()) * 0.98 + 0.01;
            let render_origin = eye.as_i64vec3();
            let eye_rel = jitter;
            let look = rng.direction();
            let f = frustum(eye_rel.as_vec3(), look.as_vec3());
            let view = VisibilityView::new(render_origin, Some(f), r as u32, v as u32);
            let set = visible_chunks(&view, |p| region.links(p));
            hidden_boxes += set.box_len() - set.visible_count();
            for _ in 0..400 {
                let dir = rng.direction();
                if !inside(&f, (eye_rel + dir * 8.0).as_vec3()) {
                    continue;
                }
                cast(&region, render_origin, eye_rel, dir, |chunk| {
                    assert!(
                        set.is_visible(chunk),
                        "{chunk:?} is crossed by a ray from {eye:?} (+{eye_rel}) along {dir} \
                         but hidden"
                    );
                    checked += 1;
                });
            }
        }
    }
    assert!(checked > 1000, "too few ray steps ({checked})");
    eprintln!("{center:?}: {checked} ray steps, {hidden_boxes} hidden box chunks");
}

#[test]
fn rays_in_caves_only_cross_visible_chunks() {
    // Chunk (0, -10, 0): a cave chunk with about a quarter of air (benches).
    assert_rays_see_visible_chunks(ChunkPos::new(0, -10, 0), 1);
    assert_rays_see_visible_chunks(ChunkPos::new(3, -6, -2), 2);
}

#[test]
fn rays_at_the_surface_only_cross_visible_chunks() {
    let generator = OverworldGenerator::default();
    for (i, (x, z)) in [(0i64, 0i64), (40, -12), (-25, 31)].into_iter().enumerate() {
        let h = generator.height_at(x * 32 + 16, z * 32 + 16);
        // The chunk just under the surface: ground, sea, trees and cave entrances.
        let center = ChunkPos::new(x, (h - 2) >> CHUNK_SHIFT, z);
        assert_rays_see_visible_chunks(center, 10 + i as u64);
    }
}

#[test]
fn rays_far_from_the_origin_only_cross_visible_chunks() {
    for (i, center) in [
        ChunkPos::new(1 << 35, -10, 7),
        ChunkPos::new(-3, -(1 << 35), 1 << 35),
        ChunkPos::new(
            MAX_CHUNK_COORD - 2,
            -(MAX_CHUNK_COORD - 2),
            MAX_CHUNK_COORD - 2,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        assert_rays_see_visible_chunks(center, 20 + i as u64);
    }
}

// ---------------------------------------------------------------- the world

fn overworld_app() -> App {
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(OverworldGenerator::default()),
        registry: Arc::new(BlockRegistry::default_blocks()),
        settings: WorldSettings::default(),
        executor: Arc::new(InlineExecutor),
    });
    app
}

fn settle(app: &mut App) {
    let start = Instant::now();
    loop {
        app.update();
        let stats = *app.world().resource::<WorldStats>();
        if stats.chunks > 0 && stats.is_settled() {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "not settled: {stats:?}"
        );
    }
}

/// In a settled world every chunk with a settled mesh is shown, and every shown chunk's links
/// are its data's; a chunk with data the renderer does not show (no mesh: not wanted) is seen
/// through; others are open.
fn assert_links_match_data(app: &App) -> usize {
    let map = app.world().resource::<ChunkMap>();
    let registry = BlockRegistry::default_blocks();
    let mut exact = 0;
    for (_, slot) in map.iter() {
        let settled = matches!(slot.mesh(), MeshState::Ready { .. } | MeshState::Empty);
        assert_eq!(slot.shown(), settled, "{:?}", slot.pos);
        match (slot.shown(), slot.data()) {
            (true, DataState::Ready(data)) => {
                let want = ChunkFaceInfo::compute(data, &registry);
                assert_eq!(slot.face_links(), Some(want.links), "{:?}", slot.pos);
                assert_eq!(slot.visibility_links(), want.links, "{:?}", slot.pos);
                assert_eq!(map.visibility_links_at(slot.pos), want.links);
                exact += 1;
            }
            (false, DataState::Ready(_)) => {
                assert_eq!(slot.visibility_links(), ALL_LINKS | XRAY, "{:?}", slot.pos);
            }
            _ => assert_eq!(slot.visibility_links(), ALL_LINKS, "{:?}", slot.pos),
        }
    }
    exact
}

/// The world's incremental visibility holds every chunk of a fresh search over the map, and
/// equals it once links that closed had time to be searched again.
fn assert_visibility_matches(app: &mut App, view: &VisibilityView) {
    let world = app.world_mut();
    let fresh = {
        let map = world.resource::<ChunkMap>();
        visible_chunks(view, |p| map.visibility_links_at(p))
    };
    world.resource_scope(|world, mut vis: boxel_ecs::world::Mut<ChunkVisibility>| {
        let map = world.resource::<ChunkMap>();
        let got = vis.update(view, |p| map.visibility_links_at(p));
        let (min, max) = fresh.bounds();
        for y in min.y..=max.y {
            for z in min.z..=max.z {
                for x in min.x..=max.x {
                    let p = ChunkPos::new(x, y, z);
                    assert!(!fresh.is_visible(p) || got.is_visible(p), "{p:?}");
                }
            }
        }
        // Wait out the delay of the links that closed (no frame to block here).
        let until = Instant::now() + TIGHTEN_DELAY;
        while Instant::now() < until {
            std::thread::yield_now();
        }
        let got = vis.update(view, |p| map.visibility_links_at(p));
        assert_eq!(*got, fresh);
    });
}

#[test]
fn the_world_keeps_face_info_and_visibility_up_to_date() {
    let mut app = overworld_app();
    // A viewer deep in the caves, a small view.
    let eye = DVec3::new(16.5, -310.25, 16.5);
    app.world_mut().spawn((
        Transform::from_translation(eye),
        ChunkViewer::new(2).with_vertical_radius(2),
    ));
    settle(&mut app);
    assert!(assert_links_match_data(&app) > 50);
    let render_origin = eye.floor().as_i64vec3();
    let view = VisibilityView::new(render_origin, None, 2, 2);
    assert_visibility_matches(&mut app, &view);
    let before = {
        let map = app.world().resource::<ChunkMap>();
        visible_chunks(&view, |p| map.visibility_links_at(p))
    };

    // Fill the viewer's chunk with stone except the eye: what occludes changes, so the search
    // sees through it until its remesh lands.
    let center = view.center;
    let origin = center.origin();
    {
        let mut queue = app.world_mut().resource_mut::<EditQueue>();
        let stone = BlockRegistry::default_blocks()
            .by_name("boxel:stone")
            .expect("stone");
        for y in 0..32 {
            for z in 0..32 {
                for x in 0..32 {
                    let p = BlockPos::new(origin.x + x, origin.y + y, origin.z + z);
                    if p != BlockPos::from_world(eye) {
                        queue.set_block(p, stone);
                    }
                }
            }
        }
    }
    app.update();
    {
        let map = app.world().resource::<ChunkMap>();
        let slot = map.get(map.id_at(center).expect("loaded")).expect("slot");
        assert_eq!(slot.face_links(), None);
        assert!(!slot.shown());
        assert_eq!(slot.visibility_links(), ALL_LINKS | XRAY);
    }
    assert_visibility_matches(&mut app, &view);
    settle(&mut app);
    assert_links_match_data(&app);
    {
        let map = app.world().resource::<ChunkMap>();
        let slot = map.get(map.id_at(center).expect("loaded")).expect("slot");
        // One open voxel inside: no link.
        assert_eq!(slot.visibility_links(), 0);
    }
    assert_visibility_matches(&mut app, &view);
    // The camera's chunk still sees its neighbors; the set did not grow.
    let after = {
        let map = app.world().resource::<ChunkMap>();
        visible_chunks(&view, |p| map.visibility_links_at(p))
    };
    assert!(after.visible_count() <= before.visible_count());
}
