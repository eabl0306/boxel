//! The far level of detail's generator side (ADR 0031): the defaulted hook and the types a
//! `LodSource` fills.
use boxel_voxel::{BlockRegistry, BlockRegistryBuilder, BlockState, ChunkData, ChunkKey, ChunkPos};
use boxel_voxel::{DimensionId, LocalPos, MAX_CHUNK_COORD};
use boxel_worldgen::lod::{
    CellPartial, LOD_PADDED, LOD_PADDED_VOLUME, LodCells, LodClasses, LodColumn, LodNode,
    LodRequest, LodScratch, LodSource,
};
use boxel_worldgen::{FlatGenerator, WorldGenerator};
use std::collections::HashMap;

/// A generator without a coarse source.
struct Plain;

impl WorldGenerator for Plain {
    fn generate(&self, _: ChunkKey, _: &BlockRegistry) -> ChunkData {
        ChunkData::uniform(BlockState::AIR)
    }
    fn name(&self) -> &str {
        "plain"
    }
}

/// A coarse source of a flat ground whose top cell row is at block y 0: layer -1 holds it (its
/// cells, as the band), everything below is `GROUND`.
struct Ground;

const GROUND: BlockState = BlockState(1);

impl LodSource for Ground {
    fn column(
        &self,
        req: &LodRequest,
        _: &BlockRegistry,
        scratch: &mut LodScratch,
        emit: &mut dyn FnMut(i64, LodNode<'_>),
    ) -> LodColumn {
        let band = LodColumn {
            lo: -1,
            hi: 0,
            below: GROUND,
        };
        for layer in req.layers.clone() {
            if band.uniform_at(layer).is_some() {
                continue;
            }
            // The top border row (y = 32) is the layer above: air.
            scratch.cells.fill(GROUND);
            for x in -1..=32 {
                for z in -1..=32 {
                    scratch.cells.set(x, 32, z, BlockState::AIR);
                }
            }
            emit(layer, LodNode::Cells(&scratch.cells));
        }
        band
    }
    fn version(&self) -> u32 {
        3
    }
}

impl WorldGenerator for Ground {
    fn generate(&self, _: ChunkKey, _: &BlockRegistry) -> ChunkData {
        ChunkData::uniform(BlockState::AIR)
    }
    fn lod(&self) -> Option<&dyn LodSource> {
        Some(self)
    }
    fn name(&self) -> &str {
        "ground"
    }
}

#[test]
fn the_default_generator_has_no_coarse_source() {
    assert!(Plain.lod().is_none());
    // The flat generator has one: its exact cells are the engine's test oracle.
    assert!(FlatGenerator::new(vec![]).lod().is_some());
    let boxed: Box<dyn WorldGenerator> = Box::new(Plain);
    assert!(boxed.lod().is_none());
}

#[test]
fn a_generator_hands_out_its_source() {
    let g: Box<dyn WorldGenerator> = Box::new(Ground);
    let source = g.lod().expect("a coarse source");
    assert_eq!(source.version(), 3);
    let registry = BlockRegistryBuilder::new().build();
    let req = LodRequest {
        dim: DimensionId(0),
        level: 2,
        column: [5, -7],
        layers: -3..=2,
    };
    let mut scratch = LodScratch::default();
    let mut emitted = Vec::new();
    let band = source.column(&req, &registry, &mut scratch, &mut |layer, node| {
        let cells = match node {
            LodNode::Cells(cells) => cells.clone(),
            LodNode::Uniform(_) => unreachable!("Ground emits cells"),
        };
        emitted.push((layer, cells));
    });
    assert_eq!(band.lo, -1);
    assert_eq!(emitted.len(), 1, "only the band's layer");
    let (layer, cells) = &emitted[0];
    assert_eq!(*layer, -1);
    assert_eq!(cells.get(10, 31, 10), GROUND);
    assert_eq!(cells.get(-1, 32, 32), BlockState::AIR);
    for layer in [-3, -2] {
        assert_eq!(band.uniform_at(layer), Some(GROUND));
    }
    for layer in [0, 1, 2] {
        assert_eq!(band.uniform_at(layer), Some(BlockState::AIR));
    }
    assert_eq!(band.uniform_at(-1), None);
    assert_eq!(LodColumn::AIR.uniform_at(0), Some(BlockState::AIR));
    assert_eq!(LodColumn::AIR.uniform_at(-5), Some(BlockState::AIR));
}

#[test]
fn cells_use_the_padded_order() {
    // The mesher's padded layout: z fastest, then x, then y, each shifted by the border.
    assert_eq!(LodCells::index(-1, -1, -1), 0);
    assert_eq!(LodCells::index(-1, -1, 0), 1);
    assert_eq!(LodCells::index(0, -1, -1), LOD_PADDED);
    assert_eq!(LodCells::index(-1, 0, -1), LOD_PADDED * LOD_PADDED);
    assert_eq!(LodCells::index(32, 32, 32), LOD_PADDED_VOLUME - 1);
    let mut cells = LodCells::new();
    assert_eq!(cells.uniform(), Some(BlockState::AIR));
    cells.set(7, 0, 31, GROUND);
    assert_eq!(cells.get(7, 0, 31), GROUND);
    assert_eq!(cells.0[LodCells::index(7, 0, 31)], GROUND);
    assert_eq!(cells.uniform(), None);
    cells.fill(GROUND);
    assert_eq!(cells.uniform(), Some(GROUND));
}

#[test]
fn requests_place_nodes_at_any_distance() {
    let at = |level: u8, column: [i64; 2]| LodRequest {
        dim: DimensionId(1),
        level,
        column,
        layers: 0..=0,
    };
    let near = at(1, [3, -2]);
    assert_eq!(near.cell_size(), 2);
    assert_eq!(near.node_size(), 64);
    assert_eq!(near.origin(-1), [192, -64, -128]);
    assert_eq!(
        near.key(4),
        ChunkKey {
            dim: DimensionId(1),
            lod: 1,
            pos: ChunkPos::new(3, 4, -2),
        }
    );
    // At the world limit of each level the origin is exact.
    for level in 1..=6u8 {
        let edge = MAX_CHUNK_COORD >> level;
        let req = at(level, [edge, -edge]);
        let origin = req.origin(edge);
        let shift = 5 + u32::from(level);
        assert_eq!(origin[0] >> shift, edge, "level {level}");
        assert_eq!(origin[1] >> shift, edge, "level {level}");
        assert_eq!(origin[2] >> shift, -edge, "level {level}");
        assert_eq!(origin[0], MAX_CHUNK_COORD << 5, "level {level}");
        assert_eq!(req.node_size(), 32 << level);
    }
}

/// The cell rule applied to `g`'s own chunks (each level-`k <= 5` cell lies in one chunk).
fn exact_cell(
    g: &FlatGenerator,
    registry: &BlockRegistry,
    classes: &LodClasses,
    chunks: &mut HashMap<ChunkPos, ChunkData>,
    origin: [i64; 3],
    s: i64,
) -> BlockState {
    let chunk = ChunkPos::new(origin[0] >> 5, origin[1] >> 5, origin[2] >> 5);
    let data = chunks
        .entry(chunk)
        .or_insert_with(|| g.generate(ChunkKey::new(chunk), registry));
    let local = origin.map(|v| (v & 31) as u32);
    CellPartial::of_cell(classes, origin, s as u32, |x, y, z| {
        data.get(LocalPos::new(local[0] + x, local[1] + y, local[2] + z))
    })
    .state()
}

#[test]
fn the_flat_generators_cells_are_the_rule_on_its_own_chunks() {
    let registry = boxel_voxel::testing::registry();
    let classes = LodClasses::from_registry(&registry);
    let layered = |layers: &[(&str, u32)]| {
        FlatGenerator::new(layers.iter().map(|&(n, h)| (n.to_string(), h)).collect())
    };
    let generators = [
        // Every class, thin layers.
        layered(&[
            ("test:solid", 1),
            ("test:slab", 2),
            ("test:cutout", 3),
            ("test:fluid", 3),
            ("test:glass", 1),
            ("test:plant", 1),
        ]),
        // A sea 9 deep over ground 37 high, then one 3 deep over 70 (fluid over ground).
        layered(&[("test:solid", 37), ("test:fluid", 9)]),
        layered(&[("test:solid", 70), ("test:fluid", 3)]),
        // Two fluid kinds over ground, and a fluid with nothing under it.
        layered(&[("test:solid", 100), ("test:hot", 2), ("test:fluid", 20)]),
        layered(&[("test:fluid", 15), ("test:solid", 1)]),
        layered(&[]),
    ];
    // Far columns too: the generator is the same everywhere.
    let columns = [[0, 0], [-3, 5], [1 << 20, -(1 << 20)]];
    for (i, g) in generators.iter().enumerate() {
        let source = g.lod().expect("a source");
        let mut chunks = HashMap::new();
        let mut scratch = LodScratch::default();
        for level in 1..=5u8 {
            for column in columns {
                let req = LodRequest {
                    dim: DimensionId(0),
                    level,
                    column,
                    layers: -4..=6,
                };
                let s = req.cell_size();
                let mut emitted: HashMap<i64, Vec<BlockState>> = HashMap::new();
                let band = source.column(&req, &registry, &mut scratch, &mut |layer, node| {
                    let cells = match node {
                        LodNode::Uniform(state) => vec![state; LOD_PADDED_VOLUME],
                        LodNode::Cells(cells) => cells.0.to_vec(),
                    };
                    assert!(
                        emitted.insert(layer, cells).is_none(),
                        "layer {layer} twice"
                    );
                });
                for layer in req.layers.clone() {
                    let o = req.origin(layer);
                    let got = emitted.get(&layer);
                    assert_eq!(
                        got.is_some(),
                        band.uniform_at(layer).is_none(),
                        "generator {i} level {level} layer {layer}: emitted inside the band only"
                    );
                    // Three cell columns of every row: the layers are flat.
                    for [x, z] in [[-1, -1], [13, 20], [32, 32]] {
                        for y in -1..=32i32 {
                            let at = [
                                o[0] + i64::from(x) * s,
                                o[1] + i64::from(y) * s,
                                o[2] + i64::from(z) * s,
                            ];
                            let want = exact_cell(g, &registry, &classes, &mut chunks, at, s);
                            let have = match got {
                                Some(cells) => cells[LodCells::index(x, y, z)],
                                None => band.uniform_at(layer).expect("outside the band"),
                            };
                            assert_eq!(
                                have, want,
                                "generator {i} level {level} layer {layer} cell {x} {y} {z}"
                            );
                        }
                    }
                }
            }
        }
    }
}
