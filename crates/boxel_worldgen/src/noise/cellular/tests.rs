//! Cellular noise against brute force: sites inside their cells, F1 and F2 (distances and ids)
//! against every site of a wide window, the grid equal to the point form, no overflow next to
//! `2^61` and `2^62`, and Voronoi cells against the empty-circle definition with exact
//! rationals (vertices, neighbors, chaining and symmetry).
use super::*;
use boxel_voxel::MAX_BLOCK_COORD;
use proptest::prelude::*;

/// The two nearest sites of `(x, z)` among every cell within `radius` of its own, by `(d2, id)`.
fn brute_nearest(c: &Cellular, x: i64, z: i64, radius: i64) -> Nearest {
    let home = c.cell_of(x, z);
    let mut all: Vec<(u128, SiteId, Site)> = Vec::new();
    for di in -radius..=radius {
        for dj in -radius..=radius {
            let s = c.site(SiteId {
                i: home.i + di,
                j: home.j + dj,
            });
            all.push((dist_sq((x, z), s.pos()), s.id, s));
        }
    }
    all.sort_by_key(|&(d, id, _)| (d, id));
    Nearest {
        f1: all[0].2,
        d1: all[0].0,
        f2: all[1].2,
        d2: all[1].0,
    }
}

/// Places: the origin, the edges of `i32`, `2^40`, next to the world limit and next to `2^62`.
fn place() -> impl Strategy<Value = i64> {
    let centers = [
        0,
        1 << 31,
        -(1 << 31),
        1 << 40,
        MAX_BLOCK_COORD,
        -MAX_BLOCK_COORD,
        (1 << 62) - (1 << 30),
        -(1 << 62) + (1 << 30),
    ];
    (
        prop::sample::select(centers.to_vec()),
        -(1i64 << 20)..(1 << 20),
    )
        .prop_map(|(c, d)| c + d)
}

/// A lattice: cells of 2^1 to 2^14 blocks with a margin from none to almost half a cell.
fn any_cellular() -> impl Strategy<Value = Cellular> {
    (any::<u64>(), 1u32..=14, 0u32..=8).prop_map(|(seed, shift, eighths)| {
        let size = 1i64 << shift;
        let margin = ((size - 1) / 2).min(size * i64::from(eighths) / 16);
        Cellular::new(seed, shift, margin)
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn sites_lie_inside_their_cells(c in any_cellular(), x in place(), z in place()) {
        let id = c.cell_of(x, z);
        let s = c.site(id);
        let size = 1i64 << c.shift;
        let (ox, oz) = (s.x - (id.i << c.shift), s.z - (id.j << c.shift));
        prop_assert!((c.margin..size - c.margin).contains(&ox), "{ox}");
        prop_assert!((c.margin..size - c.margin).contains(&oz), "{oz}");
        prop_assert_eq!(c.cell_of(s.x, s.z), id);
        prop_assert_eq!(c.site(id), s);
    }

    #[test]
    fn nearest_matches_brute_force(c in any_cellular(), x in place(), z in place()) {
        // Radius 5: past any ring the search can need (3 with no margin).
        prop_assert_eq!(c.nearest(x, z), brute_nearest(&c, x, z, 5));
    }

    #[test]
    fn nearest_grid_equals_point(
        c in any_cellular(), x0 in place(), z0 in place(), step in 1i64..5000,
    ) {
        let xs: Vec<i64> = (0..7).map(|i| x0 + i * step).collect();
        let zs: Vec<i64> = (0..6).map(|i| z0 - i * step).collect();
        let mut grid = vec![c.nearest(0, 0); 7 * 6];
        c.nearest_grid(&xs, &zs, &mut grid);
        for (i, &x) in xs.iter().enumerate() {
            for (j, &z) in zs.iter().enumerate() {
                prop_assert_eq!(grid[i * 6 + j], c.nearest(x, z));
            }
        }
    }
}

#[test]
fn nearest_matches_brute_force_on_every_point_of_a_patch() {
    // Every point of 8 x 8 cells, for small and large margins: a search that stops a ring early
    // shows up where F2 is farther than a cell.
    for (shift, margin) in [(1, 0), (3, 0), (3, 3), (5, 0), (5, 6), (5, 15)] {
        let c = Cellular::new(0x5eed + u64::from(shift), shift, margin);
        let side = 8i64 << shift;
        for origin in [0, MAX_BLOCK_COORD - side] {
            for x in origin..origin + side {
                for z in -origin - side..-origin {
                    assert_eq!(c.nearest(x, z), brute_nearest(&c, x, z, 4), "({x}, {z})");
                }
            }
        }
    }
}

#[test]
fn nearest_breaks_ties_by_id() {
    // Cells of one block: every site sits on its cell's corner, a regular grid, so a point on a
    // site has 4 equal second nearest sites; F2 is the smallest id.
    let c = Cellular::new(5, 0, 0);
    let n = c.nearest(10, -3);
    assert_eq!((n.f1.pos(), n.d1), ((10, -3), 0));
    assert_eq!((n.f2.id, n.d2), (SiteId { i: 9, j: -3 }, 1));
    assert_eq!(n, brute_nearest(&c, 10, -3, 3));
}

#[test]
fn far_queries_do_not_overflow() {
    let c = Cellular::new(77, 12, 0);
    for (x, z) in [
        ((1 << 62) - 1, (1 << 62) - 1),
        (-(1 << 62) + 1, -(1 << 62) + 1),
        (MAX_BLOCK_COORD + 31, -MAX_BLOCK_COORD),
        (i64::from(i32::MAX), i64::from(i32::MIN)),
    ] {
        assert_eq!(c.nearest(x, z), brute_nearest(&c, x, z, 4), "({x}, {z})");
        let cell = c.voronoi_cell(c.cell_of(x, z));
        assert!(cell.edges.len() >= 3);
    }
}

/// `gcd(|a|, |b|)`.
fn gcd(mut a: i128, mut b: i128) -> i128 {
    (a, b) = (a.abs(), b.abs());
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// A circumcenter relative to `o`, in lowest terms: `(nx, nz, den)` with `den > 0`.
fn normalized(v: ExactPoint, o: (i64, i64)) -> (i128, i128, i128) {
    let (nx, nz) = (
        v.nx - i128::from(o.0) * v.den,
        v.nz - i128::from(o.1) * v.den,
    );
    let g = gcd(gcd(nx, nz), v.den);
    (nx / g, nz / g, v.den / g)
}

/// `|v - p|^2 * den^2` for `v` relative to `o` (exact for the small cells the tests use).
fn scaled(v: (i128, i128, i128), o: (i64, i64), p: (i64, i64)) -> i128 {
    let (nx, nz, den) = v;
    let dx = nx - i128::from(p.0 - o.0) * den;
    let dz = nz - i128::from(p.1 - o.1) * den;
    dx * dx + dz * dz
}

/// The sites of every cell within `radius` of `id`, `id` excluded.
fn around(c: &Cellular, id: SiteId, radius: i64) -> Vec<Site> {
    let mut sites = Vec::new();
    for di in -radius..=radius {
        for dj in -radius..=radius {
            if (di, dj) != (0, 0) {
                sites.push(c.site(SiteId {
                    i: id.i + di,
                    j: id.j + dj,
                }));
            }
        }
    }
    sites
}

/// The Voronoi vertices of site `id` by definition: the centers of the empty circles through it
/// and two other sites (every site within `radius + 1` checked against the circle).
fn brute_vertices(c: &Cellular, id: SiteId, radius: i64) -> Vec<(i128, i128, i128)> {
    let a = c.site(id);
    let near = around(c, id, radius);
    let wide = around(c, id, radius + 1);
    let mut out = Vec::new();
    for (k, &b) in near.iter().enumerate() {
        for &d in &near[k + 1..] {
            let (b, d) = match orient(a.pos(), b.pos(), d.pos()) {
                0 => continue,
                o if o > 0 => (b, d),
                _ => (d, b),
            };
            let empty = wide.iter().all(|s| {
                s.id == b.id || s.id == d.id || incircle(a.pos(), b.pos(), d.pos(), s.pos()) <= 0
            });
            if empty {
                let v = circumcenter(a.pos(), b.pos(), d.pos()).unwrap();
                out.push(normalized(v, a.pos()));
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn voronoi_cells_match_the_definition(
        seed in any::<u64>(), shift in 2u32..=12, eighths in 0u32..=6, x in place(), z in place(),
    ) {
        let size = 1i64 << shift;
        let margin = ((size - 1) / 2).min(size * i64::from(eighths) / 16);
        let c = Cellular::new(seed, shift, margin);
        let id = c.cell_of(x, z);
        let cell = c.voronoi_cell(id);
        let a = cell.site;
        prop_assert_eq!(a, c.site(id));
        let n = cell.edges.len();
        prop_assert!(n >= 3);
        let wide = around(&c, id, c.window + 2);
        let mut vertices = Vec::new();
        for (k, e) in cell.edges.iter().enumerate() {
            // Edges chain counterclockwise.
            prop_assert_eq!(e.end, cell.edges[(k + 1) % n].start);
            let next = cell.edges[(k + 1) % n].neighbor;
            prop_assert!(orient(a.pos(), e.neighbor.pos(), next.pos()) > 0);
            // Each vertex is equidistant from the site and both neighbors, and no site is nearer.
            let v = normalized(e.end, a.pos());
            let r = scaled(v, a.pos(), a.pos());
            prop_assert_eq!(scaled(v, a.pos(), e.neighbor.pos()), r);
            prop_assert_eq!(scaled(v, a.pos(), next.pos()), r);
            for s in &wide {
                prop_assert!(scaled(v, a.pos(), s.pos()) >= r, "{s:?} nearer to {v:?}");
            }
            vertices.push(v);
        }
        vertices.sort_unstable();
        vertices.dedup();
        prop_assert_eq!(vertices, brute_vertices(&c, id, c.window + 1));
        // Neighbors are mutual.
        for e in &cell.edges {
            let back = c.voronoi_cell(e.neighbor.id);
            prop_assert!(back.edges.iter().any(|f| f.neighbor.id == id), "{:?}", e.neighbor.id);
        }
    }
}

#[test]
fn window_radius_follows_the_margin() {
    // No margin: 3 cells; from about 0.17 of a cell: 2; sites near the center: 1.
    assert_eq!(window_radius(4096, 0), 3);
    assert_eq!(window_radius(4096, 4096 / 10), 3);
    assert_eq!(window_radius(4096, 4096 / 5), 2);
    assert_eq!(window_radius(4096, 2047), 1);
    // Cells of one block: a vertex can lie anywhere in a unit square around a corner site.
    assert_eq!(window_radius(1, 0), 2);
    assert_eq!(window_radius(4, 0), 3);
}

#[test]
fn voronoi_neighbor_three_cells_away_on_small_cells() {
    // Cells of 4 blocks: sites (11, -1) and (10, 8) are neighbors 3 cells apart, past a window
    // of 2 that bounded the vertices as integer points (sqrt(2) (S - 1 - m)).
    let c = Cellular::new(splitmix64(605), 2, 0);
    let a = c.site(SiteId { i: 2, j: -1 });
    let far = c.site(SiteId { i: 2, j: 2 });
    assert_eq!((a.pos(), far.pos()), ((11, -1), (10, 8)));
    let cell = c.voronoi_cell(a.id);
    assert!(cell.edges.iter().any(|e| e.neighbor.id == far.id));
}

#[test]
fn voronoi_window_holds_every_neighbor() {
    // Every (shift, margin) up to 2^12 where the window bound on integer points, sqrt(2) (S - 1 -
    // m), gave a window one cell smaller than the real vertices need: the cell from the window
    // must equal the cell from a window one larger. `VORONOI_SEEDS` runs more seeds.
    let seeds: u64 = std::env::var("VORONOI_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let configs = [
        (0u32, 0i64),
        (1, 0),
        (2, 0),
        (2, 1),
        (3, 1),
        (4, 2),
        (5, 5),
        (6, 24),
        (7, 48),
        (10, 175),
        (10, 387),
        (11, 351),
        (11, 775),
        (12, 702),
    ];
    let mut wrong = Vec::new();
    for (shift, margin) in configs {
        for seed in 0..seeds {
            let c = Cellular::new(splitmix64(seed), shift, margin);
            for i in -2..2 {
                for j in -2..2 {
                    let id = SiteId { i, j };
                    if c.voronoi_cell(id) != c.voronoi_cell_in(id, c.window + 1) {
                        wrong.push((shift, margin, seed, id));
                    }
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{} cells differ, first {:?}",
        wrong.len(),
        &wrong[..wrong.len().min(4)]
    );
}

#[test]
fn voronoi_vertices_are_empty_on_small_cells() {
    // Every vertex of every cell around the origin against a window of 8 cells, exactly, for the
    // small cells and margins where the window radius is tightest.
    for (shift, margin) in [
        (0u32, 0i64),
        (1, 0),
        (2, 0),
        (2, 1),
        (3, 1),
        (4, 2),
        (5, 5),
        (6, 24),
    ] {
        for seed in 0..60u64 {
            let c = Cellular::new(splitmix64(seed), shift, margin);
            for i in -2..2 {
                for j in -2..2 {
                    let id = SiteId { i, j };
                    let cell = c.voronoi_cell(id);
                    let wide = around(&c, id, 8);
                    for e in &cell.edges {
                        let v = normalized(e.end, cell.site.pos());
                        let r = scaled(v, cell.site.pos(), cell.site.pos());
                        for s in &wide {
                            assert!(
                                scaled(v, cell.site.pos(), s.pos()) >= r,
                                "shift {shift} margin {margin} seed {seed} {id:?}: {s:?} nearer"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn site_hashes_depend_on_the_salt_and_the_cell() {
    let c = Cellular::new(9, 10, 0);
    let id = SiteId { i: 3, j: -4 };
    assert_ne!(c.site_hash(id, 1), c.site_hash(id, 2));
    assert_ne!(c.site_hash(id, 1), c.site_hash(SiteId { i: 4, j: -4 }, 1));
    assert_eq!(c.site_hash(id, 1), Cellular::new(9, 12, 7).site_hash(id, 1));
}

#[test]
#[should_panic(expected = "margin")]
fn cellular_refuses_a_margin_past_half_a_cell() {
    let _ = Cellular::new(0, 4, 8);
}

#[test]
fn field_seeds_share_no_octave_seeds() {
    // Octave k of an fbm uses the seed `seed + k`: no two fields mixed from one world seed (by
    // salt, the warp's two components, the sites' two offsets) nor nearby world seeds may reach
    // the same octave seed. With `seed + salt`, salt 1 would take salt 0's octaves.
    use crate::noise::{Fbm, Frequency, Warp};
    let meander = Fbm {
        frequency: Frequency::one_in(512),
        octaves: 3,
        lacunarity: 2,
        gain: 0.5,
    };
    let mut seen = std::collections::HashMap::new();
    for world in [
        0,
        1,
        2,
        3,
        0x00b0_7e1d - 1,
        0x00b0_7e1d,
        0x00b0_7e1d + 1,
        u64::MAX,
    ] {
        let [wx, wz] = Warp::new(world, meander, 48).seeds();
        let seeds = (0..16u64)
            .map(|salt| (format!("salt {salt}"), mix_seed(world, salt)))
            .chain([
                ("warp x".to_string(), wx),
                ("warp z".to_string(), wz),
                ("site x".to_string(), mix_seed(world, SALT_X)),
                ("site z".to_string(), mix_seed(world, SALT_Z)),
            ]);
        for (name, seed) in seeds {
            // Up to 64 octaves each (a 1/n frequency with lacunarity 2).
            for k in 0..64u64 {
                let here = (world, name.clone(), k);
                if let Some(other) = seen.insert(seed.wrapping_add(k), here.clone()) {
                    panic!("octave seed shared by {other:?} and {here:?}");
                }
            }
        }
    }
}
