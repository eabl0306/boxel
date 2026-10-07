//! Integration tests against the tiny fixture pack and optional real packs.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use boxel_pack::{
    BlockState, Condition, PackError, ResourceId, ResourcePack, condition_matches,
    parse_variant_key, resolve_model, split_frames,
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn overlays_win_over_base() {
    let pack = ResourcePack::open(&fixture("tiny_pack")).expect("open tiny_pack");
    let stone = pack
        .texture(&ResourceId::parse("minecraft:block/stone"))
        .expect("stone texture");
    // Overlay paints stone green (40,200,40); base was red.
    assert_eq!(stone.width, 16);
    assert_eq!(stone.height, 16);
    assert_eq!(&stone.rgba[0..4], &[40, 200, 40, 255]);
}

#[test]
fn animation_frames_split() {
    let pack = ResourcePack::open(&fixture("tiny_pack")).expect("open");
    let water = pack
        .texture(&ResourceId::parse("minecraft:block/water_still"))
        .expect("water");
    let anim = water.animation.as_ref().expect("animation");
    assert_eq!(anim.frametime, 2);
    assert!(anim.interpolate);
    assert_eq!(anim.frames.len(), 2);
    assert_eq!(anim.frames[0].index, 0);
    assert_eq!(anim.frames[1].index, 1);
    assert_eq!(anim.frames[1].time, Some(4));
    assert_eq!(water.frames.len(), 2);
    assert_eq!(water.frames[0].len(), 16 * 16 * 4);
    // Also exercise the public splitter on the strip bytes.
    let again = split_frames(water.width, water.height, &water.rgba, anim).unwrap();
    assert_eq!(again.len(), 2);
}

#[test]
fn model_inheritance_and_variables() {
    let pack = ResourcePack::open(&fixture("tiny_pack")).expect("open");
    let stone = pack
        .model(&ResourceId::parse("minecraft:block/stone"))
        .expect("stone model");
    assert_eq!(
        stone
            .textures
            .get("all")
            .map(ToString::to_string)
            .as_deref(),
        Some("minecraft:block/stone")
    );
    assert_eq!(stone.elements.len(), 1);
    assert_eq!(stone.elements[0].faces.len(), 6);

    let custom = pack
        .model(&ResourceId::parse("minecraft:block/custom_block"))
        .expect("custom_block");
    assert_eq!(
        custom
            .textures
            .get("north")
            .map(ToString::to_string)
            .as_deref(),
        Some("minecraft:block/dirt")
    );
    assert_eq!(
        custom
            .textures
            .get("up")
            .map(ToString::to_string)
            .as_deref(),
        Some("minecraft:block/stone")
    );

    let cross = pack
        .model(&ResourceId::parse("minecraft:block/cross_plant"))
        .expect("cross");
    assert!(!cross.ambient_occlusion);
    assert_eq!(cross.elements.len(), 2);
}

#[test]
fn undefined_variable_and_parent_cycle() {
    // Load raw models via opening and checking warnings, and call resolve_model on the
    // skipped ids using a fresh map built from the fixture files would be heavy; instead
    // open the pack and assert those models are absent + warnings mention them, then unit
    // resolve with an explicit map.
    let pack = ResourcePack::open(&fixture("tiny_pack")).expect("open");
    assert!(
        pack.model(&ResourceId::parse("minecraft:block/broken_undef"))
            .is_none()
    );
    assert!(
        pack.model(&ResourceId::parse("minecraft:block/cycle_a"))
            .is_none()
    );
    let msgs: Vec<_> = pack.warnings().iter().map(|w| w.message.clone()).collect();
    assert!(
        msgs.iter().any(|m| m.contains("broken_undef")),
        "warnings: {msgs:?}"
    );
    assert!(
        msgs.iter().any(|m| m.contains("cycle")),
        "warnings: {msgs:?}"
    );

    use boxel_pack::RawModel;
    use std::collections::BTreeMap as Map;
    let mut models = Map::new();
    models.insert(
        ResourceId::parse("minecraft:block/broken_undef"),
        RawModel {
            parent: Some(ResourceId::parse("minecraft:block/cube_all")),
            textures: Map::from([("all".into(), "#missing".into())]),
            ..RawModel::default()
        },
    );
    let err =
        resolve_model(&ResourceId::parse("minecraft:block/broken_undef"), &models).unwrap_err();
    assert!(
        matches!(err, PackError::UndefinedTextureVar { .. }),
        "{err}"
    );

    models.clear();
    models.insert(
        ResourceId::parse("minecraft:block/cycle_a"),
        RawModel {
            parent: Some(ResourceId::parse("minecraft:block/cycle_b")),
            ..RawModel::default()
        },
    );
    models.insert(
        ResourceId::parse("minecraft:block/cycle_b"),
        RawModel {
            parent: Some(ResourceId::parse("minecraft:block/cycle_a")),
            ..RawModel::default()
        },
    );
    let err = resolve_model(&ResourceId::parse("minecraft:block/cycle_a"), &models).unwrap_err();
    assert!(matches!(err, PackError::ModelParentCycle(_)), "{err}");
}

#[test]
fn variants_and_multipart() {
    let pack = ResourcePack::open(&fixture("tiny_pack")).expect("open");
    let stone = pack
        .blockstate(&ResourceId::parse("minecraft:stone"))
        .expect("stone bs");
    match stone {
        BlockState::Variants(v) => {
            let list = v.get("").expect("empty key");
            assert_eq!(list.0.len(), 1);
            assert_eq!(list.0[0].model.to_string(), "minecraft:block/stone");
        }
        BlockState::Multipart(_) => panic!("expected variants"),
    }

    let stairs = pack
        .blockstate(&ResourceId::parse("minecraft:oak_stairs"))
        .expect("stairs");
    let BlockState::Variants(v) = stairs else {
        panic!("expected variants");
    };
    let key = "facing=north,half=top,shape=inner_left";
    let parsed = parse_variant_key(key);
    assert_eq!(parsed.get("facing").map(String::as_str), Some("north"));
    let entry = v.get(key).expect("stairs variant");
    assert_eq!(entry.0[0].x, 180);
    assert_eq!(entry.0[0].y, 270);
    assert!(entry.0[0].uvlock);

    let alt = v
        .get("facing=south,half=bottom,shape=straight")
        .expect("weighted");
    assert_eq!(alt.0.len(), 2);
    assert_eq!(alt.0[0].weight, 2);

    let bars = pack
        .blockstate(&ResourceId::parse("minecraft:iron_bars"))
        .expect("bars");
    let BlockState::Multipart(cases) = bars else {
        panic!("expected multipart");
    };
    assert!(cases[0].when.is_none());
    let when = cases[2].when.as_ref().expect("OR when");
    let Condition::Or(or) = when else {
        panic!("expected OR");
    };
    assert_eq!(or.len(), 2);
    // Pipe values: east = true|false
    let state = BTreeMap::from([
        ("east".into(), "false".into()),
        ("north".into(), "true".into()),
    ]);
    assert!(condition_matches(when, &state));

    let and_when = cases[3].when.as_ref().expect("AND");
    assert!(matches!(and_when, Condition::And(_)));
}

#[test]
fn odd_texture_size_is_warning_not_panic() {
    let pack = ResourcePack::open(&fixture("tiny_pack")).expect("open");
    let odd = pack
        .texture(&ResourceId::parse("minecraft:block/odd"))
        .expect("odd texture loads");
    assert_eq!(odd.width, 32);
    assert_eq!(odd.height, 16);
    assert!(
        pack.warnings()
            .iter()
            .any(|w| w.kind == "size_mismatch" || w.message.contains("odd")),
        "expected size warning, got {:?}",
        pack.warnings()
    );
}

#[test]
fn ignored_dirs_produce_warnings() {
    let pack = ResourcePack::open(&fixture("tiny_pack")).expect("open");
    let joined: String = pack
        .warnings()
        .iter()
        .map(|w| w.message.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("lang")
            || joined.contains("sounds")
            || joined.contains("optifine")
            || joined.contains("unknown"),
        "{joined}"
    );
}

#[test]
fn invalid_json_reports_path() {
    let err = ResourcePack::open(&fixture("bad_json")).unwrap_err();
    match err {
        PackError::InvalidJson { path, .. } => {
            let s = path.to_string_lossy();
            assert!(
                s.contains("broken.json"),
                "path should mention broken.json, got {s}"
            );
        }
        other => panic!("expected InvalidJson, got {other}"),
    }
}

#[test]
fn real_pack_if_env_set() {
    let Some(dir) = std::env::var_os("BOXEL_PACK_DIR") else {
        eprintln!("BOXEL_PACK_DIR unset; skipping real pack smoke test");
        return;
    };
    let path = PathBuf::from(&dir);
    // Cargo runs crate tests with cwd = the crate dir; resolve relative paths from the
    // workspace root (parent of `crates/`).
    let path = if path.is_absolute() || path.is_dir() {
        path
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(&path)
    };
    let path = path.canonicalize().unwrap_or(path);
    let start = Instant::now();
    let pack = ResourcePack::open(&path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    let elapsed = start.elapsed();
    println!(
        "BOXEL_PACK_DIR={}: textures={} models={} blockstates={} warnings={} elapsed={:.3}s format={}",
        path.display(),
        pack.texture_count(),
        pack.model_count(),
        pack.blockstate_count(),
        pack.warnings().len(),
        elapsed.as_secs_f64(),
        pack.format()
    );
    assert!(
        pack.texture_count() > 0,
        "expected at least one block texture"
    );
}
