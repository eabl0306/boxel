use crate::{CUTOUT_BUCKET, ChunkMesh, PaddedChunk, TRANSLUCENT_BUCKET, VoxelClass};
use boxel_voxel::{BlockRegistry, ChunkData, Face, RenderShape};
use std::collections::BTreeSet;

/// Mesher-side view of the registry: per material id, bgm class, render shape (bucket routing)
/// and the texture layer of each face. Built once per registry and shared (`Arc`) by mesh jobs.
#[derive(Clone, Debug, Default)]
pub struct MaterialTable {
    class: Vec<VoxelClass>,
    shape: Vec<RenderShape>,
    layers: Vec<[u16; 6]>,
}

impl MaterialTable {
    /// From the registry (material id = `BlockRegistry::material`).
    pub fn from_registry(registry: &BlockRegistry) -> Self {
        let n = registry.len();
        let mut class = vec![VoxelClass::Empty; n];
        let mut shape = vec![RenderShape::None; n];
        let mut layers = vec![[0u16; 6]; n];
        for i in 0..n {
            let s = boxel_voxel::BlockState(i as u16);
            let m = registry.material(s) as usize;
            if m == 0 {
                continue;
            }
            shape[m] = registry.shape(s);
            class[m] = match shape[m] {
                RenderShape::None => VoxelClass::Empty,
                RenderShape::Opaque => VoxelClass::Opaque,
                RenderShape::Cutout | RenderShape::Translucent => VoxelClass::Transparent,
            };
            layers[m] = Face::ALL.map(|f| registry.face_texture(s, f).0);
        }
        Self {
            class,
            shape,
            layers,
        }
    }
    /// Class of a material (out of range -> `Empty`).
    pub fn class(&self, material: u16) -> VoxelClass {
        self.class
            .get(material as usize)
            .copied()
            .unwrap_or(VoxelClass::Empty)
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
    /// Texture layer of `face`.
    pub fn layer(&self, material: u16, face: Face) -> u16 {
        self.layers
            .get(material as usize)
            .map_or(0, |l| l[face.index()])
    }
    /// Test helper: explicit table (`shape[i]` and `layers[i]` for material `i`; the bgm
    /// class is derived from the shape exactly as in [`MaterialTable::from_registry`]).
    pub fn from_parts(shape: Vec<RenderShape>, layers: Vec<[u16; 6]>) -> Self {
        assert!(shape.len() == layers.len());
        let class = shape
            .iter()
            .map(|s| match s {
                RenderShape::None => VoxelClass::Empty,
                RenderShape::Opaque => VoxelClass::Opaque,
                RenderShape::Cutout | RenderShape::Translucent => VoxelClass::Transparent,
            })
            .collect();
        Self {
            class,
            shape,
            layers,
        }
    }
}

/// Center chunk plus its 26 neighbors in `boxel_voxel::NEIGHBOR_OFFSETS` order. A `None`
/// neighbor is treated as empty (air).
pub struct MeshInput<'a> {
    /// Chunk to mesh.
    pub center: &'a ChunkData,
    /// Neighbors.
    pub neighbors: [Option<&'a ChunkData>; 26],
}

/// Reusable mesher (one per worker thread; ~100 KiB of scratch).
pub struct Mesher {
    inner: binary_greedy_meshing::Mesher<32>,
    padded: PaddedChunk,
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
            inner: binary_greedy_meshing::Mesher::new(),
            padded: PaddedChunk::new(),
        }
    }

    /// Fills `out` from the center chunk and the border voxels of its neighbors (face
    /// neighbors: 32x32 slabs, edges: 32 rows, corners: 1 voxel). Uniform chunks take a fast path.
    pub fn gather(
        &mut self,
        input: &MeshInput<'_>,
        materials: &MaterialTable,
        out: &mut PaddedChunk,
    ) {
        todo!("WP5")
    }

    /// Meshes a padded chunk: bgm `fast_mesh`, then converts every bgm quad to the canonical
    /// [`crate::PackedQuad`] (via `Face::vertices_packed` min/max; positive faces subtract 1 on
    /// the normal axis), assigns `MaterialTable::layer`, routes each quad to
    /// `MaterialTable::bucket` and writes `out` bucket-contiguous.
    pub fn mesh(&mut self, padded: &PaddedChunk, materials: &MaterialTable, out: &mut ChunkMesh) {
        todo!("WP5")
    }

    /// `gather` + `mesh`. Fast paths: center uniform air -> empty mesh; center uniform opaque with
    /// all 6 face neighbors uniform opaque -> empty mesh.
    pub fn mesh_chunk(&mut self, input: &MeshInput<'_>, materials: &MaterialTable) -> ChunkMesh {
        todo!("WP5")
    }
}

/// Brute-force oracle: every visible unit face `(voxel, face)` of the inner 32^3 (a face is
/// visible if the voxel is non-empty and the neighbor is empty, or transparent with a different
/// material). The mesher's output, expanded with `PackedQuad::unit_faces`, must equal this set.
pub fn reference_visible_faces(padded: &PaddedChunk) -> BTreeSet<([u32; 3], Face)> {
    todo!("WP5")
}
