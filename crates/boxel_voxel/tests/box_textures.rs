//! A texture per box face (EW-BOXTEX, ADR 0045): the textures live on the `BoxModel`, crops
//! count in the model box table, and boxes and models turn around X, Y and Z.

use boxel_voxel::testing;
use boxel_voxel::{
    BlockCollision, BlockDef, BlockModel, BoxFaceTexture, BoxModel, Face, FaceTextures, ModelBox,
    RegistryError, RenderShape, TextureId, TextureImage, TextureSpec,
};

/// A cutout box block `name` of `model` with every face `test:solid`'s layer (1).
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

fn layer(name: &str) -> TextureSpec {
    TextureSpec {
        name: name.to_string(),
        image: TextureImage::solid(testing::TEXTURE_SIZE, [1, 2, 3, 255]),
    }
}

#[test]
fn box_textures_are_per_model_and_counted_in_the_box_table() {
    let mut b = testing::builder();
    let wax = b.add_texture(layer("test:wax")).expect("layer");
    let post = ModelBox::new([7, 0, 7], [9, 6, 9]);
    let mut faces = [BoxFaceTexture::layer(wax).at(0, 8); 6];
    faces[Face::NegY.index()] = BoxFaceTexture::HIDDEN;
    let plain = b
        .add_box_model(BoxModel::from_boxes(vec![post]))
        .expect("plain");
    let waxed = b
        .add_box_model(BoxModel::from_boxes(vec![post]).with_textures(vec![faces]))
        .expect("waxed");
    assert_ne!(plain, waxed, "the textures tell two models apart");
    b.add_block(def("test:plain_post", BlockModel::Boxes(plain)))
        .expect("plain post");
    b.add_block(def("test:waxed_post", BlockModel::Boxes(waxed)))
        .expect("waxed post");
    let r = b.build();
    let s = r.by_name("test:waxed_post").expect("state");
    assert_eq!(r.box_textures(s), &[faces]);
    assert!(
        r.box_textures(r.by_name("test:plain_post").expect("state"))
            .is_empty()
    );
    // The face textures and the box layer, in that order.
    let drawn: Vec<TextureId> = r.drawn_textures(s).collect();
    assert_eq!(drawn, [vec![TextureId(1); 6], vec![wax; 5]].concat());
    // ±X and ±Z crop with two entries; the hidden bottom takes none (the plain post has the
    // plain box).
    let model = r.box_model(waxed).expect("model");
    let entries: std::collections::HashSet<ModelBox> = model.table_boxes().collect();
    assert_eq!(entries.len(), 3, "{entries:?}");
    assert!(!entries.contains(&post));
}

#[test]
fn box_models_with_bad_textures_are_refused() {
    let mut b = testing::builder();
    let post = ModelBox::new([7, 0, 7], [9, 6, 9]);
    let unknown = [BoxFaceTexture::layer(TextureId(999)); 6];
    assert_eq!(
        b.add_box_model(BoxModel::from_boxes(vec![post]).with_textures(vec![unknown])),
        Err(RegistryError::UnknownTexture(999))
    );
    // One per box or none.
    let two = vec![[BoxFaceTexture::BLOCK; 6]; 2];
    assert!(
        b.add_box_model(BoxModel::from_boxes(vec![post]).with_textures(two))
            .is_err()
    );
    let outside = [BoxFaceTexture::BLOCK.at(16, 0); 6];
    assert!(
        b.add_box_model(BoxModel::from_boxes(vec![post]).with_textures(vec![outside]))
            .is_err()
    );
}

#[test]
fn a_crop_offset_shows_its_texel_at_the_face_corner() {
    let b = ModelBox::new([7, 0, 7], [9, 6, 9]);
    // Side: u = x + offset, row = 16 - (y + offset): at x = 7 and y = 6, texel (0, 8).
    let south = b.with_face_uv(Face::PosZ, [0, 8]);
    assert_eq!((7 + south.texture_offset[0]) % 16, 0);
    assert_eq!(16 - (6 + south.texture_offset[1]), 8);
    // ±X: u along z.
    let east = b.with_face_uv(Face::PosX, [2, 8]);
    assert_eq!((7 + east.texture_offset[2]) % 16, 2);
    // Top: v along +z, row = z + offset.
    let top = b.with_face_uv(Face::PosY, [0, 6]);
    assert_eq!(
        (
            (7 + top.texture_offset[0]) % 16,
            (7 + top.texture_offset[2]) % 16
        ),
        (0, 6)
    );
    assert!(top.is_valid() && south.is_valid() && east.is_valid());
}

#[test]
fn boxes_and_faces_turn_alike_around_every_axis() {
    // A thin plate on each face turns onto the face its normal turns to.
    let plate = |f: Face| {
        let mut min = [4, 4, 4];
        let mut max = [12, 12, 12];
        let (n, _, _) = f.axes();
        if f.index().is_multiple_of(2) {
            (min[n], max[n]) = (15, 16);
        } else {
            (min[n], max[n]) = (0, 1);
        }
        ModelBox::new(min, max)
    };
    type Turn = fn(ModelBox, u8) -> ModelBox;
    type FaceTurn = fn(Face, u8) -> Face;
    let axes: [(Turn, FaceTurn); 3] = [
        (ModelBox::turned_x, Face::turned_x),
        (ModelBox::turned, Face::turned_y),
        (ModelBox::turned_z, Face::turned_z),
    ];
    for (turn, face_turn) in axes {
        for f in Face::ALL {
            for k in 0..4 {
                assert_eq!(turn(plate(f), k), plate(face_turn(f, k)), "{f:?} {k}");
            }
            assert_eq!(face_turn(f, 4), f);
        }
    }
    // Minecraft's x = 90: the top to the north.
    assert_eq!(Face::PosY.turned_x(1), Face::NegZ);
    assert_eq!(Face::PosY.turned_z(1), Face::PosX);
    let rod = ModelBox::new([6, 0, 6], [10, 16, 10]);
    assert_eq!(rod.turned_x(1), ModelBox::new([6, 6, 0], [10, 10, 16]));
    assert_eq!(rod.turned_z(1), ModelBox::new([0, 6, 6], [16, 10, 10]));
    // Offsets turn with the box and four turns give it back.
    let mut lifted = ModelBox::new([7, 3, 7], [9, 13, 9]);
    lifted.texture_offset = [1, -3, 2];
    assert_eq!(lifted.turned_x(4), lifted);
    assert_eq!(lifted.turned_z(4), lifted);
    assert_eq!(lifted.turned_x(1).texture_offset, [1, 2, 3]);
    assert_eq!(lifted.turned_z(1).texture_offset, [-3, -1, 2]);
}

#[test]
fn a_turned_model_moves_its_face_textures() {
    let rod = ModelBox::new([6, 0, 6], [10, 16, 10]);
    let mut faces = [BoxFaceTexture::BLOCK; 6];
    faces[Face::PosY.index()] = BoxFaceTexture::layer(TextureId(7));
    let model = BoxModel::from_boxes(vec![rod]).with_textures(vec![faces]);
    let north = model.turned_x(1);
    assert_eq!(north.boxes, [rod.turned_x(1)]);
    assert_eq!(north.outline, [rod.turned_x(1).bounds]);
    assert_eq!(
        north.face_texture(0, Face::NegZ).texture,
        Some(TextureId(7))
    );
    assert_eq!(north.face_texture(0, Face::PosY), BoxFaceTexture::BLOCK);
    let east = model.turned_z(1);
    assert_eq!(east.face_texture(0, Face::PosX).texture, Some(TextureId(7)));
    let south = model.turned_x(1).turned(2);
    assert_eq!(
        south.face_texture(0, Face::PosZ).texture,
        Some(TextureId(7))
    );
    // A model without textures keeps none.
    assert!(
        BoxModel::from_boxes(vec![rod])
            .turned_x(1)
            .textures
            .is_empty()
    );
}
