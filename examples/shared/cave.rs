//! `--cave`: a camera in open cave space, found from the world's generator (`stress` and
//! `headless_world`).
//!
//! Cave culling (ADR 0022) culls nothing while the eye is in rock: the flying camera does not
//! collide, and the old underground runs (`--camera 16,-1000,16,...`) started inside the stone.
//! This finds, near a seed position, the air voxel nearest to it that has air on every side
//! ([`CLEARANCE`]), lies [`MIN_DEPTH`] or more under the surface and has a route of [`MIN_PATH`]
//! or more such voxels (not a pocket), and that route for the `path` scenario. The answer depends only on the generator, the
//! surface heights and the seed position: the same run finds the same cave.
#![allow(
    dead_code,
    reason = "each example uses its own part (headless_world has no path scenario)"
)]

use boxel::prelude::*;
use boxel::voxel::{BlockPos, BlockRegistry, BlockState, ChunkKey, ChunkPos, LocalPos};
use std::collections::VecDeque;

/// Chunks searched around the seed's chunk on each side (a cube of 7 x 7 x 7 chunks).
pub const SEARCH_CHUNKS: i64 = 3;
/// Air voxels around the eye on every side, diagonals included: with 2, the eye stays 1.5
/// voxels or more from anything that is not air, anywhere on the path.
pub const CLEARANCE: i64 = 2;
/// The eye is at least this many voxels under the surface of its column (a cave, not a valley).
pub const MIN_DEPTH: i64 = 16;
/// Shortest route the eye's cave must offer (steps between face neighbors): smaller caves are
/// skipped.
pub const MIN_PATH: u32 = 64;
/// Longest path, in voxels (steps between face neighbors).
pub const MAX_PATH: u32 = 600;
/// Side of the searched cube, in voxels.
const SIDE: i64 = (2 * SEARCH_CHUNKS + 1) * 32;

/// A pose in a cave.
#[derive(Clone, Debug, PartialEq)]
pub struct Cave {
    /// The center of the air voxel nearest to the seed with [`CLEARANCE`] air voxels on every
    /// side, [`MIN_DEPTH`] or more under the surface and a route of [`MIN_PATH`] such voxels.
    pub eye: DVec3,
    /// Voxels between the eye's voxel and the surface of its column.
    pub depth: i64,
    /// The longest route (up to [`MAX_PATH`] steps) from the eye through such voxels, as the
    /// centers of face-neighbor voxels, the eye first: the shortest path to the farthest one.
    pub path: Vec<DVec3>,
}

impl Cave {
    /// Chunks the path enters after the eye's.
    pub fn path_chunks(&self) -> usize {
        let chunk = |p: &DVec3| ChunkPos::containing(BlockPos::from_world(*p));
        self.path
            .windows(2)
            .filter(|w| chunk(&w[0]) != chunk(&w[1]))
            .count()
    }

    /// One line for logs and summaries.
    pub fn describe(&self) -> String {
        format!(
            "eye={},{},{} depth={} clearance={CLEARANCE} path={} voxels, {} chunk borders",
            self.eye.x,
            self.eye.y,
            self.eye.z,
            self.depth,
            self.path.len().saturating_sub(1),
            self.path_chunks()
        )
    }
}

/// Searches the cube of [`SEARCH_CHUNKS`] around the chunk of `seed` with `generator`'s blocks
/// and `surface`'s heights (the y of the top solid block of a column).
pub fn find(
    generator: &dyn WorldGenerator,
    registry: &BlockRegistry,
    surface: impl Fn(i64, i64) -> i64,
    seed: DVec3,
) -> Result<Cave, String> {
    let center = ChunkPos::containing(BlockPos::from_world(seed));
    let min = BlockPos::new(
        (center.x - SEARCH_CHUNKS) * 32,
        (center.y - SEARCH_CHUNKS) * 32,
        (center.z - SEARCH_CHUNKS) * 32,
    );
    let s = SIDE as usize;
    let index = |x: i64, y: i64, z: i64| ((y as usize * s + z as usize) * s) + x as usize;
    // Air voxels of the cube.
    let mut air = vec![false; s * s * s];
    let n = 2 * SEARCH_CHUNKS + 1;
    for cy in 0..n {
        for cz in 0..n {
            for cx in 0..n {
                let pos = ChunkPos::new(
                    center.x - SEARCH_CHUNKS + cx,
                    center.y - SEARCH_CHUNKS + cy,
                    center.z - SEARCH_CHUNKS + cz,
                );
                let data = generator.generate(ChunkKey::new(pos), registry);
                if data.as_uniform().is_some_and(|b| b != BlockState::AIR) {
                    continue;
                }
                for y in 0..32u32 {
                    for z in 0..32u32 {
                        for x in 0..32u32 {
                            if data.get(LocalPos::new(x, y, z)) == BlockState::AIR {
                                air[index(
                                    cx * 32 + i64::from(x),
                                    cy * 32 + i64::from(y),
                                    cz * 32 + i64::from(z),
                                )] = true;
                            }
                        }
                    }
                }
            }
        }
    }
    let clear = erode(&air, s, CLEARANCE as usize);
    drop(air);
    let heights: Vec<i64> = (0..SIDE * SIDE)
        .map(|i| surface(min.x + i % SIDE, min.z + i / SIDE))
        .collect();
    let depth = |x: i64, y: i64, z: i64| heights[(z * SIDE + x) as usize] - (min.y + y);
    let unindex = |i: usize| [(i % s) as i64, (i / (s * s)) as i64, (i / s % s) as i64];
    let usable: Vec<bool> = clear
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            let [x, y, z] = unindex(i);
            c && depth(x, y, z) >= MIN_DEPTH
        })
        .collect();
    drop(clear);
    // Usable voxels by distance to the seed (a stable sort: y, z, x order among equals).
    let local_seed = seed - DVec3::new(min.x as f64, min.y as f64, min.z as f64);
    let mut candidates: Vec<(f64, usize)> = usable
        .iter()
        .enumerate()
        .filter(|&(_, &u)| u)
        .map(|(i, _)| {
            let [x, y, z] = unindex(i);
            let d = (DVec3::new(x as f64, y as f64, z as f64) + 0.5 - local_seed).length_squared();
            (d, i)
        })
        .collect();
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
    // From the nearest candidate, breadth-first through usable face neighbors in a fixed order:
    // the farthest voxel found first (up to MAX_PATH steps) ends the path. A cave too small for
    // MIN_PATH steps was searched whole: its voxels are skipped as candidates.
    const STEPS: [[i64; 3]; 6] = [
        [1, 0, 0],
        [-1, 0, 0],
        [0, 1, 0],
        [0, -1, 0],
        [0, 0, 1],
        [0, 0, -1],
    ];
    let neighbor = |i: usize, [dx, dy, dz]: [i64; 3]| {
        let [x, y, z] = unindex(i);
        let (x, y, z) = (x + dx, y + dy, z + dz);
        [x, y, z]
            .iter()
            .all(|c| (0..SIDE).contains(c))
            .then(|| index(x, y, z))
    };
    let mut dist = vec![u16::MAX; s * s * s];
    let mut queue = VecDeque::new();
    for &(_, start) in &candidates {
        if dist[start] != u16::MAX {
            continue;
        }
        dist[start] = 0;
        queue.push_back(start);
        let mut far = start;
        while let Some(v) = queue.pop_front() {
            let steps = dist[v];
            if steps > dist[far] {
                far = v;
            }
            if u32::from(steps) == MAX_PATH {
                continue;
            }
            for d in STEPS {
                if let Some(w) = neighbor(v, d).filter(|&w| usable[w] && dist[w] == u16::MAX) {
                    dist[w] = steps + 1;
                    queue.push_back(w);
                }
            }
        }
        if u32::from(dist[far]) < MIN_PATH {
            continue;
        }
        let world = |i: usize| {
            let [x, y, z] = unindex(i);
            DVec3::new(
                (min.x + x) as f64 + 0.5,
                (min.y + y) as f64 + 0.5,
                (min.z + z) as f64 + 0.5,
            )
        };
        let mut path = vec![world(far)];
        let mut at = far;
        while at != start {
            let back = dist[at] - 1;
            at = STEPS
                .iter()
                .filter_map(|&d| neighbor(at, d))
                .find(|&w| dist[w] == back)
                .expect("a neighbor one step nearer");
            path.push(world(at));
        }
        path.reverse();
        let [x, y, z] = unindex(start);
        return Ok(Cave {
            eye: world(start),
            depth: depth(x, y, z),
            path,
        });
    }
    Err(format!(
        "--cave: no air voxel with {CLEARANCE} voxels of air around it, {MIN_DEPTH} or more under \
         the surface and a route of {MIN_PATH} such voxels, within {SEARCH_CHUNKS} chunks of \
         {},{},{}",
        seed.x, seed.y, seed.z
    ))
}

/// `cube` (side `s`) eroded by `r` voxels on every axis: true where the whole cube of side
/// `2r + 1` around the voxel is true (false within `r` of the border).
fn erode(cube: &[bool], s: usize, r: usize) -> Vec<bool> {
    let mut a = cube.to_vec();
    let mut b = vec![false; a.len()];
    for stride in [1, s, s * s] {
        for (i, out) in b.iter_mut().enumerate() {
            let c = i / stride % s;
            *out = c >= r && c + r < s && (0..=2 * r).all(|k| a[i + k * stride - r * stride]);
        }
        std::mem::swap(&mut a, &mut b);
    }
    a
}

/// The point `t` voxels along `path` (from its first point), going back and forth between its
/// ends; and the direction of travel there (`+1` or `-1` along the path).
pub fn along(path: &[DVec3], t: f64) -> (DVec3, f64) {
    let len = path.len().saturating_sub(1) as f64;
    if len == 0.0 {
        return (path.first().copied().unwrap_or(DVec3::ZERO), 1.0);
    }
    let round = t.rem_euclid(2.0 * len);
    let (s, dir) = if round <= len {
        (round, 1.0)
    } else {
        (2.0 * len - round, -1.0)
    };
    (at(path, s), dir)
}

/// The point `s` voxels from the start of `path` (clamped to its ends).
fn at(path: &[DVec3], s: f64) -> DVec3 {
    let last = path.len() - 1;
    let s = s.clamp(0.0, last as f64);
    let i = (s.floor() as usize).min(last.saturating_sub(1));
    let f = s - i as f64;
    if i + 1 > last {
        return path[last];
    }
    path[i].lerp(path[i + 1], f)
}

/// Where a camera `t` voxels along `path` looks: toward the average of the path a few voxels
/// ahead in its direction of travel (smooth over the voxel steps). Returns `(yaw, pitch)` in
/// degrees, as `--camera` takes them (yaw 0 looks along -z, -90 along +x).
pub fn look(path: &[DVec3], t: f64) -> (f64, f64) {
    let (eye, dir) = along(path, t);
    let len = path.len().saturating_sub(1) as f64;
    let round = t.rem_euclid(2.0 * len.max(1.0));
    let s = if round <= len {
        round
    } else {
        2.0 * len - round
    };
    // The mean of the points 6 to 18 voxels ahead.
    let mut ahead = DVec3::ZERO;
    for k in 6..=18 {
        ahead += at(path, s + dir * f64::from(k));
    }
    let mut d = ahead / 13.0 - eye;
    if d.length_squared() < 4.0 {
        // At an end of the path: keep looking the way it came.
        d = eye - at(path, s - dir * 12.0);
    }
    if d.length_squared() < 1e-9 {
        return (-90.0, 0.0);
    }
    let d = d.normalize();
    let yaw = (-d.x).atan2(-d.z).to_degrees();
    let pitch = d.y.clamp(-1.0, 1.0).asin().to_degrees().clamp(-60.0, 60.0);
    (yaw, pitch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erosion_keeps_voxels_with_air_on_every_side() {
        let s = 7;
        let mut cube = vec![true; s * s * s];
        // One solid voxel at (5, 3, 3).
        cube[(3 * s + 3) * s + 5] = false;
        let out = erode(&cube, s, 1);
        let at = |x: usize, y: usize, z: usize| out[(y * s + z) * s + x];
        assert!(at(3, 3, 3), "two voxels from the solid one");
        assert!(
            !at(4, 3, 3) && !at(4, 4, 4),
            "next to it, diagonals included"
        );
        assert!(!at(0, 3, 3) && !at(3, 6, 3), "on the border");
        // The 5 x 5 x 5 inside, without the 2 x 3 x 3 next to the solid voxel.
        assert_eq!(out.iter().filter(|&&b| b).count(), 125 - 18);
    }

    #[test]
    fn a_path_goes_back_and_forth() {
        let path: Vec<DVec3> = (0..5).map(|i| DVec3::new(f64::from(i), 0.0, 0.0)).collect();
        assert_eq!(along(&path, 0.0), (DVec3::ZERO, 1.0));
        assert_eq!(along(&path, 2.5), (DVec3::new(2.5, 0.0, 0.0), 1.0));
        assert_eq!(along(&path, 5.0), (DVec3::new(3.0, 0.0, 0.0), -1.0));
        assert_eq!(along(&path, 8.0).0, DVec3::ZERO);
        assert_eq!(along(&path, 9.0), (DVec3::new(1.0, 0.0, 0.0), 1.0));
        let (yaw, pitch) = look(&path, 1.0);
        assert!(
            (yaw + 90.0).abs() < 1e-9 && pitch.abs() < 1e-9,
            "{yaw} {pitch}"
        );
        let (yaw, _) = look(&path, 5.0);
        assert!((yaw - 90.0).abs() < 1e-9, "back along -x: {yaw}");
    }

    /// The default overworld near the old underground camera: an open eye deep in a cave, and a
    /// path of face neighbors that all keep the clearance.
    #[test]
    fn the_overworld_has_a_cave_near_the_old_underground_camera() {
        let registry = BlockRegistry::default_blocks();
        let generator = OverworldGenerator::default();
        let cave = find(
            &generator,
            &registry,
            |x, z| generator.height_at(x, z),
            DVec3::new(16.0, -1000.0, 16.0),
        )
        .expect("a cave");
        let mut chunks = std::collections::HashMap::new();
        let mut block = |p: DVec3| {
            let (chunk, local) = BlockPos::from_world(p).split();
            chunks
                .entry(chunk)
                .or_insert_with(|| generator.generate(ChunkKey::new(chunk), &registry))
                .get(local)
        };
        let mut clear = |p: DVec3| {
            (-CLEARANCE..=CLEARANCE).all(|dy| {
                (-CLEARANCE..=CLEARANCE).all(|dz| {
                    (-CLEARANCE..=CLEARANCE).all(|dx| {
                        block(p + DVec3::new(dx as f64, dy as f64, dz as f64)) == BlockState::AIR
                    })
                })
            })
        };
        assert!(clear(cave.eye), "{}", cave.describe());
        assert!(cave.depth >= MIN_DEPTH);
        assert_eq!(cave.path[0], cave.eye);
        assert!(cave.path.len() > MIN_PATH as usize, "{}", cave.describe());
        for w in cave.path.windows(2) {
            assert_eq!(w[0].distance(w[1]), 1.0);
        }
        for p in cave.path.iter().step_by(17) {
            assert!(clear(*p), "{p}");
        }
    }
}
