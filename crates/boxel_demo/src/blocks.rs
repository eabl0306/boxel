//! The demo blocks: the content the engine shipped built in until #48, byte for byte (the same
//! names, registration order and numbers), so state ids, texture layers, pixels, saves and
//! worldgen output stay the same.
//!
//! The states, in this exact order (state ids are frozen for tests and worldgen): 0
//! `boxel:air`, 1 `boxel:stone`, 2 `boxel:dirt`, 3 `boxel:grass`, 4 `boxel:sand`, 5
//! `boxel:gravel`, 6 `boxel:snow`, 7 `boxel:bedrock`, 8 to 10 `boxel:log[axis=x|y|z]` (default
//! 9, `axis=y`), 11 `boxel:leaves` (Cutout), 12 to 27 `boxel:water[level=0..15]` (Translucent
//! fluid, default 12, the source; ADR 0021), 28 `boxel:planks`, then the lamps (opaque, solid,
//! light opacity 15; emission red, green, blue): 29 `boxel:lamp_orange` (14, 11, 6), 30
//! `boxel:lamp_red` (14, 2, 2), 31 `boxel:lamp_blue` (3, 6, 14), 32 `boxel:lamp_white` (15, 15,
//! 15) ([`LAMPS`]); 33 to 48 `boxel:lava[level=0..15]` (an Opaque fluid with no collision, light
//! opacity 1, emission (15, 7, 2), [`LAVA_EMISSION`]; default 33, the source; ADR 0025); then
//! the blocks of ADR 0018: 49 `boxel:glass` (Translucent cube, solid, light opacity 0), 50 to 55
//! `boxel:stone_slab[type=bottom|top|double, waterlogged=false|true]` (default 50; the halves
//! are Opaque slabs with light opacity 1, `double` an Opaque stone cube), 56 `boxel:short_grass`
//! and 57 `boxel:poppy` (Cutout crosses, no collision, light opacity 0); then the blocks of ADR
//! 0020: 58 to 137 `boxel:stone_stairs` and 138 to 217 `boxel:oak_stairs` (`facing`, `half`,
//! `shape`, `waterlogged`; default the first, all values first; Opaque, light opacity 1), 218 to
//! 249 `boxel:oak_fence` (`east`, `north`, `south`, `waterlogged`, `west`; default 218,
//! unconnected and dry; Opaque, light opacity 0, 1 with water), 250 `boxel:torch` and 251 to 254
//! `boxel:wall_torch[facing=north|south|west|east]` (Cutout, no collision, light opacity 0,
//! emission [`TORCH_EMISSION`]); then what lava makes with water (ADR 0025): 255
//! `boxel:cobblestone` and 256 `boxel:obsidian` (Opaque solid cubes). The states whose
//! `waterlogged` is `true` hold a water source. The side faces of water and lava show their flow
//! textures, `boxel:water_flow` and `boxel:lava_flow` (the last two layers, ADR 0025). Then the
//! blocks of ADR 0027: 257 to 580 `boxel:cobblestone_wall` (`east`, `north`, `south`, `west`
//! each `none | low | tall`, `up`, `waterlogged`; default 263, every side `none`, `up=true` and
//! dry; Opaque, light opacity 0, 1 with water) and 581 to 596 `boxel:oak_fence_gate` (`facing`,
//! `in_wall`, `open`; default 581, facing north, closed; Opaque, light opacity 0; no collision
//! when open).
//!
//! The texture layers, in this order: 0 stone to 20 poppy (stone, dirt, grass top, grass side,
//! sand, gravel, snow, bedrock, log side, log top, leaves, water, planks, the 4 lamps, lava,
//! glass, short grass, poppy), 21 torch, 22 cobblestone, 23 obsidian, 24 `water_flow` and 25
//! `lava_flow`.

use crate::rules::{self, Kinds};
use boxel_voxel::rules::{NeighborRule, UseRule};
use boxel_voxel::shapes::half_from_name;
use boxel_voxel::{
    Axis, BlockCollision, BlockDef, BlockModel, BlockRegistry, BlockRegistryBuilder, BlockState,
    FLUID_LEVEL, FaceTextures, Facing, FenceSides, OPAQUE_OPACITY, Property, RenderShape, SlabHalf,
    StairShape, StateValues, TextureId, TexturePattern, TextureSpec, WATERLOGGED, WallSide,
    WallSides,
};
use std::sync::Arc;

/// One demo lamp: an opaque, solid cube that emits colored light.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lamp {
    /// Block (and texture) name, `"boxel:lamp_orange"`.
    pub name: &'static str,
    /// Emission: red, green, blue, each `0..=15`.
    pub emission: [u8; 3],
    /// Texture base color and speckle color (sRGB).
    pub colors: [[u8; 3]; 2],
}

impl Lamp {
    /// The name without the `boxel:` prefix.
    pub fn short_name(&self) -> &'static str {
        self.name.strip_prefix("boxel:").unwrap_or(self.name)
    }
}

/// The lamps, in state order (states 29 to 32 of [`registry`]).
pub const LAMPS: [Lamp; 4] = [
    Lamp {
        name: "boxel:lamp_orange",
        emission: [14, 11, 6],
        colors: [[235, 150, 60], [255, 225, 160]],
    },
    Lamp {
        name: "boxel:lamp_red",
        emission: [14, 2, 2],
        colors: [[200, 40, 35], [255, 150, 130]],
    },
    Lamp {
        name: "boxel:lamp_blue",
        emission: [3, 6, 14],
        colors: [[50, 90, 220], [160, 205, 255]],
    },
    Lamp {
        name: "boxel:lamp_white",
        emission: [15, 15, 15],
        colors: [[230, 230, 220], [255, 255, 255]],
    },
];

/// Emission (red, green, blue) of `boxel:lava`, every level (red at Minecraft's 15).
pub const LAVA_EMISSION: [u8; 3] = [15, 7, 2];

/// Emission (red, green, blue) of `boxel:torch` and `boxel:wall_torch`: level 14 like
/// Minecraft's torch, a warm white.
pub const TORCH_EMISSION: [u8; 3] = [14, 13, 10];

/// The demo registry (see the [module docs](self) for its states and layers).
pub fn registry() -> BlockRegistry {
    builder().build()
}

/// The texture layers of the demo, by id.
struct Layers {
    stone: TextureId,
    dirt: TextureId,
    grass_top: TextureId,
    grass_side: TextureId,
    sand: TextureId,
    gravel: TextureId,
    snow: TextureId,
    bedrock: TextureId,
    log_side: TextureId,
    log_top: TextureId,
    leaves: TextureId,
    water: TextureId,
    planks: TextureId,
    lamps: [TextureId; 4],
    lava: TextureId,
    glass: TextureId,
    short_grass: TextureId,
    poppy: TextureId,
    torch: TextureId,
    cobblestone: TextureId,
    obsidian: TextureId,
    water_flow: TextureId,
    lava_flow: TextureId,
}

/// Registers the 26 layers in their order.
fn add_layers(b: &mut BlockRegistryBuilder) -> Layers {
    use TexturePattern as P;
    let mut tex = |name: &str, rgb: [u8; 3], noise: u8, pattern: TexturePattern| {
        b.add_texture(TextureSpec {
            name: format!("boxel:{name}"),
            base_color: rgb,
            noise,
            pattern,
        })
        .expect("demo texture")
    };
    let stone = tex("stone", [125, 125, 125], 18, P::Noise);
    let dirt = tex("dirt", [134, 96, 67], 16, P::Noise);
    let grass_top = tex("grass_top", [95, 159, 53], 18, P::Noise);
    let grass_side = tex(
        "grass_side",
        [134, 96, 67],
        16,
        P::TopBand {
            color: [95, 159, 53],
            rows: 4,
        },
    );
    let sand = tex("sand", [219, 207, 163], 10, P::Noise);
    let gravel = tex(
        "gravel",
        [136, 126, 126],
        14,
        P::Speckle {
            color: [90, 85, 85],
            density: 40,
        },
    );
    let snow = tex("snow", [240, 245, 250], 6, P::Noise);
    let bedrock = tex(
        "bedrock",
        [70, 70, 70],
        24,
        P::Speckle {
            color: [30, 30, 30],
            density: 60,
        },
    );
    let log_side = tex("log_side", [102, 81, 51], 14, P::Bark);
    let log_top = tex("log_top", [156, 127, 78], 10, P::Noise);
    let leaves = tex("leaves", [60, 130, 40], 20, P::Leaves);
    let water_spec = ([40, 90, 200], 6);
    let water = tex(
        "water",
        water_spec.0,
        water_spec.1,
        P::Liquid { alpha: 160 },
    );
    let planks = tex("planks", [162, 130, 78], 10, P::Planks);
    // Glowing cubes: a base color with bright speckles.
    let glow = |spark: [u8; 3]| P::Speckle {
        color: spark,
        density: 60,
    };
    let lamps = LAMPS.map(|lamp| {
        let [base, spark] = lamp.colors;
        tex(lamp.short_name(), base, 10, glow(spark))
    });
    let lava_spec = ([207, 78, 16], 22);
    let lava = tex("lava", lava_spec.0, lava_spec.1, glow([255, 190, 50]));
    // Non-cube content (ADR 0018), after the lamps so the earlier texture ids stay.
    let glass = tex("glass", [214, 236, 240], 4, P::Glass);
    let short_grass = tex("short_grass", [124, 178, 66], 18, P::Plant);
    let poppy = tex(
        "poppy",
        [58, 120, 38],
        10,
        P::Flower {
            color: [205, 30, 30],
        },
    );
    // ADR 0020: the torch (both torches share it).
    let torch = tex("torch", [110, 82, 46], 8, P::Torch);
    // ADR 0025: what lava makes with water.
    let cobblestone = tex("cobblestone", [122, 122, 122], 14, P::Cobble);
    let obsidian = tex(
        "obsidian",
        [20, 16, 31],
        6,
        P::Speckle {
            color: [58, 38, 92],
            density: 40,
        },
    );
    // ADR 0025: the flow textures of the fluids, the last layers: each still texture's base
    // color and noise with streaks, water translucent like its still texture.
    let water_flow = tex(
        "water_flow",
        water_spec.0,
        water_spec.1,
        P::Flow { alpha: 160 },
    );
    let lava_flow = tex(
        "lava_flow",
        lava_spec.0,
        lava_spec.1,
        P::Flow { alpha: 255 },
    );
    Layers {
        stone,
        dirt,
        grass_top,
        grass_side,
        sand,
        gravel,
        snow,
        bedrock,
        log_side,
        log_top,
        leaves,
        water,
        planks,
        lamps,
        lava,
        glass,
        short_grass,
        poppy,
        torch,
        cobblestone,
        obsidian,
        water_flow,
        lava_flow,
    }
}

/// A builder with the demo content, to add more blocks after it (their states follow the
/// demo's).
pub fn builder() -> BlockRegistryBuilder {
    let mut b = BlockRegistryBuilder::new();
    let t = add_layers(&mut b);
    // Light opacity: 15 for opaque cubes, 1 for leaves and water (light passes, losing one
    // level per voxel like in air, but direct sky stops at them).
    let block = |name: &str, shape: RenderShape, solid: bool, textures: FaceTextures| BlockDef {
        name: format!("boxel:{name}"),
        shape,
        model: BlockModel::CUBE,
        collision: if solid {
            BlockCollision::Full
        } else {
            BlockCollision::None
        },
        emission: [0; 3],
        light_opacity: if shape == RenderShape::Opaque {
            OPAQUE_OPACITY
        } else {
            1
        },
        textures,
    };
    let all = FaceTextures::all;
    let opaque = RenderShape::Opaque;
    for def in [
        block("stone", opaque, true, all(t.stone)),
        block("dirt", opaque, true, all(t.dirt)),
        block(
            "grass",
            opaque,
            true,
            FaceTextures {
                top: t.grass_top,
                bottom: t.dirt,
                side: t.grass_side,
            },
        ),
        block("sand", opaque, true, all(t.sand)),
        block("gravel", opaque, true, all(t.gravel)),
        block("snow", opaque, true, all(t.snow)),
        block("bedrock", opaque, true, all(t.bedrock)),
    ] {
        b.add_block(def).expect("demo block");
    }
    // The log lies along its `axis` (ADR 0018, 0019): the end texture on the faces across it.
    let log_textures = FaceTextures {
        top: t.log_top,
        bottom: t.log_top,
        side: t.log_side,
    };
    b.add_block_with_states(
        block("log", opaque, true, log_textures),
        vec![Property::new("axis", ["x", "y", "z"]).with_default("y")],
        |state, def| {
            let axis = state.get("axis").and_then(Axis::from_name);
            def.model = BlockModel::Cube {
                axis: axis.unwrap_or_default(),
            };
        },
    )
    .expect("demo log");
    b.add_block(block("leaves", RenderShape::Cutout, true, all(t.leaves)))
        .expect("demo leaves");
    // A fluid's side faces show its flow texture (Minecraft draws a fluid's sides with its flow
    // sprite; ADR 0025), and the mesher gives it to the top of a cell that slopes.
    let fluid_textures = |still, flow| FaceTextures {
        top: still,
        bottom: still,
        side: flow,
    };
    // Water has Minecraft's `level` (ADR 0021): 0 is a source, 1 to 7 flowing water (lower as
    // the level grows) and 8 to 15 falling water. Every level draws, collides and lets light
    // through the same way: only the fluid ticks and the mesher's corner heights read it.
    b.add_block_with_states(
        BlockDef {
            model: BlockModel::Fluid,
            ..block(
                "water",
                RenderShape::Translucent,
                false,
                fluid_textures(t.water, t.water_flow),
            )
        },
        vec![Property::int(FLUID_LEVEL, 0..=15)],
        |_, _| {},
    )
    .expect("demo water");
    b.add_block(block("planks", opaque, true, all(t.planks)))
        .expect("demo planks");
    // The first emitters (ADR 0015): full-cube colored lamps.
    for (lamp, texture) in LAMPS.iter().zip(t.lamps) {
        let def = BlockDef {
            emission: lamp.emission,
            ..block(lamp.short_name(), opaque, true, all(texture))
        };
        b.add_block(def).expect("demo emitter");
    }
    // Lava flows like water with its own rules (ADR 0025) and the same `level` property. It
    // glows, is drawn opaque (Minecraft's solid layer) but hides no neighbor face, lets
    // everything through and lets light through like water (opacity 1, the sky stops).
    b.add_block_with_states(
        BlockDef {
            model: BlockModel::Fluid,
            emission: LAVA_EMISSION,
            light_opacity: 1,
            ..block("lava", opaque, false, fluid_textures(t.lava, t.lava_flow))
        },
        vec![Property::int(FLUID_LEVEL, 0..=15)],
        |_, _| {},
    )
    .expect("demo lava");
    // Non-cube blocks (ADR 0018). Glass is a clear cube: light and sky pass (opacity 0). Slabs
    // let light through at the cost of leaves (a slab roof is not dark; opacity 1 keeps the
    // light flood on its fast path); a double slab is a plain stone cube. Plants stop nothing.
    let model = |name: &str, shape, model: BlockModel, opacity, textures| BlockDef {
        name: format!("boxel:{name}"),
        shape,
        model,
        collision: model.default_collision(),
        emission: [0; 3],
        light_opacity: opacity,
        textures,
    };
    b.add_block(model(
        "glass",
        RenderShape::Translucent,
        BlockModel::CUBE,
        0,
        all(t.glass),
    ))
    .expect("demo glass");
    // A slab holds water (`waterlogged`, ADR 0025) unless it is double.
    b.add_block_with_states(
        model(
            "stone_slab",
            opaque,
            BlockModel::Slab(SlabHalf::Bottom),
            1,
            all(t.stone),
        ),
        vec![
            Property::new("type", ["bottom", "top", "double"]),
            Property::boolean(WATERLOGGED),
        ],
        |state, def| match state.get("type") {
            Some("top") => {
                def.model = BlockModel::Slab(SlabHalf::Top);
                def.collision = def.model.default_collision();
            }
            Some("double") => {
                def.model = BlockModel::CUBE;
                def.collision = BlockCollision::Full;
                def.light_opacity = OPAQUE_OPACITY;
            }
            _ => {}
        },
    )
    .expect("demo slab");
    for def in [
        model(
            "short_grass",
            RenderShape::Cutout,
            BlockModel::Cross,
            0,
            all(t.short_grass),
        ),
        model(
            "poppy",
            RenderShape::Cutout,
            BlockModel::Cross,
            0,
            all(t.poppy),
        ),
    ] {
        b.add_block(def).expect("demo block");
    }
    add_shapes(&mut b, &t);
    add_fluid_products(&mut b, &t);
    add_walls_and_gates(&mut b, &t);
    add_rules(&mut b);
    b
}

/// The rules of the demo blocks ([`crate::rules`]), on their blocks: stairs take their shape,
/// fences, walls and gates follow their neighbors, torches drop without a support, and a gate
/// opens when used.
fn add_rules(b: &mut BlockRegistryBuilder) {
    let kinds = Arc::new(Kinds::of_builder(b));
    let block = |b: &BlockRegistryBuilder, name: &str| b.block_by_name(name).expect(name);
    let neighbor =
        |rule: fn(&Kinds, &BlockRegistry, BlockState, &[BlockState; 6]) -> BlockState| {
            let kinds = Arc::clone(&kinds);
            NeighborRule::new(move |registry, state, neighbors| {
                rule(&kinds, registry, state, neighbors)
            })
        };
    let by_name = [
        ("boxel:stone_stairs", neighbor(rules::stairs_rule)),
        ("boxel:oak_stairs", neighbor(rules::stairs_rule)),
        ("boxel:oak_fence", neighbor(rules::fence_rule)),
        ("boxel:torch", NeighborRule::new(rules::torch_rule)),
        ("boxel:wall_torch", neighbor(rules::wall_torch_rule)),
        ("boxel:cobblestone_wall", neighbor(rules::wall_rule)),
        ("boxel:oak_fence_gate", neighbor(rules::fence_gate_rule)),
    ];
    for (name, rule) in by_name {
        let id = block(b, name);
        b.set_neighbor_rule(id, rule).expect("demo rule");
    }
    let gate = block(b, "boxel:oak_fence_gate");
    let gate_use = UseRule::new(move |registry, state, look| {
        rules::fence_gate_use(&kinds, registry, state, look)
    });
    b.set_use_rule(gate, gate_use).expect("demo use rule");
}

/// The `facing` of a state (north if it has none).
fn facing_of(state: &StateValues<'_>) -> Facing {
    state
        .get("facing")
        .and_then(Facing::from_name)
        .unwrap_or_default()
}

/// The `facing` property of stairs, torches and gates.
fn facing() -> Property {
    Property::new("facing", Facing::ALL.map(Facing::name))
}

/// The blocks of ADR 0020, after the plants: stone and oak stairs, the oak fence, the torch and
/// the wall torch.
fn add_shapes(b: &mut BlockRegistryBuilder, t: &Layers) {
    // Stairs let light through like slabs (opacity 1). `waterlogged` is Minecraft's: the stairs
    // hold a water source (ADR 0025).
    for (name, texture) in [("stone_stairs", t.stone), ("oak_stairs", t.planks)] {
        let def = BlockDef {
            name: format!("boxel:{name}"),
            shape: RenderShape::Opaque,
            model: BlockModel::Stairs {
                facing: Facing::North,
                half: SlabHalf::Bottom,
                shape: StairShape::Straight,
            },
            collision: BlockCollision::None,
            emission: [0; 3],
            light_opacity: 1,
            textures: FaceTextures::all(texture),
        };
        let properties = vec![
            facing(),
            Property::new("half", ["bottom", "top"]),
            Property::new("shape", StairShape::ALL.map(StairShape::name)),
            Property::boolean(WATERLOGGED),
        ];
        b.add_block_with_states(def, properties, |state, def| {
            def.model = BlockModel::Stairs {
                facing: facing_of(state),
                half: state
                    .get("half")
                    .and_then(half_from_name)
                    .unwrap_or_default(),
                shape: state
                    .get("shape")
                    .and_then(StairShape::from_name)
                    .unwrap_or_default(),
            };
            def.collision = def.model.default_collision();
        })
        .expect("demo stairs");
    }
    // The fence stops no light (but direct sky when it holds water); its sides follow its
    // neighbors ([`crate::rules`]).
    let fence = BlockDef {
        name: "boxel:oak_fence".to_string(),
        shape: RenderShape::Opaque,
        model: BlockModel::Fence {
            sides: FenceSides::NONE,
        },
        collision: BlockCollision::None,
        emission: [0; 3],
        light_opacity: 0,
        textures: FaceTextures::all(t.planks),
    };
    let mut properties = Facing::ALL.map(|f| Property::boolean(f.name())).to_vec();
    properties.push(Property::boolean(WATERLOGGED));
    b.add_block_with_states(fence, properties, |state, def| {
        let mut sides = FenceSides::NONE;
        for f in Facing::ALL {
            sides = sides.with(f, state.get(f.name()) == Some("true"));
        }
        def.model = BlockModel::Fence { sides };
        def.collision = def.model.default_collision();
        if state.get(WATERLOGGED) == Some("true") {
            def.light_opacity = def.light_opacity.max(1);
        }
    })
    .expect("demo fence");
    // Torches: cutout sticks that glow ([`TORCH_EMISSION`]), no collision, no opacity.
    let torch_def = |name: &str, model: BlockModel| BlockDef {
        name: format!("boxel:{name}"),
        shape: RenderShape::Cutout,
        model,
        collision: BlockCollision::None,
        emission: TORCH_EMISSION,
        light_opacity: 0,
        textures: FaceTextures::all(t.torch),
    };
    b.add_block(torch_def("torch", BlockModel::Torch))
        .expect("demo torch");
    b.add_block_with_states(
        torch_def(
            "wall_torch",
            BlockModel::WallTorch {
                facing: Facing::North,
            },
        ),
        vec![facing()],
        |state, def| {
            def.model = BlockModel::WallTorch {
                facing: facing_of(state),
            };
        },
    )
    .expect("demo wall torch");
}

/// The blocks lava makes when it meets water (ADR 0025), after the torches: cobblestone and
/// obsidian, opaque solid cubes.
fn add_fluid_products(b: &mut BlockRegistryBuilder, t: &Layers) {
    for (name, texture) in [("cobblestone", t.cobblestone), ("obsidian", t.obsidian)] {
        b.add_block(BlockDef {
            name: format!("boxel:{name}"),
            shape: RenderShape::Opaque,
            model: BlockModel::CUBE,
            collision: BlockCollision::Full,
            emission: [0; 3],
            light_opacity: OPAQUE_OPACITY,
            textures: FaceTextures::all(texture),
        })
        .expect("demo block");
    }
}

/// The blocks of ADR 0027, the last ones: the cobblestone wall (with the cobblestone layer) and
/// the oak fence gate (with the planks layer). Both let light through like a fence (opacity 0,
/// 1 for a wall that holds water: the sky stops) and follow their neighbors
/// ([`crate::rules`]): a wall connects and raises its post, a gate drops between walls.
fn add_walls_and_gates(b: &mut BlockRegistryBuilder, t: &Layers) {
    // Minecraft's `cobblestone_wall`: `up` (the post, default true), a side property per
    // direction and `waterlogged` (ADR 0025).
    let wall = BlockDef {
        name: "boxel:cobblestone_wall".to_string(),
        shape: RenderShape::Opaque,
        model: BlockModel::Wall {
            post: true,
            sides: WallSides::NONE,
        },
        collision: BlockCollision::None,
        emission: [0; 3],
        light_opacity: 0,
        textures: FaceTextures::all(t.cobblestone),
    };
    let mut properties: Vec<Property> = Facing::ALL
        .map(|f| Property::new(f.name(), WallSide::ALL.map(WallSide::name)))
        .to_vec();
    properties.push(Property::boolean("up").with_default("true"));
    properties.push(Property::boolean(WATERLOGGED));
    b.add_block_with_states(wall, properties, |state, def| {
        let mut sides = WallSides::NONE;
        for f in Facing::ALL {
            let side = state.get(f.name()).and_then(WallSide::from_name);
            sides = sides.with(f, side.unwrap_or_default());
        }
        def.model = BlockModel::Wall {
            post: state.get("up") == Some("true"),
            sides,
        };
        def.collision = def.model.default_collision();
        if state.get(WATERLOGGED) == Some("true") {
            def.light_opacity = def.light_opacity.max(1);
        }
    })
    .expect("demo wall");
    // Minecraft's `oak_fence_gate` without `powered` (no redstone): `facing` (the way it
    // opens), `in_wall` and `open`. It holds no water, as in Minecraft.
    let gate = BlockDef {
        name: "boxel:oak_fence_gate".to_string(),
        shape: RenderShape::Opaque,
        model: BlockModel::FenceGate {
            facing: Facing::North,
            open: false,
            in_wall: false,
        },
        collision: BlockCollision::None,
        emission: [0; 3],
        light_opacity: 0,
        textures: FaceTextures::all(t.planks),
    };
    let properties = vec![
        facing(),
        Property::boolean("in_wall"),
        Property::boolean("open"),
    ];
    b.add_block_with_states(gate, properties, |state, def| {
        def.model = BlockModel::FenceGate {
            facing: facing_of(state),
            open: state.get("open") == Some("true"),
            in_wall: state.get("in_wall") == Some("true"),
        };
        def.collision = def.model.default_collision();
    })
    .expect("demo fence gate");
}
