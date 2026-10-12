//! `--alloc-probe MIB[,MIB...]`: the allocation probe of chunk memory (ADR 0035, #57 M0). While
//! measuring, every half second the renderer creates one buffer with the usages of a page of
//! chunk quads, right after present, where pages will be created: four of each size, in order,
//! then one request drops them all. Each creation prints its time, whether the GPU allocator
//! needed a new memory block, and the time of the frame it ran in (a long creation shows as a
//! late frame). The lines land in the run's log.
use super::{Bench, Phase};
use boxel::prelude::*;
use boxel::render::DebugAllocProbe;
use std::time::{Duration, Instant};

/// Buffers created of each size.
pub const PER_SIZE: usize = 4;
/// Time between two requests.
pub const INTERVAL: Duration = Duration::from_millis(500);

/// `--alloc-probe`'s sizes in MiB (each 1..=1024).
pub fn parse(value: &str) -> Result<Vec<u32>, String> {
    let sizes: Vec<u32> = value
        .split(',')
        .map(|v| {
            v.trim()
                .parse::<u32>()
                .ok()
                .filter(|n| (1..=1024).contains(n))
                .ok_or_else(|| format!("--alloc-probe: {v:?} is not a size in 1..=1024 MiB"))
        })
        .collect::<Result<_, _>>()?;
    if sizes.is_empty() {
        return Err("--alloc-probe: no size".to_string());
    }
    Ok(sizes)
}

/// The requests: [`PER_SIZE`] of each size, then 0 (drop every probe buffer).
pub fn requests(sizes: &[u32]) -> Vec<u32> {
    sizes
        .iter()
        .flat_map(|&mib| [mib; PER_SIZE])
        .chain([0])
        .collect()
}

/// The probe's progress.
#[derive(Resource)]
pub struct AllocProbe {
    /// What is left to request, in order.
    requests: Vec<u32>,
    next: usize,
    last: Option<Instant>,
    /// `RenderInfo::alloc_probe`'s sequence number already printed.
    printed: u64,
}

impl AllocProbe {
    pub fn new(sizes: &[u32]) -> Self {
        Self {
            requests: requests(sizes),
            next: 0,
            last: None,
            printed: 0,
        }
    }
}

/// `Update`: while measuring, the next request every [`INTERVAL`].
pub fn request(
    time: Res<Time>,
    bench: Res<Bench>,
    mut probe: ResMut<AllocProbe>,
    mut writer: MessageWriter<DebugAllocProbe>,
) {
    if !matches!(bench.phase, Phase::Measuring { .. }) || probe.next >= probe.requests.len() {
        return;
    }
    let now = time.last_update().unwrap_or_else(Instant::now);
    if probe.last.is_some_and(|t| now.duration_since(t) < INTERVAL) {
        return;
    }
    let mib = probe.requests[probe.next];
    probe.next += 1;
    probe.last = Some(now);
    writer.write(DebugAllocProbe { mib });
}

/// `PreUpdate`: prints what the last frame's probe saw, with that frame's time.
pub fn report(time: Res<Time>, info: Res<RenderInfo>, mut probe: ResMut<AllocProbe>) {
    let Some((seq, r)) = info.alloc_probe else {
        return;
    };
    if seq == probe.printed {
        return;
    }
    probe.printed = seq;
    let frame_ms = time.real_delta().as_secs_f64() * 1e3;
    let blocks = r.blocks.map_or_else(
        || "blocks unknown".to_string(),
        |(before, after)| {
            format!(
                "blocks {before} -> {after} (new block: {})",
                if after > before { "yes" } else { "no" }
            )
        },
    );
    let reserved = r.reserved.map_or_else(String::new, |(before, after)| {
        format!(", reserved {} -> {} MiB", before >> 20, after >> 20)
    });
    if r.mib == 0 {
        println!(
            "stress: alloc probe: dropped every buffer, {blocks}{reserved}, frame {frame_ms:.2} ms"
        );
    } else {
        println!(
            "stress: alloc probe: {} MiB created in {:.3} ms ({} alive), {blocks}{reserved}, \
             frame {frame_ms:.2} ms",
            r.mib,
            r.create.as_secs_f64() * 1e3,
            r.buffers
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_are_parsed_and_requested_four_times_then_dropped() {
        assert_eq!(parse("32,64"), Ok(vec![32, 64]));
        assert_eq!(parse(" 8 "), Ok(vec![8]));
        assert!(parse("").is_err());
        assert!(parse("0").is_err());
        assert!(parse("32,x").is_err());
        assert!(parse("2048").is_err());
        assert_eq!(requests(&[32, 64]), [32, 32, 32, 32, 64, 64, 64, 64, 0]);
    }
}
