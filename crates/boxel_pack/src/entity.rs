//! Entity textures of a pack (skins, mob textures; ADR 0040), read when the game asks for one.

use std::path::Path;

use crate::error::PackError;
use crate::gui::GuiPack;
use crate::id::ResourceId;

/// A decoded entity texture: straight-alpha sRGB RGBA8, row by row from the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntityTexture {
    /// Width in texels.
    pub width: u32,
    /// Height in texels (one frame of an animated texture).
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// An entity texture by its id under `textures/`: `minecraft:entity/player/wide/steve` reads
/// `assets/minecraft/textures/entity/player/wide/steve.png` of the pack at `pack`, with its
/// overlays applied (the last applicable overlay holding the file wins, as for the GUI textures
/// of [`GuiPack`]). `Ok(None)` when the pack has no such file, so the game keeps its own image.
/// An animated texture (a `.png.mcmeta` with `animation`) gives its first frame; a PNG of more
/// than 4096 * 4096 texels is [`PackError::InvalidPng`], refused before it is decoded.
///
/// Opening reads `pack.mcmeta` only, so this is cheap next to [`crate::ResourcePack::open`].
pub fn entity_texture(pack: &Path, id: &ResourceId) -> Result<Option<EntityTexture>, PackError> {
    let gui = GuiPack::open(pack)?;
    Ok(gui.texture(id)?.map(|t| EntityTexture {
        width: t.width,
        height: t.height,
        rgba: t.rgba,
    }))
}
