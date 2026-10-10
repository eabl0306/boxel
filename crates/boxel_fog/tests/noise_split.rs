//! The GPU's split of the noise ([`LayerParams`] plus a small cell offset, with carries into
//! the high words) against the direct `i128` computation from the absolute cell, far from the
//! origin too (ADR 0014): the same coverage, bit for bit.
use boxel_fog::noise::{coverage, coverage_direct};
use boxel_fog::{FogLayer, LayerParams, NoiseDims};
use boxel_voxel::MAX_BLOCK_COORD;
use proptest::prelude::*;

fn layer(cell: [u32; 3], scale: u32, octaves: u8, dims: NoiseDims, seed: u64) -> FogLayer {
    let mut l = FogLayer::new(0, i64::from(cell[1]) * 4);
    l.cell = cell;
    l.noise.scale = scale;
    l.noise.octaves = octaves;
    l.noise.dims = dims;
    l.noise.seed = seed;
    l.threshold = 0.45;
    l.softness = 0.2;
    l.wind = [3.25, -0.75, -11.5];
    l
}

fn origin_strategy() -> impl Strategy<Value = i64> {
    let centers = [
        0i64,
        1 << 31,
        -(1 << 31),
        1 << 40,
        -(1 << 40),
        MAX_BLOCK_COORD - 5000,
        -MAX_BLOCK_COORD + 5000,
    ];
    (0..centers.len(), -5000i64..5000).prop_map(move |(i, off)| centers[i] + off)
}

fn cell_strategy() -> impl Strategy<Value = [u32; 3]> {
    prop_oneof![
        Just([1, 1, 1]),
        Just([2, 2, 2]),
        Just([4, 4, 4]),
        Just([12, 4, 12]),
        (1u32..=32, 1u32..=32, 1u32..=32).prop_map(|(a, b, c)| [a, b, c]),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn the_split_equals_the_direct_computation(
        ox in origin_strategy(), oy in origin_strategy(), oz in origin_strategy(),
        cell in cell_strategy(),
        l in 0u32..=6, two in any::<bool>(), volume in any::<bool>(), seed in any::<u64>(),
        tick in prop_oneof![Just(0u64), 0u64..100_000, 0u64..(1 << 40)],
        d in prop::array::uniform3(-4096i32..=4096),
    ) {
        let octaves = if two && l > 0 { 2 } else { 1 };
        let dims = if volume { NoiseDims::Volume } else { NoiseDims::Flat };
        let layer = layer(cell, 1 << l, octaves, dims, seed);
        let origin = [ox, oy, oz];
        let p = LayerParams::new(&layer, origin, tick);
        let abs = std::array::from_fn(|a| p.c0[a] + i64::from(d[a]));
        prop_assert_eq!(coverage(&p, d), coverage_direct(&layer, abs, tick));
    }

    /// Two render origins see the same absolute cells.
    #[test]
    fn moving_the_origin_changes_nothing(
        ox in origin_strategy(), oz in origin_strategy(),
        shift in prop::array::uniform3(-300i64..300),
        d in prop::array::uniform3(-200i32..=200),
        tick in 0u64..1_000_000,
    ) {
        let layer = layer([2, 2, 2], 8, 2, NoiseDims::Volume, 7);
        let a = LayerParams::new(&layer, [ox, 64, oz], tick);
        let b = LayerParams::new(&layer, [ox + shift[0], 64 + shift[1], oz + shift[2]], tick);
        let abs: [i64; 3] = std::array::from_fn(|i| a.c0[i] + i64::from(d[i]));
        let db: [i32; 3] = std::array::from_fn(|i| (abs[i] - b.c0[i]) as i32);
        prop_assert_eq!(coverage(&a, d), coverage(&b, db));
    }
}

/// `n` ticks of wind move the noise by exactly `n * wind` (in 1/65536 blocks), here a whole
/// number of cells.
#[test]
fn wind_translates_the_noise_exactly() {
    let mut layer = layer([4, 4, 4], 4, 1, NoiseDims::Flat, 3);
    // 4 blocks a second along x: one cell per second, 64 ticks.
    layer.wind = [4.0, 0.0, -8.0];
    for t in [0u64, 64, 640, 64 * 1000] {
        let cells = (t / 64) as i64;
        for x in -20..20i64 {
            for z in -5..5i64 {
                let now = coverage_direct(&layer, [x + cells, 0, z - 2 * cells], t);
                assert_eq!(
                    now,
                    coverage_direct(&layer, [x, 0, z], 0),
                    "t {t} x {x} z {z}"
                );
            }
        }
    }
}

/// Cells of one lattice step in a flat layer share their column's noise at every height.
#[test]
fn flat_noise_ignores_the_height() {
    let layer = layer([2, 2, 2], 4, 2, NoiseDims::Flat, 11);
    for y in [-1_000_000i64, -1, 0, 5, 1 << 40] {
        assert_eq!(
            coverage_direct(&layer, [3, y, -9], 77),
            coverage_direct(&layer, [3, 0, -9], 77)
        );
    }
}
