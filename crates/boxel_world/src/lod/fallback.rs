//! The fallback for generators without a coarse source (ADR 0031): level-1 cells built from the
//! real chunks, with the exact cell rule.
//!
//! A level-1 cell is 2 blocks wide, so a node's padded cells (one cell of border) reach 4 chunks
//! along each axis: the node's 2 and one on each side. The job reads each chunk where the world
//! has it: the loaded blocks (copied when the job is made, edits included), else the saved copy,
//! else the generator's (kept per worker, [`GENERATED_CACHE`] chunks). Only level 1 is built
//! (`LodSettings::fallback_max_level`): level 4 would read about 1,300 chunks per column.
//!
//! The band comes from the generator's [`vertical_chunks`](boxel_worldgen::WorldGenerator::vertical_chunks)
//! (every chunk outside it is air), grown to the edited chunks: a generator whose range is not
//! bounded, or spans more than [`MAX_CHUNK_LAYERS`], gets no fallback.
use super::edits::{ChunkSource, Level1, nodes_touching, widen};
use super::plan::Span;
use crate::persist::WorldStore;
use boxel_mesh::lod::LodMaterials;
use boxel_voxel::{BlockRegistry, BlockState, ChunkKey, ChunkPos, DimensionId};
use boxel_worldgen::WorldGenerator;
use boxel_worldgen::lod::{LodCells, LodColumn};
use foldhash::HashMap;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ops::RangeInclusive;
use std::sync::Arc;

/// Chunk layers a generator's vertical range may span at most for the fallback.
pub(crate) const MAX_CHUNK_LAYERS: i64 = 64;
/// Generated chunks whose level-1 cells a worker keeps.
pub(crate) const GENERATED_CACHE: usize = 64;
/// Nodes with cells a fallback job builds (each reads up to 64 chunks).
pub(crate) const NODES_PER_JOB: usize = 1;

/// The level-1 band of a generator whose chunks outside `vertical` are air, grown to `edited`
/// (the layers edited chunks reach); `None` if `vertical` is not bounded or too tall.
pub(crate) fn band(vertical: &RangeInclusive<i64>, edited: Option<Span>) -> Option<LodColumn> {
    let (a, b) = (*vertical.start(), *vertical.end());
    if a == i64::MIN || b == i64::MAX || b.saturating_sub(a) >= MAX_CHUNK_LAYERS {
        return None;
    }
    let band = if a > b {
        LodColumn::AIR
    } else {
        LodColumn {
            lo: nodes_touching(a, 1).0,
            hi: nodes_touching(b, 1).1 + 1,
            below: BlockState::AIR,
        }
    };
    Some(widen(band, edited))
}

/// True if the fallback can serve a generator of chunk range `vertical`.
pub(crate) fn serves(vertical: &RangeInclusive<i64>) -> bool {
    band(vertical, None).is_some()
}

/// The chunks the padded cells of level-1 node layers `layers` of column `[x, z]` reach.
pub(crate) fn footprint(column: [i64; 2], layers: Span) -> impl Iterator<Item = ChunkPos> {
    let span = |n: i64| (n << 1) - 1..=((n + 1) << 1);
    let (xs, zs) = (span(column[0]), span(column[1]));
    let ys = (layers.0 << 1) - 1..=((layers.1 + 1) << 1);
    ys.flat_map(move |y| {
        let zs = zs.clone();
        xs.clone()
            .flat_map(move |x| zs.clone().map(move |z| ChunkPos::new(x, y, z)))
    })
}

/// What a fallback job reads.
pub(crate) struct FallbackInput {
    /// The column's band.
    pub(crate) band: LodColumn,
    /// Where each loaded chunk of the footprint reads (the others are unloaded).
    pub(crate) chunks: HashMap<ChunkPos, ChunkSource>,
    pub(crate) store: Option<WorldStore>,
    pub(crate) materials: Arc<LodMaterials>,
    /// The world's epoch (`WorldEpoch`): a generator of another world at the same address (or
    /// the same generator with another seed) never reads this world's cached cells.
    pub(crate) epoch: u64,
}

/// A key of the generated cells: generator and registry (their addresses), the world's epoch and
/// the chunk.
type GeneratedKey = (usize, usize, u64, ChunkKey);

/// The level-1 cells of generated chunks, per worker: a short first-in first-out cache, by
/// [`GeneratedKey`].
#[derive(Default)]
struct Generated {
    cells: HashMap<GeneratedKey, Arc<Level1>>,
    order: VecDeque<GeneratedKey>,
}

thread_local! {
    static GENERATED: RefCell<Generated> = RefCell::new(Generated::default());
}

/// The level-1 cells of chunk `pos`.
fn chunk_cells(
    input: &FallbackInput,
    dim: DimensionId,
    pos: ChunkPos,
    generator: &dyn WorldGenerator,
    registry: &BlockRegistry,
) -> Arc<Level1> {
    let key = ChunkKey { dim, lod: 0, pos };
    let source = input.chunks.get(&pos).cloned();
    if let Some(source) = source {
        let data = source.read(key, input.store.as_ref(), generator, registry);
        return Arc::new(Level1::of_chunk(input.materials.classes(), &data));
    }
    if let Some(data) = input.store.as_ref().and_then(|s| s.read_chunk(key)) {
        return Arc::new(Level1::of_chunk(input.materials.classes(), &data));
    }
    if !generator.vertical_chunks().contains(&pos.y) {
        // The world makes these air without asking the generator.
        return Arc::new(Level1::Uniform(BlockState::AIR));
    }
    let key_of = (
        std::ptr::from_ref(generator).cast::<()>() as usize,
        std::ptr::from_ref(registry) as usize,
        input.epoch,
        key,
    );
    GENERATED.with(|g| {
        let mut g = g.borrow_mut();
        if let Some(cells) = g.cells.get(&key_of) {
            return Arc::clone(cells);
        }
        let data = generator.generate(key, registry);
        let cells = Arc::new(Level1::of_chunk(input.materials.classes(), &data));
        if g.order.len() >= GENERATED_CACHE
            && let Some(old) = g.order.pop_front()
        {
            g.cells.remove(&old);
        }
        g.order.push_back(key_of);
        g.cells.insert(key_of, Arc::clone(&cells));
        cells
    })
}

/// Fills `cells` with the level-1 node at `node` (its padded cells, border included).
pub(crate) fn fill(
    input: &FallbackInput,
    dim: DimensionId,
    node: ChunkPos,
    generator: &dyn WorldGenerator,
    registry: &BlockRegistry,
    cells: &mut LodCells,
) {
    // Chunk offsets -1..=2 from the node's first chunk along each axis; a chunk holds cells
    // 16 d .. 16 d + 15 of the node, clipped to -1..=32.
    for dy in -1..=2i64 {
        for dx in -1..=2i64 {
            for dz in -1..=2i64 {
                let pos = ChunkPos::new((node.x << 1) + dx, (node.y << 1) + dy, (node.z << 1) + dz);
                let chunk = chunk_cells(input, dim, pos, generator, registry);
                let clip = |d: i64| ((16 * d).max(-1), (16 * d + 15).min(32));
                let ((x0, x1), (y0, y1), (z0, z1)) = (clip(dx), clip(dy), clip(dz));
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        for z in z0..=z1 {
                            let state = chunk.get(
                                (x - 16 * dx) as usize,
                                (y - 16 * dy) as usize,
                                (z - 16 * dz) as usize,
                            );
                            cells.set(x as i32, y as i32, z as i32, state);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel_voxel::ChunkData;
    use std::ops::RangeInclusive;
    use std::sync::Mutex;

    /// Uniform chunks of the state its seed names: one generator (one address) whose world
    /// changes, as a game's generator that keeps its seed behind a lock.
    struct Seeded(Mutex<u16>);

    impl WorldGenerator for Seeded {
        fn generate(&self, _: ChunkKey, _: &BlockRegistry) -> ChunkData {
            ChunkData::uniform(BlockState(*self.0.lock().unwrap()))
        }
        fn vertical_chunks(&self) -> RangeInclusive<i64> {
            0..=0
        }
        fn name(&self) -> &str {
            "seeded"
        }
    }

    /// The generated cells are cached per world (`WorldEpoch`): the same generator at the same
    /// address with another seed in the next world never reads the cells of the last one.
    #[test]
    fn generated_cells_are_cached_per_world() {
        let registry = boxel_voxel::testing::registry();
        let solid = registry.default_state(registry.block_by_name("test:solid").unwrap());
        let lamp = registry.default_state(registry.block_by_name("test:lamp").unwrap());
        let generator = Seeded(Mutex::new(solid.0));
        let input = |epoch| FallbackInput {
            band: LodColumn::AIR,
            chunks: HashMap::default(),
            store: None,
            materials: Arc::new(LodMaterials::from_registry(&registry)),
            epoch,
        };
        let pos = ChunkPos::new(1 << 35, 0, -(1 << 35));
        let at = |epoch| chunk_cells(&input(epoch), DimensionId(0), pos, &generator, &registry);
        let first = at(1);
        assert_eq!(first.get(0, 0, 0), solid);
        *generator.0.lock().unwrap() = lamp.0;
        assert!(Arc::ptr_eq(&at(1), &first), "cached within a world");
        assert_eq!(at(2).get(0, 0, 0), lamp, "the next world's own cells");
    }
}
