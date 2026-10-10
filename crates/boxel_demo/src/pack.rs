//! How the demo layers read a Minecraft Java resource pack: the Minecraft block, model face,
//! tint and fallback textures of each layer of [`crate::blocks`] ([`recipes`]). A game passes
//! them with the pack (`boxel_render::PackSource::with_recipes`).

use boxel_pack::{FaceName, LayerRecipe};

/// Minecraft's plains grass color (sRGB), baked into grass textures with a tint index.
pub const GRASS_TINT: [u8; 3] = [0x91, 0xBD, 0x59];
/// Minecraft's plains foliage color (sRGB), baked into leaves.
pub const FOLIAGE_TINT: [u8; 3] = [0x77, 0xAB, 0x2F];
/// Minecraft's default water color (sRGB), baked into the grayscale water texture.
pub const WATER_TINT: [u8; 3] = [0x3F, 0x76, 0xE4];

/// A recipe with no state, variant, tint or quarter.
fn recipe(
    layer: &str,
    block: &str,
    face: FaceName,
    fallback: &[(&str, Option<[u8; 3]>)],
) -> LayerRecipe {
    LayerRecipe {
        layer: layer.to_string(),
        block: block.to_string(),
        state: String::new(),
        variant: String::new(),
        face,
        tint: None,
        fallback: fallback
            .iter()
            .map(|&(path, tint)| (path.to_string(), tint))
            .collect(),
        quarter: false,
    }
}

/// The recipe of every layer of [`crate::blocks`], in layer order.
pub fn recipes() -> Vec<LayerRecipe> {
    vec![
        recipe(
            "boxel:stone",
            "stone",
            FaceName::North,
            &[("block/stone", None)],
        ),
        recipe(
            "boxel:dirt",
            "dirt",
            FaceName::North,
            &[("block/dirt", None)],
        ),
        LayerRecipe {
            variant: "snowy=false".to_string(),
            tint: Some(GRASS_TINT),
            ..recipe(
                "boxel:grass_top",
                "grass_block",
                FaceName::Up,
                &[("block/grass_block_top", Some(GRASS_TINT))],
            )
        },
        LayerRecipe {
            variant: "snowy=false".to_string(),
            tint: Some(GRASS_TINT),
            ..recipe(
                "boxel:grass_side",
                "grass_block",
                FaceName::North,
                &[
                    ("block/grass_block_side", None),
                    ("block/grass_block_side_overlay", Some(GRASS_TINT)),
                ],
            )
        },
        recipe(
            "boxel:sand",
            "sand",
            FaceName::North,
            &[("block/sand", None)],
        ),
        recipe(
            "boxel:gravel",
            "gravel",
            FaceName::North,
            &[("block/gravel", None)],
        ),
        recipe(
            "boxel:snow",
            "snow_block",
            FaceName::North,
            &[("block/snow", None)],
        ),
        recipe(
            "boxel:bedrock",
            "bedrock",
            FaceName::North,
            &[("block/bedrock", None)],
        ),
        LayerRecipe {
            state: "boxel:log[axis=y]".to_string(),
            ..recipe(
                "boxel:log_side",
                "oak_log",
                FaceName::North,
                &[("block/oak_log", None)],
            )
        },
        LayerRecipe {
            state: "boxel:log[axis=y]".to_string(),
            ..recipe(
                "boxel:log_top",
                "oak_log",
                FaceName::Up,
                &[("block/oak_log_top", None)],
            )
        },
        LayerRecipe {
            tint: Some(FOLIAGE_TINT),
            ..recipe(
                "boxel:leaves",
                "oak_leaves",
                FaceName::North,
                &[("block/oak_leaves", Some(FOLIAGE_TINT))],
            )
        },
        LayerRecipe {
            tint: Some(WATER_TINT),
            ..recipe(
                "boxel:water",
                "water",
                FaceName::Up,
                &[("block/water_still", Some(WATER_TINT))],
            )
        },
        recipe(
            "boxel:planks",
            "oak_planks",
            FaceName::North,
            &[("block/oak_planks", None)],
        ),
        recipe(
            "boxel:lamp_orange",
            "glowstone",
            FaceName::North,
            &[("block/glowstone", None)],
        ),
        recipe(
            "boxel:lamp_red",
            "shroomlight",
            FaceName::North,
            &[("block/shroomlight", None)],
        ),
        recipe(
            "boxel:lamp_blue",
            "sea_lantern",
            FaceName::North,
            &[("block/sea_lantern", None)],
        ),
        LayerRecipe {
            variant: "lit=true".to_string(),
            ..recipe(
                "boxel:lamp_white",
                "redstone_lamp",
                FaceName::North,
                &[("block/redstone_lamp_on", None)],
            )
        },
        recipe(
            "boxel:lava",
            "lava",
            FaceName::Up,
            &[("block/lava_still", None)],
        ),
        // ADR 0018: glass (translucent, its alpha kept), and the cross plants (cutout). Slabs (all
        // three types) use the stone layer and the turned logs the log layers.
        recipe(
            "boxel:glass",
            "glass",
            FaceName::North,
            &[("block/glass", None)],
        ),
        LayerRecipe {
            tint: Some(GRASS_TINT),
            ..recipe(
                "boxel:short_grass",
                "short_grass",
                FaceName::North,
                &[("block/short_grass", Some(GRASS_TINT))],
            )
        },
        recipe(
            "boxel:poppy",
            "poppy",
            FaceName::North,
            &[("block/poppy", None)],
        ),
        // ADR 0020: the torch (cutout; both torches share it). Stairs use the stone and planks
        // layers, the fence the planks layer.
        recipe(
            "boxel:torch",
            "torch",
            FaceName::North,
            &[("block/torch", None)],
        ),
        // ADR 0025: what lava makes with water. ADR 0027: the cobblestone wall uses this layer
        // (Minecraft's `cobblestone_wall` models take `block/cobblestone`) and the oak fence gate
        // the planks layer (`oak_fence_gate` takes `block/oak_planks`), as the fence does.
        recipe(
            "boxel:cobblestone",
            "cobblestone",
            FaceName::North,
            &[("block/cobblestone", None)],
        ),
        recipe(
            "boxel:obsidian",
            "obsidian",
            FaceName::North,
            &[("block/obsidian", None)],
        ),
        // ADR 0025: the flow sprites of the fluids (no model names them).
        LayerRecipe {
            tint: Some(WATER_TINT),
            quarter: true,
            ..recipe(
                "boxel:water_flow",
                "water",
                FaceName::North,
                &[("block/water_flow", Some(WATER_TINT))],
            )
        },
        LayerRecipe {
            quarter: true,
            ..recipe(
                "boxel:lava_flow",
                "lava",
                FaceName::North,
                &[("block/lava_flow", None)],
            )
        },
    ]
}
