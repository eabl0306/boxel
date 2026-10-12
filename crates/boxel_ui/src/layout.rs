//! Rectangles in GUI pixels and the small layout helpers screens use: a column of rows, a
//! two-column grid and splitting a row. Fixed sizes, no solver: the sizes follow Minecraft's
//! screens (rows 20 high, 200-wide main buttons, 150-wide option buttons).

use boxel_math::IVec2;

/// A rectangle in GUI pixels: `x`, `y` is the top-left corner, `w` and `h` its size (never
/// negative in rectangles the helpers make).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct UiRect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub w: i32,
    /// Height.
    pub h: i32,
}

impl UiRect {
    /// A rectangle from its top-left corner and size.
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    /// The right edge (exclusive).
    pub const fn right(&self) -> i32 {
        self.x + self.w
    }

    /// The bottom edge (exclusive).
    pub const fn bottom(&self) -> i32 {
        self.y + self.h
    }

    /// Whether the GUI pixel `p` lies inside.
    pub const fn contains(&self, p: IVec2) -> bool {
        p.x >= self.x && p.y >= self.y && p.x < self.right() && p.y < self.bottom()
    }

    /// True when it covers no pixel.
    pub const fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    /// The rectangle `d` pixels smaller on every side.
    pub const fn shrink(&self, d: i32) -> Self {
        Self::new(self.x + d, self.y + d, self.w - 2 * d, self.h - 2 * d)
    }

    /// The overlap of two rectangles (empty when they do not overlap).
    pub fn intersect(&self, other: &Self) -> Self {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        Self::new(x, y, (right - x).max(0), (bottom - y).max(0))
    }

    /// `K` side-by-side parts with `gap` pixels between them. The widths differ by at most one
    /// pixel and the first parts take the remainder, so the parts always fill the rectangle.
    pub fn split<const K: usize>(&self, gap: i32) -> [Self; K] {
        let k = K as i32;
        let free = (self.w - gap * (k - 1)).max(0);
        let (base, extra) = (free / k.max(1), free % k.max(1));
        let mut x = self.x;
        std::array::from_fn(|i| {
            let w = base + i32::from((i as i32) < extra);
            let part = Self::new(x, self.y, w, self.h);
            x += w + gap;
            part
        })
    }
}

/// Rows stacked down a column: `Column::new(x, y, width, step)` gives rows `width` wide and 20
/// high (change it with [`Column::row_height`]) whose tops are `step` pixels apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Column {
    x: i32,
    y: i32,
    w: i32,
    row_h: i32,
    step: i32,
}

impl Column {
    /// The default row height, a Minecraft button's.
    pub const ROW_HEIGHT: i32 = 20;

    /// A column whose first row starts at `x`, `y`.
    pub const fn new(x: i32, y: i32, w: i32, step: i32) -> Self {
        Self {
            x,
            y,
            w,
            row_h: Self::ROW_HEIGHT,
            step,
        }
    }

    /// Changes the height of the rows (not the step).
    pub const fn row_height(mut self, h: i32) -> Self {
        self.row_h = h;
        self
    }

    /// The next row.
    #[expect(
        clippy::should_implement_trait,
        reason = "an endless column, used as `col.next()` in layouts"
    )]
    pub fn next(&mut self) -> UiRect {
        let row = UiRect::new(self.x, self.y, self.w, self.row_h);
        self.y += self.step;
        row
    }

    /// The next row, `extra` pixels lower than [`Column::next`] would give it.
    pub fn next_after(&mut self, extra: i32) -> UiRect {
        self.y += extra;
        self.next()
    }

    /// Where the next row starts.
    pub const fn y(&self) -> i32 {
        self.y
    }
}

/// Two columns of cells centered on a vertical line, as Minecraft's option pages: cells
/// `col_w` wide and `row_h` high, `col_gap` apart, rows `row_h + row_gap` apart. Defaults 150,
/// 20, 10 and 5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grid2 {
    center_x: i32,
    y: i32,
    col_w: i32,
    col_gap: i32,
    row_h: i32,
    row_gap: i32,
    index: usize,
}

impl Grid2 {
    /// A grid centered on `center_x` whose first row starts at `y`.
    pub const fn new(center_x: i32, y: i32) -> Self {
        Self {
            center_x,
            y,
            col_w: 150,
            col_gap: 10,
            row_h: 20,
            row_gap: 5,
            index: 0,
        }
    }

    /// Changes the cell size.
    pub const fn cell_size(mut self, w: i32, h: i32) -> Self {
        self.col_w = w;
        self.row_h = h;
        self
    }

    /// Changes the gaps between columns and between rows.
    pub const fn gaps(mut self, col_gap: i32, row_gap: i32) -> Self {
        self.col_gap = col_gap;
        self.row_gap = row_gap;
        self
    }

    /// Cell `i`: row `i / 2`, left column for even `i`.
    pub const fn cell(&self, i: usize) -> UiRect {
        let row = (i / 2) as i32;
        let left = self.center_x - self.col_gap / 2 - self.col_w;
        let x = if i.is_multiple_of(2) {
            left
        } else {
            left + self.col_w + self.col_gap
        };
        UiRect::new(
            x,
            self.y + row * (self.row_h + self.row_gap),
            self.col_w,
            self.row_h,
        )
    }

    /// The next cell, left to right and top to bottom.
    #[expect(
        clippy::should_implement_trait,
        reason = "an endless grid, used as `grid.next()` in layouts"
    )]
    pub fn next(&mut self) -> UiRect {
        let cell = self.cell(self.index);
        self.index += 1;
        cell
    }

    /// A cell spanning both columns on the next free row (a half-used row is skipped).
    pub fn next_wide(&mut self) -> UiRect {
        self.index += self.index % 2;
        let left = self.cell(self.index);
        self.index += 2;
        UiRect::new(left.x, left.y, 2 * self.col_w + self.col_gap, self.row_h)
    }

    /// Where the row after the last used one starts.
    pub const fn bottom(&self) -> i32 {
        let rows = self.index.div_ceil(2) as i32;
        self.y + rows * (self.row_h + self.row_gap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_fills_the_row_with_gaps() {
        let [l, r] = UiRect::new(10, 5, 200, 20).split(4);
        assert_eq!(l, UiRect::new(10, 5, 98, 20));
        assert_eq!(r, UiRect::new(112, 5, 98, 20));
        let parts: [UiRect; 3] = UiRect::new(0, 0, 11, 1).split(1);
        assert_eq!(parts.map(|p| p.w), [3, 3, 3]);
        assert_eq!(parts[2].right(), 11);
        let parts: [UiRect; 3] = UiRect::new(0, 0, 13, 1).split(1);
        assert_eq!(parts.map(|p| p.w), [4, 4, 3]);
    }

    #[test]
    fn column_and_grid_follow_minecraft_sizes() {
        let mut col = Column::new(60, 100, 200, 24);
        assert_eq!(col.next(), UiRect::new(60, 100, 200, 20));
        assert_eq!(col.next(), UiRect::new(60, 124, 200, 20));
        assert_eq!(col.next_after(12), UiRect::new(60, 160, 200, 20));

        let mut grid = Grid2::new(160, 40);
        assert_eq!(grid.next(), UiRect::new(5, 40, 150, 20));
        assert_eq!(grid.next(), UiRect::new(165, 40, 150, 20));
        assert_eq!(grid.next(), UiRect::new(5, 65, 150, 20));
        assert_eq!(grid.next_wide(), UiRect::new(5, 90, 310, 20));
        assert_eq!(grid.bottom(), 115);
    }

    #[test]
    fn rect_helpers() {
        let r = UiRect::new(0, 0, 10, 4);
        assert!(r.contains(IVec2::new(9, 3)));
        assert!(!r.contains(IVec2::new(10, 3)));
        assert_eq!(
            r.intersect(&UiRect::new(5, 2, 10, 10)),
            UiRect::new(5, 2, 5, 2)
        );
        assert!(r.intersect(&UiRect::new(20, 0, 1, 1)).is_empty());
        assert_eq!(r.shrink(1), UiRect::new(1, 1, 8, 2));
    }
}
