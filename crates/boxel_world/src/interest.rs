//! Pure interest math: which chunks each viewer wants, when a slot may unload and job
//! priorities. No ECS, no slot state (unit tested on its own).
use crate::Interest;
use boxel_voxel::ChunkPos;
use std::collections::BTreeMap;
use std::ops::RangeInclusive;

/// Radii are clamped to this many chunks (about 131 km) so the `i32` column math can never
/// overflow; real view distances are two orders of magnitude smaller.
pub(crate) const MAX_RADIUS: u32 = 4096;

/// A viewer as the world sees it: the chunk it is in and its mesh radius.
pub(crate) type ViewerChunk = (ChunkPos, u32);

/// Horizontal column offsets (`[dx, dz]`, row-major: `dz` outer, `dx` inner) around one viewer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ColumnOffsets {
    /// Columns that get meshes: `dx^2 + dz^2 <= radius^2`.
    pub(crate) mesh: Vec<[i32; 2]>,
    /// Columns that get data: `dx^2 + dz^2 <= (radius + 1)^2`, plus every xz neighbor of a mesh
    /// column (a mesh needs all 26 neighbors; the circle alone misses diagonal neighbors, for
    /// example `(1, 13)` of the mesh column `(0, 12)` at radius 12). Always a superset of `mesh`.
    pub(crate) data: Vec<[i32; 2]>,
}

/// Column offsets of a viewer with mesh radius `radius` (clamped to [`MAX_RADIUS`]).
pub(crate) fn column_offsets(radius: u32) -> ColumnOffsets {
    let r = i64::from(radius.min(MAX_RADIUS));
    let mesh_sq = r * r;
    let data_sq = (r + 1) * (r + 1);
    let in_mesh = |dx: i64, dz: i64| dx * dx + dz * dz <= mesh_sq;
    let near_mesh = |dx: i64, dz: i64| {
        (-1..=1).any(|ax: i64| (-1..=1).any(|az: i64| in_mesh(dx + ax, dz + az)))
    };
    // Every data column is within r + sqrt(2) < r + 2 of the viewer.
    let extent = r + 2;
    let mut out = ColumnOffsets {
        mesh: Vec::new(),
        data: Vec::new(),
    };
    for dz in -extent..=extent {
        for dx in -extent..=extent {
            let column = [narrow(dx), narrow(dz)];
            if in_mesh(dx, dz) {
                out.mesh.push(column);
            }
            if dx * dx + dz * dz <= data_sq || near_mesh(dx, dz) {
                out.data.push(column);
            }
        }
    }
    out
}

/// Every chunk wanted by at least one viewer: data columns x `vertical` get [`Interest::DATA`],
/// mesh columns x `vertical` also get [`Interest::MESH`]. Sorted by position (deterministic
/// slot creation order).
pub(crate) fn wanted_chunks(
    viewers: &[ViewerChunk],
    vertical: &RangeInclusive<i32>,
) -> BTreeMap<ChunkPos, Interest> {
    let mut wanted = BTreeMap::new();
    for &(center, radius) in viewers {
        let columns = column_offsets(radius);
        let mut mark = |offsets: &[[i32; 2]], interest: Interest| {
            for &[dx, dz] in offsets {
                for y in vertical.clone() {
                    let pos =
                        ChunkPos::new(center.x.saturating_add(dx), y, center.z.saturating_add(dz));
                    *wanted.entry(pos).or_insert(Interest::empty()) |= interest;
                }
            }
        };
        mark(&columns.data, Interest::DATA);
        mark(&columns.mesh, Interest::DATA | Interest::MESH);
    }
    wanted
}

/// True when `pos` is farther than `radius + 1 + hysteresis` (xz) from every viewer: an unwanted
/// slot there may unload. With no viewers everything is beyond.
pub(crate) fn beyond_hysteresis(pos: ChunkPos, viewers: &[ViewerChunk], hysteresis: u32) -> bool {
    viewers.iter().all(|&(center, radius)| {
        let limit = i64::from(radius.min(MAX_RADIUS)) + 1 + i64::from(hysteresis.min(MAX_RADIUS));
        pos.distance_sq_xz(center) > limit * limit
    })
}

/// Job priority: squared 3D chunk distance to the nearest viewer, capped to `u32` (`u32::MAX`
/// without viewers).
pub(crate) fn priority(pos: ChunkPos, viewers: &[ViewerChunk]) -> u32 {
    viewers
        .iter()
        .map(|&(center, _)| pos.distance_sq(center))
        .min()
        .map_or(u32::MAX, |d| u32::try_from(d).unwrap_or(u32::MAX))
}

/// `i64` -> `i32` for offsets bounded by `MAX_RADIUS + 2`.
fn narrow(v: i64) -> i32 {
    i32::try_from(v).expect("column offsets are bounded by MAX_RADIUS + 2")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn set(offsets: &[[i32; 2]]) -> BTreeSet<[i32; 2]> {
        offsets.iter().copied().collect()
    }

    /// Structural properties every radius must satisfy.
    fn check_properties(radius: u32) {
        let c = column_offsets(radius);
        let r = i64::from(radius);
        let (mesh, data) = (set(&c.mesh), set(&c.data));
        assert_eq!(mesh.len(), c.mesh.len(), "no duplicate mesh columns");
        assert_eq!(data.len(), c.data.len(), "no duplicate data columns");
        for &[dx, dz] in &mesh {
            let d2 = i64::from(dx * dx + dz * dz);
            assert!(d2 <= r * r);
            // Closure: all 8 xz neighbors of a mesh column have data.
            for ax in -1..=1 {
                for az in -1..=1 {
                    assert!(
                        data.contains(&[dx + ax, dz + az]),
                        "radius {radius}: neighbor of mesh column ({dx}, {dz}) has no data"
                    );
                }
            }
        }
        // Exact circles.
        let extent = i32::try_from(radius).expect("small radius") + 3;
        for dz in -extent..=extent {
            for dx in -extent..=extent {
                let d2 = i64::from(dx * dx + dz * dz);
                assert_eq!(mesh.contains(&[dx, dz]), d2 <= r * r);
                if d2 <= (r + 1) * (r + 1) {
                    assert!(
                        data.contains(&[dx, dz]),
                        "data circle contains ({dx}, {dz})"
                    );
                }
                if data.contains(&[dx, dz]) {
                    // Every data column is wanted for a reason: the circle or a mesh neighbor.
                    let near =
                        (-1..=1).any(|ax| (-1..=1).any(|az| mesh.contains(&[dx + ax, dz + az])));
                    assert!(d2 <= (r + 1) * (r + 1) || near);
                }
            }
        }
    }

    #[test]
    fn radius_0() {
        let c = column_offsets(0);
        assert_eq!(c.mesh, vec![[0, 0]]);
        // The 3x3 square: the plus-shaped (r + 1) circle plus the diagonal mesh neighbors.
        let mut expected = Vec::new();
        for dz in -1..=1 {
            for dx in -1..=1 {
                expected.push([dx, dz]);
            }
        }
        assert_eq!(c.data, expected);
        check_properties(0);
    }

    #[test]
    fn radius_1() {
        let c = column_offsets(1);
        assert_eq!(
            set(&c.mesh),
            set(&[[0, 0], [1, 0], [-1, 0], [0, 1], [0, -1]])
        );
        // 5x5 square without its 4 corners: the radius-2 circle has 13 columns, the diagonal
        // neighbors of the mesh plus add (+-1, +-2) and (+-2, +-1).
        assert_eq!(c.data.len(), 21);
        for corner in [[2, 2], [-2, 2], [2, -2], [-2, -2]] {
            assert!(!c.data.contains(&corner));
        }
        check_properties(1);
    }

    #[test]
    fn radius_12() {
        let c = column_offsets(12);
        // Gauss circle numbers: N(12) = 441, N(13) = 529.
        assert_eq!(c.mesh.len(), 441);
        assert!(
            c.data.len() > 529,
            "the data set extends the radius-13 circle"
        );
        // The diagonal neighbor of the mesh column (0, 12) lies outside the radius-13 circle
        // (1 + 169 = 170 > 169) and must still be loaded.
        assert!(c.mesh.contains(&[0, 12]));
        assert!(c.data.contains(&[1, 13]));
        assert!(!c.data.contains(&[0, 14]));
        check_properties(12);
    }

    #[test]
    fn small_radii_properties() {
        for r in 0..=20 {
            check_properties(r);
        }
    }

    #[test]
    fn wanted_set_spans_vertical_range_and_unions_viewers() {
        let vertical = -2..=1;
        let a = ChunkPos::new(0, 5, 0);
        let w = wanted_chunks(&[(a, 0)], &vertical);
        assert_eq!(w.len(), 9 * 4);
        for (pos, interest) in &w {
            assert!(vertical.contains(&pos.y));
            assert!(interest.contains(Interest::DATA));
            assert_eq!(
                interest.contains(Interest::MESH),
                pos.x == 0 && pos.z == 0,
                "{pos:?}"
            );
        }
        // A second viewer two chunks away: union of both sets, MESH wins over DATA.
        let b = ChunkPos::new(2, 0, 0);
        let w2 = wanted_chunks(&[(a, 0), (b, 0)], &vertical);
        assert_eq!(w2.len(), 15 * 4);
        assert_eq!(
            w2[&ChunkPos::new(1, 0, 0)],
            Interest::DATA,
            "shared data column"
        );
        assert_eq!(
            w2[&ChunkPos::new(2, -2, 0)],
            Interest::DATA | Interest::MESH
        );
        assert!(wanted_chunks(&[], &vertical).is_empty());
    }

    #[test]
    fn unload_threshold_and_priority() {
        let v = [(ChunkPos::new(0, 0, 0), 2)];
        // limit = 2 + 1 + 2 = 5 chunks.
        assert!(!beyond_hysteresis(ChunkPos::new(5, 9, 0), &v, 2));
        assert!(!beyond_hysteresis(ChunkPos::new(3, 0, 4), &v, 2));
        assert!(beyond_hysteresis(ChunkPos::new(4, 0, 4), &v, 2));
        assert!(beyond_hysteresis(ChunkPos::new(6, 0, 0), &v, 2));
        // Second viewer: limit 0 + 1 + 2 = 3 chunks.
        let two = [v[0], (ChunkPos::new(8, 0, 0), 0)];
        assert!(
            !beyond_hysteresis(ChunkPos::new(6, 0, 0), &two, 2),
            "near the 2nd viewer"
        );
        assert!(beyond_hysteresis(ChunkPos::new(6, 0, 3), &two, 2));
        assert!(beyond_hysteresis(ChunkPos::new(0, 0, 0), &[], 2));

        assert_eq!(priority(ChunkPos::new(1, 2, 3), &v), 14);
        assert_eq!(priority(ChunkPos::new(9, 0, 0), &two), 1);
        assert_eq!(priority(ChunkPos::new(1, 2, 3), &[]), u32::MAX);
        let far = [(ChunkPos::new(1 << 20, 0, 0), 0)];
        assert_eq!(priority(ChunkPos::new(0, 0, 0), &far), u32::MAX, "capped");
    }
}
