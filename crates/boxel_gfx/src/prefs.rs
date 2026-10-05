use crate::Tier;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Present mode preference (mapped to the closest supported `wgpu::PresentMode`, else `Fifo`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PresentModePref {
    /// `Fifo` (always supported).
    #[default]
    Vsync,
    /// `Mailbox`, else `Fifo`.
    Mailbox,
    /// `Immediate`, else `Mailbox`, else `Fifo`.
    Immediate,
}

/// Accepted `--present` values, for help texts.
pub const PRESENT_MODES: &str = "vsync|mailbox|immediate";

/// GPU selection preferences. Every field is optional so layers can be merged with
/// [`GpuPreferences::overlay`]. Resolution order (frozen, highest first):
/// CLI > `BOXEL_*` env > `WGPU_*` env > config file > automatic.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GpuPreferences {
    /// Backends for the instance (`vulkan,gl,dx12,metal`). Empty parse results are ignored with a warning.
    #[serde(with = "backends_serde")]
    pub backends: Option<wgpu::Backends>,
    /// Case-insensitive substring of `AdapterInfo::name`. No match is an error (explicit intent).
    pub adapter: Option<String>,
    /// Power preference (default `HighPerformance`).
    #[serde(with = "power_serde")]
    pub power: Option<wgpu::PowerPreference>,
    /// Tier cap (`--tier`): the selection never goes above it.
    pub max_tier: Option<Tier>,
    /// Only consider `DeviceClass::Software` adapters (`--software`).
    pub software: Option<bool>,
    /// Backend validation (`InstanceFlags::DEBUG | VALIDATION`). Default: debug builds.
    pub validation: Option<bool>,
    /// Read/write the last-known-good file. Default ([`GpuPreferences::lkg_enabled`]): on in
    /// release builds, off when `debug_assertions` are on (dev builds crash for reasons that
    /// are not driver failures).
    pub use_lkg: Option<bool>,
    /// Present mode preference.
    pub present_mode: Option<PresentModePref>,
}

/// Help text for the shared GPU command-line flags (examples print it on `--help`).
pub const CLI_HELP: &str = "\
GPU options (also BOXEL_* / WGPU_* env vars, see docs):
  --backend <list>     vulkan,gl,dx12,metal (comma list)      [BOXEL_BACKEND, WGPU_BACKEND]
  --adapter <substr>   pick adapter by name substring         [BOXEL_ADAPTER, WGPU_ADAPTER_NAME]
  --power <high|low>   power preference                       [BOXEL_POWER, WGPU_POWER_PREF]
  --tier <high|mid|low> cap the render tier                   [BOXEL_TIER]
  --software           only CPU rasterizers (lavapipe/llvmpipe/WARP) [BOXEL_SOFTWARE=1]
  --validation         backend validation layers              [BOXEL_GPU_VALIDATION=1]
  --lkg / --no-lkg     use / ignore last-known-good (default: on in release builds) [BOXEL_LKG=1|0]
  --present <mode>     vsync|mailbox|immediate                [BOXEL_PRESENT]
  --gpu-config <path>  TOML file with a [gpu] table           [BOXEL_GPU_CONFIG]
";

/// Errors from parsing preferences.
#[derive(Debug, thiserror::Error)]
pub enum PrefsError {
    /// Unknown or malformed value.
    #[error("invalid value {value:?} for {key}: expected {expected}")]
    BadValue {
        /// Flag or variable name.
        key: String,
        /// Offending value.
        value: String,
        /// Accepted values.
        expected: &'static str,
    },
    /// Flag without its value.
    #[error("missing value for {0}")]
    MissingValue(String),
    /// Config file unreadable.
    #[error("cannot read GPU config {path}: {source}")]
    Io {
        /// File path.
        path: String,
        /// Cause.
        source: std::io::Error,
    },
    /// Config file is not valid TOML / schema.
    #[error("invalid GPU config {path}: {message}")]
    Parse {
        /// File path.
        path: String,
        /// Parser message.
        message: String,
    },
}

impl GpuPreferences {
    /// Field-by-field merge: `self` wins, `lower` fills the gaps.
    pub fn overlay(self, lower: GpuPreferences) -> GpuPreferences {
        GpuPreferences {
            backends: self.backends.or(lower.backends),
            adapter: self.adapter.or(lower.adapter),
            power: self.power.or(lower.power),
            max_tier: self.max_tier.or(lower.max_tier),
            software: self.software.or(lower.software),
            validation: self.validation.or(lower.validation),
            use_lkg: self.use_lkg.or(lower.use_lkg),
            present_mode: self.present_mode.or(lower.present_mode),
        }
    }

    /// Parses and removes the GPU flags from `args` (program name excluded); returns the
    /// preferences, the config path given by `--gpu-config` and the remaining args in order.
    pub fn from_args(
        args: &[String],
    ) -> Result<(GpuPreferences, Option<std::path::PathBuf>, Vec<String>), PrefsError> {
        todo!("WP3")
    }

    /// Reads `BOXEL_BACKEND`, `BOXEL_ADAPTER`, `BOXEL_POWER`, `BOXEL_TIER`, `BOXEL_SOFTWARE`,
    /// `BOXEL_GPU_VALIDATION`, `BOXEL_NO_LKG`, `BOXEL_PRESENT`. Invalid values are skipped and
    /// reported in the returned warnings (never fatal).
    pub fn from_boxel_env() -> (GpuPreferences, Vec<String>) {
        todo!("WP3")
    }

    /// Reads `WGPU_BACKEND` (via `Backends::from_comma_list`; empty result ignored with a
    /// warning), `WGPU_ADAPTER_NAME`, `WGPU_POWER_PREF` (`high|low|none`).
    pub fn from_wgpu_env() -> (GpuPreferences, Vec<String>) {
        todo!("WP3")
    }

    /// Reads the `[gpu]` table of a TOML file (same field names as this struct;
    /// `backends = "vulkan,gl"`, `power = "high"`, `max_tier = "mid"`).
    pub fn from_config_file(path: &Path) -> Result<GpuPreferences, PrefsError> {
        todo!("WP3")
    }

    /// Full resolution: `cli` > BOXEL env > WGPU env > config file (`config_path`, or
    /// `BOXEL_GPU_CONFIG` if `None`; a missing file is fine, a broken one is a warning).
    /// Returns the merged preferences and all warnings.
    pub fn resolve(
        cli: GpuPreferences,
        config_path: Option<&Path>,
    ) -> (GpuPreferences, Vec<String>) {
        todo!("WP3")
    }

    /// Whether last-known-good is read and written: `use_lkg`, else `!cfg!(debug_assertions)`.
    pub fn lkg_enabled(&self) -> bool {
        self.use_lkg.unwrap_or(!cfg!(debug_assertions))
    }

    /// Effective power preference (`HighPerformance` if unset; `None` maps to `HighPerformance`).
    pub fn power_or_default(&self) -> wgpu::PowerPreference {
        match self.power {
            Some(wgpu::PowerPreference::LowPower) => wgpu::PowerPreference::LowPower,
            _ => wgpu::PowerPreference::HighPerformance,
        }
    }
}

mod backends_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    #[expect(clippy::trivially_copy_pass_by_ref, reason = "serde `with` signature")]
    pub fn serialize<S: Serializer>(v: &Option<wgpu::Backends>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(b) => s.serialize_some(&format!("{b:?}")),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<wgpu::Backends>, D::Error> {
        let s: Option<String> = Option::deserialize(d)?;
        Ok(s.map(|s| wgpu::Backends::from_comma_list(&s))
            .filter(|b| !b.is_empty()))
    }
}

mod power_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    #[expect(clippy::trivially_copy_pass_by_ref, reason = "serde `with` signature")]
    pub fn serialize<S: Serializer>(
        v: &Option<wgpu::PowerPreference>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match v {
            Some(wgpu::PowerPreference::LowPower) => s.serialize_some("low"),
            Some(wgpu::PowerPreference::HighPerformance) => s.serialize_some("high"),
            Some(wgpu::PowerPreference::None) => s.serialize_some("none"),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<wgpu::PowerPreference>, D::Error> {
        let s: Option<String> = Option::deserialize(d)?;
        Ok(s.and_then(|s| match s.to_ascii_lowercase().as_str() {
            "high" => Some(wgpu::PowerPreference::HighPerformance),
            "low" => Some(wgpu::PowerPreference::LowPower),
            "none" => Some(wgpu::PowerPreference::None),
            _ => None,
        }))
    }
}
