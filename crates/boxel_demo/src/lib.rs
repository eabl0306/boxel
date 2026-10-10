//! `boxel_demo`: the example content of boxel (#48). The engine crates hold mechanisms only; this
//! crate holds the content they shipped built in until then, byte for byte, so the examples,
//! tests and benches render and behave the same:
//!
//! - [`blocks`]: the blocks and their texture layers ([`blocks::registry`], [`blocks::builder`]),
//!   the lamps and the emissions.
//! - [`shapes`] and [`rules`]: the shapes of the blocks made of boxes (stairs, fences, torches,
//!   walls, fence gates) and the rules that follow their neighbors.
//! - [`fluids`]: water and lava.
//! - [`pack`]: how the demo layers read a Minecraft resource pack.
//! - [`textures`]: the procedural painter of the demo layers.
//! - [`worldgen`]: the terrain, forest and overworld generators, the sea and the caves, and
//!   [`worldgen::DEFAULT_SEED`].
//!
//! Not published: a game copies what it needs, or depends on it by path.

pub mod blocks;
pub mod fluids;
pub mod pack;
pub mod rules;
pub mod shapes;
pub mod textures;
pub mod worldgen;
