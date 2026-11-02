//! `--edits`: scripted block edits during the measurement, and how long the world takes to
//! publish them. Every [`EDIT_PERIOD`] the script applies the next step of a cycle around the
//! start position: place a lamp on the ground, remove it, dig a shaft down from the ground, cap
//! it with stone. With sky sources in the registry (`--sky-sources`, ADR 0061) the cycle goes on:
//! place a source over the ground, block its beam halfway down with stone, open it again, remove
//! the source. Each step that starts on a settled world is timed until the world settles
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
/// Offset (x, z) of the sky source's column.
pub const SOURCE_OFFSET: (i64, i64) = (0, -6);
/// Highest cell of the source over the ground: the top of the air there, at most this high.
pub const SOURCE_HEIGHT: i64 = 8;
/// Fewest air cells over the ground for the source steps: the source, the blocker and a cell
/// of beam between them and the ground.
const SOURCE_ROOM: i64 = 3;
/// How far below the start the ground is searched.
const GROUND_DEPTH: i64 = 512;
/// The lamp the script places.
const LAMP: &str = "boxel:lamp_orange";
/// What caps the shaft and blocks the beam.
const CAP: &str = "boxel:stone";
/// The source the script places (down, range 64).
const SOURCE: &str = boxel_demo::blocks::SKY_SOURCE;

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
    /// A sky source over the ground: its beam lights down to the ground.
    PlaceSource,
    /// Stone halfway down the beam: it ends there.
    BlockBeam,
    /// The stone removed: the beam reaches the ground again.
    OpenBeam,
    /// The source removed (the world is as before).
    RemoveSource,
}

/// Where the script edits: the ground blocks of the lamp and shaft columns, and the source's
/// cell with the cell that blocks its beam (`None`: no source steps).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spots {
    pub lamp: BlockPos,
    pub shaft: BlockPos,
    pub beam: Option<(BlockPos, BlockPos)>,
}

/// What the script places: the lamp, the stone that caps the shaft and blocks the beam, and
/// the source (`None` without sources in the registry).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Blocks {
    pub lamp: BlockState,
    pub cap: BlockState,
    pub source: Option<BlockState>,
}

impl EditStep {
    /// The cycle.
    pub const CYCLE: [EditStep; 4] = [
        EditStep::PlaceLamp,
        EditStep::RemoveLamp,
        EditStep::DigShaft,
        EditStep::CapShaft,
    ];

    /// The cycle with sky sources: [`EditStep::CYCLE`], then the source's steps.
    pub const SOURCE_CYCLE: [EditStep; 8] = [
        EditStep::PlaceLamp,
        EditStep::RemoveLamp,
        EditStep::DigShaft,
        EditStep::CapShaft,
        EditStep::PlaceSource,
        EditStep::BlockBeam,
        EditStep::OpenBeam,
        EditStep::RemoveSource,
    ];

    /// The block changes of this step at `spots`; none for a source step without a source or
    /// a beam spot.
    pub fn edits(self, spots: &Spots, blocks: Blocks) -> Vec<(BlockPos, BlockState)> {
        let above = |p: BlockPos| BlockPos::new(p.x, p.y + 1, p.z);
        let beam = blocks.source.zip(spots.beam);
        match self {
            EditStep::PlaceLamp => vec![(above(spots.lamp), blocks.lamp)],
            EditStep::RemoveLamp => vec![(above(spots.lamp), BlockState::AIR)],
            EditStep::DigShaft => (0..SHAFT_DEPTH)
                .map(|d| {
                    let p = BlockPos::new(spots.shaft.x, spots.shaft.y - d, spots.shaft.z);
                    (p, BlockState::AIR)
                })
                .collect(),
            EditStep::CapShaft => vec![(spots.shaft, blocks.cap)],
            EditStep::PlaceSource => beam.map(|(s, (at, _))| (at, s)).into_iter().collect(),
            EditStep::BlockBeam => beam
                .map(|(_, (_, b))| (b, blocks.cap))
                .into_iter()
                .collect(),
            EditStep::OpenBeam => beam
                .map(|(_, (_, b))| (b, BlockState::AIR))
                .into_iter()
                .collect(),
            EditStep::RemoveSource => beam
                .map(|(_, (at, _))| (at, BlockState::AIR))
                .into_iter()
                .collect(),
        }
    }
}

/// The source's cell and the blocker's over `ground`: the source at the top of the air over
/// the ground (at most [`SOURCE_HEIGHT`] up), the blocker halfway between it and the ground.
/// `None` while a block on the way is not loaded, `Some(None)` with fewer than 3 air cells.
pub fn beam_spot(
    ground: BlockPos,
    block_at: impl Fn(BlockPos) -> Option<BlockState>,
) -> Option<Option<(BlockPos, BlockPos)>> {
    let at = |dy: i64| BlockPos::new(ground.x, ground.y + dy, ground.z);
    let mut room = 0;
    while room < SOURCE_HEIGHT && block_at(at(room + 1))?.is_air() {
        room += 1;
    }
    Some((room >= SOURCE_ROOM).then(|| (at(room), at(1 + (room - 1) / 2))))
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
    /// Where the steps go, found before the first step.
    spots: Option<Spots>,
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
            spots: None,
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
        let (Some(lamp), Some(cap)) = (
            content.registry.by_name(LAMP),
            content.registry.by_name(CAP),
        ) else {
            return;
        };
        let blocks = Blocks {
            lamp,
            cap,
            source: content.registry.by_name(SOURCE),
        };
        let spots = match self.spots {
            Some(spots) => spots,
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
                let beam = match blocks.source.map(|_| find(SOURCE_OFFSET)) {
                    Some(None) => return, // not loaded yet
                    Some(Some(Some(ground))) => {
                        let Some(spot) = beam_spot(ground, &block_at) else {
                            return;
                        };
                        spot
                    }
                    // No source, or no ground in reach of its column.
                    None | Some(Some(None)) => None,
                };
                *self.spots.insert(Spots { lamp, shaft, beam })
            }
        };
        let cycle: &[EditStep] = if blocks.source.is_some() {
            &EditStep::SOURCE_CYCLE
        } else {
            &EditStep::CYCLE
        };
        let step = cycle[self.next % cycle.len()];
        let changes = step.edits(&spots, blocks);
        self.next += 1;
        self.last = Some(now);
        if changes.is_empty() {
            // A source step without room for a beam.
            self.skipped += 1;
            return;
        }
        for (pos, state) in changes {
            edit(pos, state);
        }
        self.steps += 1;
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
    use boxel_demo::worldgen::TerrainGenerator;
    use std::sync::Arc;

    fn content() -> WorldContent {
        WorldContent::new(
            Arc::new(TerrainGenerator::default()),
            Arc::new(boxel_demo::blocks::registry()),
        )
    }

    #[test]
    fn the_cycle_places_and_removes_a_lamp_then_digs_and_caps_a_shaft() {
        let (lamp, cap) = (BlockState(29), BlockState(1));
        let sg = BlockPos::new(-6, 60, 6);
        let spots = Spots {
            lamp: BlockPos::new(6, 63, 6),
            shaft: sg,
            beam: None,
        };
        let blocks = Blocks {
            lamp,
            cap,
            source: None,
        };
        assert_eq!(
            EditStep::PlaceLamp.edits(&spots, blocks),
            [(BlockPos::new(6, 64, 6), lamp)]
        );
        assert_eq!(
            EditStep::RemoveLamp.edits(&spots, blocks),
            [(BlockPos::new(6, 64, 6), BlockState::AIR)]
        );
        let dig = EditStep::DigShaft.edits(&spots, blocks);
        assert_eq!(dig.len(), SHAFT_DEPTH as usize);
        assert_eq!(dig[0], (sg, BlockState::AIR));
        assert_eq!(
            dig.last().unwrap().0,
            BlockPos::new(-6, 60 - SHAFT_DEPTH + 1, 6)
        );
        assert_eq!(EditStep::CapShaft.edits(&spots, blocks), [(sg, cap)]);
        // Without a source, its steps change nothing.
        assert!(EditStep::PlaceSource.edits(&spots, blocks).is_empty());
    }

    #[test]
    fn with_sources_the_cycle_places_a_source_blocks_its_beam_and_removes_it() {
        let registry = boxel_demo::blocks::registry_with_sky_source();
        let content = WorldContent::new(Arc::new(TerrainGenerator::default()), Arc::new(registry));
        let source = content.registry.by_name(SOURCE).expect("the source");
        let stone = content.registry.by_name(CAP).expect("stone");
        let t0 = Instant::now();
        // Ground at y = 63, air over it; the source column has a roof at y = 69.
        let world = |p: BlockPos| {
            let roof = (p.x, p.z) == SOURCE_OFFSET && p.y == 69;
            Some(if p.y <= 63 || roof {
                BlockState(1)
            } else {
                BlockState::AIR
            })
        };
        // Five cells of air: the source at the top, the blocker at 64 + 2.
        assert_eq!(
            beam_spot(BlockPos::new(0, 63, -6), world),
            Some(Some((BlockPos::new(0, 68, -6), BlockPos::new(0, 66, -6))))
        );
        // Open sky: the source at 63 + SOURCE_HEIGHT; a low roof leaves no room.
        let open = beam_spot(BlockPos::new(0, 63, 0), |p| {
            Some(if p.y <= 63 {
                BlockState(1)
            } else {
                BlockState::AIR
            })
        });
        assert_eq!(open.unwrap().unwrap().0.y, 63 + SOURCE_HEIGHT);
        let low = |p: BlockPos| {
            Some(if p.y == 65 {
                BlockState(1)
            } else {
                BlockState::AIR
            })
        };
        assert_eq!(beam_spot(BlockPos::new(0, 63, 0), low), Some(None));
        assert_eq!(beam_spot(BlockPos::new(0, 63, 0), |_| None), None);

        // Started under the roof, as in a cave.
        let mut script = EditScript::new(DVec3::new(0.5, 66.0, 0.5));
        let mut applied = Vec::new();
        for i in 0..EditStep::SOURCE_CYCLE.len() as u64 {
            let at = t0 + EDIT_PERIOD * u32::try_from(i).unwrap();
            let before = applied.len();
            script.frame(at, true, &content, world, |p, s| applied.push((p, s)));
            assert!(applied.len() > before, "step {i} edits");
        }
        let (at, blocker) = (BlockPos::new(0, 68, -6), BlockPos::new(0, 66, -6));
        let n = applied.len();
        assert_eq!(
            applied[n - 4..],
            [
                (at, source),
                (blocker, stone),
                (blocker, BlockState::AIR),
                (at, BlockState::AIR),
            ]
        );
        assert_eq!((script.steps, script.skipped), (8, 0));
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
