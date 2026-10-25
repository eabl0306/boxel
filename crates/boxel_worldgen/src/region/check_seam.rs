//! The seam harness of a realm's far LOD (feature `test-support`; S1 of the TFC study): where
//! full detail meets the level-1 cells at a chunk plane. A chunk meshes its sides against its
//! real neighbors, so where the chunk across the plane is ground the side has no face, and only
//! the coarse cells across can hide it. [`check_seam`] checks that their top stands at or over
//! every real top of the plane's edge row, and casts rays across the near/far boundary as the
//! renderer draws it (the chunks meshed with their real neighbors, the node with its skirts,
//! walls and banks and the chunks' octants hidden, as the `lod_cracks` tests of the demo do),
//! counting the rays that reach a point inside the ground without meeting a face: the sky seen
//! through the ground.
//!
//! It changes nothing in the generator: a game runs it on its recipe's columns, keeps the cases
//! its recipe can fix out of its far forms (a dry corner lowered at level 1), and logs the ones it
//! cannot (wet slivers, cave voids at the seam) for the engine.

use super::check_lod::{Coarse, Node};
use super::generator::RegionGenerator;
use super::kind::Context;
use crate::WorldGenerator;
use crate::lod::{LodClass, LodClasses};
use crate::noise::splitmix64;
use boxel_mesh::lod::{LodMaterials, is_bank, mesh_node, octant_of};
use boxel_mesh::{
    ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher, PackedQuad, PaddedChunk, layout,
};
use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_SHIFT, ChunkData, ChunkKey, ChunkPos, LocalPos,
    NEIGHBOR_OFFSETS,
};
use std::collections::{HashMap, HashSet};
use std::fmt;

/// The level the seam checks: full detail meets level 1 at the loading edge.
const LEVEL: u8 = 1;

/// A level-1 cell, in blocks.
const CELL: i64 = 1 << LEVEL;

/// A level-1 node column, in blocks along x and z, and a node layer along y.
const NODE: i64 = 32 << LEVEL;

/// At most this many failures of each class are listed.
const MAX_LISTED: usize = 12;

/// What [`check_seam`] builds and how many rays it casts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeamOptions {
    /// Rays of each kind per plane: from over the chunk into the coarse ground across, from over
    /// the coarse terrain into the chunk's ground, from under the chunk's fluid into the coarse
    /// ground, and (as drawn) from the chunk's caves near the plane into the coarse ground, on
    /// the two planes of each chunk column inside its node.
    pub rays: usize,
    /// Whether the rays meet the chunks and the cells as drawn: the chunks with their carvers
    /// and feature stages, the cells with the features' runs, and rays from the chunks' caves
    /// (caves at the seam). Off, the terrain alone, as `check_lod` compares it (the fill, the
    /// fluid and the surface).
    pub drawn: bool,
}

impl Default for SeamOptions {
    /// 1,000 rays of each kind per plane, the terrain alone.
    fn default() -> Self {
        Self {
            rays: 1000,
            drawn: false,
        }
    }
}

/// What [`check_seam`] compared and found. A sound far form leaves [`SeamReport::lowered`] and
/// [`SeamReport::sky_hits`] empty; [`SeamReport::wet_slivers`] is the class the engine's wet rule
/// owns (a cell is wet where the real ground of its edge row is dry).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeamReport {
    /// Level-1 cell columns compared on the chunk planes (each once).
    pub cells: usize,
    /// Real columns of their edge rows compared.
    pub columns: usize,
    /// Of them, wet in both: a fluid top over the real column and over its cell (the fluids
    /// meet, nothing to hide).
    pub wet_in_both: usize,
    /// Edge-row columns whose real top stands over the solid top of their cell, which is dry: a
    /// coarse form under the real ground (a dry corner lowered at level 1).
    pub lowered: Vec<String>,
    /// Edge-row columns whose real dry top stands over the ground of their cell, which is wet:
    /// the cell's corners lie under the fluid's level and the real ground between them over it.
    pub wet_slivers: Vec<String>,
    /// Rays cast across the near/far boundary.
    pub rays: usize,
    /// The rays that reached a point inside the ground without meeting a face, with their kind,
    /// start and target.
    pub sky_hits: Vec<String>,
}

impl SeamReport {
    /// How many failures it holds, of every class.
    pub fn failures(&self) -> usize {
        self.lowered.len() + self.wet_slivers.len() + self.sky_hits.len()
    }

    /// The report, or its text if it holds a failure of any class.
    ///
    /// # Errors
    ///
    /// The report's text ([`fmt::Display`]) when [`SeamReport::failures`] is not 0.
    pub fn check(self) -> Result<Self, String> {
        if self.failures() == 0 {
            Ok(self)
        } else {
            Err(self.to_string())
        }
    }
}

impl fmt::Display for SeamReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "check_seam: {} cells, {} edge columns ({} wet in both), {} rays; {} lowered, {} wet \
             slivers, {} sky hits",
            self.cells,
            self.columns,
            self.wet_in_both,
            self.rays,
            self.lowered.len(),
            self.wet_slivers.len(),
            self.sky_hits.len(),
        )?;
        for (class, list) in [
            ("lowered", &self.lowered),
            ("wet sliver", &self.wet_slivers),
            ("sky hit", &self.sky_hits),
        ] {
            for line in list.iter().take(MAX_LISTED) {
                write!(f, "\n- {class}: {line}")?;
            }
        }
        Ok(())
    }
}

/// Checks the seam between full detail and the level-1 far LOD of `generator` around the chunk
/// columns `chunks` (in chunks):
///
/// - on the 4 planes of each chunk column, every level-1 cell column on either side (its edge
///   row: its 2 columns next to the plane, the ones a chunk across meshes its side against): its
///   coarse solid top stands at or over every real top of its edge row (a dry one; a wet one
///   over a dry cell), wet in both excepted. The real terrain is the chunks without carvers and
///   features ([`RegionGenerator::generate_terrain`]) and the cells the pass's terrain alone, as
///   `check_lod` compares them;
/// - with the chunk column at full detail and its level-1 node column coarse (the chunk
///   column's octants hidden), on the node's two planes the chunk column shares with the
///   octants across: [`SeamOptions::rays`] rays of each kind from over the chunks' terrain into
///   the coarse ground across, from over the coarse terrain into the chunks' ground, from under
///   the chunks' fluid into the coarse ground, and with the chunks as drawn
///   ([`SeamOptions::drawn`]) from their caves near the plane into the coarse ground. A ray that
///   meets no opaque face before its target is a sky hit: the renderer would show the sky (or
///   the cave behind) through the ground there. The chunks' opaque blocks are what a ray aims
///   under: a see-through top (glass) the far LOD draws solid shows as sky hits.
///
/// Returns what it compared and every failure, by class ([`SeamReport::check`] turns any into an
/// error); the rays are the same on every run.
pub fn check_seam<Cx: Context>(
    generator: &RegionGenerator<Cx>,
    registry: &BlockRegistry,
    chunks: &[(i64, i64)],
    options: SeamOptions,
) -> SeamReport {
    let classes = LodClasses::from_registry(registry);
    let mut report = SeamReport::default();
    let mut terrain = Chunks::new(generator, registry, false);
    let mut nodes: HashMap<[i64; 2], Tops> = HashMap::new();
    let mut seen = HashSet::new();
    for &(cx, cz) in chunks {
        let (x0, z0) = (cx << CHUNK_SHIFT, cz << CHUNK_SHIFT);
        for (axis, plane) in [(0, x0), (0, x0 + 32), (1, z0), (1, z0 + 32)] {
            let along = if axis == 0 { z0 } else { x0 };
            // The cells on either side: from the plane on (edge column `plane`), and the ones
            // ending at it (edge column `plane - 1`).
            for (cell, edge) in [(plane, plane), (plane - CELL, plane - 1)] {
                for w in (along..along + 32).step_by(CELL as usize) {
                    if !seen.insert((axis, cell, w)) {
                        continue;
                    }
                    let (x, z) = if axis == 0 { (cell, w) } else { (w, cell) };
                    let node = [x.div_euclid(NODE), z.div_euclid(NODE)];
                    let tops = nodes.entry(node).or_insert_with(|| {
                        Tops::of(&Coarse::terrain(generator, registry, LEVEL, node), &classes)
                    });
                    let top = tops.at(x.rem_euclid(NODE) / CELL, z.rem_euclid(NODE) / CELL);
                    report.cells += 1;
                    for dw in 0..CELL {
                        let (ex, ez) = if axis == 0 {
                            (edge, w + dw)
                        } else {
                            (w + dw, edge)
                        };
                        edge_column(&mut report, &mut terrain, &classes, (ex, ez), top);
                    }
                }
            }
        }
    }
    let materials = (
        LodMaterials::from_registry(registry),
        MaterialTable::from_registry(registry),
    );
    let mut drawn = Chunks::new(generator, registry, true);
    for &(cx, cz) in chunks {
        let near = if options.drawn {
            &mut drawn
        } else {
            &mut terrain
        };
        scene(generator, (cx, cz), options, &classes, &materials, near).cast(&mut report);
    }
    report
}

/// One edge-row column `(x, z)` against the top of its cell.
fn edge_column<Cx: Context>(
    report: &mut SeamReport,
    terrain: &mut Chunks<'_, Cx>,
    classes: &LodClasses,
    (x, z): (i64, i64),
    cell: CellTop,
) {
    let (y, class) = terrain.top(classes, x, z);
    report.columns += 1;
    let real = y + 1;
    if class == LodClass::Fluid && cell.wet {
        report.wet_in_both += 1;
    } else if cell.ground < real {
        let what = if class == LodClass::Fluid {
            "fluid"
        } else {
            "dry"
        };
        let line = format!(
            "column ({x}, {z}): its {what} top {real} stands over its cell's solid top {}{}",
            cell.ground,
            if cell.wet { " under its fluid" } else { "" },
        );
        if cell.wet && class != LodClass::Fluid {
            report.wet_slivers.push(line);
        } else {
            report.lowered.push(line);
        }
    }
}

/// The top of a level-1 cell column, in blocks: the block over its highest Solid cell, and
/// whether its highest non-air cell is a fluid's (a node that draws nothing counts as ground).
#[derive(Clone, Copy, Debug)]
struct CellTop {
    /// The block over its highest Solid cell.
    ground: i64,
    /// The block over its highest non-air cell.
    top: i64,
    /// Whether that cell is a fluid's.
    wet: bool,
}

/// The tops of the 32 x 32 cell columns of a level-1 node column.
struct Tops(Vec<CellTop>);

impl Tops {
    fn of(coarse: &Coarse, classes: &LodClasses) -> Self {
        let band = coarse.band;
        let mut tops = Vec::with_capacity(32 * 32);
        for x in 0..32 {
            for z in 0..32 {
                let (mut top, mut wet, mut ground) = (None, false, None);
                'scan: for layer in (band.lo..band.hi).rev() {
                    for y in (0..32).rev() {
                        let class = coarse
                            .get(layer, x, y, z)
                            .map_or(LodClass::Solid, |s| classes.class(s));
                        let over = layer * NODE + (i64::from(y) + 1) * CELL;
                        if class != LodClass::Empty && top.is_none() {
                            top = Some(over);
                            wet = class == LodClass::Fluid;
                        }
                        if class == LodClass::Solid {
                            ground = Some(over);
                            break 'scan;
                        }
                    }
                }
                let floor = band.lo * NODE;
                tops.push(CellTop {
                    ground: ground.unwrap_or(floor),
                    top: top.unwrap_or(floor),
                    wet,
                });
            }
        }
        Self(tops)
    }

    /// Cell column `(x, z)` (`0..32` each).
    fn at(&self, x: i64, z: i64) -> CellTop {
        self.0[(x * 32 + z) as usize]
    }
}

/// The chunks of a generator, each generated once: the terrain alone
/// ([`RegionGenerator::generate_terrain`]) or as drawn ([`WorldGenerator::generate`]).
struct Chunks<'a, Cx: Context> {
    generator: &'a RegionGenerator<Cx>,
    registry: &'a BlockRegistry,
    drawn: bool,
    chunks: HashMap<[i64; 3], ChunkData>,
    /// Per block column, its highest non-Empty block and that block's class.
    tops: HashMap<(i64, i64), (i64, LodClass)>,
    top_layer: i64,
}

impl<'a, Cx: Context> Chunks<'a, Cx> {
    fn new(generator: &'a RegionGenerator<Cx>, registry: &'a BlockRegistry, drawn: bool) -> Self {
        Self {
            generator,
            registry,
            drawn,
            chunks: HashMap::new(),
            tops: HashMap::new(),
            top_layer: *generator.vertical_chunks().end(),
        }
    }

    fn chunk(&mut self, at: [i64; 3]) -> &ChunkData {
        let (generator, registry, drawn) = (self.generator, self.registry, self.drawn);
        self.chunks.entry(at).or_insert_with(|| {
            let key = ChunkKey::new(ChunkPos::new(at[0], at[1], at[2]));
            if drawn {
                generator.generate(key, registry)
            } else {
                generator.generate_terrain(key, registry)
            }
        })
    }

    fn block(&mut self, [x, y, z]: [i64; 3]) -> BlockState {
        let chunk = self.chunk([x >> CHUNK_SHIFT, y >> CHUNK_SHIFT, z >> CHUNK_SHIFT]);
        chunk.get(LocalPos::new(
            (x & 31) as u32,
            (y & 31) as u32,
            (z & 31) as u32,
        ))
    }

    /// The highest non-Empty block of column `(x, z)` and its class.
    fn top(&mut self, classes: &LodClasses, x: i64, z: i64) -> (i64, LodClass) {
        if let Some(&top) = self.tops.get(&(x, z)) {
            return top;
        }
        let mut found = None;
        'chunks: for layer in (self.top_layer - 4096..=self.top_layer).rev() {
            let chunk = self.chunk([x >> CHUNK_SHIFT, layer, z >> CHUNK_SHIFT]);
            if chunk.as_uniform() == Some(BlockState::AIR) {
                continue;
            }
            for ly in (0..32u32).rev() {
                let state = chunk.get(LocalPos::new((x & 31) as u32, ly, (z & 31) as u32));
                let class = classes.class(state);
                if class != LodClass::Empty {
                    found = Some(((layer << CHUNK_SHIFT) + i64::from(ly), class));
                    break 'chunks;
                }
            }
        }
        let top = found.unwrap_or_else(|| panic!("check_seam: no ground in column ({x}, {z})"));
        self.tops.insert((x, z), top);
        top
    }

    /// The highest block of column `(x, z)` that full detail draws opaque (an occluding cube:
    /// not a cutout, a glass or a fluid), under the rest.
    fn opaque_top(&mut self, classes: &LodClasses, x: i64, z: i64) -> i64 {
        let (top, _) = self.top(classes, x, z);
        let registry = self.registry;
        (top - (4096 << CHUNK_SHIFT)..=top)
            .rev()
            .find(|&y| registry.occludes(self.block([x, y, z])))
            .unwrap_or_else(|| panic!("check_seam: no opaque block under ({x}, {top}, {z})"))
    }

    /// The mesh of chunk `c` with its neighbors, as full detail draws it.
    fn mesh(&mut self, c: [i64; 3], table: &MaterialTable) -> ChunkMesh {
        let at = |d: [i64; 3]| [c[0] + d[0], c[1] + d[1], c[2] + d[2]];
        let offsets: Vec<[i64; 3]> = NEIGHBOR_OFFSETS
            .iter()
            .map(|d| [d.x, d.y, d.z].map(i64::from))
            .collect();
        let _ = self.chunk(c);
        for &d in &offsets {
            let _ = self.chunk(at(d));
        }
        let center = &self.chunks[&c];
        let neighbors: [Option<&ChunkData>; 26] =
            std::array::from_fn(|i| self.chunks.get(&at(offsets[i])));
        let input = MeshInput {
            center,
            neighbors,
            light: None,
        };
        Mesher::new().mesh_chunk(&input, table, MeshOptions::default())
    }
}

/// A quad as a rectangle in space: its face's axis, plane and direction, and its extent along
/// the two other axes; for a LOD bank, how high the eye must be for it to be drawn. The model of
/// the demo's `lod_cracks` tests.
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
    /// `q` of a mesh whose cell `(0, 0, 0)` starts at `origin`, cells `cell` blocks wide. A fluid
    /// surface quad of full detail has its top `FLUID_SURFACE` up its cell. A LOD bank lies on
    /// its cell's side away from its normal, and one on a side is drawn only for an eye above its
    /// top; drawn behind everything, it stops a ray only where nothing else would.
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
            (true, true) => f64::from(layout::FLUID_SURFACE),
            (true, false) => 1.0,
            _ => 0.0,
        };
        let span = |a: usize, len: u32| {
            let len = if surface && a == 1 {
                f64::from(layout::FLUID_SURFACE)
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
        let t = (self.plane - o[self.n]) / d[self.n];
        if t <= 1e-9 {
            return None;
        }
        let at = |a: usize| o[a] + t * d[a];
        let inside = |(a, lo, hi): (usize, f64, f64)| (lo..=hi).contains(&at(a));
        (inside(self.u) && inside(self.v)).then_some(t)
    }
}

/// The opaque quads of `mesh` (its face buckets, not the cutout or the translucent one), those
/// `skip` keeps out left out, as rectangles.
fn opaque(
    mesh: &ChunkMesh,
    origin: [f64; 3],
    cell: f64,
    skip: impl Fn(PackedQuad) -> bool,
    out: &mut Vec<Rect>,
) {
    for b in 0..6 {
        let quads = mesh.bucket(b).iter().copied().filter(|&q| !skip(q));
        out.extend(quads.map(|q| Rect::of(q, origin, cell)));
    }
}

/// A fixed sequence of numbers in `0..1` (splitmix64).
struct Numbers(u64);

impl Numbers {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        (splitmix64(self.0) >> 11) as f64 / (1u64 << 53) as f64
    }

    fn between(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next()
    }
}

/// A chunk column at full detail in its level-1 node column: the opaque faces of both, and the
/// tops the rays start over and aim under, in blocks from the scene's origin (the node column's
/// lowest corner at the band's bottom).
struct Scene {
    /// The chunk column, in chunks, for messages.
    column: (i64, i64),
    /// The chunk column's octant in the node, along x and z (0 or 1).
    octant: [i64; 2],
    rays: usize,
    faces: Vec<Rect>,
    /// Per block column of the node column (`x * 64 + z`, from its lowest corner): the top of
    /// the chunk column's terrain (the block over its highest non-Empty block, or its fluid's
    /// surface), the block over its highest opaque one (what a ray aims under), and whether a
    /// fluid lies over it.
    near: Vec<(f64, f64, bool)>,
    /// Per level-1 cell column: the coarse top and ground, from the scene's origin.
    coarse: Vec<(f64, f64)>,
    /// With the chunks as drawn, the voxels of the chunk column under its opaque top (at most
    /// 48 deep) that are not opaque: its caves, the rays of the fourth kind start in them.
    voids: Vec<[f64; 3]>,
}

/// The scene of chunk column `(cx, cz)`.
fn scene<Cx: Context>(
    generator: &RegionGenerator<Cx>,
    (cx, cz): (i64, i64),
    options: SeamOptions,
    classes: &LodClasses,
    (lod, table): &(LodMaterials, MaterialTable),
    near: &mut Chunks<'_, Cx>,
) -> Scene {
    let node = [cx.div_euclid(2), cz.div_euclid(2)];
    let registry = near.registry;
    let coarse = if options.drawn {
        Coarse::of(generator, registry, LEVEL, node)
    } else {
        Coarse::terrain(generator, registry, LEVEL, node)
    };
    let band = coarse.band;
    let origin = [node[0] * NODE, band.lo * NODE, node[1] * NODE];
    let at = |v: [i64; 3]| [0, 1, 2].map(|a| (v[a] - origin[a]) as f64);
    let octant = [cx.rem_euclid(2), cz.rem_euclid(2)];
    let mut faces = Vec::new();
    // The chunk column at full detail, every chunk of the band.
    for cy in band.lo * 2..band.hi * 2 {
        let mesh = near.mesh([cx, cy, cz], table);
        let o = at([cx << CHUNK_SHIFT, cy << CHUNK_SHIFT, cz << CHUNK_SHIFT]);
        opaque(&mesh, o, 1.0, |_| false, &mut faces);
    }
    // The node column coarse, the chunk column's octants hidden.
    let low = octant[0] | (octant[1] << 2);
    let hidden = (1u8 << low) | (1u8 << (low | 2));
    let mut padded = PaddedChunk::new();
    for layer in band.lo..band.hi {
        let Some(Node::Cells(cells)) = coarse.nodes.get(&layer) else {
            continue;
        };
        lod.fill(&cells.0[..], &mut padded);
        let mut mesh = ChunkMesh::default();
        mesh_node(&mut Mesher::new(), &padded, lod, LEVEL, &mut mesh);
        let o = at([node[0] * NODE, layer * NODE, node[1] * NODE]);
        opaque(
            &mesh,
            o,
            CELL as f64,
            |q| hidden & (1 << octant_of(q)) != 0,
            &mut faces,
        );
    }
    let surface = f64::from(layout::FLUID_SURFACE);
    let mut tops = Vec::with_capacity((NODE * NODE) as usize);
    for x in 0..NODE {
        for z in 0..NODE {
            let (wx, wz) = (origin[0] + x, origin[2] + z);
            let (y, class) = near.top(classes, wx, wz);
            let ground = (near.opaque_top(classes, wx, wz) + 1 - origin[1]) as f64;
            let wet = class == LodClass::Fluid;
            let top = (y - origin[1]) as f64 + if wet { surface } else { 1.0 };
            tops.push((top, ground, wet));
        }
    }
    let mut voids = Vec::new();
    if options.drawn {
        let (x0, z0) = (cx << CHUNK_SHIFT, cz << CHUNK_SHIFT);
        for wx in x0..x0 + 32 {
            for wz in z0..z0 + 32 {
                let top = near.opaque_top(classes, wx, wz);
                for y in (top - 48).max(origin[1])..top {
                    if !registry.occludes(near.block([wx, y, wz])) {
                        voids.push(at([wx, y, wz]));
                    }
                }
            }
        }
    }
    let cells = Tops::of(&coarse, classes);
    let coarse_tops = cells
        .0
        .iter()
        .map(|c| ((c.top - origin[1]) as f64, (c.ground - origin[1]) as f64))
        .collect();
    Scene {
        column: (cx, cz),
        octant,
        rays: options.rays,
        faces,
        near: tops,
        coarse: coarse_tops,
        voids,
    }
}

impl Scene {
    /// Whether a ray from `o` to a point inside the ground at `t = 1` meets no opaque face first.
    fn slips(&self, o: [f64; 3], d: [f64; 3]) -> bool {
        !self
            .faces
            .iter()
            .filter_map(|r| r.hit(o, d))
            .any(|t| t <= 1.0)
    }

    /// The chunk column's top, ground and fluid at `(x, z)` (from the scene's origin).
    fn near_at(&self, x: f64, z: f64) -> (f64, f64, bool) {
        self.near[(x as usize) * NODE as usize + z as usize]
    }

    /// The coarse top and ground at `(x, z)`.
    fn coarse_at(&self, x: f64, z: f64) -> (f64, f64) {
        let c = CELL as f64;
        self.coarse[((x / c) as usize) * 32 + (z / c) as usize]
    }

    /// Casts the rays of each kind on both planes the chunk column shares with the octants
    /// across, into `report`.
    fn cast(&self, report: &mut SeamReport) {
        let seed = splitmix64(self.column.0 as u64 ^ splitmix64(self.column.1 as u64));
        for axis in 0..2 {
            // Across the plane (u) and along it (w): x and z for the plane x = 32, the other way
            // for z = 32.
            let (o_u, o_w) = (self.octant[axis] as f64, self.octant[1 - axis] as f64);
            let near_u = (32.0 * o_u + 1.0, 32.0 * o_u + 31.0);
            let far_u = (32.0 * (1.0 - o_u) + 1.0, 32.0 * (1.0 - o_u) + 31.0);
            let along = (32.0 * o_w + 2.0, 32.0 * o_w + 30.0);
            let xz = |u: f64, w: f64| if axis == 0 { (u, w) } else { (w, u) };
            let mut n = Numbers(seed ^ axis as u64);
            let point = |n: &mut Numbers, (lo, hi): (f64, f64)| {
                xz(n.between(lo, hi), n.between(along.0, along.1))
            };
            // The caves within 8 blocks of the plane.
            let plane = 32.0;
            let caves: Vec<[f64; 3]> = self
                .voids
                .iter()
                .copied()
                .filter(|v| (v[axis * 2] + 0.5 - plane).abs() < 8.0)
                .collect();
            for kind in 0..4 {
                if kind == 3 && caves.is_empty() {
                    continue;
                }
                let mut cast = 0;
                for _ in 0..self.rays * 20 {
                    if cast == self.rays {
                        break;
                    }
                    let ray = match kind {
                        // From over the chunk column into the coarse ground across.
                        0 => {
                            let (x, z) = point(&mut n, near_u);
                            let (top, ..) = self.near_at(x, z);
                            let o = [x, n.between(top + 0.01, top + 20.0), z];
                            let (tx, tz) = point(&mut n, far_u);
                            let (_, ground) = self.coarse_at(tx, tz);
                            let ty = n.between((ground - 8.0).max(0.5), ground - 0.01);
                            Some((o, [tx, ty, tz]))
                        }
                        // From over the coarse terrain into the chunk column's ground.
                        1 => {
                            let (x, z) = point(&mut n, far_u);
                            let (top, _) = self.coarse_at(x, z);
                            let o = [x, n.between(top + 0.01, top + 20.0), z];
                            let (tx, tz) = point(&mut n, near_u);
                            let (_, ground, _) = self.near_at(tx, tz);
                            let ty = n.between((ground - 8.0).max(0.5), ground - 0.01);
                            Some((o, [tx, ty, tz]))
                        }
                        // From under the chunk column's fluid into the coarse ground across.
                        2 => {
                            let (x, z) = point(&mut n, near_u);
                            let (top, ground, wet) = self.near_at(x, z);
                            let (tx, tz) = point(&mut n, far_u);
                            let (_, bed) = self.coarse_at(tx, tz);
                            let ty = n.between((bed - 8.0).max(0.5), bed - 0.01);
                            (wet && ground + 1.0 < top)
                                .then(|| ([x, n.between(ground + 0.01, top), z], [tx, ty, tz]))
                        }
                        // From a cave of the chunk column near the plane into the coarse ground
                        // across, a few blocks up or down.
                        _ => {
                            let v = caves[(n.next() * caves.len() as f64) as usize % caves.len()];
                            let o = v.map(|c| c + n.between(0.1, 0.9));
                            let (tx, tz) = point(&mut n, far_u);
                            let (_, ground) = self.coarse_at(tx, tz);
                            let (lo, hi) = ((o[1] - 4.0).max(0.5), (o[1] + 4.0).min(ground - 0.01));
                            (lo < hi).then(|| (o, [tx, n.between(lo, hi), tz]))
                        }
                    };
                    let Some((o, target)) = ray else { continue };
                    cast += 1;
                    let d = [target[0] - o[0], target[1] - o[1], target[2] - o[2]];
                    if self.slips(o, d) {
                        report.sky_hits.push(format!(
                            "chunk column {:?}, plane {}, from {}: {o:?} to {target:?} (from the \
                             node column's lowest corner at the band's bottom)",
                            self.column,
                            ["x", "z"][axis],
                            [
                                "over the chunk",
                                "over the cells",
                                "under the chunk's fluid",
                                "a cave of the chunk",
                            ][kind],
                        ));
                    }
                }
                report.rays += cast;
            }
        }
    }
}
