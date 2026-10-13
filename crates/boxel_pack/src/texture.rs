//! Block texture loading (PNG → RGBA8) and optional animation.

use std::path::Path;

use crate::animation::{Animation, McMetaTextureFile, split_frames};
use crate::error::{PackError, PackWarning};
use crate::id::ResourceId;
use crate::overlay::{FileIndex, RelPath};

/// Decoded block texture.
#[derive(Debug, Clone)]
pub struct Texture {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels (full strip for animated textures).
    pub height: u32,
    /// Packed RGBA8 (`width * height * 4` bytes).
    pub rgba: Vec<u8>,
    /// Animation metadata when a `.png.mcmeta` with `animation` was present.
    pub animation: Option<Animation>,
    /// Individual frames when [`Self::animation`] is set (vertical strip split).
    pub frames: Vec<Vec<u8>>,
}

/// Loads every `assets/<ns>/textures/block/**/*.png` from the index.
pub fn load_block_textures(
    index: &FileIndex,
    warnings: &mut Vec<PackWarning>,
) -> Result<Vec<(ResourceId, Texture)>, PackError> {
    let mut out = Vec::new();
    let mut widths: Vec<(ResourceId, u32)> = Vec::new();

    let mut pngs: Vec<&RelPath> = index
        .files
        .keys()
        .filter(|rel| is_block_texture_png(rel))
        .collect();
    pngs.sort();

    for rel in pngs {
        let path = &index.files[rel];
        let Some(id) = resource_id_from_texture_rel(rel) else {
            warnings.push(PackWarning::new(
                "texture",
                format!("could not parse texture id from {rel}"),
            ));
            continue;
        };
        match load_one(path, index, rel, warnings) {
            Ok(tex) => {
                widths.push((id.clone(), tex.width));
                out.push((id, tex));
            }
            Err(e) => {
                // Corrupt individual textures become warnings so one bad PNG does not kill the pack.
                warnings.push(PackWarning::new("texture", e.to_string()));
            }
        }
    }

    // Width mismatch across the pack: warn, do not panic.
    if let Some((_, first_w)) = widths.first() {
        for (id, w) in &widths {
            if w != first_w {
                warnings.push(PackWarning::new(
                    "size_mismatch",
                    format!("texture {id} width {w} differs from pack common width {first_w}"),
                ));
            }
        }
    }

    Ok(out)
}

fn is_block_texture_png(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    if !lower.ends_with(".png") {
        return false;
    }
    // assets/<ns>/textures/block/....png
    let parts: Vec<&str> = rel.split('/').collect();
    parts.len() >= 5 && parts[0] == "assets" && parts[2] == "textures" && parts[3] == "block"
}

fn resource_id_from_texture_rel(rel: &str) -> Option<ResourceId> {
    // assets/<ns>/textures/block/<rest>.png → <ns>:block/<rest>
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.len() < 5 || parts[0] != "assets" || parts[2] != "textures" {
        return None;
    }
    let ns = parts[1];
    let after_block = &parts[3..]; // ["block", ...]
    let mut path = after_block.join("/");
    if let Some(stripped) = path.strip_suffix(".png") {
        path = stripped.to_string();
    } else if let Some(stripped) = path.strip_suffix(".PNG") {
        path = stripped.to_string();
    }
    Some(ResourceId::new(ns, path))
}

fn load_one(
    path: &Path,
    index: &FileIndex,
    rel: &str,
    warnings: &mut Vec<PackWarning>,
) -> Result<Texture, PackError> {
    let rgba_img = decode_png(path, |_, _| Ok(()))?;
    let mcmeta_rel = format!("{rel}.mcmeta");
    let animation = if let Some(meta_path) = index.files.get(&mcmeta_rel) {
        match load_animation(meta_path) {
            Ok(a) => a,
            Err(msg) => {
                warnings.push(PackWarning::new(
                    "animation",
                    format!("{mcmeta_rel}: {msg}"),
                ));
                None
            }
        }
    } else {
        None
    };

    let frames = if let Some(ref anim) = animation {
        match split_frames(rgba_img.width, rgba_img.height, &rgba_img.rgba, anim) {
            Ok(f) => f,
            Err(msg) => {
                warnings.push(PackWarning::new(
                    "animation",
                    format!("{rel}: failed to split frames: {msg}"),
                ));
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    if rgba_img.width > 0 && !rgba_img.width.is_multiple_of(16) {
        // Accept any size; note widths that are not 16 or a multiple.
        warnings.push(PackWarning::new(
            "size",
            format!(
                "{rel}: unusual texture width {} (expected 16 or a multiple)",
                rgba_img.width
            ),
        ));
    }

    Ok(Texture {
        width: rgba_img.width,
        height: rgba_img.height,
        rgba: rgba_img.rgba,
        animation,
        frames,
    })
}

pub(crate) struct RgbaImage {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<u8>,
}

/// Decodes a PNG to RGBA8. `allow` sees its width and height from the header first, and its
/// `Err` refuses the image before anything is decoded: the png crate's limits do not count the
/// image itself, so a few MB of PNG can ask for many GB.
pub(crate) fn decode_png(
    path: &Path,
    allow: impl FnOnce(u32, u32) -> Result<(), String>,
) -> Result<RgbaImage, PackError> {
    let file = std::fs::File::open(path).map_err(|source| PackError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(|e| PackError::InvalidPng {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    allow(reader.info().width, reader.info().height).map_err(|message| PackError::InvalidPng {
        path: path.to_path_buf(),
        message,
    })?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| PackError::InvalidPng {
            path: path.to_path_buf(),
            message: "png too large".into(),
        })?;
    let mut buf = vec![0; size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| PackError::InvalidPng {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
    buf.truncate(info.buffer_size());
    let data = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => {
            return Err(PackError::InvalidPng {
                path: path.to_path_buf(),
                message: "indexed png was not expanded".into(),
            });
        }
    };
    Ok(RgbaImage {
        width: info.width,
        height: info.height,
        rgba: data,
    })
}

fn load_animation(path: &Path) -> Result<Option<Animation>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let file: McMetaTextureFile = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(file
        .animation
        .map(super::animation::AnimationRaw::into_animation))
}
