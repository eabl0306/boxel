//! G1 of #48 on the demo registry: the content moved out of the engine is the content the
//! engine had (the literal of `boxel_voxel/tests/content_golden.rs`, computed before the move).

#[path = "../../boxel_voxel/tests/golden/mod.rs"]
mod golden;

#[path = "../../boxel_voxel/tests/golden/content.rs"]
mod content;

#[test]
fn the_demo_registry_is_the_old_built_in_content() {
    golden::check(
        "demo_content",
        &content::dump(&boxel_demo::blocks::registry()),
        0x4c67_fee0_91dd_19ab,
    );
}
