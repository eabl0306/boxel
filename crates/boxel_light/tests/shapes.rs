//! The light of the blocks of ADR 0020: torches glow at level 14 (warm white), stairs pass light
//! like slabs (they stop direct sky), fences and torches stop nothing.

use boxel_light::oracle::OracleWorld;
use boxel_voxel::{
    BlockRegistry, BlockState, ChunkData, ChunkPos, LightTables, LightValue, LocalPos,
    TORCH_EMISSION,
};

/// The solved light of one chunk of air with `blocks`, its column tops from the blocks (sky
/// above them).
fn solve(registry: &BlockRegistry, blocks: &[([u32; 3], &str)]) -> impl Fn([u32; 3]) -> LightValue {
    let tables = LightTables::from_registry(registry);
    let mut chunk = ChunkData::uniform(BlockState::AIR);
    for &(p, name) in blocks {
        chunk.set(
            LocalPos::new(p[0], p[1], p[2]),
            registry.parse_state(name).unwrap(),
        );
    }
    let tops = vec![i64::MIN; 32 * 32];
    let mut world =
        OracleWorld::from_chunks(ChunkPos::new(0, 0, 0), [1; 3], &[chunk], &tables, tops);
    world.tops = world.brute_force_tops();
    let light = world.solve();
    move |p: [u32; 3]| light[((p[1] * 32 + p[0]) * 32 + p[2]) as usize]
}

#[test]
fn torches_glow_warm_white_at_level_14() {
    let registry = BlockRegistry::default_blocks();
    // A torch under a stone roof that covers the whole chunk (no sky below it).
    let mut blocks: Vec<([u32; 3], &str)> = Vec::new();
    for x in 0..32 {
        for z in 0..32 {
            blocks.push(([x, 31, z], "boxel:stone"));
        }
    }
    blocks.push(([10, 10, 10], "boxel:torch"));
    blocks.push(([13, 10, 10], "boxel:wall_torch[facing=east]"));
    let light = solve(&registry, &blocks);
    let emitted = LightValue::from_rgb(TORCH_EMISSION);
    assert_eq!(light([10, 10, 10]), emitted);
    assert_eq!(
        (emitted.red(), emitted.green(), emitted.blue()),
        (14, 13, 10)
    );
    // One level less per step, per channel; nothing from the sky.
    let next = light([10, 11, 10]);
    assert_eq!(
        (next.sky(), next.red(), next.green(), next.blue()),
        (0, 13, 12, 9)
    );
    assert_eq!(light([13, 10, 10]), emitted, "the wall torch glows too");
}

#[test]
fn stairs_pass_light_like_slabs_and_fences_pass_sky() {
    let registry = BlockRegistry::default_blocks();
    let light = solve(
        &registry,
        &[
            ([4, 20, 4], "boxel:stone_stairs"),
            ([5, 20, 4], "boxel:stone_slab"),
            ([6, 20, 4], "boxel:oak_fence"),
            ([7, 20, 4], "boxel:torch"),
        ],
    );
    // Under stairs and slabs the direct sky stops: one level less, as under leaves.
    assert_eq!(light([4, 19, 4]).sky(), 14);
    assert_eq!(light([5, 19, 4]).sky(), 14);
    // Fences and torches stop nothing.
    assert_eq!(light([6, 19, 4]).sky(), 15);
    assert_eq!(light([7, 19, 4]).sky(), 15);
}
