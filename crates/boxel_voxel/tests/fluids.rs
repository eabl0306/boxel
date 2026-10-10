//! Fluid types declared in the registry (#48): which blocks can be fluids, the tables between
//! fluids, the waterlog fluid, and what the registry answers about each state.
use boxel_voxel::*;

fn texture(b: &mut BlockRegistryBuilder) -> TextureId {
    b.add_texture(TextureSpec {
        name: "test:t".into(),
        base_color: [0; 3],
        noise: 0,
        pattern: TexturePattern::Noise,
    })
    .unwrap()
}

fn def(name: &str, model: BlockModel, t: TextureId) -> BlockDef {
    BlockDef {
        name: name.into(),
        shape: RenderShape::Translucent,
        model,
        collision: BlockCollision::None,
        emission: [0; 3],
        light_opacity: 1,
        textures: FaceTextures::all(t),
    }
}

const RULES: FluidRules = FluidRules {
    delay: 5,
    slope_find_distance: 4,
    drop_off: 1,
    infinite: true,
};

/// `test:solid` (state 1), then a block with `properties` drawn as `model`.
fn builder(model: BlockModel, properties: Vec<Property>) -> (BlockRegistryBuilder, BlockId) {
    let mut b = BlockRegistryBuilder::new();
    let t = texture(&mut b);
    b.add_block(BlockDef {
        shape: RenderShape::Opaque,
        collision: BlockCollision::Full,
        light_opacity: 15,
        ..def("test:solid", BlockModel::CUBE, t)
    })
    .unwrap();
    b.add_block_with_states(def("test:fluid", model, t), properties, |_, _| {})
        .unwrap();
    let block = b.block_by_name("test:fluid").unwrap();
    (b, block)
}

fn levels() -> Vec<Property> {
    vec![Property::int(FLUID_LEVEL, 0..=15)]
}

#[test]
fn a_fluid_is_a_fluid_block_with_its_16_levels() {
    let (mut b, block) = builder(BlockModel::Fluid, levels());
    let id = b.add_fluid(block, RULES).unwrap();
    assert_eq!(id, FluidId(0));
    // Once only.
    assert_eq!(
        b.add_fluid(block, RULES),
        Err(RegistryError::InvalidFluid("test:fluid".into()))
    );
    assert_eq!(
        b.add_fluid(BlockId(9), RULES),
        Err(RegistryError::UnknownBlock(9))
    );
    let r = b.build();
    assert_eq!(r.fluids().len(), 1);
    let fluid = r.fluid(id).unwrap();
    assert_eq!(fluid.block, block);
    assert_eq!(fluid.first, BlockState(2));
    assert_eq!(fluid.rules, RULES);
    assert_eq!(fluid.rules.reach(), [5, 2, 1]);
    assert!(fluid.replaced_by.is_empty() && fluid.falls_into.is_empty());
    assert!(fluid.hardens.is_empty());
    assert!(r.fluid(FluidId(1)).is_none());
    assert_eq!(r.waterlog_fluid(), None);
    // Its states are its levels; nothing else is.
    for level in 0..16u8 {
        let state = r
            .parse_state(&format!("test:fluid[level={level}]"))
            .unwrap();
        assert_eq!(r.fluid_of(state), Some((id, level)));
    }
    for s in [
        BlockState::AIR,
        BlockState(1),
        BlockState(18),
        BlockState(u16::MAX),
    ] {
        assert_eq!(r.fluid_of(s), None, "{s:?}");
    }
}

#[test]
fn other_blocks_are_not_fluids() {
    let wrong = [
        // Not drawn as a fluid.
        builder(BlockModel::CUBE, levels()),
        // No level.
        builder(BlockModel::Fluid, Vec::new()),
        // Too few levels, another name, another property besides.
        builder(BlockModel::Fluid, vec![Property::int(FLUID_LEVEL, 0..=7)]),
        builder(BlockModel::Fluid, vec![Property::int("height", 0..=15)]),
        builder(
            BlockModel::Fluid,
            vec![
                Property::int(FLUID_LEVEL, 0..=15),
                Property::boolean(WATERLOGGED),
            ],
        ),
        // Its levels out of order.
        builder(
            BlockModel::Fluid,
            vec![Property::new(
                FLUID_LEVEL,
                (0..16).rev().map(|l: u8| l.to_string()),
            )],
        ),
    ];
    for (mut b, block) in wrong {
        assert_eq!(
            b.add_fluid(block, RULES),
            Err(RegistryError::InvalidFluid("test:fluid".into()))
        );
    }
}

#[test]
fn the_tables_between_fluids_name_known_fluids_and_states() {
    let (mut b, block) = builder(BlockModel::Fluid, levels());
    let t = TextureId(0);
    b.add_block_with_states(def("test:other", BlockModel::Fluid, t), levels(), |_, _| {})
        .unwrap();
    let other = b.block_by_name("test:other").unwrap();
    let a = b.add_fluid(block, RULES).unwrap();
    let c = b.add_fluid(other, RULES).unwrap();
    let solid = b.by_name("test:solid").unwrap();
    assert_eq!(solid, BlockState(1));
    let when = FluidReplace {
        min_amount: 4,
        only_down: false,
    };
    let unknown = FluidId(2);
    assert_eq!(
        b.fluid_replaced_by(a, unknown, when),
        Err(RegistryError::UnknownFluid(2))
    );
    assert_eq!(
        b.fluid_replaced_by(unknown, a, when),
        Err(RegistryError::UnknownFluid(2))
    );
    assert_eq!(
        b.fluid_falls_into(a, c, BlockState(999)),
        Err(RegistryError::InvalidFluid("test:fluid".into()))
    );
    assert_eq!(
        b.set_waterlog_fluid(unknown),
        Err(RegistryError::UnknownFluid(2))
    );
    let hardening = FluidHardening {
        with: c,
        faces: vec![Face::PosY],
        source: solid,
        flowing: BlockState(999),
    };
    assert_eq!(
        b.fluid_hardens(a, hardening.clone()),
        Err(RegistryError::InvalidFluid("test:fluid".into()))
    );
    b.fluid_replaced_by(a, c, when).unwrap();
    b.fluid_falls_into(c, a, solid).unwrap();
    let hardening = FluidHardening {
        flowing: solid,
        ..hardening
    };
    b.fluid_hardens(a, hardening.clone()).unwrap();
    // A second hardening adds to the first, after it.
    let below = FluidHardening {
        faces: vec![Face::NegY],
        ..hardening.clone()
    };
    b.fluid_hardens(a, below.clone()).unwrap();
    b.set_waterlog_fluid(c).unwrap();
    let r = b.build();
    assert_eq!(r.fluids()[0].replaced_by, [(c, when)]);
    assert_eq!(r.fluids()[1].falls_into, [(a, solid)]);
    assert_eq!(r.fluids()[0].hardens, [hardening, below]);
    assert!(r.fluids()[1].hardens.is_empty());
    assert_eq!(r.waterlog_fluid(), Some(c));
}

#[test]
fn a_registry_holds_at_most_16_fluids() {
    let mut b = BlockRegistryBuilder::new();
    let t = texture(&mut b);
    for i in 0..=MAX_FLUIDS {
        let name = format!("test:fluid{i}");
        b.add_block_with_states(def(&name, BlockModel::Fluid, t), levels(), |_, _| {})
            .unwrap();
        let block = b.block_by_name(&name).unwrap();
        let got = b.add_fluid(block, RULES);
        if i < MAX_FLUIDS {
            assert_eq!(got, Ok(FluidId(i as u8)));
        } else {
            assert_eq!(got, Err(RegistryError::Full("fluids")));
        }
    }
}

/// Only the fluid model makes a liquid ([`StateFlags::FLUID`]): a clear block without collision
/// is not one, and a fluid that collides is not either.
#[test]
fn only_the_fluid_model_is_a_liquid() {
    let mut b = BlockRegistryBuilder::new();
    let t = texture(&mut b);
    let mist = b.add_block(def("test:mist", BlockModel::CUBE, t)).unwrap();
    let liquid = b
        .add_block(def("test:liquid", BlockModel::Fluid, t))
        .unwrap();
    let opaque = b
        .add_block(BlockDef {
            shape: RenderShape::Opaque,
            ..def("test:magma", BlockModel::Fluid, t)
        })
        .unwrap();
    let hard = b
        .add_block(BlockDef {
            collision: BlockCollision::Full,
            ..def("test:ice", BlockModel::Fluid, t)
        })
        .unwrap();
    let r = b.build();
    let fluid = |s| r.flags(s).contains(StateFlags::FLUID);
    assert!(!fluid(mist), "a translucent cube without collision");
    assert!(fluid(liquid) && fluid(opaque));
    assert!(!fluid(hard), "a fluid model that collides");
}
