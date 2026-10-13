//! `pack.mcmeta` parsing and pack-format ranges (formats 64–120 and overlays).

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use crate::error::{PackError, PackWarning};

/// Parsed pack metadata from `pack.mcmeta`.
#[derive(Debug, Clone)]
pub struct PackMeta {
    /// Pack description (stringified if it was a text component).
    pub description: String,
    /// Declared `pack_format`, when present.
    pub pack_format: Option<u32>,
    /// Inclusive format range this pack claims to support.
    pub format_range: FormatRange,
    /// Overlay directories, in declaration order.
    pub overlays: Vec<OverlayEntry>,
    /// Absolute path to the pack root.
    pub root: PathBuf,
}

/// Inclusive major-format range (`min..=max`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatRange {
    /// Lowest supported major format.
    pub min: u32,
    /// Highest supported major format.
    pub max: u32,
}

impl FormatRange {
    /// Whether `format` lies in this inclusive range.
    pub fn contains(self, format: u32) -> bool {
        format >= self.min && format <= self.max
    }
}

/// One overlay directory applied for a format range.
#[derive(Debug, Clone)]
pub struct OverlayEntry {
    /// Directory name relative to the pack root (`format_84-88`).
    pub directory: String,
    /// Formats for which this overlay applies.
    pub formats: FormatRange,
}

/// Effective format used to pick overlays when opening a pack.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenOptions {
    /// Major pack format to load. `None` picks `pack_format`, else `format_range.max`.
    pub format: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct McMetaFile {
    pack: PackSection,
    #[serde(default)]
    overlays: Option<OverlaysSection>,
}

#[derive(Debug, Deserialize)]
struct PackSection {
    #[serde(default)]
    description: Value,
    #[serde(default)]
    pack_format: Option<u32>,
    #[serde(default)]
    supported_formats: Option<FormatSpec>,
    #[serde(default)]
    min_format: Option<FormatSpec>,
    #[serde(default)]
    max_format: Option<FormatSpec>,
}

#[derive(Debug, Deserialize)]
struct OverlaysSection {
    #[serde(default)]
    entries: Vec<OverlayRaw>,
}

#[derive(Debug, Deserialize)]
struct OverlayRaw {
    directory: String,
    #[serde(default)]
    formats: Option<FormatSpec>,
    #[serde(default)]
    min_format: Option<FormatSpec>,
    #[serde(default)]
    max_format: Option<FormatSpec>,
}

/// Flexible format field: number, `[min,max]`, `[major]`, or `{min_inclusive,max_inclusive}`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum FormatSpec {
    Single(u32),
    Seq(Vec<u32>),
    Object {
        #[serde(default)]
        min_inclusive: Option<u32>,
        #[serde(default)]
        max_inclusive: Option<u32>,
    },
}

impl FormatSpec {
    fn as_major(&self) -> Option<u32> {
        match self {
            Self::Single(n) => Some(*n),
            Self::Seq(v) => v.first().copied(),
            Self::Object {
                min_inclusive,
                max_inclusive,
            } => min_inclusive.or(*max_inclusive),
        }
    }

    fn as_range(&self) -> Option<FormatRange> {
        match self {
            Self::Single(n) => Some(FormatRange { min: *n, max: *n }),
            Self::Seq(v) if v.is_empty() => None,
            Self::Seq(v) if v.len() == 1 => Some(FormatRange {
                min: v[0],
                max: v[0],
            }),
            Self::Seq(v) => {
                let a = v[0];
                let b = v.get(1).copied().unwrap_or(a);
                Some(FormatRange {
                    min: a.min(b),
                    max: a.max(b),
                })
            }
            Self::Object {
                min_inclusive,
                max_inclusive,
            } => {
                let min = min_inclusive.or(*max_inclusive)?;
                let max = max_inclusive.or(*min_inclusive)?;
                Some(FormatRange { min, max })
            }
        }
    }
}

fn description_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Reads and parses `pack.mcmeta` from `root`.
pub fn load_meta(root: &Path) -> Result<(PackMeta, Vec<PackWarning>), PackError> {
    let mut warnings = Vec::new();
    let path = root.join("pack.mcmeta");
    if !path.is_file() {
        return Err(PackError::MissingMeta(root.to_path_buf()));
    }
    let text = std::fs::read_to_string(&path).map_err(|source| PackError::Io {
        path: path.clone(),
        source,
    })?;
    let file: McMetaFile =
        serde_json::from_str(&text).map_err(|source| PackError::InvalidJson {
            path: path.clone(),
            source,
        })?;

    let pack_format = file.pack.pack_format;
    let format_range = resolve_format_range(&file.pack, &mut warnings);
    let overlays = file
        .overlays
        .map(|o| o.entries)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|raw| match overlay_from_raw(raw) {
            Ok(e) => Some(e),
            Err(msg) => {
                warnings.push(PackWarning::new("overlay", msg));
                None
            }
        })
        .collect();

    Ok((
        PackMeta {
            description: description_to_string(&file.pack.description),
            pack_format,
            format_range,
            overlays,
            root: root.to_path_buf(),
        },
        warnings,
    ))
}

fn resolve_format_range(pack: &PackSection, warnings: &mut Vec<PackWarning>) -> FormatRange {
    if let Some(spec) = &pack.supported_formats
        && let Some(r) = spec.as_range()
    {
        return r;
    }
    let min = pack
        .min_format
        .as_ref()
        .and_then(FormatSpec::as_major)
        .or(pack.pack_format);
    let max = pack
        .max_format
        .as_ref()
        .and_then(FormatSpec::as_major)
        .or(pack.pack_format);
    match (min, max) {
        (Some(min), Some(max)) => FormatRange {
            min: min.min(max),
            max: min.max(max),
        },
        (Some(n), None) | (None, Some(n)) => FormatRange { min: n, max: n },
        (None, None) => {
            warnings.push(PackWarning::new(
                "format",
                "pack.mcmeta has no pack_format / min_format / max_format; assuming 64..=120",
            ));
            FormatRange { min: 64, max: 120 }
        }
    }
}

/// Whether `directory` is a name Minecraft takes for an overlay (`[-_a-zA-Z0-9.]+`), minus `.`
/// and `..`: one plain directory inside the pack, never a path out of it.
fn valid_overlay_directory(directory: &str) -> bool {
    !matches!(directory, "" | "." | "..")
        && directory
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn overlay_from_raw(raw: OverlayRaw) -> Result<OverlayEntry, String> {
    if !valid_overlay_directory(&raw.directory) {
        return Err(format!(
            "overlay {:?} is not a valid overlay directory (one name of -_a-zA-Z0-9.); skipped",
            raw.directory
        ));
    }
    let formats = if let Some(spec) = raw.formats {
        spec.as_range()
            .ok_or_else(|| format!("overlay '{}': empty formats", raw.directory))?
    } else {
        let min = raw
            .min_format
            .as_ref()
            .and_then(FormatSpec::as_major)
            .ok_or_else(|| format!("overlay '{}': missing min_format", raw.directory))?;
        let max = raw
            .max_format
            .as_ref()
            .and_then(FormatSpec::as_major)
            .unwrap_or(min);
        FormatRange {
            min: min.min(max),
            max: min.max(max),
        }
    };
    Ok(OverlayEntry {
        directory: raw.directory,
        formats,
    })
}

/// Chooses the major format reported for this open (overlays may still use range intersection).
pub fn effective_format(meta: &PackMeta, options: OpenOptions) -> u32 {
    if let Some(f) = options.format {
        return f;
    }
    if let Some(f) = meta.pack_format {
        return f;
    }
    meta.format_range.max
}

/// Overlay directories that apply, in declaration order (later wins over earlier).
///
/// When `forced` is `Some(format)`, only overlays whose range contains that format apply.
/// When `None`, every overlay whose range intersects the pack's declared [`PackMeta::format_range`]
/// applies (useful when the pack has no single `pack_format` and several format-specific overlays).
pub fn applicable_overlays(meta: &PackMeta, forced: Option<u32>) -> Vec<&OverlayEntry> {
    meta.overlays
        .iter()
        .filter(|o| match forced {
            Some(f) => o.formats.contains(f),
            None => ranges_intersect(o.formats, meta.format_range),
        })
        .collect()
}

fn ranges_intersect(a: FormatRange, b: FormatRange) -> bool {
    a.min <= b.max && b.min <= a.max
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_spec_array_and_object() {
        let seq: FormatSpec = serde_json::from_str("[64, 120]").unwrap();
        assert_eq!(seq.as_range(), Some(FormatRange { min: 64, max: 120 }));
        let single_arr: FormatSpec = serde_json::from_str("[64]").unwrap();
        assert_eq!(single_arr.as_major(), Some(64));
        let obj: FormatSpec =
            serde_json::from_str(r#"{"min_inclusive": 84, "max_inclusive": 88}"#).unwrap();
        assert_eq!(obj.as_range(), Some(FormatRange { min: 84, max: 88 }));
    }

    #[test]
    fn overlay_directories_are_one_plain_name() {
        let overlay = |directory: &str| {
            overlay_from_raw(OverlayRaw {
                directory: directory.to_string(),
                formats: Some(FormatSpec::Single(64)),
                min_format: None,
                max_format: None,
            })
        };
        for good in ["overlay_64", "Late-2.x", "a.b", "..."] {
            assert!(overlay(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            ".",
            "..",
            "/home/user/other",
            "../outside",
            "a/b",
            "a\\b",
            "C:",
            "ü",
        ] {
            let err = overlay(bad).expect_err(bad);
            assert!(
                err.contains("not a valid overlay directory"),
                "{bad}: {err}"
            );
        }
    }
}
