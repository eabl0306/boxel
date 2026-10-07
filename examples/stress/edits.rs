//! `--edits`: scripted block edits during the measurement, and how long the world takes to
//! publish them. Every [`EDIT_PERIOD`] the script applies the next step of a cycle around the
//! start position: place a lamp on the ground, remove it, dig a shaft down from the ground, cap
//! it with stone. Each step that starts on a settled world is timed until the world settles
//! again (every remesh of the edit batch published); the summary gives those latencies and the
//! edit batches the world held or had to split (`WorldStats`).

use super::report::EditStats;
use boxel::prelude::*;
use boxel::voxel::{BlockPos, BlockState};
use boxel::world::{WorldContent, WorldStats};
use std::time::{Duration, Instant};

/// Time between two scripted edits.
pub const EDIT_PERIOD: Duration = Duration::from_millis(1000);
/// Blocks dug by a shaft, from the ground down.
pub const SHAFT_DEPTH: i64 = 24;
/// Offsets (x, z) in blocks from the start column of the lamp and of the shaft.
pub const LAMP_OFFSET: (i64, i64) = (6, 6);
pub const SHAFT_OFFSET: (i64, i64) = (-6, 6);
/// How far below the start the ground is searched.
const GROUND_DEPTH: i64 = 512;
/// The lamp the script places.
const LAMP: &str = "boxel:lamp_orange";
/// What caps the shaft.
const CAP: &str = "boxel:stone";

/// One scripted edit, in cycle order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditStep {
    /// A lamp on the ground.
    PlaceLamp,
    /// The lamp removed (the world is as before).
    RemoveLamp,
    /// A 1x1 shaft [`SHAFT_DEPTH`] blocks deep, from the ground down: sky light reaches its
    /// bottom.
    DigShaft,
    /// Stone on top of the shaft: dark again.
    CapShaft,
}

impl EditStep {
    /// The cycle.
    pub const CYCLE: [EditStep; 4] = [
        EditStep::PlaceLamp,
        EditStep::RemoveLamp,
        EditStep::DigShaft,
        EditStep::CapShaft,
    ];

    /// The block changes of this step, given the ground blocks of the lamp and shaft columns.
    pub fn edits(
        self,
        lamp_ground: BlockPos,
        shaft_ground: BlockPos,
        lamp: BlockState,
        cap: BlockState,
    ) -> Vec<(BlockPos, BlockState)> {
        let above = |p: BlockPos| BlockPos::new(p.x, p.y + 1, p.z);
        match self {
            EditStep::PlaceLamp => vec![(above(lamp_ground), lamp)],
            EditStep::RemoveLamp => vec![(above(lamp_ground), BlockState::AIR)],
            EditStep::DigShaft => (0..SHAFT_DEPTH)
                .map(|d| {
                    let p = BlockPos::new(shaft_ground.x, shaft_ground.y - d, shaft_ground.z);
                    (p, BlockState::AIR)
                })
                .collect(),
            EditStep::CapShaft => vec![(shaft_ground, cap)],
        }
    }
}

/// The first non-air block at or under `top` in column `(x, z)`, within [`GROUND_DEPTH`]. `None`
/// while a block on the way is not loaded, `Some(None)` when the column has no ground in reach.
pub fn ground(
    x: i64,
    z: i64,
    top: i64,
    block_at: impl Fn(BlockPos) -> Option<BlockState>,
) -> Option<Option<BlockPos>> {
    for y in (top.saturating_sub(GROUND_DEPTH)..=top).rev() {
        let p = BlockPos::new(x, y, z);
        if !block_at(p)?.is_air() {
            return Some(Some(p));
        }
    }
    Some(None)
}

/// The script's state (a resource only with `--edits`).
#[derive(Resource, Debug)]
pub struct EditScript {
    /// Start position: the columns are offsets from it.
    start: DVec3,
    /// Ground blocks of the lamp and the shaft columns, found before the first step.
    columns: Option<(BlockPos, BlockPos)>,
    /// Next step of [`EditStep::CYCLE`].
    next: usize,
    /// When the last step was applied.
    last: Option<Instant>,
    /// The step being timed: when it was applied and the frames seen since.
    timing: Option<(Instant, u32)>,
    /// Steps applied, and those skipped (columns without ground).
    pub steps: usize,
    pub skipped: usize,
    /// Timed steps: frames and time until the world settled again.
    latencies: Vec<(u32, Duration)>,
}

impl EditScript {
    /// A script around `start`.
    pub fn new(start: DVec3) -> Self {
        Self {
            start,
            columns: None,
            next: 0,
            last: None,
            timing: None,
            steps: 0,
            skipped: 0,
            latencies: Vec::new(),
        }
    }

    /// One frame of the measurement at `now`. `settled`: the world had nothing left to do at
    /// the end of the previous frame. Applies the next step into `edits` when it is due.
    pub fn frame(
        &mut self,
        now: Instant,
        settled: bool,
        content: &WorldContent,
        block_at: impl Fn(BlockPos) -> Option<BlockState>,
        mut edit: impl FnMut(BlockPos, BlockState),
    ) {
        if let Some((at, frames)) = self.timing {
            // `settled` is the end of the previous frame. A step applied `frames` frames ago
            // and settled at the end of the previous frame took `frames` frames, its own
            // included (its own end never settles: its remesh is pending then).
            if frames > 1 && settled {
                self.latencies.push((frames, now.duration_since(at)));
                self.timing = None;
            } else {
                self.timing = Some((at, frames + 1));
            }
        }
        if self
            .last
            .is_some_and(|last| now.duration_since(last) < EDIT_PERIOD)
        {
            return;
        }
        let columns = match self.columns {
            Some(columns) => columns,
            None => {
                let top = self.start.y.floor() as i64;
                let (bx, bz) = (self.start.x.floor() as i64, self.start.z.floor() as i64);
                let find = |(dx, dz): (i64, i64)| ground(bx + dx, bz + dz, top, &block_at);
                let (Some(lamp), Some(shaft)) = (find(LAMP_OFFSET), find(SHAFT_OFFSET)) else {
                    return; // not loaded yet
                };
                let (Some(lamp), Some(shaft)) = (lamp, shaft) else {
                    self.skipped += 1;
                    self.last = Some(now);
                    return;
                };
                *self.columns.insert((lamp, shaft))
            }
        };
        let (Some(lamp), Some(cap)) = (
            content.registry.by_name(LAMP),
            content.registry.by_name(CAP),
        ) else {
            return;
        };
        let step = EditStep::CYCLE[self.next % EditStep::CYCLE.len()];
        for (pos, state) in step.edits(columns.0, columns.1, lamp, cap) {
            edit(pos, state);
        }
        self.next += 1;
        self.steps += 1;
        self.last = Some(now);
        // Only a step that starts on a settled world can be timed to its end (a step still
        // being timed keeps its timing).
        if self.timing.is_none() && settled {
            self.timing = Some((now, 1));
        }
    }

    /// The summary, with the world counters at the start and at the end of the measurement.
    pub fn stats(&self, first: &WorldStats, last: &WorldStats) -> EditStats {
        let mut frames: Vec<u32> = self.latencies.iter().map(|l| l.0).collect();
        let mut times: Vec<Duration> = self.latencies.iter().map(|l| l.1).collect();
        frames.sort_unstable();
        times.sort_unstable();
        let mid = |n: usize| n / 2;
        EditStats {
            steps: self.steps,
            skipped: self.skipped,
            timed: self.latencies.len(),
            frames_p50: frames.get(mid(frames.len())).copied().unwrap_or(0),
            frames_max: frames.last().copied().unwrap_or(0),
            ms_p50: times
                .get(mid(times.len()))
                .map_or(0.0, |d| d.as_secs_f64() * 1e3),
            ms_max: times.last().map_or(0.0, |d| d.as_secs_f64() * 1e3),
            batches_held: last
                .edit_batches_held
                .saturating_sub(first.edit_batches_held),
            batches_split: last
                .edit_batches_split
                .saturating_sub(first.edit_batches_split),
            light_fanout: last.light_fanout.saturating_sub(first.light_fanout),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel::voxel::BlockRegistry;
    use std::sync::Arc;

    fn content() -> WorldContent {
        WorldContent::new(
            Arc::new(TerrainGenerator::default()),
            Arc::new(BlockRegistry::default_blocks()),
        )
    }

    #[test]
    fn the_cycle_places_and_removes_a_lamp_then_digs_and_caps_a_shaft() {
        let (lamp, cap) = (BlockState(29), BlockState(1));
        let lg = BlockPos::new(6, 63, 6);
        let sg = BlockPos::new(-6, 60, 6);
        assert_eq!(
            EditStep::PlaceLamp.edits(lg, sg, lamp, cap),
            [(BlockPos::new(6, 64, 6), lamp)]
        );
        assert_eq!(
            EditStep::RemoveLamp.edits(lg, sg, lamp, cap),
            [(BlockPos::new(6, 64, 6), BlockState::AIR)]
        );
        let dig = EditStep::DigShaft.edits(lg, sg, lamp, cap);
        assert_eq!(dig.len(), SHAFT_DEPTH as usize);
        assert_eq!(dig[0], (sg, BlockState::AIR));
        assert_eq!(
            dig.last().unwrap().0,
            BlockPos::new(-6, 60 - SHAFT_DEPTH + 1, 6)
        );
        assert_eq!(EditStep::CapShaft.edits(lg, sg, lamp, cap), [(sg, cap)]);
    }

    #[test]
    fn steps_follow_the_period_and_settling_times_them() {
        let content = content();
        let t0 = Instant::now();
        let ground_at = |p: BlockPos| {
            Some(if p.y <= 63 {
                BlockState(1)
            } else {
                BlockState::AIR
            })
        };
        let mut script = EditScript::new(DVec3::new(0.5, 100.0, 0.5));
        let mut applied = Vec::new();
        let ms = |n| t0 + Duration::from_millis(n);
        // Frame 0: the first step (the lamp), on a settled world.
        script.frame(ms(0), true, &content, ground_at, |p, s| {
            applied.push((p, s));
        });
        assert_eq!(applied, [(BlockPos::new(6, 64, 6), BlockState(29))]);
        // Busy for two frames, settled in the third: 3 frames, 30 ms.
        for (t, settled) in [(10, false), (20, false), (30, true)] {
            script.frame(ms(t), settled, &content, ground_at, |p, s| {
                applied.push((p, s));
            });
        }
        assert_eq!(applied.len(), 1, "the next step waits for the period");
        script.frame(ms(1000), true, &content, ground_at, |p, s| {
            applied.push((p, s));
        });
        assert_eq!(applied[1], (BlockPos::new(6, 64, 6), BlockState::AIR));
        // A step on a busy world is applied but not timed.
        script.frame(ms(2000), false, &content, ground_at, |p, s| {
            applied.push((p, s));
        });
        assert_eq!(applied.len() - 2, SHAFT_DEPTH as usize);
        let first = WorldStats::default();
        let last = WorldStats {
            edit_batches_held: 3,
            edit_batches_split: 1,
            light_fanout: 40,
            ..WorldStats::default()
        };
        let stats = script.stats(&first, &last);
        assert_eq!((stats.steps, stats.skipped, stats.timed), (3, 0, 1));
        assert_eq!((stats.frames_p50, stats.frames_max), (3, 3));
        assert!((stats.ms_max - 30.0).abs() < 1e-9);
        assert_eq!(
            (stats.batches_held, stats.batches_split, stats.light_fanout),
            (3, 1, 40)
        );
        // Not loaded: nothing happens; no ground: skipped.
        let mut waiting = EditScript::new(DVec3::new(0.5, 100.0, 0.5));
        waiting.frame(ms(0), true, &content, |_| None, |_, _| panic!("no edit"));
        assert_eq!((waiting.steps, waiting.skipped), (0, 0));
        waiting.frame(
            ms(0),
            true,
            &content,
            |_| Some(BlockState::AIR),
            |_, _| panic!("no edit"),
        );
        assert_eq!((waiting.steps, waiting.skipped), (0, 1));
    }
}
