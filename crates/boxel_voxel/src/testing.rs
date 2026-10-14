//! A minimal block set for engine tests (#48): one block of each kind the engine handles, with
//! `test:` names, so a crate can test its mechanisms without a game's content. Built with the
//! crate's own tests or the `testing` feature.
//!
//! | Block | What it is |
//! |---|---|
//! | `test:solid` | an opaque full cube |
//! | `test:glass` | a translucent cube light passes |
//! | `test:lamp` | an opaque cube that emits light, drawn full bright |
//! | `test:slab` | a bottom slab, `waterlogged` |
//! | `test:boxes` | two boxes (a post and a bar), `waterlogged` |
//! | `test:plant` | a cross plant, cutout, no collision |
//! | `test:fluid` | a translucent fluid with a flow layer; the waterlogged blocks hold it |
//! | `test:hot` | an opaque fluid that emits light and hardens into `test:solid` beside `test:fluid` |
//! | `test:cutout` | a cutout full cube (leaves), with the plant's layer |
//!
//! Every layer is a flat [`TEXTURE_SIZE`]^2 image; the plant's has binary alpha.
use crate::{
    BlockCollision, BlockDef, BlockModel, BlockRegistry, BlockRegistryBuilder, BoxModel,
    CollisionBox, FLUID_LEVEL, Face, FaceTextures, FluidHardening, FluidRules, ModelBox,
    OPAQUE_OPACITY, Property, RenderShape, SlabHalf, TextureId, TextureImage, TextureSpec,
    WATERLOGGED,
};

/// Edge of the test layers.
pub const TEXTURE_SIZE: u32 = 16;

/// How `test:fluid` flows: a short slope, infinite.
pub const FLUID: FluidRules = FluidRules {
    delay: 5,
    slope_find_distance: 4,
    drop_off: 1,
    infinite: true,
};

/// How `test:hot` flows: slow, a shorter reach, not infinite.
pub const HOT: FluidRules = FluidRules {
    delay: 30,
    slope_find_distance: 2,
    drop_off: 2,
    infinite: false,
};

/// The light `test:lamp` and `test:hot` emit.
pub const EMISSION: [u8; 3] = [15, 12, 6];

/// A builder holding the test blocks, to add more after them.
pub fn builder() -> BlockRegistryBuilder {
    let mut b = BlockRegistryBuilder::new();
    let mut layer = |name: &str, color: [u8; 4], binary_alpha: bool| -> TextureId {
        let mut image = TextureImage::solid(TEXTURE_SIZE, color);
        if binary_alpha {
            // every other column clear
            let edge = TEXTURE_SIZE as usize;
            let texels: Vec<[u8; 4]> = (0..edge * edge)
                .map(|i| if i % 2 == 0 { color } else { [0; 4] })
                .collect();
            image = TextureImage {
                texels: texels.into(),
                binary_alpha: true,
                ..image
            };
        }
        b.add_texture(TextureSpec {
            name: name.to_string(),
            image,
        })
        .expect("test layer")
    };
    let solid = layer("test:solid", [128, 128, 128, 255], false);
    let glass = layer("test:glass", [200, 220, 240, 64], false);
    let lamp = layer("test:lamp", [250, 220, 120, 255], false);
    let plant = layer("test:plant", [60, 160, 40, 255], true);
    let fluid = layer("test:fluid", [40, 90, 200, 160], false);
    let fluid_flow = layer("test:fluid_flow", [50, 100, 210, 160], false);
    let hot = layer("test:hot", [210, 80, 20, 255], false);
    let hot_flow = layer("test:hot_flow", [220, 90, 30, 255], false);

    let cube = |name: &str, shape, textures| BlockDef {
        name: name.to_string(),
        shape,
        model: BlockModel::CUBE,
        collision: BlockCollision::Full,
        emission: [0; 3],
        light_opacity: OPAQUE_OPACITY,
        textures,
    };
    let all = FaceTextures::all;
    let solid_state = b
        .add_block(cube("test:solid", RenderShape::Opaque, all(solid)))
        .expect("test:solid");
    b.add_block(BlockDef {
        light_opacity: 0,
        ..cube("test:glass", RenderShape::Translucent, all(glass))
    })
    .expect("test:glass");
    b.add_block(BlockDef {
        emission: EMISSION,
        ..cube("test:lamp", RenderShape::Opaque, all(lamp))
    })
    .expect("test:lamp");
    let slab = BlockModel::Slab(SlabHalf::Bottom);
    b.add_block_with_states(
        BlockDef {
            model: slab,
            collision: slab.default_collision(),
            light_opacity: 1,
            ..cube("test:slab", RenderShape::Opaque, all(solid))
        },
        vec![Property::boolean(WATERLOGGED)],
        |_, _| {},
    )
    .expect("test:slab");
    let boxes = [
        ModelBox::new([6, 0, 6], [10, 16, 10]),
        ModelBox::new([0, 6, 7], [16, 9, 9]),
    ];
    let model = b
        .add_box_model(BoxModel::from_boxes(boxes.to_vec()))
        .expect("test:boxes");
    b.add_block_with_states(
        BlockDef {
            model: BlockModel::Boxes(model),
            collision: BlockCollision::Boxes(boxes.map(|b| b.bounds).to_vec()),
            light_opacity: 0,
            ..cube("test:boxes", RenderShape::Opaque, all(solid))
        },
        vec![Property::boolean(WATERLOGGED)],
        |_, _| {},
    )
    .expect("test:boxes");
    b.add_block(BlockDef {
        model: BlockModel::Cross,
        collision: BlockCollision::None,
        light_opacity: 0,
        ..cube("test:plant", RenderShape::Cutout, all(plant))
    })
    .expect("test:plant");
    let fluid_block = |name: &str, shape, still, flow| BlockDef {
        model: BlockModel::Fluid,
        collision: BlockCollision::None,
        light_opacity: 1,
        textures: FaceTextures {
            top: still,
            bottom: still,
            side: flow,
        },
        ..cube(name, shape, all(still))
    };
    let levels = || vec![Property::int(FLUID_LEVEL, 0..=15)];
    b.add_block_with_states(
        fluid_block("test:fluid", RenderShape::Translucent, fluid, fluid_flow),
        levels(),
        |_, _| {},
    )
    .expect("test:fluid");
    b.add_block_with_states(
        BlockDef {
            emission: EMISSION,
            ..fluid_block("test:hot", RenderShape::Opaque, hot, hot_flow)
        },
        levels(),
        |_, _| {},
    )
    .expect("test:hot");
    b.add_block(BlockDef {
        light_opacity: 1,
        ..cube("test:cutout", RenderShape::Cutout, all(plant))
    })
    .expect("test:cutout");

    let block = |b: &BlockRegistryBuilder, name: &str| b.block_by_name(name).expect(name);
    b.set_full_bright(block(&b, "test:lamp"), |_| true)
        .expect("test:lamp");
    let fluid = b
        .add_fluid(block(&b, "test:fluid"), FLUID)
        .expect("test:fluid");
    b.set_waterlog_fluid(fluid).expect("test:fluid");
    let hot = b.add_fluid(block(&b, "test:hot"), HOT).expect("test:hot");
    b.fluid_hardens(
        hot,
        FluidHardening {
            with: fluid,
            faces: Face::ALL.to_vec(),
            source: solid_state,
            flowing: solid_state,
        },
    )
    .expect("test:hot");
    b
}

/// The frozen test registry.
pub fn registry() -> BlockRegistry {
    builder().build()
}

/// The box bounds of `test:boxes`, for tests that compare its collision.
pub fn boxes_bounds() -> Vec<CollisionBox> {
    vec![
        CollisionBox::new([6, 0, 6], [10, 16, 10]),
        CollisionBox::new([0, 6, 7], [16, 9, 9]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockState, LightValue, StateFlags};

    #[test]
    fn the_test_blocks_build() {
        let r = registry();
        let by = |name: &str| r.parse_state(name).expect(name);
        let solid = by("test:solid");
        assert!(r.flags(solid).contains(StateFlags::SOLID));
        assert_eq!(r.textures().len(), 8);
        assert!(r.textures().iter().all(|t| t.image.size == TEXTURE_SIZE));
        let plant = r
            .textures()
            .iter()
            .find(|t| t.name == "test:plant")
            .expect("plant layer");
        assert!(plant.image.binary_alpha);
        let [red, green, blue] = EMISSION;
        assert_eq!(
            r.emission(by("test:lamp")),
            LightValue::new(0, red, green, blue)
        );
        assert!(r.is_full_bright(by("test:lamp")));
        assert!(r.is_full_bright(by("test:hot[level=3]")));
        assert!(!r.is_full_bright(solid) && !r.is_full_bright(by("test:fluid")));
        let wet = by("test:slab[waterlogged=true]");
        assert!(r.flags(wet).contains(StateFlags::WATERLOGGED));
        let boxes = by("test:boxes[waterlogged=true]");
        assert!(r.flags(boxes).contains(StateFlags::WATERLOGGED));
        assert_eq!(r.model_boxes(boxes).len(), 2);
        assert_eq!(r.collision(boxes), &BlockCollision::Boxes(boxes_bounds()));
        assert_eq!(r.model(by("test:plant")), BlockModel::Cross);
        let fluid = r.fluids();
        assert_eq!(fluid.len(), 2);
        let water = r.waterlog_fluid().expect("a waterlog fluid");
        assert_eq!(r.fluid_of(by("test:fluid")), Some((water, 0)));
        let hot = r.fluid_of(by("test:hot[level=3]")).expect("hot").0;
        assert_eq!(fluid[usize::from(hot.0)].hardens[0].source, solid);
        assert!(r.flags(by("test:fluid")).contains(StateFlags::FLUID));
        assert_ne!(by("test:hot"), BlockState::AIR);
        let cutout = by("test:cutout");
        assert_eq!(r.shape(cutout), RenderShape::Cutout);
        assert!(r.model(cutout).is_solid_cube());
        assert!(!r.occludes(cutout));
    }
}
