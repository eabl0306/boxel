//! Far level of detail meshing (ADR 0031): the LOD material table and the post-passes (octant
//! splits, skirts, fluid walls), on `test:` blocks.
use boxel_mesh::lod::{
    LodMaterials, add_fluid_walls, add_skirts, fits, mesh_node, octant_of, split_octants,
};
use boxel_mesh::{
    AoPolicy, ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher, PADDED_VOLUME, PackedQuad,
    PaddedChunk, VoxelClass,
};
use boxel_voxel::lod::{CellPartial, LodClass};
use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_VOLUME, ChunkData, Face, LocalPos, NEIGHBOR_OFFSETS,
    RenderShape, testing,
};
use proptest::prelude::*;
use std::collections::BTreeSet;

fn by(r: &BlockRegistry, name: &str) -> BlockState {
    r.parse_state(name).expect(name)
}

/// The padded index of `(x, y, z)`, each in `-1..=32`.
fn index(x: i32, y: i32, z: i32) -> usize {
    PaddedChunk::index(x, y, z)
}

/// A node's 34^3 cells from `cell(x, y, z)`, each in `-1..=32`.
fn node_cells(mut cell: impl FnMut(i32, i32, i32) -> BlockState) -> Vec<BlockState> {
    let mut cells = vec![BlockState::AIR; PADDED_VOLUME];
    for y in -1..=32 {
        for x in -1..=32 {
            for z in -1..=32 {
                cells[index(x, y, z)] = cell(x, y, z);
            }
        }
    }
    cells
}

/// The mesh of `cells` before any post-pass.
fn plain_mesh(materials: &LodMaterials, cells: &[BlockState]) -> (PaddedChunk, ChunkMesh) {
    let mut padded = PaddedChunk::new();
    materials.fill(cells, &mut padded);
    let mut mesh = ChunkMesh::default();
    Mesher::new().mesh(
        &padded,
        None,
        materials.table(),
        MeshOptions::with_ao(AoPolicy::Off),
        &mut mesh,
    );
    (padded, mesh)
}

/// Every unit face of every quad of `mesh`, with its material layer and bucket.
fn unit_faces(mesh: &ChunkMesh) -> BTreeSet<([u32; 3], Face, u16, usize)> {
    let mut out = BTreeSet::new();
    for b in 0..8 {
        for &q in mesh.bucket(b) {
            for (p, f) in q.unit_faces() {
                assert!(
                    out.insert((p, f, q.layer(), b)),
                    "a face drawn twice: {p:?} {f:?}"
                );
            }
        }
    }
    out
}

#[test]
fn materials_are_opaque_cubes_of_canonical_states() {
    let r = testing::registry();
    let m = LodMaterials::from_registry(&r);
    let t = m.table();
    let water = by(&r, "test:fluid");
    let hot = by(&r, "test:hot");
    // A fluid is an opaque cube with its source as canonical state.
    for level in 0..16 {
        let flowing = by(&r, &format!("test:fluid[level={level}]"));
        assert_eq!(m.canonical(flowing), water.0, "level {level}");
        let lava = by(&r, &format!("test:hot[level={level}]"));
        assert_eq!(m.canonical(lava), hot.0, "level {level}");
    }
    for fluid in [water, hot] {
        assert_eq!(m.class(fluid.0), LodClass::Fluid);
        assert_eq!(t.shape(fluid.0), RenderShape::Opaque);
        assert_eq!(t.class(fluid.0), VoxelClass::Opaque);
        assert!(t.model(fluid.0).is_solid_cube());
    }
    // The emissive fluid keeps its glow; the other does not get one.
    assert!(t.emissive(hot.0));
    assert!(!t.emissive(water.0));
    assert!(t.emissive(by(&r, "test:lamp").0));
    // Look-alike states share one canonical id: a slab looks like the solid block it is made of.
    let solid = by(&r, "test:solid");
    for name in ["test:slab", "test:slab[waterlogged=true]", "test:solid"] {
        assert_eq!(m.canonical(by(&r, name)), solid.0, "{name}");
    }
    assert_eq!(m.class(solid.0), LodClass::Solid);
    // Leaves are ground, with their own layer.
    let leaf = by(&r, "test:cutout");
    assert_eq!(m.canonical(leaf), leaf.0);
    assert_eq!(t.shape(leaf.0), RenderShape::Opaque);
    assert_eq!(
        t.layer(leaf.0, Face::PosY),
        r.face_texture(leaf, Face::PosY).0
    );
    // Dropped at a distance: material 0.
    for name in ["test:plant", "test:glass", "test:boxes"] {
        assert_eq!(m.canonical(by(&r, name)), 0, "{name}");
    }
    assert_eq!(m.canonical(BlockState::AIR), 0);
    assert_eq!(m.canonical(BlockState(u16::MAX)), 0);
    // The side faces of a fluid show its side layer.
    assert_eq!(
        t.layer(water.0, Face::PosX),
        r.face_texture(water, Face::PosX).0
    );
    // Canonical states are the only materials of the table.
    for s in 0..r.len() as u16 {
        let canonical = m.canonical(BlockState(s)) == s;
        assert_eq!(
            t.shape(s) == RenderShape::Opaque,
            canonical && s != 0,
            "state {s}"
        );
    }
}

#[test]
fn fill_writes_canonical_opaque_voxels() {
    let r = testing::registry();
    let m = LodMaterials::from_registry(&r);
    let (flowing, plant) = (by(&r, "test:fluid[level=3]"), by(&r, "test:plant"));
    let cells = node_cells(|x, y, z| match (x, y, z) {
        (-1, -1, -1) => flowing,
        (32, 0, 5) => plant,
        (4, 7, 9) => by(&r, "test:slab"),
        _ => BlockState::AIR,
    });
    let mut padded = PaddedChunk::new();
    padded.set(1, 1, 1, 9, VoxelClass::Opaque);
    m.fill(&cells, &mut padded);
    assert_eq!(padded.get(-1, -1, -1), by(&r, "test:fluid").0);
    assert_eq!(padded.get(32, 0, 5), 0);
    assert_eq!(padded.get(4, 7, 9), by(&r, "test:solid").0);
    assert_eq!(padded.get(1, 1, 1), 0, "cleared first");
    let row = 5 + 8 * 34;
    assert_eq!(padded.opaque[row], 1 << 10);
}

/// The states random scenes are made of: ground, leaves, a glowing block and two fluids.
fn scene_palette(r: &BlockRegistry) -> [BlockState; 5] {
    [
        by(r, "test:solid"),
        by(r, "test:cutout"),
        by(r, "test:lamp"),
        by(r, "test:fluid"),
        by(r, "test:hot"),
    ]
}

/// A random node: per padded column a ground top, a fluid top and a ground state, then some cells
/// set at random (air included).
fn scene_strategy() -> impl Strategy<Value = (Vec<(i32, i32, usize)>, Vec<(usize, usize)>)> {
    (
        prop::collection::vec((-2i32..34, -2i32..34, 0usize..3), 34 * 34),
        prop::collection::vec((0..PADDED_VOLUME, 0usize..6), 0..64),
    )
}

/// The cells of a random scene (`scene_strategy`). Neighboring columns share their tops in
/// blocks of `smooth` columns so the ground has flat stretches and steps.
fn scene_cells(
    r: &BlockRegistry,
    columns: &[(i32, i32, usize)],
    sprinkles: &[(usize, usize)],
    smooth: usize,
) -> Vec<BlockState> {
    let pal = scene_palette(r);
    let fluid = if columns[0].2 == 0 { pal[3] } else { pal[4] };
    let mut cells = node_cells(|x, y, z| {
        let (cx, cz) = (
            (x + 1) as usize / smooth * smooth,
            (z + 1) as usize / smooth * smooth,
        );
        let (ground, sea, kind) = columns[cz * 34 + cx];
        if y <= ground {
            pal[kind]
        } else if y <= sea {
            fluid
        } else {
            BlockState::AIR
        }
    });
    for &(i, s) in sprinkles {
        cells[i] = if s == 5 { BlockState::AIR } else { pal[s] };
    }
    cells
}

/// The octant of cell `p` (`x | y << 1 | z << 2`).
fn octant(p: [u32; 3]) -> u8 {
    ((p[0] >> 4) | (p[1] >> 4) << 1 | (p[2] >> 4) << 2) as u8
}

/// A quad as a rectangle in space: its face's axis, plane and direction, and its extent along
/// the two other axes.
#[derive(Clone, Copy, Debug)]
struct Rect {
    n: usize,
    plane: f64,
    sign: f64,
    u: (usize, f64, f64),
    v: (usize, f64, f64),
}

impl Rect {
    /// `q` of a mesh whose cell `(0, 0, 0)` starts at `origin`, cells `cell` blocks wide.
    fn of(q: PackedQuad, origin: [f64; 3], cell: f64) -> Self {
        let face = q.face();
        let (n, ua, va) = face.axes();
        let normal = face.normal();
        let sign = f64::from(normal.x + normal.y + normal.z);
        let p = q.pos().map(f64::from);
        let positive = if sign > 0.0 { 1.0 } else { 0.0 };
        let span = |a: usize, len: u32| {
            (
                a,
                origin[a] + p[a] * cell,
                origin[a] + (p[a] + f64::from(len)) * cell,
            )
        };
        Self {
            n,
            plane: origin[n] + (p[n] + positive) * cell,
            sign,
            u: span(ua, q.w()),
            v: span(va, q.h()),
        }
    }

    /// Where the ray `o + t d` meets the rectangle from its front, `t > 0`.
    fn hit(&self, o: [f64; 3], d: [f64; 3]) -> Option<f64> {
        if d[self.n] * self.sign >= 0.0 {
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

/// The nearest front hit of the ray among `rects`, `t` in `(0, t_max]`.
fn nearest(rects: &[Rect], o: [f64; 3], d: [f64; 3], t_max: f64) -> Option<f64> {
    rects
        .iter()
        .filter_map(|r| r.hit(o, d))
        .filter(|&t| t <= t_max)
        .min_by(f64::total_cmp)
}

fn rects(
    mesh: &ChunkMesh,
    origin: [f64; 3],
    cell: f64,
    skip: impl Fn(PackedQuad) -> bool,
) -> Vec<Rect> {
    // Opaque buckets only: a LOD mesh has nothing else, and full-detail fluids are translucent.
    (0..6)
        .flat_map(|b| mesh.bucket(b).iter().copied())
        .filter(|&q| !skip(q))
        .map(|q| Rect::of(q, origin, cell))
        .collect()
}

/// Brute force: is there a cell `pick` keeps within 2 cells of `c` (Chebyshev, inside the padded
/// grid, `c` included)?
fn near(cells: &[BlockState], c: [i32; 3], pick: impl Fn(BlockState) -> bool) -> bool {
    let range = |a: i32| (a - 2).max(-1)..=(a + 2).min(32);
    range(c[1]).any(|y| range(c[0]).any(|x| range(c[2]).any(|z| pick(cells[index(x, y, z)]))))
}

fn near_air(cells: &[BlockState], m: &LodMaterials, c: [i32; 3]) -> bool {
    near(cells, c, |s| m.canonical(s) == 0)
}

fn fluid_state(m: &LodMaterials, s: BlockState) -> bool {
    m.class(m.canonical(s)) == LodClass::Fluid
}

/// Every (cell, face) of the post-pass planes: on plane `p` of an axis, the cell at `p - 1` facing
/// the positive way and the cell at `p` facing the negative way, inside the node.
fn plane_cells() -> Vec<([i32; 3], Face, [i32; 3])> {
    let mut out = Vec::new();
    for (pos, neg) in [
        (Face::PosX, Face::NegX),
        (Face::PosY, Face::NegY),
        (Face::PosZ, Face::NegZ),
    ] {
        let (n, ua, va) = pos.axes();
        for p in [0, 16, 32] {
            for u in 0..32 {
                for v in 0..32 {
                    for (face, at, step) in [(pos, p - 1, 1), (neg, p, -1)] {
                        if !(0..32).contains(&at) {
                            continue;
                        }
                        let mut c = [0; 3];
                        c[n] = at;
                        c[ua] = u;
                        c[va] = v;
                        let mut across = c;
                        across[n] += step;
                        out.push((c, face, across));
                    }
                }
            }
        }
    }
    out
}

/// The unit faces `after` has and `before` has not, as (cell, face).
fn added(before: &ChunkMesh, after: &ChunkMesh) -> BTreeSet<([u32; 3], Face)> {
    let old = unit_faces(before);
    let new = unit_faces(after);
    assert!(old.is_subset(&new), "a post-pass removed faces");
    new.difference(&old).map(|&(p, f, _, _)| (p, f)).collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    #[test]
    fn octant_splits_keep_the_faces(scene in scene_strategy(), smooth in 1usize..9) {
        let r = testing::registry();
        let m = LodMaterials::from_registry(&r);
        let cells = scene_cells(&r, &scene.0, &scene.1, smooth);
        let (_, mut mesh) = plain_mesh(&m, &cells);
        let before = unit_faces(&mesh);
        split_octants(&mut mesh);
        prop_assert_eq!(unit_faces(&mesh), before);
        for &q in &mesh.quads {
            for (p, _) in q.unit_faces() {
                prop_assert_eq!(octant(p), octant_of(q), "{:?}", q);
            }
        }
        prop_assert_eq!(mesh.bucket_counts.iter().sum::<u32>() as usize, mesh.quads.len());
        prop_assert_eq!(mesh.hash, mesh.compute_hash());
        prop_assert!(fits(&mesh));
    }

    #[test]
    fn skirts_sit_where_ground_meets_ground_near_air(
        scene in scene_strategy(),
        smooth in 1usize..9,
    ) {
        let r = testing::registry();
        let m = LodMaterials::from_registry(&r);
        let cells = scene_cells(&r, &scene.0, &scene.1, smooth);
        let (padded, mut mesh) = plain_mesh(&m, &cells);
        split_octants(&mut mesh);
        let before = mesh.clone();
        add_skirts(&padded, &m, &mut mesh);
        let material = |c: [i32; 3]| m.canonical(cells[index(c[0], c[1], c[2])]);
        let expected: BTreeSet<([u32; 3], Face)> = plane_cells()
            .into_iter()
            .filter(|&(c, _, across)| {
                material(c) != 0 && material(across) != 0 && near_air(&cells, &m, c)
            })
            .map(|(c, f, _)| (c.map(|a| a as u32), f))
            .collect();
        prop_assert_eq!(added(&before, &mesh), expected);
        for b in 0..6 {
            for &q in mesh.bucket(b) {
                prop_assert_eq!(q.face().index(), b);
                for (p, _) in q.unit_faces() {
                    prop_assert_eq!(octant(p), octant_of(q));
                    let c = p.map(|a| a as i32);
                    prop_assert_eq!(q.layer(), m.table().layer(material(c), q.face()));
                }
            }
        }
        prop_assert_eq!(mesh.hash, mesh.compute_hash());
    }

    #[test]
    fn fluid_walls_line_every_plane_at_level_one_only(
        scene in scene_strategy(),
        smooth in 1usize..9,
    ) {
        let r = testing::registry();
        let m = LodMaterials::from_registry(&r);
        let cells = scene_cells(&r, &scene.0, &scene.1, smooth);
        let (padded, mut mesh) = plain_mesh(&m, &cells);
        split_octants(&mut mesh);
        add_skirts(&padded, &m, &mut mesh);
        let before = mesh.clone();
        for level in 2..=6 {
            add_fluid_walls(&padded, &m, level, &mut mesh);
            prop_assert_eq!(&mesh, &before, "level {}", level);
        }
        add_fluid_walls(&padded, &m, 1, &mut mesh);
        let material = |c: [i32; 3]| m.canonical(cells[index(c[0], c[1], c[2])]);
        let expected: BTreeSet<([u32; 3], Face)> = plane_cells()
            .into_iter()
            .filter(|&(c, _, across)| {
                material(c) != 0
                    && material(across) != 0
                    && near(&cells, c, |s| fluid_state(&m, s))
                    && !near_air(&cells, &m, c)
            })
            .map(|(c, f, _)| (c.map(|a| a as u32), f))
            .collect();
        prop_assert_eq!(added(&before, &mesh), expected);
        for b in 0..6 {
            for &q in mesh.bucket(b) {
                for (p, _) in q.unit_faces() {
                    prop_assert_eq!(octant(p), octant_of(q));
                }
            }
        }
    }

    #[test]
    fn post_pass_faces_are_never_seen_from_inside_a_drawn_node(
        scene in scene_strategy(),
        smooth in 1usize..9,
        rays in prop::collection::vec(
            ((0.0f64..32.0, 0.0f64..32.0, 0.0f64..32.0), (-1.0f64..1.0, -1.0f64..1.0, -1.0f64..1.0)),
            64,
        ),
    ) {
        let r = testing::registry();
        let m = LodMaterials::from_registry(&r);
        let cells = scene_cells(&r, &scene.0, &scene.1, smooth);
        let (padded, mut plain) = plain_mesh(&m, &cells);
        split_octants(&mut plain);
        let mut full = ChunkMesh::default();
        mesh_node(&mut Mesher::new(), &padded, &m, 1, &mut full);
        // Opaque cubes only, each in the bucket of its face.
        prop_assert_eq!(full.bucket_counts[6] + full.bucket_counts[7], 0);
        for b in 0..6 {
            for &q in full.bucket(b) {
                prop_assert_eq!(q.face().index(), b);
                prop_assert_eq!(q.shape(), boxel_mesh::layout::SHAPE_CUBE);
                prop_assert_eq!(q.light_index(), 0);
            }
        }
        prop_assert_eq!(full.palette.len(), usize::from(!full.quads.is_empty()));
        let (a, b) = (
            rects(&plain, [0.0; 3], 1.0, |_| false),
            rects(&full, [0.0; 3], 1.0, |_| false),
        );
        for ((x, y, z), (dx, dy, dz)) in rays {
            let c = [x as i32, y as i32, z as i32];
            if m.canonical(cells[index(c[0], c[1], c[2])]) != 0 {
                continue;
            }
            // Up to where the ray leaves the node.
            let (o, d) = ([x, y, z], [dx, dy, dz]);
            let t_max = (0..3)
                .map(|k| match d[k] {
                    v if v > 1e-6 => (32.0 - o[k]) / v,
                    v if v < -1e-6 => -o[k] / v,
                    _ => f64::INFINITY,
                })
                .fold(f64::INFINITY, f64::min);
            let (seen, with) = (nearest(&a, o, d, t_max), nearest(&b, o, d, t_max));
            prop_assert_eq!(seen, with, "ray {:?} {:?}", o, d);
        }
    }
}

#[test]
fn a_sea_gets_walls_under_its_skirts() {
    let r = testing::registry();
    let m = LodMaterials::from_registry(&r);
    let (solid, water) = (by(&r, "test:solid"), by(&r, "test:fluid"));
    // Ground up to cell 4, a sea up to cell 20.
    let cells = node_cells(|_, y, _| match y {
        ..=4 => solid,
        5..=20 => water,
        _ => BlockState::AIR,
    });
    let (padded, mut mesh) = plain_mesh(&m, &cells);
    split_octants(&mut mesh);
    let plain = mesh.clone();
    add_skirts(&padded, &m, &mut mesh);
    let skirted = mesh.clone();
    add_fluid_walls(&padded, &m, 1, &mut mesh);
    let on_plane = |faces: &BTreeSet<([u32; 3], Face)>| -> BTreeSet<u32> {
        faces
            .iter()
            .filter(|(p, f)| *f == Face::PosX && p[0] == 15 && p[2] == 7)
            .map(|(p, _)| p[1])
            .collect()
    };
    // The plane x = 16: skirts near the air, walls under them, down into the ground's side.
    assert_eq!(on_plane(&added(&plain, &skirted)), (19..=20).collect());
    let walls = added(&skirted, &mesh);
    assert_eq!(on_plane(&walls), (3..=18).collect());
    for face in [Face::NegX, Face::PosZ, Face::NegZ] {
        let count = walls.iter().filter(|(_, f)| *f == face).count();
        assert!(count > 0, "{face:?}");
    }
    // The only horizontal walls lie on the plane y = 16.
    assert!(
        walls
            .iter()
            .filter(|(_, f)| matches!(f, Face::PosY | Face::NegY))
            .all(|(p, f)| p[1] == if *f == Face::PosY { 15 } else { 16 })
    );
    // None at level 2.
    let mut level2 = skirted.clone();
    add_fluid_walls(&padded, &m, 2, &mut level2);
    assert_eq!(level2, skirted);
}

/// A smooth terrain for the crack tests: rolling ground (slopes up to about 1:1) with a basin
/// under a sea, every block below the ground solid.
struct Terrain {
    base: f64,
    amplitude: f64,
    /// Basin center `(x, z)` and depth.
    basin: (f64, f64, f64),
    sea: i64,
    /// Ground tops of `x, z` in `-64..192`.
    tops: Vec<i64>,
}

impl Terrain {
    const MIN: i64 = -64;
    const SIZE: i64 = 256;

    fn new(base: f64, amplitude: f64, basin: (f64, f64, f64), sea: i64) -> Self {
        let mut t = Self {
            base,
            amplitude,
            basin,
            sea,
            tops: Vec::new(),
        };
        t.tops = (0..Self::SIZE * Self::SIZE)
            .map(|i| t.top_at(i % Self::SIZE + Self::MIN, i / Self::SIZE + Self::MIN))
            .collect();
        t
    }

    fn top_at(&self, x: i64, z: i64) -> i64 {
        let (x, z) = (x as f64, z as f64);
        let (bx, bz, depth) = self.basin;
        let d2 = (x - bx).powi(2) + (z - bz).powi(2);
        let h = self.base + self.amplitude * (9.0 * (x / 23.0).sin() + 7.0 * (z / 17.0).cos())
            - depth * (-d2 / 900.0).exp();
        h.floor() as i64
    }

    fn top(&self, x: i64, z: i64) -> i64 {
        let (i, k) = (x - Self::MIN, z - Self::MIN);
        assert!((0..Self::SIZE).contains(&i) && (0..Self::SIZE).contains(&k));
        self.tops[(k * Self::SIZE + i) as usize]
    }

    /// The cells of the level-`level` node whose cell `(0, 0, 0)` starts at block `origin`, by the
    /// exact cell rule.
    fn cells(
        &self,
        r: &BlockRegistry,
        m: &LodMaterials,
        level: u8,
        origin: [i64; 3],
    ) -> Vec<BlockState> {
        let (solid, water) = (by(r, "test:solid"), by(r, "test:fluid"));
        let s = 1i64 << level;
        node_cells(|x, y, z| {
            let o = [
                origin[0] + i64::from(x) * s,
                origin[1] + i64::from(y) * s,
                origin[2] + i64::from(z) * s,
            ];
            CellPartial::of_cell(m.classes(), o, s as u32, |dx, dy, dz| {
                let (bx, by, bz) = (
                    o[0] + i64::from(dx),
                    o[1] + i64::from(dy),
                    o[2] + i64::from(dz),
                );
                if by <= self.top(bx, bz) {
                    solid
                } else if by <= self.sea {
                    water
                } else {
                    BlockState::AIR
                }
            })
            .state()
        })
    }
}

/// A LOD node meshed with every post-pass, as rectangles in block space, its hidden octants left
/// out.
fn node_rects(
    cells: &[BlockState],
    m: &LodMaterials,
    level: u8,
    origin: [i64; 3],
    hidden: u8,
) -> Vec<Rect> {
    let mut padded = PaddedChunk::new();
    m.fill(cells, &mut padded);
    let mut mesh = ChunkMesh::default();
    mesh_node(&mut Mesher::new(), &padded, m, level, &mut mesh);
    assert!(fits(&mesh));
    let cell = f64::from(1u32 << level);
    rects(&mesh, origin.map(|o| o as f64), cell, |q| {
        hidden & (1 << octant_of(q)) != 0
    })
}

/// The top of the non-air cells of column `(cx, cz)` of a node, in blocks above its origin.
fn cell_top(cells: &[BlockState], m: &LodMaterials, cx: i32, cz: i32, cell: f64) -> f64 {
    let top = (-1..=32)
        .rev()
        .find(|&y| m.canonical(cells[index(cx, y, cz)]) != 0)
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

/// Casts `count` rays from `start()` to `target()` through `rects`; returns the rays that hit
/// nothing before their target (they would show the sky through a crack).
fn slips(
    rects: &[Rect],
    count: usize,
    mut ray: impl FnMut() -> Option<([f64; 3], [f64; 3])>,
) -> (usize, Vec<([f64; 3], [f64; 3])>) {
    let mut cast = 0;
    let mut slipped = Vec::new();
    while cast < count {
        let Some((o, target)) = ray() else { continue };
        cast += 1;
        let d = [target[0] - o[0], target[1] - o[1], target[2] - o[2]];
        if nearest(rects, o, d, 1.0).is_none() {
            slipped.push((o, target));
        }
    }
    (cast, slipped)
}

#[test]
fn no_ray_slips_between_two_lod_levels() {
    let r = testing::registry();
    let m = LodMaterials::from_registry(&r);
    // Seas 11 to 18 blocks deep cross the seam at x = 64.
    for (basin, sea, amplitude) in [
        ((64.0, 32.0, 14.0), 32, 1.0),
        ((56.0, 24.0, 20.0), 30, 0.8),
        ((72.0, 40.0, 18.0), 34, 1.2),
    ] {
        let terrain = Terrain::new(34.0, amplitude, basin, sea);
        let fine = terrain.cells(&r, &m, 1, [0; 3]);
        let coarse = terrain.cells(&r, &m, 2, [0; 3]);
        // The fine node draws the coarse node's octant (0, 0, 0); the coarse node draws x >= 64.
        let mut rects = node_rects(&fine, &m, 1, [0; 3], 0);
        rects.extend(node_rects(&coarse, &m, 2, [0; 3], 0b0101_0101));
        let mut n = Numbers(7);
        let (cast, slipped) = slips(&rects, 10_000, || {
            let (x, z) = (n.between(2.0, 62.0), n.between(4.0, 60.0));
            let top = cell_top(&fine, &m, (x / 2.0) as i32, (z / 2.0) as i32, 2.0);
            let start = [x, n.between(top + 0.01, top + 20.0).min(63.0), z];
            let (tx, tz) = (n.between(66.0, 126.0), n.between(4.0, 60.0));
            let top = cell_top(&coarse, &m, (tx / 4.0) as i32, (tz / 4.0) as i32, 4.0);
            let target = [tx, n.between((top - 12.0).max(0.5), top - 0.01), tz];
            (start[1] < 63.0).then_some((start, target))
        });
        assert_eq!(cast, 10_000);
        assert!(
            slipped.is_empty(),
            "basin {basin:?}: {} rays slipped, first {:?}",
            slipped.len(),
            &slipped[..slipped.len().min(4)]
        );
    }
}

impl Terrain {
    /// The lod-0 chunk at chunk position `c`.
    fn chunk(&self, r: &BlockRegistry, c: [i64; 3]) -> ChunkData {
        let (solid, water) = (by(r, "test:solid"), by(r, "test:fluid"));
        let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
        for (i, slot) in dense.iter_mut().enumerate() {
            let p = LocalPos::from_index(i);
            let (x, y, z) = (
                c[0] * 32 + i64::from(p.x()),
                c[1] * 32 + i64::from(p.y()),
                c[2] * 32 + i64::from(p.z()),
            );
            *slot = if y <= self.top(x, z) {
                solid
            } else if y <= self.sea {
                water
            } else {
                BlockState::AIR
            };
        }
        ChunkData::from_dense(&dense)
    }

    /// The full-detail mesh of chunk `(0, 0, 0)`, its neighbors as they are.
    fn chunk_mesh(&self, r: &BlockRegistry) -> ChunkMesh {
        let center = self.chunk(r, [0; 3]);
        let neighbors: Vec<ChunkData> = NEIGHBOR_OFFSETS
            .iter()
            .map(|d| self.chunk(r, [i64::from(d.x), i64::from(d.y), i64::from(d.z)]))
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
}

#[test]
fn no_ray_slips_between_full_detail_and_level_one() {
    let r = testing::registry();
    let m = LodMaterials::from_registry(&r);
    // Seas 8 to 14 blocks deep cross the seam at x = 32, their beds sloping either way; the
    // ground stays inside chunk y 0.
    for (basin, sea, amplitude) in [
        ((32.0, 16.0, 12.0), 18, 0.5),
        ((24.0, 16.0, 16.0), 18, 0.5),
        ((40.0, 16.0, 16.0), 18, 0.5),
        ((20.0, 20.0, 20.0), 24, 0.3),
        ((44.0, 10.0, 20.0), 24, 0.8),
    ] {
        let terrain = Terrain::new(20.0, amplitude, basin, sea);
        let coarse = terrain.cells(&r, &m, 1, [0; 3]);
        // Full detail draws chunk (0, 0, 0), the level-1 node's octant 0; its fluid is
        // translucent, so only its opaque faces stop a ray.
        let mut rects = rects(&terrain.chunk_mesh(&r), [0.0; 3], 1.0, |_| false);
        rects.extend(node_rects(&coarse, &m, 1, [0; 3], 0b0000_0001));
        let mut n = Numbers(11);
        let target = |n: &mut Numbers| {
            let (tx, tz) = (n.between(33.0, 63.0), n.between(2.0, 30.0));
            let top = cell_top(&coarse, &m, (tx / 2.0) as i32, (tz / 2.0) as i32, 2.0);
            [tx, n.between((top - 8.0).max(0.5), top - 0.01), tz]
        };
        // From above the ground and the sea.
        let (cast, slipped) = slips(&rects, 10_000, || {
            let (x, z) = (n.between(1.0, 31.0), n.between(2.0, 30.0));
            let top = (terrain.top(x as i64, z as i64).max(terrain.sea) + 1) as f64;
            (top < 30.0).then(|| ([x, n.between(top + 0.01, 31.0), z], target(&mut n)))
        });
        assert_eq!(cast, 10_000);
        assert!(
            slipped.is_empty(),
            "basin {basin:?}: {} rays from above slipped, first {:?}",
            slipped.len(),
            &slipped[..slipped.len().min(4)]
        );
        // From under the near sea's surface.
        let (cast, slipped) = slips(&rects, 10_000, || {
            let (x, z) = (n.between(1.0, 31.0), n.between(2.0, 30.0));
            let ground = (terrain.top(x as i64, z as i64) + 1) as f64;
            let sea = terrain.sea as f64 + 0.9;
            (ground + 1.0 < sea).then(|| ([x, n.between(ground + 0.01, sea), z], target(&mut n)))
        });
        assert_eq!(cast, 10_000);
        assert!(
            slipped.is_empty(),
            "basin {basin:?}: {} rays from under the sea slipped, first {:?}",
            slipped.len(),
            &slipped[..slipped.len().min(4)]
        );
    }
}
