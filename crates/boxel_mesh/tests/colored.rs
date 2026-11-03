//! Colored sky in the mesh (ADR 0064): tinted palette entries, their order and their decode,
//! against `reference_colored_face_shading` and rules written here again.
mod common;

use boxel_light::PaddedLight;
use boxel_mesh::{
    AoPolicy, ChunkMesh, ColoredLight, LightQuantization, MAX_PALETTE_ENTRIES, MaterialTable,
    MeshOptions, Mesher, PADDED_VOLUME, PackedQuad, PaddedChunk, layout, pack_tinted_entry,
    palette_corners, reference_colored_face_shading, tinted_corners, tinted_exact_corners,
};
use boxel_voxel::{Face, LightValue};
use common::{
    CUTOUT, Fill, NO_GEOMETRY, OPAQUE, Rng, TRANSLUCENT, random_padded, test_table, verify_mesh,
};
use proptest::prelude::*;

/// Every material the random fills draw from.
const MATERIALS: [u16; 8] = [
    OPAQUE[0],
    OPAQUE[1],
    OPAQUE[2],
    CUTOUT[0],
    CUTOUT[1],
    TRANSLUCENT[0],
    TRANSLUCENT[1],
    NO_GEOMETRY,
];

/// How a random colored light is made.
#[derive(Clone, Copy, Debug)]
enum TintFill {
    /// White sky over a random height, tinted under it and fading down, white leaking in from
    /// one side, and a few colored lamps: a glass roof with open sides.
    Roof,
    /// Each sky channel its own smooth gradient: a window seen from inside.
    Window,
    /// A few random lights in 2x2x2 blocks (both views).
    Blocky,
    /// A tinted sky that never changes and block channels in a 4-level band per voxel: too many
    /// exact corners for the palette, few enough at 3 bits.
    Band,
    /// A random light per voxel: overflows the palette even at 3 bits.
    Noise,
    /// Gray sky (black or gray glass), not the uncolored level: no tinted entry.
    Gray,
}

const TINT_FILLS: [TintFill; 6] = [
    TintFill::Roof,
    TintFill::Window,
    TintFill::Blocky,
    TintFill::Band,
    TintFill::Noise,
    TintFill::Gray,
];

/// A padded array from a function of padded `(x, y, z)`.
fn padded_array(mut f: impl FnMut(i32, i32, i32) -> u16) -> Box<[u16; PADDED_VOLUME]> {
    let mut values = vec![0u16; PADDED_VOLUME];
    for y in -1..=32 {
        for x in -1..=32 {
            for z in -1..=32 {
                values[PaddedLight::index(x, y, z)] = f(x, y, z);
            }
        }
    }
    values.into_boxed_slice().try_into().expect("PADDED_VOLUME")
}

/// A colored padded light from one function giving, per padded voxel, the light gameplay reads
/// (uncolored sky and block channels) and the sky's channels (`0x0RGB`).
fn colored_light(mut f: impl FnMut(i32, i32, i32) -> (u16, u16)) -> PaddedLight {
    let mut sky = vec![0u16; PADDED_VOLUME];
    let light = padded_array(|x, y, z| {
        let (light, rgb) = f(x, y, z);
        sky[PaddedLight::index(x, y, z)] = rgb & LightValue::BLOCK_MASK;
        light
    });
    PaddedLight::Colored {
        light,
        sky_rgb: sky.into_boxed_slice().try_into().expect("PADDED_VOLUME"),
    }
}

fn rgb(c: [u16; 3]) -> u16 {
    LightValue::from_rgb(c.map(|v| v.min(15) as u8)).0
}

/// A random colored light of kind `fill`.
fn random_colored(fill: TintFill, seed: u64) -> PaddedLight {
    let mut rng = Rng::new(seed ^ 0xC0_10_5E_ED);
    let level = |rng: &mut Rng| rng.below(16) as u16;
    match fill {
        TintFill::Roof => {
            let roof = rng.range(-4, 36);
            let tint = [0; 3].map(|_| level(&mut rng));
            let edge = rng.range(-8, 40);
            let lamps: Vec<([i32; 3], [u16; 3])> = (0..rng.range(0, 3))
                .map(|_| {
                    let p = [0; 3].map(|_| rng.range(-8, 40));
                    (p, [0; 3].map(|_| level(&mut rng)))
                })
                .collect();
            colored_light(|x, y, z| {
                let depth = (roof - y).max(0) as u16;
                let white = (15 - (x - edge).abs()).max(0) as u16;
                let sky = if y > roof {
                    [15; 3]
                } else {
                    tint.map(|c| c.saturating_sub(depth / 3).max(white))
                };
                let mut block = LightValue::DARK;
                for (p, lamp) in &lamps {
                    let d = ((x - p[0]).abs() + (y - p[1]).abs() + (z - p[2]).abs()) as u16;
                    block = block.max(LightValue::from_rgb(
                        lamp.map(|c| c.saturating_sub(d) as u8),
                    ));
                }
                let uncolored = (15 - depth / 3).max(white);
                (uncolored << 12 | block.0, rgb(sky))
            })
        }
        TintFill::Window => {
            let centers = [0; 3].map(|_| [rng.range(-8, 40), rng.range(-8, 40)]);
            let slopes = [0; 3].map(|_| rng.range(1, 3));
            let lamp = level(&mut rng);
            colored_light(|x, y, z| {
                let sky = [0, 1, 2].map(|c| {
                    let [cx, cy] = centers[c];
                    let d = (x - cx).abs() + (y - cy).abs();
                    (15 - d / slopes[c]).max(0) as u16
                });
                let block = lamp.saturating_sub(((z - 16).abs() / 2) as u16);
                let uncolored = sky.iter().max().copied().unwrap_or(0);
                (uncolored << 12 | block << 8, rgb(sky))
            })
        }
        TintFill::Blocky => {
            let values: Vec<(u16, u16)> = (0..rng.range(1, 6))
                .map(|_| (rng.next_u64() as u16, rng.next_u64() as u16))
                .collect();
            let salt = rng.next_u64();
            colored_light(|x, y, z| {
                let cell = Rng::new(
                    salt ^ ((x >> 1) as u64) << 40 ^ ((y >> 1) as u64) << 20 ^ (z >> 1) as u64,
                )
                .next_u64();
                values[(cell % values.len() as u64) as usize]
            })
        }
        TintFill::Band => {
            let tint = rgb([0; 3].map(|_| level(&mut rng)));
            colored_light(|_, _, _| {
                let block = [0; 3]
                    .iter()
                    .fold(0, |v, _| v << 4 | (12 + rng.below(4) as u16));
                (0xF000 | block, tint)
            })
        }
        TintFill::Noise => colored_light(|_, _, _| (rng.next_u64() as u16, rng.next_u64() as u16)),
        TintFill::Gray => {
            let salt = rng.next_u64();
            colored_light(|x, y, z| {
                let cell = Rng::new(
                    salt ^ ((x >> 1) as u64) << 40 ^ ((y >> 2) as u64) << 20 ^ (z >> 2) as u64,
                )
                .next_u64();
                let s = (cell % 16) as u16;
                ((cell >> 8) as u16 | 0xF000, rgb([s; 3]))
            })
        }
    }
}

/// The colored light of padded voxel `p`, as the picture shows it.
fn colored_at(light: &PaddedLight, p: [i32; 3]) -> ColoredLight {
    let [x, y, z] = p;
    ColoredLight {
        sky: LightValue::from_rgb(light.sky_rgb(x, y, z)).0,
        block: light.get(x, y, z).0 & LightValue::BLOCK_MASK,
    }
}

/// The rule of the Low and Mid tiers, written again: a face with some tinted corner keeps the
/// corners of the kind that varies more (ties to the sky), the other kind is flat at its
/// rounded per-channel mean. A face of gray corners is exact.
fn flattened(corners: [ColoredLight; 4]) -> [ColoredLight; 4] {
    if corners.iter().all(|c| c.is_gray()) {
        return corners;
    }
    let channels = |kind: fn(ColoredLight) -> [u8; 3]| corners.map(kind);
    let spread = |c: [[u8; 3]; 4]| -> u32 {
        (0..3)
            .map(|ch| {
                let v = c.map(|l| l[ch]);
                u32::from(v.iter().max().unwrap() - v.iter().min().unwrap())
            })
            .sum()
    };
    let mean = |c: [[u8; 3]; 4]| -> [u8; 3] {
        std::array::from_fn(|ch| ((c.iter().map(|l| u32::from(l[ch])).sum::<u32>() + 2) / 4) as u8)
    };
    let (sky, block) = (
        channels(ColoredLight::sky_rgb),
        channels(ColoredLight::block_rgb),
    );
    if spread(block) > spread(sky) {
        let flat = mean(sky);
        corners.map(|c| ColoredLight::new(flat, c.block_rgb()))
    } else {
        let flat = mean(block);
        corners.map(|c| ColoredLight::new(c.sky_rgb(), flat))
    }
}

/// Checks a mesh under colored light against [`reference_colored_face_shading`] (lit emitters
/// of `table` flat, at their front cell's light raised to their emission): every unit face of
/// every quad has the oracle's AO byte; decoded as High draws it
/// ([`ChunkMesh::exact_corner_colors`]), the oracle's corners at the mesh's quantization level;
/// decoded as Low and Mid draw it ([`ChunkMesh::corner_colors`]), [`flattened`] of them. Also
/// the palette: it fits, every entry is used, the first `tinted_entries` are tinted (with their
/// exact parts) and the others are not.
fn verify_colored(
    padded: &PaddedChunk,
    table: &MaterialTable,
    light: &PaddedLight,
    policy: AoPolicy,
    mesh: &ChunkMesh,
) {
    let reference = reference_colored_face_shading(padded, Some(light), policy);
    let elide = policy == AoPolicy::BakedLitOnly;
    let tinted = mesh.tinted_entries as usize;
    assert!(
        mesh.palette.len() <= MAX_PALETTE_ENTRIES,
        "palette overflow"
    );
    assert!(tinted <= mesh.palette.len());
    assert_eq!(
        mesh.tinted_exact.len(),
        tinted,
        "one exact part per tinted entry"
    );
    for (i, &entry) in mesh.palette[..tinted].iter().enumerate() {
        let exact = tinted_exact_corners(entry, mesh.tinted_exact[i]);
        assert!(exact.iter().any(|c| !c.is_gray()), "entry {i} is tinted");
        assert_eq!(entry[1] >> 29, 0, "spare bits of entry {i}");
    }
    let mut used = vec![false; mesh.palette.len()];
    let mut faces = 0;
    for &quad in &mesh.quads {
        let index = usize::from(quad.light_index());
        assert!(
            index < mesh.palette.len(),
            "{quad:?} indexes past the palette"
        );
        used[index] = true;
        let (exact, low) = (mesh.exact_corner_colors(quad), mesh.corner_colors(quad));
        for (p, face) in quad.unit_faces() {
            let mut want = *reference
                .get(&(p, face))
                .unwrap_or_else(|| panic!("{p:?} {face:?} is not a visible face"));
            let [x, y, z] = p.map(|c| c as i32);
            let emission = if padded.is_surface(PaddedChunk::index(x, y, z)) {
                LightValue::DARK
            } else {
                table.lit_emission(padded.get(x, y, z))
            };
            if !emission.is_dark() {
                let n = face.normal();
                let front = colored_at(light, [x + n.x, y + n.y, z + n.z]);
                want.ao = 0xFF;
                want.light = [front.max(ColoredLight::from_value(emission)); 4];
            }
            let want = want.quantized(mesh.light_quantization, elide);
            assert_eq!(quad.ao_byte(), want.ao, "{quad:?}: AO of {p:?} {face:?}");
            assert_eq!(exact, want.light, "{quad:?}: High light of {p:?} {face:?}");
            assert_eq!(
                low,
                flattened(want.light),
                "{quad:?}: Low light of {p:?} {face:?}"
            );
            faces += 1;
        }
    }
    assert_eq!(faces, reference.len(), "unit faces");
    assert!(used.iter().all(|&u| u), "unused palette entries");
}

fn mesh(
    padded: &PaddedChunk,
    table: &MaterialTable,
    light: &PaddedLight,
    ao: AoPolicy,
) -> ChunkMesh {
    let mut out = ChunkMesh::default();
    Mesher::new().mesh(
        padded,
        Some(light),
        table,
        MeshOptions::with_ao(ao),
        &mut out,
    );
    out
}

/// A colored light whose 6 channels make every corner below 16 per channel.
fn corner(rng: &mut Rng) -> ColoredLight {
    ColoredLight {
        sky: rng.next_u64() as u16 & 0x0FFF,
        block: rng.next_u64() as u16 & 0x0FFF,
    }
}

#[test]
fn tinted_entries_round_trip_for_both_tags() {
    let mut rng = Rng::new(7);
    let mut tags = [0; 2];
    for _ in 0..20_000 {
        let mut corners = [0; 4].map(|_| corner(&mut rng));
        // Some faces with one kind flat, which every tier draws exactly.
        match rng.below(4) {
            0 => {
                corners = corners.map(|c| ColoredLight {
                    sky: corners[0].sky,
                    ..c
                });
            }
            1 => {
                corners = corners.map(|c| ColoredLight {
                    block: corners[0].block,
                    ..c
                });
            }
            _ => {}
        }
        let (entry, exact) = pack_tinted_entry(corners);
        let tag = entry[1] >> layout::TINT_TAG_BIT & 1;
        tags[tag as usize] += 1;
        assert_eq!(entry[1] >> 29, 0, "spare bits");
        assert_eq!([exact[0] >> 24, exact[1] >> 24], [0, 0], "spare bits");
        assert_eq!(
            tinted_exact_corners(entry, exact),
            corners,
            "High decodes exactly"
        );
        assert_eq!(
            tinted_corners(entry),
            flattened(corners),
            "Low and Mid flatten"
        );
        let kept = tinted_corners(entry);
        if tag == layout::TINT_SKY_CORNERS {
            assert_eq!(kept.map(|c| c.sky), corners.map(|c| c.sky));
        } else {
            assert_eq!(kept.map(|c| c.block), corners.map(|c| c.block));
        }
    }
    assert!(tags.iter().all(|&n| n > 1000), "both tags: {tags:?}");
}

#[test]
fn a_face_with_one_flat_kind_is_exact_on_every_tier() {
    let red = [13, 7, 6];
    // Tinted sky varying, block light even: sky corners, exact.
    let sky_varies = [
        ColoredLight::new(red, [3, 2, 1]),
        ColoredLight::new([12, 6, 5], [3, 2, 1]),
        ColoredLight::new([15, 15, 15], [3, 2, 1]),
        ColoredLight::new([11, 5, 4], [3, 2, 1]),
    ];
    // A lamp's gradient under an even red sky: block corners, exact.
    let block_varies = [
        ColoredLight::new(red, [14, 0, 0]),
        ColoredLight::new(red, [13, 0, 0]),
        ColoredLight::new(red, [12, 1, 0]),
        ColoredLight::new(red, [0, 0, 9]),
    ];
    // Both even.
    let even = [ColoredLight::new(red, [5, 5, 5]); 4];
    for (corners, tag) in [
        (sky_varies, layout::TINT_SKY_CORNERS),
        (block_varies, layout::TINT_BLOCK_CORNERS),
        (even, layout::TINT_SKY_CORNERS),
    ] {
        let (entry, exact) = pack_tinted_entry(corners);
        assert_eq!(entry[1] >> layout::TINT_TAG_BIT & 1, tag, "{corners:?}");
        assert_eq!(tinted_corners(entry), corners);
        assert_eq!(tinted_exact_corners(entry, exact), corners);
    }
    // Both varying: Low and Mid flatten the one that varies less, High stays exact.
    let both = [
        ColoredLight::new([13, 7, 6], [14, 0, 0]),
        ColoredLight::new([12, 6, 6], [10, 0, 0]),
        ColoredLight::new([13, 7, 6], [6, 0, 0]),
        ColoredLight::new([12, 6, 5], [2, 0, 0]),
    ];
    let (entry, exact) = pack_tinted_entry(both);
    assert_eq!(
        entry[1] >> layout::TINT_TAG_BIT & 1,
        layout::TINT_BLOCK_CORNERS
    );
    assert_eq!(
        tinted_corners(entry),
        both.map(|c| ColoredLight::new([13, 7, 6], c.block_rgb()))
    );
    assert_eq!(tinted_exact_corners(entry, exact), both);
}

/// The surface of the terrain fill, lit by `light`.
fn terrain(seed: u64) -> PaddedChunk {
    random_padded(&test_table(), Fill::Terrain, seed, 40, &MATERIALS[..4])
}

#[test]
fn equal_channels_give_normal_entries() {
    // Black glass: gray sky at 6 where gameplay sees 15. No tinted entry, and the mesh is the one
    // of a plain light whose sky is the picture's.
    let table = test_table();
    for seed in 0..8 {
        let padded = terrain(seed);
        let gray = random_colored(TintFill::Gray, seed);
        let plain = PaddedLight::Full(padded_array(|x, y, z| {
            let [s, ..] = gray.sky_rgb(x, y, z);
            u16::from(s) << 12 | gray.get(x, y, z).0 & LightValue::BLOCK_MASK
        }));
        for policy in [AoPolicy::Off, AoPolicy::Baked, AoPolicy::BakedLitOnly] {
            let colored = mesh(&padded, &table, &gray, policy);
            assert_eq!(colored.tinted_entries, 0);
            assert!(colored.tinted_exact.is_empty());
            assert_eq!(colored, mesh(&padded, &table, &plain, policy));
            verify_colored(&padded, &table, &gray, policy, &colored);
        }
    }
}

#[test]
fn a_colored_form_without_tint_meshes_as_full_byte_for_byte() {
    // `sky_rgb` equal to the uncolored sky everywhere: exactly today's mesh, hash included.
    let table = test_table();
    for seed in 0..8 {
        let padded = terrain(seed);
        let full = common::random_light(common::LightFill::Smooth, seed).expect("light");
        let PaddedLight::Full(values) = &full else {
            panic!("a full light");
        };
        let colored = PaddedLight::Colored {
            light: values.clone(),
            sky_rgb: padded_array(|x, y, z| {
                let s = u16::from(full.get(x, y, z).sky());
                s << 8 | s << 4 | s
            }),
        };
        for policy in [AoPolicy::Baked, AoPolicy::BakedLitOnly] {
            assert_eq!(
                mesh(&padded, &table, &colored, policy),
                mesh(&padded, &table, &full, policy)
            );
        }
    }
}

/// The quads of a mesh without their light index.
fn unlit(mesh: &ChunkMesh) -> Vec<PackedQuad> {
    mesh.quads.iter().map(|q| q.with_light_index(0)).collect()
}

#[test]
fn renumbering_keeps_every_merge() {
    // A colored light with one kind even across the chunk merges like a plain light holding the
    // other kind in its block channels, whose palette is one list in first-seen order: the same
    // quads, each lit by the same corners, though tinted entries moved to the front.
    let table = test_table();
    let mut tinted_meshes = 0;
    for seed in 0..12 {
        let padded = terrain(seed);
        let window = random_colored(TintFill::Window, seed);
        let lamps = random_colored(TintFill::Roof, seed);
        // Sky varying, block dark; and a red sky over block light that varies.
        let sky_only =
            colored_light(|x, y, z| (0, LightValue::from_rgb(window.sky_rgb(x, y, z)).0));
        let sky_twin = PaddedLight::Full(padded_array(|x, y, z| {
            LightValue::from_rgb(window.sky_rgb(x, y, z)).0
        }));
        let block_only = colored_light(|x, y, z| {
            (
                lamps.get(x, y, z).0 & LightValue::BLOCK_MASK,
                rgb([13, 7, 6]),
            )
        });
        let block_twin = PaddedLight::Full(padded_array(|x, y, z| {
            0xF000 | lamps.get(x, y, z).0 & LightValue::BLOCK_MASK
        }));
        for policy in [AoPolicy::Baked, AoPolicy::BakedLitOnly] {
            for (colored, twin, sky_kept) in [
                (&sky_only, &sky_twin, true),
                (&block_only, &block_twin, false),
            ] {
                let got = mesh(&padded, &table, colored, policy);
                let want = mesh(&padded, &table, twin, policy);
                assert_eq!(got.light_quantization, LightQuantization::Exact);
                assert_eq!(unlit(&got), unlit(&want), "seed {seed}: the same merges");
                assert_eq!(got.bucket_counts, want.bucket_counts);
                assert_eq!(
                    got.palette.len(),
                    want.palette.len(),
                    "one entry per corner set"
                );
                for (&g, &w) in got.quads.iter().zip(&want.quads) {
                    let kept = got.exact_corner_colors(g);
                    let twin = want.corner_light(w).map(|c| c.0 & LightValue::BLOCK_MASK);
                    let kept = kept.map(|c| if sky_kept { c.sky } else { c.block });
                    assert_eq!(kept, twin, "{g:?} against {w:?}");
                }
                verify_colored(&padded, &table, colored, policy, &got);
                tinted_meshes += usize::from(got.tinted_entries != 0);
            }
        }
    }
    assert!(
        tinted_meshes > 12,
        "the scenes have tinted entries: {tinted_meshes}"
    );
}

#[test]
fn flat_drops_the_tint_and_fits() {
    let table = test_table();
    let padded = random_padded(&table, Fill::Noise, 3, 50, &MATERIALS[..3]);
    let noise = random_colored(TintFill::Noise, 3);
    let out = mesh(&padded, &table, &noise, AoPolicy::Baked);
    assert_eq!(out.light_quantization, LightQuantization::Flat);
    assert_eq!(out.tinted_entries, 0, "no tint at Flat");
    assert!(out.tinted_exact.is_empty());
    assert!(out.palette.len() <= 1024, "{} entries", out.palette.len());
    for &entry in &out.palette {
        let c = palette_corners(entry);
        assert!(c.iter().all(|&v| v == c[0] && v.0 & 0x0333 == 0), "{c:?}");
    }
    verify_colored(&padded, &table, &noise, AoPolicy::Baked, &out);
}

#[test]
fn band_light_falls_back_to_block3_keeping_the_tint() {
    let table = test_table();
    let padded = random_padded(&table, Fill::Noise, 5, 50, &MATERIALS[..3]);
    let band = random_colored(TintFill::Band, 5);
    let out = mesh(&padded, &table, &band, AoPolicy::Baked);
    assert_eq!(out.light_quantization, LightQuantization::Block3);
    assert!(out.tinted_entries > 0, "the sky keeps its tint at Block3");
    verify_colored(&padded, &table, &band, AoPolicy::Baked, &out);
}

#[test]
fn a_dark_face_needs_all_six_channels_dark() {
    // A floor with no block light under a sky that is black in red only, then in every channel:
    // only the second elides its AO.
    let table = test_table();
    let mut padded = PaddedChunk::new();
    for x in -1..=32 {
        for z in -1..=32 {
            common::set_material(&mut padded, &table, [x, 0, z], OPAQUE[0]);
        }
    }
    common::set_material(&mut padded, &table, [5, 1, 5], OPAQUE[0]);
    let face_ao = |sky: [u16; 3]| {
        let light = colored_light(|_, _, _| (0, rgb(sky)));
        let out = mesh(&padded, &table, &light, AoPolicy::BakedLitOnly);
        verify_colored(&padded, &table, &light, AoPolicy::BakedLitOnly, &out);
        out.quads
            .iter()
            .filter(|q| q.face() == Face::PosY && q.pos()[1] == 0)
            .map(|q| q.ao_byte())
            .min()
            .expect("floor quads")
    };
    assert_ne!(face_ao([0, 4, 0]), 0xFF, "a green sky is lit");
    assert_eq!(face_ao([0, 0, 0]), 0xFF, "a black sky is dark");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// Under random colored light and AO policy, lit emitters included: the geometry equals the
    /// oracle and every face equals `reference_colored_face_shading` (exact on High, flattened
    /// by the rule on Low and Mid). Meshing again with a used mesher, after a plain chunk, gives
    /// the same mesh; and a plain chunk after a colored one is the plain mesh.
    #[test]
    fn colored_shading_matches_oracle(
        fill in proptest::sample::select(vec![Fill::Noise, Fill::Boxes, Fill::Terrain, Fill::Lattice]),
        tint in proptest::sample::select(TINT_FILLS.to_vec()),
        policy in proptest::sample::select(vec![AoPolicy::Off, AoPolicy::Baked, AoPolicy::BakedLitOnly]),
        lit in any::<bool>(),
        seed in any::<u64>(),
        density in 0u32..=100,
        palette in proptest::collection::vec(proptest::sample::select(MATERIALS.to_vec()), 1..=4),
    ) {
        let mut table = test_table();
        if lit {
            table.set_lit_emission(OPAQUE[1], LightValue::from_rgb([9, 4, 0]));
            table.set_lit_emission(TRANSLUCENT[1], LightValue::from_rgb([15, 15, 15]));
        }
        let padded = random_padded(&table, fill, seed, density, &palette);
        let light = random_colored(tint, seed);
        let options = MeshOptions::with_ao(policy);
        let mut mesher = Mesher::new();
        let mut out = ChunkMesh::default();
        mesher.mesh(&padded, Some(&light), &table, options, &mut out);
        verify_mesh(&padded, &table, &out);
        verify_colored(&padded, &table, &light, policy, &out);
        prop_assert_eq!(out.hash, out.compute_hash());

        let plain = common::random_light(common::LightFill::Smooth, seed);
        let mut fresh = ChunkMesh::default();
        Mesher::new().mesh(&padded, plain.as_ref(), &table, options, &mut fresh);
        let mut after = ChunkMesh::default();
        mesher.mesh(&padded, plain.as_ref(), &table, options, &mut after);
        prop_assert_eq!(&after, &fresh);
        let mut again = ChunkMesh::default();
        mesher.mesh(&padded, Some(&light), &table, options, &mut again);
        prop_assert_eq!(&again, &out);
    }
}
