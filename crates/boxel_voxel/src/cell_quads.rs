//! Quads a cell draws from its data (EW-ITEM, ADR 0050): the item an item frame holds, the items
//! on a shelf. A block registers a [`CellQuadRule`]; the mesher asks it, for every cell of that
//! block with data ([`crate::BlockData`]), which [`CellQuad`]s to draw, and bakes them into the
//! chunk's mesh. A change of the data remeshes the chunk (ADR 0048), so nothing runs per frame.
//!
//! A cell quad is not a block quad: it shows an image of the content's own texture array (the
//! cell textures, one layer per image, which the content gives the renderer), not a block
//! layer, and it can sit anywhere in its cell at any angle. The engine names no image: the
//! rule says the layer.
use crate::block_data::BlockData;
use crate::planes::ModelPlane;
use crate::registry::BlockRegistry;
use crate::{BlockState, Face};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// One quad a cell draws from its data, in its cell: four corners anywhere in it, each with the
/// point of the image it shows. Drawn from both sides, alpha-tested like a cutout block, lit by
/// its cell's light without ambient occlusion, shaded like the cube face [`CellQuad::face`].
#[derive(Clone, Copy, Debug)]
pub struct CellQuad {
    /// Where each corner is in the cell, in blocks (`0..=1` on each axis for a quad inside it).
    /// Corner `k` sits at `(u, v) = (k & 1, k >> 1)` of the quad: 0 and 3 are opposite.
    pub corners: [[f32; 3]; 4],
    /// The point of its image each corner shows, as a fraction of the image (`(0, 0)` its top
    /// left, `(1, 1)` its bottom right).
    pub uv: [[f32; 2]; 4],
    /// The image: a layer of the cell texture array (`0..4096`).
    pub image: u16,
    /// Shaded as this face of a cube (`PosY` is full brightness, as a plant).
    pub face: Face,
    /// Drawn at the full brightness of its image, without light or face shading (the item of a
    /// glow item frame).
    pub full_bright: bool,
    /// What the image's color is multiplied by (sRGB, `[255; 3]` keeps it).
    pub tint: [u8; 3],
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
            face: Face::PosY,
            full_bright: false,
            tint: [255; 3],
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
    fn key(&self) -> ([[u32; 3]; 4], [[u32; 2]; 4], u16, Face, bool, [u8; 3]) {
        (
            self.corners.map(|c| c.map(f32::to_bits)),
            self.uv.map(|c| c.map(f32::to_bits)),
            self.image,
            self.face,
            self.full_bright,
            self.tint,
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
}
