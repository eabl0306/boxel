//! `--sky-sources` (ADR 0061): a cavern lit by sky sources, built on the ground around the start.
//!
//! A closed room of cobblestone ([`HALF`] blocks from the center to the inner side of each wall,
//! [`HEIGHT`] blocks of air) on a stone floor at the ground of the start column, with a stone
//! foundation down to the ground of every column it covers. Under the roof, in every column of
//! the room, a sky source (`boxel:sky_source`, down, range 64): the room is lit like the open
//! sky, follows the day and night, and its walls give shade. A stone pillar from the floor to
//! the roof and a ledge along the west wall at mid height show the shadows of what stops the
//! beams. A door in the south wall lets the player in.

use boxel::prelude::*;
use boxel::voxel::{BlockPos, BlockState};

/// Blocks from the center column to the inner side of each wall.
pub const HALF: i64 = 10;
/// Rows of air between the floor and the roof; the sources are in the top one.
pub const HEIGHT: i64 = 12;
/// How far below the start the ground is searched.
const GROUND_DEPTH: i64 = 256;
/// Deepest foundation under the floor.
const FOUNDATION: i64 = 16;
/// The ledge: its row above the floor and its width from the west wall.
const LEDGE_ROW: i64 = 6;
const LEDGE_WIDTH: i64 = 5;
/// The pillar's corner columns (x, z from the center), inclusive.
const PILLAR: [(i64, i64); 2] = [(4, -4), (5, -3)];

/// The blocks the cavern is made of.
pub const FLOOR: &str = "boxel:stone";
pub const WALL: &str = "boxel:cobblestone";
pub const STONE: &str = "boxel:stone";
pub const AIR: &str = "boxel:air";
pub const SOURCE: &str = boxel_demo::blocks::SKY_SOURCE;

/// The cavern around one start: its floor and every cell it sets.
#[derive(Clone, Debug, PartialEq)]
pub struct Cavern {
    /// The floor block of the center column (on the ground of the start column).
    pub floor: BlockPos,
    /// Every cell, with the name of its block (air included: the room is cleared).
    pub cells: Vec<(BlockPos, &'static str)>,
}

/// The highest block `is_ground` in column `(x, z)` from `top` down `depth` blocks. `None`
/// while a block on the way is not loaded, `Some(None)` without ground in reach.
fn ground(
    (x, z): (i64, i64),
    top: i64,
    depth: i64,
    block_at: &impl Fn(BlockPos) -> Option<BlockState>,
    is_ground: &impl Fn(BlockState) -> bool,
) -> Option<Option<i64>> {
    for y in (top.saturating_sub(depth)..=top).rev() {
        if is_ground(block_at(BlockPos::new(x, y, z))?) {
            return Some(Some(y));
        }
    }
    Some(None)
}

impl Cavern {
    /// The cavern around `start`: its floor replaces the ground of the start column (the first
    /// block `is_ground` under the start, within 256), so it stands on the ground whatever grows
    /// on it. `None` while a column is not loaded (try again next frame), `Some(None)` when the
    /// start column has no ground in reach.
    pub fn around(
        start: DVec3,
        block_at: impl Fn(BlockPos) -> Option<BlockState>,
        is_ground: impl Fn(BlockState) -> bool,
    ) -> Option<Option<Self>> {
        let (cx, cz) = (start.x.floor() as i64, start.z.floor() as i64);
        let top = start.y.floor() as i64;
        let Some(floor) = ground((cx, cz), top, GROUND_DEPTH, &block_at, &is_ground)? else {
            return Some(None);
        };
        let outer = HALF + 1;
        let mut cells = Vec::new();
        for x in -outer..=outer {
            for z in -outer..=outer {
                let (wx, wz) = (cx + x, cz + z);
                let at = |y: i64| BlockPos::new(wx, y, wz);
                // The foundation, from the column's ground (or as deep as it goes) to the floor.
                let base = ground((wx, wz), floor - 1, FOUNDATION, &block_at, &is_ground)?
                    .unwrap_or(floor - FOUNDATION - 1);
                cells.extend((base + 1..floor).map(|y| (at(y), STONE)));
                cells.push((at(floor), FLOOR));
                let wall = x.abs() == outer || z.abs() == outer;
                let door = z == outer && x.abs() <= 1;
                let pillar = (PILLAR[0].0..=PILLAR[1].0).contains(&x)
                    && (PILLAR[0].1..=PILLAR[1].1).contains(&z);
                for row in 1..=HEIGHT {
                    let name = if wall {
                        if door && row <= 3 { AIR } else { WALL }
                    } else if pillar || (row == LEDGE_ROW && x < -HALF + LEDGE_WIDTH) {
                        STONE
                    } else if row == HEIGHT {
                        SOURCE
                    } else {
                        AIR
                    };
                    cells.push((at(floor + row), name));
                }
                cells.push((at(floor + HEIGHT + 1), WALL));
            }
        }
        Some(Some(Self {
            floor: BlockPos::new(cx, floor, cz),
            cells,
        }))
    }

    /// A camera inside, by the door, looking north at the pillar and the ledge: its position and
    /// its yaw and pitch (radians, as `Transform::from_yaw_pitch`).
    pub fn camera(&self) -> (DVec3, f32, f32) {
        let f = self.floor;
        let eye = DVec3::new(
            f.x as f64 + 0.5,
            f.y as f64 + 2.6,
            (f.z + HALF - 1) as f64 + 0.5,
        );
        (eye, 0.0, (-8.0f32).to_radians())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Ground at y = 63 (64 on the hill at x >= 8), loaded down to y = 0; `BlockState(1)` is
    /// ground, `BlockState(2)` a tree that is not.
    fn world(p: BlockPos) -> Option<BlockState> {
        let top = if p.x >= 8 { 64 } else { 63 };
        (p.y >= 0).then_some(if p.y <= top {
            BlockState(1)
        } else if (p.x, p.z) == (0, 0) && p.y <= 70 {
            BlockState(2)
        } else {
            BlockState::AIR
        })
    }

    #[test]
    fn the_cavern_is_built_on_the_ground_once_it_is_loaded() {
        let start = DVec3::new(0.5, 100.0, 0.5);
        let is_ground = |s: BlockState| s == BlockState(1);
        let cavern = Cavern::around(start, world, is_ground)
            .expect("loaded")
            .expect("ground");
        // On the ground of the start column, under the tree.
        assert_eq!(cavern.floor, BlockPos::new(0, 63, 0));
        let cells: HashMap<BlockPos, &str> = cavern.cells.iter().copied().collect();
        assert_eq!(cells.len(), cavern.cells.len(), "each cell once");
        let at = |x, y, z| cells.get(&BlockPos::new(x, y, z)).copied();
        let outer = HALF + 1;
        for x in -outer..=outer {
            for z in -outer..=outer {
                assert_eq!(at(x, 63, z), Some(FLOOR), "floor {x} {z}");
                assert_eq!(at(x, 63 + HEIGHT + 1, z), Some(WALL), "roof {x} {z}");
                // The hill under the floor stays; nothing goes under the ground.
                assert_eq!(at(x, 62, z), None);
            }
        }
        // Walls on the four sides, with the door in the south one.
        assert_eq!(at(-outer, 64, 0), Some(WALL));
        assert_eq!(at(0, 70, -outer), Some(WALL));
        assert_eq!(at(0, 64, outer), Some(AIR), "the door");
        assert_eq!(at(0, 67, outer), Some(WALL), "over the door");
        // A source in the top row of every column of the room but the pillar's.
        let sources = cavern.cells.iter().filter(|c| c.1 == SOURCE).count();
        assert_eq!(sources as i64, (2 * HALF + 1).pow(2) - 4);
        for (pos, name) in &cavern.cells {
            if *name == SOURCE {
                assert_eq!(pos.y, 63 + HEIGHT, "{pos:?}");
            }
        }
        assert_eq!(at(0, 63 + HEIGHT, 0), Some(SOURCE));
        assert_eq!(at(4, 63 + HEIGHT, -4), Some(STONE), "the pillar");
        assert_eq!(at(-HALF, 63 + LEDGE_ROW, 5), Some(STONE), "the ledge");
        // The room is cleared (the tree goes) and the camera starts in it.
        assert_eq!(at(0, 65, 0), Some(AIR));
        let (eye, _, _) = cavern.camera();
        let cell = BlockPos::from_world(eye);
        assert_eq!(at(cell.x, cell.y, cell.z), Some(AIR));
        assert_eq!(at(cell.x, cell.y + 1, cell.z), Some(AIR));

        // A column falling away: the foundation goes down to its ground, at most 16.
        let pit = |p: BlockPos| {
            if (p.x, p.z) == (5, 5) {
                Some(if p.y <= 50 {
                    BlockState(1)
                } else {
                    BlockState::AIR
                })
            } else {
                world(p)
            }
        };
        let cavern = Cavern::around(start, pit, is_ground).unwrap().unwrap();
        let foundation: Vec<i64> = cavern
            .cells
            .iter()
            .filter(|(p, n)| (p.x, p.z) == (5, 5) && *n == STONE && p.y < 63)
            .map(|(p, _)| p.y)
            .collect();
        assert_eq!(foundation, (51..63).collect::<Vec<_>>());
        // Not loaded yet: wait; no ground in reach: nothing.
        assert_eq!(Cavern::around(start, |_| None, is_ground), None);
        assert_eq!(
            Cavern::around(start, |_| Some(BlockState::AIR), is_ground),
            Some(None)
        );
    }
}
