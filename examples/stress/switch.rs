//! `--scenario switch` (ADR 0034, #58 M3): the camera flies as in `fly`, and every
//! `--switch-every` seconds of the measurement the world closes and the other of two seeds opens
//! (`--seed` and the next one), in this process, as a game's world change. The camera's viewer
//! is the app's, so it streams in the next world from where it is.
//!
//! Per switch the run reports the close frame (save snapshot and teardown), the frames the world
//! spent closing, the time to `WorldEvent::Closed`, the open frame, the time until the new world
//! settled and the process's resident memory then; for the run, the 99th percentile and the
//! longest of the measured frames outside the declared stalls (the close frame and the open
//! frame, `Time::stall`).
//!
//! In the open frame, with the old world gone and the new one not streamed yet, it also reads
//! malloc's own statistics (glibc): the bytes in use, which a leak makes grow from switch to
//! switch, and the freed bytes malloc keeps in its arenas, resident but nobody's memory. The
//! resident memory alone cannot tell the two apart.
use super::{Bench, Phase, Recorder};
use boxel::prelude::*;
use boxel::world::WorldStats;
use boxel::world::lifecycle::{CloseOptions, WorldLifecycle, WorldStatus};
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Default `--switch-every`.
pub const DEFAULT_EVERY: Duration = Duration::from_secs(20);

/// The two worlds and when to change.
#[derive(Resource)]
pub struct SwitchPlan {
    /// The generators of `--seed` and the next seed (the first is open at the start).
    pub generators: [Arc<dyn WorldGenerator>; 2],
    /// `--switch-every`.
    pub every: Duration,
    /// Switches asked for so far.
    asked: u32,
    /// The generator of the open world.
    current: usize,
}

impl SwitchPlan {
    /// Starts on `generators[0]`.
    pub fn new(generators: [Arc<dyn WorldGenerator>; 2], every: Duration) -> Self {
        Self {
            generators,
            every,
            asked: 0,
            current: 0,
        }
    }

    /// The generator of the next switch due `elapsed` into the measurement, if one is due
    /// (counted once).
    fn due(&mut self, elapsed: Duration) -> Option<Arc<dyn WorldGenerator>> {
        if elapsed.as_secs_f64() < self.every.as_secs_f64() * f64::from(self.asked + 1) {
            return None;
        }
        self.asked += 1;
        self.current = 1 - self.current;
        Some(Arc::clone(&self.generators[self.current]))
    }
}

/// One switch.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Switch {
    /// The frame of the close (`Time::frame_count`).
    pub close_frame: u64,
    /// The frame of the open, once it opened.
    pub open_frame: Option<u64>,
    /// The close's main-thread time and its time to `Closed` (`CloseReport`).
    pub teardown: Duration,
    pub closed_after: Option<Duration>,
    opened_at: Option<Instant>,
    /// From the open to the first settled frame of the new world.
    pub settled_after: Option<Duration>,
    /// Resident memory of the process when the new world settled, and of it the bytes of the
    /// run's own frame records (they grow with every measured frame).
    pub rss: Option<u64>,
    pub logs: u64,
    /// The memory in the open frame (`None` without glibc).
    pub at_open: Option<Memory>,
    /// The open was refused.
    pub failed: bool,
}

/// The process's memory at one moment: resident pages, and malloc's heap over all its arenas
/// (Rust's allocations and the C libraries', the GPU driver's among them).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Memory {
    /// Resident bytes (`VmRSS`).
    pub resident: Option<u64>,
    /// Bytes allocated and not freed (`uordblks + hblkhd`), the run's own records included.
    pub in_use: u64,
    /// Of `in_use`, the run's own frame records.
    pub records: u64,
    /// Freed bytes malloc keeps in its arenas for later allocations (`fordblks`): resident
    /// once touched, but nobody's memory.
    pub free: u64,
    /// What reading malloc's statistics took (it walks every arena's free lists).
    pub took: Duration,
}

impl Memory {
    /// The bytes in use without the run's own records: what the engine and the drivers hold.
    pub fn heap(&self) -> u64 {
        self.in_use.saturating_sub(self.records)
    }

    /// The process's memory now, with `records` bytes of the run's own records in the heap.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    fn now(records: u64) -> Option<Self> {
        let start = Instant::now();
        let info = glibc::mallinfo2();
        let took = start.elapsed();
        Some(Self {
            resident: resident_bytes(),
            in_use: (info.uordblks + info.hblkhd) as u64,
            records,
            free: info.fordblks as u64,
            took,
        })
    }

    /// Without glibc the run does not read the heap.
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    fn now(_records: u64) -> Option<Self> {
        None
    }
}

/// glibc's `struct mallinfo2` (`malloc.h`, glibc 2.33 and later): ten `size_t`.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[repr(C)]
struct MallInfo2 {
    arena: usize,
    ordblks: usize,
    smblks: usize,
    hblks: usize,
    hblkhd: usize,
    usmblks: usize,
    fsmblks: usize,
    uordblks: usize,
    fordblks: usize,
    keepcost: usize,
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[expect(unsafe_code, reason = "malloc's statistics are a C function of glibc")]
mod glibc {
    // SAFETY: `mallinfo2` takes nothing and returns the struct by value (`MallInfo2` has its
    // layout); it only reads malloc's state, under each arena's lock.
    unsafe extern "C" {
        pub safe fn mallinfo2() -> super::MallInfo2;
    }
}

/// A measured frame's wall time, and whether a stall was declared in it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameTime {
    pub frame: u64,
    pub ms: f64,
    pub stall: bool,
}

/// What the switches did.
#[derive(Resource, Debug, Default)]
pub struct SwitchTrack {
    pub every: Duration,
    pub switches: Vec<Switch>,
    /// Every measured frame.
    pub frames: Vec<FrameTime>,
    last_status: Option<WorldStatus>,
}

impl SwitchTrack {
    /// For switches every `every`.
    pub fn new(every: Duration) -> Self {
        Self {
            every,
            ..Self::default()
        }
    }

    fn frame_ms(&self, frame: u64) -> Option<f64> {
        self.frames.iter().find(|f| f.frame == frame).map(|f| f.ms)
    }

    /// The measured frames outside the declared stalls: (99th percentile, longest, count) in
    /// ms.
    pub fn outside(&self) -> (f64, f64, usize) {
        let mut ms: Vec<f64> = self
            .frames
            .iter()
            .filter(|f| !f.stall)
            .map(|f| f.ms)
            .collect();
        ms.sort_by(f64::total_cmp);
        let p99 = ms
            .get(ms.len().saturating_sub(1) * 99 / 100)
            .copied()
            .unwrap_or(0.0);
        (p99, ms.last().copied().unwrap_or(0.0), ms.len())
    }

    /// The `stress: switch N:` lines and the run's `stress: switches` line.
    pub fn lines(&self) -> Vec<String> {
        let ms = |d: Option<Duration>| d.map_or_else(|| "-".to_string(), fmt_ms);
        let mut out: Vec<String> = self
            .switches
            .iter()
            .enumerate()
            .map(|(i, s)| {
                format!(
                    "stress: switch {}: close_frame_ms={} teardown_ms={:.2} closing_frames={} \
                     closed_after_ms={} open_frame_ms={} settled_after_s={} rss_mib={} \
                     records_mib={} at_open: rss_mib={} heap_mib={} malloc_free_mib={} \
                     probe_ms={}{}",
                    i + 1,
                    self.frame_ms(s.close_frame)
                        .map_or("-".to_string(), |v| format!("{v:.2}")),
                    s.teardown.as_secs_f64() * 1e3,
                    s.open_frame
                        .map_or("-".to_string(), |f| (f - s.close_frame).to_string()),
                    ms(s.closed_after),
                    s.open_frame
                        .and_then(|f| self.frame_ms(f))
                        .map_or("-".to_string(), |v| format!("{v:.2}")),
                    s.settled_after
                        .map_or("-".to_string(), |d| format!("{:.2}", d.as_secs_f64())),
                    s.rss.map_or("-".to_string(), |b| (b >> 20).to_string()),
                    s.logs >> 20,
                    mib(s.at_open.and_then(|m| m.resident)),
                    mib1(s.at_open.map(|m| m.heap())),
                    mib1(s.at_open.map(|m| m.free)),
                    s.at_open.map_or("-".to_string(), |m| format!(
                        "{:.2}",
                        m.took.as_secs_f64() * 1e3
                    )),
                    if s.failed { " (the open failed)" } else { "" },
                )
            })
            .collect();
        let (p99, max, count) = self.outside();
        let stalls = self.frames.iter().filter(|f| f.stall).count();
        let rss = |i: usize, own: bool| {
            self.switches
                .get(i)
                .and_then(|s| {
                    s.rss
                        .map(|b| b.saturating_sub(if own { 0 } else { s.logs }))
                })
                .map_or("-".to_string(), |b| (b >> 20).to_string())
        };
        let at_open = |i: usize, of: fn(&Memory) -> u64| {
            mib1(self.switches.get(i).and_then(|s| s.at_open).map(|m| of(&m)))
        };
        out.push(format!(
            "stress: switches={} stall_frames={stalls} outside: frames={count} p99_ms={p99:.2} \
             max_ms={max:.2} rss_mib: first={} tenth={}, without the run's records: first={} \
             tenth={}; at the open, without the records: heap_mib first={} tenth={}, \
             malloc_free_mib first={} tenth={}",
            self.switches.len(),
            rss(0, true),
            rss(9, true),
            rss(0, false),
            rss(9, false),
            at_open(0, Memory::heap),
            at_open(9, Memory::heap),
            at_open(0, |m| m.free),
            at_open(9, |m| m.free),
        ));
        out
    }

    /// The summary's `## World switches` section.
    pub fn markdown(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "## World switches\n");
        let _ = writeln!(
            out,
            "Every {:.0} s the world closed and the other seed opened (ADR 0034).\n",
            self.every.as_secs_f64()
        );
        let _ = writeln!(
            out,
            "| switch | close frame ms | teardown ms | frames closing | closed after ms | open \
             frame ms | settled after s | RSS MiB | records MiB | at the open: RSS MiB | heap \
             MiB | malloc free MiB |\n\
             |---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
        );
        let cell = |v: Option<String>| v.unwrap_or_else(|| "-".to_string());
        for (i, s) in self.switches.iter().enumerate() {
            let _ = writeln!(
                out,
                "| {} | {} | {:.2} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                i + 1,
                cell(self.frame_ms(s.close_frame).map(|v| format!("{v:.2}"))),
                s.teardown.as_secs_f64() * 1e3,
                cell(s.open_frame.map(|f| (f - s.close_frame).to_string())),
                cell(s.closed_after.map(fmt_ms)),
                cell(
                    s.open_frame
                        .and_then(|f| self.frame_ms(f))
                        .map(|v| format!("{v:.2}"))
                ),
                cell(s.settled_after.map(|d| format!("{:.2}", d.as_secs_f64()))),
                cell(s.rss.map(|b| (b >> 20).to_string())),
                s.logs >> 20,
                mib(s.at_open.and_then(|m| m.resident)),
                mib1(s.at_open.map(|m| m.heap())),
                mib1(s.at_open.map(|m| m.free)),
            );
        }
        let (p99, max, count) = self.outside();
        let _ = writeln!(
            out,
            "\nFrames outside the declared stalls: {count}, p99 {p99:.2} ms, longest {max:.2} \
             ms. The run's own frame records grow with every frame: compare the RSS without \
             them. At the open (the old world gone, the new one not streamed yet) the heap is \
             malloc's bytes in use without the records, which grow from switch to switch only \
             with a leak; malloc's free bytes are resident but nobody's.\n"
        );
        out
    }
}

fn fmt_ms(d: Duration) -> String {
    format!("{:.1}", d.as_secs_f64() * 1e3)
}

/// Whole MiB, or `-`.
fn mib(bytes: Option<u64>) -> String {
    bytes.map_or_else(|| "-".to_string(), |b| (b >> 20).to_string())
}

/// MiB with one decimal (a leak of a few hundred KiB a switch shows), or `-`.
fn mib1(bytes: Option<u64>) -> String {
    bytes.map_or_else(
        || "-".to_string(),
        |b| format!("{:.1}", b as f64 / f64::from(1 << 20)),
    )
}

/// Resident memory of this process in bytes (Linux; `None` elsewhere).
fn resident_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kib: u64 = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    Some(kib * 1024)
}

/// `Update`, after `drive`: asks for the switch that is due while measuring (a close, then the
/// open of the other world), once the world is open.
pub fn request(
    time: Res<Time>,
    bench: Res<Bench>,
    lifecycle: Res<WorldLifecycle>,
    settings: Res<WorldSettings>,
    mut plan: ResMut<SwitchPlan>,
    mut commands: Commands,
) {
    let Phase::Measuring { start } = bench.phase else {
        return;
    };
    if !lifecycle.is_open() {
        return;
    }
    let now = time.last_update().unwrap_or_else(Instant::now);
    if let Some(generator) = plan.due(now.saturating_duration_since(start)) {
        commands.close_world(CloseOptions::default());
        commands.open_world(WorldSpec::new(generator, settings.clone()));
    }
}

/// `PreUpdate`, after the lifecycle's driver and `record`: the previous frame's time, then what
/// the lifecycle did in this frame (a close, an open, the end of a close) and whether the new
/// world settled.
pub fn track(
    time: Res<Time>,
    bench: Res<Bench>,
    lifecycle: Res<WorldLifecycle>,
    stats: Res<WorldStats>,
    mut events: MessageReader<WorldEvent>,
    mut track: ResMut<SwitchTrack>,
    mut rec: ResMut<Recorder>,
) {
    let frame = time.frame_count();
    let now = Instant::now();
    if matches!(bench.phase, Phase::Measuring { .. }) && frame > 0 {
        track.frames.push(FrameTime {
            frame: frame - 1,
            ms: time.real_delta().as_secs_f64() * 1e3,
            stall: !time.stall().is_zero(),
        });
    }
    let (mut opened, mut closed, mut failed) = (false, None, false);
    for event in events.read() {
        match event {
            WorldEvent::Opened { .. } => opened = true,
            WorldEvent::Closed { report, .. } => closed = Some((report.teardown, report.took)),
            WorldEvent::OpenFailed { error } => {
                eprintln!("stress: the next world did not open: {error}");
                failed = true;
            }
        }
    }
    let status = lifecycle.status();
    let was_open = track.last_status == Some(WorldStatus::Open);
    if was_open && (status != WorldStatus::Open || opened) {
        // The world closed in this frame (and maybe the next one opened in it too). The meshes
        // the recorder counts were the closed world's: the renderer forgot them.
        track.switches.push(Switch {
            close_frame: frame,
            ..Switch::default()
        });
        rec.resident.clear();
        rec.quads = 0;
    }
    track.last_status = Some(status);
    // The run's own records as allocated (whole blocks and vectors).
    let records = (rec.log.heap_bytes() + track.frames.capacity() * size_of::<FrameTime>()) as u64;
    let Some(switch) = track.switches.last_mut() else {
        return;
    };
    if let Some((teardown, took)) = closed {
        switch.teardown = teardown;
        switch.closed_after = Some(took);
    }
    if opened {
        switch.open_frame = Some(frame);
        switch.opened_at = Some(now);
        // A declared stall frame: the probe's time stays out of the frames compared.
        switch.at_open = Memory::now(records);
    }
    switch.failed |= failed;
    if let (Some(open_frame), Some(at), None) =
        (switch.open_frame, switch.opened_at, switch.settled_after)
        && frame > open_frame
        && stats.chunks > 0
        && stats.is_settled()
    {
        switch.settled_after = Some(now.saturating_duration_since(at));
        switch.rss = resident_bytes();
        switch.logs = records;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_come_every_period_and_alternate() {
        let a: Arc<dyn WorldGenerator> = Arc::new(FlatGenerator::new(Vec::new()));
        let b: Arc<dyn WorldGenerator> = Arc::new(FlatGenerator::new(Vec::new()));
        let mut plan = SwitchPlan::new([Arc::clone(&a), Arc::clone(&b)], Duration::from_secs(4));
        assert!(plan.due(Duration::from_secs(3)).is_none());
        let first = plan.due(Duration::from_secs(4)).expect("due at 4 s");
        assert!(Arc::ptr_eq(&first, &b));
        assert!(plan.due(Duration::from_secs(5)).is_none(), "once");
        let second = plan.due(Duration::from_secs(9)).expect("due at 8 s");
        assert!(Arc::ptr_eq(&second, &a));
    }

    #[test]
    fn the_stall_frames_stay_out_of_the_percentile() {
        let mut track = SwitchTrack::new(Duration::from_secs(4));
        for frame in 0..200 {
            track.frames.push(FrameTime {
                frame,
                ms: if frame == 50 || frame == 52 {
                    40.0
                } else {
                    2.0
                },
                stall: frame == 50 || frame == 52,
            });
        }
        track.switches.push(Switch {
            close_frame: 50,
            open_frame: Some(52),
            teardown: Duration::from_millis(3),
            closed_after: Some(Duration::from_millis(20)),
            settled_after: Some(Duration::from_secs(2)),
            rss: Some(512 << 20),
            logs: 12 << 20,
            at_open: Some(Memory {
                resident: Some(300 << 20),
                in_use: 210 << 20,
                records: 10 << 20,
                free: 40 << 20,
                took: Duration::from_micros(250),
            }),
            ..Switch::default()
        });
        assert_eq!(track.outside(), (2.0, 2.0, 198));
        let lines = track.lines();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("close_frame_ms=40.00 teardown_ms=3.00 closing_frames=2"));
        assert!(lines[0].contains("rss_mib=512 records_mib=12"));
        assert!(
            lines[0]
                .contains("at_open: rss_mib=300 heap_mib=200.0 malloc_free_mib=40.0 probe_ms=0.25")
        );
        assert!(lines[1].contains(
            "first=512 tenth=-, without the run's records: first=500 tenth=-; at the open, \
             without the records: heap_mib first=200.0 tenth=-, malloc_free_mib first=40.0 tenth=-"
        ));
        assert!(lines[1].starts_with("stress: switches=1 stall_frames=2"));
        let markdown = track.markdown();
        assert!(markdown.starts_with("## World switches"));
        assert!(markdown.contains("| 512 | 12 | 300 | 200.0 | 40.0 |"));
    }

    /// malloc's statistics read in this process: the bytes in use cover a live allocation and
    /// drop once it is freed.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn the_heap_counts_what_is_allocated() {
        // zeroed: mapped fresh, never touched, so it costs no resident memory
        const SIZE: u64 = 256 << 20;
        // other tests of this binary allocate and free meanwhile
        const MARGIN: u64 = 64 << 20;
        let before = Memory::now(0).expect("glibc");
        let block = std::hint::black_box(vec![0u8; SIZE as usize]);
        let during = Memory::now(0).expect("glibc");
        drop(block);
        let after = Memory::now(0).expect("glibc");
        assert!(before.resident.is_some());
        assert!(during.heap() + MARGIN >= before.heap() + SIZE);
        assert!(after.heap() + SIZE <= during.heap() + MARGIN);
        let records = Memory::now(1 << 20).expect("glibc");
        assert_eq!(records.heap(), records.in_use - (1 << 20));
    }
}
