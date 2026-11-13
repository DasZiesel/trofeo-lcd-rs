//! Hardware-free source configuration and role projection.
use crate::config::ConfigFile;
use std::{fmt, str::FromStr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Auto,
    Native,
    Lhm,
    Bridge,
    Hwinfo,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Mock,
    Real,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    CpuTemp,
    CpuFan,
    CaseFan,
    Pump,
    MbTemp,
    GpuTemp,
}
pub const ROLES: [Role; 6] = [
    Role::CpuTemp,
    Role::CpuFan,
    Role::CaseFan,
    Role::Pump,
    Role::MbTemp,
    Role::GpuTemp,
];
pub const KEYS: [&str; 6] = [
    "sensor_cpu_temp",
    "sensor_cpu_fan",
    "sensor_case_fan",
    "sensor_pump",
    "sensor_mb_temp",
    "sensor_gpu_temp",
];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    Celsius,
    Rpm,
}
impl Role {
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn unit(self) -> Unit {
        match self {
            Self::CpuTemp | Self::MbTemp | Self::GpuTemp => Unit::Celsius,
            _ => Unit::Rpm,
        }
    }
    fn parse(s: &str) -> Result<Self, String> {
        ROLES
            .into_iter()
            .find(|r| &KEYS[r.index()][7..] == s)
            .ok_or_else(|| "unknown sensor role alias".into())
    }
}

/// Canonical form: hwinfo:0xhhhhhhhh/0xhhhhhhhh/0xhhhhhhhh (lowercase).
/// Input accepts 1..=8 ASCII hex digits, with either 0x or 0X. No signs or spaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HwinfoSelector {
    pub sensor_id: u32,
    pub sensor_instance: u32,
    pub reading_id: u32,
}
impl FromStr for HwinfoSelector {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rest = s
            .strip_prefix("hwinfo:")
            .ok_or("expected hwinfo: selector")?;
        let mut parts = rest.split('/');
        let mut read = || -> Result<u32, String> {
            let part = parts.next().ok_or("HWiNFO selector requires three IDs")?;
            let digits = part
                .strip_prefix("0x")
                .or_else(|| part.strip_prefix("0X"))
                .ok_or("HWiNFO IDs require 0x prefix")?;
            if digits.is_empty()
                || digits.len() > 8
                || !digits.bytes().all(|c| c.is_ascii_hexdigit())
            {
                return Err("HWiNFO ID must contain 1..8 hexadecimal digits (u32)".into());
            }
            u32::from_str_radix(digits, 16).map_err(|_| "HWiNFO ID exceeds u32".into())
        };
        let value = Self {
            sensor_id: read()?,
            sensor_instance: read()?,
            reading_id: read()?,
        };
        if parts.next().is_some() {
            return Err("HWiNFO selector requires exactly three IDs".into());
        }
        Ok(value)
    }
}
impl fmt::Display for HwinfoSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "hwinfo:0x{:08x}/0x{:08x}/0x{:08x}",
            self.sensor_id, self.sensor_instance, self.reading_id
        )
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BridgeAlias {
    CpuPackage,
    AioPump,
    RadiatorFan,
    VrmFan,
    MainboardTemp,
}
impl BridgeAlias {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "cpu_package" => Ok(Self::CpuPackage),
            "aio_pump" => Ok(Self::AioPump),
            "radiator_fan" => Ok(Self::RadiatorFan),
            "vrm_fan" => Ok(Self::VrmFan),
            "mainboard_temp" => Ok(Self::MainboardTemp),
            _ => Err("unknown bridge alias".into()),
        }
    }
    fn unit(self) -> Unit {
        match self {
            Self::CpuPackage | Self::MainboardTemp => Unit::Celsius,
            _ => Unit::Rpm,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding {
    Legacy,
    Native(Role),
    Lhm(Role),
    Bridge(BridgeAlias),
    Hwinfo(HwinfoSelector),
    Unbound,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Unavailable,
    NotFound,
    Invalid,
    Unverified,
    Stale,
    Disabled,
    Closed,
}
/// Normalized role value. Identity, time and generation remain in provider snapshots.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub source: Source,
    pub role: Role,
    pub unit: Unit,
    pub value: Option<f64>,
    pub status: Status,
}
impl Sample {
    pub(crate) fn new(source: Source, role: Role, value: Option<f64>, status: Status) -> Self {
        let valid = value.filter(|v| {
            v.is_finite() && (role.unit() != Unit::Rpm || (*v >= 0.0 && *v <= u32::MAX as f64))
        });
        let status = if status == Status::Ok && valid.is_none() {
            Status::Invalid
        } else {
            status
        };
        Self {
            source,
            role,
            unit: role.unit(),
            value: if status == Status::Ok { valid } else { None },
            status,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub source: Source,
    pub mode: Option<Mode>,
    pub roles: [Binding; 6],
}

pub fn is_key(key: &str) -> bool {
    key == "sensor_source" || key == "bridge_mode" || key == "bridge_service" || KEYS.contains(&key)
}
pub fn cli_key(arg: &str) -> Option<String> {
    let key = arg.strip_prefix("--")?.replace('-', "_");
    is_key(&key).then_some(key)
}
/// Applies only explicitly supplied overrides, then validates the complete candidate.
pub fn configured(
    mut file: ConfigFile,
    overrides: &[(String, String)],
) -> Result<(ConfigFile, Settings), String> {
    for (key, value) in overrides {
        file.set(key, value);
    }
    let settings = Settings::parse(&file)?;
    Ok((file, settings))
}
pub fn validate_text(text: &str) -> Result<Settings, String> {
    Settings::parse(&ConfigFile::parse(text)?)
}

impl Settings {
    pub fn parse(f: &ConfigFile) -> Result<Self, String> {
        let source = match f.get("sensor_source").unwrap_or("auto") {
            "auto" | "existing" => Source::Auto,
            "native" => Source::Native,
            "lhm" => Source::Lhm,
            "bridge" => Source::Bridge,
            "hwinfo" => Source::Hwinfo,
            _ => return Err("unknown sensor_source".into()),
        };
        let mode = match f.get("bridge_mode") {
            None => None,
            Some("mock") => Some(Mode::Mock),
            Some("real") => Some(Mode::Real),
            _ => return Err("invalid bridge_mode".into()),
        };
        if f.get("bridge_service").unwrap_or("TrofeoSensorService") != "TrofeoSensorService" {
            return Err("unsupported bridge_service".into());
        }
        let mut roles = ROLES.map(|role| match source {
            Source::Auto => Binding::Legacy,
            Source::Native => Binding::Native(role),
            Source::Lhm if role != Role::GpuTemp => Binding::Lhm(role),
            Source::Bridge if role != Role::GpuTemp => Binding::Bridge(
                [
                    BridgeAlias::CpuPackage,
                    BridgeAlias::RadiatorFan,
                    BridgeAlias::VrmFan,
                    BridgeAlias::AioPump,
                    BridgeAlias::MainboardTemp,
                ][role.index()],
            ),
            Source::Hwinfo if role != Role::GpuTemp => Binding::Unbound,
            _ => Binding::Native(role),
        });
        for (i, key) in KEYS.iter().enumerate() {
            if let Some(v) = f.get(key) {
                let (s, alias) = v.split_once(':').ok_or("role requires source:selector")?;
                roles[i] = match s {
                    "hwinfo" => Binding::Hwinfo(v.parse().map_err(|e| format!("{key}: {e}"))?),
                    "bridge" => {
                        let a = BridgeAlias::parse(alias)?;
                        if a.unit() != ROLES[i].unit() {
                            return Err("role unit mismatch".into());
                        }
                        Binding::Bridge(a)
                    }
                    "native" | "lhm" => {
                        let role = Role::parse(alias)?;
                        if role.unit() != ROLES[i].unit() {
                            return Err("role unit mismatch".into());
                        }
                        if s == "native" {
                            if !matches!(role, Role::CpuTemp | Role::GpuTemp) {
                                return Err("unsupported native role".into());
                            }
                            Binding::Native(role)
                        } else {
                            if role == Role::GpuTemp {
                                return Err("LHM gpu_temp role unavailable".into());
                            }
                            Binding::Lhm(role)
                        }
                    }
                    _ => return Err("unknown role source".into()),
                };
            }
        }
        let value = Self {
            source,
            mode,
            roles,
        };
        if value.bridge() && mode.is_none() {
            return Err("bridge_mode must be explicit".into());
        }
        if roles.iter().any(|b| matches!(b, Binding::Lhm(_))) && !f.get_bool("lhm")?.unwrap_or(true)
        {
            return Err("LHM role requires lhm=true".into());
        }
        Ok(value)
    }
    pub fn bridge(&self) -> bool {
        self.roles.iter().any(|b| matches!(b, Binding::Bridge(_)))
    }
    pub fn native_cpu(&self) -> bool {
        self.roles
            .iter()
            .any(|b| matches!(b, Binding::Legacy | Binding::Native(Role::CpuTemp)))
    }
    pub fn lhm(&self) -> bool {
        self.roles
            .iter()
            .any(|b| matches!(b, Binding::Legacy | Binding::Lhm(_)))
    }
    pub fn legacy(&self) -> bool {
        self.roles.iter().all(|b| *b == Binding::Legacy)
    }
    pub fn hwinfo(&self) -> bool {
        self.source == Source::Hwinfo || self.roles.iter().any(|b| matches!(b, Binding::Hwinfo(_)))
    }
    /// Explicit HWiNFO bindings only; all other roles keep their existing policy.
    /// A missing/contended worker view is unavailable, never a fallback source.
    pub fn resolve_with_hwinfo(
        &self,
        native: [Option<f64>; 6],
        lhm: [Option<f64>; 6],
        legacy: [Option<f64>; 6],
        view: Option<&crate::hwinfo_provider::View>,
    ) -> [Sample; 6] {
        let mut samples = self.resolve(native, lhm, legacy);
        for (i, binding) in self.roles.iter().enumerate() {
            if let Binding::Hwinfo(id) = binding {
                samples[i] = view
                    .map(|v| v.resolve(ROLES[i], *id).sample)
                    .unwrap_or_else(|| {
                        Sample::new(Source::Hwinfo, ROLES[i], None, Status::Unavailable)
                    });
            }
        }
        samples
    }
    pub fn ensure_available(&self) -> Result<(), String> {
        if self.hwinfo() && !cfg!(all(windows, target_pointer_width = "64")) {
            return Err("sensor provider hwinfo is unavailable on this platform".into());
        }
        if self.source == Source::Bridge || self.bridge() {
            return Err("sensor provider bridge is unavailable in this build".into());
        }
        Ok(())
    }
    /// No hot role switching until provider-aware histories are integrated.
    pub fn validate_reload(&self, next: &Self) -> Result<(), String> {
        next.ensure_available()?;
        if self != next {
            return Err("sensor source or bindings changed; restart required (keeping previous configuration)".into());
        }
        Ok(())
    }
    /// Pure role reduction; callers supply already fresh native/LHM values.
    pub fn resolve(
        &self,
        native: [Option<f64>; 6],
        lhm: [Option<f64>; 6],
        legacy: [Option<f64>; 6],
    ) -> [Sample; 6] {
        std::array::from_fn(|i| {
            let (source, value, missing) = match self.roles[i] {
                Binding::Legacy => (Source::Auto, legacy[i], Status::NotFound),
                Binding::Native(r) => (Source::Native, native[r.index()], Status::NotFound),
                Binding::Lhm(r) => (Source::Lhm, lhm[r.index()], Status::NotFound),
                Binding::Bridge(_) => (Source::Bridge, None, Status::Unavailable),
                Binding::Hwinfo(_) => (Source::Hwinfo, None, Status::Unavailable),
                Binding::Unbound => (self.source, None, Status::NotFound),
            };
            Sample::new(
                source,
                ROLES[i],
                value,
                if value.is_some() { Status::Ok } else { missing },
            )
        })
    }
}
