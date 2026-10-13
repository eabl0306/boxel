//! `boxel_pack`: read a Minecraft Java Edition resource pack from disk into Rust data.
//!
//! Loads `pack.mcmeta` (including overlays), block textures (`PNG` + `.png.mcmeta`
//! animations), block models (parent inheritance and `#texture` variables) and blockstates
//! (`variants` / `multipart`). Does **not** talk to the renderer or the block registry: a
//! [`LayerRecipe`] is plain data a game hands the renderer, saying which pack textures fill each
//! of its texture layers.
//!
//! [`GuiPack`] reads the GUI side of a pack when the game asks for it: GUI sprites with their
//! `gui.scaling` (`stretch`, `tile`, `nine_slice`), other textures by id and bitmap fonts
//! (`font/*.json` with `bitmap`, `space` and `reference` providers). The game names the sprites
//! and fonts it wants, as it passes a [`LayerRecipe`] per texture layer; `boxel_ui` (feature
//! `pack`) puts them in its atlas and font.
//!
//! Supported pack formats: roughly 64 through 120. [`ResourcePack`] skips other asset kinds
//! (sounds, lang, fonts, GUI textures, shaders, texts, optifine, emissive, …) with a
//! [`PackWarning`].
//!
//! # Example
//!
//! ```rust,no_run
//! use std::path::Path;
//! use boxel_pack::ResourcePack;
//!
//! let pack = ResourcePack::open(Path::new("assets/Whimscape")).expect("pack");
//! println!(
//!     "{} textures, {} models, {} blockstates, {} warnings",
//!     pack.texture_count(),
//!     pack.model_count(),
//!     pack.blockstate_count(),
//!     pack.warnings().len()
//! );
//! ```

#![forbid(unsafe_code)]

mod animation;
mod blockstate;
mod builtin_models;
mod error;
mod font;
mod gui;
mod id;
mod meta;
mod model;
mod overlay;
mod pack;
mod recipe;
mod texture;

pub use animation::{
    Animation, AnimationFrame, FrameAt, TimedFrame, Timeline, mix_channel, mix_pixel, split_frames,
};
pub use blockstate::{
    BlockState, Condition, MultipartCase, VariantList, VariantModel, condition_matches,
    parse_variant_key,
};
pub use error::{PackError, PackWarning};
pub use font::{BitmapProvider, FontProvider, PackFont, SpaceProvider};
pub use gui::{GuiPack, GuiScaling, GuiTexture};
pub use id::ResourceId;
pub use meta::{FormatRange, OpenOptions, OverlayEntry, PackMeta};
pub use model::{
    Axis, FaceName, ModelElement, ModelFace, ModelRotation, RawModel, ResolvedModel, Vec3,
    resolve_model,
};
pub use pack::ResourcePack;
pub use recipe::LayerRecipe;
pub use texture::Texture;
