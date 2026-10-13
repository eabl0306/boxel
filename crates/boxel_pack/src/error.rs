//! Errors and non-fatal warnings produced while reading a resource pack.

use std::path::PathBuf;

use thiserror::Error;

use crate::id::ResourceId;

/// Fatal failure while opening or decoding a pack.
#[derive(Debug, Error)]
pub enum PackError {
    /// `pack.mcmeta` is missing at the pack root.
    #[error("missing pack.mcmeta in {0}")]
    MissingMeta(PathBuf),
    /// I/O failure for a pack file.
    #[error("io error for {path}: {source}")]
    Io {
        /// File that failed.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// JSON that does not parse; always includes the file path.
    #[error("invalid JSON in {path}: {source}")]
    InvalidJson {
        /// File that failed to parse.
        path: PathBuf,
        /// Serde/JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// PNG that cannot be decoded as RGBA8.
    #[error("invalid PNG in {path}: {message}")]
    InvalidPng {
        /// Texture file.
        path: PathBuf,
        /// Decoder message.
        message: String,
    },
    /// Model `parent` chain loops back on itself.
    #[error("model parent cycle involving {0}")]
    ModelParentCycle(ResourceId),
    /// A `#variable` in a model was never bound to a concrete texture.
    #[error("undefined texture variable #{name} in model {model}")]
    UndefinedTextureVar {
        /// Model being resolved.
        model: ResourceId,
        /// Variable name without the leading `#`.
        name: String,
    },
    /// Pack root is not a directory.
    #[error("pack path is not a directory: {0}")]
    NotADirectory(PathBuf),
    /// Blockstate content that is structurally wrong after JSON parse.
    #[error("invalid blockstate in {path}: {message}")]
    InvalidBlockstate {
        /// Blockstate file.
        path: PathBuf,
        /// What went wrong.
        message: String,
    },
    /// Referenced model id is neither in the pack nor a known builtin.
    #[error("missing model {0}")]
    MissingModel(ResourceId),
    /// A GUI texture's `.png.mcmeta` (its `gui.scaling`, say) that Minecraft would reject.
    #[error("invalid GUI texture metadata in {path}: {message}")]
    InvalidGuiMeta {
        /// The `.png.mcmeta` file.
        path: PathBuf,
        /// What went wrong.
        message: String,
    },
    /// A font definition (`font/*.json`) that Minecraft would reject.
    #[error("invalid font in {path}: {message}")]
    InvalidFont {
        /// The font file.
        path: PathBuf,
        /// What went wrong.
        message: String,
    },
}

/// Non-fatal note collected while loading (ignored folders, odd sizes, skipped models).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackWarning {
    /// Short machine-oriented kind, e.g. `ignored_dir`, `size_mismatch`.
    pub kind: String,
    /// Human-readable detail, often including a path.
    pub message: String,
}

impl PackWarning {
    /// Builds a warning with the given kind and message.
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
        }
    }
}
