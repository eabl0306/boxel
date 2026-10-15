//! The bounds of a whole grid of cells at once ([`CellGrid`], [`Cells`], [`Field::cells`]): what
//! a far LOD node asks for, its 34 x 34 cells of `2^level` blocks, without asking every leaf of
//! a tree once per cell and per bound.

use super::{Field, Fine, Rect, Span, by_bound};

/// `nx` by `nz` square cells of `size` blocks, neighbors sharing their corners: cell `(i, j)` is
/// [`Rect::cell`] at `(x0 + i * size, z0 + j * size)` with that size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CellGrid {
    /// x of the first cell's low corner.
    pub x0: i64,
    /// z of the first cell's low corner.
    pub z0: i64,
    /// Cell edge in blocks.
    pub size: i64,
    /// Cells along x.
    pub nx: usize,
    /// Cells along z.
    pub nz: usize,
}

impl CellGrid {
    /// `nx` by `nz` cells of `size` blocks from `(x0, z0)`. Panics if `size` is negative.
    pub const fn new(x0: i64, z0: i64, size: i64, nx: usize, nz: usize) -> Self {
        assert!(size >= 0, "CellGrid: a negative cell size");
        Self {
            x0,
            z0,
            size,
            nx,
            nz,
        }
    }

    /// The number of cells.
    pub const fn len(&self) -> usize {
        self.nx * self.nz
    }

    /// Whether there is no cell.
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Cell `(i, j)`.
    pub const fn rect(&self, i: usize, j: usize) -> Rect {
        Rect::cell(
            self.x0 + i as i64 * self.size,
            self.z0 + j as i64 * self.size,
            self.size,
        )
    }

    /// The `nx + 1` corner coordinates along x.
    pub fn corner_xs(&self) -> Vec<i64> {
        (0..=self.nx as i64)
            .map(|i| self.x0 + i * self.size)
            .collect()
    }

    /// The `nz + 1` corner coordinates along z.
    pub fn corner_zs(&self) -> Vec<i64> {
        (0..=self.nz as i64)
            .map(|j| self.z0 + j * self.size)
            .collect()
    }

    /// The cell centers along x ([`Rect::center_x`] of each cell).
    pub fn center_xs(&self) -> Vec<i64> {
        let half = self.size / 2;
        (0..self.nx as i64)
            .map(|i| self.x0 + i * self.size + half)
            .collect()
    }

    /// The cell centers along z.
    pub fn center_zs(&self) -> Vec<i64> {
        let half = self.size / 2;
        (0..self.nz as i64)
            .map(|j| self.z0 + j * self.size + half)
            .collect()
    }

    /// Half the diagonal of a cell ([`Rect::half_diagonal`]).
    pub fn half_diagonal(&self) -> u64 {
        Rect::cell(0, 0, self.size).half_diagonal()
    }

    /// The squared distance from a cell's center to its farthest corner.
    pub(super) fn center_reach_sq(&self) -> u128 {
        let far = u128::from(self.size.unsigned_abs() - self.size.unsigned_abs() / 2);
        2 * far * far
    }
}

/// What a field knows over a [`CellGrid`]: its values at the corners (its grid form there, bit
/// for bit), and per cell a bound and a fine, each as sound as [`Field::bound`] and
/// [`Field::fine`] on that cell (not always equal to them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cells {
    /// The values at the `(nx + 1) x (nz + 1)` corners, `corners[i * (nz + 1) + j]` at corner
    /// `(i, j)`.
    pub corners: Vec<i64>,
    /// The bound of each cell, `bounds[i * nz + j]` for cell `(i, j)`.
    pub bounds: Vec<Span>,
    /// The fine of each cell, in the same order.
    pub fines: Vec<Fine>,
}

impl Cells {
    /// The 4 corner values of cell `(i, j)`, in [`Rect::corners`] order.
    pub fn corner_values(&self, grid: &CellGrid, i: usize, j: usize) -> [i64; 4] {
        let d = grid.nz + 1;
        let k = i * d + j;
        [
            self.corners[k],
            self.corners[k + d],
            self.corners[k + 1],
            self.corners[k + d + 1],
        ]
    }

    /// Every cell's `(i, j, index)`, in storage order.
    pub(super) fn indices(grid: &CellGrid) -> impl Iterator<Item = (usize, usize, usize)> {
        let nz = grid.nz;
        (0..grid.nx).flat_map(move |i| (0..nz).map(move |j| (i, j, i * nz + j)))
    }
}

/// The field's values at the grid's corners.
pub(super) fn corner_grid<F: Field + ?Sized>(f: &F, grid: &CellGrid) -> Vec<i64> {
    let mut corners = vec![0; (grid.nx + 1) * (grid.nz + 1)];
    f.grid(&grid.corner_xs(), &grid.corner_zs(), &mut corners);
    corners
}

/// The default [`Field::cells`]: the corner grid once, then [`Field::bound`] and [`Field::fine`]
/// per cell. Always right, for any field; a field whose bound and fine are the slope defaults
/// gets the same answer from [`slope_cells`] with two grids instead.
pub fn default_cells<F: Field + ?Sized>(f: &F, grid: &CellGrid) -> Cells {
    let corners = corner_grid(f, grid);
    let (mut bounds, mut fines) = (
        Vec::with_capacity(grid.len()),
        Vec::with_capacity(grid.len()),
    );
    for (i, j, _) in Cells::indices(grid) {
        let rect = grid.rect(i, j);
        bounds.push(f.bound(rect));
        fines.push(f.fine(rect));
    }
    Cells {
        corners,
        bounds,
        fines,
    }
}

/// [`Field::cells`] from the slope alone: the corner grid and the center grid, then per cell
/// [`bound_from_slope`](super::bound_from_slope) and [`fine_from_slope`](super::fine_from_slope)
/// from those values. Equal, cell by cell, to the per-cell defaults.
pub fn slope_cells<F: Field + ?Sized>(f: &F, grid: &CellGrid) -> Cells {
    let corners = corner_grid(f, grid);
    let mut centers = vec![0; grid.len()];
    f.grid(&grid.center_xs(), &grid.center_zs(), &mut centers);
    let (slope, range) = (f.slope(), f.range());
    let reach = slope.over_sq(grid.center_reach_sq());
    let spread = Fine::both(slope.over(grid.half_diagonal()));
    let mut cells = Cells {
        corners,
        bounds: Vec::with_capacity(grid.len()),
        fines: Vec::with_capacity(grid.len()),
    };
    for (i, j, k) in Cells::indices(grid) {
        let bound = Span::around(centers[k], reach, range);
        let fine = by_bound(bound, cells.corner_values(grid, i, j)).min(spread);
        cells.bounds.push(bound);
        cells.fines.push(fine);
    }
    cells
}
