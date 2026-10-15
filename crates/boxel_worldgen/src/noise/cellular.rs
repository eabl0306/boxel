//! Cellular noise ([`Cellular`]): one jittered site per cell of a power-of-two lattice, with a
//! stable id (its cell), the nearest and second nearest sites of any point ([`Nearest`], F1 and
//! F2) and the Voronoi cell of a site with exact vertices ([`VoronoiCell`]).
//!
//! Everything is integer arithmetic: sites are hashed into their cells, distances are compared
//! squared in `u128`, ties go to the smaller id, and the Voronoi vertices are exact rationals
//! ([`ExactPoint`]). So a point query, a chunk grid and a LOD corner find the same sites at any
//! coordinate of magnitude under `2^62`.

use super::{hash2_mixed, mix_seed, splitmix64};
use crate::exact::{ExactPoint, MAX_SPREAD, circumcenter, dist_sq, incircle, orient};

/// Salt of the sites' x offsets.
const SALT_X: u64 = 0x4345_4c4c_5f58_0001;
/// Salt of the sites' z offsets.
const SALT_Z: u64 = 0x4345_4c4c_5f5a_0002;

/// The id of a site: the lattice cell `(i, j)` it was jittered into. It does not depend on the
/// point that found it, so every query that reaches a site names it the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SiteId {
    /// Cell index along x (`x >> shift` of every point of the cell).
    pub i: i64,
    /// Cell index along z.
    pub j: i64,
}

/// A site: its id and its block position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Site {
    /// The cell it belongs to.
    pub id: SiteId,
    /// Block x.
    pub x: i64,
    /// Block z.
    pub z: i64,
}

impl Site {
    /// `(x, z)`.
    pub const fn pos(self) -> (i64, i64) {
        (self.x, self.z)
    }
}

/// The two nearest sites of a point, ordered by `(squared distance, id)`: F1 is the nearest
/// (ties to the smaller id), F2 the next one (its distance may equal F1's).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Nearest {
    /// The nearest site.
    pub f1: Site,
    /// Its squared distance.
    pub d1: u128,
    /// The second nearest site.
    pub f2: Site,
    /// Its squared distance.
    pub d2: u128,
}

/// An edge of a [`VoronoiCell`]: the part of the bisector with `neighbor` that bounds the cell,
/// from `start` to `end` counterclockwise around the site (x to the right, z up). Cocircular
/// sites (rare with hashed jitter) give an edge of length 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VoronoiEdge {
    /// The site on the other side.
    pub neighbor: Site,
    /// The vertex shared with the previous edge.
    pub start: ExactPoint,
    /// The vertex shared with the next edge.
    pub end: ExactPoint,
}

/// The Voronoi cell of a site: its edges in counterclockwise order, each starting where the
/// previous one ends. Each vertex is the exact circumcenter of the site and two neighbors.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VoronoiCell {
    /// The site.
    pub site: Site,
    /// The edges, counterclockwise.
    pub edges: Vec<VoronoiEdge>,
}

/// Cellular noise: the plane cut into cells of `2^shift` blocks, one site per cell at a hashed
/// offset `[margin, 2^shift - margin)` on each axis.
///
/// The site of cell `(i, j)` is at `(i * 2^shift + ox, j * 2^shift + oz)`, with `ox = margin +
/// ((hash2(sx, i, j) * span) >> 32)` and `oz` the same with `sz`, `span = 2^shift - 2 * margin`,
/// `sx` and `sz` the seed mixed with two salts ([`mix_seed`]). A margin of 0 jitters over the
/// whole cell; `2^shift / 5` keeps the sites in `[0.2, 0.8)` of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Cellular {
    /// Mixed seeds of the x and z offsets (for [`hash2_mixed`]).
    mixed: [u64; 2],
    /// The seed, for [`Cellular::site_hash`].
    seed: u64,
    shift: u32,
    margin: i64,
    /// `2^shift - 2 * margin`: the number of offsets.
    span: u64,
    /// Cells around a site that hold all its Voronoi neighbors (Chebyshev radius).
    window: i64,
}

impl Cellular {
    /// The largest cell: `2^24` blocks. A site's Voronoi neighbors then lie within `2^27` blocks,
    /// where the exact predicates stay inside `i128`.
    pub const MAX_SHIFT: u32 = 24;

    /// Cells of `2^shift` blocks with sites at offsets `[margin, 2^shift - margin)`. Panics if
    /// `shift > MAX_SHIFT` or `margin` is not in `0..2^(shift - 1)` (at least one offset).
    pub fn new(seed: u64, shift: u32, margin: i64) -> Self {
        assert!(
            shift <= Self::MAX_SHIFT,
            "Cellular: cells of 2^{shift} > 2^24"
        );
        let size = 1i64 << shift;
        assert!(
            (0..=(size - 1) / 2).contains(&margin),
            "Cellular: margin {margin} out of 0..={}",
            (size - 1) / 2
        );
        Self {
            mixed: [
                splitmix64(mix_seed(seed, SALT_X)),
                splitmix64(mix_seed(seed, SALT_Z)),
            ],
            seed,
            shift,
            margin,
            span: (size - 2 * margin) as u64,
            window: window_radius(size, margin),
        }
    }

    /// `log2` of the cell size.
    pub const fn shift(&self) -> u32 {
        self.shift
    }

    /// The cell holding `(x, z)`.
    pub const fn cell_of(&self, x: i64, z: i64) -> SiteId {
        SiteId {
            i: x >> self.shift,
            j: z >> self.shift,
        }
    }

    /// The site of cell `id`.
    #[inline]
    pub fn site(&self, id: SiteId) -> Site {
        let offset = |mixed: u64| {
            let h = u64::from(hash2_mixed(mixed, id.i, id.j));
            self.margin + ((h * self.span) >> 32) as i64
        };
        Site {
            id,
            x: (id.i << self.shift) + offset(self.mixed[0]),
            z: (id.j << self.shift) + offset(self.mixed[1]),
        }
    }

    /// 32 hashed bits of site `id` for `salt` (its plate motion, a lake radius...): `hash2` of
    /// the cell with the seed mixed with the salt. Independent of the site's position bits.
    pub fn site_hash(&self, id: SiteId, salt: u64) -> u32 {
        super::hash2(mix_seed(self.seed, salt), id.i, id.j)
    }

    /// The two nearest sites of `(x, z)` (F1 and F2), exact.
    pub fn nearest(&self, x: i64, z: i64) -> Nearest {
        self.search(x, z, |id| self.site(id))
    }

    /// [`Cellular::nearest`] at every point of the grid `xs` x `zs` (`out[i * zs.len() + j]` for
    /// `(xs[i], zs[j])`), equal per point. The sites around the grid are hashed once.
    ///
    /// # Panics
    ///
    /// If `out.len()` is not `xs.len() * zs.len()`.
    pub fn nearest_grid(&self, xs: &[i64], zs: &[i64], out: &mut [Nearest]) {
        assert_eq!(out.len(), xs.len() * zs.len(), "nearest_grid output size");
        if out.is_empty() {
            return;
        }
        // Sites of every cell within 3 of the grid's cells: a search rarely looks further, and
        // when it does it hashes that site itself (the table is only a cache).
        let reach = 3;
        let cells = |cs: &[i64]| {
            cs.iter().fold((i64::MAX, i64::MIN), |(lo, hi), &c| {
                (lo.min(c >> self.shift), hi.max(c >> self.shift))
            })
        };
        let ((i0, i1), (j0, j1)) = (cells(xs), cells(zs));
        let (i0, j0) = (i0 - reach, j0 - reach);
        let width = i1.abs_diff(i0) + 1 + reach as u64;
        let depth = j1.abs_diff(j0) + 1 + reach as u64;
        let table =
            (u128::from(width) * u128::from(depth) <= 4 * out.len() as u128 + 64).then(|| {
                let mut sites = Vec::with_capacity((width * depth) as usize);
                for i in 0..width as i64 {
                    for j in 0..depth as i64 {
                        sites.push(self.site(SiteId {
                            i: i0 + i,
                            j: j0 + j,
                        }));
                    }
                }
                sites
            });
        let lookup = |id: SiteId| {
            if let Some(sites) = &table {
                let (di, dj) = (id.i.wrapping_sub(i0), id.j.wrapping_sub(j0));
                if (0..width as i64).contains(&di) && (0..depth as i64).contains(&dj) {
                    return sites[(di * depth as i64 + dj) as usize];
                }
            }
            self.site(id)
        };
        for (row, &x) in out.chunks_exact_mut(zs.len()).zip(xs) {
            for (n, &z) in row.iter_mut().zip(zs) {
                *n = self.search(x, z, lookup);
            }
        }
    }

    /// The ring search behind [`Cellular::nearest`]: rings of cells around the point's cell,
    /// outward, until the next ring cannot hold a site as near as the current F2.
    ///
    /// A site of a cell `k >= 1` rings away lies past the point's own cell by `k - 1` whole cells
    /// on some axis, plus at least `margin + 1` blocks (the point is inside its cell, the site at
    /// least `margin` inside its own), so its squared distance is at least `((k - 1) * 2^shift +
    /// margin + 1)^2`. The search stops when that bound for the next ring is strictly over F2: a
    /// site there could otherwise tie F2 with a smaller id.
    #[inline]
    fn search(&self, x: i64, z: i64, site: impl Fn(SiteId) -> Site) -> Nearest {
        let SiteId { i: ci, j: cj } = self.cell_of(x, z);
        let size = 1i64 << self.shift;
        let mut best = Best::default();
        let mut r = 0i64;
        loop {
            if r == 0 {
                best.consider((x, z), site(SiteId { i: ci, j: cj }));
            } else {
                for d in -r..r {
                    // The four sides of the ring, each without its last corner.
                    best.consider(
                        (x, z),
                        site(SiteId {
                            i: ci + d,
                            j: cj - r,
                        }),
                    );
                    best.consider(
                        (x, z),
                        site(SiteId {
                            i: ci + r,
                            j: cj + d,
                        }),
                    );
                    best.consider(
                        (x, z),
                        site(SiteId {
                            i: ci - d,
                            j: cj + r,
                        }),
                    );
                    best.consider(
                        (x, z),
                        site(SiteId {
                            i: ci - r,
                            j: cj - d,
                        }),
                    );
                }
            }
            if let [_, Some((d2, ..))] = best.0 {
                let gap = u128::from((r * size + self.margin + 1) as u64);
                if gap * gap > d2 {
                    break;
                }
            }
            r += 1;
        }
        let [Some((d1, _, f1)), Some((d2, _, f2))] = best.0 else {
            unreachable!("the first ring holds 9 sites");
        };
        Nearest { f1, d1, f2, d2 }
    }

    /// The Voronoi cell of site `id`, exact.
    ///
    /// Its neighbors lie within [`Cellular`]'s window of cells (a radius of 3 cells with no
    /// margin, 2 from a margin of about 0.17 of a cell): a point of the cell's boundary, a real
    /// point, is under `sqrt(2) * (2^shift - margin)` from its nearest site (its own cell's site
    /// is that close), and a neighbor shares such a point, so it is under twice that from the
    /// site. The neighbors come counterclockwise by gift wrapping: from the nearest site, each
    /// next one is the site left of the last edge whose circle through the site and that edge
    /// holds no other site (exact `incircle`; between cocircular sites, the one at the smaller
    /// angle, so every one is visited).
    pub fn voronoi_cell(&self, id: SiteId) -> VoronoiCell {
        let a = self.site(id);
        let w = self.window;
        let mut others = Vec::with_capacity(((2 * w + 1) * (2 * w + 1) - 1) as usize);
        for di in -w..=w {
            for dj in -w..=w {
                if (di, dj) != (0, 0) {
                    others.push(self.site(SiteId {
                        i: id.i + di,
                        j: id.j + dj,
                    }));
                }
            }
        }
        let first = *others
            .iter()
            .min_by_key(|s| (dist_sq(a.pos(), s.pos()), s.id))
            .expect("a window holds other sites");
        let mut fan = vec![first];
        loop {
            let last = fan[fan.len() - 1];
            let mut next: Option<Site> = None;
            for &d in &others {
                if d.id == last.id || orient(a.pos(), last.pos(), d.pos()) <= 0 {
                    continue;
                }
                next = Some(match next {
                    None => d,
                    Some(c) => {
                        let t = incircle(a.pos(), last.pos(), c.pos(), d.pos());
                        if t > 0 || (t == 0 && orient(a.pos(), c.pos(), d.pos()) < 0) {
                            d
                        } else {
                            c
                        }
                    }
                });
            }
            let next = next.expect("the window closes the Voronoi cell");
            if next.id == first.id {
                break;
            }
            assert!(
                fan.len() < others.len(),
                "Voronoi gift wrapping did not close"
            );
            fan.push(next);
        }
        let n = fan.len();
        let vertex = |k: usize| {
            circumcenter(a.pos(), fan[k % n].pos(), fan[(k + 1) % n].pos())
                .expect("consecutive Voronoi neighbors are not collinear with the site")
        };
        let edges = (0..n)
            .map(|k| VoronoiEdge {
                neighbor: fan[k],
                start: vertex(k + n - 1),
                end: vertex(k),
            })
            .collect();
        VoronoiCell { site: a, edges }
    }
}

/// The two nearest sites seen so far, as `(squared distance, id, site)`, ordered by distance
/// then id.
#[derive(Default)]
struct Best([Option<(u128, SiteId, Site)>; 2]);

impl Best {
    /// Keeps `s` if it is one of the two nearest to `p` so far.
    #[inline]
    fn consider(&mut self, p: (i64, i64), s: Site) {
        let key = (dist_sq(p, s.pos()), s.id, s);
        let less = |a: &(u128, SiteId, Site)| (key.0, key.1) < (a.0, a.1);
        self.0 = match self.0 {
            [Some(a), _] if less(&a) => [Some(key), Some(a)],
            [Some(a), Some(b)] if !less(&b) => [Some(a), Some(b)],
            [Some(a), _] => [Some(a), Some(key)],
            [None, _] => [Some(key), None],
        };
    }
}

/// The Chebyshev radius, in cells, of the window around a site that holds all its Voronoi
/// neighbors: the smallest `r` such that a site `r + 1` cells away on some axis is at least
/// `2 sqrt(2) (size - margin)` away. A Voronoi vertex is a real point, under `size - margin`
/// from its own cell's site along each axis, so it is under `sqrt(2) (size - margin)` from its
/// nearest site, and a neighbor is under twice that from the site. Sites `r + 1` cells apart on
/// an axis are at least `r * size + 2 * margin + 1` apart on it.
fn window_radius(size: i64, margin: i64) -> i64 {
    let reach_sq = 8 * i128::from(size - margin).pow(2);
    let r = (1..)
        .find(|&r: &i64| {
            let gap = i128::from(r) * i128::from(size) + i128::from(2 * margin + 1);
            gap * gap >= reach_sq
        })
        .expect("a radius of 3 always suffices");
    debug_assert!(
        (2 * r + 1) * size <= MAX_SPREAD,
        "the window fits the exact predicates"
    );
    r
}

#[cfg(test)]
mod tests;
