//! A pack's fonts (`assets/<ns>/font/<name>.json`): bitmap and space providers, with
//! `reference` providers followed inside the pack.

use std::path::Path;

use serde_json::Value;

use crate::error::{PackError, PackWarning};
use crate::gui::GuiPack;
use crate::id::ResourceId;
use crate::texture::decode_png;

/// How deep `reference` providers may nest (Minecraft's own fonts use one level).
const MAX_REFERENCE_DEPTH: usize = 8;

/// A pack's font: its providers in Minecraft's order, where the first provider that has a
/// character draws it. `reference` providers are replaced by the providers of the font they
/// name; providers behind a `filter` are kept only for Minecraft's default options (uniform
/// font off, Japanese variants off).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PackFont {
    /// The usable providers, first wins.
    pub providers: Vec<FontProvider>,
    /// Providers that were skipped and why (`ttf`, `unihex`, a reference to a font the pack
    /// lacks, an image that is missing...).
    pub warnings: Vec<PackWarning>,
}

/// One provider of a [`PackFont`].
#[derive(Clone, Debug, PartialEq)]
pub enum FontProvider {
    /// Glyphs cut from an image grid.
    Bitmap(BitmapProvider),
    /// Characters that draw nothing and only move the pen.
    Space(SpaceProvider),
}

/// Minecraft's `bitmap` provider: an image cut into a grid of equal cells, one character per
/// cell. Row `i` of the grid holds the characters of `chars[i]`, left to right.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BitmapProvider {
    /// The image's texture id (`minecraft:font/ascii.png`, under `textures/`).
    pub file: ResourceId,
    /// Image width in texels.
    pub width: u32,
    /// Image height in texels.
    pub height: u32,
    /// The image, straight-alpha sRGB RGBA8, row by row.
    pub rgba: Vec<u8>,
    /// One string per row of cells, all with the same number of characters; `'\0'` marks an
    /// empty cell.
    pub chars: Vec<String>,
    /// Height of a glyph in GUI pixels (Minecraft's `height`, 8 by default), whatever the
    /// cell's height in texels.
    pub glyph_height: u32,
    /// GUI pixels from the glyph's top to the baseline (Minecraft's `ascent`): 7 puts the
    /// glyph's top on the line's top.
    pub ascent: i32,
}

impl BitmapProvider {
    /// Width and height of one cell in texels (the image divided by the grid, rounded down).
    pub fn cell(&self) -> [u32; 2] {
        let columns = self.chars.first().map_or(0, |row| row.chars().count()) as u32;
        let rows = self.chars.len() as u32;
        [
            self.width.checked_div(columns).unwrap_or(0),
            self.height.checked_div(rows).unwrap_or(0),
        ]
    }
}

/// Minecraft's `space` provider: characters that draw nothing.
#[derive(Clone, Debug, PartialEq)]
pub struct SpaceProvider {
    /// Each character and how far it moves the pen, in GUI pixels (may be fractional or
    /// negative).
    pub advances: Vec<(char, f32)>,
}

/// Reads the font `id` of `pack`, or `None` when the pack has no such file.
pub(crate) fn load(pack: &GuiPack, id: &ResourceId) -> Result<Option<PackFont>, PackError> {
    let Some(path) = pack.file(&id.namespace, &format!("font/{}.json", id.path)) else {
        return Ok(None);
    };
    let mut font = PackFont::default();
    read_providers(pack, &path, &mut vec![id.clone()], &mut font)?;
    Ok(Some(font))
}

/// Appends the providers of the font file at `path`. `stack` holds the fonts being read, to
/// stop reference cycles.
fn read_providers(
    pack: &GuiPack,
    path: &Path,
    stack: &mut Vec<ResourceId>,
    font: &mut PackFont,
) -> Result<(), PackError> {
    let text = std::fs::read_to_string(path).map_err(|source| PackError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let value: Value = serde_json::from_str(&text).map_err(|source| PackError::InvalidJson {
        path: path.to_path_buf(),
        source,
    })?;
    let providers = value
        .get("providers")
        .and_then(Value::as_array)
        .ok_or_else(|| PackError::InvalidFont {
            path: path.to_path_buf(),
            message: "no \"providers\" list".to_string(),
        })?;
    for (n, provider) in providers.iter().enumerate() {
        let warn = |font: &mut PackFont, message: String| {
            font.warnings.push(PackWarning::new(
                "font",
                format!("{} provider {n}: {message}", path.display()),
            ));
        };
        if !filter_passes(provider) {
            continue;
        }
        match provider.get("type").and_then(Value::as_str) {
            Some("bitmap") => match bitmap(pack, provider) {
                Ok(bitmap) => font.providers.push(FontProvider::Bitmap(bitmap)),
                Err(message) => warn(font, message),
            },
            Some("space") => match space(provider) {
                Ok(space) => font.providers.push(FontProvider::Space(space)),
                Err(message) => warn(font, message),
            },
            Some("reference") => {
                let Some(target) = provider.get("id").and_then(Value::as_str) else {
                    warn(font, "reference without an \"id\"".to_string());
                    continue;
                };
                let target = ResourceId::parse(target);
                if stack.contains(&target) {
                    warn(font, format!("reference cycle through {target}"));
                    continue;
                }
                if stack.len() >= MAX_REFERENCE_DEPTH {
                    warn(
                        font,
                        format!("references nested deeper than {MAX_REFERENCE_DEPTH}"),
                    );
                    continue;
                }
                let file = format!("font/{}.json", target.path);
                let Some(next) = pack.file(&target.namespace, &file) else {
                    warn(
                        font,
                        format!("references {target}, which the pack does not have"),
                    );
                    continue;
                };
                stack.push(target);
                let read = read_providers(pack, &next, stack, font);
                stack.pop();
                if let Err(err) = read {
                    warn(font, err.to_string());
                }
            }
            Some(other) => warn(font, format!("type \"{other}\" is not supported")),
            None => warn(font, "no \"type\"".to_string()),
        }
    }
    Ok(())
}

/// Whether a provider's `filter` lets it in with Minecraft's default options, all off.
fn filter_passes(provider: &Value) -> bool {
    provider
        .get("filter")
        .and_then(Value::as_object)
        .is_none_or(|options| options.values().all(|v| v.as_bool() == Some(false)))
}

/// A `bitmap` provider with its image, checked as Minecraft does.
fn bitmap(pack: &GuiPack, provider: &Value) -> Result<BitmapProvider, String> {
    let file = provider
        .get("file")
        .and_then(Value::as_str)
        .ok_or("bitmap without a \"file\"")?;
    let file = ResourceId::parse(file);
    let glyph_height = match provider.get("height") {
        None => 8,
        Some(h) => h
            .as_u64()
            .and_then(|h| u32::try_from(h).ok())
            .ok_or_else(|| format!("bitmap height {h} is not a whole number"))?,
    };
    let ascent = provider
        .get("ascent")
        .and_then(Value::as_i64)
        .and_then(|a| i32::try_from(a).ok())
        .ok_or("bitmap without a whole \"ascent\"")?;
    if i64::from(ascent) > i64::from(glyph_height) {
        return Err(format!(
            "ascent {ascent} is higher than height {glyph_height}"
        ));
    }
    let rows = provider
        .get("chars")
        .and_then(Value::as_array)
        .ok_or("bitmap without a \"chars\" list")?;
    let chars = rows
        .iter()
        .map(|row| row.as_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()
        .ok_or("bitmap chars that are not strings")?;
    let columns = chars.first().map_or(0, |row| row.chars().count());
    if columns == 0 {
        return Err("bitmap with no characters".to_string());
    }
    if chars.iter().any(|row| row.chars().count() != columns) {
        return Err("bitmap rows of different lengths".to_string());
    }
    let path = format!("textures/{}", file.path);
    let image = pack
        .file(&file.namespace, &path)
        .ok_or_else(|| format!("image {file} is not in the pack"))?;
    let image = decode_png(&image).map_err(|e| e.to_string())?;
    if image.width < columns as u32 || image.height < chars.len() as u32 {
        return Err(format!(
            "image {file} ({}x{}) is smaller than its {columns}x{} grid",
            image.width,
            image.height,
            chars.len()
        ));
    }
    Ok(BitmapProvider {
        file,
        width: image.width,
        height: image.height,
        rgba: image.rgba,
        chars,
        glyph_height,
        ascent,
    })
}

/// A `space` provider: one character per key.
fn space(provider: &Value) -> Result<SpaceProvider, String> {
    let advances = provider
        .get("advances")
        .and_then(Value::as_object)
        .ok_or("space without an \"advances\" map")?;
    let mut out = Vec::with_capacity(advances.len());
    for (key, advance) in advances {
        let mut chars = key.chars();
        let (Some(c), None) = (chars.next(), chars.next()) else {
            return Err(format!("space key {key:?} is not one character"));
        };
        let advance = advance
            .as_f64()
            .ok_or_else(|| format!("space advance of {key:?} is not a number"))?;
        out.push((c, advance as f32));
    }
    Ok(SpaceProvider { advances: out })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_pass_only_for_the_default_options() {
        let p = |json: &str| filter_passes(&serde_json::from_str(json).unwrap());
        assert!(p(r#"{"type": "bitmap"}"#));
        assert!(p(r#"{"filter": {"uniform": false}}"#));
        assert!(p(r#"{"filter": {}}"#));
        assert!(!p(r#"{"filter": {"uniform": true}}"#));
        assert!(!p(r#"{"filter": {"uniform": false, "jp": true}}"#));
    }

    #[test]
    fn space_providers_take_one_character_per_key() {
        let v = serde_json::from_str(r#"{"advances": {" ": 4, "\u200c": 0, "x": -1.5}}"#).unwrap();
        let advances = space(&v).unwrap().advances;
        assert_eq!(advances.len(), 3);
        assert!(advances.contains(&(' ', 4.0)));
        assert!(advances.contains(&('\u{200c}', 0.0)));
        assert!(advances.contains(&('x', -1.5)));
        let v = serde_json::from_str(r#"{"advances": {"ab": 4}}"#).unwrap();
        assert!(space(&v).is_err());
    }
}
