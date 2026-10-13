//! `GuiPack` against synthetic packs written by each test (no pack content in the repo).

use std::path::{Path, PathBuf};

use boxel_pack::{
    BitmapProvider, FontProvider, GuiPack, GuiScaling, GuiTexture, OpenOptions, PackError,
    ResourceId,
};

/// A pack directory under Cargo's temporary directory for tests, emptied first.
struct TempPack {
    root: PathBuf,
}

impl TempPack {
    fn new(name: &str, mcmeta: &str) -> Self {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("gui_pack")
            .join(name);
        if root.exists() {
            std::fs::remove_dir_all(&root).expect("clear the old pack");
        }
        std::fs::create_dir_all(&root).expect("pack dir");
        let pack = Self { root };
        pack.text("pack.mcmeta", mcmeta);
        pack
    }

    fn simple(name: &str) -> Self {
        Self::new(
            name,
            r#"{"pack": {"pack_format": 64, "description": "test"}}"#,
        )
    }

    fn text(&self, rel: &str, text: &str) {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        std::fs::write(path, text).expect("write");
    }

    /// An RGBA PNG whose texel (x, y) is `texel(x, y)`.
    fn png(&self, rel: &str, w: u32, h: u32, texel: impl Fn(u32, u32) -> [u8; 4]) {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
        let file = std::fs::File::create(path).expect("png file");
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&texel(x, y));
            }
        }
        writer.write_image_data(&data).expect("png data");
    }

    fn open(&self) -> GuiPack {
        GuiPack::open(&self.root).expect("open the pack")
    }
}

impl Drop for TempPack {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const SPRITES: &str = "assets/minecraft/textures/gui/sprites";

fn id(s: &str) -> ResourceId {
    ResourceId::parse(s)
}

fn sprite(pack: &GuiPack, name: &str) -> GuiTexture {
    pack.sprite(&id(name))
        .expect("sprite reads")
        .expect("sprite exists")
}

#[test]
fn sprites_carry_their_gui_scaling() {
    let pack = TempPack::simple("scaling");
    // A 2x sprite: 8 by 6 texels, 4 by 3 GUI pixels, its texel (x, y) = (x, y, 7, 255).
    pack.png(&format!("{SPRITES}/widget/button.png"), 8, 6, |x, y| {
        [x as u8, y as u8, 7, 255]
    });
    pack.text(
        &format!("{SPRITES}/widget/button.png.mcmeta"),
        r#"{"gui": {"scaling": {"type": "nine_slice", "width": 4, "height": 3, "border": 1}}}"#,
    );
    pack.png(&format!("{SPRITES}/widget/sides.png"), 20, 10, |_, _| {
        [9; 4]
    });
    pack.text(
        &format!("{SPRITES}/widget/sides.png.mcmeta"),
        r#"{"gui": {"scaling": {"type": "nine_slice", "width": 20, "height": 10,
            "border": {"left": 2, "top": 3, "right": 4, "bottom": 5}, "stretch_inner": true}}}"#,
    );
    pack.png(&format!("{SPRITES}/bg.png"), 16, 16, |_, _| [1; 4]);
    pack.text(
        &format!("{SPRITES}/bg.png.mcmeta"),
        r#"{"gui": {"scaling": {"type": "tile", "width": 16, "height": 16}}}"#,
    );
    pack.png(&format!("{SPRITES}/stretched.png"), 3, 3, |_, _| [2; 4]);
    pack.text(
        &format!("{SPRITES}/stretched.png.mcmeta"),
        r#"{"gui": {"scaling": {"type": "stretch"}}}"#,
    );
    pack.png(&format!("{SPRITES}/plain.png"), 2, 1, |_, _| [3; 4]);
    let pack_reader = pack.open();

    let button = sprite(&pack_reader, "minecraft:widget/button");
    assert_eq!((button.width, button.height), (8, 6));
    assert_eq!(
        button.scaling,
        GuiScaling::NineSlice {
            width: 4,
            height: 3,
            border: [1; 4],
            stretch_inner: false,
        }
    );
    let at = |x: u32, y: u32| ((y * button.width + x) * 4) as usize;
    assert_eq!(&button.rgba[at(5, 3)..at(5, 3) + 4], &[5, 3, 7, 255]);

    assert_eq!(
        sprite(&pack_reader, "widget/sides").scaling,
        GuiScaling::NineSlice {
            width: 20,
            height: 10,
            border: [2, 3, 4, 5],
            stretch_inner: true,
        }
    );
    assert_eq!(
        sprite(&pack_reader, "bg").scaling,
        GuiScaling::Tile {
            width: 16,
            height: 16
        }
    );
    assert_eq!(
        sprite(&pack_reader, "stretched").scaling,
        GuiScaling::Stretch
    );
    let plain = sprite(&pack_reader, "plain");
    assert_eq!(
        (plain.width, plain.height, plain.scaling, plain.rgba),
        (2, 1, GuiScaling::Stretch, vec![3; 8])
    );
    // Any texture by its id under textures/, with the same metadata rules.
    let bg = pack_reader
        .texture(&id("gui/sprites/bg"))
        .expect("reads")
        .expect("exists");
    assert_eq!(bg, sprite(&pack_reader, "bg"));
}

#[test]
fn what_the_pack_lacks_is_none_and_names_stay_inside_it() {
    let pack = TempPack::simple("missing");
    pack.png("assets/minecraft/textures/secret.png", 1, 1, |_, _| [1; 4]);
    pack.png("outside.png", 1, 1, |_, _| [1; 4]);
    let reader = pack.open();
    assert_eq!(reader.sprite(&id("widget/button")).expect("reads"), None);
    assert_eq!(reader.font(&id("default")).expect("reads"), None);
    assert!(reader.texture(&id("secret")).expect("reads").is_some());
    // `..` would leave textures/ (and the pack): Minecraft rejects such names, so do we.
    for name in ["gui/../secret", "../../../outside", "Secret", "gui//secret"] {
        assert_eq!(reader.texture(&id(name)).expect("reads"), None, "{name}");
    }
    assert_eq!(reader.texture(&id("..:secret")).expect("reads"), None);
}

#[test]
fn bad_gui_metadata_is_an_error() {
    let pack = TempPack::simple("bad_meta");
    let cases = [
        (
            "no_center",
            r#"{"gui": {"scaling": {"type": "nine_slice", "width": 8, "height": 8, "border": 4}}}"#,
        ),
        (
            "no_vertical_center",
            r#"{"gui": {"scaling": {"type": "nine_slice", "width": 8, "height": 8,
                "border": {"left": 1, "top": 6, "right": 1, "bottom": 2}}}}"#,
        ),
        (
            "empty_tile",
            r#"{"gui": {"scaling": {"type": "tile", "width": 0, "height": 8}}}"#,
        ),
        (
            "unknown_type",
            r#"{"gui": {"scaling": {"type": "spiral"}}}"#,
        ),
        (
            "no_border",
            r#"{"gui": {"scaling": {"type": "nine_slice", "width": 8, "height": 8}}}"#,
        ),
        // Minecraft reads these sizes as ints: anything past i32::MAX is rejected.
        (
            "huge_tile",
            r#"{"gui": {"scaling": {"type": "tile", "width": 3000000000, "height": 16}}}"#,
        ),
        (
            "huge_nine_slice",
            r#"{"gui": {"scaling": {"type": "nine_slice", "width": 2147483648, "height": 8,
                "border": 1}}}"#,
        ),
    ];
    for (name, meta) in cases {
        pack.png(&format!("{SPRITES}/{name}.png"), 8, 8, |_, _| [0; 4]);
        pack.text(&format!("{SPRITES}/{name}.png.mcmeta"), meta);
    }
    pack.png(&format!("{SPRITES}/broken.png"), 8, 8, |_, _| [0; 4]);
    pack.text(&format!("{SPRITES}/broken.png.mcmeta"), "{ not json");
    let reader = pack.open();
    for (name, _) in cases {
        let err = reader.sprite(&id(name)).expect_err(name);
        assert!(
            matches!(err, PackError::InvalidGuiMeta { .. }),
            "{name}: {err}"
        );
    }
    let err = reader.sprite(&id("broken")).expect_err("broken");
    assert!(matches!(err, PackError::InvalidJson { .. }), "{err}");
}

#[test]
fn overlays_win_and_animated_sprites_show_their_first_frame() {
    let pack = TempPack::new(
        "overlays",
        r#"{"pack": {"pack_format": 64, "description": "test"},
            "overlays": {"entries": [
                {"directory": "late", "formats": [60, 70]},
                {"directory": "never", "formats": [90, 95]}
            ]}}"#,
    );
    pack.png(&format!("{SPRITES}/icon.png"), 2, 2, |_, _| {
        [255, 0, 0, 255]
    });
    pack.png(&format!("late/{SPRITES}/icon.png"), 2, 2, |_, _| {
        [0, 255, 0, 255]
    });
    pack.text(
        &format!("late/{SPRITES}/icon.png.mcmeta"),
        r#"{"gui": {"scaling": {"type": "tile", "width": 2, "height": 2}}}"#,
    );
    pack.png(&format!("never/{SPRITES}/icon.png"), 2, 2, |_, _| {
        [0, 0, 255, 255]
    });
    // A strip of three 2x2 frames played 2, 0, 1: the first frame shown is strip frame 2.
    pack.png(&format!("{SPRITES}/spinner.png"), 2, 6, |_, y| {
        [y as u8 / 2, 0, 0, 255]
    });
    pack.text(
        &format!("{SPRITES}/spinner.png.mcmeta"),
        r#"{"animation": {"frames": [2, 0, 1]},
            "gui": {"scaling": {"type": "stretch"}}}"#,
    );
    pack.png(&format!("{SPRITES}/blink.png"), 2, 4, |_, y| {
        [y as u8 / 2, 0, 0, 255]
    });
    pack.text(
        &format!("{SPRITES}/blink.png.mcmeta"),
        r#"{"animation": {}}"#,
    );
    let reader = pack.open();
    assert!(reader.warnings().is_empty(), "{:?}", reader.warnings());

    let icon = sprite(&reader, "icon");
    assert_eq!(&icon.rgba[..4], &[0, 255, 0, 255]);
    assert_eq!(
        icon.scaling,
        GuiScaling::Tile {
            width: 2,
            height: 2
        }
    );
    let spinner = sprite(&reader, "spinner");
    assert_eq!((spinner.width, spinner.height), (2, 2));
    assert_eq!(spinner.rgba, [2, 0, 0, 255].repeat(4));
    let blink = sprite(&reader, "blink");
    assert_eq!((blink.width, blink.height), (2, 2));
    assert_eq!(blink.rgba, [0, 0, 0, 255].repeat(4));

    // Forcing a format outside the overlay's range reads the base.
    let base = GuiPack::open_with(&pack.root, OpenOptions { format: Some(80) }).expect("open");
    let icon = base.sprite(&id("icon")).expect("reads").expect("exists");
    assert_eq!(
        (&icon.rgba[..4], icon.scaling),
        (&[255, 0, 0, 255][..], GuiScaling::Stretch)
    );
}

#[test]
fn animated_sprites_drop_the_frames_minecraft_drops() {
    let pack = TempPack::simple("animation_rules");
    // Strips of 2x2 frames whose red channel is the frame's index; `frames` lists the sequence.
    let cases = [
        // One usable frame left: the texture is static and shows strip frame 0.
        ("single", 2, r#"[1]"#, 0),
        // A frame of 0 ticks is dropped, so the sequence starts at strip frame 0.
        ("zero_time", 3, r#"[{"index": 1, "time": 0}, 0, 2]"#, 0),
        // An index outside the strip is dropped too.
        ("outside", 2, r#"[5, 1, 0]"#, 1),
        // Nothing usable: static, frame 0.
        ("none", 2, r#"[{"index": 1, "time": 0}, 7]"#, 0),
    ];
    for (name, frames, sequence, _) in cases {
        pack.png(&format!("{SPRITES}/{name}.png"), 2, 2 * frames, |_, y| {
            [y as u8 / 2, 0, 0, 255]
        });
        pack.text(
            &format!("{SPRITES}/{name}.png.mcmeta"),
            &format!(r#"{{"animation": {{"frames": {sequence}}}}}"#),
        );
    }
    let reader = pack.open();
    for (name, _, _, shown) in cases {
        let texture = sprite(&reader, name);
        assert_eq!((texture.width, texture.height), (2, 2), "{name}");
        assert_eq!(texture.rgba, [shown, 0, 0, 255].repeat(4), "{name}");
    }
}

#[test]
fn overlay_directories_minecraft_rejects_are_skipped() {
    let outside = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("gui_pack")
        .join("outside_overlay");
    let pack = TempPack::new(
        "bad_overlays",
        &format!(
            r#"{{"pack": {{"pack_format": 64, "description": "test"}},
                "overlays": {{"entries": [
                    {{"directory": "../outside_overlay", "formats": [1, 200]}},
                    {{"directory": "{}", "formats": [1, 200]}},
                    {{"directory": "late/inner", "formats": [1, 200]}},
                    {{"directory": "..", "formats": [1, 200]}},
                    {{"directory": ".", "formats": [1, 200]}},
                    {{"directory": "", "formats": [1, 200]}},
                    {{"directory": "Late-2.x_ok", "formats": [1, 200]}}
                ]}}}}"#,
            outside.display()
        ),
    );
    // Each rejected directory holds an icon that would win over the base's if it were read.
    let red = |_: u32, _: u32| [255, 0, 0, 255];
    let green = |_: u32, _: u32| [0, 255, 0, 255];
    let blue = |_: u32, _: u32| [0, 0, 255, 255];
    pack.png(&format!("{SPRITES}/icon.png"), 1, 1, red);
    pack.png(
        &format!("../outside_overlay/{SPRITES}/icon.png"),
        1,
        1,
        blue,
    );
    pack.png(&format!("late/inner/{SPRITES}/icon.png"), 1, 1, blue);
    pack.png(&format!("Late-2.x_ok/{SPRITES}/other.png"), 1, 1, green);
    let reader = pack.open();
    assert_eq!(
        sprite(&reader, "icon").rgba,
        [255, 0, 0, 255],
        "{:?}",
        reader.warnings()
    );
    // Upper case, `-`, `_` and `.` are fine, as in Minecraft.
    assert_eq!(sprite(&reader, "other").rgba, [0, 255, 0, 255]);
    let rejected: Vec<_> = reader
        .warnings()
        .iter()
        .filter(|w| w.message.contains("not a valid overlay directory"))
        .collect();
    assert_eq!(rejected.len(), 6, "{:?}", reader.warnings());
    let _ = std::fs::remove_dir_all(&outside);
}

/// The bitmap providers of a font, in order.
fn bitmaps(providers: &[FontProvider]) -> Vec<&BitmapProvider> {
    providers
        .iter()
        .filter_map(|p| match p {
            FontProvider::Bitmap(b) => Some(b),
            FontProvider::Space(_) => None,
        })
        .collect()
}

#[test]
fn fonts_follow_references_in_order() {
    let pack = TempPack::simple("fonts");
    pack.text(
        "assets/minecraft/font/default.json",
        r#"{"providers": [
            {"type": "space", "advances": {" ": 4, "\u200c": 0}},
            {"type": "reference", "id": "minecraft:include/default", "filter": {"uniform": false}},
            {"type": "bitmap", "file": "minecraft:font/uniform.png", "ascent": 7,
             "chars": ["ab"], "filter": {"uniform": true}},
            {"type": "reference", "id": "minecraft:include/unifont"},
            {"type": "ttf", "file": "minecraft:some.ttf"},
            {"type": "unihex", "hex_file": "minecraft:font/unifont.zip"},
            {"type": "legacy_unicode", "sizes": "x", "template": "y"}
        ]}"#,
    );
    pack.text(
        "assets/minecraft/font/include/default.json",
        r#"{"providers": [
            {"type": "bitmap", "file": "minecraft:font/accented.png", "height": 12,
             "ascent": 10, "chars": ["\u00e1\u00e9", "\u00f1\u0000"]},
            {"type": "bitmap", "file": "minecraft:font/ascii.png", "ascent": 7,
             "chars": ["ab", "cd", "ef"]}
        ]}"#,
    );
    pack.png(
        "assets/minecraft/textures/font/accented.png",
        18,
        24,
        |x, _| [255, 255, 255, (x % 9 < 5) as u8 * 255],
    );
    pack.png(
        "assets/minecraft/textures/font/ascii.png",
        16,
        24,
        |x, y| [255, 255, 255, (x + y) as u8],
    );
    let font = pack
        .open()
        .font(&id("minecraft:default"))
        .expect("reads")
        .expect("exists");

    assert_eq!(font.providers.len(), 3, "{:?}", font.warnings);
    let FontProvider::Space(space) = &font.providers[0] else {
        panic!("space first");
    };
    assert!(space.advances.contains(&(' ', 4.0)));
    let bitmaps = bitmaps(&font.providers);
    let [accented, ascii] = bitmaps[..] else {
        panic!("two bitmaps");
    };
    assert_eq!(accented.file, id("minecraft:font/accented.png"));
    assert_eq!(accented.chars, ["áé", "ñ\0"]);
    assert_eq!(
        (accented.glyph_height, accented.ascent, accented.cell()),
        (12, 10, [9, 12])
    );
    assert_eq!((accented.width, accented.height), (18, 24));
    assert_eq!(
        (ascii.glyph_height, ascii.ascent, ascii.cell()),
        (8, 7, [8, 8])
    );
    assert_eq!(
        &ascii.rgba[(16 + 3) * 4..(16 + 3) * 4 + 4],
        &[255, 255, 255, 4]
    );
    // The missing unifont reference and the three provider types skipped; the uniform-only
    // bitmap is left out quietly.
    assert_eq!(font.warnings.len(), 4, "{:?}", font.warnings);
    let text: String = font.warnings.iter().map(|w| w.message.clone()).collect();
    for word in ["include/unifont", "ttf", "unihex", "legacy_unicode"] {
        assert!(text.contains(word), "{word}: {text}");
    }
}

#[test]
fn bad_providers_are_skipped_with_a_warning() {
    let pack = TempPack::simple("bad_fonts");
    pack.png("assets/minecraft/textures/font/grid.png", 8, 8, |_, _| {
        [255; 4]
    });
    pack.png("assets/minecraft/textures/font/tiny.png", 1, 1, |_, _| {
        [255; 4]
    });
    pack.text(
        "assets/minecraft/font/default.json",
        r#"{"providers": [
            {"type": "bitmap", "file": "minecraft:font/grid.png", "ascent": 9, "chars": ["a"]},
            {"type": "bitmap", "file": "minecraft:font/grid.png", "ascent": 7, "chars": ["ab", "c"]},
            {"type": "bitmap", "file": "minecraft:font/grid.png", "ascent": 7, "chars": [""]},
            {"type": "bitmap", "file": "minecraft:font/grid.png", "chars": ["a"]},
            {"type": "bitmap", "file": "minecraft:font/none.png", "ascent": 7, "chars": ["a"]},
            {"type": "bitmap", "file": "minecraft:font/tiny.png", "ascent": 7, "chars": ["ab"]},
            {"type": "bitmap", "file": "minecraft:../../pack.png", "ascent": 7, "chars": ["a"]},
            {"type": "space", "advances": {"ab": 1}},
            {"type": "reference", "id": "minecraft:loop"},
            {"providers": []},
            {"type": "bitmap", "file": "minecraft:font/grid.png", "ascent": -2147483648,
             "chars": ["a"]},
            {"type": "bitmap", "file": "minecraft:font/grid.png", "height": 600000000,
             "ascent": 7, "chars": ["a"]},
            {"type": "bitmap", "file": "minecraft:font/grid.png", "ascent": -32768,
             "chars": ["y"]},
            {"type": "bitmap", "file": "minecraft:font/grid.png", "ascent": 7, "chars": ["z"]}
        ]}"#,
    );
    pack.text(
        "assets/minecraft/font/loop.json",
        r#"{"providers": [{"type": "reference", "id": "minecraft:default"}]}"#,
    );
    let font = pack
        .open()
        .font(&id("default"))
        .expect("reads")
        .expect("exists");
    let bitmaps = bitmaps(&font.providers);
    // A glyph far below the line (an old trick for invisible glyphs) is kept; one past
    // 32768 GUI pixels is skipped.
    assert_eq!(bitmaps.len(), 2);
    assert_eq!(bitmaps[0].chars, ["y"]);
    assert_eq!(bitmaps[0].ascent, -32768);
    assert_eq!(bitmaps[1].chars, ["z"]);
    assert_eq!(font.warnings.len(), 12, "{:?}", font.warnings);
    assert!(
        font.warnings[8].message.contains("cycle"),
        "{:?}",
        font.warnings[8]
    );

    // A font file that does not parse, or has no providers, is an error.
    pack.text("assets/minecraft/font/broken.json", "[");
    pack.text("assets/minecraft/font/empty.json", "{}");
    let reader = pack.open();
    let err = reader.font(&id("broken")).expect_err("broken");
    assert!(matches!(err, PackError::InvalidJson { .. }), "{err}");
    let err = reader.font(&id("empty")).expect_err("empty");
    assert!(matches!(err, PackError::InvalidFont { .. }), "{err}");
}

#[test]
fn a_font_referenced_again_is_read_once() {
    let pack = TempPack::simple("repeated_references");
    pack.png("assets/minecraft/textures/font/grid.png", 8, 8, |_, _| {
        [255; 4]
    });
    // Each level names the next three times, seven levels deep: read every time, the last
    // font would be read 3^7 = 2187 times.
    let refs = |next: &str| {
        let one = format!(r#"{{"type": "reference", "id": "minecraft:{next}"}}"#);
        format!(r#"{{"providers": [{one}, {one}, {one}]}}"#)
    };
    pack.text("assets/minecraft/font/default.json", &refs("f1"));
    for level in 1..7 {
        pack.text(
            &format!("assets/minecraft/font/f{level}.json"),
            &refs(&format!("f{}", level + 1)),
        );
    }
    pack.text(
        "assets/minecraft/font/f7.json",
        r#"{"providers": [
            {"type": "bitmap", "file": "minecraft:font/grid.png", "ascent": 7, "chars": ["x"]}
        ]}"#,
    );
    // A diamond: both branches name `c`, which comes in once, where it is first named.
    pack.text(
        "assets/minecraft/font/diamond.json",
        r#"{"providers": [
            {"type": "reference", "id": "minecraft:a"},
            {"type": "reference", "id": "minecraft:b"}
        ]}"#,
    );
    for (name, next) in [("a", "c"), ("b", "c")] {
        pack.text(
            &format!("assets/minecraft/font/{name}.json"),
            &format!(
                r#"{{"providers": [
                    {{"type": "reference", "id": "minecraft:{next}"}},
                    {{"type": "bitmap", "file": "minecraft:font/grid.png", "ascent": 7,
                      "chars": ["{name}"]}}
                ]}}"#
            ),
        );
    }
    pack.text(
        "assets/minecraft/font/c.json",
        r#"{"providers": [
            {"type": "bitmap", "file": "minecraft:font/grid.png", "ascent": 7, "chars": ["c"]}
        ]}"#,
    );
    let reader = pack.open();

    let font = reader.font(&id("default")).expect("reads").expect("exists");
    assert_eq!(font.providers.len(), 1, "{:?}", font.warnings.len());
    assert!(font.warnings.is_empty(), "{:?}", font.warnings);

    let font = reader.font(&id("diamond")).expect("reads").expect("exists");
    let chars: Vec<_> = bitmaps(&font.providers)
        .iter()
        .map(|b| b.chars[0].clone())
        .collect();
    assert_eq!(chars, ["c", "a", "b"]);
    assert!(font.warnings.is_empty(), "{:?}", font.warnings);
}
