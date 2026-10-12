//! Crack ray tests of the far level of detail (ADR 0031) on the demo's own coarse cells: rays
//! cast from finer terrain across a seam toward coarser terrain must meet an opaque face before
//! a point inside the coarser ground, or the sky would show through the seam, and must cross a
//! face of the water before a point inside the coarser water (translucent near and far, issue
//! #50), or its bed would show untinted. Two scenes over a sea more than 8 blocks deep: two LOD
//! levels of the forest's source, and real full-detail chunks of the forest beside a level-1
//! node, there also from the level-1 terrain into a chunk (a chunk at the loading edge drawn by
//! the LOD with a meshed one behind it).
use boxel_demo::worldgen::ForestGenerator;
use boxel_mesh::lod::{LodMaterials, Seam, fits, is_bank, mesh_node, octant_of, seam_of};
use boxel_mesh::{
    ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher, PackedQuad, PaddedChunk,
    TRANSLUCENT_BUCKET, layout,
};
use boxel_voxel::{
    BlockRegistry, BlockState, ChunkData, ChunkKey, ChunkPos, DimensionId, NEIGHBOR_OFFSETS,
};
use boxel_worldgen::WorldGenerator;
use boxel_worldgen::lod::{LodCells, LodNode, LodRequest, LodScratch};

/// A quad as a rectangle in space: its face's axis, plane and direction, and its extent along
/// the two other axes; for a LOD bank, how high the eye must be for it to be drawn.
#[derive(Clone, Copy, Debug)]
struct Rect {
    n: usize,
    plane: f64,
    sign: f64,
    u: (usize, f64, f64),
    v: (usize, f64, f64),
    bank: Option<f64>,
}

impl Rect {
    /// `q` of a mesh whose cell `(0, 0, 0)` starts at `origin`, cells `cell` blocks wide.
    /// A fluid surface quad of full detail (`SHAPE_FLUID_SURFACE`) has its top 0.875 up its
    /// cell. A LOD bank lies on its cell's side away from its normal, and one on a side is drawn
    /// only for an eye above its top (`lod_common.wgsl`); drawn behind everything, it stops a
    /// ray only where nothing else would (nothing lies past a crack test's point in the ground).
    fn of(q: PackedQuad, origin: [f64; 3], cell: f64) -> Self {
        let face = q.face();
        let (n, ua, va) = face.axes();
        let normal = face.normal();
        let sign = f64::from(normal.x + normal.y + normal.z);
        let p = q.pos().map(f64::from);
        let surface = q.shape() == layout::SHAPE_FLUID_SURFACE;
        let bank = is_bank(q);
        let positive = match (sign > 0.0, surface && n == 1) {
            _ if bank => f64::from(u8::from(sign < 0.0)),
            (true, true) => layout::FLUID_SURFACE as f64,
            (true, false) => 1.0,
            _ => 0.0,
        };
        let span = |a: usize, len: u32| {
            let len = if surface && a == 1 {
                layout::FLUID_SURFACE as f64
            } else {
                f64::from(len)
            };
            (a, origin[a] + p[a] * cell, origin[a] + (p[a] + len) * cell)
        };
        let v = span(va, q.h());
        Self {
            n,
            plane: origin[n] + (p[n] + positive) * cell,
            sign,
            u: span(ua, q.w()),
            v,
            bank: bank.then_some(if n == 1 { f64::NEG_INFINITY } else { v.2 }),
        }
    }

    /// Where the ray `o + t d` meets the rectangle from its front, `t > 0`.
    fn hit(&self, o: [f64; 3], d: [f64; 3]) -> Option<f64> {
        if d[self.n] * self.sign >= 0.0 || self.bank.is_some_and(|top| o[1] <= top) {
            return None;
        }
        self.crosses(o, d)
    }

    /// Where the ray `o + t d` meets the rectangle from either side, `t > 0` (water, drawn with
    /// no face culling).
    fn crosses(&self, o: [f64; 3], d: [f64; 3]) -> Option<f64> {
        if d[self.n] == 0.0 {
            return None;
        }
        let t = (self.plane - o[self.n]) / d[self.n];
        if t <= 1e-9 {
            return None;
        }
        let at = |a: usize| o[a] + t * d[a];
        let inside = |(a, lo, hi): (usize, f64, f64)| (lo..=hi).contains(&at(a));
        (inside(self.u) && inside(self.v)).then_some(t)
    }
}

/// The quads of `mesh` as rectangles, those `skip` keeps out left out: its opaque ones (the face
/// buckets) and its water's (the translucent bucket). With `lod` (a LOD mesh), a water skirt on a
/// side starts `FLUID_SURFACE` up its cell, as the LOD's water draws it (`lod_common.wgsl`).
fn rects(
    mesh: &ChunkMesh,
    origin: [f64; 3],
    cell: f64,
    lod: bool,
    skip: impl Fn(PackedQuad) -> bool,
) -> Rects {
    let of = |buckets: &[usize]| -> Vec<Rect> {
        buckets
            .iter()
            .flat_map(|&b| mesh.bucket(b).iter().copied())
            .filter(|&q| !skip(q))
            .map(|q| Rect::of(q, origin, cell))
            .collect()
    };
    let mut water = of(&[TRANSLUCENT_BUCKET]);
    if lod {
        let quads = mesh
            .bucket(TRANSLUCENT_BUCKET)
            .iter()
            .filter(|&&q| !skip(q));
        for (r, q) in water.iter_mut().zip(quads) {
            if seam_of(*q) != Seam::None && r.n != 1 {
                r.v.1 += f64::from(layout::FLUID_SURFACE);
            }
        }
    }
    Rects {
        opaque: of(&[0, 1, 2, 3, 4, 5]),
        water,
    }
}

/// The rectangles of a scene: what stops a ray, and the water it crosses.
#[derive(Default)]
struct Rects {
    opaque: Vec<Rect>,
    water: Vec<Rect>,
}

impl Rects {
    fn extend(&mut self, other: Rects) {
        self.opaque.extend(other.opaque);
        self.water.extend(other.water);
    }

    /// True if a ray to a point inside the ground (at `t = 1`) meets no opaque face first: it
    /// would show the sky through a crack.
    fn slips(&self, o: [f64; 3], d: [f64; 3]) -> bool {
        !self
            .opaque
            .iter()
            .filter_map(|r| r.hit(o, d))
            .any(|t| t <= 1.0)
    }

    /// True if a ray from above every surface to a point inside the water (at `t = 1`) meets no
    /// opaque face and crosses no face of the water first: it would show the bed untinted.
    fn untinted(&self, o: [f64; 3], d: [f64; 3]) -> bool {
        self.slips(o, d)
            && !self
                .water
                .iter()
                .filter_map(|r| r.crosses(o, d))
                .any(|t| t <= 1.0)
    }
}

/// The level-`level` node `[x, y, z]` of the forest's coarse terrain, `None` if it is uniform
/// air (nothing to draw).
fn node(
    world: &ForestGenerator,
    r: &BlockRegistry,
    level: u8,
    [x, y, z]: [i64; 3],
) -> Option<LodCells> {
    let source = world.lod().expect("a LOD source");
    let req = LodRequest {
        dim: DimensionId(0),
        level,
        column: [x, z],
        layers: y..=y,
    };
    let mut out = None;
    let band = source.column(&req, r, &mut LodScratch::default(), &mut |_, node| {
        out = Some(node_cells(node));
    });
    match band.uniform_at(y) {
        Some(state) => {
            assert_eq!(state, BlockState::AIR, "node {x} {y} {z} at level {level}");
            None
        }
        None => out.expect("an emitted node"),
    }
}

fn node_cells(node: LodNode<'_>) -> Option<LodCells> {
    match node {
        LodNode::Cells(cells) => Some(cells.clone()),
        LodNode::Uniform(BlockState::AIR) => None,
        LodNode::Uniform(state) => panic!("a node that draws nothing ({state:?}) in a scene"),
    }
}

/// A LOD node meshed with every post-pass, as rectangles in blocks from `origin`, its hidden
/// octants left out.
fn node_rects(
    cells: &LodCells,
    m: &LodMaterials,
    level: u8,
    origin: [f64; 3],
    hidden: u8,
) -> Rects {
    let mut padded = PaddedChunk::new();
    m.fill(&cells.0[..], &mut padded);
    let mut mesh = ChunkMesh::default();
    mesh_node(&mut Mesher::new(), &padded, m, level, &mut mesh);
    assert!(fits(&mesh));
    rects(&mesh, origin, f64::from(1u32 << level), true, |q| {
        hidden & (1 << octant_of(q)) != 0
    })
}

/// The top of the non-air cells of column `(x, z)` of a node, in blocks above its origin; with
/// `ground`, of the cells that are not see-through (not water).
fn cell_top(cells: &LodCells, m: &LodMaterials, x: i32, z: i32, cell: f64, ground: bool) -> f64 {
    let top = (-1..=32)
        .rev()
        .find(|&y| {
            let c = m.canonical(cells.get(x, y, z));
            c != 0 && !(ground && m.see_through(c))
        })
        .unwrap_or(-1);
    f64::from(top + 1) * cell
}

/// A fixed sequence of numbers in `0..1` (splitmix64).
struct Numbers(u64);

impl Numbers {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }

    fn between(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next()
    }
}

/// Casts `count` rays from `ray()`'s start to its target; returns those `fails(o, d)`.
fn cast(
    count: usize,
    mut ray: impl FnMut() -> Option<([f64; 3], [f64; 3])>,
    fails: impl Fn([f64; 3], [f64; 3]) -> bool,
) -> Vec<([f64; 3], [f64; 3])> {
    let (mut cast, mut failed) = (0, Vec::new());
    while cast < count {
        let Some((o, target)) = ray() else { continue };
        cast += 1;
        let d = [target[0] - o[0], target[1] - o[1], target[2] - o[2]];
        if fails(o, d) {
            failed.push((o, target));
        }
    }
    failed
}

/// Fails with the first rays of `failed` unless it is empty.
fn assert_none(what: &str, failed: &[([f64; 3], [f64; 3])]) {
    assert!(
        failed.is_empty(),
        "{what}: {} rays failed, first {:?}",
        failed.len(),
        &failed[..failed.len().min(4)]
    );
}

/// True if the forest's sea is more than 8 blocks deep somewhere along the seam `x = seam`,
/// `z` in `zs`.
fn deep_sea_at(world: &ForestGenerator, seam: i64, zs: std::ops::Range<i64>) -> bool {
    let sea = world.sea.expect("the default forest has a sea").level;
    zs.into_iter().any(|z| {
        [seam - 1, seam]
            .iter()
            .all(|&x| sea - i64::from(world.terrain.height_at(x, z)) > 8)
    })
}

/// Casts 10,000 rays from above the level-`fine` terrain of the coarse node column `column`'s
/// octants `(0, *, 0)` (drawn by the two fine nodes of layers 0 and 1) into the level `fine + 1`
/// ground of its octants `(1, *, 0)`, and 10,000 into its water; returns the rays that slipped
/// and those that reached the water untinted.
fn two_levels(
    world: &ForestGenerator,
    r: &BlockRegistry,
    m: &LodMaterials,
    fine: u8,
    column: [i64; 2],
) -> [Vec<([f64; 3], [f64; 3])>; 2] {
    let coarse = fine + 1;
    let (sf, sc) = (f64::from(1u32 << fine), f64::from(1u32 << coarse));
    // The coarse node's edge, and the fine one's.
    let (e, half) = (32.0 * sc, 32.0 * sf);
    let fine_nodes: Vec<LodCells> = (0..2)
        .filter_map(|y| node(world, r, fine, [column[0] * 2, y, column[1] * 2]))
        .collect();
    let big = node(world, r, coarse, [column[0], 0, column[1]]).expect("coarse terrain");
    // The fine nodes draw the coarse node's octants with x = 0 (rays stay at z < half).
    let mut rects = Rects::default();
    for (y, cells) in fine_nodes.iter().enumerate() {
        rects.extend(node_rects(cells, m, fine, [0.0, y as f64 * half, 0.0], 0));
    }
    rects.extend(node_rects(&big, m, coarse, [0.0; 3], 0b0101_0101));
    // The top of the fine terrain (its water included) at `(x, z)`, in blocks.
    let fine_top = |x: f64, z: f64| {
        let (cx, cz) = ((x / sf) as i32, (z / sf) as i32);
        fine_nodes
            .iter()
            .enumerate()
            .map(|(y, cells)| match cell_top(cells, m, cx, cz, sf, false) {
                0.0 => 0.0,
                top => top + y as f64 * half,
            })
            .fold(0.0, f64::max)
    };
    let mut n = Numbers(7);
    let start = |n: &mut Numbers| {
        let (x, z) = (
            n.between(sf, half - sf),
            n.between(2.0 * sf, half - 2.0 * sf),
        );
        let top = fine_top(x, z);
        [x, n.between(top + 0.01, top + 20.0), z]
    };
    let target = |n: &mut Numbers| {
        (
            n.between(half + sf, e - sf),
            n.between(2.0 * sf, half - 2.0 * sf),
        )
    };
    let into_ground = |n: &mut Numbers| {
        let (tx, tz) = target(n);
        let top = cell_top(&big, m, (tx / sc) as i32, (tz / sc) as i32, sc, true);
        [tx, n.between((top - 6.0 * sc).max(0.5), top - 0.01), tz]
    };
    // A point inside the coarse water, if the scene has some (a few tries).
    let into_water = |n: &mut Numbers| {
        (0..64).find_map(|_| {
            let (tx, tz) = target(n);
            let (cx, cz) = ((tx / sc) as i32, (tz / sc) as i32);
            let bed = cell_top(&big, m, cx, cz, sc, true);
            let top = cell_top(&big, m, cx, cz, sc, false);
            (top > bed).then(|| [tx, n.between(bed + 0.01, top - 0.01), tz])
        })
    };
    let slipped = cast(
        10_000,
        || {
            let (o, t) = (start(&mut n), into_ground(&mut n));
            (o[1] < e - 1.0).then_some((o, t))
        },
        |o, d| rects.slips(o, d),
    );
    let untinted = if into_water(&mut Numbers(3)).is_some() {
        cast(
            10_000,
            || {
                let o = start(&mut n);
                let t = into_water(&mut n)?;
                (o[1] < e - 1.0).then_some((o, t))
            },
            |o, d| rects.untinted(o, d),
        )
    } else {
        Vec::new()
    };
    [slipped, untinted]
}

#[test]
fn no_ray_slips_between_two_demo_lod_levels() {
    let r = boxel_demo::blocks::registry();
    let m = LodMaterials::from_registry(&r);
    let world = ForestGenerator::default();
    // Coarse node columns over a sea 9 to 20 blocks deep across the seam (one with hills up to
    // y = 56 behind it, and at level 3 hills up to 75), and hills up to y = 85 without a sea.
    for (fine, column, sea) in [
        (1, [-34, -2], true),
        (1, [15, -27], true),
        (1, [-15, 7], false),
        (2, [-32, -20], true),
        (2, [-38, 19], true),
    ] {
        let e = 32i64 << (fine + 1);
        let origin = column.map(|c| c * e);
        let seam = origin[0] + e / 2;
        let deep = deep_sea_at(&world, seam, origin[1]..origin[1] + e / 2);
        assert_eq!(deep, sea, "level {fine} column {column:?}");
        let [slipped, untinted] = two_levels(&world, &r, &m, fine, column);
        assert_none(&format!("level {fine} column {column:?}, ground"), &slipped);
        assert_none(&format!("level {fine} column {column:?}, water"), &untinted);
    }
}

/// The full-detail mesh of the forest's chunk `c`, its neighbors as generated.
fn chunk_mesh(world: &ForestGenerator, r: &BlockRegistry, c: [i64; 3]) -> ChunkMesh {
    let chunk = |d: [i64; 3]| {
        let pos = ChunkPos::new(c[0] + d[0], c[1] + d[1], c[2] + d[2]);
        world.generate(ChunkKey::new(pos), r)
    };
    let center = chunk([0; 3]);
    let neighbors: Vec<ChunkData> = NEIGHBOR_OFFSETS
        .iter()
        .map(|d| chunk([d.x, d.y, d.z].map(i64::from)))
        .collect();
    let input = MeshInput {
        center: &center,
        neighbors: std::array::from_fn(|i| Some(&neighbors[i])),
        light: None,
    };
    Mesher::new().mesh_chunk(
        &input,
        &MaterialTable::from_registry(r),
        MeshOptions::default(),
    )
}

#[test]
fn no_ray_slips_between_demo_chunks_and_level_one() {
    let r = boxel_demo::blocks::registry();
    let m = LodMaterials::from_registry(&r);
    let world = ForestGenerator::default();
    let sea = world.sea.expect("a sea").level as f64;
    // Level-1 node columns whose octant (0, 1, 0), the chunk at y 32..64, holds a sea 9 to 13
    // blocks deep across the seam at x = 32: open sea, and a coast.
    for column in [[-53, 20], [-53, -48], [-60, -20]] {
        let origin = [column[0] * 64, 0, column[1] * 64];
        assert!(
            deep_sea_at(&world, origin[0] + 32, origin[2] + 2..origin[2] + 30),
            "no deep sea at {column:?}"
        );
        let height = |x: f64, z: f64| {
            let (x, z) = (origin[0] + x as i64, origin[2] + z as i64);
            f64::from(world.terrain.height_at(x, z))
        };
        let coarse = node(&world, &r, 1, [column[0], 0, column[1]]).expect("terrain");
        // Full detail draws chunk (0, 1, 0) of the node, its octant 2: its opaque faces stop a
        // ray, its translucent water tints it.
        let chunk = [column[0] * 2, 1, column[1] * 2];
        let mut rects = rects(
            &chunk_mesh(&world, &r, chunk),
            [0.0, 32.0, 0.0],
            1.0,
            false,
            |_| false,
        );
        rects.extend(node_rects(&coarse, &m, 1, [0.0; 3], 0b0000_0100));
        let mut n = Numbers(11);
        let target = |n: &mut Numbers| (n.between(33.0, 63.0), n.between(2.0, 30.0));
        let into_ground = |n: &mut Numbers| {
            let (tx, tz) = target(n);
            let top = cell_top(&coarse, &m, (tx / 2.0) as i32, (tz / 2.0) as i32, 2.0, true);
            [tx, n.between((top - 8.0).max(32.5), top - 0.01), tz]
        };
        let into_water = |n: &mut Numbers| loop {
            let (tx, tz) = target(n);
            let (cx, cz) = ((tx / 2.0) as i32, (tz / 2.0) as i32);
            let bed = cell_top(&coarse, &m, cx, cz, 2.0, true);
            let top = cell_top(&coarse, &m, cx, cz, 2.0, false);
            if top > bed {
                return [tx, n.between(bed.max(32.0) + 0.01, top - 0.01), tz];
            }
        };
        // From above the ground and the sea (whose surface is 0.875 up its top block).
        let above = |n: &mut Numbers| {
            let (x, z) = (n.between(1.0, 31.0), n.between(2.0, 30.0));
            let ground = height(x, z);
            let top = if ground >= sea {
                ground + 1.0
            } else {
                sea + 0.875
            };
            (top < 62.0).then(|| [x, n.between(top + 0.01, 63.0), z])
        };
        let slipped = cast(
            10_000,
            || Some((above(&mut n)?, into_ground(&mut n))),
            |o, d| rects.slips(o, d),
        );
        assert_none(&format!("column {column:?}, from above"), &slipped);
        let untinted = cast(
            10_000,
            || Some((above(&mut n)?, into_water(&mut n))),
            |o, d| rects.untinted(o, d),
        );
        assert_none(
            &format!("column {column:?}, from above into the water"),
            &untinted,
        );
        // From under the near sea's surface.
        let slipped = cast(
            10_000,
            || {
                let (x, z) = (n.between(1.0, 31.0), n.between(2.0, 30.0));
                let ground = height(x, z) + 1.0;
                let top = sea + 0.875;
                (ground + 1.0 < top)
                    .then(|| ([x, n.between(ground + 0.01, top), z], into_ground(&mut n)))
            },
            |o, d| rects.slips(o, d),
        );
        assert_none(&format!("column {column:?}, from under the sea"), &slipped);
    }
}

#[test]
fn no_ray_slips_from_demo_level_one_into_chunks() {
    let r = boxel_demo::blocks::registry();
    let m = LodMaterials::from_registry(&r);
    let world = ForestGenerator::default();
    // The other way from the steady view (a chunk at the loading edge drawn by level 1 with a
    // meshed one behind it): full detail draws the node's octant (1, 1, 0), the chunk at x 32..64
    // and y 32..64, over the seas of the test above; rays from above the level-1 terrain at
    // x < 32, its water included, into the ground of the chunk.
    for column in [[-53, 20], [-53, -48], [-60, -20]] {
        let origin = [column[0] * 64, 0, column[1] * 64];
        let coarse = node(&world, &r, 1, [column[0], 0, column[1]]).expect("terrain");
        let chunk = [column[0] * 2 + 1, 1, column[1] * 2];
        let mut rects = rects(
            &chunk_mesh(&world, &r, chunk),
            [32.0, 32.0, 0.0],
            1.0,
            false,
            |_| false,
        );
        rects.extend(node_rects(&coarse, &m, 1, [0.0; 3], 0b0000_1000));
        let mut n = Numbers(17);
        let into_ground = |n: &mut Numbers| {
            let (tx, tz) = (n.between(33.0, 63.0), n.between(2.0, 30.0));
            let (x, z) = (origin[0] + tx as i64, origin[2] + tz as i64);
            let top = (f64::from(world.terrain.height_at(x, z)) + 1.0).min(64.0);
            (top > 33.0).then(|| [tx, n.between((top - 8.0).max(32.5), top - 0.01), tz])
        };
        let above = |n: &mut Numbers| {
            let (x, z) = (n.between(1.0, 31.0), n.between(2.0, 30.0));
            let top = cell_top(&coarse, &m, (x / 2.0) as i32, (z / 2.0) as i32, 2.0, false);
            (top < 62.0).then(|| [x, n.between(top + 0.01, 63.0), z])
        };
        let slipped = cast(
            10_000,
            || Some((above(&mut n)?, into_ground(&mut n)?)),
            |o, d| rects.slips(o, d),
        );
        assert_none(&format!("column {column:?}, into the chunk"), &slipped);
    }
}
