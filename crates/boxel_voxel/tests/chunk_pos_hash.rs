//! Through foldhash (the world's chunk index) the hashes of `ChunkPos` spread like random ones
//! over the position sets a world holds (grids, columns, rows, negative and far coordinates, views
//! `2^32` chunks apart): no shared 64-bit hash, bucket use and the hashbrown tag (top 7 bits) as
//! even as random hashing gives. Nor does any straight line of positions share one hash (a hash
//! that feeds foldhash one 128-bit word made of two halves hashes a whole line to 0 when one half
//! matches foldhash's seed).
use boxel_voxel::{ChunkPos, MAX_CHUNK_COORD};
use std::collections::HashSet;
use std::hash::BuildHasher;

fn hash(p: ChunkPos) -> u64 {
    foldhash::fast::FixedState::default().hash_one(p)
}

/// A box of positions `x0..x0 + nx` (and so on), x innermost.
fn grid((x0, nx): (i64, i64), (y0, ny): (i64, i64), (z0, nz): (i64, i64)) -> Vec<ChunkPos> {
    let mut out = Vec::new();
    for y in y0..y0 + ny {
        for z in z0..z0 + nz {
            for x in x0..x0 + nx {
                out.push(ChunkPos::new(x, y, z));
            }
        }
    }
    out
}

/// Asserts that the hashes of `positions` look random to a hash table.
fn check(name: &str, positions: &[ChunkPos]) {
    let n = positions.len();
    let distinct: HashSet<ChunkPos> = positions.iter().copied().collect();
    assert_eq!(distinct.len(), n, "{name}: duplicate positions in the set");
    let hashes: HashSet<u64> = positions.iter().map(|&p| hash(p)).collect();
    assert_eq!(hashes.len(), n, "{name}: positions share a 64-bit hash");

    // Bucket index (low bits) of a table with at least twice as many buckets as positions:
    // random hashing leaves `m * e^(-n/m)` buckets empty, and no bucket holds many.
    let m = (2 * n).next_power_of_two();
    let mut buckets = vec![0u32; m];
    for &h in &hashes {
        buckets[usize::try_from(h).expect("64-bit usize") & (m - 1)] += 1;
    }
    let used = buckets.iter().filter(|&&c| c > 0).count();
    #[expect(clippy::cast_precision_loss, reason = "small counts")]
    let expected = m as f64 * (1.0 - (-(n as f64) / m as f64).exp());
    #[expect(clippy::cast_precision_loss, reason = "small counts")]
    let used_f = used as f64;
    assert!(
        used_f > 0.98 * expected,
        "{name}: {used} of {m} buckets used, random hashing uses {expected:.0}"
    );
    let fullest = buckets.iter().copied().max().unwrap_or(0);
    assert!(fullest <= 9, "{name}: a bucket holds {fullest} positions");

    // hashbrown's tag (top 7 bits): every value about as often.
    let mut tags = [0usize; 128];
    for &h in &hashes {
        tags[usize::try_from(h >> 57).expect("7 bits")] += 1;
    }
    let mean = n / 128;
    for (tag, &count) in tags.iter().enumerate() {
        assert!(
            count.abs_diff(mean) * 4 <= mean,
            "{name}: tag {tag} on {count} positions (mean {mean})"
        );
    }
}

#[test]
fn views_around_the_origin() {
    // A radius-64 view (horizontal square) 16 layers deep, centered on the origin.
    check("grid", &grid((-64, 128), (-8, 16), (-64, 128)));
    // Flat and thin boxes.
    check("layer", &grid((-256, 512), (0, 1), (-256, 512)));
    check("slab x", &grid((-4096, 8192), (-2, 4), (-4, 8)));
}

#[test]
fn columns_and_rows() {
    let column: Vec<ChunkPos> = (-(1 << 17)..1 << 17)
        .map(|y| ChunkPos::new(3, y, -7))
        .collect();
    check("column", &column);
    let row_x: Vec<ChunkPos> = (-(1 << 17)..1 << 17)
        .map(|x| ChunkPos::new(x, 2, 0))
        .collect();
    check("row x", &row_x);
    let row_z: Vec<ChunkPos> = (-(1 << 17)..1 << 17)
        .map(|z| ChunkPos::new(0, 2, z))
        .collect();
    check("row z", &row_z);
}

#[test]
fn negative_and_far_coordinates() {
    check("negative", &grid((-200, 128), (-40, 16), (-300, 128)));
    let m = MAX_CHUNK_COORD;
    check("far +", &grid((m - 127, 128), (m - 15, 16), (m - 127, 128)));
    check("far -", &grid((-m, 128), (-m, 16), (-m, 128)));
    check("far mixed", &grid((m - 127, 128), (-m, 16), (-m, 128)));
    // Across the former i32 edge of chunk coordinates.
    check(
        "2^31",
        &grid(((1 << 31) - 64, 128), (-8, 16), ((1 << 31) - 64, 128)),
    );
}

#[test]
fn views_far_apart_do_not_collide() {
    // Views exactly 2^32 (and multiples) chunks apart: the low half of the word repeats with `y`
    // flipped, the high half tells them apart.
    let mut views = Vec::new();
    for k in 0..8i64 {
        views.extend(grid((k << 32, 64), (-8, 16), (0, 64)));
        views.extend(grid((-(k + 1) << 32, 64), (-8, 16), ((k + 1) << 32, 64)));
    }
    check("views 2^32 apart", &views);
    // Views on a regular lattice across the whole world.
    let mut lattice = Vec::new();
    for i in -4..4i64 {
        for j in -4..4i64 {
            lattice.extend(grid((i << 53, 16), (j << 50, 8), (j << 53, 16)));
        }
    }
    check("lattice", &lattice);
}

/// All 64-bit hashes of `line` differ.
fn assert_distinct(name: &str, line: impl Iterator<Item = ChunkPos>) {
    let mut seen = HashSet::new();
    for p in line {
        assert!(seen.insert(hash(p)), "{name}: {p:?} shares a hash");
    }
}

#[test]
fn lines_do_not_share_a_hash() {
    // Lines that hashed to 0 when `ChunkPos` fed foldhash the word `x ^ (y << 32)`,
    // `z ^ y * 0x9e3779b97f4a7c15` (the halves matched foldhash's seeds).
    assert_distinct(
        "z row",
        (-2048..2048).map(|z| ChunkPos::new(3_964_562_569, 137_296_536, z)),
    );
    assert_distinct(
        "x row",
        (-2048..2048).map(|x| ChunkPos::new(x, 36, -44_693_351_583_899_095)),
    );
    assert_distinct(
        "x row, y < 0",
        (-2048..2048).map(|x| ChunkPos::new(x, -36, 44_693_351_583_899_089)),
    );
    // Rows along every axis and the diagonals through a few starting points, near and far.
    let m = MAX_CHUNK_COORD;
    for start in [
        ChunkPos::new(0, 0, 0),
        ChunkPos::new(-7, 3, 11),
        ChunkPos::new(1 << 40, -(1 << 33), 5),
        ChunkPos::new(m - 4096, -m, m - 4096),
    ] {
        for step in [
            (1, 0, 0),
            (0, 1, 0),
            (0, 0, 1),
            (1, 1, 0),
            (1, 0, 1),
            (0, 1, 1),
            (1, -1, 1),
        ] {
            assert_distinct(
                &format!("{start:?} + t * {step:?}"),
                (0..4096).map(|t| {
                    ChunkPos::new(
                        start.x + t * step.0,
                        start.y + t * step.1,
                        start.z + t * step.2,
                    )
                }),
            );
        }
    }
}
