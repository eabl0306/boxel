//! The GUI side of a pack ([`GuiPack`]): GUI sprites with their `gui.scaling`, other textures by
//! id and bitmap fonts, each read when the game asks for it.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::animation::{AnimationRaw, split_frames};
use crate::error::{PackError, PackWarning};
use crate::font::{self, PackFont};
use crate::id::ResourceId;
use crate::meta::{self, OpenOptions, PackMeta};
use crate::texture::decode_png;

/// How a GUI sprite fills a rectangle of another size: Minecraft's `gui.scaling` in the sprite's
/// `.png.mcmeta`. Sizes are GUI pixels; the texture may hold more texels per GUI pixel (a
/// high-resolution pack).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum GuiScaling {
    /// The whole sprite stretched over the rectangle (no `gui.scaling`, or `stretch`).
    #[default]
    Stretch,
    /// The sprite repeated from the top-left, each copy `width` by `height`; the last row and
    /// column are cut.
    Tile {
        /// Width of one copy.
        width: u32,
        /// Height of one copy.
        height: u32,
    },
    /// Corners drawn 1:1, edges and center repeated (cut at the end) or stretched.
    NineSlice {
        /// The sprite's width.
        width: u32,
        /// The sprite's height.
        height: u32,
        /// Left, top, right and bottom borders.
        border: [u32; 4],
        /// Stretch the edges and the center instead of repeating them.
        stretch_inner: bool,
    },
}

/// A decoded GUI texture (straight-alpha sRGB RGBA8) and how it scales.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuiTexture {
    /// Width in texels.
    pub width: u32,
    /// Height in texels (one frame of an animated texture).
    pub height: u32,
    /// `width * height * 4` bytes, row by row.
    pub rgba: Vec<u8>,
    /// From the `.png.mcmeta`; [`GuiScaling::Stretch`] without one.
    pub scaling: GuiScaling,
}

/// The GUI side of a Minecraft Java resource pack: GUI sprites, other textures and bitmap fonts,
/// read when asked for, with the pack's overlays applied (the last applicable overlay holding a
/// file wins, as in [`crate::ResourcePack`]).
///
/// Opening reads `pack.mcmeta` only, so it is cheap next to [`crate::ResourcePack::open`], which
/// decodes every block texture. The game names what it wants (`minecraft:widget/button`, the
/// font `minecraft:default`); a missing file is `Ok(None)`, so the game keeps its own look for it.
#[derive(Clone, Debug)]
pub struct GuiPack {
    meta: PackMeta,
    format: u32,
    /// `assets` directories: the base first, then the applicable overlays (later wins).
    roots: Vec<PathBuf>,
    warnings: Vec<PackWarning>,
}

impl GuiPack {
    /// Opens a pack directory with default format selection.
    pub fn open(path: &Path) -> Result<Self, PackError> {
        Self::open_with(path, OpenOptions::default())
    }

    /// Opens a pack directory, optionally forcing the pack format used for overlays.
    pub fn open_with(path: &Path, options: OpenOptions) -> Result<Self, PackError> {
        if !path.is_dir() {
            return Err(PackError::NotADirectory(path.to_path_buf()));
        }
        let (meta, mut warnings) = meta::load_meta(path)?;
        let format = meta::effective_format(&meta, options);
        let mut roots = vec![meta.root.join("assets")];
        for overlay in meta::applicable_overlays(&meta, options.format) {
            let dir = meta.root.join(&overlay.directory);
            if dir.is_dir() {
                roots.push(dir.join("assets"));
            } else {
                warnings.push(PackWarning::new(
                    "overlay",
                    format!(
                        "overlay directory '{}' not found (format {format})",
                        overlay.directory
                    ),
                ));
            }
        }
        Ok(Self {
            meta,
            format,
            roots,
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

    /// Non-fatal warnings of opening the pack (`pack.mcmeta`, missing overlay directories).
    pub fn warnings(&self) -> &[PackWarning] {
        &self.warnings
    }

    /// A GUI sprite: `minecraft:widget/button` reads
    /// `assets/minecraft/textures/gui/sprites/widget/button.png` and its `.png.mcmeta`.
    pub fn sprite(&self, id: &ResourceId) -> Result<Option<GuiTexture>, PackError> {
        let path = format!("textures/gui/sprites/{}.png", id.path);
        self.texture_file(&id.namespace, &path)
    }

    /// Any texture by its id under `textures/`: `minecraft:gui/title/minecraft` reads
    /// `assets/minecraft/textures/gui/title/minecraft.png`, `minecraft:font/ascii` the default
    /// font image. Its `.png.mcmeta` gives the scaling, as for a sprite.
    pub fn texture(&self, id: &ResourceId) -> Result<Option<GuiTexture>, PackError> {
        let path = format!("textures/{}.png", id.path);
        self.texture_file(&id.namespace, &path)
    }

    /// A font: `minecraft:default` reads `assets/minecraft/font/default.json` with its bitmap
    /// and space providers and their images, following `reference` providers inside the pack.
    /// Providers that cannot be used are skipped with a warning in [`PackFont::warnings`]; a
    /// font file that does not parse is an error. Each font file is read and each image decoded
    /// once, however many times the pack names them. The cells a font's bitmap providers name
    /// hold at most 2 * 4096 * 4096 texels together: the providers past that are skipped.
    pub fn font(&self, id: &ResourceId) -> Result<Option<PackFont>, PackError> {
        font::load(self, id)
    }

    /// The file at `assets/<namespace>/<path>` after overlays, if the pack has it. Names
    /// Minecraft would reject (`..`, upper case, a backslash) find nothing, so a font cannot
    /// point outside the pack.
    pub(crate) fn file(&self, namespace: &str, path: &str) -> Option<PathBuf> {
        if !valid_namespace(namespace) || !valid_path(path) {
            return None;
        }
        self.roots
            .iter()
            .rev()
            .map(|root| root.join(namespace).join(path))
            .find(|file| file.is_file())
    }

    /// Decodes the texture at `assets/<namespace>/<path>` (a `.png`) with its metadata.
    pub(crate) fn texture_file(
        &self,
        namespace: &str,
        path: &str,
    ) -> Result<Option<GuiTexture>, PackError> {
        let Some(png) = self.file(namespace, path) else {
            return Ok(None);
        };
        let image = decode_png(&png)?;
        let meta = match self.file(namespace, &format!("{path}.mcmeta")) {
            Some(file) => read_meta(&file)?,
            None => GuiMeta::default(),
        };
        let (width, height, rgba) = match meta.animation {
            None => (image.width, image.height, image.rgba),
            Some(raw) => {
                // An animated GUI texture shows the first frame of its sequence, after
                // Minecraft drops the frames it cannot play; with one frame or none left it is
                // static and shows strip frame 0. The dropped frames' warnings do not matter
                // for a still frame.
                let animation = raw.into_animation();
                let (fw, fh) = animation.frame_size(image.width, image.height);
                let frames = split_frames(image.width, image.height, &image.rgba, &animation)
                    .map_err(|message| PackError::InvalidGuiMeta {
                        path: png.clone(),
                        message,
                    })?;
                let first = animation
                    .timeline(frames.len() as u32, &mut Vec::new())
                    .map_or(0, |timeline| timeline.frames()[0].index);
                let frame = frames
                    .get(first as usize)
                    .or(frames.first())
                    .cloned()
                    .unwrap_or_default();
                (fw, fh, frame)
            }
        };
        Ok(Some(GuiTexture {
            width,
            height,
            rgba,
            scaling: meta.scaling,
        }))
    }
}

/// What a GUI texture's `.png.mcmeta` says.
#[derive(Debug, Default)]
struct GuiMeta {
    animation: Option<AnimationRaw>,
    scaling: GuiScaling,
}

#[derive(Debug, Deserialize)]
struct GuiMetaFile {
    #[serde(default)]
    animation: Option<AnimationRaw>,
    #[serde(default)]
    gui: Option<GuiSection>,
}

#[derive(Debug, Deserialize)]
struct GuiSection {
    #[serde(default)]
    scaling: Option<ScalingRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ScalingRaw {
    Stretch,
    Tile {
        width: u32,
        height: u32,
    },
    NineSlice {
        width: u32,
        height: u32,
        border: BorderRaw,
        #[serde(default)]
        stretch_inner: bool,
    },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BorderRaw {
    All(u32),
    Sides {
        left: u32,
        top: u32,
        right: u32,
        bottom: u32,
    },
}

/// Whether `width` by `height` is a size Minecraft takes: above 0 and an `int`.
fn valid_gui_size(width: u32, height: u32) -> bool {
    let valid = |n: u32| (1..=i32::MAX as u32).contains(&n);
    valid(width) && valid(height)
}

/// Reads a GUI texture's `.png.mcmeta`, with Minecraft's checks: sizes above 0 and up to
/// `i32::MAX` and a nine-slice that keeps a center on both axes.
fn read_meta(path: &Path) -> Result<GuiMeta, PackError> {
    let text = std::fs::read_to_string(path).map_err(|source| PackError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let value: Value = serde_json::from_str(&text).map_err(|source| PackError::InvalidJson {
        path: path.to_path_buf(),
        source,
    })?;
    let invalid = |message: String| PackError::InvalidGuiMeta {
        path: path.to_path_buf(),
        message,
    };
    let file = GuiMetaFile::deserialize(value).map_err(|e| invalid(e.to_string()))?;
    let scaling = match file.gui.and_then(|g| g.scaling) {
        None | Some(ScalingRaw::Stretch) => GuiScaling::Stretch,
        Some(ScalingRaw::Tile { width, height }) => {
            if !valid_gui_size(width, height) {
                return Err(invalid(format!("tile size {width}x{height}")));
            }
            GuiScaling::Tile { width, height }
        }
        Some(ScalingRaw::NineSlice {
            width,
            height,
            border,
            stretch_inner,
        }) => {
            let border = match border {
                BorderRaw::All(n) => [n; 4],
                BorderRaw::Sides {
                    left,
                    top,
                    right,
                    bottom,
                } => [left, top, right, bottom],
            };
            let [left, top, right, bottom] = border;
            if !valid_gui_size(width, height) {
                return Err(invalid(format!("nine-slice size {width}x{height}")));
            }
            if left.saturating_add(right) >= width {
                return Err(invalid(format!(
                    "nine-slice has no horizontal center: {left} + {right} >= {width}"
                )));
            }
            if top.saturating_add(bottom) >= height {
                return Err(invalid(format!(
                    "nine-slice has no vertical center: {top} + {bottom} >= {height}"
                )));
            }
            GuiScaling::NineSlice {
                width,
                height,
                border,
                stretch_inner,
            }
        }
    };
    Ok(GuiMeta {
        animation: file.animation,
        scaling,
    })
}

/// Minecraft's namespace characters: `a-z`, `0-9`, `_`, `-` and `.`.
fn valid_namespace(namespace: &str) -> bool {
    !namespace.is_empty()
        && namespace != "."
        && namespace != ".."
        && namespace
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.'))
}

/// Minecraft's path characters (a namespace's and `/`), with no empty, `.` or `..` segment.
fn valid_path(path: &str) -> bool {
    path.split('/').all(|segment| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .bytes()
                .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' | b'.'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_minecraft_rejects_find_nothing() {
        assert!(valid_namespace("minecraft"));
        assert!(valid_namespace("my_pack.v2"));
        assert!(!valid_namespace(""));
        assert!(!valid_namespace(".."));
        assert!(!valid_namespace("Minecraft"));
        assert!(valid_path("textures/gui/sprites/widget/button.png"));
        assert!(!valid_path("textures/../../secret.png"));
        assert!(!valid_path("/etc/passwd"));
        assert!(!valid_path("textures//a.png"));
        assert!(!valid_path("textures/a\\b.png"));
        assert!(!valid_path("textures/Button.png"));
    }
}
