//! The search against a brute force over (chunk, entry face) states on small random worlds,
//! near and far from the origin, with and without a frustum; the incremental
//! [`ChunkVisibility`] against a search from scratch.
use super::*;
use boxel_math::DVec3;
use boxel_math::projection::{look_to, perspective_infinite_reverse};
use boxel_math::{IVec3, Mat4};
use boxel_voxel::{ALL_LINKS, MAX_CHUNK_COORD};
use proptest::prelude::*;
use std::collections::HashSet;
use std::time::Instant;

/// Small deterministic hash of a position relative to the center and a seed: the random links
/// of a test world, the same at any shift of the world.
fn hash(seed: u64, d: IVec3) -> u64 {
    let mut h = seed ^ 0x9e37_79b9_7f4a_7c15;
    for v in [d.x, d.y, d.z] {
        h = (h ^ v as u64).wrapping_mul(0xff51_afd7_ed55_8ccd);
        h ^= h >> 33;
    }
    h
}

/// Random links: each pair present with probability `p_percent`, some chunks fully open or
/// closed.
fn random_links(seed: u64, d: IVec3, p_percent: u64) -> u16 {
    let h = hash(seed, d);
    match h % 16 {
        0 => ALL_LINKS,
        1 => 0,
        _ => {
            let mut links = 0;
            for bit in 0..15 {
                if hash(h, IVec3::new(bit, 0, 0)) % 100 < p_percent {
                    links |= 1 << bit;
                }
            }
            links
        }
    }
}

/// A camera at `eye` (relative to the render origin) looking along `dir`.
fn frustum(eye: Vec3, dir: Vec3, distance: f32) -> Frustum {
    let up = if dir.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let view_proj: Mat4 = perspective_infinite_reverse(70f32.to_radians(), 16.0 / 9.0, 0.1)
        * look_to(eye, dir.normalize(), up);
    let mut f = Frustum::from_view_proj_reverse_z(view_proj);
    f.set_max_distance(eye, dir.normalize(), distance);
    f
}

/// The visible chunks by brute force: a depth-first search over (chunk, entry face, seen
/// through) states with the same rules (links, never back toward the camera along an axis, the
/// frustum outside the 27 chunks around the camera, every face away from the camera out of a
/// chunk seen through or entered from one), as relative offsets.
fn brute_force(view: &VisibilityView, links: &impl Fn(ChunkPos) -> u16) -> HashSet<IVec3> {
    let half = view.half();
    let c = view.center;
    let in_box = |d: IVec3| {
        i64::from(d.x).abs() <= half[0]
            && i64::from(d.y).abs() <= half[1]
            && i64::from(d.z).abs() <= half[2]
    };
    let pos = |d: IVec3| {
        ChunkPos::new(
            c.x + i64::from(d.x),
            c.y + i64::from(d.y),
            c.z + i64::from(d.z),
        )
    };
    // The search's own chunk-plane test (checked against the renderer's in
    // `the_frustum_test_never_rejects_what_the_renderer_draws`).
    let min = ChunkPos::new(c.x - half[0], c.y - half[1], c.z - half[2]);
    let planes = chunk_planes(view, min, SEARCH_SLACK);
    let passes = |d: IVec3| {
        if d.abs().max_element() <= 1 {
            return true;
        }
        let Some(planes) = &planes else {
            return true;
        };
        let l = Vec3::new(
            (i64::from(d.x) + half[0]) as f32,
            (i64::from(d.y) + half[1]) as f32,
            (i64::from(d.z) + half[2]) as f32,
        );
        touches(planes, l)
    };
    let mut visible = HashSet::from([IVec3::ZERO]);
    let mut seen = HashSet::new();
    let mut stack = Vec::new();
    let start_xray = links(pos(IVec3::ZERO)) & XRAY != 0;
    for f in Face::ALL {
        let n = f.normal();
        if in_box(n) && passes(n) {
            stack.push((n, f.opposite(), start_xray));
        }
    }
    while let Some((d, entry, xray)) = stack.pop() {
        if !seen.insert((d, entry, xray)) {
            continue;
        }
        visible.insert(d);
        let l = links(pos(d));
        let xray = xray || l & XRAY != 0;
        for f in Face::ALL {
            let n = f.normal();
            // Away from the camera (or sideways at its coordinate) on the step's axis.
            let along = n.dot(d);
            let axis_offset = (n * d).abs().max_element();
            if along < 0 && axis_offset != 0 {
                continue;
            }
            if !xray && l & link_bit(entry, f) == 0 {
                continue;
            }
            let next = d + n;
            if in_box(next) && passes(next) {
                stack.push((next, f.opposite(), xray));
            }
        }
    }
    visible
}

fn link_bit(a: Face, b: Face) -> u16 {
    boxel_voxel::link_bit(a, b)
}

/// The search's set as relative offsets (the box only).
fn offsets(set: &VisibleSet, view: &VisibilityView) -> HashSet<IVec3> {
    let half = view.half();
    let mut out = HashSet::new();
    for dy in -half[1]..=half[1] {
        for dz in -half[2]..=half[2] {
            for dx in -half[0]..=half[0] {
                let c = view.center;
                if set.is_visible(ChunkPos::new(c.x + dx, c.y + dy, c.z + dz)) {
                    out.insert(IVec3::new(dx as i32, dy as i32, dz as i32));
                }
            }
        }
    }
    assert_eq!(out.len(), set.visible_count());
    out
}

/// A view centered on chunk `center` with the eye at `(16.5, 20.25, 16.5)` voxels inside it.
fn view_at(center: ChunkPos, dir: Option<Vec3>, radius: u32, vertical: u32) -> VisibilityView {
    let render_origin = (center.as_i64vec3() << CHUNK_SHIFT) + I64Vec3::new(16, 20, 16);
    let eye = Vec3::new(0.5, 0.25, 0.5);
    let frustum = dir.map(|d| frustum(eye, d, 32.0 * radius as f32));
    let view = VisibilityView::new(render_origin, frustum, radius, vertical);
    assert_eq!(view.center, center);
    view
}

/// Shifts where the world is moved to: the former `i32` edge, `2^40` voxels, and the world
/// limit (`MAX_CHUNK_COORD` chunks) on every axis, with the box crossing it.
fn shifts() -> Vec<ChunkPos> {
    let m = MAX_CHUNK_COORD;
    vec![
        ChunkPos::new(0, 0, 0),
        ChunkPos::new((1 << 26) - 1, -(1 << 26), 1 << 26),
        ChunkPos::new(-(1 << 35), 1 << 35, (1 << 35) + 3),
        ChunkPos::new(m, -m, m - 1),
        ChunkPos::new(-m + 2, m, -m),
    ]
}

#[test]
fn an_open_world_is_all_visible_and_a_closed_one_only_the_neighbors() {
    let view = view_at(ChunkPos::new(3, -2, 7), None, 4, 2);
    let open = visible_chunks(&view, |_| ALL_LINKS);
    assert_eq!(open.visible_count(), 9 * 5 * 9);
    let closed = visible_chunks(&view, |_| 0);
    let got = offsets(&closed, &view);
    let mut want: HashSet<IVec3> = Face::ALL.iter().map(|f| f.normal()).collect();
    want.insert(IVec3::ZERO);
    assert_eq!(got, want);
    // Outside the box: visible.
    assert!(closed.is_visible(ChunkPos::new(3 + 5, -2, 7)));
    assert!(closed.is_visible(ChunkPos::new(3, -2 + 3, 7)));
    assert!(!closed.is_visible(ChunkPos::new(3 + 4, -2, 7)));
    assert!(!closed.covers(ChunkPos::new(3 - 5, -2, 7)));
}

#[test]
fn a_straight_tunnel_is_seen_along_its_axis_only() {
    // Only +X/-X links: from the camera's chunk the row along x is visible, nothing else past
    // the 6 neighbors.
    let view = view_at(ChunkPos::new(0, 0, 0), None, 5, 3);
    let tunnel = link_bit(Face::PosX, Face::NegX);
    let set = visible_chunks(&view, |_| tunnel);
    let got = offsets(&set, &view);
    let mut want: HashSet<IVec3> = (-5..=5).map(|x| IVec3::new(x, 0, 0)).collect();
    want.extend(Face::ALL.iter().map(|f| f.normal()));
    assert_eq!(got, want);
    // An L bend (enter -X, leave +Z) is followed; a U turn back toward the camera is not.
    let bend = |p: ChunkPos| match (p.x, p.z) {
        (1..=2, 0) => tunnel,
        (3, 0) => link_bit(Face::NegX, Face::PosZ),
        (3, 1..=5) => link_bit(Face::NegZ, Face::PosZ) | link_bit(Face::NegZ, Face::NegX),
        _ => 0,
    };
    let got = offsets(&visible_chunks(&view, bend), &view);
    for z in 1..=5 {
        assert!(got.contains(&IVec3::new(3, 0, z)), "bend at z = {z}");
        assert!(
            !got.contains(&IVec3::new(2, 0, z)),
            "no turn back toward x = 0"
        );
    }
    assert_eq!(got, brute_force(&view, &bend));
}

#[test]
fn search_matches_brute_force_far_from_the_origin() {
    let mut rng_seed = 1u64;
    for &shift in &shifts() {
        for dir in [
            None,
            Some(Vec3::new(1.0, -0.3, 0.2)),
            Some(Vec3::new(-0.2, 0.9, -0.4)),
            Some(Vec3::new(0.0, -1.0, 0.0)),
        ] {
            for p in [35, 55, 75] {
                rng_seed += 1;
                let seed = rng_seed;
                let view = view_at(shift, dir, 5, 3);
                let c = view.center;
                let links = move |pos: ChunkPos| {
                    let d = IVec3::new(
                        (pos.x - c.x) as i32,
                        (pos.y - c.y) as i32,
                        (pos.z - c.z) as i32,
                    );
                    random_links(seed, d, p)
                };
                let set = visible_chunks(&view, links);
                let got = offsets(&set, &view);
                assert_eq!(
                    got,
                    brute_force(&view, &links),
                    "shift {shift:?} dir {dir:?}"
                );
                // The same world near the origin gives the same set.
                let near = view_at(ChunkPos::new(0, 0, 0), dir, 5, 3);
                let near_links = move |pos: ChunkPos| {
                    random_links(
                        seed,
                        IVec3::new(pos.x as i32, pos.y as i32, pos.z as i32),
                        p,
                    )
                };
                let near_set = visible_chunks(&near, near_links);
                assert_eq!(offsets(&near_set, &near), got, "shift {shift:?}");
            }
        }
    }
}

#[test]
fn the_frustum_only_removes_chunks() {
    let view = view_at(ChunkPos::new(0, 0, 0), None, 6, 3);
    let links = |p: ChunkPos| random_links(7, IVec3::new(p.x as i32, p.y as i32, p.z as i32), 60);
    let all = offsets(&visible_chunks(&view, links), &view);
    let ahead = view_at(
        ChunkPos::new(0, 0, 0),
        Some(Vec3::new(0.3, -0.1, -1.0)),
        6,
        3,
    );
    let seen = offsets(&visible_chunks(&ahead, links), &ahead);
    assert!(seen.is_subset(&all));
    assert!(seen.len() < all.len());
    // Nothing behind the camera past the chunks around it.
    assert!(seen.iter().all(|d| d.z <= 1));
}

#[test]
fn the_frustum_test_never_rejects_what_the_renderer_draws() {
    // The renderer's test: chunk AABB relative to the render origin (i64 first), p-vertex.
    let mut seed = 7u64;
    let mut checked = 0;
    for &shift in &shifts() {
        for k in 0..40 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let a = (seed >> 11) as f32 / (1u64 << 53) as f32 * std::f32::consts::TAU;
            let b = ((seed >> 20) % 1000) as f32 / 1000.0 * 2.8 - 1.4;
            let dir = Vec3::new(a.cos() * b.cos(), b.sin(), a.sin() * b.cos());
            let view = view_at(shift, Some(dir), 6 + k % 3, 4);
            let half = view.half();
            let c = view.center;
            let min = ChunkPos::new(c.x - half[0], c.y - half[1], c.z - half[2]);
            // Only the slack against rounding: the search never uses less.
            let planes = chunk_planes(&view, min, FRUSTUM_SLACK).expect("frustum");
            let f = view.frustum.expect("frustum");
            for y in 0..2 * half[1] + 1 {
                for z in 0..2 * half[2] + 1 {
                    for x in 0..2 * half[0] + 1 {
                        let pos = ChunkPos::new(min.x + x, min.y + y, min.z + z);
                        let lo = ((pos.as_i64vec3() << CHUNK_SHIFT) - view.render_origin).as_vec3();
                        let aabb = boxel_math::Aabb::new(lo, lo + Vec3::splat(32.0));
                        let drawn = f.intersects_aabb(&aabb);
                        let l = Vec3::new(x as f32, y as f32, z as f32);
                        assert!(!drawn || touches(&planes, l), "{pos:?} from {view:?}");
                        checked += usize::from(drawn);
                    }
                }
            }
        }
    }
    assert!(checked > 10_000);
}

#[test]
fn incremental_updates_match_a_fresh_search() {
    let mut vis = ChunkVisibility::default();
    let mut world: std::collections::HashMap<ChunkPos, u16> = std::collections::HashMap::new();
    let links_of = |world: &std::collections::HashMap<ChunkPos, u16>, p: ChunkPos| {
        world.get(&p).copied().unwrap_or(ALL_LINKS)
    };
    let mut seed = 99u64;
    let mut center = ChunkPos::new(10, -3, -4);
    // Past the tightening delay at every step: every change is searched at once.
    let mut now = Instant::now();
    for step in 0..60 {
        now += TIGHTEN_DELAY * 2;
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        // Move the camera by up to 2 chunks on each axis, sometimes jump.
        let jump = step % 17 == 16;
        let d = |k: u32| ((seed >> k) % 5) as i64 - 2;
        center = if jump {
            ChunkPos::new(center.x + 40, center.y - 9, center.z)
        } else {
            ChunkPos::new(center.x + d(3), center.y + d(9), center.z + d(15))
        };
        // Change a few links around it, and tell the visibility.
        let mut changes = Vec::new();
        for k in 0..6u64 {
            let h = hash(seed, IVec3::new(k as i32, 0, 0));
            let p = ChunkPos::new(
                center.x + (h % 9) as i64 - 4,
                center.y + (h >> 8) as i64 % 5 - 2,
                center.z + (h >> 16) as i64 % 9 - 4,
            );
            let l = random_links(h, IVec3::ZERO, 50);
            world.insert(p, l);
            changes.push((p, l));
        }
        vis.apply_changes_at(&changes, now);
        let dir = (step % 3 != 0).then(|| Vec3::new(d(5) as f32, d(11) as f32 + 0.5, 1.0));
        let view = view_at(center, dir, 4, 2);
        let got = vis.update_at(&view, |p| links_of(&world, p), now);
        let want = visible_chunks(&view, |p| links_of(&world, p));
        assert_eq!(*got, want, "step {step}");
        // Asking again without changes reuses the result.
        let again = vis.update_at(&view, |p| links_of(&world, p), now);
        assert!(Arc::ptr_eq(&got, &again));
    }
    assert!(vis.stats().reused >= 60);
}

/// A view with the eye at `eye` (world voxels) looking along `dir`.
fn view_from(eye: DVec3, dir: Vec3, radius: u32, vertical: u32) -> VisibilityView {
    let render_origin = eye.floor().as_i64vec3();
    let eye_rel = (eye - render_origin.as_dvec3()).as_vec3();
    let f = frustum(eye_rel, dir, 32.0 * radius as f32);
    VisibilityView::new(render_origin, Some(f), radius, vertical)
}

/// `a` holds every chunk of `b` (the box is the same).
fn covers(a: &VisibleSet, b: &VisibleSet) -> bool {
    let (min, max) = b.bounds();
    (min.y..=max.y).all(|y| {
        (min.z..=max.z).all(|z| {
            (min.x..=max.x).all(|x| {
                let p = ChunkPos::new(x, y, z);
                !b.is_visible(p) || a.is_visible(p)
            })
        })
    })
}

#[test]
fn small_moves_reuse_the_set_and_it_still_covers_the_view() {
    let links = |p: ChunkPos| random_links(5, IVec3::new(p.x as i32, p.y as i32, p.z as i32), 55);
    let now = Instant::now();
    let mut seed = 3u64;
    for dir in [
        Vec3::new(1.0, -0.35, 0.1),
        Vec3::new(-0.3, 0.2, -1.0),
        Vec3::new(0.05, -1.0, 0.1),
    ] {
        let start = DVec3::new(16.25, 20.5, 16.75);
        let mut vis = ChunkVisibility::default();
        vis.update_at(&view_from(start, dir, 6, 3), links, now);
        let searches = vis.stats().searches;
        for _ in 0..200 {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let r = |k: u32| ((seed >> k) % 1000) as f64 / 1000.0 - 0.5;
            // Within 0.9 x MOVE_SLACK of the start, inside its chunk.
            let step = DVec3::new(r(3), r(13), r(23)).normalize_or_zero()
                * f64::from(MOVE_SLACK)
                * 0.9
                * (r(33) + 0.5);
            let eye = start + step;
            if ChunkPos::containing(BlockPos::from_world(eye))
                != ChunkPos::containing(BlockPos::from_world(start))
            {
                continue;
            }
            let view = view_from(eye, dir, 6, 3);
            let got = vis.update_at(&view, links, now);
            // What the renderer needs for this view: a search with no room to move.
            let exact = visible_chunks_with_slack(&view, links, FRUSTUM_SLACK);
            assert!(covers(&got, &exact), "eye {eye}");
        }
        assert_eq!(
            vis.stats().searches,
            searches,
            "every small move reused the set"
        );
        // Farther than the slack, or turned: a new search.
        vis.update_at(&view_from(start + DVec3::X * 8.0, dir, 6, 3), links, now);
        assert_eq!(vis.stats().searches, searches + 1);
        vis.update_at(&view_from(start, dir + Vec3::Y * 0.01, 6, 3), links, now);
        assert_eq!(vis.stats().searches, searches + 2);
    }
}

#[test]
fn opened_links_search_at_once_and_closed_ones_wait() {
    let mut world: std::collections::HashMap<ChunkPos, u16> = std::collections::HashMap::new();
    let view = view_at(
        ChunkPos::new(0, 0, 0),
        Some(Vec3::new(0.0, 0.0, -1.0)),
        4,
        2,
    );
    let mut vis = ChunkVisibility::default();
    let now = Instant::now();
    // Closed rock, with an open chunk between the camera and the wall.
    world.insert(ChunkPos::new(0, 0, -1), ALL_LINKS);
    let lookup =
        |w: &std::collections::HashMap<ChunkPos, u16>, p: ChunkPos| w.get(&p).copied().unwrap_or(0);
    vis.update_at(&view, |p| lookup(&world, p), now);
    let searches = vis.stats().searches;
    // A wall chunk ahead opens: searched at once, and the chunk behind it shows.
    let wall = ChunkPos::new(0, 0, -2);
    let behind = ChunkPos::new(0, 0, -3);
    assert!(!vis.last().expect("set").is_visible(behind));
    world.insert(wall, ALL_LINKS);
    vis.apply_changes_at(&[(wall, ALL_LINKS)], now);
    let set = vis.update_at(&view, |p| lookup(&world, p), now);
    assert_eq!(vis.stats().searches, searches + 1);
    assert!(set.is_visible(behind));
    // It closes again: the open set is kept until the delay passes.
    world.insert(wall, 0);
    vis.apply_changes_at(&[(wall, 0)], now);
    let kept = vis.update_at(&view, |p| lookup(&world, p), now + TIGHTEN_DELAY / 2);
    assert!(Arc::ptr_eq(&kept, &set));
    let later = vis.update_at(&view, |p| lookup(&world, p), now + TIGHTEN_DELAY);
    assert_eq!(vis.stats().searches, searches + 2);
    assert!(!later.is_visible(behind));
}

#[test]
fn the_eye_is_in_open_space_only_with_every_voxel_near_it_open() {
    let origin = I64Vec3::new(-(1 << 40) + 16, 70, (1 << 35) - 3);
    let eye = Vec3::new(0.5, 0.5, 0.5);
    let stone = |rock: BlockPos| move |p: BlockPos| Some(p == rock);
    let at = |d: I64Vec3| BlockPos::from_i64vec3(origin + d);
    // Open all around, the default camera's reach (about 0.17).
    assert!(eye_in_open(origin, eye, 0.17, |_| Some(false)));
    // The eye inside a stone voxel.
    assert!(!eye_in_open(origin, eye, 0.17, stone(at(I64Vec3::ZERO))));
    // A wall 0.1 away, inside the reach: its face is cut by the near plane.
    let near_wall = Vec3::new(0.9, 0.5, 0.5);
    assert!(!eye_in_open(origin, near_wall, 0.17, stone(at(I64Vec3::X))));
    // The same wall 0.3 away, outside it; and a diagonal neighbor out of reach.
    assert!(eye_in_open(
        origin,
        Vec3::new(0.7, 0.5, 0.5),
        0.17,
        stone(at(I64Vec3::X))
    ));
    assert!(eye_in_open(
        origin,
        near_wall,
        0.17,
        stone(at(I64Vec3::new(1, 1, 0)))
    ));
    // Near a corner, all 8 voxels around it count.
    let corner = Vec3::new(0.95, 0.03, 0.97);
    assert!(!eye_in_open(
        origin,
        corner,
        0.17,
        stone(at(I64Vec3::new(1, -1, 1)))
    ));
    // An unknown voxel, an unbounded or invalid reach: no.
    assert!(!eye_in_open(origin, eye, 0.17, |p| (p
        != at(I64Vec3::ZERO))
    .then_some(false)));
    assert!(!eye_in_open(origin, eye, MAX_EYE_REACH, |_| Some(false)));
    assert!(!eye_in_open(origin, eye, f32::NAN, |_| Some(false)));
    // The world's state counts the frames it culls nothing in.
    let mut vis = ChunkVisibility::default();
    assert!(vis.eye_in_open(origin, eye, 0.17, |_| Some(false)));
    assert!(!vis.eye_in_open(origin, eye, 0.17, stone(at(I64Vec3::ZERO))));
    assert_eq!(vis.stats().blocked, 1);
}

#[test]
fn chunks_seen_through_show_every_chunk_past_them() {
    // Closed rock with an open chunk ahead of the camera and, past it, a chunk the renderer
    // does not show.
    let view = view_at(ChunkPos::new(0, 0, 0), None, 4, 2);
    let open = ChunkPos::new(0, 0, -1);
    let unshown = ChunkPos::new(0, 0, -2);
    let links = |xray: u16| {
        move |p: ChunkPos| {
            if p == open {
                ALL_LINKS
            } else if p == unshown {
                ALL_LINKS | xray
            } else {
                0
            }
        }
    };
    let plain = visible_chunks(&view, links(0));
    let seen = visible_chunks(&view, links(XRAY));
    // Open: only its neighbors past it.
    assert!(plain.is_visible(ChunkPos::new(0, 0, -3)));
    assert!(!plain.is_visible(ChunkPos::new(0, 0, -4)));
    assert!(!plain.is_visible(ChunkPos::new(1, 0, -3)));
    // Seen through: every chunk past it (z <= -2), and nothing more nearer the camera.
    let (min, max) = seen.bounds();
    for y in min.y..=max.y {
        for z in min.z..=max.z {
            for x in min.x..=max.x {
                let p = ChunkPos::new(x, y, z);
                let want = if z <= -2 { true } else { plain.is_visible(p) };
                assert_eq!(seen.is_visible(p), want, "{p:?}");
            }
        }
    }
}

#[test]
fn unshown_chunks_are_seen_through_until_shown_again() {
    let center = ChunkPos::new(3, -40, 9);
    let at = |d: i64| ChunkPos::new(center.x, center.y, center.z + d);
    let view = view_at(center, Some(Vec3::new(0.0, 0.0, -1.0)), 4, 2);
    // Closed rock, with an open chunk between the camera and a wall.
    let (wall, behind, far) = (at(-2), at(-3), at(-4));
    let lookup = |p: ChunkPos| if p == at(-1) { ALL_LINKS } else { 0 };
    let now = Instant::now();
    // A chunk refused before the grid held it counts from the first search.
    let mut vis = ChunkVisibility::default();
    vis.set_unshown_at(wall, true, 0, now);
    assert!(vis.update_at(&view, lookup, now).is_visible(far));
    let mut vis = ChunkVisibility::default();
    let set = vis.update_at(&view, lookup, now);
    assert!(!set.is_visible(behind));
    let searches = vis.stats().searches;
    // The renderer refuses the wall chunk: searched at once, everything past it shows.
    vis.set_unshown_at(wall, true, 0, now);
    let set = vis.update_at(&view, lookup, now);
    assert_eq!(vis.stats().searches, searches + 1);
    assert!(set.is_visible(behind) && set.is_visible(far));
    // The map's changes do not undo it.
    vis.apply_changes_at(&[(wall, 0)], now);
    let kept = vis.update_at(&view, lookup, now + TIGHTEN_DELAY);
    assert!(Arc::ptr_eq(&kept, &set));
    // Shown again: its links count again, as a link that closed.
    vis.set_unshown_at(wall, false, 0, now);
    let kept = vis.update_at(&view, lookup, now + TIGHTEN_DELAY / 2);
    assert!(Arc::ptr_eq(&kept, &set));
    let later = vis.update_at(&view, lookup, now + TIGHTEN_DELAY);
    assert!(!later.is_visible(behind));
    // The blocks of a chunk the renderer does not show are unknown to the eye test.
    let render_origin = view.render_origin;
    let eye = Vec3::new(0.5, 0.25, 0.5);
    assert!(vis.eye_in_open(render_origin, eye, 0.17, |_| Some(false)));
    vis.set_unshown_at(center, true, 0, now);
    assert!(!vis.eye_in_open(render_origin, eye, 0.17, |_| Some(false)));
    // A new renderer shows every chunk again.
    vis.clear_unshown(lookup);
    assert!(vis.eye_in_open(render_origin, eye, 0.17, |_| Some(false)));
    let set = vis.update_at(&view, lookup, now + TIGHTEN_DELAY * 4);
    assert!(!set.is_visible(behind));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn random_worlds_match_brute_force(
        seed in any::<u64>(),
        p in 20u64..90,
        radius in 1u32..6,
        vertical in 1u32..4,
        dir in (-1.0f32..1.0, -1.0f32..1.0, -1.0f32..1.0),
        frustum_on in any::<bool>(),
        seen_through in 0u64..3,
    ) {
        let d = Vec3::new(dir.0, dir.1, dir.2);
        let dir = (frustum_on && d.length() > 0.1).then_some(d);
        let view = view_at(ChunkPos::new(-7, 4, 11), dir, radius, vertical);
        // Up to 1 chunk in 16 the renderer does not show.
        let links = |pos: ChunkPos| {
            let d = IVec3::new(pos.x as i32, pos.y as i32, pos.z as i32);
            if hash(seed ^ 0x5eed, d) % 32 < seen_through {
                ALL_LINKS | XRAY
            } else {
                random_links(seed, d, p)
            }
        };
        let set = visible_chunks(&view, links);
        prop_assert_eq!(offsets(&set, &view), brute_force(&view, &links));
    }

    #[test]
    fn an_eye_in_open_space_has_only_open_voxels_within_reach(
        seed in any::<u64>(),
        eye in (0.0f32..1.0, 0.0f32..1.0, 0.0f32..1.0),
        reach in 0.0f32..1.5,
        far in any::<bool>(),
    ) {
        let origin = if far {
            I64Vec3::new((1 << 31) - 1, -(1 << 40), 5)
        } else {
            I64Vec3::new(3, -2, 7)
        };
        // About one voxel in five occludes.
        let occludes = |p: BlockPos| {
            let d = p.as_i64vec3() - origin;
            Some(hash(seed, IVec3::new(d.x as i32, d.y as i32, d.z as i32)).is_multiple_of(5))
        };
        let eye = Vec3::new(eye.0, eye.1, eye.2);
        if eye_in_open(origin, eye, reach, occludes) {
            // Points on spheres around the eye, out to the reach.
            for i in 0..512u64 {
                let h = hash(seed ^ i, IVec3::ZERO);
                let unit = |k: u32| ((h >> k) & 0xffff) as f32 / 65535.0 * 2.0 - 1.0;
                let dir = Vec3::new(unit(0), unit(16), unit(32)).normalize_or_zero();
                let point = eye + dir * reach * ((h >> 48) as f32 / 65535.0);
                let p = BlockPos::from_i64vec3(origin + point.floor().as_i64vec3());
                prop_assert_eq!(occludes(p), Some(false), "{} from {}", point, eye);
            }
        }
    }
}
