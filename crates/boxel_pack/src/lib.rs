//! `boxel_pack`: read a Minecraft Java Edition resource pack from disk into Rust data.
//!
//! Loads `pack.mcmeta` (including overlays), block textures (`PNG` + `.png.mcmeta`
//! animations), block models (parent inheritance and `#texture` variables) and blockstates
//! (`variants` / `multipart`). Does **not** talk to the renderer or the block registry.
//!
//! Supported pack formats: roughly 64 through 120. Unknown asset kinds (sounds, lang, fonts,
//! shaders, texts, optifine, emissive, …) are skipped with a [`PackWarning`].
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
mod id;
mod meta;
mod model;
mod overlay;
mod pack;
mod texture;

pub use animation::{
    Animation, AnimationFrame, FrameAt, TimedFrame, Timeline, mix_channel, mix_pixel, split_frames,
};
pub use blockstate::{
    BlockState, Condition, MultipartCase, VariantList, VariantModel, condition_matches,
    parse_variant_key,
};
pub use error::{PackError, PackWarning};
pub use id::ResourceId;
pub use meta::{FormatRange, OpenOptions, OverlayEntry, PackMeta};
pub use model::{
    Axis, FaceName, ModelElement, ModelFace, ModelRotation, RawModel, ResolvedModel, Vec3,
    resolve_model,
};
pub use pack::ResourcePack;
pub use texture::Texture;
