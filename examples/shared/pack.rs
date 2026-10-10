//! `--pack DIR` of the examples (`voxel_world`, `snapshot`, `stress`): block textures from a
//! Minecraft Java resource pack instead of the procedural ones, read with the demo's recipes.
#![allow(dead_code, reason = "each example uses its own part")]

use boxel::render::{PackSource, TextureReport, TextureSource};
use std::path::PathBuf;

/// Help line of `--pack`.
pub const USAGE: &str =
    "  --pack DIR           block textures from a Minecraft Java resource pack (the directory
                       with pack.mcmeta; default: procedural textures)";

/// `--pack DIR`: the directory must hold a `pack.mcmeta`. The demo layers read it through
/// `boxel_demo::pack::recipes`.
pub fn parse(flag: &str, value: &str) -> Result<TextureSource, String> {
    let path = PathBuf::from(value);
    if !path.join("pack.mcmeta").is_file() {
        return Err(format!("{flag}: {value:?} has no pack.mcmeta"));
    }
    Ok(TextureSource::Pack(
        PackSource::new(path).with_recipes(boxel_demo::pack::recipes()),
    ))
}

/// One line about the block textures: `procedural 16px` or `BlockPixel 16px, 17/18 layers from
/// the pack, 4 animated (52 KB), 3 pack warnings, loaded in 41.2 ms`.
pub fn summary(report: &TextureReport) -> String {
    if report.from_pack == 0 && report.pack_warnings == 0 && report.warnings.is_empty() {
        return format!("{} {}px", report.source, report.size);
    }
    format!(
        "{} {}px, {}/{} layers from the pack, {} animated ({} KB), {} pack warnings, {} mapping \
         warnings, loaded in {:.1} ms",
        report.source,
        report.size,
        report.from_pack,
        report.layers,
        report.animated,
        report.animation_bytes.div_ceil(1024),
        report.pack_warnings,
        report.warnings.len(),
        report.load_time.as_secs_f64() * 1000.0
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_needs_a_pack_mcmeta() {
        assert!(parse("--pack", "/nonexistent/boxel/pack").is_err());
        let dir = std::env::temp_dir().join(format!("boxel_example_pack_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        assert!(parse("--pack", dir.to_str().expect("utf-8")).is_err());
        std::fs::write(dir.join("pack.mcmeta"), "{}").expect("pack.mcmeta");
        assert_eq!(
            parse("--pack", dir.to_str().expect("utf-8")),
            Ok(TextureSource::Pack(
                PackSource::new(dir.clone()).with_recipes(boxel_demo::pack::recipes())
            ))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
