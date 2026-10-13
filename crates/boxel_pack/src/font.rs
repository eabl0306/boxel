//! A pack's fonts (`assets/<ns>/font/<name>.json`): bitmap and space providers, with
//! `reference` providers followed inside the pack.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use serde_json::Value;

use crate::error::{PackError, PackWarning};
use crate::gui::GuiPack;
use crate::id::ResourceId;
use crate::texture::decode_png;

/// How deep `reference` providers may nest (Minecraft's own fonts use one level).
const MAX_REFERENCE_DEPTH: usize = 8;

/// The largest glyph height, depth below the baseline and drawn width in GUI pixels. A
/// provider past it is skipped: no real font comes near, and the UI's integer math stays far
/// from overflowing.
const MAX_GLYPH_SPAN: u32 = 32768;

/// The widest and tallest cell in texels. A provider past it is skipped: a 512x pack's cells are
/// 256, and a glyph cut from a taller cell drawn a few GUI pixels tall would be padded (to whole
/// GUI pixels, as `boxel_ui` draws it) past any UI atlas.
const MAX_CELL_SIDE: u32 = 4096;

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
    /// The image, straight-alpha sRGB RGBA8, row by row; shared by the providers of a font
    /// that name the same image.
    pub rgba: Arc<[u8]>,
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
    let mut load = Load {
        pack,
        stack: vec![id.clone()],
        read: HashSet::new(),
        images: HashMap::new(),
        font: PackFont::default(),
    };
    load.read_providers(&path)?;
    Ok(Some(load.font))
}

/// The state of one [`load`]. Each font file is read once and each image decoded once, so a
/// pack that names the same font or image many times costs no more than naming it once.
struct Load<'a> {
    pack: &'a GuiPack,
    /// The fonts being read, outermost first, to stop reference cycles.
    stack: Vec<ResourceId>,
    /// Fonts read to the end. A reference to one adds nothing: its providers are already
    /// earlier in the list, where they win.
    read: HashSet<ResourceId>,
    /// Images decoded so far (or why they could not be), by texture id.
    images: HashMap<ResourceId, Result<Image, String>>,
    font: PackFont,
}

/// A decoded font image: width, height and RGBA8 texels.
type Image = (u32, u32, Arc<[u8]>);

impl Load<'_> {
    /// Appends the providers of the font file at `path`, the font on top of `stack`.
    fn read_providers(&mut self, path: &Path) -> Result<(), PackError> {
        let text = std::fs::read_to_string(path).map_err(|source| PackError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let value: Value =
            serde_json::from_str(&text).map_err(|source| PackError::InvalidJson {
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
                Some("bitmap") => match self.bitmap(provider) {
                    Ok(bitmap) => self.font.providers.push(FontProvider::Bitmap(bitmap)),
                    Err(message) => warn(&mut self.font, message),
                },
                Some("space") => match space(provider) {
                    Ok(space) => self.font.providers.push(FontProvider::Space(space)),
                    Err(message) => warn(&mut self.font, message),
                },
                Some("reference") => {
                    let Some(target) = provider.get("id").and_then(Value::as_str) else {
                        warn(&mut self.font, "reference without an \"id\"".to_string());
                        continue;
                    };
                    let target = ResourceId::parse(target);
                    if self.stack.contains(&target) {
                        warn(&mut self.font, format!("reference cycle through {target}"));
                        continue;
                    }
                    if self.read.contains(&target) {
                        continue;
                    }
                    if self.stack.len() >= MAX_REFERENCE_DEPTH {
                        warn(
                            &mut self.font,
                            format!("references nested deeper than {MAX_REFERENCE_DEPTH}"),
                        );
                        continue;
                    }
                    let file = format!("font/{}.json", target.path);
                    let Some(next) = self.pack.file(&target.namespace, &file) else {
                        warn(
                            &mut self.font,
                            format!("references {target}, which the pack does not have"),
                        );
                        continue;
                    };
                    self.stack.push(target.clone());
                    let read = self.read_providers(&next);
                    self.stack.pop();
                    self.read.insert(target);
                    if let Err(err) = read {
                        warn(&mut self.font, err.to_string());
                    }
                }
                Some(other) => warn(&mut self.font, format!("type \"{other}\" is not supported")),
                None => warn(&mut self.font, "no \"type\"".to_string()),
            }
        }
        Ok(())
    }

    /// The image `file` (a texture id), decoded the first time it is named.
    fn image(&mut self, file: &ResourceId) -> Result<Image, String> {
        let pack = self.pack;
        self.images
            .entry(file.clone())
            .or_insert_with(|| {
                let path = format!("textures/{}", file.path);
                let path = pack
                    .file(&file.namespace, &path)
                    .ok_or_else(|| format!("image {file} is not in the pack"))?;
                let image = decode_png(&path).map_err(|e| e.to_string())?;
                Ok((image.width, image.height, image.rgba.into()))
            })
            .clone()
    }

    /// A `bitmap` provider with its image, checked as Minecraft does.
    fn bitmap(&mut self, provider: &Value) -> Result<BitmapProvider, String> {
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
        if glyph_height > MAX_GLYPH_SPAN {
            return Err(format!(
                "bitmap height {glyph_height} is past {MAX_GLYPH_SPAN} GUI pixels"
            ));
        }
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
        if ascent < -(MAX_GLYPH_SPAN as i32) {
            return Err(format!(
                "ascent {ascent} is more than {MAX_GLYPH_SPAN} GUI pixels below the baseline"
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
        let (width, height, rgba) = self.image(&file)?;
        if width < columns as u32 || height < chars.len() as u32 {
            return Err(format!(
                "image {file} ({width}x{height}) is smaller than its {columns}x{} grid",
                chars.len()
            ));
        }
        let bitmap = BitmapProvider {
            file,
            width,
            height,
            rgba,
            chars,
            glyph_height,
            ascent,
        };
        let [cell_w, cell_h] = bitmap.cell();
        if cell_w > MAX_CELL_SIDE || cell_h > MAX_CELL_SIDE {
            return Err(format!(
                "cells of {} are {cell_w}x{cell_h} texels, past {MAX_CELL_SIDE}",
                bitmap.file
            ));
        }
        // A cell drawn `glyph_height` GUI pixels tall is this wide.
        if u64::from(cell_w) * u64::from(glyph_height)
            > u64::from(MAX_GLYPH_SPAN) * u64::from(cell_h)
        {
            return Err(format!(
                "glyphs of {} are drawn wider than {MAX_GLYPH_SPAN} GUI pixels",
                bitmap.file
            ));
        }
        Ok(bitmap)
    }
}

/// Whether a provider's `filter` lets it in with Minecraft's default options, all off.
fn filter_passes(provider: &Value) -> bool {
    provider
        .get("filter")
        .and_then(Value::as_object)
        .is_none_or(|options| options.values().all(|v| v.as_bool() == Some(false)))
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
        if advance.abs() > f64::from(MAX_GLYPH_SPAN) {
            return Err(format!(
                "space advance {advance} of {key:?} is past {MAX_GLYPH_SPAN} GUI pixels"
            ));
        }
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
        // Past 32768 GUI pixels either way: skipped, as a bitmap that large.
        let v = serde_json::from_str(r#"{"advances": {"x": 32768, "y": -32768}}"#).unwrap();
        assert!(space(&v).is_ok());
        for json in [
            r#"{"advances": {"x": 32769}}"#,
            r#"{"advances": {"x": -1e30}}"#,
        ] {
            assert!(
                space(&serde_json::from_str(json).unwrap()).is_err(),
                "{json}"
            );
        }
    }
}
