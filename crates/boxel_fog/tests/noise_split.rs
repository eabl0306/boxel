//! The GPU's split of the noise ([`LayerParams`] plus a small cell offset, with carries into
//! the high words) against the direct `i128` computation from the absolute cell, far from the
//! origin too (ADR 0014): the same coverage, bit for bit, at the cells' corners and at points
//! inside them, at whole ticks and between them. And how the coverage moves with the wind.
use boxel_fog::layer::displacement;
use boxel_fog::noise::{coverage, coverage_direct, noise_at, noise_direct, step_bound};
use boxel_fog::{FogLayer, FogTime, LayerParams, NoiseDims};
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
        subtick in prop_oneof![Just(0u16), any::<u16>()],
        rise in prop_oneof![Just(0.0f32), 0.0f32..=1.0],
        d in prop::array::uniform3(-4096i32..=4096),
        sub in prop::array::uniform3(0i32..4096),
    ) {
        let octaves = if two && l > 0 { 2 } else { 1 };
        let dims = if volume { NoiseDims::Volume } else { NoiseDims::Flat };
        let mut layer = layer(cell, 1 << l, octaves, dims, seed);
        layer.threshold_rise = rise;
        let origin = [ox, oy, oz];
        let time = FogTime::new(tick, subtick);
        let p = LayerParams::new(&layer, origin, time);
        let abs = std::array::from_fn(|a| p.c0[a] + i64::from(d[a]));
        prop_assert_eq!(coverage(&p, d), coverage_direct(&layer, abs, time));
        prop_assert_eq!(noise_at(&p, d, sub), noise_direct(&layer, abs, sub, time));
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

/// A sub-tick is that fraction of a tick of wind: at 64 blocks a second over 1-block cells, a
/// tick is a cell and half a tick half a cell, along either sign.
#[test]
fn subticks_move_the_wind_between_ticks() {
    let mut layer = layer([1, 1, 1], 4, 1, NoiseDims::Volume, 3);
    layer.wind = [64.0, 0.0, -64.0];
    assert_eq!(
        displacement(&layer, FogTime::new(3, 0)),
        displacement(&layer, 3)
    );
    let half = displacement(&layer, FogTime::new(3, 32768));
    assert_eq!(half[0], (3, 2048));
    assert_eq!(half[2], (-4, 2048), "-3.5 cells");
    assert_eq!(
        displacement(&layer, FogTime::new(3, 1))[0],
        (3, 0),
        "under 1/4096"
    );
    assert_eq!(displacement(&layer, FogTime::new(3, 16))[0], (3, 1));
    // The noise of the half step is between the ticks', point for point.
    let p = LayerParams::new(&layer, [0; 3], FogTime::new(3, 32768));
    let before = LayerParams::new(&layer, [0; 3], 3);
    for x in -40..40 {
        // Half a tick later the field has moved half a cell along +x and -z.
        assert_eq!(
            noise_at(&p, [x, 0, -1], [2048, 0, 2048]),
            noise_at(&before, [x, 0, 0], [0; 3]),
            "x {x}"
        );
    }
}

/// With a rising threshold over flat noise every column is widest at the band's bottom: a row
/// has fog only where the row below it has (a flat base, thinner tops).
#[test]
fn a_rising_threshold_keeps_the_base_widest() {
    let mut layer = layer([4, 4, 4], 8, 2, NoiseDims::Flat, 21);
    layer.threshold = 0.35;
    layer.threshold_rise = 0.5;
    let rows = 4;
    let p = LayerParams::new(&layer, [0; 3], 0);
    let mut counts = [0; 4];
    for x in -200..200 {
        for z in -50..50 {
            let mut below = true;
            for (r, count) in counts.iter_mut().enumerate() {
                let filled = coverage(&p, [x, r as i32, z]).coverage > 0;
                assert!(
                    !filled || below,
                    "({x}, {r}, {z}) floats over an empty cell"
                );
                below = filled;
                *count += i32::from(filled);
            }
        }
    }
    assert!(
        counts.windows(2).all(|w| w[0] > w[1]) && counts[rows - 1] > 0,
        "each row thinner than the one below: {counts:?}"
    );
}

/// The largest change of a cell's coverage between moments `step` apart (1/65536 ticks), over
/// a block of cells and many moments.
fn largest_step(layer: &FogLayer, step: u64, moments: u64) -> u32 {
    let mut worst = 0;
    let at = |u: u64| {
        let p = LayerParams::new(layer, [0; 3], FogTime::new(u >> 16, (u & 0xffff) as u16));
        let mut out = Vec::new();
        for x in -12..12 {
            for y in 0..4 {
                for z in -12..12 {
                    out.push(coverage(&p, [x, y, z]).coverage);
                }
            }
        }
        out
    };
    let mut prev = at(0);
    for i in 1..moments {
        let now = at(i * step);
        for (a, b) in prev.iter().zip(&now) {
            worst = worst.max(a.abs_diff(*b));
        }
        prev = now;
    }
    worst
}

/// A soft layer changes no cell's coverage by more than a small step per tick as the wind
/// moves the noise: cells are born almost empty and fade out, never pop. A frame at 144 Hz
/// (between ticks) moves it less. With softness 0 cells jump from empty to full.
#[test]
fn soft_cells_fill_and_empty_gradually() {
    let mut l = layer([8, 8, 8], 4, 2, NoiseDims::Volume, 5);
    (l.min_y, l.max_y) = (0, 32);
    l.threshold = 0.5;
    l.threshold_rise = 0.3;
    l.softness = 0.3;
    l.wind = [3.0, 0.0, 1.5];
    let tick = largest_step(&l, 1 << 16, 200);
    let frame = largest_step(&l, (1 << 16) * 64 / 144, 200);
    assert!(tick <= 64, "{tick} of 4096 in a tick");
    assert!(frame < tick, "a frame {frame}, a tick {tick}");
    l.softness = 0.0;
    assert_eq!(largest_step(&l, 1 << 16, 200), 4096, "binary cells pop");
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

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// A lattice step the bound calls empty has no cell with coverage, and every cell in it
    /// shares the bound (the march skips it whole).
    #[test]
    fn the_step_bound_is_conservative(
        ox in origin_strategy(), oz in origin_strategy(),
        l in 1u32..=3, two in any::<bool>(), volume in any::<bool>(), seed in any::<u64>(),
        threshold in 0.3f32..0.9, softness in 0.0f32..0.5,
        tick in 0u64..100_000,
        d in prop::array::uniform3(-300i32..=300),
    ) {
        let dims = if volume { NoiseDims::Volume } else { NoiseDims::Flat };
        let mut layer = layer([2, 2, 2], 1 << l, if two { 2 } else { 1 }, dims, seed);
        layer.threshold = threshold;
        layer.softness = softness;
        let p = LayerParams::new(&layer, [ox, 64, oz], tick);
        let bound = step_bound(&p, d);
        for (a, &da) in d.iter().enumerate() {
            if a == 1 && !volume {
                prop_assert_eq!((bound.lo[a], bound.hi[a]), (i32::MIN, i32::MAX));
            } else {
                prop_assert!(bound.lo[a] <= da && da < bound.hi[a]);
                prop_assert_eq!(bound.hi[a] - bound.lo[a], 1 << l);
            }
        }
        let ys = if volume { bound.lo[1]..bound.hi[1] } else { d[1]..d[1] + 1 };
        for x in bound.lo[0]..bound.hi[0] {
            for y in ys.clone() {
                for z in bound.lo[2]..bound.hi[2] {
                    let c = coverage(&p, [x, y, z]);
                    prop_assert!(c.noise <= bound.max_noise, "{:?} over {}", c, bound.max_noise);
                    if bound.is_empty(&p) {
                        prop_assert_eq!(c.coverage, 0);
                    }
                    prop_assert_eq!(step_bound(&p, [x, y, z]), bound);
                }
            }
        }
    }
}
