//! Quads a cell draws from its data (EW-ITEM, ADR 0050): the item an item frame holds, the items
//! on a shelf. A block registers a [`CellQuadRule`]; the mesher asks it, for every cell of that
//! block with data ([`crate::BlockData`]), which [`CellQuad`]s to draw, and bakes them into the
//! chunk's mesh. A change of the data remeshes the chunk (ADR 0048), so nothing runs per frame.
//!
//! A cell quad is not a block quad: it can sit anywhere in its cell at any angle, and it shows an
//! image of the content's own texture array (the cell textures, one layer per image, which the
//! content gives the renderer) or a layer of the block texture array ([`CellImage::Block`], ADR
//! 0053: a block drawn small in a frame). The engine names no image: the rule says the layer.
//! [`model_quads`] gives the quads a state's model draws in the terrain, as cell quads of block
//! layers, for a rule to place them (scaled, turned) where it wants.
use crate::block_data::BlockData;
use crate::planes::ModelPlane;
use crate::registry::{BlockModel, BlockRegistry, BoxFaceTexture, box_face_entries};
use crate::{BlockState, Face, ModelBox};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// The texture array a [`CellQuad::image`] is a layer of.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CellImage {
    /// The cell textures (ADR 0050), which the content gives the renderer.
    #[default]
    Cell,
    /// The block textures: the layers the terrain draws ([`crate::TextureId`]), so a block
    /// drawn small (in a frame, on a shelf) looks as it does in the world (ADR 0053).
    Block,
    /// A pattern fixed to the screen (ADR 0056), from the pattern textures the content gives:
    /// [`CellQuad::image`] is the first of three layers, the base image, the layer image and the
    /// tints (row 0: texel 0 tints the base, texel `k` tints layer `k`). The quad shows the base
    /// stretched over the screen and, over it, this many layers (at most
    /// [`PATTERN_LAYERS_MAX`]) of the layer image, each scaled, turned and drifting by its own
    /// amount, added up: Minecraft's end portal (15 layers) and end gateway (16). Drawn opaque at
    /// full brightness, with fog; [`CellQuad::uv`], [`CellQuad::full_bright`] and
    /// [`CellQuad::blend`] are not read, [`CellQuad::tint`] multiplies the sum. Seen only from
    /// the side its [`CellQuad::face`] looks to, which must be across the quad (a cube face).
    Pattern(u8),
}

/// The most layers a [`CellImage::Pattern`] draws over its base.
pub const PATTERN_LAYERS_MAX: u8 = 16;

/// How a [`CellQuad`] covers what is behind it (ADR 0055).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CellBlend {
    /// Alpha-tested like a cutout block: a texel shows whole (alpha at least a half) or not at
    /// all, from both sides.
    #[default]
    Cutout,
    /// Blended over what is behind it by its image's alpha times this much (`255` keeps the
    /// image's alpha), as Minecraft's translucent items: drawn after the cutout cell quads, back
    /// to front with the other blended ones, before the terrain's translucent quads, and writing
    /// depth. Seen only from the side its [`CellQuad::face`] looks to (Minecraft culls the back
    /// of a translucent item), from both when that face lies along the quad. Texels of the
    /// image's alpha under a tenth are not drawn (Minecraft's item shader).
    Translucent(u8),
    /// Blended as [`CellBlend::Translucent`], but writing no depth: a glow that hides nothing
    /// behind it, not even the clouds, which paint over it (the beacon beam's outer layer at
    /// `32`, as Minecraft's beacon beam).
    Glow(u8),
}

impl CellBlend {
    /// Blended by its image's own alpha.
    pub const TRANSLUCENT: Self = Self::Translucent(255);

    /// True if blended ([`CellBlend::Translucent`] or [`CellBlend::Glow`]).
    pub const fn is_translucent(self) -> bool {
        !matches!(self, Self::Cutout)
    }

    /// What a blended quad multiplies its image's alpha by (`0` for a cutout one).
    pub const fn opacity(self) -> u8 {
        match self {
            Self::Cutout => 0,
            Self::Translucent(a) | Self::Glow(a) => a,
        }
    }
}

/// One quad a cell draws from its data, in its cell: four corners anywhere in it, each with the
/// point of the image it shows. Alpha-tested like a cutout block and drawn from both sides, or
/// blended ([`CellQuad::blend`]); lit by its cell's light without ambient occlusion, shaded like
/// the cube face [`CellQuad::face`].
#[derive(Clone, Copy, Debug)]
pub struct CellQuad {
    /// Where each corner is in the cell, in blocks (`0..=1` on each axis for a quad inside it).
    /// Corner `k` sits at `(u, v) = (k & 1, k >> 1)` of the quad: 0 and 3 are opposite.
    pub corners: [[f32; 3]; 4],
    /// The point of its image each corner shows, as a fraction of the image (`(0, 0)` its top
    /// left, `(1, 1)` its bottom right).
    pub uv: [[f32; 2]; 4],
    /// The image: a layer of the texture array [`CellQuad::source`] says (`0..4096`).
    pub image: u16,
    /// The texture array `image` is a layer of: the cell textures, or the block textures.
    pub source: CellImage,
    /// Shaded as this face of a cube (`PosY` is full brightness, as a plant).
    pub face: Face,
    /// Drawn at the full brightness of its image, without light or face shading (the item of a
    /// glow item frame).
    pub full_bright: bool,
    /// What the image's color is multiplied by (sRGB, `[255; 3]` keeps it).
    pub tint: [u8; 3],
    /// Alpha-tested or blended.
    pub blend: CellBlend,
}

impl CellQuad {
    /// The quad of `plane` ([`ModelPlane::corners`]: its box, kind, spin and quarter turns)
    /// showing the rectangle [`ModelPlane::uv`] of `image` (sixteenths of the image), shaded as
    /// `PosY`. The plane's own [`ModelPlane::texture`] is not read.
    pub fn from_plane(plane: &ModelPlane, image: u16) -> Self {
        let corners = plane.corners();
        Self {
            corners: corners.map(|c| c.position),
            uv: corners.map(|c| c.texel.map(|t| t / 16.0)),
            image,
            source: CellImage::Cell,
            face: Face::PosY,
            full_bright: false,
            tint: [255; 3],
            blend: CellBlend::Cutout,
        }
    }

    /// The same quad shaded as `face`.
    #[must_use]
    pub const fn shaded_as(mut self, face: Face) -> Self {
        self.face = face;
        self
    }

    /// The same quad drawn at full brightness, or not.
    #[must_use]
    pub const fn with_full_bright(mut self, on: bool) -> Self {
        self.full_bright = on;
        self
    }

    /// The same quad with its image multiplied by `tint`.
    #[must_use]
    pub const fn with_tint(mut self, tint: [u8; 3]) -> Self {
        self.tint = tint;
        self
    }

    /// The same quad alpha-tested or blended as `blend`.
    #[must_use]
    pub const fn with_blend(mut self, blend: CellBlend) -> Self {
        self.blend = blend;
        self
    }

    /// The same quad moved by `by` blocks (into its chunk: the mesher adds the cell's place).
    #[must_use]
    pub fn moved(mut self, by: [f32; 3]) -> Self {
        for c in &mut self.corners {
            for (a, d) in c.iter_mut().zip(by) {
                *a += d;
            }
        }
        self
    }

    /// Every float as its bits, then the rest: what equality and hashing compare.
    #[expect(
        clippy::type_complexity,
        reason = "a tuple of the fields, compared once"
    )]
    fn key(
        &self,
    ) -> (
        [[u32; 3]; 4],
        [[u32; 2]; 4],
        u16,
        CellImage,
        Face,
        bool,
        [u8; 3],
        CellBlend,
    ) {
        (
            self.corners.map(|c| c.map(f32::to_bits)),
            self.uv.map(|c| c.map(f32::to_bits)),
            self.image,
            self.source,
            self.face,
            self.full_bright,
            self.tint,
            self.blend,
        )
    }
}

impl PartialEq for CellQuad {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for CellQuad {}

impl Hash for CellQuad {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

/// The quads state `s` draws in the terrain, alone in its cell (no neighbor hides a face), as
/// cell quads of block layers ([`CellImage::Block`]) in that cell: the CPU twin of the terrain
/// shader's cube faces, crosses, box faces and planes (`common.wgsl`, `decode_quad`), with the
/// same corners, texture points and layers, and the same box faces the mesher leaves out (those
/// another box of the model covers, the hidden ones). Each is shaded as its face (crosses and
/// planes as `PosY`, full brightness, as the terrain does). A translucent state's quads are
/// blended ([`CellBlend::TRANSLUCENT`], as Minecraft draws a translucent block's item), the
/// others alpha-tested. A fluid or a block not drawn gives nothing. Appended to `out`.
///
/// What a rule turns, scales and moves to draw a block small (ADR 0053: a block in an item
/// frame); the light is its cell's and there is no ambient occlusion, as for every cell quad.
pub fn model_quads(registry: &BlockRegistry, s: BlockState, out: &mut Vec<CellQuad>) {
    let blend = match registry.shape(s) {
        crate::RenderShape::None => return,
        crate::RenderShape::Translucent => CellBlend::TRANSLUCENT,
        crate::RenderShape::Opaque | crate::RenderShape::Cutout => CellBlend::Cutout,
    };
    let quad = |corners: [[f32; 3]; 4], uv: [[f32; 2]; 4], layer: u16, face: Face| CellQuad {
        corners,
        uv,
        image: layer,
        source: CellImage::Block,
        face,
        full_bright: false,
        tint: [255; 3],
        blend,
    };
    match registry.model(s) {
        BlockModel::Fluid => {}
        BlockModel::Cube { .. } => {
            let model = registry.model(s);
            for face in Face::ALL {
                let mut q = box_face(ModelBox::new([0; 3], [16; 3]), face);
                if model.face_turned(face) {
                    // SHAPE_TURNED: the texture's vertical runs along u.
                    q.1 = q.1.map(|[u, v]| [v, u]);
                }
                out.push(quad(q.0, q.1, registry.face_texture(s, face).0, face));
            }
        }
        BlockModel::Cross => {
            let layer = registry.face_texture(s, Face::PosX).0;
            // Face PosX from (0, 0) to (1, 1) in (x, z), face PosZ from (1, 0) to (0, 1).
            for (start, along) in [([0.0, 0.0], [1.0, 1.0]), ([1.0, 0.0], [-1.0, 1.0])] {
                let corners = std::array::from_fn(|k| {
                    let (cu, cv) = ((k & 1) as f32, (k >> 1) as f32);
                    [start[0] + along[0] * cu, cv, start[1] + along[1] * cu]
                });
                let uv = std::array::from_fn(|k| [(k & 1) as f32, 1.0 - (k >> 1) as f32]);
                out.push(quad(corners, uv, layer, Face::PosY));
            }
        }
        BlockModel::Slab(_) | BlockModel::Boxes(_) => {
            let boxes = registry.model_boxes(s);
            let textures = registry.box_textures(s);
            let texture = |i: usize, face: Face| {
                textures
                    .get(i)
                    .map_or(BoxFaceTexture::BLOCK, |t| t[face.index()])
            };
            for (i, &b) in boxes.iter().enumerate() {
                let entries = box_face_entries(b, &Face::ALL.map(|f| texture(i, f)));
                for face in Face::ALL {
                    let Some(entry) = entries[face.index()] else {
                        continue;
                    };
                    if covered(boxes, i, face, &texture) {
                        continue;
                    }
                    let layer = texture(i, face)
                        .texture
                        .unwrap_or_else(|| registry.face_texture(s, face))
                        .0;
                    let (corners, uv) = box_face(entry, face);
                    out.push(quad(corners, uv, layer, face));
                }
            }
            for plane in registry.model_planes(s) {
                let layer = plane
                    .texture
                    .unwrap_or_else(|| registry.face_texture(s, Face::PosX))
                    .0;
                out.push(CellQuad {
                    source: CellImage::Block,
                    blend,
                    ..CellQuad::from_plane(plane, layer)
                });
            }
        }
    }
}

/// The corners (blocks) and texture points (fractions of a layer) of `face` of box `b`, corner
/// `k` at `(u, v) = (k & 1, k >> 1)` of the face's axes ([`Face::axes`]): the terrain shader's
/// `SHAPE_BOX` face (a full box is the cube's face), its texture the part of the layer under the
/// box moved by the box's texture offset, `v` down on the side faces.
fn box_face(b: ModelBox, face: Face) -> ([[f32; 3]; 4], [[f32; 2]; 4]) {
    let (n, u, v) = face.axes();
    let lo = b.bounds.min.map(|c| f32::from(c) / 16.0);
    let hi = b.bounds.max.map(|c| f32::from(c) / 16.0);
    let shift = b.texture_offset.map(|c| f32::from(c) / 16.0);
    let depth = if face.index().is_multiple_of(2) {
        hi[n]
    } else {
        lo[n]
    };
    let mut corners = [[0.0; 3]; 4];
    let mut uv = [[0.0; 2]; 4];
    for k in 0..4 {
        let su = if k & 1 == 1 { hi[u] } else { lo[u] };
        let sv = if k >> 1 == 1 { hi[v] } else { lo[v] };
        corners[k][n] = depth;
        corners[k][u] = su;
        corners[k][v] = sv;
        let (tu, tv) = (su + shift[u], sv + shift[v]);
        uv[k] = if matches!(face, Face::PosY | Face::NegY) {
            [tu, tv]
        } else {
            [tu, 1.0 - tv]
        };
    }
    (corners, uv)
}

/// True if the other boxes of `boxes` cover `face` of box `i` (their opposite faces on its
/// plane, shown, hold all of it): the mesher leaves such a face out (`BoxShape`).
fn covered(
    boxes: &[ModelBox],
    i: usize,
    face: Face,
    texture: &impl Fn(usize, Face) -> BoxFaceTexture,
) -> bool {
    let b = boxes[i];
    let (n, u, v) = face.axes();
    let positive = face.index().is_multiple_of(2);
    let (min, max) = (b.bounds.min, b.bounds.max);
    let plane = if positive { max[n] } else { min[n] };
    // Sixteenths of the face's plane: bit u of row v.
    let mut cover = [0u32; 16];
    for (j, o) in boxes.iter().enumerate() {
        if *o == b || texture(j, face.opposite()).hidden {
            continue;
        }
        let opposite = if positive {
            o.bounds.min[n]
        } else {
            o.bounds.max[n]
        };
        if opposite != plane {
            continue;
        }
        let bits = (1u32 << o.bounds.max[u]) - (1u32 << o.bounds.min[u]);
        for row in &mut cover[usize::from(o.bounds.min[v])..usize::from(o.bounds.max[v])] {
            *row |= bits;
        }
    }
    let bits = (1u32 << max[u]) - (1u32 << min[u]);
    cover[usize::from(min[v])..usize::from(max[v])]
        .iter()
        .all(|&row| row & bits == bits)
}

/// The quads of one state's cells from their data: what a [`CellQuadRule`] gives for a state.
pub type CellQuadsFn = dyn Fn(&BlockData, &mut Vec<CellQuad>) + Send + Sync;

type BindFn = dyn Fn(&BlockRegistry, BlockState) -> Option<Arc<CellQuadsFn>> + Send + Sync;

/// What the cells of a block draw from their data (ADR 0050), registered per block
/// ([`crate::BlockRegistryBuilder::set_cell_quads`]). It binds once per state, when the mesher's
/// table is built from the registry: given the registry and the state, it returns the function
/// the mesher calls for each cell of that state with data (`None`: that state draws nothing from
/// its data). So the function reads the state's properties (a frame's facing) once, not per
/// cell.
#[derive(Clone)]
pub struct CellQuadRule(Arc<BindFn>);

impl CellQuadRule {
    /// The rule `bind`.
    pub fn new(
        bind: impl Fn(&BlockRegistry, BlockState) -> Option<Arc<CellQuadsFn>> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(bind))
    }

    /// The function of state `s`'s cells, if it draws any.
    pub fn bind(&self, registry: &BlockRegistry, s: BlockState) -> Option<Arc<CellQuadsFn>> {
        (self.0)(registry, s)
    }
}

impl fmt::Debug for CellQuadRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CellQuadRule")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planes::PlaneKind;
    use crate::registry::Axis;

    #[test]
    fn a_plane_quad_has_the_plane_corners_and_its_rectangle_in_fractions() {
        let plane = ModelPlane::new(PlaneKind::Mid(Axis::Z), [8, 8, 2], [24, 24, 2])
            .with_uv([0, 0, 16, 16])
            .spun(2);
        let quad = CellQuad::from_plane(&plane, 7);
        let corners = plane.corners();
        for (k, c) in corners.iter().enumerate() {
            assert_eq!(quad.corners[k], c.position);
            assert_eq!(quad.uv[k], c.texel.map(|t| t / 16.0));
        }
        assert_eq!(quad.image, 7);
        assert_eq!(quad.face, Face::PosY);
        assert!(!quad.full_bright);
        // Spun 45 degrees in the plane z = 1/16: the corners stay there and leave the box.
        assert!(quad.corners.iter().all(|c| c[2] == 0.0625));
        let xs = quad.corners.map(|c| c[0]);
        assert!(xs.iter().any(|&x| x < 0.25) && xs.iter().any(|&x| x > 0.75));
    }

    #[test]
    fn equal_quads_compare_and_hash_by_their_bits() {
        use std::collections::HashSet;
        let plane = ModelPlane::new(PlaneKind::Mid(Axis::X), [4, 0, 0], [4, 32, 32]);
        let a = CellQuad::from_plane(&plane, 1);
        let b = a
            .shaded_as(Face::PosX)
            .with_full_bright(true)
            .with_tint([10, 20, 30]);
        assert_eq!(a, a.moved([0.0; 3]));
        assert_ne!(a, b);
        assert_ne!(a, a.moved([1.0, 0.0, 0.0]));
        let set: HashSet<_> = [a, a, b].into_iter().collect();
        assert_eq!(set.len(), 2);
        assert_eq!(b.face, Face::PosX);
        assert_eq!(b.tint, [10, 20, 30]);
    }

    fn state(registry: &BlockRegistry, name: &str) -> BlockState {
        registry.parse_state(name).expect(name)
    }

    /// A cube draws its six faces from the block layers, each at its side of the cell with the
    /// terrain's texture points: `v` down on the sides, `(x, z)` on the top.
    #[test]
    fn a_cube_draws_six_faces_of_block_layers() {
        let registry = crate::testing::registry();
        let s = state(&registry, "test:solid");
        let mut out = Vec::new();
        model_quads(&registry, s, &mut out);
        assert_eq!(out.len(), 6);
        for (q, face) in out.iter().zip(Face::ALL) {
            assert_eq!(q.face, face);
            assert_eq!(q.source, CellImage::Block);
            assert_eq!(q.image, registry.face_texture(s, face).0);
            let (n, _, _) = face.axes();
            let side = if face.index() % 2 == 0 { 1.0 } else { 0.0 };
            assert!(q.corners.iter().all(|c| c[n] == side), "{face:?}");
        }
        let top = out[Face::PosY.index()];
        assert_eq!(top.corners[3], [1.0, 1.0, 1.0]);
        assert_eq!(top.uv, [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]]);
        let south = out[Face::PosZ.index()];
        assert_eq!(south.corners[0], [0.0, 0.0, 1.0]);
        assert_eq!(south.uv[0], [0.0, 1.0]);
        assert_eq!(south.uv[3], [1.0, 0.0]);
    }

    /// A translucent block's quads blend by their layer's alpha (ADR 0055); an opaque or cutout
    /// block's are alpha-tested.
    #[test]
    fn a_translucent_block_draws_blended_quads() {
        let registry = crate::testing::registry();
        let mut out = Vec::new();
        model_quads(&registry, state(&registry, "test:glass"), &mut out);
        assert_eq!(out.len(), 6);
        assert!(out.iter().all(|q| q.blend == CellBlend::TRANSLUCENT));
        for name in ["test:solid", "test:plant", "test:slab"] {
            out.clear();
            model_quads(&registry, state(&registry, name), &mut out);
            assert!(out.iter().all(|q| q.blend == CellBlend::Cutout), "{name}");
        }
        let q = out[0];
        assert_ne!(q, q.with_blend(CellBlend::Translucent(32)));
        assert_ne!(
            q.with_blend(CellBlend::Glow(32)),
            q.with_blend(CellBlend::Translucent(32))
        );
        assert!(CellBlend::Glow(32).is_translucent() && !CellBlend::Cutout.is_translucent());
        assert_eq!(CellBlend::Glow(32).opacity(), 32);
        assert_eq!(CellBlend::Cutout.opacity(), 0);
    }

    /// A slab's sides show the lower half of the layer; a cross is two diagonals; a fluid and
    /// air draw nothing.
    #[test]
    fn slabs_crosses_fluids_and_air() {
        let registry = crate::testing::registry();
        let mut out = Vec::new();
        model_quads(&registry, state(&registry, "test:slab"), &mut out);
        assert_eq!(out.len(), 6);
        let east = out[Face::PosX.index()];
        assert_eq!(east.corners.map(|c| c[1]), [0.0, 0.0, 0.5, 0.5]);
        assert_eq!(east.uv.map(|t| t[1]), [1.0, 1.0, 0.5, 0.5]);
        out.clear();
        model_quads(&registry, state(&registry, "test:plant"), &mut out);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].corners[3], [1.0, 1.0, 1.0]);
        assert_eq!(out[1].corners[1], [0.0, 0.0, 1.0]);
        assert!(out.iter().all(|q| q.face == Face::PosY));
        out.clear();
        model_quads(&registry, state(&registry, "test:fluid"), &mut out);
        model_quads(&registry, BlockState::AIR, &mut out);
        assert!(out.is_empty());
    }

    /// A box face another box of the model covers is left out, as the mesher does: the bottom
    /// of a stair's step on its slab. The rest are drawn, planes too.
    #[test]
    fn covered_box_faces_are_left_out_and_planes_drawn() {
        use crate::{BlockCollision, BlockDef, BoxModel, FaceTextures, RenderShape};
        let mut b = crate::testing::builder();
        // `test:solid`'s layer, the first one.
        let stone = crate::TextureId(0);
        let boxes = vec![
            ModelBox::new([0, 0, 0], [16, 8, 16]),
            ModelBox::new([0, 8, 8], [16, 16, 16]),
        ];
        let plane = ModelPlane::new(PlaneKind::Mid(Axis::Y), [0, 16, 0], [16, 16, 16]);
        let model = b
            .add_box_model(BoxModel::from_boxes(boxes).with_planes(vec![plane]))
            .expect("model");
        let s = b
            .add_block(BlockDef {
                name: "test:stairs".to_string(),
                shape: RenderShape::Opaque,
                model: BlockModel::Boxes(model),
                collision: BlockCollision::Full,
                emission: [0; 3],
                light_opacity: 0,
                textures: FaceTextures::all(stone),
            })
            .expect("stairs");
        let registry = b.build();
        let mut out = Vec::new();
        model_quads(&registry, s, &mut out);
        assert_eq!(out.len(), 6 + 5 + 1);
        let bottoms: Vec<_> = out.iter().filter(|q| q.face == Face::NegY).collect();
        assert_eq!(bottoms.len(), 1);
        assert!(bottoms[0].corners.iter().all(|c| c[1] == 0.0));
        // The step's south face shows the upper half of the layer.
        let step_south = out
            .iter()
            .find(|q| q.face == Face::PosZ && q.corners[0][1] == 0.5)
            .expect("step");
        assert_eq!(step_south.uv[2][1], 0.0);
        let plane = out.last().expect("plane");
        assert_eq!(plane.source, CellImage::Block);
        assert!(plane.corners.iter().all(|c| c[1] == 0.5));
    }
}
