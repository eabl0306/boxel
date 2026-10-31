//! The quarter turns of a box share its model box table entry (ADR 0052): the entry a box
//! stands for ([`ModelBox::canonical`]) is the same for its four turns around Y, the registry
//! counts entries that way, and a box face cannot show a layer past what its quad holds.

use boxel_voxel::testing;
use boxel_voxel::{
    BlockCollision, BlockDef, BlockModel, BoxFaceTexture, BoxModel, CollisionBox, FaceTextures,
    MAX_BOX_FACE_LAYERS, MAX_MODEL_BOXES, ModelBox, RegistryError, RenderShape, SlabHalf,
    TextureId, TextureImage, TextureSpec,
};
use proptest::prelude::*;

/// A cutout block `name` of `model` with every face `texture`.
fn def(name: &str, model: BlockModel, texture: TextureId) -> BlockDef {
    BlockDef {
        name: name.to_string(),
        shape: RenderShape::Cutout,
        model,
        collision: BlockCollision::None,
        emission: [0; 3],
        light_opacity: 0,
        textures: FaceTextures::all(texture),
    }
}

/// Any valid box: bounds in the cell, offsets in `-32..32`.
fn any_box() -> impl Strategy<Value = ModelBox> {
    let axis = (0u8..16).prop_flat_map(|lo| (Just(lo), lo + 1..=16));
    (
        [axis.clone(), axis.clone(), axis],
        [-32i8..32, -32i8..32, -32i8..32],
    )
        .prop_map(|(axes, texture_offset)| ModelBox {
            bounds: CollisionBox::new(axes.map(|a| a.0), axes.map(|a| a.1)),
            texture_offset,
        })
}

proptest! {
    #[test]
    fn every_turn_of_a_box_stands_for_the_same_entry(b in any_box(), k in 0u8..4) {
        let (entry, turns) = b.canonical();
        prop_assert!(entry.is_valid());
        prop_assert!(turns < 4);
        prop_assert_eq!(entry.turned(turns), b);
        let turned = b.turned(k);
        if turned.is_valid() {
            prop_assert_eq!(turned.canonical().0, entry);
        }
    }
}

#[test]
fn a_box_its_turns_leave_alone_is_its_own_entry() {
    for b in [
        ModelBox::slab(SlabHalf::Bottom),
        ModelBox::slab(SlabHalf::Top),
        ModelBox::new([6, 0, 6], [10, 16, 10]),
    ] {
        assert_eq!(b.canonical(), (b, 0));
    }
    // A stair's step: the four facings share the entry, each with its own turns.
    let step = ModelBox::new([8, 8, 0], [16, 16, 16]);
    let (entry, _) = step.canonical();
    let turns: Vec<u8> = (0..4).map(|k| step.turned(k).canonical().1).collect();
    let mut sorted = turns.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, [0, 1, 2, 3], "{turns:?}");
    for k in 0..4 {
        assert_eq!(step.turned(k).canonical().0, entry);
    }
}

#[test]
fn a_turned_model_draws_with_the_entries_of_the_model() {
    let step = BoxModel::from_boxes(vec![
        ModelBox::slab(SlabHalf::Bottom),
        ModelBox {
            bounds: CollisionBox::new([8, 8, 0], [16, 16, 16]),
            texture_offset: [3, 0, -5],
        },
    ]);
    let entries: Vec<_> = step.table_entries().collect();
    for k in 1..4 {
        assert_eq!(step.turned(k).table_entries().collect::<Vec<_>>(), entries);
    }
    // Turned around X it is another entry (the quad holds turns around Y only).
    assert_ne!(
        step.turned_x(1).table_entries().collect::<Vec<_>>(),
        entries
    );
}

#[test]
fn the_registry_counts_the_turns_of_a_box_once() {
    // A registry with no box yet.
    let mut b = boxel_voxel::BlockRegistryBuilder::new();
    let image = TextureImage::solid(testing::TEXTURE_SIZE, [1, 2, 3, 255]);
    let texture = b
        .add_texture(TextureSpec {
            name: "test:solid".to_string(),
            image,
        })
        .expect("layer");
    // MAX_MODEL_BOXES distinct entries: posts of every width, height and place along X.
    let mut posts = Vec::new();
    'fill: for w in 1..=16u8 {
        for h in 1..=16u8 {
            for x in 0..=16 - w {
                if posts.len() == MAX_MODEL_BOXES {
                    break 'fill;
                }
                posts.push(ModelBox::new([x, 0, 0], [x + w, h, 1]));
            }
        }
    }
    assert_eq!(posts.len(), MAX_MODEL_BOXES);
    let distinct: std::collections::HashSet<ModelBox> =
        posts.iter().map(|p| p.canonical().0).collect();
    // Some posts are turns of others (a 1-wide post in a corner): keep one of each.
    let posts: Vec<ModelBox> = {
        let mut seen = std::collections::HashSet::new();
        posts
            .into_iter()
            .filter(|p| seen.insert(p.canonical().0))
            .collect()
    };
    assert_eq!(posts.len(), distinct.len());
    let model = |boxes: Vec<ModelBox>| BoxModel::from_boxes(boxes);
    for (i, chunk) in posts.chunks(64).enumerate() {
        let id = b.add_box_model(model(chunk.to_vec())).expect("model");
        b.add_block(def(
            &format!("test:posts_{i}"),
            BlockModel::Boxes(id),
            texture,
        ))
        .expect("posts");
    }
    // Their turns take no entry.
    for (i, chunk) in posts.chunks(64).enumerate() {
        for k in 1..4 {
            let turned: Vec<ModelBox> = chunk.iter().map(|p| p.turned(k)).collect();
            let id = b.add_box_model(model(turned)).expect("model");
            let name = format!("test:posts_{i}_turned_{k}");
            b.add_block(def(&name, BlockModel::Boxes(id), texture))
                .expect("the turns of counted boxes");
        }
    }
    // The table holds `distinct` entries: room is left only if the posts did not fill it.
    let room = MAX_MODEL_BOXES - distinct.len();
    let mut extra = Vec::new();
    for z in 1..=16u8 {
        for y in 1..=16u8 {
            extra.push(ModelBox::new([0, 0, 0], [16, y, z]));
        }
    }
    let extra: Vec<ModelBox> = extra
        .into_iter()
        .filter(|e| !distinct.contains(&e.canonical().0))
        .take(room + 1)
        .collect();
    let (fits, over) = extra.split_at(room);
    if !fits.is_empty() {
        let id = b.add_box_model(model(fits.to_vec())).expect("model");
        b.add_block(def("test:fits", BlockModel::Boxes(id), texture))
            .expect("the last entries");
    }
    let id = b.add_box_model(model(over.to_vec())).expect("model");
    assert_eq!(
        b.add_block(def("test:over", BlockModel::Boxes(id), texture)),
        Err(RegistryError::Full("model boxes"))
    );
}

#[test]
fn box_faces_show_only_the_layers_their_quad_holds() {
    let mut b = testing::builder();
    let image = TextureImage::solid(testing::TEXTURE_SIZE, [1, 2, 3, 255]);
    let mut last = TextureId(0);
    let mut n = 0;
    while usize::from(last.0) < MAX_BOX_FACE_LAYERS {
        n += 1;
        let name = format!("test:layer_{n}");
        last = b
            .add_texture(TextureSpec {
                name,
                image: image.clone(),
            })
            .expect("layer");
    }
    assert_eq!(usize::from(last.0), MAX_BOX_FACE_LAYERS);
    let fits = TextureId(last.0 - 1);
    let post = ModelBox::new([7, 0, 7], [9, 6, 9]);
    let faced = |t: TextureId| {
        BoxModel::from_boxes(vec![post]).with_textures(vec![[BoxFaceTexture::layer(t); 6]])
    };
    assert!(b.add_box_model(faced(fits)).is_ok());
    assert_eq!(
        b.add_box_model(faced(last)),
        Err(RegistryError::Full("box face layers"))
    );
    let plain = b
        .add_box_model(BoxModel::from_boxes(vec![post]))
        .expect("plain");
    assert!(
        b.add_block(def("test:post_fits", BlockModel::Boxes(plain), fits))
            .is_ok()
    );
    assert_eq!(
        b.add_block(def("test:post_past", BlockModel::Boxes(plain), last)),
        Err(RegistryError::Full("box face layers"))
    );
    assert_eq!(
        b.add_block(def(
            "test:slab_past",
            BlockModel::Slab(SlabHalf::Bottom),
            last
        )),
        Err(RegistryError::Full("box face layers"))
    );
    // A cube is not drawn with box faces: any layer.
    assert!(
        b.add_block(def("test:cube_past", BlockModel::CUBE, last))
            .is_ok()
    );
}
