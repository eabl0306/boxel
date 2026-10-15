use crate::fluid::{FluidScratch, emit_sloped, find_sloped};
use crate::greedy::Greedy;
use crate::packed::{LightQuantization, layout, palette_entry};
use crate::shading::{
    AO_OPEN_BYTE, AO_TABLE, FaceKey, FaceShading, KeyTable, LightPalette, PaletteFull, corner_ao,
    face_corner_lights, neighborhood8,
};
use crate::{
    BUCKET_COUNT, CUTOUT_BUCKET, ChunkMesh, FLUID_SURFACE_BIT, PADDED, PADDED_AREA, PADDED_VOLUME,
    PackedQuad, PaddedChunk, TRANSLUCENT_BUCKET, VoxelClass,
};
use boxel_light::PaddedLight;
use boxel_voxel::{
    BlockModel, BlockRegistry, BlockState, CHUNK_SIZE, CHUNK_VOLUME, ChunkData, FLUID_LEVEL, Face,
    LightValue, LocalPos, MAX_MODEL_BOXES, ModelBox, NEIGHBOR_OFFSETS, Paletted, RenderShape,
    neighbor_index,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;

/// Which faces get ambient occlusion (ADR 0015). AO is part of the greedy-merge key, so every
/// AO value baked splits merges: it costs quads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AoPolicy {
    /// No AO: every face is open, quads merge as by material alone.
    Off,
    /// AO on every face.
    Baked,
    /// AO on every face with some light at one of its corners. A face whose 4 corner lights are
    /// all 0 keeps AO open and merges as with `Off`: almost all cave geometry is dark, and AO
    /// there is invisible at the default ambient floor. To revisit when held lights arrive.
    #[default]
    BakedLitOnly,
}

/// Mesher knobs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct MeshOptions {
    /// Ambient occlusion policy.
    pub ao: AoPolicy,
}

impl MeshOptions {
    /// Options with this AO policy.
    pub const fn with_ao(ao: AoPolicy) -> Self {
        Self { ao }
    }
}

/// Mesher-side view of the registry: per material id, bgm class, render shape (bucket routing),
/// model, the texture layer and shape code of each face, whether it is drawn full bright
/// (`BlockRegistry::is_full_bright`) and the light it is lit by when it emits without being
/// full bright ([`MaterialTable::lit_emission`]). Built once per registry and shared (`Arc`) by
/// mesh jobs.
#[derive(Clone, Debug, Default)]
pub struct MaterialTable {
    class: Vec<VoxelClass>,
    shape: Vec<RenderShape>,
    model: Vec<BlockModel>,
    layers: Vec<[u16; 6]>,
    /// Shape code of each cube face (`layout::SHAPE_CUBE` or `SHAPE_TURNED`).
    codes: Vec<[u8; 6]>,
    emissive: Vec<bool>,
    /// Per material: the emission of a lit emitter (ADR 0036), dark for the others.
    lit: Vec<LightValue>,
    /// Some material is a lit emitter: gather looks for them in the center chunk.
    has_lit: bool,
    /// Some material is a fluid (`BlockModel::Fluid`): gather marks the surface cells.
    has_fluid: bool,
    /// Per state: the material it is meshed as (itself, but the source's material for every
    /// level of a fluid, ADR 0021, and the water source's for a waterlogged block, ADR 0025)
    /// and its fluid level (`0` for sources and non-fluids).
    fluid_source: Vec<u16>,
    fluid_level: Vec<u8>,
    /// Per state: a waterlogged block (ADR 0025), gathered as its water (`fluid_source`) and
    /// drawn as its boxes too.
    waterlogged: Vec<bool>,
    /// The models made of boxes (ADR 0020), see [`BoxModels`].
    box_models: BoxModels,
}

/// No box shape (`MaterialTable` materials that are not made of boxes).
const NO_SHAPE: u16 = u16::MAX;

/// The models made of boxes of a material table (slabs, stairs, fences, torches; ADR 0020):
/// every distinct [`ModelBox`] gets an index (the box index of its `SHAPE_BOX` quads, the
/// renderer's box table), every distinct model a [`BoxShape`].
#[derive(Clone, Debug, Default)]
struct BoxModels {
    /// [`BoxShape`] index of each material ([`NO_SHAPE`] for the others).
    shape_of: Vec<u16>,
    shapes: Vec<BoxShape>,
    shape_ids: HashMap<BlockModel, u16>,
    /// Distinct boxes in first-use order: the box table.
    boxes: Vec<ModelBox>,
    box_ids: HashMap<ModelBox, u16>,
}

/// Sixteenths of a cell border face: bit `u` of row `v` (the face's axes, [`Face::axes`]).
type Cover = [u16; 16];

/// A face rectangle in sixteenths along the face's u and v axes: `[u0, u1, v0, v1]`.
type Rect = [u8; 4];

/// The bits `rect` covers in a [`Cover`] row.
fn rect_bits(rect: Rect) -> u16 {
    ((1u32 << rect[1]) - (1u32 << rect[0])) as u16
}

/// True if `cover` holds every sixteenth of `rect`.
fn is_covered(cover: &Cover, rect: Rect) -> bool {
    let bits = rect_bits(rect);
    cover[usize::from(rect[2])..usize::from(rect[3])]
        .iter()
        .all(|&row| row & bits == bits)
}

/// Adds `rect` to `cover`.
fn add_rect(cover: &mut Cover, rect: Rect) {
    let bits = rect_bits(rect);
    for row in &mut cover[usize::from(rect[2])..usize::from(rect[3])] {
        *row |= bits;
    }
}

/// One drawn face of one box of a model.
#[derive(Clone, Copy, Debug)]
struct BoxFace {
    face: Face,
    /// Index of the box in [`BoxModels::boxes`].
    index: u16,
    /// Flush with the cell border (a neighbor can hide it).
    border: bool,
    /// Where it is on the face's plane.
    rect: Rect,
}

/// No solid top ([`BoxShape::top_floor`] of a model whose boxes leave part of its top layer
/// open).
const NO_FLOOR: u8 = u8::MAX;

/// The faces a model made of boxes draws: every box face that no other box of the model covers
/// (the bottom of a stair's step on its slab, a fence bar's end on the post), and what its faces
/// flush with the cell border cover, per face.
#[derive(Clone, Debug)]
struct BoxShape {
    faces: Vec<BoxFace>,
    covers: [Cover; 6],
    /// The faces (bit [`Face::index`]) its boxes cover whole on the cell border (Minecraft's
    /// sturdy faces: a slab's bottom, the back of stairs).
    sturdy: u8,
    /// The lowest y (sixteenths) from which its boxes fill every layer up to the cell's top
    /// whole: 8 for a top slab or upside-down stairs, 0 for a full box, [`NO_FLOOR`] when the
    /// top layer is not full (a bottom slab, a fence).
    top_floor: u8,
}

impl BoxShape {
    /// The shape of `boxes`, `index` giving each box's table index.
    fn new(boxes: &[ModelBox], mut index: impl FnMut(ModelBox) -> u16) -> Self {
        let mut faces = Vec::new();
        let mut covers = [[0u16; 16]; 6];
        for &b in boxes {
            let (min, max) = (b.bounds.min, b.bounds.max);
            for face in Face::ALL {
                let (n, u, v) = face.axes();
                let positive = face.index().is_multiple_of(2);
                let plane = if positive { max[n] } else { min[n] };
                let rect = [min[u], max[u], min[v], max[v]];
                // The other boxes whose opposite face lies on this plane, touching this box.
                let mut inner: Cover = [0; 16];
                for other in boxes.iter().filter(|o| **o != b) {
                    let (omin, omax) = (other.bounds.min, other.bounds.max);
                    let opposite = if positive { omin[n] } else { omax[n] };
                    if opposite == plane {
                        add_rect(&mut inner, [omin[u], omax[u], omin[v], omax[v]]);
                    }
                }
                if is_covered(&inner, rect) {
                    continue;
                }
                let border = plane == if positive { 16 } else { 0 };
                if border {
                    add_rect(&mut covers[face.index()], rect);
                }
                faces.push(BoxFace {
                    face,
                    index: index(b),
                    border,
                    rect,
                });
            }
        }
        let sturdy = Face::ALL
            .iter()
            .filter(|f| covers[f.index()] == [u16::MAX; 16])
            .fold(0, |bits, f| bits | 1 << f.index());
        // Down from the top layer while the boxes fill the layer's whole 16x16 section.
        let mut top_floor = NO_FLOOR;
        for y in (0..16).rev() {
            let mut layer: Cover = [0; 16];
            for b in boxes {
                let (min, max) = (b.bounds.min, b.bounds.max);
                if min[1] <= y && y < max[1] {
                    add_rect(&mut layer, [min[0], max[0], min[2], max[2]]);
                }
            }
            if layer != [u16::MAX; 16] {
                break;
            }
            top_floor = y;
        }
        Self {
            faces,
            covers,
            sturdy,
            top_floor,
        }
    }
}

impl BoxModels {
    /// The shape of `model`, made of `model_boxes`, interning its boxes and itself.
    fn intern(&mut self, model: BlockModel, model_boxes: &[ModelBox]) -> u16 {
        if let Some(&id) = self.shape_ids.get(&model) {
            return id;
        }
        let (boxes, ids) = (&mut self.boxes, &mut self.box_ids);
        let shape = BoxShape::new(model_boxes, |b| {
            *ids.entry(b).or_insert_with(|| {
                boxes.push(b);
                (boxes.len() - 1) as u16
            })
        });
        assert!(
            self.boxes.len() <= MAX_MODEL_BOXES,
            "more than {MAX_MODEL_BOXES} model boxes (the registry refuses them)"
        );
        let id = self.shapes.len() as u16;
        self.shapes.push(shape);
        self.shape_ids.insert(model, id);
        id
    }

    /// The shape of material `m`, if it is made of boxes.
    #[inline]
    fn of(&self, m: u16) -> Option<&BoxShape> {
        let id = *self.shape_of.get(usize::from(m))?;
        self.shapes.get(usize::from(id))
    }
}

/// The mesher class of a block drawn as `shape` with `model`. A fluid is transparent whatever
/// its shape: it hides no neighbor face, its top sits lower (lava is drawn opaque, ADR 0025).
fn class_of(shape: RenderShape, model: BlockModel) -> VoxelClass {
    match (shape, model.is_cube()) {
        (RenderShape::None, _) => VoxelClass::Empty,
        (_, false) => VoxelClass::Special,
        (_, true) if model == BlockModel::Fluid => VoxelClass::Transparent,
        (RenderShape::Opaque, true) => VoxelClass::Opaque,
        (RenderShape::Cutout | RenderShape::Translucent, true) => VoxelClass::Transparent,
    }
}

/// The shape codes of the 6 faces of a cube of `model` (turned faces for logs, fluid cube
/// faces for fluids).
fn cube_codes(model: BlockModel) -> [u8; 6] {
    Face::ALL.map(|f| {
        if model == BlockModel::Fluid {
            layout::SHAPE_FLUID_CUBE
        } else if model.face_turned(f) {
            layout::SHAPE_TURNED
        } else {
            layout::SHAPE_CUBE
        }
    })
}

impl MaterialTable {
    /// From the registry (material id = `BlockRegistry::material`).
    pub fn from_registry(registry: &BlockRegistry) -> Self {
        let n = registry.len();
        let mut table = Self {
            class: vec![VoxelClass::Empty; n],
            shape: vec![RenderShape::None; n],
            model: vec![BlockModel::CUBE; n],
            layers: vec![[0u16; 6]; n],
            codes: vec![[0u8; 6]; n],
            emissive: vec![false; n],
            lit: vec![LightValue::DARK; n],
            has_lit: false,
            has_fluid: false,
            fluid_source: (0..n as u16).collect(),
            fluid_level: vec![0; n],
            waterlogged: vec![false; n],
            box_models: BoxModels {
                shape_of: vec![NO_SHAPE; n],
                ..BoxModels::default()
            },
        };
        // The source of the fluid the waterlogged blocks hold.
        let water = registry
            .waterlog_fluid()
            .and_then(|id| registry.fluid(id))
            .map(|fluid| registry.material(fluid.first));
        for i in 0..n {
            let s = BlockState(i as u16);
            let m = registry.material(s) as usize;
            if m == 0 {
                continue;
            }
            table.shape[m] = registry.shape(s);
            table.layers[m] = Face::ALL.map(|f| registry.face_texture(s, f).0);
            table.emissive[m] = registry.is_full_bright(s);
            // An emitter that is not full bright is lit by its own light. A fluid never is: it
            // is full bright when it emits.
            if !table.emissive[m] {
                table.lit[m] = registry.emission(s);
                table.has_lit |= !table.lit[m].is_dark();
            }
            table.set_model(m as u16, registry.model(s), registry.model_boxes(s));
            // Every level of a fluid meshes as its source (level 0).
            if registry.model(s) == BlockModel::Fluid
                && let Some(level) = registry.values(s).index(FLUID_LEVEL)
                && let Some(source) = registry.with(s, FLUID_LEVEL, "0")
            {
                table.set_fluid_level(m as u16, registry.material(source), level as u8);
            }
            if registry.is_waterlogged(s)
                && let Some(water) = water
            {
                table.set_waterlogged(m as u16, water);
            }
        }
        table
    }
    /// Declares `material` (a block made of boxes) as waterlogged, holding a source of fluid
    /// `water` (ADR 0025): gather puts the water in its cell (it culls, merges and slopes as
    /// that fluid) and keeps the block in [`PaddedChunk::waterlogged`], whose boxes are drawn
    /// as for the dry block. The water's faces the block hides are not drawn (only an opaque
    /// block hides any, ADR 0038), the others sit [`layout::FLUID_INSET`] inside the cell (but
    /// the top).
    pub fn set_waterlogged(&mut self, material: u16, water: u16) {
        let i = usize::from(material);
        if i >= self.class.len() || usize::from(water) >= self.class.len() {
            return;
        }
        self.class[i] = VoxelClass::Transparent;
        self.fluid_source[i] = water;
        self.fluid_level[i] = 0;
        self.waterlogged[i] = true;
        self.has_fluid = true;
    }
    /// True if `material` is a waterlogged block ([`MaterialTable::set_waterlogged`]).
    pub fn is_waterlogged(&self, material: u16) -> bool {
        self.waterlogged
            .get(usize::from(material))
            .copied()
            .unwrap_or(false)
    }
    /// The faces (bit [`Face::index`]) of the water held by the waterlogged block `material`
    /// that the block itself hides, the water's top corners being `heights` (sixteenths), as
    /// Minecraft's liquid renderer does (ADR 0038). Only a block that occludes hides its water
    /// (`canOcclude`: here [`RenderShape::Opaque`]); a cutout or translucent one (roots, leaves)
    /// hides none. An opaque one hides the sides and the bottom its boxes cover whole on the
    /// cell border (`isFaceOccludedBySelf`: a slab's bottom, the back of stairs), and the top
    /// only where every corner lies inside its solid top or on it (at or above
    /// [`BoxShape::top_floor`]: under a top slab), where it would be hidden or lose its depth
    /// tie anyway (ADR 0025 sec. 6). 0 for a material not made of boxes.
    pub(crate) fn held_water_hidden(&self, material: u16, heights: [u8; 4]) -> u8 {
        if self.shape(material) != RenderShape::Opaque {
            return 0;
        }
        self.box_models.of(material).map_or(0, |shape| {
            let top = 1 << Face::PosY.index();
            let inside = heights.iter().all(|&h| h >= shape.top_floor);
            shape.sturdy & !top | if inside { top } else { 0 }
        })
    }
    /// Declares `material` as level `level` (`1..=15`; 0 is the source itself) of the fluid
    /// whose source is `source`: it meshes as `source` and its level shapes the corner heights
    /// (ADR 0021). Both must be fluids (`BlockModel::Fluid`) of the table.
    pub fn set_fluid_level(&mut self, material: u16, source: u16, level: u8) {
        let i = usize::from(material);
        if i >= self.class.len() || usize::from(source) >= self.class.len() {
            return;
        }
        self.fluid_source[i] = source;
        self.fluid_level[i] = level;
    }
    /// The material `material` meshes as and its fluid level (itself and 0 but for the levels of
    /// a fluid; out of range -> itself and 0).
    pub fn fluid_level(&self, material: u16) -> (u16, u8) {
        let i = usize::from(material);
        match (self.fluid_source.get(i), self.fluid_level.get(i)) {
            (Some(&source), Some(&level)) => (source, level),
            _ => (material, 0),
        }
    }
    /// Class of a material (out of range -> `Empty`).
    pub fn class(&self, material: u16) -> VoxelClass {
        self.class
            .get(material as usize)
            .copied()
            .unwrap_or(VoxelClass::Empty)
    }
    /// Model of a material (out of range -> the cube).
    pub fn model(&self, material: u16) -> BlockModel {
        self.model
            .get(material as usize)
            .copied()
            .unwrap_or(BlockModel::CUBE)
    }
    /// Sets the model of `material` and the boxes it is drawn with (`BlockRegistry::model_boxes`;
    /// none for cubes, crosses and fluids). Its class follows from its shape and model; a turned
    /// cube gets turned textures on its side faces. Materials with equal models share the boxes
    /// of the first one set. Tables built with [`MaterialTable::from_parts`] start with every
    /// material a cube.
    pub fn set_model(&mut self, material: u16, model: BlockModel, boxes: &[ModelBox]) {
        let i = material as usize;
        if i >= self.class.len() {
            return;
        }
        self.model[i] = model;
        self.class[i] = if self.waterlogged[i] {
            VoxelClass::Transparent
        } else {
            class_of(self.shape[i], model)
        };
        self.codes[i] = cube_codes(model);
        self.has_fluid |= model == BlockModel::Fluid;
        let made_of_boxes = self.shape[i] != RenderShape::None && !boxes.is_empty();
        self.box_models.shape_of[i] = if made_of_boxes {
            self.box_models.intern(model, boxes)
        } else {
            NO_SHAPE
        };
    }
    /// The model box table: every distinct [`ModelBox`] of the materials made of boxes, in the
    /// order first met (materials in id order). A `SHAPE_BOX` quad's
    /// [`PackedQuad::box_index`] indexes it; the renderer uploads it packed
    /// ([`crate::pack_model_box`]). At most [`MAX_MODEL_BOXES`] (the registry checks it).
    pub fn model_boxes(&self) -> &[ModelBox] {
        &self.box_models.boxes
    }
    /// True if gather marks fluid surface cells ([`FLUID_SURFACE_BIT`]): some material is a
    /// fluid and every material id is below the bit (else fluids keep a full-height top).
    fn marks_fluid_surfaces(&self) -> bool {
        self.has_fluid && self.class.len() <= usize::from(FLUID_SURFACE_BIT)
    }
    /// The material of a merge key's voxel id and whether it is a fluid surface cell.
    #[inline]
    fn split_surface(&self, material: u16) -> (u16, bool) {
        if !self.marks_fluid_surfaces() {
            return (material, false);
        }
        (
            material & !FLUID_SURFACE_BIT,
            material & FLUID_SURFACE_BIT != 0,
        )
    }
    /// Shape code of the cube faces of `material` facing `face` (`layout::SHAPE_CUBE` or
    /// `layout::SHAPE_TURNED`; out of range -> `SHAPE_CUBE`).
    pub fn cube_code(&self, material: u16, face: Face) -> u8 {
        self.codes
            .get(material as usize)
            .map_or(layout::SHAPE_CUBE, |c| c[face.index()])
    }
    /// Render shape of a material (out of range -> `None`).
    pub fn shape(&self, material: u16) -> RenderShape {
        self.shape
            .get(material as usize)
            .copied()
            .unwrap_or(RenderShape::None)
    }
    /// Bucket of a quad of `material` facing `face`: `Opaque` -> `face.index()`, `Cutout` ->
    /// [`CUTOUT_BUCKET`], `Translucent` -> [`TRANSLUCENT_BUCKET`] (`None` never produces quads).
    pub fn bucket(&self, material: u16, face: Face) -> usize {
        match self.shape(material) {
            RenderShape::Cutout => CUTOUT_BUCKET,
            RenderShape::Translucent => TRANSLUCENT_BUCKET,
            RenderShape::Opaque | RenderShape::None => face.index(),
        }
    }
    /// Bucket of the `face` quad of a sloped fluid cell of `material` whose top corners are
    /// `heights` high (`SHAPE_FLUID_SLOPE`): [`MaterialTable::bucket`], but the top of an opaque
    /// fluid (lava) whose corners are not all at one height goes to [`CUTOUT_BUCKET`] (ADR 0025).
    /// Such a top is not perpendicular to y: it faces eyes below its chunk on its downhill side,
    /// where the renderer skips the whole +Y bucket (a +Y bucket is only drawn from above the
    /// chunk's min y). The cutout bucket has no per-face test, and an opaque texture passes its
    /// alpha test.
    pub fn sloped_bucket(&self, material: u16, face: Face, heights: [u8; 4]) -> usize {
        let bucket = self.bucket(material, face);
        let level = heights.iter().all(|&h| h == heights[0]);
        if face == Face::PosY && bucket < CUTOUT_BUCKET && !level {
            CUTOUT_BUCKET
        } else {
            bucket
        }
    }
    /// Texture layer of `face`.
    pub fn layer(&self, material: u16, face: Face) -> u16 {
        self.layers
            .get(material as usize)
            .map_or(0, |l| l[face.index()])
    }
    /// True if the material is drawn full bright (`BlockRegistry::is_full_bright`): its quads
    /// get [`layout::FLAG_EMISSIVE`] (out of range -> `false`).
    pub fn emissive(&self, material: u16) -> bool {
        self.emissive
            .get(material as usize)
            .copied()
            .unwrap_or(false)
    }
    /// Marks `material` as emissive or not (tables built with [`MaterialTable::from_parts`]).
    pub fn set_emissive(&mut self, material: u16, emissive: bool) {
        let i = material as usize;
        if self.emissive.len() <= i {
            self.emissive.resize(i + 1, false);
        }
        self.emissive[i] = emissive;
    }
    /// The emission of `material` if it is a lit emitter, else dark (out of range too): a block
    /// that emits light and is not drawn full bright (ADR 0036). The mesher draws its faces as
    /// Minecraft draws such a block, flat: AO open and every corner the light of the voxel the
    /// face reads (the one in front of it, or the block's own for a cross and for a box face
    /// against an opaque cube), raised per channel to this emission. The shader keeps their face
    /// shade.
    #[inline]
    pub fn lit_emission(&self, material: u16) -> LightValue {
        self.lit
            .get(usize::from(material))
            .copied()
            .unwrap_or(LightValue::DARK)
    }
    /// Sets the [`MaterialTable::lit_emission`] of `material` (tables built with
    /// [`MaterialTable::from_parts`]; dark: not a lit emitter). For a material that is not
    /// drawn full bright and is not a fluid, as [`MaterialTable::from_registry`] gives them.
    pub fn set_lit_emission(&mut self, material: u16, emission: LightValue) {
        let i = usize::from(material);
        if self.lit.len() <= i {
            self.lit.resize(i + 1, LightValue::DARK);
        }
        self.lit[i] = emission.block();
        self.has_lit = self.lit.iter().any(|e| !e.is_dark());
    }
    /// True if some material is a lit emitter ([`MaterialTable::lit_emission`]).
    pub fn has_lit_emitters(&self) -> bool {
        self.has_lit
    }
    /// Test helper: explicit table (`shape[i]` and `layers[i]` for material `i`; every material
    /// is a cube until [`MaterialTable::set_model`]; the bgm class is derived from the shape and
    /// model exactly as in [`MaterialTable::from_registry`]; no material is emissive until
    /// [`MaterialTable::set_emissive`], nor a lit emitter until
    /// [`MaterialTable::set_lit_emission`]).
    pub fn from_parts(shape: Vec<RenderShape>, layers: Vec<[u16; 6]>) -> Self {
        assert!(shape.len() == layers.len());
        let n = shape.len();
        let class = shape
            .iter()
            .map(|&s| class_of(s, BlockModel::CUBE))
            .collect();
        Self {
            class,
            emissive: vec![false; n],
            lit: vec![LightValue::DARK; n],
            has_lit: false,
            model: vec![BlockModel::CUBE; n],
            codes: vec![[0; 6]; n],
            has_fluid: false,
            fluid_source: (0..n as u16).collect(),
            fluid_level: vec![0; n],
            waterlogged: vec![false; n],
            box_models: BoxModels {
                shape_of: vec![NO_SHAPE; n],
                ..BoxModels::default()
            },
            shape,
            layers,
        }
    }
    /// Mesher voxel value and class of a block state. M0 identity map (as
    /// `BlockRegistry::material`): the table is indexed by material id, and the material id of
    /// a state with geometry is the state id itself; states without geometry (and ids outside
    /// the table) become `0` / `Empty`. The levels of a fluid become their source's material
    /// after the gather ([`remap_fluid_levels`]).
    #[inline]
    fn resolve(&self, state: BlockState) -> (u16, VoxelClass) {
        match self.class(state.0) {
            VoxelClass::Empty => (0, VoxelClass::Empty),
            class => (state.0, class),
        }
    }
}

/// Center chunk plus its 26 neighbors in `boxel_voxel::NEIGHBOR_OFFSETS` order, and the light of
/// the padded chunk. A `None` neighbor is treated as empty (air).
pub struct MeshInput<'a> {
    /// Chunk to mesh.
    pub center: &'a ChunkData,
    /// Neighbors.
    pub neighbors: [Option<&'a ChunkData>; 26],
    /// Light of the chunk and its 1-voxel border (`boxel_light::compute`). `None`: open sky
    /// (sky 15) everywhere.
    pub light: Option<&'a PaddedLight>,
}

/// Mesh scratch other than the padded input (borrowed apart from it by `mesh_chunk`).
struct Scratch {
    greedy: Greedy,
    /// Quads of the per-block path (non-cube blocks) and their buckets.
    specials: Vec<(u8, PackedQuad)>,
    /// Merge key per padded voxel, written per face at that face's visible voxels only.
    keys: Box<[u32]>,
    palette: LightPalette,
    key_table: KeyTable,
    /// Fluid surface cells with their own corner heights (ADR 0021).
    fluid: FluidScratch,
    /// A uniform light spread over every padded voxel, for a chunk with a lit emitter (made on
    /// first use, kept).
    expanded: Option<Box<[u16; PADDED_VOLUME]>>,
}

/// Reusable mesher (one per worker thread; about 0.6 MiB of scratch plus the quad lists).
pub struct Mesher {
    scratch: Scratch,
    padded: PaddedChunk,
    /// `ChunkData::to_dense` target for non-uniform center chunks (`CHUNK_VOLUME` states).
    dense: Box<[BlockState]>,
}

impl Default for Mesher {
    fn default() -> Self {
        Self::new()
    }
}

impl Mesher {
    /// New mesher.
    pub fn new() -> Self {
        Self {
            scratch: Scratch {
                greedy: Greedy::new(),
                specials: Vec::new(),
                keys: vec![0; PADDED_VOLUME].into_boxed_slice(),
                palette: LightPalette::new(),
                key_table: KeyTable::new(),
                fluid: FluidScratch::new(),
                expanded: None,
            },
            padded: PaddedChunk::new(),
            dense: vec![BlockState::AIR; CHUNK_VOLUME].into_boxed_slice(),
        }
    }

    /// Fills `out` from the center chunk and the border voxels of its neighbors (face
    /// neighbors: 32x32 slabs, edges: 32 rows, corners: 1 voxel). Uniform chunks take a fast path.
    /// `input.light` is not read.
    pub fn gather(
        &mut self,
        input: &MeshInput<'_>,
        materials: &MaterialTable,
        out: &mut PaddedChunk,
    ) {
        gather_into(&mut self.dense, input, materials, out);
    }

    /// Meshes a padded chunk lit by `light` (`None`: open sky everywhere) and writes `out`
    /// bucket-contiguous with its light palette and hash.
    ///
    /// Face culling and greedy merging are binary greedy meshing (an in-repo fork, `greedy.rs`).
    /// Two unit faces merge only if they have the same material, the same AO byte and the same 4
    /// corner lights ([`reference_face_shading`] defines both, per `options.ao`; the faces of a
    /// lit emitter are flat, [`reference_lit_emitter_shading`]). Each quad gets
    /// `MaterialTable::layer` of its material and face (and [`layout::FLAG_EMISSIVE`] if
    /// `MaterialTable::emissive`) and is routed to `MaterialTable::bucket`.
    /// Corner lights go to the palette in first-seen order; past 4096 distinct entries the chunk
    /// is meshed again at the next [`LightQuantization`] level.
    pub fn mesh(
        &mut self,
        padded: &PaddedChunk,
        light: Option<&PaddedLight>,
        materials: &MaterialTable,
        options: MeshOptions,
        out: &mut ChunkMesh,
    ) {
        mesh_into(&mut self.scratch, padded, light, materials, options, out);
    }

    /// `gather` + `mesh`. Fast paths: center uniform air -> empty mesh; center uniform opaque with
    /// all 6 face neighbors uniform opaque -> empty mesh.
    pub fn mesh_chunk(
        &mut self,
        input: &MeshInput<'_>,
        materials: &MaterialTable,
        options: MeshOptions,
    ) -> ChunkMesh {
        let mut out = ChunkMesh::default();
        if trivially_empty(input, materials) {
            out.hash = out.compute_hash();
            return out;
        }
        gather_into(&mut self.dense, input, materials, &mut self.padded);
        mesh_into(
            &mut self.scratch,
            &self.padded,
            input.light,
            materials,
            options,
            &mut out,
        );
        out
    }
}

/// True when the mesh is empty without looking at any voxel: the center is uniform air, or it
/// is uniform opaque and all six face neighbors are uniform opaque too.
fn trivially_empty(input: &MeshInput<'_>, materials: &MaterialTable) -> bool {
    let uniform_class =
        |chunk: &ChunkData| chunk.as_uniform().map(|state| materials.resolve(state).1);
    match uniform_class(input.center) {
        Some(VoxelClass::Empty) => true,
        Some(VoxelClass::Opaque) => Face::ALL.iter().all(|face| {
            neighbor_index(face.normal())
                .and_then(|i| input.neighbors[i])
                .and_then(uniform_class)
                == Some(VoxelClass::Opaque)
        }),
        Some(VoxelClass::Transparent | VoxelClass::Special) | None => false,
    }
}

/// [`Mesher::gather`] body. Borrows only the dense scratch, so `mesh_chunk` can gather into the
/// mesher's own padded buffer.
fn gather_into(
    dense: &mut [BlockState],
    input: &MeshInput<'_>,
    materials: &MaterialTable,
    out: &mut PaddedChunk,
) {
    out.clear();
    gather_center(dense, input.center, materials, out);
    // Only the inner 32^3 is meshed, so only the center's lit emitters count.
    out.lit_emitters = materials.has_lit && may_hold_lit_emitter(input.center, materials);
    for (offset, neighbor) in NEIGHBOR_OFFSETS.iter().zip(input.neighbors) {
        if let Some(chunk) = neighbor {
            gather_border([offset.x, offset.y, offset.z], chunk, materials, out);
        }
    }
    if materials.has_fluid {
        remap_fluid_levels(materials, out);
    }
    if materials.marks_fluid_surfaces() {
        mark_fluid_surfaces(materials, out);
    }
}

/// True if `chunk` may hold a lit emitter ([`MaterialTable::lit_emission`]): a palette scan, so
/// it may answer yes for an entry no longer used, but never no for a chunk that holds one.
/// `Direct` storage answers yes.
fn may_hold_lit_emitter(chunk: &ChunkData, materials: &MaterialTable) -> bool {
    // The material of a state with geometry is its id; the others have none and are not lit.
    let lit = |s: &BlockState| !materials.lit_emission(s.0).is_dark();
    match chunk.blocks() {
        Paletted::Uniform(s) => lit(s),
        Paletted::Packed { palette, .. } => palette.iter().any(lit),
        Paletted::Direct(_) => true,
    }
}

/// Every level of a fluid becomes its source's material, so the levels cull and merge as one
/// fluid (ADR 0021); the levels other than 0 go to [`PaddedChunk::fluid_levels`]. A waterlogged
/// block becomes its water and goes to [`PaddedChunk::waterlogged`] (ADR 0025). Visits only
/// the transparent voxels (fluids are transparent cubes), so a chunk without water pays one read
/// per column.
fn remap_fluid_levels(materials: &MaterialTable, out: &mut PaddedChunk) {
    for y in -1..=CHUNK_SIZE {
        for x in -1..=CHUNK_SIZE {
            let mut bits = out.transparent[mask_row(x, y)];
            while bits != 0 {
                // Mask bit `b` is padded z `b - 1`.
                let z = bits.trailing_zeros() as i32 - 1;
                bits &= bits - 1;
                let i = PaddedChunk::index(x, y, z);
                let id = out.voxels[i];
                let (source, level) = materials.fluid_level(id);
                if level != 0 {
                    out.voxels[i] = source;
                    out.fluid_levels.push((i as u32, level));
                } else if source != id && materials.is_waterlogged(id) {
                    // In index order (y, then x, then z): the list stays sorted.
                    out.voxels[i] = source;
                    out.waterlogged.push((i as u32, id));
                }
            }
        }
    }
}

/// Marks the fluid surface cells of the inner 32^3 with [`FLUID_SURFACE_BIT`]: the fluid voxels
/// whose voxel above is neither the same fluid nor opaque. Under an opaque block the fluid stays
/// full height, so no gap shows under it (its top face is culled there).
fn mark_fluid_surfaces(materials: &MaterialTable, out: &mut PaddedChunk) {
    out.cull_mask = !FLUID_SURFACE_BIT;
    for y in 0..CHUNK_SIZE {
        for x in 0..CHUNK_SIZE {
            // Inner voxels only (mask bit of padded z is z + 1).
            let mut bits = (out.transparent[mask_row(x, y)] >> 1) & u64::from(u32::MAX);
            let opaque_above = out.opaque[mask_row(x, y + 1)] >> 1;
            while bits != 0 {
                let z = bits.trailing_zeros() as i32;
                bits &= bits - 1;
                let i = PaddedChunk::index(x, y, z);
                let m = out.voxels[i];
                if materials.model(m) != BlockModel::Fluid || opaque_above >> z & 1 != 0 {
                    continue;
                }
                let above = out.voxels[PaddedChunk::index(x, y + 1, z)] & !FLUID_SURFACE_BIT;
                if above != m {
                    out.voxels[i] = m | FLUID_SURFACE_BIT;
                }
            }
        }
    }
}

/// Copies the inner 32^3 voxels. A uniform chunk fills whole rows; any other is decoded once
/// with `ChunkData::to_dense` and copied row by row (the local index `(y << 10) | (x << 5) | z`
/// and the padded layout both run along z, so dense row `y * 32 + x` is one padded column).
fn gather_center(
    dense: &mut [BlockState],
    center: &ChunkData,
    materials: &MaterialTable,
    out: &mut PaddedChunk,
) {
    const INNER: Range<i32> = 0..CHUNK_SIZE;
    if let Some(state) = center.as_uniform() {
        let (material, class) = materials.resolve(state);
        fill_region(out, [INNER, INNER, INNER], material, class);
        return;
    }
    center.to_dense(dense);
    let row_len = CHUNK_SIZE as usize;
    for (r, row) in dense.chunks_exact(row_len).enumerate() {
        let (x, y) = ((r % row_len) as i32, (r / row_len) as i32);
        write_run(out, materials, x, y, 0, row.iter().copied());
    }
}

/// Copies the part of the neighbor at `offset` (components in -1..=1) that lies in the padding:
/// a 32x32 slab for a face neighbor, a row of 32 for an edge neighbor, one voxel for a corner.
fn gather_border(
    offset: [i32; 3],
    chunk: &ChunkData,
    materials: &MaterialTable,
    out: &mut PaddedChunk,
) {
    let spans = offset.map(border_span);
    if let Some(state) = chunk.as_uniform() {
        let (material, class) = materials.resolve(state);
        fill_region(out, spans.map(|(range, _)| range), material, class);
        return;
    }
    let [(xs, sx), (ys, sy), (zs, sz)] = spans;
    for y in ys {
        for x in xs.clone() {
            let (lx, ly) = ((x + sx) as u32, (y + sy) as u32);
            let states = zs
                .clone()
                .map(|z| chunk.get(LocalPos::new(lx, ly, (z + sz) as u32)));
            write_run(out, materials, x, y, zs.start, states);
        }
    }
}

/// Writes a run of states along z, starting at padded `(x, y, z0)`, into a clear `out`: material
/// ids through the table, mask bits gathered per run. Consecutive equal states (common along
/// terrain columns) reuse the previous table lookup.
#[inline]
fn write_run(
    out: &mut PaddedChunk,
    materials: &MaterialTable,
    x: i32,
    y: i32,
    z0: i32,
    states: impl ExactSizeIterator<Item = BlockState>,
) {
    let start = PaddedChunk::index(x, y, z0);
    let dst = &mut out.voxels[start..start + states.len()];
    let (mut opaque, mut transparent, mut special) = (0u64, 0u64, 0u64);
    let mut last = BlockState(u16::MAX);
    let mut resolved = materials.resolve(last);
    for (z, (slot, state)) in dst.iter_mut().zip(states).enumerate() {
        if state != last {
            last = state;
            resolved = materials.resolve(state);
        }
        let (material, class) = resolved;
        *slot = material;
        opaque |= u64::from(class == VoxelClass::Opaque) << z;
        transparent |= u64::from(class == VoxelClass::Transparent) << z;
        special |= u64::from(class == VoxelClass::Special) << z;
    }
    // Mask bit of padded z is z + 1.
    let row = mask_row(x, y);
    out.opaque[row] |= opaque << (z0 + 1);
    out.transparent[row] |= transparent << (z0 + 1);
    out.special[row] |= special << (z0 + 1);
}

/// Padded-space range covered by a neighbor at `d` (-1, 0 or 1) along one axis, and the shift
/// that turns a padded coordinate of that range into the neighbor's local coordinate.
fn border_span(d: i32) -> (Range<i32>, i32) {
    match d {
        -1 => (-1..0, CHUNK_SIZE),
        0 => (0..CHUNK_SIZE, 0),
        _ => (CHUNK_SIZE..CHUNK_SIZE + 1, -CHUNK_SIZE),
    }
}

/// Fills a padded-space box (`[xs, ys, zs]`, each within -1..=32, non-empty) with one material.
/// `out` must be clear inside the box (`gather` clears first), so `Empty` writes nothing.
fn fill_region(
    out: &mut PaddedChunk,
    [xs, ys, zs]: [Range<i32>; 3],
    material: u16,
    class: VoxelClass,
) {
    let masks = match class {
        VoxelClass::Empty => return,
        VoxelClass::Opaque => &mut out.opaque,
        VoxelClass::Transparent => &mut out.transparent,
        VoxelClass::Special => &mut out.special,
    };
    let len = zs.len();
    // Mask bit of padded z is z + 1.
    let bits = (u64::MAX >> (64 - len)) << (zs.start + 1);
    for y in ys {
        for x in xs.clone() {
            masks[mask_row(x, y)] |= bits;
            let start = PaddedChunk::index(x, y, zs.start);
            out.voxels[start..start + len].fill(material);
        }
    }
}

/// Column-mask row of padded `(x, y)` (each in -1..=32).
#[inline]
pub(crate) const fn mask_row(x: i32, y: i32) -> usize {
    (x + 1) as usize + (y + 1) as usize * PADDED
}

/// How the merge keys of a mesh decode back to a [`FaceKey`].
#[derive(Clone, Copy, Debug)]
enum KeyKind {
    /// The key is the material (AO open, palette index 0).
    Material,
    /// `material << 8 | ao` (palette index 0).
    MaterialAo,
    /// A [`KeyTable`] id.
    Table,
}

/// [`Mesher::mesh`] body. Borrows only the scratch, so `mesh_chunk` can pass the mesher's own
/// padded buffer.
fn mesh_into(
    s: &mut Scratch,
    padded: &PaddedChunk,
    light: Option<&PaddedLight>,
    materials: &MaterialTable,
    options: MeshOptions,
    out: &mut ChunkMesh,
) {
    out.clear();
    s.greedy.cull(
        &padded.voxels,
        &padded.opaque,
        &padded.transparent,
        padded.cull_mask,
    );
    // Sloped fluid cells leave the greedy pass (ADR 0021).
    find_sloped(padded, materials, &mut s.greedy, &mut s.fluid);
    let elide = options.ao == AoPolicy::BakedLitOnly;
    // The faces of lit emitters are flat (ADR 0036). Only the full-light path draws them, so a
    // uniform light around one (no light, or a chunk all of one emitter) is spread first.
    let glow = padded.lit_emitters && materials.has_lit;
    let full = match light {
        None => Err(LightValue::FULL_SKY),
        Some(PaddedLight::Uniform(v)) => Err(*v),
        Some(PaddedLight::Full(values)) => Ok(&**values),
    };
    let mut expanded = None;
    let full = match full {
        Err(v) if glow => {
            let mut values = s.expanded.take().unwrap_or_else(|| {
                vec![0; PADDED_VOLUME]
                    .into_boxed_slice()
                    .try_into()
                    .expect("PADDED_VOLUME values")
            });
            values.fill(v.0);
            Ok(&**expanded.insert(values))
        }
        full => full,
    };
    let kind = match full {
        // Uniform light: one palette entry, the key is the material and maybe the AO.
        Err(v) => {
            s.greedy.clear();
            let ao = match options.ao {
                AoPolicy::Off => false,
                AoPolicy::Baked => true,
                AoPolicy::BakedLitOnly => !v.is_dark(),
            };
            for face in 0..6 {
                if ao {
                    let keys = &mut s.keys;
                    // Never fails: no palette here.
                    let _ = visit_face(face, &s.greedy, padded, |i, _, nb9| {
                        let ao = AO_TABLE[neighborhood8(nb9)];
                        keys[i] = u32::from(padded.voxels[i]) << 8 | u32::from(ao);
                        Ok(())
                    });
                    s.greedy.merge_face(face, &s.keys);
                } else {
                    s.greedy.merge_face(face, &padded.voxels);
                }
            }
            // Never fails: no palette here.
            let _ = emit_specials(padded, materials, &mut s.specials, |_, _, _, nb9| {
                Ok(match nb9 {
                    Some(nb9) if ao => (AO_TABLE[neighborhood8(nb9)], 0),
                    _ => (AO_OPEN_BYTE, 0),
                })
            });
            let _ = emit_sloped(
                &s.fluid.cells,
                padded,
                materials,
                &mut s.specials,
                |_, _, _, _| Ok((AO_OPEN_BYTE, 0)),
            );
            if s.greedy.quads.iter().any(|q| !q.is_empty()) || !s.specials.is_empty() {
                out.palette.push(palette_entry([v; 4]));
            }
            if ao {
                KeyKind::MaterialAo
            } else {
                KeyKind::Material
            }
        }
        Ok(values) => {
            // `Flat` always fits, so some level succeeds.
            for level in LightQuantization::ALL {
                s.greedy.clear();
                s.palette.clear();
                s.key_table.clear();
                let meshed = if glow {
                    mesh_lit::<true>(s, padded, values, materials, options.ao, elide, level)
                } else {
                    mesh_lit::<false>(s, padded, values, materials, options.ao, elide, level)
                };
                if meshed.is_ok() {
                    out.light_quantization = level;
                    break;
                }
            }
            out.palette.extend_from_slice(s.palette.entries());
            KeyKind::Table
        }
    };
    let (greedy, specials) = (&s.greedy, &s.specials);
    match kind {
        KeyKind::Material => assemble(greedy, specials, materials, out, |k| FaceKey {
            material: k as u16,
            ao: AO_OPEN_BYTE,
            light: 0,
        }),
        KeyKind::MaterialAo => assemble(greedy, specials, materials, out, |k| FaceKey {
            material: (k >> 8) as u16,
            ao: k as u8,
            light: 0,
        }),
        KeyKind::Table => assemble(greedy, specials, materials, out, |k| s.key_table.key(k)),
    }
    out.hash = out.compute_hash();
    if expanded.is_some() {
        s.expanded = expanded;
    }
}

/// Fills the keys and merges every face under full light at quantization `level`, then adds the
/// quads of the non-cube blocks. `Err` when the palette overflows (the caller tries the next
/// level). With `GLOW` (the chunk may hold a lit emitter), the faces of the lit emitters are
/// flat ([`flat_shading`]); without it no face asks, so the hot path stays as it was.
fn mesh_lit<const GLOW: bool>(
    s: &mut Scratch,
    padded: &PaddedChunk,
    light: &[u16; PADDED_VOLUME],
    materials: &MaterialTable,
    ao: AoPolicy,
    elide: bool,
    level: LightQuantization,
) -> Result<(), PaletteFull> {
    for face in 0..6 {
        let (keys, palette, table) = (&mut s.keys, &mut s.palette, &mut s.key_table);
        visit_face(face, &s.greedy, padded, |i, front, nb9| {
            let material = padded.voxels[i];
            // A fluid surface id (`FLUID_SURFACE_BIT`) is past the table: never lit.
            let emission = if GLOW {
                materials.lit_emission(material)
            } else {
                LightValue::DARK
            };
            let (ao, corners) = if GLOW && !emission.is_dark() {
                flat_shading(light, front, emission, level)
            } else {
                lit_shading(light, front, face, nb9, ao, elide, level)
            };
            keys[i] = table.id(material, ao, corners, palette)?;
            Ok(())
        })?;
        s.greedy.merge_face(face, &s.keys);
    }
    let palette = &mut s.palette;
    let mut shade = |material, cell, face: Face, nb9| {
        let emission = if GLOW {
            materials.lit_emission(material)
        } else {
            LightValue::DARK
        };
        let (ao, corners) = match nb9 {
            _ if GLOW && !emission.is_dark() => flat_shading(light, cell, emission, level),
            Some(nb9) => lit_shading(light, cell, face.index(), nb9, ao, elide, level),
            // A cross: the light of its own cell, AO open.
            None => (AO_OPEN_BYTE, level.apply([LightValue(light[cell]); 4])),
        };
        Ok((ao, palette.index(corners)?))
    };
    emit_specials(padded, materials, &mut s.specials, &mut shade)?;
    emit_sloped(
        &s.fluid.cells,
        padded,
        materials,
        &mut s.specials,
        &mut shade,
    )
}

/// AO byte and corner lights of a unit face under full light: `front` is the padded index of
/// the voxel the face looks at and `nb9` the opacity of the front layer around it (see
/// [`visit_face`]), `face` its index.
#[inline]
fn lit_shading(
    light: &[u16; PADDED_VOLUME],
    front: usize,
    face: usize,
    nb9: u32,
    ao: AoPolicy,
    elide: bool,
    level: LightQuantization,
) -> (u8, [LightValue; 4]) {
    let (su, sv) = LIGHT_STRIDES[face];
    let ao = match ao {
        AoPolicy::Off => AO_OPEN_BYTE,
        AoPolicy::Baked | AoPolicy::BakedLitOnly => AO_TABLE[neighborhood8(nb9)],
    };
    // Cell (du, dv) in -1..=1 is `cells[(dv + 1) * 3 + (du + 1)]`.
    let corner = front - sv - su;
    let cells: [u16; 9] = std::array::from_fn(|c| light[corner + (c / 3) * sv + (c % 3) * su]);
    // Equal cells (open sky, dark caves): every corner is that value.
    let corners = level.apply(if cells.iter().all(|&c| c == cells[4]) {
        [LightValue(cells[4]); 4]
    } else {
        face_corner_lights(&cells, nb9)
    });
    let ao = if elide && corners.iter().all(|c| c.is_dark()) {
        AO_OPEN_BYTE
    } else {
        ao
    };
    (ao, corners)
}

/// AO byte and corner lights of a face of a lit emitter ([`MaterialTable::lit_emission`],
/// ADR 0036), the way Minecraft lights a block that emits: flat, AO open, every corner the
/// light of `cell` (the voxel whose light the face reads) raised per channel to `emission`.
#[inline]
fn flat_shading(
    light: &[u16; PADDED_VOLUME],
    cell: usize,
    emission: LightValue,
    level: LightQuantization,
) -> (u8, [LightValue; 4]) {
    let flat = LightValue(light[cell]).max(emission);
    (AO_OPEN_BYTE, level.apply([flat; 4]))
}

/// The quads of the non-cube blocks of the inner 32^3 (ADR 0018, ADR 0020), one block at a time,
/// into `out` with their buckets (cleared first). `shade(material, cell, face, nb9)` gives the
/// AO byte and light palette index of a quad of the block of `material`: for a box face, `cell`
/// is the padded index of the voxel whose layer shades it and `nb9` the opacity of that 3x3
/// layer ([`box_face_layer`]), as for the cube face there; for a cross, `cell` is the block's
/// own voxel and `nb9` is `None`.
///
/// A model made of boxes (slabs, stairs, fences, torches) draws one `SHAPE_BOX` quad per box
/// face its other boxes do not cover. A face flush with the cell border is hidden by an opaque
/// cube in front of it, or by a neighbor made of boxes whose faces on that border cover it, if
/// that neighbor is opaque or the same material (two bottom slabs side by side, a stair's back
/// against a full stair face). Faces inside the cell are always drawn. Cubes never hide against
/// a box model (the greedy pass sees it as empty), so a cube face behind a stair's back is drawn
/// and hidden by depth. A cross gives two quads (faces `PosX` and `PosZ`, see
/// [`layout::SHAPE_CROSS`]) with the texture of its side. The waterlogged blocks of the inner
/// 32^3 ([`PaddedChunk::waterlogged`], ADR 0025) draw their boxes the same way, after the rest.
fn emit_specials(
    padded: &PaddedChunk,
    materials: &MaterialTable,
    out: &mut Vec<(u8, PackedQuad)>,
    mut shade: impl FnMut(u16, usize, Face, Option<u32>) -> Result<(u8, u16), PaletteFull>,
) -> Result<(), PaletteFull> {
    out.clear();
    // `index`: the model box of a `SHAPE_BOX` quad (0 is `w = h = 1` for the others).
    let mut push =
        |m: u16, pos: [u32; 3], face: Face, layer_face: Face, code: u8, index, shading| {
            let (ao, light): (u8, u16) = shading;
            let mut quad = PackedQuad::new(pos, 1, 1, face, materials.layer(m, layer_face))
                .with_ao_byte(ao)
                .with_light_index(light)
                .with_shape(code)
                .with_box_index(index);
            if materials.emissive(m) {
                quad.geo |= layout::FLAG_EMISSIVE << layout::GEO_FLAGS_SHIFT;
            }
            out.push((materials.bucket(m, face) as u8, quad));
        };
    let models = &materials.box_models;
    // The quads of block `m` at inner `[x, y, z]`.
    let mut block = |m: u16, [x, y, z]: [i32; 3]| {
        let pos = [x as u32, y as u32, z as u32];
        if let Some(shape) = models.of(m) {
            // Each direction shades once per block.
            let mut shaded: [Option<(u8, u16)>; 6] = [None; 6];
            for bf in &shape.faces {
                let face = bf.face;
                if bf.border && box_face_hidden(padded, materials, m, [x, y, z], *bf) {
                    continue;
                }
                let shading = match shaded[face.index()] {
                    Some(shading) => shading,
                    None => {
                        let (cell, nb9) = box_face_layer(padded, [x, y, z], face);
                        let shading = shade(m, cell, face, Some(nb9))?;
                        shaded[face.index()] = Some(shading);
                        shading
                    }
                };
                push(m, pos, face, face, layout::SHAPE_BOX, bf.index, shading);
            }
        } else if materials.model(m) == BlockModel::Cross {
            let i = PaddedChunk::index(x, y, z);
            for face in [Face::PosX, Face::PosZ] {
                let shading = shade(m, i, face, None)?;
                push(m, pos, face, Face::PosX, layout::SHAPE_CROSS, 0, shading);
            }
        }
        Ok(())
    };
    for y in 0..CHUNK_SIZE {
        for x in 0..CHUNK_SIZE {
            // Inner voxels only (mask bit of padded z is z + 1).
            let mut bits = (padded.special[mask_row(x, y)] >> 1) & u64::from(u32::MAX);
            while bits != 0 {
                let z = bits.trailing_zeros() as i32;
                bits &= bits - 1;
                block(padded.voxels[PaddedChunk::index(x, y, z)], [x, y, z])?;
            }
        }
    }
    for &(i, m) in &padded.waterlogged {
        let p = padded_pos(i as usize);
        if p.iter().all(|c| (0..CHUNK_SIZE).contains(c)) {
            block(m, p)?;
        }
    }
    Ok(())
}

/// Padded `(x, y, z)` (each in `-1..=32`) of padded index `i`.
#[inline]
pub(crate) const fn padded_pos(i: usize) -> [i32; 3] {
    let (z, rest) = (i % PADDED, i / PADDED);
    let (x, y) = (rest % PADDED, rest / PADDED);
    [x as i32 - 1, y as i32 - 1, z as i32 - 1]
}

/// True if the neighbor in front of `bf` (a face of material `m` at inner `p`, flush with the
/// cell border) hides it: an opaque cube, or a model made of boxes (dry or waterlogged),
/// opaque or of material `m`, whose faces on that border cover it.
fn box_face_hidden(
    padded: &PaddedChunk,
    materials: &MaterialTable,
    m: u16,
    [x, y, z]: [i32; 3],
    bf: BoxFace,
) -> bool {
    let n = bf.face.normal();
    let (nx, ny, nz) = (x + n.x, y + n.y, z + n.z);
    let nm = match class_from_masks(padded, nx, ny, nz) {
        VoxelClass::Opaque => return true,
        VoxelClass::Special => padded.get(nx, ny, nz),
        VoxelClass::Transparent => match padded.waterlogged_at(PaddedChunk::index(nx, ny, nz)) {
            Some(block) => block,
            None => return false,
        },
        VoxelClass::Empty => return false,
    };
    let hides = nm == m || materials.shape(nm) == RenderShape::Opaque;
    hides
        && materials
            .box_models
            .of(nm)
            .is_some_and(|other| is_covered(&other.covers[bf.face.opposite().index()], bf.rect))
}

/// The voxel whose layer shades `face` of the box model at inner `p`, and the opacity of that
/// 3x3 layer (`PaddedChunk::opaque`, bit `(dv + 1) * 3 + (du + 1)`): the voxel in front, as for
/// the cube face there, unless that voxel is an opaque cube, then the block's own voxel (a fence
/// post's face against a wall, a stair's step under a block: inside the cell, where light still
/// reaches, as Minecraft shades faces off the cell border). A face on the border with an opaque
/// cube in front is hidden, so only inner faces take their own voxel.
fn box_face_layer(padded: &PaddedChunk, [x, y, z]: [i32; 3], face: Face) -> (usize, u32) {
    let n = face.normal();
    let (nx, ny, nz) = (x + n.x, y + n.y, z + n.z);
    if class_from_masks(padded, nx, ny, nz) == VoxelClass::Opaque {
        // The block's own layer (rare: read cell by cell).
        let (_, ua, va) = face.axes();
        let mut nb9 = 0;
        for dv in -1..=1 {
            for du in -1..=1 {
                let mut c = [x, y, z];
                c[ua] += du;
                c[va] += dv;
                let opaque = class_from_masks(padded, c[0], c[1], c[2]) == VoxelClass::Opaque;
                nb9 |= u32::from(opaque) << ((dv + 1) * 3 + (du + 1));
            }
        }
        (PaddedChunk::index(x, y, z), nb9)
    } else {
        let nb9 = front_nb9(face, &padded.opaque, x as usize, y as usize, z as usize);
        (PaddedChunk::index(nx, ny, nz), nb9)
    }
}

/// [`front_neighborhood`] of a face given at run time.
pub(crate) fn front_nb9(face: Face, opaque: &[u64], x: usize, y: usize, z: usize) -> u32 {
    match face {
        Face::PosY => front_neighborhood::<0>(opaque, x, y, z),
        Face::NegY => front_neighborhood::<1>(opaque, x, y, z),
        Face::PosX => front_neighborhood::<2>(opaque, x, y, z),
        Face::NegX => front_neighborhood::<3>(opaque, x, y, z),
        Face::PosZ => front_neighborhood::<4>(opaque, x, y, z),
        Face::NegZ => front_neighborhood::<5>(opaque, x, y, z),
    }
}

/// Padded-index strides along the u and v axes of each face ([`Face::axes`]: ±Y -> x, z;
/// ±X -> z, y; ±Z -> x, y).
const LIGHT_STRIDES: [(usize, usize); 6] = [
    (PADDED, 1),
    (PADDED, 1),
    (1, PADDED_AREA),
    (1, PADDED_AREA),
    (PADDED, PADDED_AREA),
    (PADDED, PADDED_AREA),
];

/// Calls `visit(voxel index, front index, nb9)` for every visible unit face of `face` (after
/// `Greedy::cull`): the padded indices of the voxel and of the voxel in front of the face, and
/// the opacity (`PaddedChunk::opaque`) of the 3x3 front-layer neighborhood (bit
/// `(dv + 1) * 3 + (du + 1)`).
#[inline]
fn visit_face(
    face: usize,
    greedy: &Greedy,
    padded: &PaddedChunk,
    visit: impl FnMut(usize, usize, u32) -> Result<(), PaletteFull>,
) -> Result<(), PaletteFull> {
    let rows = greedy.face_rows(face);
    let opaque = &padded.opaque;
    match face {
        0 => visit_rows::<0>(rows, opaque, visit),
        1 => visit_rows::<1>(rows, opaque, visit),
        2 => visit_rows::<2>(rows, opaque, visit),
        3 => visit_rows::<3>(rows, opaque, visit),
        4 => visit_rows::<4>(rows, opaque, visit),
        _ => visit_rows::<5>(rows, opaque, visit),
    }
}

/// [`visit_face`] for face `F`.
#[inline]
fn visit_rows<const F: usize>(
    rows: &[u64],
    opaque: &[u64],
    mut visit: impl FnMut(usize, usize, u32) -> Result<(), PaletteFull>,
) -> Result<(), PaletteFull> {
    // Padded index offset of the voxel in front.
    let normal: isize = match F {
        0 => PADDED_AREA as isize,
        1 => -(PADDED_AREA as isize),
        2 => PADDED as isize,
        3 => -(PADDED as isize),
        4 => 1,
        _ => -1,
    };
    for (r, &bits) in rows.iter().enumerate() {
        if bits == 0 {
            continue;
        }
        // Unpadded x and y of the row (see `Greedy::face_rows`).
        let (x, y) = if F == 2 || F == 3 {
            (r / 32, r % 32)
        } else {
            (r % 32, r / 32)
        };
        let mut bits = bits;
        while bits != 0 {
            let bit = bits.trailing_zeros() as usize;
            bits &= bits - 1;
            let z = if F >= 4 { bit - 1 } else { bit };
            let i = (z + 1) + (x + 1) * PADDED + (y + 1) * PADDED_AREA;
            let front = i.wrapping_add_signed(normal);
            visit(i, front, front_neighborhood::<F>(opaque, x, y, z))?;
        }
    }
    Ok(())
}

/// Opacity of the 3x3 front-layer neighborhood of the face `F` of unpadded voxel `(x, y, z)`,
/// bit `(dv + 1) * 3 + (du + 1)` (see `shading`).
#[inline]
fn front_neighborhood<const F: usize>(opaque: &[u64], x: usize, y: usize, z: usize) -> u32 {
    // Mask row of padded (px, py); bit pz.
    let row = |px: usize, py: usize| opaque[px + py * PADDED];
    // Bits j -> 3j (a column of three cells along v).
    let spread3 = |t: u64| ((t & 1) | (t & 2) << 2 | (t & 4) << 4) as u32;
    match F {
        // ±Y: u = x, v = z; the front layer is padded y + 2 (+Y) or y (-Y). The bits of padded
        // z..z + 2 are the cells dv = -1..=1.
        0 | 1 => {
            let py = if F == 0 { y + 2 } else { y };
            (0..3).fold(0, |nb, du| nb | spread3((row(x + du, py) >> z) & 7) << du)
        }
        // ±X: u = z, v = y; the front layer is padded x + 2 (+X) or x (-X).
        2 | 3 => {
            let px = if F == 2 { x + 2 } else { x };
            (0..3).fold(0, |nb, dv| {
                nb | (((row(px, y + dv) >> z) & 7) as u32) << (3 * dv)
            })
        }
        // ±Z: u = x, v = y; the front layer is padded z + 2 (+Z) or z (-Z).
        _ => {
            let pz = if F == 4 { z + 2 } else { z };
            let mut nb = 0;
            for dv in 0..3 {
                for du in 0..3 {
                    nb |= (((row(x + du, y + dv) >> pz) & 1) as u32) << (3 * dv + du);
                }
            }
            nb
        }
    }
}

/// Converts the merged quads to [`PackedQuad`]s and writes them bucket-contiguous into `out`
/// (its palette already set), decoding each quad's key with `decode`; each bucket ends with the
/// quads of the non-cube blocks (`specials`) routed to it.
fn assemble(
    greedy: &Greedy,
    specials: &[(u8, PackedQuad)],
    materials: &MaterialTable,
    out: &mut ChunkMesh,
    decode: impl Fn(u32) -> FaceKey,
) {
    // Pass 1: bucket sizes, so each bucket is written in place (no second buffer).
    let mut counts = [0u32; BUCKET_COUNT];
    for (face, quads) in Face::ALL.into_iter().zip(&greedy.quads) {
        for &quad in quads {
            let (material, _) = materials.split_surface(decode((quad >> 32) as u32).material);
            counts[materials.bucket(material, face)] += 1;
        }
    }
    for &(bucket, _) in specials {
        counts[bucket as usize] += 1;
    }
    // Bounds: an opaque face bucket holds at most `MAX_QUADS_PER_FACE_BUCKET`. The cutout and
    // translucent buckets hold at most `MAX_QUADS_PER_BUCKET` while each has a single material
    // (the default registry), but up to 6 * 32^3 when two materials of one bucket alternate
    // (every face between them is visible). The output is never truncated.
    let mut cursor = [0usize; BUCKET_COUNT];
    let mut total = 0usize;
    for (start, count) in cursor.iter_mut().zip(counts) {
        *start = total;
        total += count as usize;
    }

    // Pass 2: convert and scatter.
    out.quads.resize(total, PackedQuad::default());
    for (face, quads) in Face::ALL.into_iter().zip(&greedy.quads) {
        for &quad in quads {
            let mut key = decode((quad >> 32) as u32);
            let surface;
            (key.material, surface) = materials.split_surface(key.material);
            let bucket = materials.bucket(key.material, face);
            let mut packed = to_packed(quad, face, materials.layer(key.material, face), key);
            if materials.emissive(key.material) {
                packed.geo |= layout::FLAG_EMISSIVE << layout::GEO_FLAGS_SHIFT;
            }
            let code = if surface && face != Face::NegY {
                layout::SHAPE_FLUID_SURFACE
            } else {
                materials.cube_code(key.material, face)
            };
            if code != layout::SHAPE_CUBE {
                packed = packed.with_shape(code);
            }
            if cfg!(debug_assertions) {
                debug_check_merged(packed, out.corner_light(packed));
            }
            out.quads[cursor[bucket]] = packed;
            cursor[bucket] += 1;
        }
    }
    for &(bucket, quad) in specials {
        out.quads[cursor[bucket as usize]] = quad;
        cursor[bucket as usize] += 1;
    }
    out.bucket_counts = counts;
}

/// The [`PackedQuad`] of a merged quad (`Greedy::quads` format) on `face`, with its texture
/// layer, AO byte and light palette index.
#[inline]
fn to_packed(quad: u64, face: Face, layer: u16, key: FaceKey) -> PackedQuad {
    use layout::*;
    PackedQuad {
        geo: quad as u32 | (face as u32) << GEO_FACE_SHIFT,
        mat: (u32::from(layer) & MAT_LAYER_MASK)
            | u32::from(key.ao) << MAT_AO_SHIFT
            | (u32::from(key.light) & MAT_LIGHT_MASK) << MAT_LIGHT_SHIFT,
    }
}

/// Debug check that a merged quad's AO and corner lights are constant along every axis it
/// spans more than one voxel on, so interpolating its 4 corners gives exactly what its unit
/// faces would (holds because both only depend on the vertex).
fn debug_check_merged(quad: PackedQuad, light: [LightValue; 4]) {
    let ao = |k| quad.ao(k);
    if quad.w() > 1 {
        debug_assert!(
            light[0] == light[1] && light[2] == light[3] && ao(0) == ao(1) && ao(2) == ao(3),
            "{quad:?} varies along u: light {light:?}"
        );
    }
    if quad.h() > 1 {
        debug_assert!(
            light[0] == light[2] && light[1] == light[3] && ao(0) == ao(2) && ao(1) == ao(3),
            "{quad:?} varies along v: light {light:?}"
        );
    }
}

/// Mesher class of padded `(x, y, z)` from the column masks (what face culling reads).
pub(crate) fn class_from_masks(padded: &PaddedChunk, x: i32, y: i32, z: i32) -> VoxelClass {
    let row = mask_row(x, y);
    let bit = 1u64 << (z + 1);
    if padded.opaque[row] & bit != 0 {
        VoxelClass::Opaque
    } else if padded.transparent[row] & bit != 0 {
        VoxelClass::Transparent
    } else if padded.special[row] & bit != 0 {
        VoxelClass::Special
    } else {
        VoxelClass::Empty
    }
}

/// Brute-force oracle: every visible unit face `(voxel, face)` of the cubes of the inner 32^3 (a
/// face is visible if the voxel is an opaque or transparent cube and the neighbor is empty or a
/// non-cube block, or transparent with a different material). The mesher's greedy output (the
/// quads with shape code `SHAPE_CUBE` or `SHAPE_TURNED`), expanded with
/// `PackedQuad::unit_faces`, must equal this set.
pub fn reference_visible_faces(padded: &PaddedChunk) -> BTreeSet<([u32; 3], Face)> {
    // Class from the column masks (what bgm reads), material from the voxel ids.
    let class = |x, y, z| class_from_masks(padded, x, y, z);
    let mut faces = BTreeSet::new();
    for y in 0..CHUNK_SIZE {
        for x in 0..CHUNK_SIZE {
            for z in 0..CHUNK_SIZE {
                if matches!(class(x, y, z), VoxelClass::Empty | VoxelClass::Special) {
                    continue;
                }
                let material = padded.get(x, y, z);
                for face in Face::ALL {
                    let n = face.normal();
                    let (nx, ny, nz) = (x + n.x, y + n.y, z + n.z);
                    let visible = match class(nx, ny, nz) {
                        VoxelClass::Empty | VoxelClass::Special => true,
                        VoxelClass::Transparent => {
                            (padded.get(nx, ny, nz) ^ material) & padded.cull_mask != 0
                        }
                        VoxelClass::Opaque => false,
                    };
                    if visible {
                        faces.insert(([x as u32, y as u32, z as u32], face));
                    }
                }
            }
        }
    }
    faces
}

/// Brute-force oracle of the shading: for every visible unit face ([`reference_visible_faces`]),
/// its AO byte and the exact light of its 4 corners (no [`LightQuantization`]), straight from
/// the definitions (ADR 0015):
/// - The front layer is the layer of voxels the face looks at; the 4 front-layer voxels around
///   a corner are the one in front of the face, its side along u, its side along v and the
///   diagonal. Opaque means `PaddedChunk::opaque` (render-opaque).
/// - Corner light: the rounded per-channel mean `(sum + n / 2) / n` of the light of those 4
///   voxels, without the opaque ones, and without the diagonal when both sides are opaque.
///   `light` `None` is sky 15 everywhere.
/// - AO per corner: 0 when both sides are opaque, else 3 minus the opaque count of the sides
///   and the diagonal. Always open with [`AoPolicy::Off`]; open on a face whose 4 corner lights
///   are 0 with [`AoPolicy::BakedLitOnly`].
///
/// A face of a lit emitter ([`MaterialTable::lit_emission`]) is shaded flat instead
/// ([`reference_lit_emitter_shading`]); this oracle knows no materials.
pub fn reference_face_shading(
    padded: &PaddedChunk,
    light: Option<&PaddedLight>,
    policy: AoPolicy,
) -> BTreeMap<([u32; 3], Face), FaceShading> {
    reference_visible_faces(padded)
        .into_iter()
        .map(|(p, face)| {
            (
                (p, face),
                reference_face_shading_at(padded, light, policy, p, face),
            )
        })
        .collect()
}

/// [`reference_face_shading`] of one unit face: `face` of voxel `p` (inner 32^3), whether it
/// is visible or not.
pub fn reference_face_shading_at(
    padded: &PaddedChunk,
    light: Option<&PaddedLight>,
    policy: AoPolicy,
    p: [u32; 3],
    face: Face,
) -> FaceShading {
    let n = face.normal().to_array();
    let front: [i32; 3] = std::array::from_fn(|a| p[a] as i32 + n[a]);
    layer_shading(padded, light, policy, front, face)
}

/// The oracle of a box face of a model made of boxes (the per-block path, ADR 0018 and 0020):
/// `face` of the block at `p` shades as the cube face there ([`reference_face_shading_at`]),
/// unless the voxel in front of it is an opaque cube: then the same rules apply to the block's
/// own layer (the 4 corner cells around each corner in the plane through `p`).
pub fn reference_box_face_shading_at(
    padded: &PaddedChunk,
    light: Option<&PaddedLight>,
    policy: AoPolicy,
    p: [u32; 3],
    face: Face,
) -> FaceShading {
    let n = face.normal().to_array();
    let front: [i32; 3] = std::array::from_fn(|a| p[a] as i32 + n[a]);
    let opaque = class_from_masks(padded, front[0], front[1], front[2]) == VoxelClass::Opaque;
    let layer = if opaque { p.map(|c| c as i32) } else { front };
    layer_shading(padded, light, policy, layer, face)
}

/// The oracle of a face of a lit emitter ([`MaterialTable::lit_emission`] `emission`, ADR 0036):
/// AO open, under every policy, and every corner the light of padded voxel `cell` raised per
/// channel to `emission` (no [`LightQuantization`]; `light` `None` is sky 15 everywhere).
/// `cell` is the voxel whose light the face reads: the one in front of a cube face, the front
/// layer's of a box face ([`reference_box_face_shading_at`]), a cross's own.
pub fn reference_lit_emitter_shading(
    light: Option<&PaddedLight>,
    cell: [i32; 3],
    emission: LightValue,
) -> FaceShading {
    let at = light.map_or(LightValue::FULL_SKY, |l| l.get(cell[0], cell[1], cell[2]));
    FaceShading {
        ao: AO_OPEN_BYTE,
        light: [at.max(emission); 4],
    }
}

/// AO and corner lights of a face of direction `face` whose front layer is the one through
/// `front` (see [`reference_face_shading`]).
fn layer_shading(
    padded: &PaddedChunk,
    light: Option<&PaddedLight>,
    policy: AoPolicy,
    front: [i32; 3],
    face: Face,
) -> FaceShading {
    let opaque = |p: [i32; 3]| class_from_masks(padded, p[0], p[1], p[2]) == VoxelClass::Opaque;
    let light_at = |p: [i32; 3]| light.map_or(LightValue::FULL_SKY, |l| l.get(p[0], p[1], p[2]));
    let (_, ua, va) = face.axes();
    let cell = |du: i32, dv: i32| {
        let mut c = front;
        c[ua] += du;
        c[va] += dv;
        c
    };
    let mut ao = 0u8;
    let mut corners = [LightValue::DARK; 4];
    for (k, corner) in corners.iter_mut().enumerate() {
        let su = if k & 1 == 1 { 1 } else { -1 };
        let sv = if k >> 1 == 1 { 1 } else { -1 };
        let (side_u, side_v, diagonal) = (cell(su, 0), cell(0, sv), cell(su, sv));
        let (ou, ov, od) = (opaque(side_u), opaque(side_v), opaque(diagonal));
        ao |= corner_ao(ou, ov, od) << (2 * k);
        let mut kept = vec![light_at(front)];
        if !ou {
            kept.push(light_at(side_u));
        }
        if !ov {
            kept.push(light_at(side_v));
        }
        if !od && !(ou && ov) {
            kept.push(light_at(diagonal));
        }
        let count = kept.len() as u16;
        let mut value = 0u16;
        for shift in [12, 8, 4, 0] {
            let sum: u16 = kept.iter().map(|l| (l.0 >> shift) & 15).sum();
            value |= ((sum + count / 2) / count) << shift;
        }
        *corner = LightValue(value);
    }
    let dark = corners.iter().all(|c| c.is_dark());
    let ao = match policy {
        AoPolicy::Off => AO_OPEN_BYTE,
        AoPolicy::BakedLitOnly if dark => AO_OPEN_BYTE,
        AoPolicy::Baked | AoPolicy::BakedLitOnly => ao,
    };
    FaceShading { ao, light: corners }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of the boxes `(min, max)` (sixteenths).
    fn shape(boxes: &[([u8; 3], [u8; 3])]) -> BoxShape {
        let boxes: Vec<ModelBox> = boxes
            .iter()
            .map(|&(min, max)| ModelBox::new(min, max))
            .collect();
        BoxShape::new(&boxes, |_| 0)
    }

    #[test]
    fn the_top_floor_is_where_the_solid_top_starts() {
        // A bottom slab has no solid top, a top slab from 8, a full box from 0.
        assert_eq!(shape(&[([0; 3], [16, 8, 16])]).top_floor, NO_FLOOR);
        assert_eq!(shape(&[([0, 8, 0], [16; 3])]).top_floor, 8);
        assert_eq!(shape(&[([0; 3], [16; 3])]).top_floor, 0);
        // A plate under the top.
        assert_eq!(shape(&[([0, 15, 0], [16; 3])]).top_floor, 15);
        // Two half boxes of different depth: whole from 8 up.
        let halves = [([0, 4, 0], [16, 16, 8]), ([0, 8, 8], [16; 3])];
        assert_eq!(shape(&halves).top_floor, 8);
        // Upside-down stairs: the step below fills only half its layers.
        let stairs = [([0, 8, 0], [16; 3]), ([0; 3], [16, 8, 8])];
        assert_eq!(shape(&stairs).top_floor, 8);
        // A top that misses one sixteenth.
        assert_eq!(shape(&[([0, 8, 0], [16, 16, 15])]).top_floor, NO_FLOOR);
    }

    #[test]
    fn sturdy_faces_are_the_border_faces_the_boxes_cover_whole() {
        let registry = boxel_demo::blocks::registry();
        let table = MaterialTable::from_registry(&registry);
        let mut models = 0;
        for i in 0..registry.len() {
            let s = BlockState(i as u16);
            let Some(shape) = table.box_models.of(registry.material(s)) else {
                continue;
            };
            models += 1;
            let boxes = registry.model_boxes(s);
            for face in Face::ALL {
                let (n, u, v) = face.axes();
                let positive = face.index().is_multiple_of(2);
                let border = if positive { 16 } else { 0 };
                // Every sixteenth of the border face lies on a box face on that border.
                let whole = (0..16).all(|a| {
                    (0..16).all(|b| {
                        boxes.iter().any(|bx| {
                            let (min, max) = (bx.bounds.min, bx.bounds.max);
                            let plane = if positive { max[n] } else { min[n] };
                            plane == border
                                && (min[u]..max[u]).contains(&a)
                                && (min[v]..max[v]).contains(&b)
                        })
                    })
                });
                let sturdy = shape.sturdy >> face.index() & 1 == 1;
                assert_eq!(sturdy, whole, "{} {face:?}", registry.state_name(s));
            }
        }
        assert!(models > 0);
    }
}
