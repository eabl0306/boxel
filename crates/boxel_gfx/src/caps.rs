use serde::{Deserialize, Serialize};

/// Device class derived from `wgpu::DeviceType`. `Software` = CPU rasterizers (lavapipe,
/// llvmpipe, WARP): never auto-selected while a hardware adapter works.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceClass {
    /// Discrete GPU.
    Discrete,
    /// Integrated GPU.
    Integrated,
    /// Virtual GPU (VMs).
    Virtual,
    /// CPU rasterizer.
    Software,
    /// Unknown (all GL adapters report this).
    Other,
}

impl From<wgpu::DeviceType> for DeviceClass {
    fn from(t: wgpu::DeviceType) -> Self {
        match t {
            wgpu::DeviceType::DiscreteGpu => DeviceClass::Discrete,
            wgpu::DeviceType::IntegratedGpu => DeviceClass::Integrated,
            wgpu::DeviceType::VirtualGpu => DeviceClass::Virtual,
            wgpu::DeviceType::Cpu => DeviceClass::Software,
            wgpu::DeviceType::Other => DeviceClass::Other,
        }
    }
}

/// OpenGL context version parsed from `AdapterInfo::driver_info` on `Backend::Gl`
/// (`"3.3.0 NVIDIA 595.91.07"`, `"4.6 (Core Profile) Mesa 26.0.8"`, `"OpenGL ES 3.2 Mesa ..."`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GlVersion {
    /// Major.
    pub major: u8,
    /// Minor.
    pub minor: u8,
    /// OpenGL ES context.
    pub es: bool,
}

impl GlVersion {
    /// Parses a `driver_info` string; `None` if no `major.minor` is found.
    pub fn parse(driver_info: &str) -> Option<GlVersion> {
        let s = driver_info.trim();
        let (es, rest) = match s.strip_prefix("OpenGL ES ") {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        let token = rest.split_whitespace().next()?;
        let mut parts = token.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        Some(GlVersion { major, minor, es })
    }

    /// True if storage buffers in the vertex stage and compute are really available
    /// (desktop >= 4.3, ES >= 3.1). naga cannot emit SSBOs/compute below that.
    pub fn supports_storage_and_compute(self) -> bool {
        let floor = if self.es { (3, 1) } else { (4, 3) };
        (self.major, self.minor) >= floor
    }
}

/// Stable, serializable identity of an adapter (logs, LKG file, exclusions).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AdapterSummary {
    /// `AdapterInfo::name`.
    pub name: String,
    /// `"Vulkan" | "Gl" | "Dx12" | "Metal"` (Debug of `wgpu::Backend`).
    pub backend: String,
    /// Device class.
    pub class: DeviceClass,
    /// PCI vendor id (0 if unknown).
    pub vendor: u32,
    /// PCI device id (0 if unknown).
    pub device: u32,
    /// `AdapterInfo::driver`.
    pub driver: String,
    /// `AdapterInfo::driver_info`.
    pub driver_info: String,
}

impl AdapterSummary {
    /// From wgpu's adapter info.
    pub fn from_info(info: &wgpu::AdapterInfo) -> Self {
        Self {
            name: info.name.clone(),
            backend: format!("{:?}", info.backend),
            class: info.device_type.into(),
            vendor: info.vendor,
            device: info.device,
            driver: info.driver.clone(),
            driver_info: info.driver_info.clone(),
        }
    }

    /// `"{backend}:{vendor:04x}:{device:04x}:{name}:{driver_info}"`: changes when the driver changes.
    pub fn key(&self) -> String {
        format!(
            "{}:{:04x}:{:04x}:{}:{}",
            self.backend, self.vendor, self.device, self.name, self.driver_info
        )
    }
}

/// Snapshot of everything classification needs from one adapter.
#[derive(Clone, Debug)]
pub struct AdapterCaps {
    /// Raw adapter info.
    pub info: wgpu::AdapterInfo,
    /// Adapter features.
    pub features: wgpu::Features,
    /// Adapter limits.
    pub limits: wgpu::Limits,
    /// Downlevel capabilities.
    pub downlevel: wgpu::DownlevelCapabilities,
    /// Parsed GL version (`Backend::Gl` only).
    pub gl_version: Option<GlVersion>,
}

impl AdapterCaps {
    /// Reads all capabilities from `adapter`.
    pub fn from_adapter(adapter: &wgpu::Adapter) -> Self {
        let info = adapter.get_info();
        let gl_version = (info.backend == wgpu::Backend::Gl)
            .then(|| GlVersion::parse(&info.driver_info))
            .flatten();
        Self {
            features: adapter.features(),
            limits: adapter.limits(),
            downlevel: adapter.get_downlevel_capabilities(),
            gl_version,
            info,
        }
    }

    /// Device class.
    pub fn class(&self) -> DeviceClass {
        self.info.device_type.into()
    }

    /// Identity.
    pub fn summary(&self) -> AdapterSummary {
        AdapterSummary::from_info(&self.info)
    }
}
