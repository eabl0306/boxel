//! Beam tiles (ADR 0061) against a brute-force trace, cell by cell, of every source of the
//! column: on hand-built columns (random blockers of opacity 0, 1, 5 and 15, sources of both
//! directions and every range, realm planes, gaps of air, void below) and on generated overworld
//! content with sources under its cave roofs. Beams end at leaves and water like the sky, never
//! cross a plane, windows built in pieces agree with one big window, and an edit retraced in
//! place equals a rebuild of the edited content. Colored tiles (ADR 0064) keep exactly the
//! filter cells inside their beams, and keep them through edits, with the band of every cell
//! whose filtered sky changed.
mod common;

use boxel_demo::worldgen::OverworldGenerator;
use boxel_light::beam_tile::{self, MAX_WINDOW, beam_limit, reaches};
use boxel_light::sky_tile::walk;
use boxel_light::{Beam, BeamTile, LayerData, Retrace};
use boxel_voxel::{
    BeamDir, BlockState, CHUNK_SHIFT, CHUNK_VOLUME, ChunkData, ChunkKey, ChunkPos, LightTables,
    LocalPos, MAX_BEAM, SkySourceDef,
};
use boxel_worldgen::WorldGenerator as _;
use common::{Blocks, Rng, test_registry_full};
use proptest::prelude::*;
use std::collections::BTreeMap;
use std::ops::RangeInclusive;

/// One chunk column of content: the layers that hold blocks; every other layer is air.
#[derive(Clone, Default)]
struct Column {
    chunks: BTreeMap<i64, ChunkData>,
}

impl Column {
    fn get(&self, x: u32, y: i64, z: u32) -> BlockState {
        self.chunks
            .get(&(y >> CHUNK_SHIFT))
            .map_or(BlockState::AIR, |c| c.get(local(x, y, z)))
    }

    fn set(&mut self, x: u32, y: i64, z: u32, s: BlockState) {
        self.chunks
            .entry(y >> CHUNK_SHIFT)
            .or_insert_with(|| ChunkData::uniform(BlockState::AIR))
            .set(local(x, y, z), s);
    }

    /// Layer `cy` as a walk reads it: the chunk, or air; with `skip`, a gap of air is one
    /// [`LayerData::AirUntil`] and nothing below is [`LayerData::Void`].
    fn layer(&self, cy: i64, skip: bool) -> LayerData<'_> {
        if let Some(c) = self.chunks.get(&cy) {
            return LayerData::Ref(c);
        }
        if !skip {
            return LayerData::Air;
        }
        match self.chunks.range(..cy).next_back() {
            Some((&below, _)) => LayerData::AirUntil(below),
            None => LayerData::Void,
        }
    }

    fn build(
        &self,
        window: RangeInclusive<i64>,
        planes: &[i64],
        tables: &LightTables,
        skip: bool,
    ) -> BeamTile {
        beam_tile::build(window, planes, tables, |cy| self.layer(cy, skip))
    }
}

fn local(x: u32, y: i64, z: u32) -> LocalPos {
    LocalPos::new(x, (y & 31) as u32, z)
}

/// Every beam that reaches `window`, traced cell by cell, sorted.
fn brute_force(
    column: &Column,
    window: &RangeInclusive<i64>,
    planes: &[i64],
    tables: &LightTables,
) -> Vec<(u32, u32, Beam)> {
    let realm = |y: i64| planes.partition_point(|&p| p <= y);
    let mut out = Vec::new();
    for (&cy, chunk) in &column.chunks {
        for i in 0..CHUNK_VOLUME {
            let p = LocalPos::from_index(i);
            let Some(def) = tables.sky_source(chunk.get(p)) else {
                continue;
            };
            let y = (cy << CHUNK_SHIFT) + i64::from(p.y());
            if !reaches(def, y, window) {
                continue;
            }
            let step = if def.dir == BeamDir::Down { -1 } else { 1 };
            let mut end = y;
            for _ in 1..def.range {
                let next = end + step;
                if realm(next) != realm(y) || tables.blocks_sky(column.get(p.x(), next, p.z())) {
                    break;
                }
                end = next;
            }
            let (lo, hi) = if step < 0 { (end, y) } else { (y, end) };
            out.push((
                p.x(),
                p.z(),
                Beam {
                    source: y,
                    lo,
                    hi,
                    def,
                },
            ));
        }
    }
    sorted(out)
}

fn sorted(mut beams: Vec<(u32, u32, Beam)>) -> Vec<(u32, u32, Beam)> {
    beams.sort_by_key(|&(x, z, b)| {
        let up = b.def.dir == BeamDir::Up;
        ((x << 5) | z, b.lo, b.hi, b.source, up, b.def.range)
    });
    beams
}

fn listed(tile: &BeamTile) -> Vec<(u32, u32, Beam)> {
    sorted(tile.iter().collect())
}

/// The blocks a random column uses: clear (air, glass), opacity 1 (leaves, water), 5 (smoke)
/// and 15 (stone), and every source state.
fn palette(b: &Blocks) -> Vec<BlockState> {
    let mut all = vec![b.air, b.air, b.glass, b.leaves, b.water, b.stone];
    all.extend(b.smoke);
    all
}

/// A random column: layers `-3..=3` (some missing), stone and air in runs with other blocks
/// sprinkled, and sources at random air cells.
fn random_column(rng: &mut Rng, b: &Blocks) -> Column {
    let blocks = palette(b);
    let mut column = Column::default();
    for cy in -3..=3i64 {
        if rng.chance(0.25) {
            continue;
        }
        let mut chunk = ChunkData::uniform(b.air);
        // A floor of stone with holes, then sprinkled blocks.
        if rng.chance(0.5) {
            let ly = rng.below(32) as u32;
            for x in 0..32 {
                for z in 0..32 {
                    if !rng.chance(0.3) {
                        chunk.set(LocalPos::new(x, ly, z), b.stone);
                    }
                }
            }
        }
        let solid = rng.unit() * 0.4;
        for i in 0..CHUNK_VOLUME {
            let u = rng.unit();
            let s = if u < solid * 0.1 {
                b.stone
            } else if u < solid * 0.1 + 0.02 {
                blocks[rng.below(blocks.len() as u64) as usize]
            } else if u < solid * 0.1 + 0.02 + 0.004 {
                b.sources[rng.below(b.sources.len() as u64) as usize]
            } else {
                continue;
            };
            chunk.set(LocalPos::from_index(i), s);
        }
        column.chunks.insert(cy, chunk);
    }
    column
}

fn random_window(rng: &mut Rng) -> RangeInclusive<i64> {
    let lo = -120 + rng.below(160) as i64;
    lo..=lo + rng.below(200) as i64
}

fn random_planes(rng: &mut Rng) -> Vec<i64> {
    let mut planes: Vec<i64> = (0..rng.below(3))
        .map(|_| -100 + rng.below(200) as i64)
        .collect();
    planes.sort_unstable();
    planes.dedup();
    planes
}

fn registry(heavy: bool, filters: bool) -> (LightTables, Blocks) {
    let (registry, blocks) = test_registry_full(heavy, filters, true);
    (LightTables::from_registry(&registry), blocks)
}

/// The source state of `b` with `def`.
fn source(b: &Blocks, tables: &LightTables, dir: BeamDir, range: u16) -> BlockState {
    *b.sources
        .iter()
        .find(|&&s| tables.sky_source(s) == Some(SkySourceDef { dir, range }))
        .expect("a test:sky_source state")
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    #[test]
    fn build_equals_brute_force_on_hand_built_columns(seed in any::<u64>(), heavy in any::<bool>(),
                                                      skip in any::<bool>()) {
        let (tables, b) = registry(heavy, false);
        let mut rng = Rng::new(seed);
        let column = random_column(&mut rng, &b);
        let window = random_window(&mut rng);
        let planes = random_planes(&mut rng);
        let tile = column.build(window.clone(), &planes, &tables, skip);
        prop_assert_eq!(listed(&tile), brute_force(&column, &window, &planes, &tables));
        prop_assert_eq!(tile.window(), window);
        if let Some((lo, hi)) = tile.bounds() {
            prop_assert!(tile.iter().all(|(_, _, b)| lo <= b.lo && b.hi <= hi));
        }
    }

    #[test]
    fn an_in_place_retrace_after_an_edit_equals_a_rebuild(seed in any::<u64>()) {
        let (tables, b) = registry(true, false);
        let mut rng = Rng::new(seed);
        let mut column = random_column(&mut rng, &b);
        let window = random_window(&mut rng);
        let planes = random_planes(&mut rng);
        let mut tile = column.build(window.clone(), &planes, &tables, false);
        let mut edits = palette(&b);
        edits.extend(&b.sources);
        let mut unresolved = 0;
        for _ in 0..40 {
            // Two columns, so edits meet the beams they place; most edits at a beam's ends,
            // where cuts and growth happen, the rest anywhere in reach (and a little past it).
            let (x, z) = (3 + rng.below(2) as u32, 7);
            let beams: Vec<Beam> = tile.column(x, z).collect();
            let y = if !beams.is_empty() && rng.chance(0.7) {
                let b = beams[rng.below(beams.len() as u64) as usize];
                let mid = b.lo + (b.hi - b.lo) / 2;
                [b.lo - 1, b.hi + 1, b.lo, b.hi, b.source, mid][rng.below(6) as usize]
            } else {
                let span = (window.end() - window.start() + 600) as u64;
                window.start() - 300 + rng.below(span) as i64
            };
            let s = edits[rng.below(edits.len() as u64) as usize];
            let before = coverage(&tile, x, z);
            column.set(x, y, z, s);
            let missing = rng.chance(0.1).then(|| (y >> CHUNK_SHIFT) - 1 + rng.below(3) as i64);
            let retrace = tile.edit(x, z, y, s, &planes, &tables, |cy| {
                (Some(cy) != missing).then(|| column.layer(cy, false))
            });
            let rebuilt = column.build(window.clone(), &planes, &tables, false);
            if retrace == Retrace::Unresolved {
                unresolved += 1;
                tile = rebuilt;
                continue;
            }
            prop_assert!(tile == rebuilt, "edit {s:?} at ({x}, {y}, {z})");
            let after = coverage(&tile, x, z);
            let changed: Vec<i64> = before.symmetric_difference(&after).copied().collect();
            match retrace {
                Retrace::Unchanged => prop_assert!(changed.is_empty(), "{changed:?}"),
                Retrace::Changed { lo, hi } => {
                    prop_assert!(!changed.is_empty());
                    prop_assert_eq!((lo, hi), (changed[0], changed[changed.len() - 1]));
                }
                Retrace::Unresolved => unreachable!(),
            }
        }
        prop_assert!(unresolved <= 40);
    }
}

/// The beam cells of column `(x, z)` of `tile`.
fn coverage(tile: &BeamTile, x: u32, z: u32) -> std::collections::BTreeSet<i64> {
    tile.column(x, z).flat_map(|b| b.lo..=b.hi).collect()
}

/// Generated overworld layers with a down source in the first air cell under every blocker of
/// the cave layers (as a game's lit cavern roof), an up source on some floors, and a range per
/// source in turn; the tile of a mesh box equals the brute force, and so does a window over every
/// layer.
#[test]
fn build_equals_brute_force_on_generated_content() {
    let mut builder = boxel_demo::blocks::builder();
    boxel_voxel::testing::add_sky_source(&mut builder).unwrap();
    let registry = builder.build();
    let tables = LightTables::from_registry(&registry);
    let states: Vec<BlockState> = (0..registry.len())
        .map(|i| BlockState(i as u16))
        .filter(|&s| tables.is_sky_source(s))
        .collect();
    let g = OverworldGenerator::default();
    let (cx, cz) = (3, -2);
    // From the surface layer down through the caves.
    let top = (g.height_at(cx * 32, cz * 32) >> CHUNK_SHIFT) + 1;
    let mut column = Column::default();
    let mut placed = 0;
    for cy in top - 7..=top {
        let mut chunk = g.generate(ChunkKey::new(ChunkPos::new(cx, cy, cz)), &registry);
        for i in 0..CHUNK_VOLUME - 1024 {
            let (p, above) = (LocalPos::from_index(i), LocalPos::from_index(i + 1024));
            if chunk.get(p).is_air() && tables.blocks_sky(chunk.get(above)) && i % 3 != 0 {
                let s = states[placed % states.len()];
                chunk.set(p, s);
                placed += 1;
            }
        }
        column.chunks.insert(cy, chunk);
    }
    assert!(placed > 100, "{placed} sources");
    let y0 = (top - 4) << CHUNK_SHIFT;
    for window in [
        y0 - 16..=y0 + 47,
        ((top - 7) << CHUNK_SHIFT)..=(top << CHUNK_SHIFT) + 31,
    ] {
        for planes in [vec![], vec![y0 + 5]] {
            let tile = column.build(window.clone(), &planes, &tables, true);
            let want = brute_force(&column, &window, &planes, &tables);
            assert!(want.len() > 50, "{} beams", want.len());
            assert_eq!(listed(&tile), want);
        }
    }
}

/// A source above leaves, water or a cyan filter of opacity 1 ends its beam on top of them,
/// where the sky top of the same column (without the source) lies; glass and red glass
/// (opacity 0) let it through, white.
#[test]
fn a_beam_stops_at_leaves_and_water_like_the_sky() {
    let (tables, b) = registry(false, true);
    let [red, cyan] = b.filters.unwrap();
    let down = source(&b, &tables, BeamDir::Down, 256);
    for (blocker, stops) in [
        (b.leaves, true),
        (b.water, true),
        (cyan, true),
        (b.stone, true),
        (b.glass, false),
        (red, false),
    ] {
        let mut column = Column::default();
        column.set(3, 40, 5, blocker);
        column.set(3, 100, 5, down);
        let tile = column.build(0..=63, &[], &tables, false);
        let beam = tile.column(3, 5).next().unwrap();
        // The sky over the same blocks: the walk from the source's cell down.
        let mut sky = column.clone();
        sky.set(3, 100, 5, b.air);
        let t = walk(127, -1000, &tables, |cy| sky.layer(cy, true)).top(3, 5);
        if stops {
            assert_eq!((beam.lo, t), (41, 40), "{blocker:?}");
        } else {
            assert_eq!(beam.lo, 100 - 255, "{blocker:?}");
            assert_eq!(t, i64::MIN, "{blocker:?}: open down to the void");
        }
        assert_eq!(beam.hi, 100);
    }
}

/// A beam never crosses the plane of its source's realm: a down beam stops on the plane (the
/// bottom cell of its realm), an up beam under it.
#[test]
fn a_beam_stops_at_a_plane() {
    let (tables, b) = registry(false, false);
    let mut column = Column::default();
    let plane = 4096;
    column.set(1, plane + 4, 1, source(&b, &tables, BeamDir::Down, 256));
    column.set(2, plane - 6, 2, source(&b, &tables, BeamDir::Up, 256));
    column.set(3, plane, 3, source(&b, &tables, BeamDir::Down, 64));
    column.set(4, plane - 1, 4, source(&b, &tables, BeamDir::Up, 64));
    column.set(5, plane + 30, 5, source(&b, &tables, BeamDir::Down, 16));
    let window = plane - 64..=plane + 63;
    let tile = column.build(window.clone(), &[plane], &tables, false);
    let ends = |x| {
        let beam = tile.column(x, x).next().unwrap();
        (beam.lo, beam.hi)
    };
    assert_eq!(ends(1), (plane, plane + 4));
    assert_eq!(ends(2), (plane - 6, plane - 1));
    assert_eq!(ends(3), (plane, plane));
    assert_eq!(ends(4), (plane - 1, plane - 1));
    assert_eq!(ends(5), (plane + 15, plane + 30), "the range first");
    // Without the plane the beams run to their ranges.
    let open = column.build(window.clone(), &[], &tables, false);
    assert_eq!(open.column(1, 1).next().unwrap().lo, plane + 4 - 255);
    assert_eq!(
        listed(&tile),
        brute_force(&column, &window, &[plane], &tables)
    );
    assert_eq!(
        beam_limit(
            SkySourceDef {
                dir: BeamDir::Down,
                range: 256
            },
            plane + 4,
            &[plane]
        ),
        plane
    );
}

/// The beams of a sub-window are the beams of the big window that reach it: building a tall
/// window once, or each mesh box's window, gives the same beams for each box.
#[test]
fn windows_built_in_pieces_equal_one_big_window() {
    let (tables, b) = registry(true, false);
    let mut rng = Rng::new(7);
    for _ in 0..8 {
        let column = random_column(&mut rng, &b);
        let planes = random_planes(&mut rng);
        let big = column.build(-160..=159, &planes, &tables, true);
        assert!(!big.is_empty());
        for lo in (-160..=96).step_by(32) {
            let piece = lo..=lo + 63;
            let tile = column.build(piece.clone(), &planes, &tables, true);
            let want: Vec<_> = listed(&big)
                .into_iter()
                .filter(|(_, _, b)| reaches(b.def, b.source, &piece))
                .collect();
            assert_eq!(listed(&tile), want, "window {piece:?}");
            assert!(big.covers(lo, lo + 63) && tile.covers(lo, lo + 63));
            assert!(!tile.covers(lo - 1, lo + 63));
        }
    }
}

#[test]
fn a_retrace_that_needs_a_missing_layer_is_unresolved() {
    let (tables, b) = registry(false, false);
    let mut column = Column::default();
    column.set(0, 10, 0, b.stone);
    let mut tile = column.build(0..=63, &[], &tables, false);
    let old = tile.clone();
    let down = source(&b, &tables, BeamDir::Down, 64);
    column.set(0, 40, 0, down);
    // The trace from y = 40 reads layer 0 (y 31 down to 11): missing.
    let r = tile.edit(0, 0, 40, down, &[], &tables, |cy| {
        (cy != 0).then(|| column.layer(cy, false))
    });
    assert_eq!(r, Retrace::Unresolved);
    assert_eq!(tile, old);
    assert_eq!(tile.version, old.version);
    let r = tile.edit(0, 0, 40, down, &[], &tables, |cy| {
        Some(column.layer(cy, false))
    });
    assert_eq!(r, Retrace::Changed { lo: 11, hi: 40 });
    assert_ne!(tile.version, old.version);
    // A cut needs no layer.
    column.set(0, 20, 0, b.leaves);
    let r = tile.edit(0, 0, 20, b.leaves, &[], &tables, |_| None);
    assert_eq!(r, Retrace::Changed { lo: 11, hi: 20 });
    assert_eq!(tile, column.build(0..=63, &[], &tables, false));
    // Removing the leaves lets it grow back to the stone, and removing the stone to its range.
    column.set(0, 20, 0, b.air);
    let r = tile.edit(0, 0, 20, b.air, &[], &tables, |cy| {
        Some(column.layer(cy, false))
    });
    assert_eq!(r, Retrace::Changed { lo: 11, hi: 20 });
    column.set(0, 10, 0, b.glass);
    let r = tile.edit(0, 0, 10, b.glass, &[], &tables, |cy| {
        Some(column.layer(cy, false))
    });
    assert_eq!(r, Retrace::Changed { lo: -23, hi: 10 });
    assert_eq!(tile.column(0, 0).next().unwrap().lo, 40 - 63);
    assert_eq!(tile, column.build(0..=63, &[], &tables, false));
    // An edit out of reach changes nothing.
    let r = tile.edit(0, 0, 63 + 256, down, &[], &tables, |_| None);
    assert_eq!(r, Retrace::Unchanged);
}

/// Tables without a source read no layer; an empty tile costs no heap.
#[test]
fn without_sources_nothing_is_read() {
    let (registry, _) = common::test_registry(false);
    let tables = LightTables::from_registry(&registry);
    let tile = beam_tile::build(0..=63, &[], &tables, |_| panic!("a layer read"));
    assert!(tile.is_empty() && tile.bounds().is_none());
    assert_eq!(tile.heap_bytes(), 0);
    assert_eq!(tile.column(0, 0).len(), 0);
}

/// The tallest window keeps every offset in 16 bits: sources at both ends of its reach.
#[test]
fn the_tallest_window_holds_its_farthest_beams() {
    let (tables, b) = registry(false, false);
    let (lo, hi) = (-(MAX_WINDOW / 2), MAX_WINDOW / 2 - 1);
    let reach = i64::from(MAX_BEAM) - 1;
    let mut column = Column::default();
    column.set(0, hi + reach, 0, source(&b, &tables, BeamDir::Down, 256));
    column.set(1, lo - reach, 1, source(&b, &tables, BeamDir::Up, 256));
    column.set(
        2,
        hi + reach + 1,
        2,
        source(&b, &tables, BeamDir::Down, 256),
    );
    let tile = column.build(lo..=hi, &[], &tables, true);
    assert_eq!(tile.len(), 2);
    assert_eq!(tile.column(0, 0).next().unwrap().lo, hi);
    assert_eq!(tile.column(1, 1).next().unwrap().hi, lo);
    assert_eq!(tile.bounds(), Some((lo - reach, hi + reach)));
}

/// [`random_column`] with colored glass: panes in air cells, and in some layers a plane of
/// glass (red, or red over light blue).
fn glass_column(rng: &mut Rng, b: &Blocks) -> Column {
    let mut column = random_column(rng, b);
    let ([red, _], blue) = (b.filters.unwrap(), b.blue.unwrap());
    let layers: Vec<i64> = column.chunks.keys().copied().collect();
    for cy in layers {
        if rng.chance(0.5) {
            let y = (cy << CHUNK_SHIFT) + rng.below(31) as i64 + 1;
            let double = rng.chance(0.4);
            for x in 0..32 {
                for z in 0..32 {
                    if column.get(x, y, z).is_air() {
                        column.set(x, y, z, red);
                    }
                    if double && column.get(x, y - 1, z).is_air() {
                        column.set(x, y - 1, z, blue);
                    }
                }
            }
        }
        for _ in 0..200 {
            let (x, z) = (rng.below(32) as u32, rng.below(32) as u32);
            let y = (cy << CHUNK_SHIFT) + rng.below(32) as i64;
            if column.get(x, y, z).is_air() {
                column.set(x, y, z, if rng.chance(0.5) { red } else { blue });
            }
        }
    }
    column
}

/// The filter cells of opacity 0 inside the beams of column `(x, z)`, ascending, by brute force.
fn brute_tints(
    column: &Column,
    beams: &[(u32, u32, Beam)],
    tables: &LightTables,
    x: u32,
    z: u32,
) -> Vec<(i64, [u8; 3])> {
    let cells: std::collections::BTreeSet<i64> = beams
        .iter()
        .filter(|&&(bx, bz, _)| (bx, bz) == (x, z))
        .flat_map(|(_, _, b)| b.lo..=b.hi)
        .collect();
    cells
        .into_iter()
        .filter_map(|y| {
            let s = column.get(x, y, z);
            (tables.filters(s) && !tables.blocks_sky(s)).then(|| (y, tables.light_filter(s)))
        })
        .collect()
}

/// The filtered sky of every beam cell of column `(x, z)` of a colored tile, by brute force:
/// white from each source, filtered past each filter cell, per-channel max over the beams.
fn cell_colors(tile: &BeamTile, x: u32, z: u32) -> BTreeMap<i64, [u8; 3]> {
    let tints: BTreeMap<i64, [u8; 3]> = tile.column_tints(x, z).collect();
    let mut out: BTreeMap<i64, [u8; 3]> = BTreeMap::new();
    for b in tile.column(x, z) {
        let cells: Vec<i64> = if b.def.dir == BeamDir::Down {
            (b.lo..=b.hi).rev().collect()
        } else {
            (b.lo..=b.hi).collect()
        };
        let mut c = [15u8; 3];
        for y in cells {
            if let Some(f) = tints.get(&y) {
                c = [0, 1, 2].map(|k| boxel_voxel::transmit(u16::from(c[k]), f[k]) as u8);
            }
            let e = out.entry(y).or_insert(c);
            *e = [0, 1, 2].map(|k| e[k].max(c[k]));
        }
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    #[test]
    fn colored_build_keeps_the_filter_cells_of_its_beams(seed in any::<u64>(), skip in any::<bool>()) {
        let (tables, b) = registry(seed % 2 == 0, true);
        let mut rng = Rng::new(seed);
        let column = glass_column(&mut rng, &b);
        let window = random_window(&mut rng);
        let planes = random_planes(&mut rng);
        let tile = beam_tile::build_colored(window.clone(), &planes, &tables, |cy| {
            column.layer(cy, skip)
        });
        let plain = column.build(window.clone(), &planes, &tables, skip);
        let want = brute_force(&column, &window, &planes, &tables);
        prop_assert_eq!(listed(&tile), want.clone());
        prop_assert!(!plain.has_tints() && !plain.colored_sky() && tile.colored_sky());
        for x in 0..32 {
            for z in 0..32 {
                let got: Vec<(i64, [u8; 3])> = tile.column_tints(x, z).collect();
                prop_assert_eq!(got, brute_tints(&column, &want, &tables, x, z), "({}, {})", x, z);
            }
        }
    }

    #[test]
    fn a_colored_retrace_after_an_edit_equals_a_rebuild(seed in any::<u64>()) {
        let (tables, b) = registry(true, true);
        let mut rng = Rng::new(seed);
        let mut column = glass_column(&mut rng, &b);
        let window = random_window(&mut rng);
        let planes = random_planes(&mut rng);
        let build = |column: &Column| {
            beam_tile::build_colored(window.clone(), &planes, &tables, |cy| column.layer(cy, false))
        };
        let mut tile = build(&column);
        let mut edits = palette(&b);
        edits.extend(&b.sources);
        let ([red, cyan], blue) = (b.filters.unwrap(), b.blue.unwrap());
        edits.extend([red, red, blue, blue, cyan]);
        for _ in 0..40 {
            let (x, z) = (3 + rng.below(2) as u32, 7);
            let beams: Vec<Beam> = tile.column(x, z).collect();
            let y = if !beams.is_empty() && rng.chance(0.8) {
                let b = beams[rng.below(beams.len() as u64) as usize];
                let inside = b.lo + rng.below((b.hi - b.lo + 1) as u64) as i64;
                [b.lo - 1, b.hi + 1, b.lo, b.hi, inside, inside][rng.below(6) as usize]
            } else {
                let span = (window.end() - window.start() + 600) as u64;
                window.start() - 300 + rng.below(span) as i64
            };
            let s = edits[rng.below(edits.len() as u64) as usize];
            let before = cell_colors(&tile, x, z);
            column.set(x, y, z, s);
            let retrace = tile.edit(x, z, y, s, &planes, &tables, |cy| Some(column.layer(cy, false)));
            let rebuilt = build(&column);
            prop_assert!(tile == rebuilt, "edit {s:?} at ({x}, {y}, {z})");
            let after = cell_colors(&tile, x, z);
            let changed: Vec<i64> = before
                .keys()
                .chain(after.keys())
                .filter(|y| before.get(y) != after.get(y))
                .copied()
                .collect::<std::collections::BTreeSet<i64>>()
                .into_iter()
                .collect();
            match retrace {
                Retrace::Unchanged => prop_assert!(changed.is_empty(), "{changed:?}"),
                Retrace::Changed { lo, hi } => {
                    prop_assert!(!changed.is_empty());
                    prop_assert_eq!((lo, hi), (changed[0], changed[changed.len() - 1]));
                }
                Retrace::Unresolved => prop_assert!(false, "every layer is there"),
            }
        }
    }
}
