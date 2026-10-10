// boxel: modified by tools/refork (crate renames, platform shim, test-only patches or backports).
//! Minimal desktop-only replacement for the subset of the bevy-platform crate used by
//! `boxel_ecs` (written for boxel; template: tools/refork/template/platform/mod.rs).
//!
//! Everything maps to `std`, `hashbrown`, `foldhash` and `futures-lite`. `cfg`, `hash` and
//! `cell` are vendored from the bevy-platform crate version 0.20.0
//! (MIT OR Apache-2.0, Bevy Engine contributors); `collections`, `sync`, `future`, `time`,
//! `thread` and `prelude` are thin re-exports.

pub mod cell;
pub mod cfg;
pub mod collections;
pub mod hash;

/// Re-export of `std::sync` (desktop targets always have native atomics + std).
pub mod sync {
    pub use std::sync::*;
}

/// Futures helpers.
pub mod future {
    pub use futures_lite::future::block_on;
}

/// Time helpers.
pub mod time {
    pub use std::time::*;
}

/// Thread helpers.
pub mod thread {
    pub use std::thread::*;
}

/// Commonly used `alloc` items.
pub mod prelude {
    pub use alloc::{
        borrow::ToOwned, boxed::Box, format, string::String, string::ToString, vec, vec::Vec,
    };
}
