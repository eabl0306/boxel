use crate::{AdapterSummary, Classification, Tier};
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
    pub fn to_text(&self) -> String {
        todo!("WP3")
    }
}
