//! How a game's texture layer finds its pixels in a pack ([`LayerRecipe`]). Plain data: the
//! renderer reads the recipes the game passes with the pack, and this crate never looks at them.

use crate::FaceName;

/// How one texture layer of a game (`game:grass_side`) is read from a Minecraft resource pack:
/// the Minecraft block whose blockstate and model name the texture, the model face that shows
/// it, the biome tint baked into it, and fallback texture names for packs that ship no model.
///
/// A layer without a recipe reads the texture named like it (`game:copper_ore` reads
/// `minecraft:block/copper_ore`), untinted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerRecipe {
    /// The game's texture layer name (`game:grass_side`).
    pub layer: String,
    /// Minecraft block whose blockstate and model are read (`grass_block`, in the `minecraft`
    /// namespace).
    pub block: String,
    /// A state of the game whose property values pick the blockstate variant
    /// (`game:log[axis=y]`), or `""`. Its properties are named like Minecraft's.
    pub state: String,
    /// Values of Minecraft properties the game's block lacks (`snowy=false`), added to the
    /// state's.
    pub variant: String,
    /// Model face giving the texture (`North` stands for every side face).
    pub face: FaceName,
    /// Biome tint (sRGB) baked into model faces with a `tintindex`, or `None`.
    pub tint: Option<[u8; 3]>,
    /// Texture parts (`block/...` in the `minecraft` namespace, bottom first, each with its
    /// tint) when the pack has no usable model.
    pub fallback: Vec<(String, Option<[u8; 3]>)>,
    /// A fluid's flow sprite: Minecraft's are twice the block size and a face shows a quarter of
    /// one, so the layer takes the top-left quarter of a frame twice the layer's edge (other
    /// sizes are scaled whole).
    pub quarter: bool,
}
