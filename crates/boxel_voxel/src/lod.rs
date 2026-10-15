//! Far level of detail (ADR 0031): what a block is seen from far away, and the cell rule that
//! turns a cube of blocks into one coarse cell.
//!
//! A level-`k` LOD cell stands for `s^3` blocks, `s = 2^k`. Every state of a registry has a
//! [`LodClass`] ([`LodClasses::from_registry`], from its [`RenderShape`], its [`BlockModel`] and
//! the water it holds, never from its name):
//!
//! | Class | States |
//! |---|---|
//! | [`LodClass::Empty`] | `RenderShape::None`, crosses (plants), translucent blocks that are not fluids (glass), slabs and models made of boxes filling less than half the cell |
//! | [`LodClass::Solid`] | opaque and cutout cubes (leaves), slabs and models made of boxes filling at least half the cell |
//! | [`LodClass::Fluid`] | `BlockModel::Fluid` states, each counted as its fluid's source; states that hold water and are not Solid (below) |
//!
//! The first class that matches wins:
//!
//! 1. Solid, as the table says. A waterlogged Solid state (a slab) stays Solid: its water is not
//!    counted.
//! 2. Fluid for a state that holds water ([`BlockRegistry::is_waterlogged`]) in a registry with a
//!    waterlog fluid ([`BlockRegistry::waterlog_fluid`]), counted as that fluid's source: a
//!    waterlogged plant or thin model is the sea it stands in, so a sea floor full of them stays
//!    water far away. A state with the flag in a registry without a waterlog fluid holds nothing.
//! 3. Fluid or Empty, as the table says.
//!
//! The cell rule (the contract every LOD source follows):
//!
//! 1. If any block of the cell is Solid: when the top-most Fluid block lies above the top-most
//!    Solid block and the cell holds at least `s^2 / 2` Fluid blocks (one layer over half its
//!    area), the cell is that fluid (a sea over its bed). Otherwise the cell is Solid, with the
//!    state of its Solid block of greatest `y`, ties to the smallest `z`, then the smallest `x`.
//! 2. Else, if at least half of the cell's blocks are Fluid, the cell is that fluid.
//! 3. Else air.
//!
//! "That fluid" is the source with the most blocks in the cell, ties to the smallest state id.
//! Any solid block wins so that trunks, canopies, towers and builds stay visible at every level;
//! the clause of rule 1 keeps shallow seas water instead of turning them into their bed, and its
//! area threshold keeps a one-block puddle on land from turning a coarse cell into water. Rule 2
//! keeps a cell without ground from flooding a coast.
//!
//! [`CellPartial`] is what a region of blocks contributes to the rule: a cell of level `k + 1`
//! is the [`CellPartial::merge`] of its 8 level-`k` partials, exactly the rule applied to its
//! blocks while the cell holds one fluid kind ([`CellPartial::of_cell`] is the rule applied
//! directly).
use crate::{BlockModel, BlockRegistry, BlockState, RenderShape, rules};

/// What a state is seen from far away ([`LodClasses`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum LodClass {
    /// Not drawn at a distance (air, glass, plants and thin models that hold no water).
    #[default]
    Empty,
    /// Ground: any solid block makes its cell solid.
    Solid,
    /// A fluid, counted as its source, or a state that is not Solid and holds water, counted as
    /// the waterlog fluid's source.
    Fluid,
}

/// Half a block cell, in cubic sixteenths: a slab or a model made of boxes must fill at least this
/// much of its cell to be [`LodClass::Solid`].
pub const HALF_CELL_VOLUME: u32 = 16 * 16 * 16 / 2;

/// The [`LodClass`] of every state of a registry, and the source each Fluid state counts as.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LodClasses {
    /// By state id: the class and, for a Fluid state, the source it counts as (else the state
    /// itself).
    entries: Box<[(LodClass, BlockState)]>,
}

impl LodClasses {
    /// The classes of `registry`'s states, from their [`RenderShape`], their [`BlockModel`] and
    /// the water they hold (module docs).
    pub fn from_registry(registry: &BlockRegistry) -> Self {
        // Box model fill by `BoxModelId`, measured once per model.
        let mut boxes_solid: Vec<Option<bool>> = Vec::new();
        // The source the waterlogged states hold.
        let held = rules::waterlog_source(registry);
        let entries = (0..registry.len())
            .map(|i| {
                let s = BlockState(i as u16);
                let shape = registry.shape(s);
                let class = match (shape, registry.model(s)) {
                    (RenderShape::None, _) => LodClass::Empty,
                    (_, BlockModel::Fluid) => LodClass::Fluid,
                    (_, BlockModel::Cross) | (RenderShape::Translucent, _) => LodClass::Empty,
                    (_, BlockModel::Cube { .. }) => LodClass::Solid,
                    (_, BlockModel::Slab(_)) => {
                        solid_if(filled_volume(registry, s) >= HALF_CELL_VOLUME)
                    }
                    (_, BlockModel::Boxes(id)) => {
                        let at = usize::from(id.0);
                        if boxes_solid.len() <= at {
                            boxes_solid.resize(at + 1, None);
                        }
                        let solid = *boxes_solid[at]
                            .get_or_insert_with(|| filled_volume(registry, s) >= HALF_CELL_VOLUME);
                        solid_if(solid)
                    }
                };
                // Any solid wins, then the water a state holds, then the table.
                match (class, held) {
                    (LodClass::Solid, _) => (class, s),
                    (_, Some(water)) if registry.is_waterlogged(s) => (LodClass::Fluid, water),
                    (LodClass::Fluid, _) => (class, fluid_source(registry, s)),
                    (LodClass::Empty, _) => (class, s),
                }
            })
            .collect();
        Self { entries }
    }

    /// Number of states.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True for a registry without states.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The class of `s` (out of range -> `Empty`).
    pub fn class(&self, s: BlockState) -> LodClass {
        self.entries
            .get(usize::from(s.0))
            .map_or(LodClass::Empty, |e| e.0)
    }

    /// The source Fluid state `s` counts as (its fluid's level 0, or the waterlog fluid's for a
    /// state that holds water), `None` for a state that is not Fluid.
    pub fn fluid(&self, s: BlockState) -> Option<BlockState> {
        match self.entries.get(usize::from(s.0)) {
            Some(&(LodClass::Fluid, source)) => Some(source),
            _ => None,
        }
    }
}

fn solid_if(solid: bool) -> LodClass {
    if solid {
        LodClass::Solid
    } else {
        LodClass::Empty
    }
}

/// The source of a `BlockModel::Fluid` state: level 0 of its declared fluid, or its block's default
/// state when the registry declares no fluid for it.
fn fluid_source(registry: &BlockRegistry, s: BlockState) -> BlockState {
    match registry.fluid_of(s) {
        Some((id, _)) => registry.fluid(id).map_or(s, |f| f.first),
        None => registry.block(s).map_or(s, |b| registry.default_state(b)),
    }
}

/// Cubic sixteenths of its cell that the boxes of `s` fill (overlaps counted once).
fn filled_volume(registry: &BlockRegistry, s: BlockState) -> u32 {
    // One u16 row along x per (y, z).
    let mut rows = [0u16; 16 * 16];
    for b in registry.model_boxes(s) {
        let (min, max) = (b.bounds.min, b.bounds.max);
        let bits = (((1u32 << max[0]) - 1) & !((1u32 << min[0]) - 1)) as u16;
        for y in min[1]..max[1] {
            for z in min[2]..max[2] {
                rows[usize::from(y) * 16 + usize::from(z)] |= bits;
            }
        }
    }
    rows.iter().map(|r| r.count_ones()).sum()
}

/// What a region of blocks contributes to the cell rule (module docs). Merging the partials of
/// the 8 cells of a coarser cell gives that cell's partial: the tops are maxima and the counts
/// sums, so the merge is exact while the region holds one fluid kind. With two kinds it keeps the
/// one with the larger count (ties to the smaller id), an approximation of the direct rule that
/// depends on the merge order (a pyramid merges in a fixed order, so it stays deterministic).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CellPartial {
    /// The top-most Solid block, `[x, y, z]` and its state: greatest `y`, then smallest `z`, then
    /// smallest `x`.
    pub solid: Option<([i64; 3], BlockState)>,
    /// The `y` of the top-most Fluid block.
    pub fluid_top: Option<i64>,
    /// The fluid with the most blocks (its source) and its count.
    pub fluid: Option<(BlockState, u64)>,
    /// Fluid blocks of every kind.
    pub fluid_blocks: u64,
    /// Blocks in the region (`s^3` for a whole cell).
    pub blocks: u64,
}

impl CellPartial {
    /// `blocks` blocks of nothing (air or Empty states).
    pub const fn empty(blocks: u64) -> Self {
        Self {
            solid: None,
            fluid_top: None,
            fluid: None,
            fluid_blocks: 0,
            blocks,
        }
    }

    /// One block: `state` at `pos` (`[x, y, z]`).
    pub fn of_block(classes: &LodClasses, pos: [i64; 3], state: BlockState) -> Self {
        let mut p = Self::empty(1);
        match classes.class(state) {
            LodClass::Empty => {}
            LodClass::Solid => p.solid = Some((pos, state)),
            LodClass::Fluid => {
                p.fluid_top = Some(pos[1]);
                p.fluid = Some((classes.fluid(state).unwrap_or(state), 1));
                p.fluid_blocks = 1;
            }
        }
        p
    }

    /// The exact partial of the cube of `edge^3` blocks at `origin` (`[x, y, z]`, its min corner),
    /// `block(x, y, z)` giving the state at `origin + (x, y, z)`: the cell rule applied directly,
    /// with every fluid kind counted apart.
    pub fn of_cell(
        classes: &LodClasses,
        origin: [i64; 3],
        edge: u32,
        mut block: impl FnMut(u32, u32, u32) -> BlockState,
    ) -> Self {
        let e = u64::from(edge);
        let mut p = Self::empty(e * e * e);
        // (source, count) per fluid kind: a registry declares at most 16.
        let mut kinds: Vec<(BlockState, u64)> = Vec::new();
        for y in 0..edge {
            for z in 0..edge {
                for x in 0..edge {
                    let state = block(x, y, z);
                    let pos = [
                        origin[0] + i64::from(x),
                        origin[1] + i64::from(y),
                        origin[2] + i64::from(z),
                    ];
                    match classes.class(state) {
                        LodClass::Empty => {}
                        LodClass::Solid => p.solid = top_solid(p.solid, Some((pos, state))),
                        LodClass::Fluid => {
                            let source = classes.fluid(state).unwrap_or(state);
                            p.fluid_top = p.fluid_top.max(Some(pos[1]));
                            p.fluid_blocks += 1;
                            match kinds.iter_mut().find(|k| k.0 == source) {
                                Some(k) => k.1 += 1,
                                None => kinds.push((source, 1)),
                            }
                        }
                    }
                }
            }
        }
        p.fluid = kinds.into_iter().reduce(more_fluid);
        p
    }

    /// The partial of the union of two disjoint regions (commutative; associative while they
    /// hold one fluid kind).
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        let fluid = match (self.fluid, other.fluid) {
            (Some((a, n)), Some((b, m))) if a == b => Some((a, n + m)),
            (Some(a), Some(b)) => Some(more_fluid(a, b)),
            (a, b) => a.or(b),
        };
        Self {
            solid: top_solid(self.solid, other.solid),
            fluid_top: self.fluid_top.max(other.fluid_top),
            fluid,
            fluid_blocks: self.fluid_blocks + other.fluid_blocks,
            blocks: self.blocks + other.blocks,
        }
    }

    /// The cell's state by the cell rule (module docs), for a partial of a whole cube cell
    /// (`blocks` = `s^3`).
    pub fn state(&self) -> BlockState {
        if let Some((pos, solid)) = self.solid {
            // Rule 1: a fluid over the ground, one block deep over half the cell's area.
            let edge = cube_root(self.blocks);
            match (self.fluid_top, self.fluid) {
                (Some(top), Some((fluid, _)))
                    if top > pos[1] && 2 * u128::from(self.fluid_blocks) >= edge * edge =>
                {
                    fluid
                }
                _ => solid,
            }
        } else {
            // Rule 2: at least half of the cell is fluid.
            match self.fluid {
                Some((fluid, _)) if 2 * self.fluid_blocks >= self.blocks => fluid,
                _ => BlockState::AIR,
            }
        }
    }
}

/// The higher of two Solid tops by the rule's order (greatest `y`, then smallest `z`, then smallest
/// `x`, then the smaller state so the merge is commutative).
fn top_solid(
    a: Option<([i64; 3], BlockState)>,
    b: Option<([i64; 3], BlockState)>,
) -> Option<([i64; 3], BlockState)> {
    let key = |(p, s): ([i64; 3], BlockState)| {
        (
            p[1],
            std::cmp::Reverse(p[2]),
            std::cmp::Reverse(p[0]),
            std::cmp::Reverse(s),
        )
    };
    match (a, b) {
        (Some(a), Some(b)) => Some(if key(b) > key(a) { b } else { a }),
        (a, b) => a.or(b),
    }
}

/// The fluid kind with more blocks, ties to the smaller source id.
fn more_fluid(a: (BlockState, u64), b: (BlockState, u64)) -> (BlockState, u64) {
    if (b.1, std::cmp::Reverse(b.0)) > (a.1, std::cmp::Reverse(a.0)) {
        b
    } else {
        a
    }
}

/// The integer cube root of `n` (floor), as `u128`: the edge of a cube cell of `n` blocks.
fn cube_root(n: u64) -> u128 {
    let mut r = (n as f64).cbrt() as u128;
    let n = u128::from(n);
    while r * r * r > n {
        r -= 1;
    }
    while (r + 1) * (r + 1) * (r + 1) <= n {
        r += 1;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;
    use crate::{
        BlockCollision, BlockDef, BlockRegistryBuilder, BoxModel, FLUID_LEVEL, FaceTextures,
        MAX_BLOCK_COORD, ModelBox, OPAQUE_OPACITY, Property, TextureId, TextureImage, TextureSpec,
        WATERLOGGED,
    };
    use proptest::prelude::*;

    fn by(r: &BlockRegistry, name: &str) -> BlockState {
        r.parse_state(name).expect(name)
    }

    /// A cross plant that always holds water (its `waterlogged` property has the one value
    /// `true`), with the layer `texture`.
    fn wet_plant(texture: TextureId) -> (BlockDef, Vec<Property>) {
        let def = BlockDef {
            name: "test:wet_plant".to_string(),
            shape: RenderShape::Cutout,
            model: BlockModel::Cross,
            collision: BlockCollision::None,
            emission: [0; 3],
            light_opacity: 0,
            textures: FaceTextures::all(texture),
        };
        (def, vec![Property::new(WATERLOGGED, ["true"])])
    }

    /// The test registry plus models made of boxes of a quarter, three quarters and two equal
    /// boxes of three eighths (overlapping), two half models that can hold water (the translucent
    /// `test:clear_half` and the unseen `test:unseen`) and `test:wet_plant`, a cross that always
    /// holds water.
    fn registry() -> BlockRegistry {
        let mut b = testing::builder();
        let mut boxes = |name: &str, shape, boxes: Vec<ModelBox>, properties: Vec<Property>| {
            let model = b
                .add_box_model(BoxModel::from_boxes(boxes))
                .expect("box model");
            let def = BlockDef {
                name: name.to_string(),
                shape,
                model: BlockModel::Boxes(model),
                collision: BlockCollision::None,
                emission: [0; 3],
                light_opacity: OPAQUE_OPACITY,
                textures: FaceTextures::all(TextureId(0)),
            };
            b.add_block_with_states(def, properties, |_, _| {})
                .expect(name);
        };
        let half = || ModelBox::new([0, 0, 0], [16, 8, 16]);
        let under_half = || ModelBox::new([0, 0, 0], [16, 8, 12]);
        let wet = || vec![Property::boolean(WATERLOGGED)];
        boxes(
            "test:quarter",
            RenderShape::Opaque,
            vec![ModelBox::new([0, 0, 0], [16, 8, 8])],
            vec![],
        );
        boxes(
            "test:three_quarters",
            RenderShape::Cutout,
            vec![half(), ModelBox::new([0, 8, 0], [16, 16, 8])],
            vec![],
        );
        boxes(
            "test:overlap",
            RenderShape::Opaque,
            vec![under_half(), under_half()],
            vec![],
        );
        boxes(
            "test:clear_half",
            RenderShape::Translucent,
            vec![half()],
            wet(),
        );
        boxes("test:unseen", RenderShape::None, vec![half()], wet());
        let (def, wet) = wet_plant(TextureId(0));
        b.add_block_with_states(def, wet, |_, _| {})
            .expect("test:wet_plant");
        b.build()
    }

    #[test]
    fn classes_follow_shape_and_model() {
        let r = registry();
        let c = LodClasses::from_registry(&r);
        assert_eq!(c.len(), r.len());
        let class = |name: &str| c.class(by(&r, name));
        assert_eq!(c.class(BlockState::AIR), LodClass::Empty);
        assert_eq!(class("test:solid"), LodClass::Solid);
        assert_eq!(class("test:lamp"), LodClass::Solid);
        assert_eq!(class("test:cutout"), LodClass::Solid, "leaves are ground");
        assert_eq!(class("test:glass"), LodClass::Empty);
        assert_eq!(class("test:plant"), LodClass::Empty, "a cross is dropped");
        assert_eq!(class("test:slab"), LodClass::Solid, "a slab fills half");
        assert_eq!(class("test:slab[waterlogged=true]"), LodClass::Solid);
        assert_eq!(
            c.fluid(by(&r, "test:slab[waterlogged=true]")),
            None,
            "any solid wins"
        );
        assert_eq!(class("test:boxes"), LodClass::Empty, "a post and a bar");
        // What holds water and is not solid is the water it holds, even translucent or unseen.
        let water = by(&r, "test:fluid");
        for wet in [
            "test:boxes[waterlogged=true]",
            "test:wet_plant",
            "test:clear_half[waterlogged=true]",
            "test:unseen[waterlogged=true]",
        ] {
            assert_eq!(class(wet), LodClass::Fluid, "{wet}");
            assert_eq!(c.fluid(by(&r, wet)), Some(water), "{wet}");
        }
        assert_eq!(class("test:quarter"), LodClass::Empty);
        assert_eq!(class("test:three_quarters"), LodClass::Solid);
        assert_eq!(
            class("test:overlap"),
            LodClass::Empty,
            "overlaps count once"
        );
        assert_eq!(class("test:clear_half"), LodClass::Empty);
        assert_eq!(class("test:unseen"), LodClass::Empty);
        assert_eq!(c.class(BlockState(u16::MAX)), LodClass::Empty);
        assert_eq!(c.fluid(by(&r, "test:solid")), None);
    }

    #[test]
    fn held_water_needs_a_waterlog_fluid() {
        // A fluid and a wet cross, but no `set_waterlog_fluid`: the flag holds nothing.
        let mut b = BlockRegistryBuilder::new();
        let layer = b
            .add_texture(TextureSpec {
                name: "test:layer".to_string(),
                image: TextureImage::solid(testing::TEXTURE_SIZE, [40, 90, 200, 160]),
            })
            .expect("test:layer");
        let fluid = b
            .add_block_with_states(
                BlockDef {
                    name: "test:fluid".to_string(),
                    shape: RenderShape::Translucent,
                    model: BlockModel::Fluid,
                    collision: BlockCollision::None,
                    emission: [0; 3],
                    light_opacity: 1,
                    textures: FaceTextures::all(layer),
                },
                vec![Property::int(FLUID_LEVEL, 0..=15)],
                |_, _| {},
            )
            .expect("test:fluid");
        let block = b.block_by_name("test:fluid").expect("test:fluid");
        b.add_fluid(block, testing::FLUID).expect("test:fluid");
        let (def, wet) = wet_plant(layer);
        let plant = b.add_block_with_states(def, wet, |_, _| {}).expect("plant");
        let r = b.build();
        let c = LodClasses::from_registry(&r);
        assert!(r.waterlog_fluid().is_none());
        assert!(r.is_waterlogged(plant));
        assert_eq!(c.class(plant), LodClass::Empty);
        assert_eq!(c.fluid(plant), None);
        assert_eq!(c.class(fluid), LodClass::Fluid);
        assert_eq!(c.fluid(fluid), Some(fluid));
    }

    #[test]
    fn a_dry_twin_differs_only_by_its_water() {
        let r = registry();
        let c = LodClasses::from_registry(&r);
        let water = by(&r, "test:fluid");
        for s in (0..r.len() as u16).map(BlockState) {
            let dry = r.with_water(s, false).unwrap_or(s);
            let name = r.state_name(s);
            if r.is_waterlogged(s) && c.class(dry) != LodClass::Solid {
                assert_eq!(c.class(s), LodClass::Fluid, "{name}");
                assert_eq!(c.fluid(s), Some(water), "{name}");
            } else {
                assert_eq!(c.class(s), c.class(dry), "{name}");
                assert_eq!(c.fluid(s), c.fluid(dry), "{name}");
            }
        }
    }

    #[test]
    fn fluids_count_as_their_source() {
        let r = registry();
        let c = LodClasses::from_registry(&r);
        for fluid in ["test:fluid", "test:hot"] {
            let source = by(&r, fluid);
            for level in 0..16 {
                let s = by(&r, &format!("{fluid}[level={level}]"));
                assert_eq!(c.class(s), LodClass::Fluid, "{fluid} {level}");
                assert_eq!(c.fluid(s), Some(source), "{fluid} {level}");
            }
        }
    }

    /// The state of the `edge^3` cell at `origin` whose blocks `block` gives, by the direct rule.
    fn direct(
        c: &LodClasses,
        origin: [i64; 3],
        edge: u32,
        block: impl FnMut(u32, u32, u32) -> BlockState,
    ) -> BlockState {
        CellPartial::of_cell(c, origin, edge, block).state()
    }

    #[test]
    fn any_solid_block_wins() {
        let r = registry();
        let c = LodClasses::from_registry(&r);
        let solid = by(&r, "test:solid");
        let cell = direct(&c, [0; 3], 16, |x, y, z| {
            if [x, y, z] == [3, 9, 12] {
                solid
            } else {
                BlockState::AIR
            }
        });
        assert_eq!(cell, solid, "one solid block in 4096");
        let leaf = by(&r, "test:cutout");
        let cell = direct(&c, [0; 3], 16, |x, y, z| {
            if (x, z) == (5, 5) && y < 4 {
                leaf
            } else {
                BlockState::AIR
            }
        });
        assert_eq!(cell, leaf, "a thin column of leaves");
    }

    #[test]
    fn the_top_most_solid_state_wins() {
        let r = registry();
        let c = LodClasses::from_registry(&r);
        let (solid, lamp, leaf) = (
            by(&r, "test:solid"),
            by(&r, "test:lamp"),
            by(&r, "test:cutout"),
        );
        // A lamp layer over a solid ground, and leaves over a trunk of lamps.
        for edge in [2, 4, 8, 16] {
            let ground = direct(&c, [0; 3], edge, |_, y, _| match y {
                0 => solid,
                1 => lamp,
                _ => BlockState::AIR,
            });
            assert_eq!(ground, lamp, "edge {edge}");
            let tree = direct(&c, [0; 3], edge, |x, y, z| match (x, y, z) {
                (1, 0, 1) => lamp,
                (_, 1, _) => leaf,
                _ => BlockState::AIR,
            });
            assert_eq!(tree, leaf, "edge {edge}");
        }
        // Ties at the top: the smallest z, then the smallest x.
        let tie = direct(&c, [0; 3], 4, |x, y, z| match (x, y, z) {
            (3, 2, 1) => lamp,
            (0, 2, 2) | (2, 1, 0) => solid,
            (1, 2, 1) => leaf,
            _ => BlockState::AIR,
        });
        assert_eq!(tie, leaf);
    }

    #[test]
    fn a_fluid_needs_half_the_cell_without_ground() {
        let r = registry();
        let c = LodClasses::from_registry(&r);
        let water = by(&r, "test:fluid");
        for (layers, expect) in [(15, BlockState::AIR), (16, water)] {
            let cell = direct(&c, [0; 3], 32, |_, y, _| {
                if y < layers { water } else { BlockState::AIR }
            });
            assert_eq!(cell, expect, "{layers} layers of 32");
        }
        // Flowing levels count as the source.
        let flowing = by(&r, "test:fluid[level=5]");
        let cell = direct(&c, [0; 3], 2, |_, y, _| {
            if y == 0 { flowing } else { BlockState::AIR }
        });
        assert_eq!(cell, water);
    }

    #[test]
    fn the_fluid_with_most_blocks_wins() {
        let r = registry();
        let c = LodClasses::from_registry(&r);
        let (water, hot) = (by(&r, "test:fluid"), by(&r, "test:hot[level=2]"));
        let cell = direct(&c, [0; 3], 2, |x, y, z| {
            if (x, y, z) == (0, 0, 0) { water } else { hot }
        });
        assert_eq!(cell, by(&r, "test:hot"));
        // A tie goes to the smaller source id.
        let cell = direct(&c, [0; 3], 2, |_, y, _| if y == 0 { water } else { hot });
        assert_eq!(cell, water.min(by(&r, "test:hot")));
    }

    /// Ground up to `ground` (inclusive), water above it up to `sea` (inclusive), air above.
    fn sea_block(r: &BlockRegistry, ground: i64, sea: i64) -> impl Fn(i64) -> BlockState {
        let (solid, water) = (by(r, "test:solid"), by(r, "test:fluid"));
        move |y| {
            if y <= ground {
                solid
            } else if y <= sea {
                water
            } else {
                BlockState::AIR
            }
        }
    }

    #[test]
    fn shallow_seas_stay_water() {
        let r = registry();
        let c = LodClasses::from_registry(&r);
        let water = by(&r, "test:fluid");
        let sea = 46;
        for depth in [3, 10] {
            let block = sea_block(&r, sea - depth, sea);
            for level in 2..=4 {
                let s = 1i64 << level;
                // Every cell holding water, the surface's included.
                let (first, last) = ((sea - depth + 1).div_euclid(s), sea.div_euclid(s));
                for cell_y in first..=last {
                    let y0 = cell_y * s;
                    let cell = direct(&c, [0, y0, 0], s as u32, |_, y, _| block(y0 + i64::from(y)));
                    assert_eq!(cell, water, "depth {depth} level {level} cell y {y0}");
                }
            }
        }
    }

    #[test]
    fn held_water_is_water_to_the_rule() {
        let r = registry();
        let c = LodClasses::from_registry(&r);
        let (solid, water) = (by(&r, "test:solid"), by(&r, "test:fluid"));
        let wet = [
            by(&r, "test:wet_plant"),
            by(&r, "test:boxes[waterlogged=true]"),
        ];
        let dry = [by(&r, "test:plant"), by(&r, "test:boxes")];
        let sea = 46;
        // Far from the origin too (ADR 0014), up to the world limit.
        let origins = [
            0,
            1i64 << 31,
            -(1i64 << 31),
            1i64 << 40,
            MAX_BLOCK_COORD,
            -MAX_BLOCK_COORD,
        ];
        // Even grounds: the bed's top and the plants share a level-1 cell.
        for depth in [4, 10] {
            let ground = sea - depth;
            for o in origins {
                let origin = [o, o, -o];
                // `[plant, post]`: a layer of plants on the bed, a post from the bed to the
                // surface in one column of every 16 by 16 blocks, water over the rest.
                let block = |[plant, post]: [BlockState; 2], at: [i64; 3]| {
                    let [x, y, z] = [0, 1, 2].map(|a| at[a] - origin[a]);
                    if y <= ground {
                        solid
                    } else if y > sea {
                        BlockState::AIR
                    } else if x.rem_euclid(16) == 0 && z.rem_euclid(16) == 0 {
                        post
                    } else if y == ground + 1 {
                        plant
                    } else {
                        water
                    }
                };
                let mut dry_cells = Vec::new();
                for level in 1..=4 {
                    let s = 1i64 << level;
                    let (first, last) = ((ground + 1).div_euclid(s), sea.div_euclid(s));
                    // The cell at x 0, with a post, and the next one, without a post below
                    // level 4 (at level 4 it reaches the post at x 16). Every cell holds water.
                    for cell_x in [0, 1] {
                        for cell_y in first..=last {
                            let at = [origin[0] + cell_x * s, origin[1] + cell_y * s, origin[2]];
                            let cell = |pair: [BlockState; 2]| {
                                direct(&c, at, s as u32, |x, y, z| {
                                    let d = [x, y, z].map(i64::from);
                                    block(pair, [at[0] + d[0], at[1] + d[1], at[2] + d[2]])
                                })
                            };
                            assert_eq!(
                                cell(wet),
                                water,
                                "depth {depth} origin {o} level {level} cell {cell_x} {cell_y}"
                            );
                            dry_cells.push(cell(dry));
                        }
                    }
                }
                // Dry, the same sea gets its bed and holes.
                for want in [solid, BlockState::AIR] {
                    assert!(
                        dry_cells.contains(&want),
                        "depth {depth} origin {o}: {want:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_puddle_on_land_stays_ground() {
        let r = registry();
        let c = LodClasses::from_registry(&r);
        let (solid, water) = (by(&r, "test:solid"), by(&r, "test:fluid"));
        for level in 1..=4 {
            let s = 1u32 << level;
            // Ground up to the cell's second-to-last layer, one water block on it.
            let cell = direct(&c, [0; 3], s, |x, y, z| {
                if y + 2 <= s {
                    solid
                } else if (x, z) == (0, 0) {
                    water
                } else {
                    BlockState::AIR
                }
            });
            assert_eq!(cell, solid, "level {level}");
        }
    }

    /// Merges the block partials of the `edge^3` cube at `at` (local to the cell at `origin`,
    /// whose blocks `block` gives) up an octree, 8 cells at a time.
    fn by_merge(
        c: &LodClasses,
        origin: [i64; 3],
        at: [u32; 3],
        edge: u32,
        block: &dyn Fn(u32, u32, u32) -> BlockState,
    ) -> CellPartial {
        if edge == 1 {
            let pos = [0, 1, 2].map(|a| origin[a] + i64::from(at[a]));
            return CellPartial::of_block(c, pos, block(at[0], at[1], at[2]));
        }
        let h = edge / 2;
        let mut out: Option<CellPartial> = None;
        for o in 0..8u32 {
            let sub = [
                at[0] + (o & 1) * h,
                at[1] + ((o >> 1) & 1) * h,
                at[2] + ((o >> 2) & 1) * h,
            ];
            let child = by_merge(c, origin, sub, h, block);
            out = Some(out.map_or(child, |p| p.merge(child)));
        }
        out.expect("8 children")
    }

    /// The states of the property tests: one fluid kind (the wet states hold it), so the merge
    /// is exact.
    fn palette(r: &BlockRegistry) -> Vec<BlockState> {
        [
            "air",
            "test:solid",
            "test:lamp",
            "test:cutout",
            "test:glass",
            "test:plant",
            "test:slab",
            "test:boxes",
            "test:fluid",
            "test:fluid[level=7]",
            "test:slab[waterlogged=true]",
            "test:boxes[waterlogged=true]",
            "test:wet_plant",
        ]
        .iter()
        .map(|n| {
            if *n == "air" {
                BlockState::AIR
            } else {
                by(r, n)
            }
        })
        .collect()
    }

    /// A random cell: a ground height and a sea top per column (either may be below the cell),
    /// with random blocks sprinkled over it.
    fn cell_strategy(edge: u32) -> impl Strategy<Value = (Vec<(i32, i32)>, Vec<(u32, usize)>)> {
        let e = edge as i32;
        let n = (edge * edge) as usize;
        let volume = edge * edge * edge;
        (
            prop::collection::vec((-1..e, -1..e + 1), n),
            prop::collection::vec((0..volume, 0..13usize), 0..8),
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(96))]

        #[test]
        fn merging_partials_equals_the_direct_rule(
            level in 1u32..=4,
            columns in cell_strategy(16),
            far in -(1i64 << 36)..(1i64 << 36),
        ) {
            let r = registry();
            let c = LodClasses::from_registry(&r);
            let pal = palette(&r);
            let (solid, water) = (by(&r, "test:solid"), by(&r, "test:fluid"));
            let edge = 1u32 << level;
            let (heights, sprinkles) = columns;
            let block = |x: u32, y: u32, z: u32| -> BlockState {
                let index = y * edge * edge + z * edge + x;
                if let Some(&(_, p)) = sprinkles.iter().rev().find(|s| s.0 % (edge * edge * edge) == index) {
                    return pal[p];
                }
                let (g, w) = heights[(z * 16 + x) as usize];
                let y = y as i32;
                if y <= g.min(edge as i32 - 1) * (edge as i32) / 16 {
                    solid
                } else if y <= w * (edge as i32) / 16 {
                    water
                } else {
                    BlockState::AIR
                }
            };
            let origin = [far * i64::from(edge), (far >> 7) * i64::from(edge), -far * i64::from(edge)];
            let exact = CellPartial::of_cell(&c, origin, edge, block);
            let merged = by_merge(&c, origin, [0; 3], edge, &block);
            prop_assert_eq!(merged, exact);
            prop_assert_eq!(merged.state(), exact.state());
        }

        #[test]
        fn partials_merge_associatively_and_commutatively(
            blocks in prop::collection::vec(
                ((-40i64..40, -40i64..40, -40i64..40), 0..13usize, 0u64..3), 3),
        ) {
            let r = registry();
            let c = LodClasses::from_registry(&r);
            let pal = palette(&r);
            // Each partial: one block, plus some empty blocks.
            let p: Vec<CellPartial> = blocks
                .iter()
                .map(|&((x, y, z), s, extra)| {
                    CellPartial::of_block(&c, [x, y, z], pal[s]).merge(CellPartial::empty(extra))
                })
                .collect();
            prop_assert_eq!(p[0].merge(p[1]), p[1].merge(p[0]));
            prop_assert_eq!(p[0].merge(p[1]).merge(p[2]), p[0].merge(p[1].merge(p[2])));
        }
    }
}
