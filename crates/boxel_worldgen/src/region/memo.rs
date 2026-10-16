//! The per-worker memo of lattice points (ADR 0039 sec. 13): each point's kind and bounds, keyed
//! by the [`BuildKey`] of the generator that computed them.

use super::generator::LocalBounds;
use crate::registry::{BuildKey, BuildMemo};
use std::cell::RefCell;

/// Lattice points per side of a memo table: a direct-mapped `32 x 32` table (8 km at the default
/// lattice), so a 4 x 4 window never evicts itself.
const SIDE: usize = 32;

/// Memo tables per worker, one per generator by `key % SLOTS`: the realms of a stack (built one
/// after the other, so with consecutive keys) never evict each other.
const SLOTS: usize = 4;

/// What the memo keeps of a lattice point: its kind and the bounds of that kind there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Point {
    /// The kind's index among the realm's kinds.
    pub(crate) kind: u32,
    /// The kind's bounds there ([`TerrainKind::bounds`](super::TerrainKind::bounds)), in
    /// [`HeightQ`](super::HeightQ).
    pub(crate) lo: i32,
    pub(crate) hi: i32,
}

/// The 16 points of a window ([`Window`](super::blend::Window)'s order).
pub(crate) type WindowPoints = [Point; 16];

/// One slot of a table.
#[derive(Clone, Copy, Debug)]
struct Slot {
    i: i64,
    j: i64,
    point: Option<Point>,
}

/// The last window a table gave whole: its lattice cell, its points and its column's bounds.
#[derive(Clone, Copy, Debug)]
struct Last {
    cell: (i64, i64),
    points: WindowPoints,
    bounds: LocalBounds,
}

/// A direct-mapped table of lattice points (`SIDE * SIDE` slots of 32 bytes, 32 KiB), and the
/// last window: the chunks of one lattice cell, often generated one after the other, read it
/// once, and a chunk over or under its bounds reads only them.
struct Table {
    slots: Box<[Slot]>,
    last: Option<Last>,
}

impl Table {
    fn new() -> Self {
        let empty = Slot {
            i: 0,
            j: 0,
            point: None,
        };
        Self {
            slots: vec![empty; SIDE * SIDE].into_boxed_slice(),
            last: None,
        }
    }

    const fn slot(i: i64, j: i64) -> usize {
        (i as usize % SIDE) * SIDE + j as usize % SIDE
    }

    fn get(&self, i: i64, j: i64) -> Option<Point> {
        let s = &self.slots[Self::slot(i, j)];
        s.point.filter(|_| s.i == i && s.j == j)
    }

    fn set(&mut self, i: i64, j: i64, point: Point) {
        self.slots[Self::slot(i, j)] = Slot {
            i,
            j,
            point: Some(point),
        };
    }

    fn clear(&mut self) {
        for s in self.slots.iter_mut() {
            s.point = None;
        }
        self.last = None;
    }
}

type Tables = [BuildMemo<Option<Table>>; SLOTS];

thread_local! {
    static MEMO: RefCell<Tables> = const { RefCell::new([const { BuildMemo::new(None) }; SLOTS]) };
}

/// The memo of the generator of `key` on this thread: `f` gets its table, emptied first if
/// another build filled it.
fn with_table<R>(key: BuildKey, f: impl FnOnce(&mut Table) -> R) -> R {
    MEMO.with(|memo| {
        let mut tables = memo.borrow_mut();
        let slot = (key.get() % SLOTS as u64) as usize;
        let table = tables[slot].get(key, |t| {
            if let Some(t) = t {
                t.clear();
            }
        });
        f(table.get_or_insert_with(Table::new))
    })
}

/// Reads the points `(i, j)` of `at` into `out` where the memo of `key` holds them, and returns
/// the mask of the ones it lacks (bit `n` for `at[n]`).
pub(crate) fn read(key: BuildKey, at: &[(i64, i64)], out: &mut [Option<Point>]) -> u32 {
    with_table(key, |table| {
        let mut missing = 0;
        for (n, (&(i, j), o)) in at.iter().zip(out.iter_mut()).enumerate() {
            *o = table.get(i, j);
            if o.is_none() {
                missing |= 1 << n;
            }
        }
        missing
    })
}

/// Stores the points of `at` set in `mask` (`points[n]` for `at[n]`) in the memo of `key`.
pub(crate) fn write(key: BuildKey, at: &[(i64, i64)], points: &[Option<Point>], mask: u32) {
    with_table(key, |table| {
        for (n, (&(i, j), p)) in at.iter().zip(points).enumerate() {
            if mask & (1 << n) != 0
                && let Some(p) = p
            {
                table.set(i, j, *p);
            }
        }
    });
}

/// The bounds of the window of lattice cell `cell`, if it is the last one of the memo of `key`.
pub(crate) fn last_bounds(key: BuildKey, cell: (i64, i64)) -> Option<LocalBounds> {
    with_table(key, |table| {
        table
            .last
            .as_ref()
            .filter(|last| last.cell == cell)
            .map(|last| last.bounds)
    })
}

/// The window of lattice cell `cell` (its 16 points `at`) and its bounds, if it is the last one
/// of the memo of `key`; else [`read`] of its points into `out`, with the mask of the missing
/// ones.
pub(crate) fn read_window(
    key: BuildKey,
    cell: (i64, i64),
    at: &[(i64, i64); 16],
    out: &mut [Option<Point>; 16],
) -> Result<(WindowPoints, LocalBounds), u32> {
    with_table(key, |table| {
        if let Some(last) = table.last.filter(|last| last.cell == cell) {
            return Ok((last.points, last.bounds));
        }
        let mut missing = 0;
        for (n, (&(i, j), o)) in at.iter().zip(out.iter_mut()).enumerate() {
            *o = table.get(i, j);
            if o.is_none() {
                missing |= 1 << n;
            }
        }
        Err(missing)
    })
}

/// Stores the points of `at` set in `mask` in the memo of `key`, and the window of `cell` with
/// its bounds as the last one.
pub(crate) fn write_window(
    key: BuildKey,
    cell: (i64, i64),
    at: &[(i64, i64); 16],
    (points, bounds): (&WindowPoints, LocalBounds),
    mask: u32,
) {
    with_table(key, |table| {
        for (n, (&(i, j), p)) in at.iter().zip(points).enumerate() {
            if mask & (1 << n) != 0 {
                table.set(i, j, *p);
            }
        }
        table.last = Some(Last {
            cell,
            points: *points,
            bounds,
        });
    });
}

/// Empties every memo of the region generators on the calling thread (benches of the cold path,
/// tests of eviction). The memos are only a cache: the output never depends on them.
pub fn clear_worker_memo() {
    MEMO.with(|memo| {
        for table in memo.borrow_mut().iter_mut() {
            *table = BuildMemo::new(None);
        }
    });
}
