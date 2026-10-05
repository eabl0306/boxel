use crate::{AdapterSummary, Classification, GlVersion, Tier, one_line};
use std::fmt::Write as _;
use std::time::Duration;

/// Outcome of one (adapter, tier) attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttemptOutcome {
    /// Device created and smoke test passed (the winner).
    Ok,
    /// `request_device` failed.
    RequestDeviceFailed(String),
    /// The smoke test failed: first error message of the Validation/Internal/OOM scopes, an
    /// uncaptured error, or the device was lost during the test.
    SmokeFailed(String),
    /// Skipped: LKG marks the pair `Bad` (two crashes during bring-up; `--no-lkg` retries).
    SkippedKnownBad,
    /// Skipped: excluded after a device loss in this session.
    SkippedExcluded,
    /// Not attempted (dry run).
    NotAttempted,
}

/// One attempt.
#[derive(Clone, Debug)]
pub struct Attempt {
    /// Tier tried.
    pub tier: Tier,
    /// Candidate score.
    pub score: i64,
    /// Outcome.
    pub outcome: AttemptOutcome,
    /// Wall time of `request_device` + smoke test.
    pub elapsed: Duration,
}

/// Everything known about one adapter.
#[derive(Clone, Debug)]
pub struct CandidateReport {
    /// Identity.
    pub adapter: AdapterSummary,
    /// Static classification.
    pub classification: Classification,
    /// `is_surface_supported` (None when headless).
    pub surface_supported: Option<bool>,
    /// Format chosen by [`crate::choose_surface_format`] for this adapter (None when headless or
    /// when no acceptable format exists, which filters the adapter).
    pub surface_format: Option<wgpu::TextureFormat>,
    /// Why the adapter was filtered out before any attempt (name filter, software policy...).
    pub filtered: Option<String>,
    /// Attempts in the order they ran.
    pub attempts: Vec<Attempt>,
}

/// Decision trace of `select_gpu` (printed by `gpu_info`, logged at info by the renderer).
#[derive(Clone, Debug, Default)]
pub struct SelectionReport {
    /// Instance backends.
    pub backends: wgpu::Backends,
    /// Instance flags.
    pub flags: wgpu::InstanceFlags,
    /// One per enumerated adapter, enumeration order.
    pub candidates: Vec<CandidateReport>,
    /// Index into `candidates` and tier of the winner.
    pub chosen: Option<(usize, Tier)>,
    /// Free-form notes (instance retries, env warnings, overrides applied).
    pub notes: Vec<String>,
}

impl SelectionReport {
    /// Multi-line human-readable table. The last line is always
    /// `DECISION: <summary_line>` or `DECISION: none (<reason>)` (stable, grep-able).
    ///
    /// Format: a header with the instance backends and flags; per adapter
    /// a `[i] name  backend=.. class=.. driver=".." [gl=M.m]` line followed by its max tier
    /// and reasons, surface support, filter reason and attempts; then the notes.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "GPU selection (instance backends: {}, flags: {:?})",
            crate::instance::backends_list(self.backends),
            self.flags
        );
        for (i, cand) in self.candidates.iter().enumerate() {
            write_candidate(&mut out, i, cand);
        }
        out.push_str("notes:\n");
        if self.notes.is_empty() {
            out.push_str("  - (none)\n");
        }
        for note in &self.notes {
            let _ = writeln!(out, "  - {}", one_line(note));
        }
        let _ = write!(out, "DECISION: {}", self.decision());
        out
    }

    /// The text after `DECISION: ` (the winner's summary line, or `none (<reason>)`).
    fn decision(&self) -> String {
        if let Some((index, tier)) = self.chosen
            && let Some(cand) = self.candidates.get(index)
        {
            let a = &cand.adapter;
            return format!(
                "backend={} tier={tier} class={:?} adapter={:?} driver={:?}",
                a.backend, a.class, a.name, a.driver_info
            );
        }
        let attempts = || self.candidates.iter().flat_map(|c| &c.attempts);
        let reason = if self.candidates.is_empty() {
            "no adapters found"
        } else if attempts().any(|a| a.outcome == AttemptOutcome::NotAttempted) {
            "dry run: no devices created"
        } else if self.candidates.iter().all(|c| c.filtered.is_some()) {
            "every adapter was filtered out"
        } else if attempts().next().is_none() {
            "no adapter supports any tier within the requested cap"
        } else {
            "every candidate failed"
        };
        format!("none ({reason})")
    }
}

fn write_candidate(out: &mut String, index: usize, cand: &CandidateReport) {
    let a = &cand.adapter;
    let _ = write!(
        out,
        "[{index}] {}  backend={} class={:?} driver={:?}",
        a.name, a.backend, a.class, a.driver_info
    );
    if a.backend == "Gl"
        && let Some(v) = GlVersion::parse(&a.driver_info)
    {
        let es = if v.es { " es" } else { "" };
        let _ = write!(out, " gl={}.{}{es}", v.major, v.minor);
    }
    out.push('\n');
    let max = cand
        .classification
        .max_tier
        .map_or_else(|| "none".to_string(), |t| t.to_string());
    let _ = write!(out, "    max tier: {max}");
    if !cand.classification.reasons.is_empty() {
        let _ = write!(out, "  ({})", cand.classification.reasons.join("; "));
    }
    out.push('\n');
    match (cand.surface_supported, cand.surface_format) {
        (Some(true), Some(format)) => {
            let _ = writeln!(out, "    surface: supported ({format:?})");
        }
        (Some(true), None) => out.push_str("    surface: supported (no 8-bit format)\n"),
        (Some(false), _) => out.push_str("    surface: not supported\n"),
        (None, _) => {}
    }
    if let Some(reason) = &cand.filtered {
        let _ = writeln!(out, "    filtered: {}", one_line(reason));
    }
    for attempt in &cand.attempts {
        let _ = writeln!(
            out,
            "    attempt {} score={}  {}",
            attempt.tier,
            attempt.score,
            outcome_text(attempt)
        );
    }
}

fn outcome_text(attempt: &Attempt) -> String {
    let ms = attempt.elapsed.as_secs_f64() * 1000.0;
    match &attempt.outcome {
        AttemptOutcome::Ok => format!("OK ({ms:.1} ms)"),
        AttemptOutcome::RequestDeviceFailed(e) => {
            format!("request_device failed ({ms:.1} ms): {}", one_line(e))
        }
        AttemptOutcome::SmokeFailed(e) => {
            format!("smoke test failed ({ms:.1} ms): {}", one_line(e))
        }
        AttemptOutcome::SkippedKnownBad => {
            "skipped: known bad in last-known-good (--no-lkg retries it)".to_string()
        }
        AttemptOutcome::SkippedExcluded => "skipped: excluded in this session".to_string(),
        AttemptOutcome::NotAttempted => "not attempted".to_string(),
    }
}
