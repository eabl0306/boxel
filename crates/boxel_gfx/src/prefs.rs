use crate::{Tier, one_line};
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
    ///
    /// Accepts `--flag value` and `--flag=value`. Value flags followed by nothing (or by another
    /// `--flag`) give [`PrefsError::MissingValue`]; malformed values give
    /// [`PrefsError::BadValue`]. Switches (`--software`, `--validation`, `--lkg`, `--no-lkg`)
    /// take no value (`--software=0|1|true|false` is also accepted). Everything else, `--help`
    /// included, is returned untouched.
    pub fn from_args(
        args: &[String],
    ) -> Result<(GpuPreferences, Option<std::path::PathBuf>, Vec<String>), PrefsError> {
        let mut prefs = GpuPreferences::default();
        let mut config = None;
        let mut rest = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            i += 1;
            let (flag, inline) = match arg.split_once('=') {
                Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
                _ => (arg.as_str(), None),
            };
            let takes_value = matches!(
                flag,
                "--backend" | "--adapter" | "--power" | "--tier" | "--present" | "--gpu-config"
            );
            let is_switch = matches!(flag, "--software" | "--validation" | "--lkg" | "--no-lkg");
            if !takes_value && !is_switch {
                rest.push(arg.clone());
                continue;
            }
            if is_switch {
                let on = match inline.as_deref() {
                    None => true,
                    Some(v) => {
                        parse_bool(v).ok_or_else(|| bad(flag, v, "nothing, 1|0 or true|false"))?
                    }
                };
                match flag {
                    "--software" => prefs.software = Some(on),
                    "--validation" => prefs.validation = Some(on),
                    "--lkg" => prefs.use_lkg = Some(on),
                    _ => prefs.use_lkg = Some(!on),
                }
                continue;
            }
            let value = match inline {
                Some(v) => v,
                None => match args.get(i) {
                    Some(v) if !v.starts_with("--") => {
                        i += 1;
                        v.clone()
                    }
                    _ => return Err(PrefsError::MissingValue(flag.to_string())),
                },
            };
            if value.is_empty() {
                return Err(PrefsError::MissingValue(flag.to_string()));
            }
            match flag {
                "--backend" => prefs.backends = Some(parse_backends(flag, &value)?),
                "--adapter" => prefs.adapter = Some(value),
                "--power" => prefs.power = Some(parse_power(flag, &value)?),
                "--tier" => prefs.max_tier = Some(parse_tier(flag, &value)?),
                "--present" => prefs.present_mode = Some(parse_present(flag, &value)?),
                _ => config = Some(std::path::PathBuf::from(value)),
            }
        }
        Ok((prefs, config, rest))
    }

    /// Reads `BOXEL_BACKEND`, `BOXEL_ADAPTER`, `BOXEL_POWER`, `BOXEL_TIER`, `BOXEL_SOFTWARE`,
    /// `BOXEL_GPU_VALIDATION`, `BOXEL_NO_LKG`, `BOXEL_PRESENT`. Invalid values are skipped and
    /// reported in the returned warnings (never fatal).
    /// `BOXEL_LKG=1|0` is also read (it wins over its alias `BOXEL_NO_LKG=1`).
    pub fn from_boxel_env() -> (GpuPreferences, Vec<String>) {
        boxel_env_with(&env_lookup)
    }

    /// Reads `WGPU_BACKEND` (via `Backends::from_comma_list`; empty result ignored with a
    /// warning), `WGPU_ADAPTER_NAME`, `WGPU_POWER_PREF` (`high|low|none`).
    pub fn from_wgpu_env() -> (GpuPreferences, Vec<String>) {
        wgpu_env_with(&env_lookup)
    }

    /// Reads the `[gpu]` table of a TOML file (same field names as this struct;
    /// `backends = "vulkan,gl"`, `power = "high"`, `max_tier = "mid"`).
    /// A file without a `[gpu]` table gives default preferences; other tables are ignored, but
    /// unknown keys and invalid values inside `[gpu]` are errors.
    pub fn from_config_file(path: &Path) -> Result<GpuPreferences, PrefsError> {
        let text = std::fs::read_to_string(path).map_err(|source| PrefsError::Io {
            path: path.display().to_string(),
            source,
        })?;
        parse_config(&text).map_err(|message| PrefsError::Parse {
            path: path.display().to_string(),
            message,
        })
    }

    /// Full resolution: `cli` > BOXEL env > WGPU env > config file (`config_path`, or
    /// `BOXEL_GPU_CONFIG` if `None`; a missing file is fine, a broken one is a warning).
    /// Returns the merged preferences and all warnings.
    pub fn resolve(
        cli: GpuPreferences,
        config_path: Option<&Path>,
    ) -> (GpuPreferences, Vec<String>) {
        resolve_with(cli, config_path, &env_lookup)
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

/// Process environment lookup.
fn env_lookup(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

/// Environment variable lookup injected by tests (never mutate the process environment).
type Lookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// Non-empty, trimmed value of `key`.
fn lookup_value(lookup: Lookup<'_>, key: &str) -> Option<String> {
    lookup(key)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// [`GpuPreferences::from_boxel_env`] with an injected lookup.
fn boxel_env_with(lookup: Lookup<'_>) -> (GpuPreferences, Vec<String>) {
    let mut prefs = GpuPreferences::default();
    let mut warnings = Vec::new();
    let read = |key: &str| lookup_value(lookup, key);
    if let Some(v) = read("BOXEL_BACKEND") {
        prefs.backends = keep(parse_backends("BOXEL_BACKEND", &v), &mut warnings);
    }
    prefs.adapter = read("BOXEL_ADAPTER");
    if let Some(v) = read("BOXEL_POWER") {
        prefs.power = keep(parse_power("BOXEL_POWER", &v), &mut warnings);
    }
    if let Some(v) = read("BOXEL_TIER") {
        prefs.max_tier = keep(parse_tier("BOXEL_TIER", &v), &mut warnings);
    }
    if let Some(v) = read("BOXEL_SOFTWARE") {
        prefs.software = keep(parse_switch("BOXEL_SOFTWARE", &v), &mut warnings);
    }
    if let Some(v) = read("BOXEL_GPU_VALIDATION") {
        prefs.validation = keep(parse_switch("BOXEL_GPU_VALIDATION", &v), &mut warnings);
    }
    let no_lkg = read("BOXEL_NO_LKG")
        .and_then(|v| keep(parse_switch("BOXEL_NO_LKG", &v), &mut warnings))
        .map(|no| !no);
    let lkg = read("BOXEL_LKG").and_then(|v| keep(parse_switch("BOXEL_LKG", &v), &mut warnings));
    if let (Some(a), Some(b)) = (lkg, no_lkg)
        && a != b
    {
        warnings.push(format!(
            "BOXEL_LKG and BOXEL_NO_LKG disagree; using BOXEL_LKG={}",
            u8::from(a)
        ));
    }
    prefs.use_lkg = lkg.or(no_lkg);
    if let Some(v) = read("BOXEL_PRESENT") {
        prefs.present_mode = keep(parse_present("BOXEL_PRESENT", &v), &mut warnings);
    }
    (prefs, warnings)
}

/// `Ok` value, or `None` plus a warning (environment values are never fatal).
fn keep<T>(result: Result<T, PrefsError>, warnings: &mut Vec<String>) -> Option<T> {
    match result {
        Ok(v) => Some(v),
        Err(e) => {
            warnings.push(format!("ignored: {e}"));
            None
        }
    }
}

/// [`GpuPreferences::from_wgpu_env`] with an injected lookup.
fn wgpu_env_with(lookup: Lookup<'_>) -> (GpuPreferences, Vec<String>) {
    let mut prefs = GpuPreferences::default();
    let mut warnings = Vec::new();
    if let Some(v) = lookup_value(lookup, "WGPU_BACKEND") {
        let backends = wgpu::Backends::from_comma_list(&v);
        if backends.is_empty() {
            warnings.push(format!(
                "ignored: WGPU_BACKEND={v:?} names no known backend (expected vulkan,gl,dx12,metal)"
            ));
        } else {
            prefs.backends = Some(backends);
        }
    }
    prefs.adapter = lookup_value(lookup, "WGPU_ADAPTER_NAME");
    if let Some(v) = lookup_value(lookup, "WGPU_POWER_PREF") {
        prefs.power = keep(parse_power("WGPU_POWER_PREF", &v), &mut warnings);
    }
    (prefs, warnings)
}

/// [`GpuPreferences::resolve`] with an injected lookup.
fn resolve_with(
    cli: GpuPreferences,
    config_path: Option<&Path>,
    lookup: Lookup<'_>,
) -> (GpuPreferences, Vec<String>) {
    let (boxel, mut warnings) = boxel_env_with(lookup);
    let (wgpu_env, wgpu_warnings) = wgpu_env_with(lookup);
    warnings.extend(wgpu_warnings);
    let path = config_path
        .map(Path::to_path_buf)
        .or_else(|| lookup_value(lookup, "BOXEL_GPU_CONFIG").map(std::path::PathBuf::from));
    let file = match path {
        Some(path) if path.exists() => match GpuPreferences::from_config_file(&path) {
            Ok(prefs) => prefs,
            Err(e) => {
                warnings.push(format!("ignored: {e}"));
                GpuPreferences::default()
            }
        },
        Some(path) => {
            tracing::debug!("GPU config {} not found; ignored", path.display());
            GpuPreferences::default()
        }
        None => GpuPreferences::default(),
    };
    (cli.overlay(boxel).overlay(wgpu_env).overlay(file), warnings)
}

/// Parses the text of a GPU config file (see [`GpuPreferences::from_config_file`]).
fn parse_config(text: &str) -> Result<GpuPreferences, String> {
    let mut table: toml::Table = toml::from_str(text).map_err(|e| one_line(&e.to_string()))?;
    let Some(gpu) = table.remove("gpu") else {
        return Ok(GpuPreferences::default());
    };
    let toml::Value::Table(mut gpu) = gpu else {
        return Err("`gpu` must be a table".to_string());
    };
    // The serde helpers drop unknown values silently; a config file deserves a clear error. The
    // strict parsers' results are used as is, so what is validated is exactly what is applied
    // (same rules as the CLI and env vars, surrounding spaces included).
    let backends = gpu
        .remove("backends")
        .map(|v| {
            let s = v.as_str().ok_or("`backends` must be a string")?;
            parse_backends("backends", s).map_err(|e| e.to_string())
        })
        .transpose()?;
    let power = gpu
        .remove("power")
        .map(|v| {
            let s = v.as_str().ok_or("`power` must be a string")?;
            parse_power("power", s).map_err(|e| e.to_string())
        })
        .transpose()?;
    let mut prefs = toml::Value::Table(gpu)
        .try_into::<GpuPreferences>()
        .map_err(|e| one_line(&e.to_string()))?;
    prefs.backends = backends;
    prefs.power = power;
    Ok(prefs)
}

fn bad(key: &str, value: &str, expected: &'static str) -> PrefsError {
    PrefsError::BadValue {
        key: key.to_string(),
        value: value.to_string(),
        expected,
    }
}

/// Strict comma list of backend names (unknown names are an error, unlike
/// `Backends::from_comma_list`).
fn parse_backends(key: &str, value: &str) -> Result<wgpu::Backends, PrefsError> {
    const EXPECTED: &str = "a comma list of vulkan,gl,dx12,metal";
    let mut backends = wgpu::Backends::empty();
    for name in value.split(',').map(str::trim).filter(|n| !n.is_empty()) {
        backends |= match name.to_ascii_lowercase().as_str() {
            "vulkan" | "vk" => wgpu::Backends::VULKAN,
            "gl" | "gles" | "opengl" => wgpu::Backends::GL,
            "dx12" | "d3d12" => wgpu::Backends::DX12,
            "metal" | "mtl" => wgpu::Backends::METAL,
            _ => return Err(bad(key, value, EXPECTED)),
        };
    }
    if backends.is_empty() {
        return Err(bad(key, value, EXPECTED));
    }
    Ok(backends)
}

fn parse_power(key: &str, value: &str) -> Result<wgpu::PowerPreference, PrefsError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "high" => Ok(wgpu::PowerPreference::HighPerformance),
        "low" => Ok(wgpu::PowerPreference::LowPower),
        "none" => Ok(wgpu::PowerPreference::None),
        _ => Err(bad(key, value, "high|low|none")),
    }
}

fn parse_tier(key: &str, value: &str) -> Result<Tier, PrefsError> {
    Tier::parse(value).ok_or_else(|| bad(key, value, "high|mid|low"))
}

fn parse_present(key: &str, value: &str) -> Result<PresentModePref, PrefsError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "vsync" => Ok(PresentModePref::Vsync),
        "mailbox" => Ok(PresentModePref::Mailbox),
        "immediate" => Ok(PresentModePref::Immediate),
        _ => Err(bad(key, value, PRESENT_MODES)),
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// `1|0` switch from an environment variable.
fn parse_switch(key: &str, value: &str) -> Result<bool, PrefsError> {
    parse_bool(value).ok_or_else(|| bad(key, value, "1|0"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn boxel_env_reads_every_variable() {
        let lookup = env(&[
            ("BOXEL_BACKEND", "gl"),
            ("BOXEL_ADAPTER", "radeon"),
            ("BOXEL_POWER", "low"),
            ("BOXEL_TIER", "Mid"),
            ("BOXEL_SOFTWARE", "1"),
            ("BOXEL_GPU_VALIDATION", "0"),
            ("BOXEL_NO_LKG", "1"),
            ("BOXEL_PRESENT", "immediate"),
        ]);
        let (p, warnings) = boxel_env_with(&lookup);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(p.backends, Some(wgpu::Backends::GL));
        assert_eq!(p.adapter.as_deref(), Some("radeon"));
        assert_eq!(p.power, Some(wgpu::PowerPreference::LowPower));
        assert_eq!(p.max_tier, Some(Tier::Mid));
        assert_eq!(p.software, Some(true));
        assert_eq!(p.validation, Some(false));
        assert_eq!(p.use_lkg, Some(false));
        assert_eq!(p.present_mode, Some(PresentModePref::Immediate));
    }

    #[test]
    fn boxel_env_invalid_values_are_warnings() {
        let lookup = env(&[
            ("BOXEL_BACKEND", "glide"),
            ("BOXEL_TIER", "ultra"),
            ("BOXEL_SOFTWARE", "maybe"),
            ("BOXEL_ADAPTER", "   "),
        ]);
        let (p, warnings) = boxel_env_with(&lookup);
        assert_eq!(p, GpuPreferences::default());
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(warnings.iter().all(|w| w.starts_with("ignored: ")));
    }

    #[test]
    fn boxel_lkg_wins_over_its_alias() {
        let (p, w) = boxel_env_with(&env(&[("BOXEL_LKG", "1")]));
        assert_eq!((p.use_lkg, w.len()), (Some(true), 0));
        let (p, w) = boxel_env_with(&env(&[("BOXEL_LKG", "0")]));
        assert_eq!((p.use_lkg, w.len()), (Some(false), 0));
        let (p, w) = boxel_env_with(&env(&[("BOXEL_LKG", "1"), ("BOXEL_NO_LKG", "1")]));
        assert_eq!((p.use_lkg, w.len()), (Some(true), 1));
    }

    #[test]
    fn wgpu_env_reads_backend_adapter_and_power() {
        let lookup = env(&[
            ("WGPU_BACKEND", "vulkan"),
            ("WGPU_ADAPTER_NAME", "llvmpipe"),
            ("WGPU_POWER_PREF", "high"),
        ]);
        let (p, warnings) = wgpu_env_with(&lookup);
        assert!(warnings.is_empty());
        assert_eq!(p.backends, Some(wgpu::Backends::VULKAN));
        assert_eq!(p.adapter.as_deref(), Some("llvmpipe"));
        assert_eq!(p.power, Some(wgpu::PowerPreference::HighPerformance));
        // An empty parse result is ignored with a warning (never "no backends at all").
        let (p, warnings) = wgpu_env_with(&env(&[("WGPU_BACKEND", "nothing")]));
        assert_eq!((p.backends, warnings.len()), (None, 1));
        let (p, warnings) = wgpu_env_with(&env(&[("WGPU_BACKEND", "")]));
        assert_eq!((p.backends, warnings.len()), (None, 0));
    }

    #[test]
    fn resolve_precedence_cli_boxel_wgpu_file() {
        let dir = std::env::temp_dir().join(format!("boxel_gfx_prefs_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("gpu.toml");
        std::fs::write(
            &file,
            "[gpu]\nbackends = \"vulkan,gl\"\nadapter = \"file\"\npower = \"low\"\n\
             max_tier = \"low\"\nsoftware = false\npresent_mode = \"mailbox\"\n",
        )
        .unwrap();
        let lookup = env(&[
            ("BOXEL_ADAPTER", "boxel"),
            ("BOXEL_TIER", "mid"),
            ("WGPU_ADAPTER_NAME", "wgpu"),
            ("WGPU_POWER_PREF", "high"),
            ("WGPU_BACKEND", "gl"),
            ("BOXEL_GPU_CONFIG", file.to_str().unwrap()),
        ]);
        let cli = GpuPreferences {
            max_tier: Some(Tier::High),
            ..Default::default()
        };
        let (p, warnings) = resolve_with(cli, None, &lookup);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(p.max_tier, Some(Tier::High), "CLI wins");
        assert_eq!(p.adapter.as_deref(), Some("boxel"), "BOXEL_* beats WGPU_*");
        assert_eq!(
            p.power,
            Some(wgpu::PowerPreference::HighPerformance),
            "WGPU_* beats file"
        );
        assert_eq!(p.backends, Some(wgpu::Backends::GL), "WGPU_* beats file");
        assert_eq!(p.software, Some(false), "file fills the gaps");
        assert_eq!(p.present_mode, Some(PresentModePref::Mailbox));
        assert_eq!(p.validation, None, "automatic");

        // An explicit path beats BOXEL_GPU_CONFIG; a missing file is fine, a broken one warns.
        let missing = dir.join("missing.toml");
        let (p, warnings) = resolve_with(GpuPreferences::default(), Some(&missing), &lookup);
        assert!(warnings.is_empty());
        assert_eq!(p.software, None);
        let broken = dir.join("broken.toml");
        std::fs::write(&broken, "[gpu]\npower = \"turbo\"\n").unwrap();
        let (p, warnings) = resolve_with(GpuPreferences::default(), Some(&broken), &env(&[]));
        assert_eq!(p, GpuPreferences::default());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn config_text_parsing() {
        let p = parse_config("[gpu]\nbackends = \"Vulkan, GL\"\nvalidation = true\n").unwrap();
        assert_eq!(
            p.backends,
            Some(wgpu::Backends::VULKAN | wgpu::Backends::GL)
        );
        assert_eq!(p.validation, Some(true));
        assert_eq!(
            parse_config("[window]\nwidth = 3\n").unwrap(),
            GpuPreferences::default()
        );
        assert!(
            parse_config("[gpu]\nbogus = 1\n").is_err(),
            "unknown keys are errors"
        );
        assert!(parse_config("[gpu]\nbackends = \"glide\"\n").is_err());
        // Values that pass validation are never dropped afterwards.
        let p = parse_config("[gpu]\npower = \" Low \"\nbackends = \" gl \"\n").unwrap();
        assert_eq!(p.power, Some(wgpu::PowerPreference::LowPower));
        assert_eq!(p.backends, Some(wgpu::Backends::GL));
        assert_eq!(
            parse_config("[gpu]\npower = \"none\"\n").unwrap().power,
            Some(wgpu::PowerPreference::None)
        );
        assert!(parse_config("[gpu]\npower = \"turbo\"\n").is_err());
        assert!(parse_config("[gpu]\npower = 3\n").is_err());
        assert!(parse_config("[gpu]\nmax_tier = \"ultra\"\n").is_err());
        assert!(parse_config("gpu = 3\n").is_err());
        assert!(parse_config("not toml").is_err());
    }
}
