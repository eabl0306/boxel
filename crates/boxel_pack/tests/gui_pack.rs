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
    assert_eq!(bitmaps.len(), 1);
    assert_eq!(bitmaps[0].chars, ["z"]);
    assert_eq!(font.warnings.len(), 10, "{:?}", font.warnings);
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
