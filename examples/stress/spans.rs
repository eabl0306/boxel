//! `--spans`: busy time per system and span in every measured frame (needs feature `trace`),
//! and what is written from it: a summary section and a CSV.
//!
//! The times come from `boxel::log::span_times`: every ECS system run is a span (feature
//! `trace`), plus the coarse spans inside the world and render systems (`world.positions`,
//! `render.upsert`, ...). Systems that run in parallel add up to more than the wall time; the
//! `schedule ...` rows are wall time on the main thread. `outside Main` is the frame time minus the
//! `Main` schedule: the winit runner between two updates.
//!
//! Frames are split in three classes: `border` (the camera crossed a chunk border), `catch-up`
//! (the frames after a crossing up to and including the one after which the world is settled
//! again, cut at the next crossing) and `normal` (the rest).

use crate::report::FrameRecord;
use boxel::log::span_times::SpanFrame;
use std::collections::HashMap;
use std::fmt::Write as _;

/// Pseudo key: the whole frame.
pub const FRAME: &str = "frame";
/// Pseudo key: frame time minus the `Main` schedule.
pub const OUTSIDE_MAIN: &str = "outside Main";
/// Key of the `Main` schedule span.
const MAIN: &str = "schedule Main";
/// Worst border frames listed.
pub const WORST_BORDER_FRAMES: usize = 10;
/// Spans listed for each worst frame.
const TOP_SPANS: usize = 8;
/// Most rows of the per-span tables.
const MAX_ROWS: usize = 45;
/// A span gets a row (and a CSV column) when some class averages at least this much.
const MIN_MEAN_NS: f64 = 2_000.0;
/// Or when its slowest frame took at least this much.
const MIN_MAX_NS: u64 = 50_000;
/// The slowest spans of a worst frame only list spans at least this long.
const MIN_LISTED_NS: u64 = 5_000;

/// The sums of one measured frame: busy nanoseconds, then calls, per key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpanRow(Box<[u32]>);

impl SpanRow {
    /// From the layer's frame (nanoseconds saturate at about 4.3 s).
    pub fn from_frame(frame: &SpanFrame) -> Self {
        let mut v = Vec::with_capacity(2 * frame.busy_ns.len());
        v.extend(
            frame
                .busy_ns
                .iter()
                .map(|&ns| u32::try_from(ns).unwrap_or(u32::MAX)),
        );
        v.extend(frame.calls.iter().copied());
        Self(v.into_boxed_slice())
    }

    fn keys(&self) -> usize {
        self.0.len() / 2
    }

    /// Busy nanoseconds of `key` (0 for keys newer than the row).
    pub fn busy_ns(&self, key: usize) -> u64 {
        if key < self.keys() {
            u64::from(self.0[key])
        } else {
            0
        }
    }

    /// Times `key` was entered.
    pub fn calls(&self, key: usize) -> u32 {
        if key < self.keys() {
            self.0[self.keys() + key]
        } else {
            0
        }
    }
}

/// One row per measured frame, in fixed blocks (no reallocation spikes while measuring).
#[derive(Debug, Default)]
pub struct SpanLog {
    blocks: Vec<Vec<SpanRow>>,
    /// Rows whose frame could not be matched to the record (the layer closed more or fewer than
    /// one frame between two records). They are kept, but the numbers are suspect if not 0.
    pub unmatched: usize,
}

impl SpanLog {
    const BLOCK: usize = 4096;

    /// Adds a row.
    pub fn push(&mut self, row: SpanRow) {
        if self.blocks.last().is_none_or(|b| b.len() == Self::BLOCK) {
            self.blocks.push(Vec::with_capacity(Self::BLOCK));
        }
        if let Some(block) = self.blocks.last_mut() {
            block.push(row);
        }
    }

    /// In order.
    pub fn rows(&self) -> Vec<&SpanRow> {
        self.blocks.iter().flatten().collect()
    }
}

/// Frame class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// The camera crossed a chunk border.
    Border,
    /// After a crossing, until the world is settled again.
    CatchUp,
    /// The rest.
    Normal,
}

impl Class {
    const ALL: [Class; 3] = [Class::Border, Class::CatchUp, Class::Normal];

    fn name(self) -> &'static str {
        match self {
            Class::Border => "border",
            Class::CatchUp => "catch-up",
            Class::Normal => "normal",
        }
    }
}

/// The frames of one crossing: from the crossing frame to the frame after which the world is
/// settled (`finished`), or to the next crossing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    /// The crossing frame.
    pub start: usize,
    /// One past the last frame.
    pub end: usize,
    /// The world settled before the next crossing (or the end).
    pub finished: bool,
}

/// The class of every record, and the crossing windows.
pub fn classify(records: &[FrameRecord]) -> (Vec<Class>, Vec<Window>) {
    let mut class = vec![Class::Normal; records.len()];
    let crossings: Vec<usize> = (0..records.len()).filter(|&i| records[i].crossed).collect();
    let mut windows = Vec::with_capacity(crossings.len());
    for (n, &start) in crossings.iter().enumerate() {
        let next = crossings.get(n + 1).copied().unwrap_or(records.len());
        let settled = (start..next).find(|&j| records[j].world.is_settled());
        let end = settled.map_or(next, |j| j + 1);
        class[start] = Class::Border;
        for c in &mut class[start + 1..end] {
            *c = Class::CatchUp;
        }
        windows.push(Window {
            start,
            end,
            finished: settled.is_some(),
        });
    }
    (class, windows)
}

/// The per-frame values: frame time, outside `Main`, then every key (nanoseconds).
struct Series<'a> {
    records: &'a [FrameRecord],
    rows: Vec<&'a SpanRow>,
    keys: &'a [String],
    main: Option<usize>,
}

impl<'a> Series<'a> {
    fn new(records: &'a [FrameRecord], log: &'a SpanLog, keys: &'a [String]) -> Self {
        let rows = log.rows();
        Self {
            records,
            rows,
            keys,
            main: keys.iter().position(|k| k == MAIN),
        }
    }

    /// Series count: the two pseudo keys plus the keys.
    fn len(&self) -> usize {
        2 + self.keys.len()
    }

    fn name(&self, s: usize) -> &str {
        match s {
            0 => FRAME,
            1 => OUTSIDE_MAIN,
            k => &self.keys[k - 2],
        }
    }

    /// Nanoseconds of series `s` in frame `i` (0 when the frame has no row).
    fn value(&self, s: usize, i: usize) -> u64 {
        let frame = u64::try_from(self.records[i].frame_time.as_nanos()).unwrap_or(u64::MAX);
        let row = self.rows.get(i);
        match s {
            0 => frame,
            1 => {
                let main = match (row, self.main) {
                    (Some(row), Some(m)) => row.busy_ns(m),
                    _ => 0,
                };
                frame.saturating_sub(main)
            }
            k => row.map_or(0, |row| row.busy_ns(k - 2)),
        }
    }

    fn calls(&self, s: usize, i: usize) -> u32 {
        match (s, self.rows.get(i)) {
            (0 | 1, _) | (_, None) => 0,
            (k, Some(row)) => row.calls(k - 2),
        }
    }
}

/// Mean, nearest-rank 99th percentile and maximum, in nanoseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Stats {
    mean: f64,
    p99: u64,
    max: u64,
}

impl Stats {
    fn of(mut values: Vec<u64>) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        let mean = values.iter().map(|&v| v as f64).sum::<f64>() / values.len() as f64;
        values.sort_unstable();
        let rank = (values.len() * 99).div_ceil(100).max(1);
        Self {
            mean,
            p99: values[rank - 1],
            max: values[values.len() - 1],
        }
    }
}

fn ms(ns: f64) -> f64 {
    ns / 1e6
}

/// `boxel_world::plugin::update_interest` -> `world::update_interest`; `system_commands X` ->
/// `commands ...`; paths inside generics lose their modules. Keys without `::` stay.
pub fn short_name(key: &str) -> String {
    if let Some(rest) = key.strip_prefix("system_commands ") {
        return format!("commands {}", short_name(rest));
    }
    if let Some(rest) = key.strip_prefix("check_conditions ") {
        return format!("conditions {}", short_name(rest));
    }
    let Some((krate, rest)) = key.split_once("::") else {
        return key.to_string();
    };
    if krate.contains([' ', '<']) {
        return key.to_string();
    }
    let krate = krate.strip_prefix("boxel_").unwrap_or(krate);
    format!("{krate}::{}", strip_paths(rest))
}

/// Drops every `module::` prefix: `a::b::f<c::D>` -> `f<D>`.
fn strip_paths(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut ident_start = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ':' && chars.peek() == Some(&':') {
            chars.next();
            out.truncate(ident_start);
            continue;
        }
        if !(c.is_alphanumeric() || c == '_') {
            out.push(c);
            ident_start = out.len();
            continue;
        }
        out.push(c);
    }
    out
}

/// Short names, falling back to the full key when two keys would share one.
fn display_names(series: &Series<'_>) -> Vec<String> {
    let short: Vec<String> = (0..series.len())
        .map(|s| short_name(series.name(s)))
        .collect();
    let mut count: HashMap<&str, usize> = HashMap::new();
    for name in &short {
        *count.entry(name.as_str()).or_default() += 1;
    }
    (0..series.len())
        .map(|s| {
            if count[short[s].as_str()] > 1 {
                series.name(s).to_string()
            } else {
                short[s].clone()
            }
        })
        .collect()
}

/// Statistics of each series in each class.
struct ClassStats {
    frames: [usize; 3],
    /// `[class][series]`.
    stats: [Vec<Stats>; 3],
}

impl ClassStats {
    fn compute(series: &Series<'_>, class: &[Class]) -> Self {
        let members: Vec<Vec<usize>> = Class::ALL
            .iter()
            .map(|&c| (0..class.len()).filter(|&i| class[i] == c).collect())
            .collect();
        let stats = std::array::from_fn(|c| {
            (0..series.len())
                .map(|s| Stats::of(members[c].iter().map(|&i| series.value(s, i)).collect()))
                .collect()
        });
        Self {
            frames: std::array::from_fn(|c| members[c].len()),
            stats,
        }
    }

    /// Series worth a row: the pseudo keys, then the keys that matter, slowest border mean
    /// first.
    fn rows(&self) -> Vec<usize> {
        let n = self.stats[0].len();
        let matters = |s: usize| {
            self.stats
                .iter()
                .any(|st| st[s].mean >= MIN_MEAN_NS || st[s].max >= MIN_MAX_NS)
        };
        let mut keys: Vec<usize> = (2..n).filter(|&s| matters(s)).collect();
        keys.sort_by(|&a, &b| {
            let key = |s: usize| (self.stats[0][s].mean, self.stats[1][s].mean);
            key(b)
                .partial_cmp(&key(a))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        keys.truncate(MAX_ROWS);
        let mut rows = vec![0, 1];
        rows.extend(keys);
        rows
    }
}

/// The summary section (Markdown).
pub fn markdown(records: &[FrameRecord], log: &SpanLog, keys: &[String]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "## Time per system (--spans)\n");
    if log.rows().is_empty() || keys.is_empty() {
        let _ = writeln!(
            out,
            "No span was recorded. The binary needs feature `trace`, and `RUST_LOG` must let \
             `info` spans through.\n"
        );
        return out;
    }
    let series = Series::new(records, log, keys);
    let (class, windows) = classify(records);
    let stats = ClassStats::compute(&series, &class);
    let names = display_names(&series);
    let _ = writeln!(
        out,
        "Busy time per span in each frame (feature `trace`; spans cost time too, so compare only \
         runs made the same way). Systems that run in parallel add up to more than the frame; \
         `schedule` rows are wall time on the main thread; `{OUTSIDE_MAIN}` is the frame minus the \
         `Main` schedule. Classes: {} border frames, {} catch-up frames (after a border until the \
         world is settled again), {} normal frames.{}\n",
        stats.frames[0],
        stats.frames[1],
        stats.frames[2],
        if log.unmatched > 0 {
            format!(
                " Warning: {} frames could not be matched to their spans.",
                log.unmatched
            )
        } else {
            String::new()
        }
    );
    let _ = writeln!(
        out,
        "| span (ms) | border mean | p99 | max | catch-up mean | p99 | max | normal mean | p99 | \
         max |"
    );
    let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for s in stats.rows() {
        let _ = write!(out, "| `{}` |", names[s]);
        for class_stats in &stats.stats {
            let st = class_stats[s];
            let _ = write!(
                out,
                " {:.3} | {:.3} | {:.3} |",
                ms(st.mean),
                ms(st.p99 as f64),
                ms(st.max as f64)
            );
        }
        out.push('\n');
    }

    // Per crossing: the sum over the window.
    let finished: Vec<&Window> = windows.iter().filter(|w| w.finished).collect();
    if !finished.is_empty() {
        let _ = writeln!(
            out,
            "\nPer border, summed from the crossing frame to the frame after which the world was \
             settled: {} of {} borders, the ones that settled before the next crossing (a build \
             that catches up more slowly leaves out more of its heavy borders, so compare runs \
             with about the same share). `{FRAME}` is the wall time of those frames; the last \
             column, the frames per border or how many times each span ran. A sum grows with the \
             frames per border even when each frame costs the same, so `above normal` takes away \
             what those frames cost in a normal frame (frames per border times the normal-frame \
             mean): the extra work of a crossing.\n",
            finished.len(),
            windows.len()
        );
        let _ = writeln!(
            out,
            "| span (ms) | mean per border | above normal | max per border | frames or calls |"
        );
        let _ = writeln!(out, "|---|---:|---:|---:|---:|");
        let sums: Vec<Vec<u64>> = (0..series.len())
            .map(|s| {
                finished
                    .iter()
                    .map(|w| (w.start..w.end).map(|i| series.value(s, i)).sum())
                    .collect()
            })
            .collect();
        let window_stats: Vec<Stats> = sums.iter().map(|v| Stats::of(v.clone())).collect();
        let mut rows: Vec<usize> = (2..series.len())
            .filter(|&s| window_stats[s].mean >= 10.0 * MIN_MEAN_NS)
            .collect();
        rows.sort_by(|&a, &b| {
            window_stats[b]
                .mean
                .partial_cmp(&window_stats[a].mean)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        rows.truncate(MAX_ROWS);
        let frames = Stats::of(finished.iter().map(|w| (w.end - w.start) as u64).collect());
        let calls = |s: usize| {
            let total: u64 = finished
                .iter()
                .flat_map(|w| w.start..w.end)
                .map(|i| u64::from(series.calls(s, i)))
                .sum();
            total as f64 / finished.len() as f64
        };
        for s in [0, 1].into_iter().chain(rows) {
            let st = window_stats[s];
            let above = st.mean - frames.mean * stats.stats[2][s].mean;
            let count = match s {
                0 => format!("{:.1} avg, {} max", frames.mean, frames.max),
                1 => String::new(),
                s => format!("{:.0}", calls(s)),
            };
            let _ = writeln!(
                out,
                "| `{}` | {:.3} | {:.3} | {:.3} | {count} |",
                names[s],
                ms(st.mean),
                ms(above),
                ms(st.max as f64)
            );
        }
    }

    // The worst border frames.
    let mut worst: Vec<usize> = (0..records.len())
        .filter(|&i| class[i] == Class::Border)
        .collect();
    worst.sort_by(|&a, &b| {
        records[b]
            .frame_time
            .cmp(&records[a].frame_time)
            .then(a.cmp(&b))
    });
    worst.truncate(WORST_BORDER_FRAMES);
    if !worst.is_empty() {
        let schedules: Vec<(usize, &str)> = [
            "First",
            "PreUpdate",
            "Update",
            "PostUpdate",
            "Last",
            "Render",
        ]
        .into_iter()
        .filter_map(|label| {
            let key = format!("schedule {label}");
            keys.iter().position(|k| *k == key).map(|k| (k + 2, label))
        })
        .collect();
        let _ = writeln!(
            out,
            "\nThe {} slowest border frames (ms; the slowest spans exclude `schedule` rows):\n",
            worst.len()
        );
        let _ = write!(out, "| frame | t s | ms | outside Main |");
        for (_, label) in &schedules {
            let _ = write!(out, " {label} |");
        }
        let _ = writeln!(out, " slowest spans |");
        let _ = write!(out, "|---:|---:|---:|---:|");
        for _ in &schedules {
            out.push_str("---:|");
        }
        out.push_str("---|\n");
        for &i in &worst {
            let _ = write!(
                out,
                "| {i} | {:.2} | {:.2} | {:.2} |",
                records[i].t.as_secs_f64(),
                ms(series.value(0, i) as f64),
                ms(series.value(1, i) as f64)
            );
            for &(s, _) in &schedules {
                let _ = write!(out, " {:.2} |", ms(series.value(s, i) as f64));
            }
            let mut top: Vec<usize> = (2..series.len())
                .filter(|&s| !series.name(s).starts_with("schedule "))
                .filter(|&s| series.value(s, i) >= MIN_LISTED_NS)
                .collect();
            top.sort_by_key(|&s| std::cmp::Reverse(series.value(s, i)));
            top.truncate(TOP_SPANS);
            let text: Vec<String> = top
                .iter()
                .map(|&s| {
                    let calls = series.calls(s, i);
                    let calls = if calls > 1 {
                        format!(" x{calls}")
                    } else {
                        String::new()
                    };
                    format!("`{}` {:.2}{calls}", names[s], ms(series.value(s, i) as f64))
                })
                .collect();
            let _ = writeln!(out, " {} |", text.join(", "));
        }
    }
    out.push('\n');
    out
}

/// The span CSV: one row per frame with its class and crossing, the frame time, the time outside
/// `Main` and every span that gets a row in the summary, in milliseconds.
pub fn csv(records: &[FrameRecord], log: &SpanLog, keys: &[String]) -> String {
    let series = Series::new(records, log, keys);
    let (class, windows) = classify(records);
    let names = display_names(&series);
    let stats = ClassStats::compute(&series, &class);
    let mut columns = stats.rows();
    columns.sort_unstable();
    let mut crossing = vec![None; records.len()];
    for (n, w) in windows.iter().enumerate() {
        for c in &mut crossing[w.start..w.end] {
            *c = Some(n);
        }
    }
    let mut out = String::with_capacity((records.len() + 1) * (24 + 8 * columns.len()));
    out.push_str("frame,t_ms,class,crossing");
    for &s in &columns {
        out.push(',');
        out.push_str(&csv_field(if s == 0 { "frame_ms" } else { &names[s] }));
    }
    out.push('\n');
    for (i, ((record, class), crossing)) in records.iter().zip(&class).zip(&crossing).enumerate() {
        let _ = write!(
            out,
            "{i},{:.3},{},{}",
            record.t.as_secs_f64() * 1e3,
            class.name(),
            crossing.map_or_else(String::new, |n| n.to_string())
        );
        for &s in &columns {
            match series.value(s, i) {
                0 => out.push_str(",0"),
                v => {
                    let _ = write!(out, ",{:.4}", ms(v as f64));
                }
            }
        }
        out.push('\n');
    }
    out
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel::world::WorldStats;
    use std::time::Duration;

    const BUSY: WorldStats = WorldStats {
        chunks: 10,
        data_ready: 0,
        generating: 0,
        mesh_ready: 0,
        mesh_empty: 0,
        meshing: 0,
        queued_jobs: 5,
        in_flight: 0,
        interest_backlog: 0,
    };

    /// Frame `i` lasts `ms[i]`; crossings at `crossed`, the world busy at `busy`.
    fn records(ms: &[u64], crossed: &[usize], busy: &[usize]) -> Vec<FrameRecord> {
        let mut t = Duration::ZERO;
        ms.iter()
            .enumerate()
            .map(|(i, &m)| {
                let r = FrameRecord {
                    t,
                    frame_time: Duration::from_millis(m),
                    crossed: crossed.contains(&i),
                    world: if busy.contains(&i) {
                        BUSY
                    } else {
                        WorldStats::default()
                    },
                    ..FrameRecord::default()
                };
                t += r.frame_time;
                r
            })
            .collect()
    }

    fn row(busy_ns: &[u64]) -> SpanRow {
        SpanRow::from_frame(&SpanFrame {
            seq: 0,
            busy_ns: busy_ns.to_vec(),
            calls: vec![1; busy_ns.len()],
        })
    }

    #[test]
    fn classes_and_windows() {
        // Crossing at 1 settles after frame 3; crossing at 5 never settles before 7; at 7 the
        // crossing frame itself is settled.
        let r = records(&[1; 9], &[1, 5, 7], &[1, 2, 3, 5, 6]);
        let (class, windows) = classify(&r);
        use Class::{Border as B, CatchUp as C, Normal as N};
        assert_eq!(class, [N, B, C, C, C, B, C, B, N]);
        assert_eq!(
            windows,
            [
                Window {
                    start: 1,
                    end: 5,
                    finished: true
                },
                Window {
                    start: 5,
                    end: 7,
                    finished: false
                },
                Window {
                    start: 7,
                    end: 8,
                    finished: true
                },
            ]
        );
    }

    #[test]
    fn short_names() {
        assert_eq!(
            short_name("boxel_world::plugin::update_interest"),
            "world::update_interest"
        );
        assert_eq!(
            short_name("system_commands boxel_render::plugin::setup"),
            "commands render::setup"
        );
        assert_eq!(
            short_name("boxel_ecs::message::update::message_update_system<boxel_app::app::X>"),
            "ecs::message_update_system<X>"
        );
        assert_eq!(short_name("stress::record"), "stress::record");
        assert_eq!(short_name("schedule Main"), "schedule Main");
        assert_eq!(short_name("world.positions"), "world.positions");
    }

    #[test]
    fn section_and_csv() {
        let keys: Vec<String> = [
            "schedule Main",
            "boxel_world::plugin::update_interest",
            "tiny",
        ]
        .map(String::from)
        .to_vec();
        let r = records(&[1, 4, 2, 1], &[1], &[1]);
        let mut log = SpanLog::default();
        for busy in [
            [900_000, 0, 10],
            [3_800_000, 2_500_000, 10],
            [1_900_000, 500_000, 10],
            [900_000, 0, 10],
        ] {
            log.push(row(&busy));
        }
        let md = markdown(&r, &log, &keys);
        assert!(md.contains("1 border frames, 1 catch-up frames"), "{md}");
        // Border: update_interest 2.5 ms; outside Main is 4 - 3.8 ms.
        assert!(
            md.contains("| `world::update_interest` | 2.500 | 2.500 | 2.500 | 0.500 |"),
            "{md}"
        );
        assert!(md.contains("| `outside Main` | 0.200 |"), "{md}");
        assert!(!md.contains("`tiny`"), "too small for a row: {md}");
        // One finished border: frames 1 and 2 (6 ms), 4 ms more than two normal frames (1 ms
        // each, no update_interest).
        assert!(md.contains("1 of 1 borders"), "{md}");
        assert!(
            md.contains("| `frame` | 6.000 | 4.000 | 6.000 | 2.0 avg, 2 max |"),
            "{md}"
        );
        assert!(
            md.contains("| `world::update_interest` | 3.000 | 3.000 | 3.000 | 2 |"),
            "{md}"
        );
        assert!(md.contains("`world::update_interest` 2.50"), "{md}");

        let csv = csv(&r, &log, &keys);
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(
            lines[0],
            "frame,t_ms,class,crossing,frame_ms,outside Main,schedule Main,world::update_interest"
        );
        assert_eq!(lines[2], "1,1.000,border,0,4.0000,0.2000,3.8000,2.5000");
        assert_eq!(lines[4], "3,7.000,normal,,1.0000,0.1000,0.9000,0");
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn rows_outlive_new_keys() {
        let r = row(&[5, 6]);
        assert_eq!((r.busy_ns(1), r.calls(1)), (6, 1));
        assert_eq!((r.busy_ns(7), r.calls(7)), (0, 0));
        assert_eq!(csv_field("a<b, c>"), "\"a<b, c>\"");
    }
}
