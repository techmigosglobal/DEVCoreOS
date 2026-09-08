#![forbid(unsafe_code)]
//! Bounded, read-only Linux hardware inventory for DevCore.
//!
//! The inventory intentionally reads kernel-exported files and directory
//! entries only. It does not probe devices by executing commands and it does
//! not expose a mutation API. Hardware-specific policy remains a separate
//! privileged boundary.

use std::{error::Error, fmt, fs, io, path::PathBuf, thread};

const CPUINFO_PATH: &str = "/proc/cpuinfo";
const MEMINFO_PATH: &str = "/proc/meminfo";
const POWER_SUPPLY_PATH: &str = "/sys/class/power_supply";
const NETWORK_PATH: &str = "/sys/class/net";
const DRM_PATH: &str = "/sys/class/drm";
const MAX_NETWORK_INTERFACES: usize = 64;
const MAX_GPU_DEVICES: u16 = 64;

/// A bounded read-only inventory of hardware facts needed by DevCore UI and
/// policy decisions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HardwareSnapshot {
    /// Kernel-reported CPU model, or `unknown` when unavailable.
    pub cpu_model: String,
    /// Logical CPUs visible to this process.
    pub logical_cpus: u16,
    /// Total memory reported by `/proc/meminfo`, in KiB.
    pub memory_total_kib: u64,
    /// Whether the kernel exposes at least one battery.
    pub battery_present: bool,
    /// First valid battery charge percentage, when available.
    pub battery_percent: Option<u8>,
    /// First valid AC or USB power-online state, when available.
    pub ac_online: Option<bool>,
    /// Names of visible network interfaces, sorted and bounded.
    pub network_interfaces: Vec<String>,
    /// Number of primary DRM card devices, bounded to avoid malformed sysfs
    /// entries causing an integer overflow.
    pub gpu_devices: u16,
}

/// Reads one hardware inventory from Linux kernel interfaces.
pub fn collect_snapshot() -> Result<HardwareSnapshot, HardwareError> {
    let cpu_info = read_required(CPUINFO_PATH)?;
    let memory_info = read_required(MEMINFO_PATH)?;
    let memory_total_kib = parse_memory_total(&memory_info)?;
    let power = read_power_state();

    Ok(HardwareSnapshot {
        cpu_model: parse_cpu_model(&cpu_info).unwrap_or_else(|| "unknown".to_owned()),
        logical_cpus: logical_cpu_count(),
        memory_total_kib,
        battery_present: power.battery_present,
        battery_percent: power.battery_percent,
        ac_online: power.ac_online,
        network_interfaces: read_network_interfaces(),
        gpu_devices: read_gpu_devices(),
    })
}

/// Formats an inventory as a compact JSON document without a third-party
/// serializer in the low-level hardware crate.
#[must_use]
pub fn snapshot_as_json(snapshot: &HardwareSnapshot) -> String {
    let interfaces = snapshot
        .network_interfaces
        .iter()
        .map(|interface| format!("\"{}\"", json_escape(interface)))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        concat!(
            "{{\"cpu_model\":\"{}\",\"logical_cpus\":{},",
            "\"memory_total_kib\":{},\"battery_present\":{},",
            "\"battery_percent\":{},\"ac_online\":{},",
            "\"network_interfaces\":[{}],\"gpu_devices\":{}}}"
        ),
        json_escape(&snapshot.cpu_model),
        snapshot.logical_cpus,
        snapshot.memory_total_kib,
        snapshot.battery_present,
        optional_u8_json(snapshot.battery_percent),
        optional_bool_json(snapshot.ac_online),
        interfaces,
        snapshot.gpu_devices,
    )
}

/// A failure while reading or interpreting required hardware information.
#[derive(Debug)]
pub enum HardwareError {
    /// A required kernel file could not be read.
    Read {
        /// Path that failed to read.
        path: PathBuf,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// A required kernel file did not contain the expected value.
    Malformed {
        /// Source name being parsed.
        source: &'static str,
        /// Explanation of the malformed value.
        detail: String,
    },
}

impl fmt::Display for HardwareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(formatter, "could not read {}: {source}", path.display())
            }
            Self::Malformed { source, detail } => {
                write!(formatter, "could not parse {source}: {detail}")
            }
        }
    }
}

impl Error for HardwareError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Malformed { .. } => None,
        }
    }
}

fn read_required(path: &str) -> Result<String, HardwareError> {
    fs::read_to_string(path).map_err(|source| HardwareError::Read {
        path: PathBuf::from(path),
        source,
    })
}

fn logical_cpu_count() -> u16 {
    thread::available_parallelism()
        .ok()
        .and_then(|count| u16::try_from(count.get()).ok())
        .filter(|count| *count > 0)
        .unwrap_or(1)
}

fn parse_cpu_model(contents: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        if key.trim() != "model name" && key.trim() != "Hardware" {
            return None;
        }
        let value = value.trim();
        (!value.is_empty()).then(|| value.to_owned())
    })
}

fn parse_memory_total(contents: &str) -> Result<u64, HardwareError> {
    contents
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            if key.trim() != "MemTotal" {
                return None;
            }
            value.split_whitespace().next()?.parse().ok()
        })
        .ok_or_else(|| HardwareError::Malformed {
            source: MEMINFO_PATH,
            detail: "MemTotal is missing or invalid".to_owned(),
        })
}

#[derive(Clone, Copy, Debug, Default)]
struct PowerState {
    battery_present: bool,
    battery_percent: Option<u8>,
    ac_online: Option<bool>,
}

fn read_power_state() -> PowerState {
    let Ok(entries) = fs::read_dir(POWER_SUPPLY_PATH) else {
        return PowerState::default();
    };

    let mut state = PowerState::default();
    for entry in entries.flatten() {
        let path = entry.path();
        match read_trimmed(path.join("type")).as_deref() {
            Some("Battery") => {
                state.battery_present = true;
                state.battery_percent = state.battery_percent.or_else(|| {
                    read_trimmed(path.join("capacity")).and_then(|value| value.parse().ok())
                });
            }
            Some("Mains") | Some("USB") | Some("USB_C") | Some("Wireless") => {
                state.ac_online = state.ac_online.or_else(|| {
                    read_trimmed(path.join("online")).and_then(|value| parse_online(&value))
                });
            }
            _ => {}
        }
    }
    state
}

fn read_network_interfaces() -> Vec<String> {
    let Ok(entries) = fs::read_dir(NETWORK_PATH) else {
        return Vec::new();
    };
    let mut interfaces = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .take(MAX_NETWORK_INTERFACES)
        .collect::<Vec<_>>();
    interfaces.sort_unstable();
    interfaces.dedup();
    interfaces
}

fn read_gpu_devices() -> u16 {
    let Ok(entries) = fs::read_dir(DRM_PATH) else {
        return 0;
    };
    let count = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            let Some(number) = name.strip_prefix("card") else {
                return false;
            };
            !number.is_empty() && number.chars().all(|character| character.is_ascii_digit())
        })
        .count();
    u16::try_from(count.min(usize::from(MAX_GPU_DEVICES))).unwrap_or(MAX_GPU_DEVICES)
}

fn read_trimmed(path: PathBuf) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn parse_online(value: &str) -> Option<bool> {
    match value {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character.is_control() => {
                use std::fmt::Write;

                let _ = write!(escaped, "\\u{:04x}", character as u32);
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn optional_u8_json(value: Option<u8>) -> String {
    value.map_or_else(|| "null".to_owned(), |value| value.to_string())
}

fn optional_bool_json(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "null",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HardwareSnapshot, json_escape, parse_cpu_model, parse_memory_total, snapshot_as_json,
    };

    #[test]
    fn parses_cpu_model_from_x86_and_arm_shapes() {
        assert_eq!(
            parse_cpu_model("model name : Test CPU\n"),
            Some("Test CPU".to_owned())
        );
        assert_eq!(
            parse_cpu_model("Hardware: Test ARM\n"),
            Some("Test ARM".to_owned())
        );
    }

    #[test]
    fn parses_mem_total_in_kib() {
        assert_eq!(
            parse_memory_total("MemFree: 2 kB\nMemTotal: 4096 kB\n").unwrap(),
            4096
        );
        assert!(parse_memory_total("MemAvailable: 3 kB\n").is_err());
    }

    #[test]
    fn escapes_json_control_characters() {
        assert_eq!(json_escape("GPU \"one\"\\\n"), "GPU \\\"one\\\"\\\\\\n");
    }

    #[test]
    fn serializes_optional_hardware_values_as_null() {
        let snapshot = HardwareSnapshot {
            cpu_model: "Test CPU".to_owned(),
            logical_cpus: 2,
            memory_total_kib: 4096,
            battery_present: false,
            battery_percent: None,
            ac_online: None,
            network_interfaces: vec!["lo".to_owned(), "enp1s0".to_owned()],
            gpu_devices: 1,
        };
        assert_eq!(
            snapshot_as_json(&snapshot),
            concat!(
                "{\"cpu_model\":\"Test CPU\",\"logical_cpus\":2,",
                "\"memory_total_kib\":4096,\"battery_present\":false,",
                "\"battery_percent\":null,\"ac_online\":null,",
                "\"network_interfaces\":[\"lo\",\"enp1s0\"],\"gpu_devices\":1}"
            )
        );
    }
}
