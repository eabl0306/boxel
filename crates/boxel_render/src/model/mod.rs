//! Entity models (ADR 0040): a model is a tree of textured boxes (parts), drawn per instance with
//! a pose, lit like the terrain at the instance's position, depth tested with the world and
//! faded into the same fog.
//!
//! Model space is the one of Minecraft's Java Edition entity models, so a model and its
//! animations can be written as Minecraft (or Blockbench's Java entity models) writes them:
//! units are pixels (1/16 of a block), **+Y points down**, the model faces **-Z**, and a part's
//! rotation is the matrix `Rz * Ry * Rx` (Minecraft's `rotationZYX`) about its pivot. Each box
//! takes its texture from the skin in Minecraft's box-UV layout ([`Cuboid`]); an outer layer is
//! a box inflated by a fraction of a pixel, alpha tested ([`ModelMaterial::Cutout`]).
//!
//! - [`Model`]: the parts, built once with [`Model::builder`] and shared (`Arc`).
//! - [`Pose`]: per part an offset, a rotation and whether it shows; the game writes one every
//!   frame it animates ([`Model::rest_pose`] is the model as built).
//! - [`ModelTexture`]: a skin, raw RGBA from the game or from a resource pack
//!   ([`ModelTexture::from_pack_or`]).
//! - [`ModelInstance`]: the component the render plugin draws, with the entity's `Transform`.
//!   [`ModelLayer::View`] draws it relative to the camera over everything (a first-person hand).
//! - [`ModelDraw`]: one instance of one frame, for [`crate::VoxelRenderer::set_models`] without
//!   the ECS.
//!
//! The CPU poses every box into a small vertex buffer (`mesh`, `scene`), so the three tiers run
//! one pipeline on the same bytes (`model.wgsl`): an instance whose inputs did not change is not
//! posed again, and a frame where nothing changed uploads nothing. The renderer creates the
//! pipelines with the first instance; without instances every frame is what it was before.

mod extract;
mod mesh;
mod pass;
mod scene;

pub(crate) use extract::extract_models;
pub use mesh::{
    ENTITY_LIGHT_DIRECTIONS, FaceTexels, PosedFace, cuboid_faces, entity_shade, pose_parts,
    realm_shade,
};
pub(crate) use pass::ModelPass;
pub use scene::{ModelDrawRange, ModelScene};

use boxel_ecs::prelude::*;
use boxel_math::{DVec3, Quat, Vec3};
use boxel_transform::Transform;
use boxel_voxel::LightValue;
use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Index of a part in its [`Model`] (and in a [`Pose`] of it).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PartId(pub u16);

impl PartId {
    /// The index as `usize`.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// How a box is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ModelMaterial {
    /// The skin's alpha is ignored and back faces are culled (a body, a head).
    #[default]
    Opaque,
    /// Alpha tested (texels under half alpha are not drawn) and both sides drawn, so the inside
    /// of an outer layer shows through its holes (a hat, a jacket).
    Cutout,
}

/// One textured box of a part, in the part's frame (pixels, model space: +Y down).
///
/// Its texture is Minecraft's box-UV layout: from `uv`, with `(w, h, d)` the size, the top row
/// holds the top face (y = min, model space is +Y down) at `(u + d, v)` and the bottom face at
/// `(u + d + w, v)` (each `w` by `d`); the row below (`h` tall, from `v + d`) holds the side at
/// x = min (`d` wide, at `u`: an entity's right side, as the model faces -Z), the front (z =
/// min, `w` wide, at `u + d`), the side at x = max (`d`, at `u + d + w`) and the back (`w`, at
/// `u + 2d + w`). Each face shows its texture as seen from outside with the texture's top up
/// (the bottom face as seen from above, through the box), as in Minecraft.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cuboid {
    /// Top-left of the layout on the skin, in the model's texels.
    pub uv: [f32; 2],
    /// The box's min corner (pixels).
    pub origin: Vec3,
    /// Its size (pixels, each at least 0). The texture layout uses it as is.
    pub size: Vec3,
    /// Grows the box by this much on every side without changing its texture (Minecraft's
    /// `CubeDeformation`: 0.5 for a hat, 0.25 for a jacket and sleeves).
    pub inflate: f32,
    /// Mirrors the box along x: its texture shows mirrored, and the side textures swap sides
    /// (Minecraft's `mirror`, a left limb with the right limb's texture).
    pub mirror: bool,
    /// How it is drawn.
    pub material: ModelMaterial,
}

impl Cuboid {
    /// An opaque box from `origin` of `size`, its texture layout at `uv`.
    pub fn new(uv: [f32; 2], origin: impl Into<Vec3>, size: impl Into<Vec3>) -> Self {
        Self {
            uv,
            origin: origin.into(),
            size: size.into(),
            inflate: 0.0,
            mirror: false,
            material: ModelMaterial::Opaque,
        }
    }

    /// The same box grown by `inflate` on every side.
    pub fn inflate(mut self, inflate: f32) -> Self {
        self.inflate = inflate;
        self
    }

    /// The same box mirrored along x.
    pub fn mirror(mut self) -> Self {
        self.mirror = true;
        self
    }

    /// The same box, alpha tested ([`ModelMaterial::Cutout`]).
    pub fn cutout(mut self) -> Self {
        self.material = ModelMaterial::Cutout;
        self
    }
}

/// Where a part sits in its parent and how it turns: Minecraft's `PartPose`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PartPose {
    /// The pivot in the parent's frame (pixels, model space: +Y down).
    pub offset: Vec3,
    /// Rotation about the pivot, radians: the matrix `Rz(z) * Ry(y) * Rx(x)`.
    pub rotation: Vec3,
    /// Drawn; a hidden part hides its children too.
    pub visible: bool,
}

impl Default for PartPose {
    fn default() -> Self {
        Self::at(Vec3::ZERO)
    }
}

impl PartPose {
    /// At `offset`, not turned, visible.
    pub fn at(offset: impl Into<Vec3>) -> Self {
        Self {
            offset: offset.into(),
            rotation: Vec3::ZERO,
            visible: true,
        }
    }
}

/// One part: a name, its parent, its pose as built and its boxes.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelPart {
    /// Unique in the model.
    pub name: String,
    /// The part it hangs from (always before it), `None` for a root.
    pub parent: Option<PartId>,
    /// The pose of [`Model::rest_pose`].
    pub rest: PartPose,
    /// The boxes, in the part's frame.
    pub cuboids: Vec<Cuboid>,
}

/// Why a model or a texture was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    /// Two parts with one name.
    #[error("two parts named {0:?}")]
    DuplicatePart(String),
    /// A box with a negative or non-finite size, origin, inflate or texture origin.
    #[error("part {part:?}: box {index} has a negative or non-finite number")]
    BadCuboid {
        /// The part.
        part: String,
        /// The box's index in the part.
        index: usize,
    },
    /// A pose number that is not finite.
    #[error("part {0:?}: its pose is not finite")]
    BadPose(String),
    /// More parts than [`PartId`] counts.
    #[error("{0} parts, more than 65535")]
    TooManyParts(usize),
    /// A texture of size 0 or bigger than 4096, or whose bytes are not `width * height * 4`.
    #[error("texture {width}x{height} with {bytes} bytes")]
    BadTexture {
        /// Width.
        width: u32,
        /// Height.
        height: u32,
        /// Bytes given.
        bytes: usize,
    },
}

/// A model: its parts, parents first, and the size of the texture its boxes are laid out on.
/// Immutable once built; share it with an `Arc`.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    texture_size: [u32; 2],
    parts: Vec<ModelPart>,
    /// Faces of each material with every part shown.
    faces: [u32; 2],
}

impl Model {
    /// Starts a model whose boxes are laid out on a texture of `texture_size` texels (64 by 64
    /// for Minecraft's player). A skin of another size is read in proportion: a 128 by 128 skin
    /// has two texels per model texel.
    pub fn builder(texture_size: [u32; 2]) -> ModelBuilder {
        ModelBuilder {
            texture_size: texture_size.map(|s| s.max(1)),
            parts: Vec::new(),
        }
    }

    /// The texture size the boxes are laid out on.
    pub fn texture_size(&self) -> [u32; 2] {
        self.texture_size
    }

    /// The parts, parents before their children.
    pub fn parts(&self) -> &[ModelPart] {
        &self.parts
    }

    /// The part named `name`.
    pub fn part(&self, name: &str) -> Option<PartId> {
        self.parts
            .iter()
            .position(|p| p.name == name)
            .map(|i| PartId(i as u16))
    }

    /// The pose of every part as built.
    pub fn rest_pose(&self) -> Pose {
        Pose {
            parts: self.parts.iter().map(|p| p.rest).collect(),
        }
    }

    /// Faces of opaque and cutout boxes with every part shown (each one quad).
    pub fn faces(&self) -> [u32; 2] {
        self.faces
    }
}

/// Builds a [`Model`] part by part ([`Model::builder`]).
#[derive(Clone, Debug)]
pub struct ModelBuilder {
    texture_size: [u32; 2],
    parts: Vec<ModelPart>,
}

impl ModelBuilder {
    /// Adds a part named `name` under `parent` (a part added before, `None` for a root) at its
    /// rest pose `rest`.
    pub fn part(&mut self, name: &str, parent: Option<PartId>, rest: PartPose) -> PartId {
        let id = PartId(self.parts.len().min(usize::from(u16::MAX)) as u16);
        self.parts.push(ModelPart {
            name: name.to_string(),
            parent: parent.filter(|p| p.index() < self.parts.len()),
            rest,
            cuboids: Vec::new(),
        });
        id
    }

    /// Adds a box to `part`.
    pub fn cuboid(&mut self, part: PartId, cuboid: Cuboid) -> &mut Self {
        if let Some(p) = self.parts.get_mut(part.index()) {
            p.cuboids.push(cuboid);
        }
        self
    }

    /// The model, checked: unique part names, finite numbers, sizes and inflates at least 0.
    pub fn build(self) -> Result<Model, ModelError> {
        if self.parts.len() > usize::from(u16::MAX) {
            return Err(ModelError::TooManyParts(self.parts.len()));
        }
        let mut faces = [0u32; 2];
        for (i, part) in self.parts.iter().enumerate() {
            if self.parts[..i].iter().any(|p| p.name == part.name) {
                return Err(ModelError::DuplicatePart(part.name.clone()));
            }
            let rest = part.rest;
            if !(rest.offset.is_finite() && rest.rotation.is_finite()) {
                return Err(ModelError::BadPose(part.name.clone()));
            }
            for (index, c) in part.cuboids.iter().enumerate() {
                let ok = c.origin.is_finite()
                    && c.size.is_finite()
                    && c.size.min_element() >= 0.0
                    && c.inflate.is_finite()
                    && c.inflate >= 0.0
                    && c.uv.iter().all(|v| v.is_finite() && *v >= 0.0);
                if !ok {
                    return Err(ModelError::BadCuboid {
                        part: part.name.clone(),
                        index,
                    });
                }
                faces[c.material as usize] += 6;
            }
        }
        Ok(Model {
            texture_size: self.texture_size,
            parts: self.parts,
            faces,
        })
    }
}

/// A pose of a [`Model`]: one [`PartPose`] per part, indexed by [`PartId`]. Start from
/// [`Model::rest_pose`] and change what the animation moves.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pose {
    /// One per part of the model, in its order.
    pub parts: Vec<PartPose>,
}

impl std::ops::Index<PartId> for Pose {
    type Output = PartPose;
    fn index(&self, part: PartId) -> &PartPose {
        &self.parts[part.index()]
    }
}

impl std::ops::IndexMut<PartId> for Pose {
    fn index_mut(&mut self, part: PartId) -> &mut PartPose {
        &mut self.parts[part.index()]
    }
}

/// Source of [`ModelTexture::id`]: unique in the process.
static TEXTURE_IDS: AtomicU64 = AtomicU64::new(1);

struct TextureData {
    id: u64,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// A model's texture (a skin): straight-alpha sRGB RGBA8, row by row from the top. Cheap to
/// clone; the renderer uploads it once per id ([`ModelTexture::id`]) while an instance uses it.
#[derive(Clone)]
pub struct ModelTexture(Arc<TextureData>);

impl fmt::Debug for ModelTexture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelTexture")
            .field("id", &self.0.id)
            .field("size", &[self.0.width, self.0.height])
            .finish()
    }
}

impl PartialEq for ModelTexture {
    fn eq(&self, other: &Self) -> bool {
        self.0.id == other.0.id
    }
}

impl Eq for ModelTexture {}

impl ModelTexture {
    /// The texture `width` by `height` (each 1 to 4096) with `rgba` (`width * height * 4`
    /// bytes).
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, ModelError> {
        let ok = (1..=4096).contains(&width)
            && (1..=4096).contains(&height)
            && rgba.len() == width as usize * height as usize * 4;
        if !ok {
            return Err(ModelError::BadTexture {
                width,
                height,
                bytes: rgba.len(),
            });
        }
        Ok(Self(Arc::new(TextureData {
            id: TEXTURE_IDS.fetch_add(1, Ordering::Relaxed),
            width,
            height,
            rgba,
        })))
    }

    /// Unique in the process: two textures made apart never share one, even with the same
    /// bytes.
    pub fn id(&self) -> u64 {
        self.0.id
    }

    /// Width in texels.
    pub fn width(&self) -> u32 {
        self.0.width
    }

    /// Height in texels.
    pub fn height(&self) -> u32 {
        self.0.height
    }

    /// The texels.
    pub fn rgba(&self) -> &[u8] {
        &self.0.rgba
    }

    /// The texture `id` (`game:entity/figure/plain` reads
    /// `assets/game/textures/entity/figure/plain.png`) of the resource pack at `pack`
    /// ([`boxel_pack::entity_texture`]); `Ok(None)` when the pack has none.
    pub fn from_pack(pack: &Path, id: &str) -> Result<Option<Self>, String> {
        let id = boxel_pack::ResourceId::parse(id);
        match boxel_pack::entity_texture(pack, &id) {
            Ok(Some(t)) => Self::new(t.width, t.height, t.rgba)
                .map(Some)
                .map_err(|e| format!("{id}: {e}")),
            Ok(None) => Ok(None),
            Err(e) => Err(format!("{id}: {e}")),
        }
    }

    /// The pack's texture `id` when `pack` is given and has it in the proportions of `fallback`
    /// (the same size or a whole multiple of it on both axes: a high-resolution pack), else
    /// `fallback` (the game's own image), with a warning saying why when the pack had a file
    /// that could not be used.
    pub fn from_pack_or(pack: Option<&Path>, id: &str, fallback: Self) -> (Self, Option<String>) {
        let Some(pack) = pack else {
            return (fallback, None);
        };
        match Self::from_pack(pack, id) {
            Ok(Some(t)) => {
                let (w, h) = (fallback.width(), fallback.height());
                let k = t.width() / w;
                if k >= 1 && t.width() == k * w && t.height() == k * h {
                    (t, None)
                } else {
                    let warning = format!(
                        "{id}: {}x{} is not {w}x{h} or a whole multiple of it; the game's own \
                         image is used",
                        t.width(),
                        t.height()
                    );
                    (fallback, Some(warning))
                }
            }
            Ok(None) => (fallback, None),
            Err(e) => (fallback, Some(e)),
        }
    }
}

/// Where an instance is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ModelLayer {
    /// In the world: the instance's `Transform` is a world placement, and the model is depth
    /// tested with the terrain in the main pass (after the opaque and cutout terrain and the far
    /// LOD, before the block outline and the water).
    #[default]
    World,
    /// Relative to the camera, over everything: the `Transform` is in the camera's frame (the
    /// eye at the origin, looking down -Z, +Y up, +X right), and the model is drawn after the
    /// whole world (the fog included) over a cleared depth, so it never sinks into a wall: a
    /// first-person hand. Lit by the world's light at the eye; its faces are shaded as seen
    /// from the camera, so its look does not change as the view turns.
    View,
}

/// What lights an instance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ModelLight {
    /// The world's light (sky and block light, as the terrain at that cell) of the block at this
    /// offset in blocks: from the `Transform`'s translation, not turned with it, for
    /// [`ModelLayer::World`]; from the eye for [`ModelLayer::View`]. Open sky where nothing is
    /// loaded.
    Probe(Vec3),
    /// This light.
    Fixed(LightValue),
}

impl Default for ModelLight {
    fn default() -> Self {
        Self::Probe(Vec3::ZERO)
    }
}

/// A model instance the render plugin draws every frame, placed by the entity's `Transform`:
/// the model point [`ModelInstance::origin`] goes to its translation, and the model turns with
/// its rotation (the model's front, -Z, along the transform's forward). One part of the model
/// becomes `scale / 16` blocks.
#[derive(Component, Clone, Debug)]
#[require(Transform)]
pub struct ModelInstance {
    /// The model.
    pub model: Arc<Model>,
    /// Its skin.
    pub texture: ModelTexture,
    /// The pose of this frame (one entry per part of `model`; extra or missing entries are
    /// ignored or take the rest pose).
    pub pose: Pose,
    /// Where it is drawn.
    pub layer: ModelLayer,
    /// The model point (pixels, model space) at the `Transform`'s translation: the feet of a
    /// standing model whose legs end at y = 24 are at (0, 24, 0).
    pub origin: Vec3,
    /// Size multiplier (Minecraft draws its player at 0.9375).
    pub scale: f32,
    /// Drawn.
    pub visible: bool,
    /// What lights it.
    pub light: ModelLight,
}

impl ModelInstance {
    /// `model` with `texture` at its rest pose, in the world, at its origin (0, 0, 0), scale 1,
    /// visible, lit at its translation.
    pub fn new(model: Arc<Model>, texture: ModelTexture) -> Self {
        let pose = model.rest_pose();
        Self {
            model,
            texture,
            pose,
            layer: ModelLayer::World,
            origin: Vec3::ZERO,
            scale: 1.0,
            visible: true,
            light: ModelLight::default(),
        }
    }
}

/// One instance of one frame for [`crate::VoxelRenderer::set_models`] (the render plugin makes
/// them from the [`ModelInstance`]s). Two draws are equal when they draw the same: the same
/// model (the same `Arc`), skin, pose and placement.
#[derive(Clone, Debug)]
pub struct ModelDraw {
    /// Stable while the instance lives (the render plugin uses the entity): an instance whose
    /// draw is the same as last frame's is not posed again.
    pub id: u64,
    /// The model.
    pub model: Arc<Model>,
    /// Its skin.
    pub texture: ModelTexture,
    /// Its pose.
    pub pose: Pose,
    /// Where it is drawn.
    pub layer: ModelLayer,
    /// [`ModelLayer::World`]: world position; [`ModelLayer::View`]: position in the camera's
    /// frame. Of the model's `origin`.
    pub position: DVec3,
    /// Rotation (world, or in the camera's frame).
    pub rotation: Quat,
    /// The model point at `position` (pixels).
    pub origin: Vec3,
    /// Size multiplier.
    pub scale: f32,
    /// Its light this frame.
    pub light: LightValue,
    /// The sky's red, green and blue at its light's cell, when colored glass tinted it (ADR
    /// 0064): each sky channel lights it in place of `light`'s sky level. `None`: no tint.
    pub sky_rgb: Option<[u8; 3]>,
    /// The realm it is lit in.
    pub realm: ModelRealm,
}

/// The realm an instance is lit in (ADR 0039): the realm of the block its light comes from. Its
/// light takes that realm's sky light and ambient floor, and its faces are shaded relative to
/// that realm's face shade ([`realm_shade`]). [`ModelRealm::default`] is realm 0 with the
/// default shade: the look of a world without realms.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelRealm {
    /// The realm (below `boxel_shaders::gpu::REALM_ENVS`).
    pub index: u8,
    /// Its face shade, in the mesher's face order (+Y, -Y, +X, -X, +Z, -Z).
    pub face_shade: [f32; 6],
}

impl Default for ModelRealm {
    fn default() -> Self {
        Self {
            index: 0,
            face_shade: boxel_world::DEFAULT_FACE_SHADE,
        }
    }
}

impl PartialEq for ModelDraw {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && Arc::ptr_eq(&self.model, &other.model)
            && self.texture == other.texture
            && self.layer == other.layer
            && self.position == other.position
            && self.rotation == other.rotation
            && self.origin == other.origin
            && self.scale == other.scale
            && self.light == other.light
            && self.sky_rgb == other.sky_rgb
            && self.realm == other.realm
            && self.pose == other.pose
    }
}

/// Counters of the entity models (in [`crate::RenderStats::models`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModelStats {
    /// Instances drawn last frame.
    pub instances: u32,
    /// Their quads (one per box face).
    pub quads: u32,
    /// Draw calls last frame (one per layer, material and skin with quads); not counted in
    /// [`crate::RenderStats::draws`].
    pub draws: u32,
    /// Instances posed again last frame (their draw changed).
    pub posed: u32,
    /// Vertex buffer uploads (cumulative): one a frame where anything changed, none in a still
    /// scene.
    pub uploads: u64,
    /// Their bytes (cumulative).
    pub upload_bytes: u64,
    /// Skins on the GPU.
    pub textures: u32,
    /// Instances left out (cumulative): a skin bigger than the device takes, or an instance more
    /// than 2^20 blocks from the camera.
    pub skipped: u64,
}
