//! Planes inside a box (EW-PLANE, ADR 0046): where a plane's corners are and what they show,
//! how planes turn with their model, and how they count in the model box table.

use boxel_voxel::testing;
use boxel_voxel::{
    Axis, BlockCollision, BlockDef, BlockModel, BoxModel, CollisionBox, FaceTextures,
    MAX_MODEL_BOXES, ModelBox, ModelPlane, PlaneCorner, PlaneKind, RegistryError, RenderShape,
    TableEntry, TextureId, TextureImage, TextureSpec,
};
use proptest::prelude::*;

fn def(name: &str, model: BlockModel) -> BlockDef {
    BlockDef {
        name: name.to_string(),
        shape: RenderShape::Cutout,
        model,
        collision: BlockCollision::None,
        emission: [0; 3],
        light_opacity: 0,
        textures: FaceTextures::all(TextureId(1)),
    }
}

fn near(a: [f32; 3], b: [f32; 3]) -> bool {
    (0..3).all(|k| (a[k] - b[k]).abs() < 1e-5)
}

/// The corners as (position, texel) pairs.
fn pairs(corners: [PlaneCorner; 4]) -> Vec<([f32; 3], [f32; 2])> {
    corners.iter().map(|c| (c.position, c.texel)).collect()
}

/// True if `a` and `b` hold the same corners in any order.
fn same(a: &[([f32; 3], [f32; 2])], b: &[([f32; 3], [f32; 2])]) -> bool {
    a.len() == b.len()
        && a.iter().all(|(p, t)| {
            b.iter().any(|(q, s)| {
                near(*p, *q) && (t[0] - s[0]).abs() < 1e-5 && (t[1] - s[1]).abs() < 1e-5
            })
        })
}

#[test]
fn the_full_cell_cross_is_the_cross_plant() {
    // The PosX cross quad runs from (0, 0) to (1, 1) in (x, z), the PosZ one from (1, 0) to
    // (0, 1); corner k at (u, v) = (k & 1, k >> 1), the texture's top row at the top.
    let [rising, falling] = ModelPlane::cross([0; 3], [32; 3]);
    let r = rising.corners();
    let f = falling.corners();
    for (corners, x0, dx) in [(r, 0.0, 1.0), (f, 1.0, -1.0)] {
        for (k, c) in corners.iter().enumerate() {
            let (u, v) = ((k & 1) as f32, (k >> 1) as f32);
            assert!(near(c.position, [x0 + dx * u, v, u]), "{k}: {c:?}");
            assert_eq!(c.texel, [16.0 * u, 16.0 * (1.0 - v)], "{k}");
        }
    }
}

#[test]
fn planes_lie_where_their_kind_says() {
    let lo = [4, 8, 12];
    let hi = [20, 24, 28];
    let at = |kind| {
        ModelPlane::new(kind, lo, hi)
            .corners()
            .map(|c| c.position.map(|x| x * 32.0))
    };
    // A mid plane across X sits at x = 12, spanning z (u) and y (v).
    let mid = at(PlaneKind::Mid(Axis::X));
    assert_eq!(
        mid,
        [
            [12.0, 8.0, 12.0],
            [12.0, 8.0, 28.0],
            [12.0, 24.0, 12.0],
            [12.0, 24.0, 28.0]
        ]
    );
    // Across Y it lies at y = 16, its v along +Z.
    let flat = at(PlaneKind::Mid(Axis::Y));
    assert_eq!(flat[2], [4.0, 16.0, 28.0]);
    // Rising along X: up the slope as z grows (a ramp).
    let ramp = at(PlaneKind::Rising(Axis::X));
    assert_eq!(
        ramp,
        [
            [4.0, 8.0, 12.0],
            [20.0, 8.0, 12.0],
            [4.0, 24.0, 28.0],
            [20.0, 24.0, 28.0]
        ]
    );
    // Falling along X: up the slope as z falls.
    let back = at(PlaneKind::Falling(Axis::X));
    assert_eq!(back[0], [4.0, 8.0, 28.0]);
    assert_eq!(back[2], [4.0, 24.0, 12.0]);
    // Along Z: u along +Z, v up the slope in x.
    let side = at(PlaneKind::Rising(Axis::Z));
    assert_eq!(side[1], [4.0, 8.0, 28.0]);
    assert_eq!(side[2], [20.0, 24.0, 12.0]);
    let other = at(PlaneKind::Falling(Axis::Z));
    assert_eq!(other[0], [20.0, 8.0, 12.0]);
    assert_eq!(other[3], [4.0, 24.0, 28.0]);
}

#[test]
fn textures_crop_mirror_and_turn() {
    let plane = ModelPlane::new(PlaneKind::Mid(Axis::Z), [0; 3], [32; 3]).with_uv([3, 0, 0, 16]);
    let t = plane.corners().map(|c| c.texel);
    // Backward u mirrors: the left (u = 0) shows texel column 3.
    assert_eq!(t, [[3.0, 16.0], [0.0, 16.0], [3.0, 0.0], [0.0, 0.0]]);
    // A quarter turn clockwise: the top left corner shows what the bottom left showed.
    let turned = plane
        .with_uv([0, 0, 16, 16])
        .with_turn(1)
        .corners()
        .map(|c| c.texel);
    assert_eq!(turned[2], [0.0, 16.0]);
    assert_eq!(turned[3], [0.0, 0.0]);
}

#[test]
fn a_spun_plane_turns_about_its_center() {
    let square = ModelPlane::new(PlaneKind::Mid(Axis::Z), [8, 8, 16], [24, 24, 16]);
    let corners = square.spun(2).corners();
    // 45 degrees: the corners land on the axes through the center, as far as the half diagonal.
    let r = 8.0 * std::f32::consts::SQRT_2 / 32.0;
    let c = 0.5;
    let wanted = [[c, c - r, c], [c + r, c, c], [c - r, c, c], [c, c + r, c]];
    for (k, corner) in corners.iter().enumerate() {
        assert!(near(corner.position, wanted[k]), "{k}: {corner:?}");
    }
    // A quarter turn is exact, its texture with it.
    let quarter = square.spun(4).corners();
    assert_eq!(quarter[0].position, [24.0 / 32.0, 8.0 / 32.0, 0.5]);
    assert_eq!(quarter[0].texel, square.corners()[0].texel);
}

fn kind() -> impl Strategy<Value = PlaneKind> {
    (0u8..9).prop_map(|c| PlaneKind::from_code(c).expect("code"))
}

fn plane() -> impl Strategy<Value = ModelPlane> {
    (
        kind(),
        [0u8..32, 0u8..32, 0u8..32],
        [1u8..33, 1u8..33, 1u8..33],
        [0u8..17, 0u8..17, 0u8..17, 0u8..17],
        0u8..4,
        0u8..16,
        0u8..4,
    )
        .prop_filter_map("a valid plane", |(kind, a, b, uv, turn, spin, quarter)| {
            let min = [0, 1, 2].map(|k| a[k].min(b[k]));
            let max = [0, 1, 2].map(|k| a[k].max(b[k]));
            let p = ModelPlane::new(kind, min, max)
                .with_uv(uv)
                .with_turn(turn)
                .spun(spin)
                .turned(quarter);
            p.is_valid().then_some(p)
        })
}

/// A point turned the way `ModelPlane::turned`, `turned_x` or `turned_z` turn (`axis` 0, 1, 2
/// for X, Y, Z), once.
fn turn_point(axis: usize, [x, y, z]: [f32; 3]) -> [f32; 3] {
    match axis {
        0 => [x, z, 1.0 - y],
        1 => [1.0 - z, y, x],
        _ => [y, 1.0 - x, z],
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// A turned plane is the same quad turned: each turned corner shows the texel it showed,
    /// four quarter turns give the plane back, and the box turns as a model box does.
    #[test]
    fn planes_turn_with_their_model(p in plane(), axis in 0usize..3, turns in 1u8..4) {
        let turn = |p: ModelPlane, n| match axis {
            0 => p.turned_x(n),
            1 => p.turned(n),
            _ => p.turned_z(n),
        };
        let turned = turn(p, turns);
        prop_assert!(turned.is_valid());
        let mut wanted = pairs(p.corners());
        for _ in 0..turns {
            for c in &mut wanted {
                c.0 = turn_point(axis, c.0);
            }
        }
        prop_assert!(same(&pairs(turned.corners()), &wanted), "{p:?} -> {turned:?}");
        prop_assert!(same(&pairs(turn(p, 4).corners()), &pairs(p.corners())));
        prop_assert_eq!(turned.texture, p.texture);
    }
}

#[test]
fn a_facing_turns_the_quad_and_keeps_the_entry() {
    let plane =
        ModelPlane::new(PlaneKind::Rising(Axis::X), [0, 4, 2], [32, 20, 30]).with_uv([2, 0, 14, 9]);
    for k in 0..4u8 {
        let turned = plane.turned(k);
        assert_eq!(turned.entry(), plane.entry(), "{k}");
        assert_eq!(
            turned.quad_face(),
            boxel_voxel::QUARTER_FACES[usize::from(k)]
        );
        // The same quad as the plane turned with its box.
        assert!(
            same(&pairs(turned.corners()), &pairs(turned.baked().corners())),
            "{k}"
        );
        assert_eq!(turned.baked().quarter, 0);
    }
    // A quarter turn takes the plane's low west end to the north.
    let c = plane.turned(1).corners();
    assert!(
        near(c[0].position, [1.0 - 2.0 / 32.0, 4.0 / 32.0, 0.0]),
        "{:?}",
        c[0]
    );
}

#[test]
fn planes_count_in_the_box_table_and_a_cross_shares_its_entry() {
    let mut b = testing::builder();
    let wax = b
        .add_texture(TextureSpec {
            name: "test:wick".to_string(),
            image: TextureImage::solid(testing::TEXTURE_SIZE, [9, 9, 9, 255]),
        })
        .expect("layer");
    let body = ModelBox::new([7, 0, 7], [9, 6, 9]);
    let wick = ModelPlane::cross([15, 12, 15], [17, 14, 17])
        .map(|p| p.with_uv([0, 5, 1, 6]).with_texture(wax));
    let model = BoxModel::from_boxes(vec![body]).with_planes(wick.to_vec());
    let id = b.add_box_model(model.clone()).expect("model");
    b.add_block(def("test:candle", BlockModel::Boxes(id)))
        .expect("candle");
    let r = b.build();
    let s = r.by_name("test:candle").expect("state");
    assert_eq!(r.model_planes(s), &wick);
    assert!(r.drawn_textures(s).any(|t| t == wax));
    // The two planes agree on their box and rectangle: one entry with the body's.
    let mut entries: Vec<TableEntry> = model.table_entries().collect();
    entries.dedup();
    assert_eq!(
        entries,
        [TableEntry::Box(body), TableEntry::Plane(wick[0].entry())]
    );
    // Rounded out to sixteenths, the plane's entry holds its quads.
    assert_eq!(entries[1].bounds(), CollisionBox::new([7, 6, 7], [9, 7, 9]));
    // The turned model turns its planes.
    let turned = model.turned_x(1);
    assert_eq!(turned.planes, wick.map(|p| p.turned_x(1)).to_vec());
}

#[test]
fn invalid_planes_and_unknown_layers_are_refused() {
    let mut b = testing::builder();
    let flat = ModelPlane::new(PlaneKind::Rising(Axis::Y), [0, 0, 4], [32, 32, 4]);
    let refused = b.add_box_model(BoxModel::default().with_planes(vec![flat]));
    assert!(
        matches!(refused, Err(RegistryError::InvalidCollision(_))),
        "{refused:?}"
    );
    let past = ModelPlane::new(PlaneKind::Mid(Axis::Y), [0; 3], [33, 4, 32]);
    assert!(
        b.add_box_model(BoxModel::default().with_planes(vec![past]))
            .is_err()
    );
    let unknown =
        ModelPlane::new(PlaneKind::Mid(Axis::Y), [0; 3], [32, 4, 32]).with_texture(TextureId(4000));
    let refused = b.add_box_model(BoxModel::default().with_planes(vec![unknown]));
    assert!(
        matches!(refused, Err(RegistryError::UnknownTexture(4000))),
        "{refused:?}"
    );
    // A mid plane may be flat across its axis (Minecraft's planes are).
    let flat_mid = ModelPlane::new(PlaneKind::Mid(Axis::Z), [0, 0, 4], [32, 32, 4]);
    assert!(
        b.add_box_model(BoxModel::default().with_planes(vec![flat_mid]))
            .is_ok()
    );
}

#[test]
fn the_box_table_cap_counts_planes() {
    let mut b = testing::builder();
    // Distinct plane entries fill the table up to MAX_MODEL_BOXES: then a new one is refused,
    // and a block reusing an entry is not.
    let mut added = 0;
    let mut refused = None;
    'fill: for y in 0..32u8 {
        for z in 0..33u8 {
            for u in 0..17u8 {
                let p = ModelPlane::new(PlaneKind::Mid(Axis::Z), [0, y, z], [32, y + 1, z])
                    .with_uv([u, 0, 16, 16]);
                let id = b
                    .add_box_model(BoxModel::default().with_planes(vec![p]))
                    .expect("model");
                match b.add_block(def(&format!("test:p{added}"), BlockModel::Boxes(id))) {
                    Ok(_) => added += 1,
                    Err(e) => {
                        refused = Some((e, p));
                        break 'fill;
                    }
                }
            }
        }
    }
    let (error, _) = refused.expect("the table fills");
    assert!(
        matches!(error, RegistryError::Full("model boxes")),
        "{error:?}"
    );
    assert!(
        added > MAX_MODEL_BOXES - 100 && added <= MAX_MODEL_BOXES,
        "{added}"
    );
    let again = ModelPlane::new(PlaneKind::Mid(Axis::Z), [0, 0, 0], [32, 1, 0]);
    let id = b
        .add_box_model(BoxModel::default().with_planes(vec![again]))
        .expect("model");
    assert!(
        b.add_block(def("test:again", BlockModel::Boxes(id)))
            .is_ok()
    );
}
