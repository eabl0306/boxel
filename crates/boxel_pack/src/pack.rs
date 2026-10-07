//! Top-level [`ResourcePack`] loader.

use std::collections::BTreeMap;
use std::path::Path;

use crate::blockstate::{self, BlockState};
use crate::error::{PackError, PackWarning};
use crate::id::ResourceId;
use crate::meta::{self, OpenOptions, PackMeta};
use crate::model::{self, ResolvedModel};
use crate::overlay;
use crate::texture::{self, Texture};

/// A Minecraft Java resource pack loaded from disk (read-only data model).
#[derive(Debug)]
pub struct ResourcePack {
    meta: PackMeta,
    /// Major format used to select overlays.
    format: u32,
    textures: BTreeMap<ResourceId, Texture>,
    models: BTreeMap<ResourceId, ResolvedModel>,
    blockstates: BTreeMap<ResourceId, BlockState>,
    warnings: Vec<PackWarning>,
}

impl ResourcePack {
    /// Opens a pack directory with default format selection.
    pub fn open(path: &Path) -> Result<Self, PackError> {
        Self::open_with(path, OpenOptions::default())
    }

    /// Opens a pack directory, optionally forcing the pack format used for overlays.
    pub fn open_with(path: &Path, options: OpenOptions) -> Result<Self, PackError> {
        if !path.is_dir() {
            return Err(PackError::NotADirectory(path.to_path_buf()));
        }
        let mut warnings = Vec::new();
        let (meta, meta_warnings) = meta::load_meta(path)?;
        warnings.extend(meta_warnings);

        let format = meta::effective_format(&meta, options);
        let overlays = meta::applicable_overlays(&meta, options.format);
        let index =
            overlay::build_index(&meta, format, &overlays, &mut warnings).map_err(|source| {
                PackError::Io {
                    path: path.to_path_buf(),
                    source,
                }
            })?;

        let textures_vec = texture::load_block_textures(&index, &mut warnings)?;
        let textures: BTreeMap<_, _> = textures_vec.into_iter().collect();

        let raw_models = model::load_raw_models(&index, &mut warnings)?;
        let models = model::resolve_all(&raw_models, &mut warnings);

        let blockstates = blockstate::load_blockstates(&index, &mut warnings)?;

        Ok(Self {
            meta,
            format,
            textures,
            models,
            blockstates,
            warnings,
        })
    }

    /// Pack metadata (`pack.mcmeta`).
    pub fn meta(&self) -> &PackMeta {
        &self.meta
    }

    /// Major format used when resolving overlays.
    pub fn format(&self) -> u32 {
        self.format
    }

    /// Non-fatal warnings collected during load.
    pub fn warnings(&self) -> &[PackWarning] {
        &self.warnings
    }

    /// Iterates loaded block textures.
    pub fn block_textures(&self) -> impl Iterator<Item = (&ResourceId, &Texture)> {
        self.textures.iter()
    }

    /// Number of loaded block textures.
    pub fn texture_count(&self) -> usize {
        self.textures.len()
    }

    /// Looks up a block texture by id (`minecraft:block/stone`).
    pub fn texture(&self, id: &ResourceId) -> Option<&Texture> {
        self.textures.get(id)
    }

    /// Resolved block model (inheritance and `#` vars already applied).
    pub fn model(&self, id: &ResourceId) -> Option<&ResolvedModel> {
        self.models.get(id)
    }

    /// Iterates resolved block models that loaded successfully.
    pub fn models(&self) -> impl Iterator<Item = (&ResourceId, &ResolvedModel)> {
        self.models.iter()
    }

    /// Number of resolved models.
    pub fn model_count(&self) -> usize {
        self.models.len()
    }

    /// Blockstate for a block id (`minecraft:stone`).
    pub fn blockstate(&self, id: &ResourceId) -> Option<&BlockState> {
        self.blockstates.get(id)
    }

    /// Iterates blockstates.
    pub fn blockstates(&self) -> impl Iterator<Item = (&ResourceId, &BlockState)> {
        self.blockstates.iter()
    }

    /// Number of blockstates.
    pub fn blockstate_count(&self) -> usize {
        self.blockstates.len()
    }
}
