//! The far level of detail's generator side (ADR 0031): the defaulted hook and the types a
//! `LodSource` fills.
use boxel_voxel::{BlockRegistry, BlockRegistryBuilder, BlockState, ChunkData, ChunkKey, ChunkPos};
use boxel_voxel::{DimensionId, MAX_CHUNK_COORD};
use boxel_worldgen::lod::{
    LOD_PADDED, LOD_PADDED_VOLUME, LodCells, LodColumn, LodNode, LodRequest, LodScratch, LodSource,
};
use boxel_worldgen::{FlatGenerator, WorldGenerator};

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
    assert!(FlatGenerator::new(vec![]).lod().is_none());
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
