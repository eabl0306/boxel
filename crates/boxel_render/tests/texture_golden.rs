//! G2 of #48 (no GPU): the block texture array of the built-in content, procedural and from a
//! synthetic resource pack written here (a tinted grass, a log variant, a `water_flow` with an
//! `.mcmeta` animation, and the other layers missing so they fall back), with what the report
//! says about it. Frozen before the content moved out of the engine: the literal never changes,
//! only the setup lines below may.

#[path = "../../boxel_voxel/tests/golden/mod.rs"]
mod golden;

use boxel_gfx::RgbaImage;
use boxel_render::{
    BlockTextureData, PackSource, TextureSource, generate_block_textures, load_block_textures,
};
use boxel_voxel::BlockRegistry;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// The registry under test.
fn registry() -> BlockRegistry {
    boxel_demo::blocks::registry()
}

/// The texture source of the pack at `dir`.
fn pack_source(dir: &Path) -> TextureSource {
    TextureSource::Pack(PackSource::new(dir).with_recipes(boxel_demo::pack::recipes()))
}

/// A temporary resource pack of generated files (no third-party content), removed on drop.
struct TempPack(PathBuf);

impl TempPack {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("boxel_texture_golden_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp pack dir");
        std::fs::write(
            dir.join("pack.mcmeta"),
            r#"{"pack":{"pack_format":34,"description":"boxel golden pack"}}"#,
        )
        .expect("pack.mcmeta");
        Self(dir)
    }

    /// Writes `assets/minecraft/<rel>` (parents created).
    fn file(&self, rel: &str, contents: &str) {
        let path = self.0.join("assets/minecraft").join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(path, contents).expect("write");
    }

    /// Writes `textures/block/<name>.png`, every texel `pixel(x, y)`.
    fn png(&self, name: &str, width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) {
        let path = self
            .0
            .join("assets/minecraft/textures/block")
            .join(format!("{name}.png"));
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        let data = (0..width * height)
            .flat_map(|i| pixel(i % width, i / width))
            .collect();
        RgbaImage {
            width,
            height,
            data,
        }
        .save_png(&path)
        .expect("png");
    }
}

impl Drop for TempPack {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn data_lines(d: &mut String, what: &str, data: &BlockTextureData) {
    let _ = writeln!(
        d,
        "{what} size={} layers={} mips={} bytes={} data={:#018x} animated_flow={:?}",
        data.size,
        data.layers,
        data.mip_levels,
        data.data.len(),
        golden::fnv1a64(&data.data),
        data.animated_flow,
    );
    for a in data.animations.iter() {
        let indices = a.timeline().indices();
        let frames: Vec<String> = indices
            .iter()
            .map(|&i| format!("{i}:{:#018x}", golden::fnv1a64(a.frame(i))))
            .collect();
        let keys: Vec<String> = (0..48).map(|t| format!("{:?}", a.key(t))).collect();
        let _ = writeln!(
            d,
            "  animation {a:?} frames=[{}] keys={:#018x}",
            frames.join(" "),
            golden::fnv1a64(keys.join(";").as_bytes()),
        );
    }
}

fn dump(r: &BlockRegistry) -> String {
    let mut d = String::new();
    data_lines(&mut d, "procedural", &generate_block_textures(r.textures()));

    let pack = TempPack::new();
    // Grass: a grayscale top (tinted by the plains grass color) and a side with its overlay.
    pack.png("grass_block_top", 16, 16, |x, y| {
        let v = 120 + ((x * 7 + y * 3) % 64) as u8;
        [v, v, v, 255]
    });
    pack.png("grass_block_side", 16, 16, |x, y| {
        [100 + x as u8, 60 + y as u8, 30, 255]
    });
    pack.png("grass_block_side_overlay", 16, 16, |x, y| {
        if y < 3 {
            [200, 200, 200, 255]
        } else {
            [0, 0, (x * 9) as u8, 0]
        }
    });
    // The log through its blockstate variant and a cube_column model.
    pack.file(
        "blockstates/oak_log.json",
        r#"{"variants":{"axis=x":{"model":"block/oak_log_horizontal","x":90,"y":90},
            "axis=y":{"model":"block/oak_log"},"axis=z":{"model":"block/oak_log_horizontal","x":90}}}"#,
    );
    pack.file(
        "models/block/oak_log.json",
        r#"{"parent":"block/cube_column","textures":{"end":"block/golden_end","side":"block/golden_bark"}}"#,
    );
    pack.png("golden_end", 16, 16, |x, y| {
        [150, 120 + (x ^ y) as u8, 80, 255]
    });
    pack.png("golden_bark", 16, 16, |x, y| {
        [90 + (x * 3) as u8, 70, 40 + (y * 2) as u8, 255]
    });
    // The water flow sprite: twice the block size, two frames, animated.
    pack.png("water_flow", 32, 64, |x, y| {
        let frame = (y / 32) as u8;
        [180 + frame * 30, (x * 4) as u8, (y * 3) as u8, 200]
    });
    pack.file(
        "textures/block/water_flow.png.mcmeta",
        r#"{"animation":{"frametime":3,"interpolate":true}}"#,
    );
    // Every other layer is missing from the pack: it keeps its procedural pixels with a warning.
    let (data, report) = load_block_textures(r, &pack_source(&pack.0)).expect("pack");
    data_lines(&mut d, "pack", &data);
    let _ = writeln!(
        d,
        "report size={} layers={} from_pack={} animated={} animation_bytes={} pack_warnings={}",
        report.size,
        report.layers,
        report.from_pack,
        report.animated,
        report.animation_bytes,
        report.pack_warnings,
    );
    for w in &report.warnings {
        let w = w.replace(&pack.0.display().to_string(), "<pack>");
        let _ = writeln!(d, "  warning {w}");
    }
    d
}

#[test]
fn the_textures_of_the_content_are_frozen() {
    golden::check("textures", &dump(&registry()), 0x61ac_fc98_ad28_e261);
}
