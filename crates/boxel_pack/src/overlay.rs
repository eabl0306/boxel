//! Overlay-aware virtual file index over a pack root.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::error::PackWarning;
use crate::meta::{OverlayEntry, PackMeta};

/// Relative pack path using `/` separators (`assets/minecraft/textures/block/stone.png`).
pub type RelPath = String;

/// Maps relative asset paths to the concrete file that wins after overlays.
#[derive(Debug, Default)]
pub struct FileIndex {
    /// Winning file for each relative path.
    pub files: BTreeMap<RelPath, PathBuf>,
    /// Relative directories under the pack root that were ignored with a warning.
    pub ignored: BTreeSet<String>,
}

/// Top-level and under-`assets/<ns>/` names we load.
const LOADED_ASSET_DIRS: &[&str] = &["textures", "models", "blockstates"];

/// Known names we skip with a warning (not loaded by this crate).
const IGNORED_NAMES: &[&str] = &[
    "sounds",
    "lang",
    "font",
    "fonts",
    "shaders",
    "texts",
    "optifine",
    "emissive",
    "colormap",
    "particles",
    "atlases",
    "equipment",
    "items",
    "item",
    "gui",
    "mob_effect",
    "painting",
    "environment",
    "misc",
    "trims",
    "entity", // textures/entity — not block textures
];

/// Builds the overlay-resolved file index for `meta` at `format`.
pub fn build_index(
    meta: &PackMeta,
    format: u32,
    overlays: &[&OverlayEntry],
    warnings: &mut Vec<PackWarning>,
) -> Result<FileIndex, std::io::Error> {
    let mut index = FileIndex::default();
    index_tree(&meta.root.join("assets"), "assets", &mut index, warnings)?;

    for overlay in overlays {
        let overlay_root = meta.root.join(&overlay.directory);
        if !overlay_root.is_dir() {
            warnings.push(PackWarning::new(
                "overlay",
                format!(
                    "overlay directory '{}' not found (format {format})",
                    overlay.directory
                ),
            ));
            continue;
        }
        index_tree(&overlay_root.join("assets"), "assets", &mut index, warnings)?;
    }

    // Top-level unknown folders (not pack.mcmeta / pack.png / assets / overlay dirs).
    let overlay_names: BTreeSet<&str> =
        meta.overlays.iter().map(|o| o.directory.as_str()).collect();
    if let Ok(entries) = std::fs::read_dir(&meta.root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.')
                || name == "pack.mcmeta"
                || name == "pack.png"
                || name == "assets"
                || overlay_names.contains(name.as_str())
                || name == "CHANGELOG.md"
                || name == "Copyrights.txt"
                || name == "Latest Version.txt"
                || name == "README.md"
                || name == "readme.txt"
            {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                if is_ignored_name(&name) {
                    warn_ignored(&mut index, warnings, &name, "ignored top-level directory");
                } else {
                    warn_ignored(&mut index, warnings, &name, "unknown top-level directory");
                }
            }
        }
    }

    let _ = format;
    Ok(index)
}

fn index_tree(
    dir: &Path,
    rel: &str,
    index: &mut FileIndex,
    warnings: &mut Vec<PackWarning>,
) -> Result<(), std::io::Error> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let child_rel = if rel.is_empty() {
            name.to_string()
        } else {
            format!("{rel}/{name}")
        };
        let path = entry.path();
        if path.is_dir() {
            if should_skip_dir(rel, &name, index, warnings) {
                continue;
            }
            index_tree(&path, &child_rel, index, warnings)?;
        } else {
            index.files.insert(child_rel, path);
        }
    }
    Ok(())
}

fn should_skip_dir(
    parent_rel: &str,
    name: &str,
    index: &mut FileIndex,
    warnings: &mut Vec<PackWarning>,
) -> bool {
    // assets/<namespace>/<kind>/...
    let parts: Vec<&str> = parent_rel.split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        ["assets", _ns] => {
            if LOADED_ASSET_DIRS.contains(&name) {
                false
            } else if is_ignored_name(name) {
                warn_ignored(
                    index,
                    warnings,
                    &format!("{parent_rel}/{name}"),
                    "ignored asset directory",
                );
                true
            } else {
                warn_ignored(
                    index,
                    warnings,
                    &format!("{parent_rel}/{name}"),
                    "unknown asset directory",
                );
                true
            }
        }
        ["assets", _ns, "textures"] => {
            // Only load textures/block/**; warn on other texture folders.
            if name == "block" {
                false
            } else {
                warn_ignored(
                    index,
                    warnings,
                    &format!("{parent_rel}/{name}"),
                    "skipped non-block texture directory",
                );
                true
            }
        }
        ["assets", _ns, "models"] => {
            // Only models/block/**
            if name == "block" {
                false
            } else {
                warn_ignored(
                    index,
                    warnings,
                    &format!("{parent_rel}/{name}"),
                    "skipped non-block model directory",
                );
                true
            }
        }
        _ => false,
    }
}

fn is_ignored_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    IGNORED_NAMES.iter().any(|n| *n == lower) || lower.contains("emissive") || lower == "optifine"
}

fn warn_ignored(index: &mut FileIndex, warnings: &mut Vec<PackWarning>, key: &str, kind_msg: &str) {
    if index.ignored.insert(key.to_string()) {
        warnings.push(PackWarning::new("ignored", format!("{kind_msg}: {key}")));
    }
}
