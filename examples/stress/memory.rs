//! Chunk memory in a stress run (ADR 0035, #57 M2): `--chunk-memory fixed|grow`,
//! `--max-chunk-quads N` and `--radius-steps R0,R1@T1,...` (the mesh radius steps to `R1` `T1`
//! seconds into the measurement, and so on), and the summary of what chunk memory did: pages
//! created and released, the slowest creation, pages made inside a frame (all of them, and those
//! after the first second of each radius step, which steady streaming should not need), the
//! peak page bytes and the most one frame moved.
use super::{Bench, Phase};
use boxel::prelude::*;
use boxel::render::ChunkMemoryStats;
use std::time::{Duration, Instant};

/// Seconds after a radius step during which pages made inside a frame are expected (the burst
/// of the new radius).
pub const STEP_GRACE: Duration = Duration::from_secs(1);

/// `--radius-steps`: the radius to settle at, then `(radius, seconds into the measurement)`,
/// the times rising. Each radius in `1..=64`.
pub fn parse_steps(value: &str) -> Result<Vec<(u32, f64)>, String> {
    let mut steps = Vec::new();
    for (i, part) in value.split(',').enumerate() {
        let part = part.trim();
        let (radius, at) = match part.split_once('@') {
            Some((r, t)) if i > 0 => (r, t.trim().parse::<f64>().ok()),
            None if i == 0 => (part, Some(0.0)),
            _ => {
                return Err(format!(
                    "--radius-steps: {part:?}: the first step is a radius, the others radius@seconds"
                ));
            }
        };
        let radius = radius
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|r| (1..=64).contains(r))
            .ok_or_else(|| format!("--radius-steps: {part:?}: the radius is not in 1..=64"))?;
        let at = at
            .filter(|t| t.is_finite() && *t >= 0.0)
            .ok_or_else(|| format!("--radius-steps: {part:?}: bad seconds"))?;
        if steps.last().is_some_and(|&(_, last)| at <= last) && i > 0 {
            return Err(format!("--radius-steps: {part:?}: the times must rise"));
        }
        steps.push((radius, at));
    }
    if steps.len() < 2 {
        return Err("--radius-steps: no step after the first radius".to_string());
    }
    Ok(steps)
}

/// The radius steps of the run and what chunk memory did while measuring.
#[derive(Resource, Debug, Default)]
pub struct MemoryTrack {
    /// `--radius-steps` (empty: none).
    pub steps: Vec<(u32, f64)>,
    /// Steps applied so far (the first is the settle radius).
    pub applied: usize,
    /// Pages made inside a frame when the measurement started, and the last count seen.
    start_emergencies: Option<u32>,
    last_emergencies: u32,
    /// Of them, made later than [`STEP_GRACE`] after the start or after a radius step.
    pub late_emergencies: u32,
    /// The most page bytes seen.
    pub peak_page_bytes: u64,
    /// When the last step (or the measurement) started.
    step_at: Option<Instant>,
}

impl MemoryTrack {
    /// With `steps` (`--radius-steps`, empty for none).
    pub fn new(steps: Vec<(u32, f64)>) -> Self {
        Self {
            steps,
            applied: 1,
            ..Self::default()
        }
    }

    /// One measured frame at `now`, the measurement started at `start`, with `memory` from the
    /// renderer.
    pub fn frame(&mut self, now: Instant, start: Instant, memory: &ChunkMemoryStats) {
        let step_at = *self.step_at.get_or_insert(start);
        let first = *self.start_emergencies.get_or_insert(memory.emergency_pages);
        let seen = self.last_emergencies.max(first);
        let new = memory.emergency_pages.saturating_sub(seen);
        if now.saturating_duration_since(step_at) > STEP_GRACE {
            self.late_emergencies += new;
        }
        self.last_emergencies = memory.emergency_pages.max(first);
        self.peak_page_bytes = self.peak_page_bytes.max(memory.page_bytes);
    }

    /// The radius due at `elapsed` into the measurement, if a step is due (applied once).
    pub fn due(&mut self, elapsed: Duration, now: Instant) -> Option<u32> {
        let &(radius, at) = self.steps.get(self.applied)?;
        if elapsed.as_secs_f64() < at {
            return None;
        }
        self.applied += 1;
        self.step_at = Some(now);
        Some(radius)
    }

    /// Pages made inside a frame while measuring.
    pub fn emergencies(&self) -> u32 {
        self.last_emergencies
            .saturating_sub(self.start_emergencies.unwrap_or(self.last_emergencies))
    }
}

/// `Update`: the radius steps of `--radius-steps`, while measuring (the view distance follows,
/// 32 blocks a chunk of radius as at the start).
pub fn step_radius(
    bench: Res<Bench>,
    mut track: ResMut<MemoryTrack>,
    mut viewers: Query<(&mut Camera, &mut ChunkViewer)>,
) {
    let Phase::Measuring { start } = bench.phase else {
        return;
    };
    let now = Instant::now();
    if let Some(radius) = track.due(now.saturating_duration_since(start), now) {
        for (mut camera, mut viewer) in &mut viewers {
            camera.view_distance = radius as f32 * 32.0;
            viewer.radius = radius;
        }
        println!("stress: radius {radius}");
    }
}

/// The run's chunk memory for the summary.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MemoryRun {
    /// `grow` or `fixed`.
    pub mode: String,
    /// `--max-chunk-quads` (0: automatic).
    pub max_quads: u32,
    /// `--radius-steps` as given (empty: none).
    pub radius_steps: String,
    /// Pages created past the floor and released by shrink, since the start.
    pub pages_created: u32,
    /// See `pages_created`.
    pub pages_released: u32,
    /// Pages at the end.
    pub pages: u32,
    /// The slowest creation between frames, in milliseconds.
    pub max_page_create_ms: f64,
    /// Pages made inside a frame while measuring, and of them those made later than one second
    /// after the start or a radius step.
    pub emergency_pages: u32,
    /// See `emergency_pages`.
    pub late_emergency_pages: u32,
    /// The most page bytes seen while measuring.
    pub peak_page_bytes: u64,
    /// The most bytes one frame moved from page to page.
    pub max_relocated_frame: u64,
    /// Refused chunks waiting at the end.
    pub refused_waiting: u32,
}

impl MemoryRun {
    /// One line for the log.
    pub fn line(&self) -> String {
        format!(
            "stress: chunk memory {}: {} pages ({} created, {} released), slowest creation \
             {:.3} ms, {} pages made in a frame ({} after the first second of a step), peak \
             {} MiB of pages, at most {} KiB moved in a frame, {} chunks waiting",
            self.mode,
            self.pages,
            self.pages_created,
            self.pages_released,
            self.max_page_create_ms,
            self.emergency_pages,
            self.late_emergency_pages,
            self.peak_page_bytes >> 20,
            self.max_relocated_frame >> 10,
            self.refused_waiting
        )
    }
}

/// The summary of a run with `track`, `memory` (the renderer's at the end) and the options.
pub fn summary(
    track: &MemoryTrack,
    memory: &ChunkMemoryStats,
    grow: bool,
    max_quads: Option<u32>,
    steps: &str,
) -> MemoryRun {
    MemoryRun {
        mode: if grow { "grow" } else { "fixed" }.to_string(),
        max_quads: max_quads.unwrap_or(0),
        radius_steps: steps.to_string(),
        pages_created: memory.pages_created,
        pages_released: memory.pages_released,
        pages: memory.pages,
        max_page_create_ms: memory.max_page_create.as_secs_f64() * 1000.0,
        emergency_pages: track.emergencies(),
        late_emergency_pages: track.late_emergencies,
        peak_page_bytes: track.peak_page_bytes.max(memory.page_bytes),
        max_relocated_frame: memory.max_relocated_frame,
        refused_waiting: memory.refused_waiting,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radius_steps_parse() {
        assert_eq!(
            parse_steps("8,32@1,8@20"),
            Ok(vec![(8, 0.0), (32, 1.0), (8, 20.0)])
        );
        assert_eq!(parse_steps(" 8 , 32@1.5 "), Ok(vec![(8, 0.0), (32, 1.5)]));
        for bad in [
            "",
            "8",
            "8@1,32@2",
            "8,32",
            "8,32@2,16@1",
            "0,8@1",
            "8,65@1",
            "8,9@-1",
        ] {
            assert!(parse_steps(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn steps_come_due_once_in_order() {
        let mut track = MemoryTrack::new(parse_steps("8,32@1,8@20").expect("steps"));
        let now = Instant::now();
        assert_eq!(track.due(Duration::from_millis(500), now), None);
        assert_eq!(track.due(Duration::from_secs(1), now), Some(32));
        assert_eq!(track.due(Duration::from_secs(2), now), None);
        assert_eq!(track.due(Duration::from_secs(25), now), Some(8));
        assert_eq!(track.due(Duration::from_secs(30), now), None);
        assert_eq!(MemoryTrack::new(Vec::new()).due(Duration::MAX, now), None);
    }

    #[test]
    fn pages_made_in_a_frame_count_late_only_past_the_grace() {
        let start = Instant::now();
        let mut track = MemoryTrack::new(parse_steps("8,32@1").expect("steps"));
        let memory = |emergency_pages, page_bytes| ChunkMemoryStats {
            emergency_pages,
            page_bytes,
            ..ChunkMemoryStats::default()
        };
        // 2 before the measurement: not the run's
        track.frame(start, start, &memory(2, 10));
        track.frame(start + Duration::from_millis(500), start, &memory(3, 20));
        // the step at 1 s: its first second is the burst's
        assert_eq!(
            track.due(Duration::from_secs(1), start + Duration::from_secs(1)),
            Some(32)
        );
        track.frame(start + Duration::from_millis(1500), start, &memory(5, 40));
        assert_eq!(track.late_emergencies, 0);
        track.frame(start + Duration::from_millis(2500), start, &memory(6, 30));
        assert_eq!(track.late_emergencies, 1);
        assert_eq!(track.emergencies(), 4);
        assert_eq!(track.peak_page_bytes, 40);
        let run = summary(&track, &memory(6, 30), true, None, "8,32@1");
        assert_eq!((run.emergency_pages, run.late_emergency_pages), (4, 1));
        assert!(
            run.line().contains("4 pages made in a frame (1 after"),
            "{}",
            run.line()
        );
    }
}
