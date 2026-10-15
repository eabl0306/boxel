//! Far level of detail meshing (ADR 0031): the LOD material table and the post-passes (octant
//! splits, the bed under water, skirts, walls), on `test:` blocks. `test:fluid` is see-through
//! (translucent, like water), `test:hot` opaque (like lava).
mod common;

use boxel_mesh::lod::{
    LOD_PALETTE, LodMaterials, MAX_DARK, Seam, add_banks, add_fluid_walls, add_skirts,
    add_underwater, dark_of, fits, is_bank, mesh_node, octant_of, seam_counts, seam_of,
    split_octants, water_seams,
};
use boxel_mesh::{
    AoPolicy, ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher, PADDED_VOLUME, PackedQuad,
    PaddedChunk, TRANSLUCENT_BUCKET, VoxelClass, layout,
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
fn only_full_bright_states_glow_at_a_distance() {
    let mut b = common::glow_builder();
    let paint = b.block_by_name("test:paint").expect("test:paint");
    b.set_full_bright(paint, |_| true).expect("test:paint");
    let r = b.build();
    let m = LodMaterials::from_registry(&r);
    let t = m.table();
    // An unmarked emitter looks like the plain block with its layers, also at a distance.
    let solid = by(&r, "test:solid");
    let ember = by(&r, "test:ember");
    assert_eq!(m.canonical(ember), solid.0);
    assert!(!t.emissive(m.canonical(ember)));
    // A marked state glows, and does not merge with a plain look-alike.
    let paint = by(&r, "test:paint");
    assert_eq!(m.canonical(paint), paint.0);
    assert!(t.emissive(paint.0));
    let lamp = by(&r, "test:lamp");
    assert!(t.emissive(m.canonical(lamp)));
    for s in (0..r.len() as u16).map(BlockState) {
        let c = m.canonical(s);
        if c != 0 {
            assert_eq!(t.emissive(c), r.is_full_bright(s), "{}", r.state_name(s));
        }
    }
}

#[test]
fn materials_are_cubes_of_canonical_states_and_water_is_see_through() {
    let r = testing::registry();
    let m = LodMaterials::from_registry(&r);
    let t = m.table();
    let water = by(&r, "test:fluid");
    let hot = by(&r, "test:hot");
    // A fluid is a cube with its source as canonical state.
    for level in 0..16 {
        let flowing = by(&r, &format!("test:fluid[level={level}]"));
        assert_eq!(m.canonical(flowing), water.0, "level {level}");
        let lava = by(&r, &format!("test:hot[level={level}]"));
        assert_eq!(m.canonical(lava), hot.0, "level {level}");
    }
    for fluid in [water, hot] {
        assert_eq!(m.class(fluid.0), LodClass::Fluid);
        assert!(t.model(fluid.0).is_solid_cube());
    }
    // The water a thin model holds is water's material, though the wet state comes first.
    let wet = by(&r, "test:boxes[waterlogged=true]");
    assert!(wet < water);
    assert_eq!(m.canonical(wet), water.0);
    // A translucent fluid is see-through at a distance too, its faces in the translucent
    // bucket; an opaque one (lava) stays an opaque cube.
    assert!(m.see_through(water.0));
    assert_eq!(t.shape(water.0), RenderShape::Translucent);
    assert_eq!(t.bucket(water.0, Face::PosY), TRANSLUCENT_BUCKET);
    assert!(!m.see_through(hot.0));
    assert_eq!(t.shape(hot.0), RenderShape::Opaque);
    assert_eq!(t.class(hot.0), VoxelClass::Opaque);
    assert_eq!(t.bucket(hot.0, Face::PosY), Face::PosY.index());
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
    // Canonical states are the only materials of the table, and only water is see-through.
    for s in 0..r.len() as u16 {
        let canonical = m.canonical(BlockState(s)) == s;
        assert_eq!(
            t.shape(s) != RenderShape::None,
            canonical && s != 0,
            "state {s}"
        );
        assert_eq!(m.see_through(s), s == water.0, "state {s}");
    }
    assert!(!m.see_through(u16::MAX));
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
    /// `q` of a mesh whose cell `(0, 0, 0)` starts at `origin`, cells `cell` blocks wide. A
    /// fluid surface quad of full detail (`SHAPE_FLUID_SURFACE`) has its top 0.875 up its cell.
    /// A LOD bank lies on its cell's side away from its normal, and one on a side is drawn only
    /// for an eye above its top (`lod_common.wgsl`); it is drawn behind everything, so it stops
    /// a ray only where nothing else would (a crack test's ray into the ground meets nothing
    /// past it).
    fn of(q: PackedQuad, origin: [f64; 3], cell: f64) -> Self {
        let face = q.face();
        let (n, ua, va) = face.axes();
        let normal = face.normal();
        let sign = f64::from(normal.x + normal.y + normal.z);
        let p = q.pos().map(f64::from);
        let surface = q.shape() == layout::SHAPE_FLUID_SURFACE;
        let bank = is_bank(q);
        let positive = match (sign > 0.0, surface && face == Face::PosY) {
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

    /// Where the ray `o + t d` meets the rectangle from either side, `t > 0` (a fluid face, drawn
    /// with no face culling).
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

/// The nearest front hit of the ray among `rects`, `t` in `(0, t_max]`.
fn nearest(rects: &[Rect], o: [f64; 3], d: [f64; 3], t_max: f64) -> Option<f64> {
    rects
        .iter()
        .filter_map(|r| r.hit(o, d))
        .filter(|&t| t <= t_max)
        .min_by(f64::total_cmp)
}

/// True if the ray crosses one of `rects` (from either side), `t` in `(0, t_max]`.
fn crosses(rects: &[Rect], o: [f64; 3], d: [f64; 3], t_max: f64) -> bool {
    rects
        .iter()
        .filter_map(|r| r.crosses(o, d))
        .any(|t| t <= t_max)
}

/// The opaque quads of `mesh` (its face buckets: what stops a ray), as rectangles.
fn rects(
    mesh: &ChunkMesh,
    origin: [f64; 3],
    cell: f64,
    skip: impl Fn(PackedQuad) -> bool,
) -> Vec<Rect> {
    (0..6)
        .flat_map(|b| mesh.bucket(b).iter().copied())
        .filter(|&q| !skip(q))
        .map(|q| Rect::of(q, origin, cell))
        .collect()
}

/// The translucent quads of `mesh` (the see-through fluid's), as rectangles. With `lod`, a skirt
/// on a side starts `FLUID_SURFACE` up its cell, as the LOD's water draws it (`lod_common.wgsl`).
fn water_rects(
    mesh: &ChunkMesh,
    origin: [f64; 3],
    cell: f64,
    lod: bool,
    skip: impl Fn(PackedQuad) -> bool,
) -> Vec<Rect> {
    mesh.bucket(TRANSLUCENT_BUCKET)
        .iter()
        .copied()
        .filter(|&q| !skip(q))
        .map(|q| {
            let mut r = Rect::of(q, origin, cell);
            if lod && seam_of(q) != Seam::None && r.n != 1 {
                r.v.1 += f64::from(layout::FLUID_SURFACE);
            }
            r
        })
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

/// True if state `s` is a see-through fluid at a distance.
fn clear_state(m: &LodMaterials, s: BlockState) -> bool {
    m.see_through(m.canonical(s))
}

/// True if state `s` is ground at a distance: drawn, not see-through.
fn ground_state(m: &LodMaterials, s: BlockState) -> bool {
    m.canonical(s) != 0 && !clear_state(m, s)
}

/// Brute force: the darkness of a face whose front cell is `c` in a node of `level`: the cells
/// from `c` up through non-air cells to the highest see-through one, `n s - s/2` blocks, at most
/// 15.
fn dark_at(cells: &[BlockState], m: &LodMaterials, level: u8, c: [i32; 3]) -> u8 {
    let s = 1u32 << level;
    let mut n = 0;
    for y in c[1]..=32 {
        let state = cells[index(c[0], y, c[2])];
        if m.canonical(state) == 0 {
            break;
        }
        if clear_state(m, state) {
            n = y - c[1] + 1;
        }
    }
    // A column whose run starts with ground and holds no see-through fluid is not under water.
    if n == 0 {
        return 0;
    }
    ((n as u32 * s).saturating_sub(s / 2)).min(15) as u8
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
    fn the_bed_under_water_is_every_ground_face_toward_it_lit_by_depth(
        scene in scene_strategy(),
        smooth in 1usize..9,
        level in 1u8..=4,
    ) {
        let r = testing::registry();
        let m = LodMaterials::from_registry(&r);
        let cells = scene_cells(&r, &scene.0, &scene.1, smooth);
        let (padded, mut mesh) = plain_mesh(&m, &cells);
        split_octants(&mut mesh);
        let before = mesh.clone();
        add_underwater(&padded, &m, level, &mut mesh);
        let state = |c: [i32; 3]| cells[index(c[0], c[1], c[2])];
        // Every face of a ground cell of the node whose neighbor is see-through.
        let mut expected = BTreeSet::new();
        for y in 0..32 {
            for x in 0..32 {
                for z in 0..32 {
                    if !ground_state(&m, state([x, y, z])) {
                        continue;
                    }
                    for face in Face::ALL {
                        let n = face.normal();
                        let a = [x + n.x, y + n.y, z + n.z];
                        if clear_state(&m, state(a)) {
                            let dark = dark_at(&cells, &m, level, a);
                            expected.insert(([x as u32, y as u32, z as u32], face, dark));
                        }
                    }
                }
            }
        }
        let added: BTreeSet<_> = added(&before, &mesh).into_iter().collect();
        let got: BTreeSet<_> = mesh
            .quads
            .iter()
            .filter(|&&q| seam_of(q) == Seam::None)
            .flat_map(|&q| q.unit_faces().map(move |(p, f)| (p, f, dark_of(q))))
            .filter(|&(p, f, _)| added.contains(&(p, f)))
            .collect();
        prop_assert_eq!(got, expected);
        // In the bucket of their face, after the mesher's quads, which keep open sky.
        for b in 0..6 {
            let quads = mesh.bucket(b);
            let old = before.bucket(b);
            prop_assert_eq!(&quads[..old.len()], old);
            for &q in quads {
                prop_assert_eq!(q.face().index(), b);
                prop_assert_eq!(seam_of(q), Seam::None);
                for (p, _) in q.unit_faces() {
                    prop_assert_eq!(octant(p), octant_of(q));
                }
            }
        }
        prop_assert!(before.quads.iter().all(|&q| dark_of(q) == 0));
        prop_assert_eq!(mesh.bucket(TRANSLUCENT_BUCKET), before.bucket(TRANSLUCENT_BUCKET));
        prop_assert_eq!(mesh.hash, mesh.compute_hash());
    }

    #[test]
    fn skirts_sit_on_ground_near_air_and_on_the_top_of_the_water(
        scene in scene_strategy(),
        smooth in 1usize..9,
    ) {
        let r = testing::registry();
        let m = LodMaterials::from_registry(&r);
        let cells = scene_cells(&r, &scene.0, &scene.1, smooth);
        let (padded, mut mesh) = plain_mesh(&m, &cells);
        split_octants(&mut mesh);
        let before = mesh.clone();
        add_skirts(&padded, &m, 1, &mut mesh);
        let state = |c: [i32; 3]| cells[index(c[0], c[1], c[2])];
        let material = |c: [i32; 3]| m.canonical(state(c));
        let expected: BTreeSet<([u32; 3], Face)> = plane_cells()
            .into_iter()
            .filter(|&(c, _, across)| {
                if clear_state(&m, state(c)) {
                    material(across) != 0 && material([c[0], c[1] + 1, c[2]]) == 0
                } else {
                    ground_state(&m, state(c))
                        && ground_state(&m, state(across))
                        && near_air(&cells, &m, c)
                }
            })
            .map(|(c, f, _)| (c.map(|a| a as u32), f))
            .collect();
        prop_assert_eq!(added(&before, &mesh), expected);
        for b in [0, 1, 2, 3, 4, 5, TRANSLUCENT_BUCKET] {
            for &q in mesh.bucket(b) {
                if b < 6 {
                    prop_assert_eq!(q.face().index(), b);
                }
                for (p, _) in q.unit_faces() {
                    prop_assert_eq!(octant(p), octant_of(q));
                    let c = p.map(|a| a as i32);
                    prop_assert_eq!(b == TRANSLUCENT_BUCKET, clear_state(&m, state(c)));
                    prop_assert_eq!(q.layer(), m.table().layer(material(c), q.face()));
                    if b == TRANSLUCENT_BUCKET {
                        prop_assert_eq!(dark_of(q), 0);
                    }
                }
            }
        }
        prop_assert_eq!(mesh.hash, mesh.compute_hash());
    }

    #[test]
    fn walls_line_the_ground_under_see_through_fluid_at_every_level(
        scene in scene_strategy(),
        smooth in 1usize..9,
        level in 1u8..=6,
    ) {
        let r = testing::registry();
        let m = LodMaterials::from_registry(&r);
        let cells = scene_cells(&r, &scene.0, &scene.1, smooth);
        let (padded, mut mesh) = plain_mesh(&m, &cells);
        split_octants(&mut mesh);
        add_skirts(&padded, &m, level, &mut mesh);
        let before = mesh.clone();
        add_fluid_walls(&padded, &m, level, &mut mesh);
        let state = |c: [i32; 3]| cells[index(c[0], c[1], c[2])];
        let expected: BTreeSet<([u32; 3], Face)> = plane_cells()
            .into_iter()
            .filter(|&(c, _, across)| {
                ground_state(&m, state(c))
                    && ground_state(&m, state(across))
                    && near(&cells, c, |s| clear_state(&m, s))
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
        // Lit as deep as the cell each faces.
        let new: Vec<PackedQuad> = mesh.quads[..]
            .iter()
            .copied()
            .filter(|q| !before.quads.contains(q))
            .collect();
        for q in new {
            for (p, f) in q.unit_faces() {
                let n = f.normal();
                let a = [p[0] as i32 + n.x, p[1] as i32 + n.y, p[2] as i32 + n.z];
                prop_assert_eq!(dark_of(q), dark_at(&cells, &m, level, a), "{:?}", q);
            }
        }
    }

    #[test]
    fn banks_face_into_water_on_ground_by_every_plane(
        scene in scene_strategy(),
        smooth in 1usize..9,
        level in 1u8..=6,
    ) {
        let r = testing::registry();
        let m = LodMaterials::from_registry(&r);
        let cells = scene_cells(&r, &scene.0, &scene.1, smooth);
        let (padded, mut mesh) = plain_mesh(&m, &cells);
        split_octants(&mut mesh);
        add_skirts(&padded, &m, level, &mut mesh);
        add_fluid_walls(&padded, &m, level, &mut mesh);
        let before = mesh.clone();
        add_banks(&padded, &m, level, &mut mesh);
        let state = |c: [i32; 3]| cells[index(c[0], c[1], c[2])];
        // A see-through cell by a plane, on ground: on its bottom toward a y plane, on its side
        // toward an x or z plane with ground across, or a see-through cell on ground; the bank
        // faces back into it.
        let expected: BTreeSet<([u32; 3], Face)> = plane_cells()
            .into_iter()
            .filter(|&(c, face, across)| {
                let under = |c: [i32; 3]| ground_state(&m, state([c[0], c[1] - 1, c[2]]));
                clear_state(&m, state(c))
                    && match face {
                        Face::PosY => false,
                        Face::NegY => ground_state(&m, state(across)),
                        _ if clear_state(&m, state(across)) => under(c) && under(across),
                        _ => under(c) && ground_state(&m, state(across)),
                    }
            })
            .map(|(c, f, _)| (c.map(|a| a as u32), f.opposite()))
            .collect();
        prop_assert_eq!(added(&before, &mesh), expected);
        let new: Vec<PackedQuad> = mesh.quads[..]
            .iter()
            .copied()
            .filter(|q| !before.quads.contains(q))
            .collect();
        prop_assert!(before.quads.iter().all(|&q| !is_bank(q)));
        for q in new {
            // In the bucket of its face, always drawn, on a plane.
            prop_assert!(is_bank(q), "{:?}", q);
            let b = (0..6).find(|&b| mesh.bucket(b).contains(&q));
            prop_assert_eq!(b, Some(q.face().index()));
            prop_assert_eq!(seam_of(q), Seam::None);
            let (na, _, _) = q.face().axes();
            prop_assert!([0, 15, 16, 31].contains(&q.pos()[na]), "{:?}", q);
            for (p, f) in q.unit_faces() {
                let c = p.map(|a| a as i32);
                prop_assert_eq!(octant(p), octant_of(q));
                // The ground under the cell, lit as deep as the cell.
                let under = m.canonical(state([c[0], c[1] - 1, c[2]]));
                prop_assert_eq!(q.layer(), m.table().layer(under, f));
                prop_assert_eq!(dark_of(q), dark_at(&cells, &m, level, c), "{:?}", q);
            }
        }
        let top = mesh.quads.iter().map(|q| usize::from(q.light_index())).max();
        prop_assert!(top.is_none_or(|t| t < mesh.palette.len()));
        prop_assert_eq!(mesh.hash, mesh.compute_hash());
    }

    #[test]
    fn post_pass_faces_are_never_seen_from_inside_a_drawn_node(
        scene in scene_strategy(),
        smooth in 1usize..9,
        level in 1u8..=4,
        rays in prop::collection::vec(
            ((0.0f64..32.0, 0.0f64..32.0, 0.0f64..32.0), (-1.0f64..1.0, -1.0f64..1.0, -1.0f64..1.0)),
            64,
        ),
    ) {
        let r = testing::registry();
        let m = LodMaterials::from_registry(&r);
        let cells = scene_cells(&r, &scene.0, &scene.1, smooth);
        // The node's surfaces: the mesher's faces and the bed under water.
        let (padded, mut plain) = plain_mesh(&m, &cells);
        split_octants(&mut plain);
        add_underwater(&padded, &m, level, &mut plain);
        let mut full = ChunkMesh::default();
        mesh_node(&mut Mesher::new(), &padded, &m, level, &mut full);
        // Cubes only, each opaque one in the bucket of its face and the see-through fluid's in
        // the translucent bucket, the surfaces (and banks) first, then the post-pass quads on a
        // node face, then those on a middle plane (and those planes only), which `seam_counts`
        // and `water_seams` find; open sky only.
        prop_assert_eq!(full.bucket_counts[6], 0);
        let seams = seam_counts(&full);
        for b in [0, 1, 2, 3, 4, 5, TRANSLUCENT_BUCKET] {
            let quads = full.bucket(b);
            for &q in quads {
                if b < 6 {
                    prop_assert_eq!(q.face().index(), b);
                } else {
                    prop_assert_eq!(dark_of(q), 0);
                }
                prop_assert_eq!(q.shape(), layout::SHAPE_CUBE);
                let (na, _, _) = q.face().axes();
                let n = q.pos()[na];
                match seam_of(q) {
                    Seam::None => {}
                    Seam::Face => prop_assert!(n == 0 || n == 31, "{q:?}"),
                    Seam::Middle => prop_assert!(n == 15 || n == 16, "{q:?}"),
                }
            }
            prop_assert!(quads.windows(2).all(|w| seam_of(w[0]) <= seam_of(w[1])));
            let count = |seam| quads.iter().filter(|&&q| seam_of(q) == seam).count() as u32;
            let at_end = if b < 6 { seams[b] } else { water_seams(&full) };
            prop_assert_eq!(at_end, [count(Seam::Face), count(Seam::Middle)]);
            let banks = quads.iter().filter(|&&q| is_bank(q)).count() as u32;
            prop_assert_eq!(
                quads.len() as u32 - at_end[0] - at_end[1] - banks,
                plain.bucket_counts[b],
                "the post-passes add only seam quads and banks"
            );
        }
        prop_assert!(plain.quads.iter().all(|&q| seam_of(q) == Seam::None));
        // Open sky for every light palette index the quads use, and no more.
        let top = full.quads.iter().map(|q| usize::from(q.light_index()) + 1).max();
        prop_assert_eq!(&full.palette[..], &LOD_PALETTE[..top.unwrap_or(0)]);
        // Without the banks: drawn behind everything, they show only where a ray leaves the
        // node and meets nothing past it (`banks_close_the_ground_the_cell_rule_drops`).
        let (a, b) = (
            rects(&plain, [0.0; 3], 1.0, |_| false),
            rects(&full, [0.0; 3], 1.0, is_bank),
        );
        for ((x, y, z), (dx, dy, dz)) in rays {
            // From air or from inside the see-through fluid.
            let c = [x as i32, y as i32, z as i32];
            if ground_state(&m, cells[index(c[0], c[1], c[2])]) {
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
fn a_sea_gets_its_bed_skirts_on_its_top_and_walls_under_it() {
    let r = testing::registry();
    let m = LodMaterials::from_registry(&r);
    let (solid, water) = (by(&r, "test:solid"), by(&r, "test:fluid"));
    // Ground up to cell 4, a sea up to cell 8, at level 1: 4 cells of 2 blocks over the bed.
    let cells = node_cells(|_, y, _| match y {
        ..=4 => solid,
        5..=8 => water,
        _ => BlockState::AIR,
    });
    let mut padded = PaddedChunk::new();
    m.fill(&cells, &mut padded);
    let mut mesh = ChunkMesh::default();
    mesh_node(&mut Mesher::new(), &padded, &m, 1, &mut mesh);
    // The sea's top in the translucent bucket, open sky; no other face of it.
    let water_faces: Vec<_> = mesh.bucket(TRANSLUCENT_BUCKET).to_vec();
    assert!(!water_faces.is_empty());
    for &q in &water_faces {
        assert_eq!(dark_of(q), 0);
        for (p, f) in q.unit_faces() {
            match seam_of(q) {
                // The top, and the skirts of the top cell on the 9 planes.
                Seam::None => assert_eq!((p[1], f), (8, Face::PosY), "{q:?}"),
                _ => assert_eq!(p[1], 8, "{q:?}"),
            }
        }
    }
    let top: u32 = water_faces
        .iter()
        .filter(|&&q| seam_of(q) == Seam::None)
        .map(|q| q.w() * q.h())
        .sum();
    assert_eq!(top, 32 * 32);
    // The bed: every top of the ground, 4 cells under the surface: 4 * 2 - 1 = 7 blocks, sky
    // light 8.
    let bed: Vec<_> = mesh
        .bucket(Face::PosY.index())
        .iter()
        .filter(|&&q| seam_of(q) == Seam::None)
        .copied()
        .collect();
    assert_eq!(bed.iter().map(|q| q.w() * q.h()).sum::<u32>(), 32 * 32);
    assert!(bed.iter().all(|&q| q.pos()[1] == 4 && dark_of(q) == 7));
    // On the plane x = 16: the water's skirt on its top cell, walls on the ground within 2
    // cells of it (cells 3 and 4), lit as deep as the ground across: (8 - 3 + 1) * 2 - 1.
    let on_plane = |b: usize, seam: Seam| -> BTreeSet<(u32, u8)> {
        mesh.bucket(b)
            .iter()
            .filter(|&&q| seam_of(q) == seam && q.face() == Face::PosX)
            .flat_map(|&q| q.unit_faces().map(move |(p, _)| (p, dark_of(q))))
            .filter(|(p, _)| p[0] == 15 && p[2] == 7)
            .map(|(p, dark)| (p[1], dark))
            .collect()
    };
    assert_eq!(on_plane(TRANSLUCENT_BUCKET, Seam::Middle), [(8, 0)].into());
    assert_eq!(
        on_plane(Face::PosX.index(), Seam::Middle),
        [(3, 11), (4, 9)].into()
    );
    // Deeper: 6 cells of 2 blocks (11 blocks) and more saturate at sky light 0.
    let deep = node_cells(|_, y, _| match y {
        ..=4 => solid,
        5..=20 => water,
        _ => BlockState::AIR,
    });
    m.fill(&deep, &mut padded);
    mesh_node(&mut Mesher::new(), &padded, &m, 1, &mut mesh);
    let bed = mesh.bucket(Face::PosY.index());
    assert!(
        bed.iter()
            .any(|&q| q.pos()[1] == 4 && dark_of(q) == MAX_DARK)
    );
    // An opaque fluid (lava) gets no bed, skirts on its top or walls: the mesh before water.
    let hot = by(&r, "test:hot");
    let lava = node_cells(|_, y, _| match y {
        ..=4 => solid,
        5..=8 => hot,
        _ => BlockState::AIR,
    });
    m.fill(&lava, &mut padded);
    mesh_node(&mut Mesher::new(), &padded, &m, 1, &mut mesh);
    assert_eq!(mesh.bucket_counts[TRANSLUCENT_BUCKET], 0);
    assert!(mesh.quads.iter().all(|&q| dark_of(q) == 0));
    assert!(
        mesh.bucket(Face::PosY.index())
            .iter()
            .all(|&q| seam_of(q) != Seam::None || q.pos()[1] == 8)
    );
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
/// out: its opaque quads and its see-through fluid's.
fn node_rects(
    cells: &[BlockState],
    m: &LodMaterials,
    level: u8,
    origin: [i64; 3],
    hidden: u8,
) -> (Vec<Rect>, Vec<Rect>) {
    let mut padded = PaddedChunk::new();
    m.fill(cells, &mut padded);
    let mut mesh = ChunkMesh::default();
    mesh_node(&mut Mesher::new(), &padded, m, level, &mut mesh);
    assert!(fits(&mesh));
    let cell = f64::from(1u32 << level);
    let origin = origin.map(|o| o as f64);
    let skip = |q: PackedQuad| hidden & (1 << octant_of(q)) != 0;
    (
        rects(&mesh, origin, cell, skip),
        water_rects(&mesh, origin, cell, true, skip),
    )
}

/// The top of the cells `keep` keeps of column `(cx, cz)` of a node, in blocks above its origin.
fn top_of(
    cells: &[BlockState],
    cx: i32,
    cz: i32,
    cell: f64,
    keep: impl Fn(BlockState) -> bool,
) -> f64 {
    let top = (-1..=32)
        .rev()
        .find(|&y| keep(cells[index(cx, y, cz)]))
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

/// Casts `count` rays from `start()` to `target()`; returns the rays that `fails(o, d)` (the
/// target is at `t = 1`).
fn cast(
    count: usize,
    mut ray: impl FnMut() -> Option<([f64; 3], [f64; 3])>,
    fails: impl Fn([f64; 3], [f64; 3]) -> bool,
) -> (usize, Vec<([f64; 3], [f64; 3])>) {
    let mut cast = 0;
    let mut failed = Vec::new();
    while cast < count {
        let Some((o, target)) = ray() else { continue };
        cast += 1;
        let d = [target[0] - o[0], target[1] - o[1], target[2] - o[2]];
        if fails(o, d) {
            failed.push((o, target));
        }
    }
    (cast, failed)
}

/// A ray to a point inside the ground that meets no opaque quad before it would show the sky
/// through a crack.
fn slips(opaque: &[Rect]) -> impl Fn([f64; 3], [f64; 3]) -> bool {
    move |o, d| nearest(opaque, o, d, 1.0).is_none()
}

/// A ray from above every surface to a point inside a see-through fluid that meets no opaque quad
/// before it and crosses no quad of the fluid would show the bed under it untinted.
fn untinted<'a>(opaque: &'a [Rect], water: &'a [Rect]) -> impl Fn([f64; 3], [f64; 3]) -> bool {
    move |o, d| nearest(opaque, o, d, 1.0).is_none() && !crosses(water, o, d, 1.0)
}

/// A ray from above every surface that crosses a quad of the near water and one of the far water
/// before it meets an opaque quad: two layers of water blended over what it shows. (Any number
/// of far water quads is one layer: the far water's depth prepass keeps the nearest.)
fn twice<'a>(
    opaque: &'a [Rect],
    near: &'a [Rect],
    far: &'a [Rect],
) -> impl Fn([f64; 3], [f64; 3]) -> bool {
    move |o, d| {
        let stop = nearest(opaque, o, d, 1.0).unwrap_or(1.0);
        crosses(near, o, d, stop) && crosses(far, o, d, stop)
    }
}

fn assert_none(what: &str, (cast, failed): (usize, Vec<([f64; 3], [f64; 3])>), count: usize) {
    assert_eq!(cast, count);
    assert!(
        failed.is_empty(),
        "{what}: {} of {cast} rays failed, first {:?}",
        failed.len(),
        &failed[..failed.len().min(4)]
    );
}

#[test]
fn no_ray_slips_between_two_lod_levels() {
    let r = testing::registry();
    let m = LodMaterials::from_registry(&r);
    let any = |s: BlockState| m.canonical(s) != 0;
    let ground = |s: BlockState| ground_state(&m, s);
    // Seas 11 to 18 blocks deep cross the seam at x = 64. A cell without ground is water when
    // at least half of it is: with water up to y 32 the coarse top (32) is under the fine one
    // (34), up to 33 it is over it (36 and 34: the coarse skirt closes that step), up to 30
    // and 34 they meet.
    for (basin, sea, amplitude) in [
        ((64.0, 32.0, 14.0), 32, 1.0),
        ((56.0, 24.0, 20.0), 30, 0.8),
        ((72.0, 40.0, 18.0), 34, 1.2),
        ((64.0, 32.0, 14.0), 33, 1.0),
        ((60.0, 28.0, 18.0), 33, 1.4),
    ] {
        let terrain = Terrain::new(34.0, amplitude, basin, sea);
        let fine = terrain.cells(&r, &m, 1, [0; 3]);
        let coarse = terrain.cells(&r, &m, 2, [0; 3]);
        // The fine node draws the coarse node's octant (0, 0, 0); the coarse node draws x >= 64.
        let (mut opaque, mut water) = node_rects(&fine, &m, 1, [0; 3], 0);
        let (o2, w2) = node_rects(&coarse, &m, 2, [0; 3], 0b0101_0101);
        opaque.extend(o2);
        water.extend(w2);
        let mut n = Numbers(7);
        // From above the fine surface (its water included), into the coarse ground: every ray
        // meets an opaque face (the coarse water is see-through).
        let start = |n: &mut Numbers| {
            let (x, z) = (n.between(2.0, 62.0), n.between(4.0, 60.0));
            let top = top_of(&fine, (x / 2.0) as i32, (z / 2.0) as i32, 2.0, any);
            [x, n.between(top + 0.01, top + 20.0).min(63.0), z]
        };
        let into_ground = |n: &mut Numbers| {
            let (tx, tz) = (n.between(66.0, 126.0), n.between(4.0, 60.0));
            let top = top_of(&coarse, (tx / 4.0) as i32, (tz / 4.0) as i32, 4.0, ground);
            [tx, n.between((top - 12.0).max(0.5), top - 0.01), tz]
        };
        let found = cast(
            10_000,
            || {
                let (o, t) = (start(&mut n), into_ground(&mut n));
                (o[1] < 63.0).then_some((o, t))
            },
            slips(&opaque),
        );
        assert_none(&format!("basin {basin:?}, into the ground"), found, 10_000);
        // Into the coarse water: every ray that reaches it crosses a quad of the water first
        // (the fine one's top, the coarse one's top, or the coarse skirt over the step).
        let into_water = |n: &mut Numbers| loop {
            let (tx, tz) = (n.between(66.0, 126.0), n.between(4.0, 60.0));
            let (cx, cz) = ((tx / 4.0) as i32, (tz / 4.0) as i32);
            let (bed, top) = (
                top_of(&coarse, cx, cz, 4.0, ground),
                top_of(&coarse, cx, cz, 4.0, any),
            );
            if top > bed {
                return [tx, n.between(bed + 0.01, top - 0.01), tz];
            }
        };
        let found = cast(
            10_000,
            || {
                let (o, t) = (start(&mut n), into_water(&mut n));
                (o[1] < 63.0).then_some((o, t))
            },
            untinted(&opaque, &water),
        );
        assert_none(&format!("basin {basin:?}, into the water"), found, 10_000);
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

    /// The full-detail mesh of chunk `c`, its neighbors as they are.
    fn chunk_mesh(&self, r: &BlockRegistry, c: [i64; 3]) -> ChunkMesh {
        let center = self.chunk(r, c);
        let neighbors: Vec<ChunkData> = NEIGHBOR_OFFSETS
            .iter()
            .map(|d| {
                let d = [d.x, d.y, d.z].map(i64::from);
                self.chunk(r, [c[0] + d[0], c[1] + d[1], c[2] + d[2]])
            })
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
    let any = |s: BlockState| m.canonical(s) != 0;
    let ground = |s: BlockState| ground_state(&m, s);
    // Seas 8 to 14 blocks deep cross the seam at x = 32, their beds sloping either way; the
    // ground stays inside chunk y 0. Their top blocks are even (the near surface 0.875 up the
    // lower block of its level-1 cell) or odd (up the upper one).
    for (basin, sea, amplitude) in [
        ((32.0, 16.0, 12.0), 18, 0.5),
        ((24.0, 16.0, 16.0), 18, 0.5),
        ((40.0, 16.0, 16.0), 18, 0.5),
        ((20.0, 20.0, 20.0), 24, 0.3),
        ((44.0, 10.0, 20.0), 24, 0.8),
        ((32.0, 16.0, 12.0), 17, 0.5),
        ((44.0, 10.0, 20.0), 23, 0.8),
    ] {
        let terrain = Terrain::new(20.0, amplitude, basin, sea);
        let coarse = terrain.cells(&r, &m, 1, [0; 3]);
        // Full detail draws chunk (0, 0, 0), the level-1 node's octant 0: its opaque faces stop a
        // ray, and its translucent fluid tints it.
        let near = terrain.chunk_mesh(&r, [0; 3]);
        let mut opaque = rects(&near, [0.0; 3], 1.0, |_| false);
        let near_water = water_rects(&near, [0.0; 3], 1.0, false, |_| false);
        let (o1, far_water) = node_rects(&coarse, &m, 1, [0; 3], 0b0000_0001);
        opaque.extend(o1);
        let water: Vec<Rect> = near_water.iter().chain(&far_water).copied().collect();
        let mut n = Numbers(11);
        let into_ground = |n: &mut Numbers| {
            let (tx, tz) = (n.between(33.0, 63.0), n.between(2.0, 30.0));
            let top = top_of(&coarse, (tx / 2.0) as i32, (tz / 2.0) as i32, 2.0, ground);
            [tx, n.between((top - 8.0).max(0.5), top - 0.01), tz]
        };
        let into_water = |n: &mut Numbers| loop {
            let (tx, tz) = (n.between(33.0, 63.0), n.between(2.0, 30.0));
            let (cx, cz) = ((tx / 2.0) as i32, (tz / 2.0) as i32);
            let (bed, top) = (
                top_of(&coarse, cx, cz, 2.0, ground),
                top_of(&coarse, cx, cz, 2.0, any),
            );
            if top > bed {
                return [tx, n.between(bed + 0.01, top - 0.01), tz];
            }
        };
        // From above the ground and the sea.
        let above = |n: &mut Numbers| {
            let (x, z) = (n.between(1.0, 31.0), n.between(2.0, 30.0));
            let ground = terrain.top(x as i64, z as i64) + 1;
            let top = if ground > terrain.sea {
                ground as f64
            } else {
                terrain.sea as f64 + 0.875
            };
            (top < 30.0).then(|| [x, n.between(top + 0.01, 31.0), z])
        };
        let found = cast(
            10_000,
            || Some((above(&mut n)?, into_ground(&mut n))),
            slips(&opaque),
        );
        assert_none(&format!("basin {basin:?}, from above"), found, 10_000);
        let found = cast(
            10_000,
            || Some((above(&mut n)?, into_water(&mut n))),
            untinted(&opaque, &water),
        );
        assert_none(
            &format!("basin {basin:?}, from above into the water"),
            found,
            10_000,
        );
        // One layer of water: the far water's skirt on the seam starts 0.875 up its cell, the
        // near surface over an even top block. Over an odd one the near surface lies a block
        // higher, and the skirt under it is a second layer from 0.875 up the cell to the near
        // surface (a known limit, ADR 0031: the LOD does not know the fine top in its cell).
        let (cast_n, doubled) = cast(
            10_000,
            || Some((above(&mut n)?, into_water(&mut n))),
            twice(&opaque, &near_water, &far_water),
        );
        if sea % 2 == 0 {
            assert_none(
                &format!("basin {basin:?} sea {sea}, one layer of water"),
                (cast_n, doubled),
                10_000,
            );
        } else {
            let band = (sea - 1) as f64 + 0.875..sea as f64 + 0.875;
            assert!(
                !doubled.is_empty(),
                "basin {basin:?} sea {sea}: no second layer"
            );
            for (o, t) in doubled {
                let k = (32.0 - o[0]) / (t[0] - o[0]);
                let y = o[1] + k * (t[1] - o[1]);
                assert!(band.contains(&y), "a second layer at y {y}: {o:?} {t:?}");
            }
        }
        // From under the near sea's surface.
        let found = cast(
            10_000,
            || {
                let (x, z) = (n.between(1.0, 31.0), n.between(2.0, 30.0));
                let ground = (terrain.top(x as i64, z as i64) + 1) as f64;
                let sea = terrain.sea as f64 + 0.875;
                (ground + 1.0 < sea)
                    .then(|| ([x, n.between(ground + 0.01, sea), z], into_ground(&mut n)))
            },
            slips(&opaque),
        );
        assert_none(
            &format!("basin {basin:?}, from under the sea"),
            found,
            10_000,
        );
    }
}

/// Casts rays into the near ground of full-detail chunk `chunk` (under its true top, at most 8
/// blocks) from above the level-1 terrain of `nodes` (`(origin, hidden octants)`, their water
/// included), over `x` and `z` in `starts`; asserts every ray meets an opaque face first. The
/// other way from the steady view: the full-detail region is convex around the eye, so a ray from
/// it never comes back into it, but a chunk left unmeshed or refused at the loading edge is drawn
/// by level 1 with a meshed one behind it. (Not from inside the far water: a bank on a side shows
/// only to an eye above its top.)
fn assert_far_to_near(
    what: &str,
    r: &BlockRegistry,
    m: &LodMaterials,
    terrain: &Terrain,
    chunk: [i64; 3],
    nodes: &[([i64; 3], u8)],
    starts: ([f64; 2], [f64; 2]),
) {
    let any = |s: BlockState| m.canonical(s) != 0;
    let corner = chunk.map(|c| (c * 32) as f64);
    let mut opaque = rects(&terrain.chunk_mesh(r, chunk), corner, 1.0, |_| false);
    let cells: Vec<_> = nodes
        .iter()
        .map(|&(origin, hidden)| {
            let cells = terrain.cells(r, m, 1, origin);
            opaque.extend(node_rects(&cells, m, 1, origin, hidden).0);
            (origin, cells)
        })
        .collect();
    // The far cell column under `(x, z)`: its node's cells and the column there.
    let column = |x: f64, z: f64| {
        cells.iter().find_map(|(o, cells)| {
            let (cx, cz) = ((x - o[0] as f64) / 2.0, (z - o[2] as f64) / 2.0);
            ((0.0..32.0).contains(&cx) && (0.0..32.0).contains(&cz))
                .then(|| (o[1] as f64, cells, cx as i32, cz as i32))
        })
    };
    let mut n = Numbers(13);
    let into_ground = |n: &mut Numbers| {
        let (tx, tz) = (
            n.between(corner[0] + 1.0, corner[0] + 31.0),
            n.between(corner[2] + 2.0, corner[2] + 30.0),
        );
        let top = ((terrain.top(tx as i64, tz as i64) + 1) as f64).min(corner[1] + 32.0);
        [
            tx,
            n.between((top - 8.0).max(corner[1] + 0.5), top - 0.01),
            tz,
        ]
    };
    let (xs, zs) = starts;
    let start = |n: &mut Numbers| (n.between(xs[0], xs[1]), n.between(zs[0], zs[1]));
    let above = |n: &mut Numbers| {
        let (x, z) = start(n);
        let (y0, cells, cx, cz) = column(x, z)?;
        let top = y0 + top_of(cells, cx, cz, 2.0, any);
        (top < y0 + 62.0).then(|| [x, n.between(top + 0.01, y0 + 63.0), z])
    };
    let found = cast(
        10_000,
        || Some((above(&mut n)?, into_ground(&mut n))),
        slips(&opaque),
    );
    assert_none(&format!("{what}, from above"), found, 10_000);
}

/// Banks close the ground the cell rule drops under far water (a sea over a bed in the lower
/// block of a cell keeps the cell water) where a ray through the far water crosses into full
/// detail: across a middle plane of the level-1 node, across a node face, and down through a
/// y plane onto a hidden chunk under the far water.
#[test]
fn banks_close_the_ground_the_cell_rule_drops() {
    let r = testing::registry();
    let m = LodMaterials::from_registry(&r);
    // Seas 8 to 14 blocks deep (even and odd tops) across x = 32, their beds sloping either way.
    for (basin, sea, amplitude) in [
        ((32.0, 16.0, 12.0), 18, 0.5),
        ((24.0, 16.0, 16.0), 18, 0.5),
        ((40.0, 16.0, 16.0), 18, 0.5),
        ((20.0, 20.0, 20.0), 24, 0.3),
        ((44.0, 10.0, 20.0), 24, 0.8),
        ((32.0, 16.0, 12.0), 17, 0.5),
        ((44.0, 10.0, 20.0), 23, 0.8),
    ] {
        let terrain = Terrain::new(20.0, amplitude, basin, sea);
        // Full detail draws chunk (1, 0, 0), the node's octant 1, across its middle plane.
        assert_far_to_near(
            &format!("middle plane, basin {basin:?} sea {sea}"),
            &r,
            &m,
            &terrain,
            [1, 0, 0],
            &[([0; 3], 0b0000_0010)],
            ([1.0, 31.0], [2.0, 30.0]),
        );
        // The same seas moved 32 blocks on: full detail draws chunk (2, 0, 0), octant 0 of the
        // node at x = 64, across the face of the node at x = 0.
        let moved = Terrain::new(20.0, amplitude, (basin.0 + 32.0, basin.1, basin.2), sea);
        assert_far_to_near(
            &format!("node face, basin {basin:?} sea {sea}"),
            &r,
            &m,
            &moved,
            [2, 0, 0],
            &[([0; 3], 0), ([64, 0, 0], 0b0000_0001)],
            ([33.0, 63.0], [2.0, 30.0]),
        );
    }
    // A sea over a bed around y 32: full detail draws chunk (0, 0, 0), the node's octant 0, and
    // the node the far water over it.
    for (basin, sea) in [((16.0, 16.0, 6.0), 38), ((24.0, 8.0, 8.0), 39)] {
        let terrain = Terrain::new(33.0, 0.3, basin, sea);
        assert_far_to_near(
            &format!("y plane, basin {basin:?} sea {sea}"),
            &r,
            &m,
            &terrain,
            [0, 0, 0],
            &[([0; 3], 0b0000_0001)],
            ([1.0, 63.0], [2.0, 30.0]),
        );
    }
}

/// A cutout cube holding water (ADR 0038) is ground at a distance like the dry one, with its id:
/// its water is not counted, as for a waterlogged slab.
#[test]
fn a_wet_cutout_cube_looks_like_the_dry_one_far_away() {
    use boxel_voxel::{BlockCollision, BlockDef, BlockModel, FaceTextures, Property, WATERLOGGED};
    let base = testing::registry();
    let leaves = base.face_texture(by(&base, "test:cutout"), Face::PosY);
    let mut b = testing::builder();
    let def = BlockDef {
        name: "test:wet_cube".into(),
        shape: RenderShape::Cutout,
        model: BlockModel::CUBE,
        collision: BlockCollision::Full,
        emission: [0; 3],
        light_opacity: 1,
        textures: FaceTextures::all(leaves),
    };
    b.add_block_with_states(def, vec![Property::boolean(WATERLOGGED)], |_, _| {})
        .expect("test:wet_cube");
    let r = b.build();
    let wet = by(&r, "test:wet_cube[waterlogged=true]");
    let dry = by(&r, "test:wet_cube[waterlogged=false]");
    assert!(r.is_waterlogged(wet));
    let classes = boxel_voxel::lod::LodClasses::from_registry(&r);
    assert_eq!(classes.class(wet), LodClass::Solid);
    assert_eq!(classes.fluid(wet), None);
    let m = LodMaterials::from_registry(&r);
    assert_eq!(m.canonical(wet), m.canonical(dry));
    assert_eq!(m.canonical(dry), m.canonical(by(&r, "test:cutout")));
    assert_eq!(m.class(m.canonical(wet)), LodClass::Solid);
}
