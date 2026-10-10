//! G1 of #48: everything the registry says about each state of the built-in content (name,
//! shape, model kind, boxes, outline, collision, flags, light, textures, material), the texture
//! layer names in order, and what placing a block into air or a water source and breaking it
//! leave. Frozen before the content moved out of the engine: the literal never changes, only the
//! accessor helpers and the setup line below may.

mod golden;

#[path = "golden/content.rs"]
mod content;

use boxel_voxel::BlockRegistry;

/// The registry under test.
fn registry() -> BlockRegistry {
    BlockRegistry::default_blocks()
}

#[test]
fn the_content_is_frozen() {
    golden::check(
        "content",
        &content::dump(&registry()),
        0x4c67_fee0_91dd_19ab,
    );
}
