//! Block state properties (ADR 0019): ids, values, parsing and formatting.
use boxel_voxel::*;
use proptest::prelude::*;

fn texture(b: &mut BlockRegistryBuilder) -> TextureId {
    b.add_texture(TextureSpec {
        name: "test:t".into(),
        base_color: [0; 3],
        noise: 0,
        pattern: TexturePattern::Noise,
    })
    .unwrap()
}

fn cube(name: &str, t: TextureId) -> BlockDef {
    BlockDef {
        name: name.into(),
        shape: RenderShape::Opaque,
        model: BlockModel::CUBE,
        collision: BlockCollision::Full,
        emission: [0; 3],
        light_opacity: 15,
        textures: FaceTextures::all(t),
    }
}

/// A builder with `count` blocks after air; block `i` has properties `p0..` with
/// `sizes[i][j]` values `v0..`, each defaulting to `defaults[i][j] % size`.
fn registry(sizes: &[Vec<usize>], defaults: &[Vec<usize>]) -> BlockRegistry {
    let mut b = BlockRegistryBuilder::new();
    let t = texture(&mut b);
    for (i, block) in sizes.iter().enumerate() {
        let properties = block
            .iter()
            .enumerate()
            .map(|(j, &n)| {
                let p = Property::new(format!("p{j}"), (0..n).map(|v| format!("v{v}")));
                let default = defaults[i][j] % n;
                p.with_default(&format!("v{default}"))
            })
            .collect();
        // The light opacity follows the first property, to see per-state data land in its state.
        b.add_block_with_states(cube(&format!("test:b{i}"), t), properties, |values, def| {
            if values.index("p0") == Some(1) {
                def.shape = RenderShape::Cutout;
                def.light_opacity = 1;
            }
        })
        .unwrap();
    }
    b.build()
}

fn schema() -> impl Strategy<Value = (Vec<Vec<usize>>, Vec<Vec<usize>>)> {
    prop::collection::vec(prop::collection::vec(1usize..6, 0..4), 1..5).prop_flat_map(|sizes| {
        let defaults = sizes
            .iter()
            .map(|b| prop::collection::vec(0usize..6, b.len()))
            .collect::<Vec<_>>();
        (Just(sizes), defaults)
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Every state of every block: its values give it back (through `with` from the default and
    /// through parsing its string), its string parses back to it, and the per-state data is the
    /// one its values asked for.
    #[test]
    fn state_ids_values_and_strings_round_trip((sizes, defaults) in schema()) {
        let r = registry(&sizes, &defaults);
        let total: usize = 1 + sizes.iter().map(|b| b.iter().product::<usize>()).sum::<usize>();
        prop_assert_eq!(r.len(), total);
        let mut next = 1u16;
        for (i, block_sizes) in sizes.iter().enumerate() {
            let name = format!("test:b{i}");
            let block = r.block_by_name(&name).unwrap();
            prop_assert_eq!(r.block_name(block), name.as_str());
            let states: Vec<BlockState> = r.states(block).collect();
            prop_assert_eq!(states.len(), block_sizes.iter().product::<usize>());
            // Contiguous, in registration order.
            prop_assert_eq!(states[0], BlockState(next));
            next += states.len() as u16;
            let default = r.default_state(block);
            prop_assert_eq!(r.by_name(&name), Some(default));
            for (j, &n) in block_sizes.iter().enumerate() {
                let want = format!("v{}", defaults[i][j] % n);
                prop_assert_eq!(r.property(default, &format!("p{j}")), Some(want.as_str()));
            }
            for (k, &s) in states.iter().enumerate() {
                prop_assert_eq!(r.block(s), Some(block));
                prop_assert_eq!(r.name(s), name.as_str());
                let text = r.state_name(s).to_string();
                prop_assert_eq!(r.parse_state(&text), Ok(s), "{}", text);
                // Rebuild from the default with `with`, one property at a time.
                let mut rebuilt = default;
                for (p, v) in r.values(s).iter() {
                    rebuilt = r.with(rebuilt, p, v).unwrap();
                }
                prop_assert_eq!(rebuilt, s);
                // The last property varies fastest.
                if let Some(&last) = block_sizes.last() {
                    let p = format!("p{}", block_sizes.len() - 1);
                    prop_assert_eq!(r.values(s).index(&p), Some(k % last));
                }
                let cutout = r.values(s).index("p0") == Some(1);
                prop_assert_eq!(r.light_opacity(s), if cutout { 1 } else { 15 });
                prop_assert_eq!(r.occludes(s), !cutout);
                prop_assert!(r.with(s, "nope", "v0").is_none());
                prop_assert!(r.with(s, "p0", "nope").is_none());
            }
        }
    }

    /// Any order of the properties in the string, and any subset (the rest default).
    #[test]
    fn parsing_takes_any_order_and_defaults_the_rest(
        (sizes, defaults) in schema(),
        pick in any::<prop::sample::Index>(),
        keep in any::<u8>(),
        reverse in any::<bool>(),
    ) {
        let r = registry(&sizes, &defaults);
        let s = BlockState(pick.index(r.len() - 1) as u16 + 1);
        let block = r.block(s).unwrap();
        let mut pairs: Vec<(String, String)> = r
            .values(s)
            .iter()
            .enumerate()
            .filter(|&(j, _)| keep & (1 << j) != 0)
            .map(|(_, (p, v))| (p.to_string(), v.to_string()))
            .collect();
        if reverse {
            pairs.reverse();
        }
        let body: Vec<String> = pairs.iter().map(|(p, v)| format!("{p}={v}")).collect();
        let text = format!("{}[{}]", r.name(s), body.join(","));
        let mut want = r.default_state(block);
        for (p, v) in &pairs {
            want = r.with(want, p, v).unwrap();
        }
        prop_assert_eq!(r.parse_state(&text), Ok(want), "{}", text);
    }
}

#[test]
fn parse_errors_name_the_problem() {
    let r = boxel_demo::blocks::registry();
    let slab = |t: &str| r.parse_state(&format!("boxel:stone_slab{t}"));
    assert_eq!(slab(""), Ok(BlockState(50)));
    assert_eq!(slab("[]"), Ok(BlockState(50)));
    assert_eq!(slab("[type=top]"), Ok(BlockState(52)));
    assert_eq!(slab("[ type = double ]"), Ok(BlockState(54)));
    assert_eq!(slab("[waterlogged=true]"), Ok(BlockState(51)));
    assert_eq!(
        r.parse_state("boxel:nope[type=top]"),
        Err(StateParseError::UnknownBlock("boxel:nope".into()))
    );
    assert_eq!(
        slab("[axis=x]"),
        Err(StateParseError::UnknownProperty {
            block: "boxel:stone_slab".into(),
            property: "axis".into(),
        })
    );
    assert_eq!(
        slab("[type=middle]"),
        Err(StateParseError::UnknownValue {
            block: "boxel:stone_slab".into(),
            property: "type".into(),
            value: "middle".into(),
        })
    );
    assert_eq!(
        slab("[type=top,type=bottom]"),
        Err(StateParseError::Repeated {
            block: "boxel:stone_slab".into(),
            property: "type".into(),
        })
    );
    for bad in ["[type=top", "[type]", "[=top]", "[type=]", "[type=top,]"] {
        assert!(
            matches!(slab(bad), Err(StateParseError::Syntax(_))),
            "{bad}: {:?}",
            slab(bad)
        );
    }
    // A block without properties has none to set.
    assert!(matches!(
        r.parse_state("boxel:stone[axis=x]"),
        Err(StateParseError::UnknownProperty { .. })
    ));
    assert_eq!(r.state_name(BlockState(1)).to_string(), "boxel:stone");
    assert_eq!(r.values(BlockState(9)).to_string(), "axis=y");
    assert_eq!(r.values(BlockState(1)).to_string(), "");
    // Out of range: no block, no values.
    let out = BlockState(u16::MAX);
    assert_eq!(r.block(out), None);
    assert_eq!(r.state_name(out).to_string(), "boxel:unknown");
    assert_eq!(r.with(out, "axis", "x"), None);
}

#[test]
fn properties_are_sorted_by_name_and_checked() {
    let mut b = BlockRegistryBuilder::new();
    let t = texture(&mut b);
    let s = b
        .add_block_with_states(
            cube("test:sorted", t),
            vec![
                Property::new("zeta", ["a", "b"]),
                Property::boolean("alpha").with_default("true"),
            ],
            |_, _| {},
        )
        .unwrap();
    for (name, properties) in [
        ("test:empty", vec![Property::new("p", Vec::<String>::new())]),
        ("test:upper", vec![Property::new("P", ["a"])]),
        ("test:value", vec![Property::new("p", ["a b"])]),
        ("test:twice", vec![Property::new("p", ["a", "a"])]),
        (
            "test:default",
            vec![Property::new("p", ["a"]).with_default("b")],
        ),
        (
            "test:same",
            vec![Property::new("p", ["a"]), Property::boolean("p")],
        ),
    ] {
        assert_eq!(
            b.add_block_with_states(cube(name, t), properties, |_, _| {}),
            Err(RegistryError::InvalidProperty(name.into())),
            "{name}"
        );
    }
    // Every state is checked like a plain block: here `p=b` is a leaky opaque cube.
    assert_eq!(
        b.add_block_with_states(
            cube("test:leaky", t),
            vec![Property::new("p", ["a", "b"])],
            |v, def| {
                if v.get("p") == Some("b") {
                    def.light_opacity = 3;
                }
            }
        ),
        Err(RegistryError::InvalidLight("test:leaky".into()))
    );
    let r = b.build();
    let block = r.block(s).unwrap();
    let names: Vec<&str> = r.properties(block).iter().map(Property::name).collect();
    assert_eq!(names, ["alpha", "zeta"]);
    assert_eq!(
        r.state_name(s).to_string(),
        "test:sorted[alpha=true,zeta=a]"
    );
    // alpha (stride 2) false/true, zeta (stride 1) a/b.
    let first = r.states(block).next().unwrap();
    assert_eq!(s, BlockState(first.0 + 2));
    // The failed blocks left nothing behind.
    assert_eq!(r.block_count(), 2);
    assert_eq!(r.len(), 5);
}

/// The blocks of round 2 fit: stairs (80 states with `waterlogged`, from one function), fence
/// sides, water levels.
#[test]
fn many_states_come_from_one_function() {
    let mut b = boxel_demo::blocks::builder();
    let t = texture(&mut b);
    let stairs = b
        .add_block_with_states(
            cube("test:stairs", t),
            vec![
                Property::new("facing", ["north", "south", "west", "east"]),
                Property::new("half", ["top", "bottom"]).with_default("bottom"),
                Property::new(
                    "shape",
                    [
                        "straight",
                        "inner_left",
                        "inner_right",
                        "outer_left",
                        "outer_right",
                    ],
                ),
                Property::boolean("waterlogged"),
            ],
            |v, def| {
                def.model = BlockModel::Slab(match v.get("half") {
                    Some("top") => SlabHalf::Top,
                    _ => SlabHalf::Bottom,
                });
                def.collision = def.model.default_collision();
                def.light_opacity = 1;
            },
        )
        .unwrap();
    let fence = b
        .add_block_with_states(
            cube("test:fence", t),
            ["north", "east", "south", "west"]
                .map(Property::boolean)
                .into(),
            |_, def| {
                def.model = BlockModel::Cross;
                def.collision =
                    BlockCollision::Boxes(vec![CollisionBox::new([6, 0, 6], [10, 16, 10])]);
                def.light_opacity = 0;
            },
        )
        .unwrap();
    let water = b
        .add_block_with_states(
            BlockDef {
                shape: RenderShape::Translucent,
                model: BlockModel::Fluid,
                collision: BlockCollision::None,
                light_opacity: 1,
                ..cube("test:water", t)
            },
            vec![Property::int("level", 0..=15)],
            |_, _| {},
        )
        .unwrap();
    let r = b.build();
    let block = r.block(stairs).unwrap();
    assert_eq!(r.states(block).len(), 80);
    assert_eq!(
        r.state_name(stairs).to_string(),
        "test:stairs[facing=north,half=bottom,shape=straight,waterlogged=false]"
    );
    let top = r.with(stairs, "half", "top").unwrap();
    assert_eq!(r.model(top), BlockModel::Slab(SlabHalf::Top));
    assert_eq!(r.model(stairs), BlockModel::Slab(SlabHalf::Bottom));
    // A fence connects by setting a side from its neighbors.
    let north = r.with(fence, "north", "true").unwrap();
    assert_eq!(r.property(north, "north"), Some("true"));
    assert_eq!(r.property(north, "east"), Some("false"));
    assert_eq!(r.states(r.block(fence).unwrap()).len(), 16);
    assert_eq!(r.property(water, "level"), Some("0"));
    assert_eq!(
        r.parse_state("test:water[level=15]").map(|s| s.0 - water.0),
        Ok(15)
    );
}

#[test]
fn the_registry_stays_below_the_mesher_surface_bit() {
    let mut b = BlockRegistryBuilder::new();
    let t = texture(&mut b);
    // 8 * 8 * 8 * 8 * 8 = 32768 states: one more than room is left after air.
    let wide = || {
        (0..5)
            .map(|i| Property::int(format!("p{i}"), 0..=7))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        b.add_block_with_states(cube("test:wide", t), wide(), |_, _| {}),
        Err(RegistryError::Full("block states"))
    );
    // 2^14 fits, twice does not.
    let half = || {
        (0..2)
            .map(|i| Property::int(format!("p{i}"), 0..=127))
            .collect::<Vec<_>>()
    };
    b.add_block_with_states(cube("test:a", t), half(), |_, _| {})
        .unwrap();
    assert_eq!(
        b.add_block_with_states(cube("test:b", t), half(), |_, _| {}),
        Err(RegistryError::Full("block states"))
    );
    let r = b.build();
    assert_eq!(r.len(), 1 + (1 << 14));
    assert!(r.len() <= MAX_STATES);
}
