//! What the sweep and the raycast read: a [`VoxelSource`] answers one block at a time, and the
//! [`CollisionTable`] says how each block state collides.

use super::shape::LocalBox;
use boxel_math::DVec3;
use boxel_voxel::{
    BlockCollision, BlockPos, BlockRegistry, BlockState, ChunkData, ChunkPos, CollisionBox,
    RenderShape, StateFlags,
};
use std::collections::{HashMap, HashSet};

/// The answer of a [`VoxelSource`] for one block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    /// The block. Space with no data that is known to be empty (a chunk whose generation failed,
    /// layers outside the world's vertical bounds) reads as air.
    Block(BlockState),
    /// The block's chunk is not loaded yet: its content is unknown. Physics never guesses: a
    /// sweep that needs this block stops ([`crate::Unloaded`]) and a ray reports it.
    Unloaded,
}

/// Read access to voxels, one block at a time. `&mut self` so implementations can cache (the
/// last chunk read: consecutive probes are almost always in the same chunk).
pub trait VoxelSource {
    /// The block at `p`.
    fn probe(&mut self, p: BlockPos) -> Probe;
}

impl<S: VoxelSource + ?Sized> VoxelSource for &mut S {
    #[inline]
    fn probe(&mut self, p: BlockPos) -> Probe {
        (**self).probe(p)
    }
}

/// How a block collides (or, for [`CollisionTable::outline`], how it is picked).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[non_exhaustive]
pub enum CollisionShape<'a> {
    /// No collision (air, water, flowers).
    #[default]
    Empty,
    /// The whole unit cube.
    Full,
    /// A union of boxes in the block's frame (`0..=1` on every axis; a bottom slab is
    /// `(0, 0, 0)..(1, 0.5, 1)`). A collision box may reach above the cell, up to
    /// [`CollisionTable::reach`] (a fence is 1.5 tall, ADR 0026); outlines stay inside it.
    Boxes(&'a [LocalBox]),
}

/// `CollisionTable` bit: the block collides as a full cube.
const FULL: u8 = 1;
/// `CollisionTable` bit: the block is a fluid.
const FLUID: u8 = 1 << 1;
/// `CollisionTable` bit: the block has geometry (picking outline).
const OUTLINE: u8 = 1 << 2;
/// `CollisionTable` bit: the block collides with boxes (`collision` range).
const BOXES: u8 = 1 << 3;
/// `CollisionTable` bit: the block is picked by boxes (`outline` range), not the full cube.
const OUTLINE_BOXES: u8 = 1 << 4;
/// `CollisionTable` bit: some collision box of the block reaches into the cell above.
const TALL: u8 = 1 << 5;

/// Per block state, what physics needs to know: its [`CollisionShape`], its picking outline and
/// whether it is a fluid. Built from a [`BlockRegistry`] (ADR 0016, ADR 0018):
/// `BlockDef::collision` gives the shape (a box list that is exactly the unit cube counts as
/// `Full`, an empty one as `Empty`), the registry's `FLUID` flag gives the fluid bit, and a
/// render shape other than `None` gives an outline, the model's (`BlockModel::outline`: the
/// cube, a slab's half, a box around a plant). States outside the registry are empty.
///
/// A collision box may reach into the cell above (a fence collides 1.5 blocks tall, ADR 0026):
/// such a block is [`CollisionTable::is_tall`], and the sweep and the collision raycast also
/// look at the cell under the ones a box or a ray crosses, up to [`CollisionTable::reach`]
/// above it. Outlines stay inside the cell.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CollisionTable {
    bits: Vec<u8>,
    /// Range of each state's collision boxes in `boxes` (states with the `BOXES` bit).
    collision: Vec<(u32, u32)>,
    /// Range of each state's outline boxes in `boxes` (states with the `OUTLINE_BOXES` bit).
    outline: Vec<(u32, u32)>,
    boxes: Vec<LocalBox>,
    /// Some state has the `BOXES` bit.
    any_boxes: bool,
    /// The smallest distance from a box face that is not on a block plane to the block plane
    /// nearest it (1 without such faces; 0 in a table that was not built from a registry).
    plane_gap: f64,
    /// How far above its cell the highest collision box reaches (0 without tall blocks).
    reach: f64,
}

impl CollisionTable {
    /// The table of every state of `registry`.
    pub fn from_registry(registry: &BlockRegistry) -> Self {
        let mut table = Self::default();
        for i in 0..registry.len() {
            let state = BlockState(i as u16);
            let flags = registry.flags(state);
            let mut bits = 0;
            if flags.contains(StateFlags::FLUID) {
                bits |= FLUID;
            }
            let collision = table.push_shape(registry.collision(state));
            match collision {
                Pushed::Full => bits |= FULL,
                Pushed::Boxes((start, end)) => {
                    bits |= BOXES;
                    table.any_boxes = true;
                    for bx in &table.boxes[start as usize..end as usize] {
                        if bx.max.y > 1.0 {
                            bits |= TALL;
                            table.reach = table.reach.max(bx.max.y - 1.0);
                        }
                    }
                }
                Pushed::Empty => {}
            }
            let outline = if registry.shape(state) == RenderShape::None {
                Pushed::Empty
            } else {
                bits |= OUTLINE;
                table.push_shape(&registry.model(state).outline())
            };
            if let Pushed::Boxes(_) = outline {
                bits |= OUTLINE_BOXES;
            }
            table.bits.push(bits);
            table.collision.push(collision.range());
            table.outline.push(outline.range());
        }
        // `boxel_voxel::MAX_COLLISION_HEIGHT` keeps it at half a block: sweeps and rays look
        // at one row under what they cross, never two.
        debug_assert!(table.reach < 1.0, "collision boxes reach a block up");
        table.plane_gap = 1.0;
        for bx in &table.boxes {
            for v in bx.min.to_array().into_iter().chain(bx.max.to_array()) {
                let f = v - v.floor();
                if f != 0.0 {
                    table.plane_gap = table.plane_gap.min(f.min(1.0 - f));
                }
            }
        }
        table
    }

    /// Adds the boxes of `collision` (in sixteenths) to `boxes`.
    fn push_shape(&mut self, collision: &BlockCollision) -> Pushed {
        match collision {
            BlockCollision::None => Pushed::Empty,
            BlockCollision::Full => Pushed::Full,
            BlockCollision::Boxes(list) if list.is_empty() => Pushed::Empty,
            BlockCollision::Boxes(list) if list.as_slice() == [CollisionBox::FULL] => Pushed::Full,
            BlockCollision::Boxes(list) => {
                let start = self.boxes.len() as u32;
                let unit = |v: [u8; 3]| DVec3::new(v[0].into(), v[1].into(), v[2].into()) / 16.0;
                self.boxes
                    .extend(list.iter().map(|b| LocalBox::new(unit(b.min), unit(b.max))));
                Pushed::Boxes((start, self.boxes.len() as u32))
            }
        }
    }

    #[inline]
    fn bits(&self, state: BlockState) -> u8 {
        self.bits.get(state.0 as usize).copied().unwrap_or(0)
    }

    #[inline]
    fn range(&self, ranges: &[(u32, u32)], state: BlockState) -> &[LocalBox] {
        let (start, end) = ranges.get(state.0 as usize).copied().unwrap_or((0, 0));
        &self.boxes[start as usize..end as usize]
    }

    /// How `state` collides.
    #[inline]
    pub fn shape(&self, state: BlockState) -> CollisionShape<'_> {
        let bits = self.bits(state);
        if bits & FULL != 0 {
            CollisionShape::Full
        } else if bits & BOXES != 0 {
            CollisionShape::Boxes(self.range(&self.collision, state))
        } else {
            CollisionShape::Empty
        }
    }

    /// The shape `state` is picked by (its outline): `Empty` for blocks without geometry.
    #[inline]
    pub fn outline(&self, state: BlockState) -> CollisionShape<'_> {
        let bits = self.bits(state);
        if bits & OUTLINE_BOXES != 0 {
            CollisionShape::Boxes(self.range(&self.outline, state))
        } else if bits & OUTLINE != 0 {
            CollisionShape::Full
        } else {
            CollisionShape::Empty
        }
    }

    /// `state` collides as a full cube.
    #[inline]
    pub fn is_full(&self, state: BlockState) -> bool {
        self.bits(state) & FULL != 0
    }

    /// Some collision box of `state` reaches into the cell above (a fence, ADR 0026).
    #[inline]
    pub fn is_tall(&self, state: BlockState) -> bool {
        self.bits(state) & TALL != 0
    }

    /// How far above its cell the highest collision box of the table reaches, in blocks: 0
    /// without tall blocks, 0.5 with fences (`boxel_voxel::MAX_COLLISION_HEIGHT` caps it there).
    /// Sweeps and collision rays look at the cell under the ones they cross only when a box
    /// there can reach them.
    #[inline]
    pub fn reach(&self) -> f64 {
        self.reach
    }

    /// `state` collides at all (a full cube or boxes).
    #[inline]
    pub fn collides(&self, state: BlockState) -> bool {
        self.bits(state) & (FULL | BOXES) != 0
    }

    /// Some state collides with boxes (not a full cube): sweeps then also look at the layer the
    /// moving box's face is in.
    #[inline]
    pub fn has_boxes(&self) -> bool {
        self.any_boxes
    }

    /// How close to a block plane a face of a block's boxes can be without being on it: every
    /// face of the table's boxes (collision and outline) is on a block plane or at least this far
    /// from the nearest one (1 when the table has no boxes, 0 for a default table). A sweep's
    /// step-up uses it to tell, without reading blocks, that no block face fits between a box and
    /// the plane it touches.
    #[inline]
    pub fn plane_gap(&self) -> f64 {
        self.plane_gap
    }

    /// `state` is a fluid (water).
    #[inline]
    pub fn is_fluid(&self, state: BlockState) -> bool {
        self.bits(state) & FLUID != 0
    }

    /// `state` has geometry, so it can be picked (everything but air and invisible blocks).
    #[inline]
    pub fn has_outline(&self, state: BlockState) -> bool {
        self.bits(state) & OUTLINE != 0
    }

    /// Number of states.
    pub fn len(&self) -> usize {
        self.bits.len()
    }

    /// No states at all (not even air).
    pub fn is_empty(&self) -> bool {
        self.bits.is_empty()
    }
}

/// What [`CollisionTable::push_shape`] added.
#[derive(Clone, Copy)]
enum Pushed {
    Empty,
    Full,
    Boxes((u32, u32)),
}

impl Pushed {
    fn range(self) -> (u32, u32) {
        match self {
            Pushed::Boxes(range) => range,
            Pushed::Empty | Pushed::Full => (0, 0),
        }
    }
}

/// A small in-memory world for tests, benches and tools: chunks in a hash map (a chunk that is
/// not there is air) plus a set of chunks that read as [`Probe::Unloaded`]. No cache: every
/// probe is one hash lookup. The ECS layer reads the real world through
/// [`crate::ChunkSource`].
#[derive(Clone, Debug, Default)]
pub struct TestWorld {
    chunks: HashMap<ChunkPos, ChunkData>,
    unloaded: HashSet<ChunkPos>,
}

impl TestWorld {
    /// All air, all loaded.
    pub fn new() -> Self {
        Self::default()
    }

    /// The block at `p` (air in a chunk that was never written; also for unloaded chunks).
    pub fn get(&self, p: BlockPos) -> BlockState {
        let (chunk, local) = p.split();
        self.chunks
            .get(&chunk)
            .map_or(BlockState::AIR, |data| data.get(local))
    }

    /// Sets one block.
    pub fn set(&mut self, p: BlockPos, state: BlockState) {
        let (chunk, local) = p.split();
        self.chunks
            .entry(chunk)
            .or_insert_with(|| ChunkData::uniform(BlockState::AIR))
            .set(local, state);
    }

    /// Sets every block of the box `min..=max` (inclusive on every axis).
    pub fn fill(&mut self, min: BlockPos, max: BlockPos, state: BlockState) {
        for y in min.y..=max.y {
            for x in min.x..=max.x {
                for z in min.z..=max.z {
                    self.set(BlockPos::new(x, y, z), state);
                }
            }
        }
    }

    /// Replaces a whole chunk (any storage: uniform, packed, direct).
    pub fn set_chunk(&mut self, pos: ChunkPos, data: ChunkData) {
        self.chunks.insert(pos, data);
    }

    /// The stored chunk at `pos`, if any was written.
    pub fn chunk(&self, pos: ChunkPos) -> Option<&ChunkData> {
        self.chunks.get(&pos)
    }

    /// Marks a chunk as unloaded (`true`) or loaded again (`false`); its blocks are kept.
    pub fn set_unloaded(&mut self, pos: ChunkPos, unloaded: bool) {
        if unloaded {
            self.unloaded.insert(pos);
        } else {
            self.unloaded.remove(&pos);
        }
    }
}

impl VoxelSource for TestWorld {
    fn probe(&mut self, p: BlockPos) -> Probe {
        let mut view: &TestWorld = self;
        view.probe(p)
    }
}

impl VoxelSource for &TestWorld {
    fn probe(&mut self, p: BlockPos) -> Probe {
        let (chunk, local) = p.split();
        if self.unloaded.contains(&chunk) {
            return Probe::Unloaded;
        }
        Probe::Block(
            self.chunks
                .get(&chunk)
                .map_or(BlockState::AIR, |data| data.get(local)),
        )
    }
}
