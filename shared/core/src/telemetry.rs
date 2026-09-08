//! Linux hardware and pressure telemetry collection.
//!
//! Collection is intentionally one-shot. The `devcored` D-Bus service invokes
//! it at startup or on an explicit refresh; a later kernel/systemd event
//! watcher will trigger refreshes without placing this module in a busy loop.

use std::{
    error::Error,
    fmt, fs, io,
    path::{Path, PathBuf},
    thread,
};

use crate::{HardwareCapacity, MachineProfile};

const MEMINFO_PATH: &str = "/proc/meminfo";
const CPU_PRESSURE_PATH: &str = "/proc/pressure/cpu";
const MEMORY_PRESSURE_PATH: &str = "/proc/pressure/memory";
const IO_PRESSURE_PATH: &str = "/proc/pressure/io";
const POWER_SUPPLY_PATH: &str = "/sys/class/power_supply";

/// A pressure-stall measurement expressed in thousandths of a percent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PressureMetrics {
    /// Ten-second `some` pressure average in thousandths of a percent.
    pub some_avg10_milli_percent: u32,
    /// Ten-second `full` pressure average in thousandths of a percent, if provided.
    pub full_avg10_milli_percent: Option<u32>,
}

/// The observed battery and AC state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PowerState {
    /// Whether the host exposes at least one battery device.
    pub battery_present: bool,
    /// Reported battery charge percentage, if the kernel provides one.
    pub battery_percent: Option<u8>,
    /// Whether an external power source is online, if the kernel provides one.
    pub ac_online: Option<bool>,
}

/// A single non-persistent resource telemetry snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceSnapshot {
    /// Capacity observed for the current host or cgroup.
    pub capacity: HardwareCapacity,
    /// Conservative startup profile derived from the observed capacity.
    pub initial_profile: MachineProfile,
    /// Memory available for allocation in KiB, if exported by the kernel.
    pub memory_available_kib: Option<u64>,
    /// CPU pressure, if PSI is enabled by the kernel.
    pub cpu_pressure: Option<PressureMetrics>,
    /// Memory pressure, if PSI is enabled by the kernel.
    pub memory_pressure: Option<PressureMetrics>,
    /// I/O pressure, if PSI is enabled by the kernel.
    pub io_pressure: Option<PressureMetrics>,
    /// Battery and AC information derived from sysfs.
    pub power: PowerState,
}

/// An error encountered while obtaining or interpreting kernel telemetry.
#[derive(Debug)]
pub enum SnapshotError {
    /// A required kernel-provided file could not be read.
    Read {
        /// Path that failed to be read.
        path: PathBuf,
        /// Underlying I/O failure.
        source: io::Error,
    },
    /// A required kernel-provided value did not match its documented shape.
    Malformed {
        /// Source being parsed.
        source: &'static str,
        /// Human-readable explanation of the invalid value.
        detail: String,
    },
}

impl fmt::Display for SnapshotError {
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

impl Error for SnapshotError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Malformed { .. } => None,
        }
    }
}

/// Collects a one-shot snapshot from Linux `/proc` and `/sys`.
///
/// PSI is optional because some kernels or container environments do not expose
/// it. Memory totals are required because DevCore cannot select even conservative
/// defaults without them.
pub fn collect_snapshot() -> Result<ResourceSnapshot, SnapshotError> {
    let memory_info = read_required(MEMINFO_PATH)?;
    let (memory_total_kib, memory_available_kib) = parse_meminfo(&memory_info)?;
    let logical_cpus = available_logical_cpus();
    let capacity = HardwareCapacity::new(logical_cpus, memory_total_kib);

    Ok(ResourceSnapshot {
        capacity,
        initial_profile: capacity.initial_profile(),
        memory_available_kib,
        cpu_pressure: read_optional_pressure(CPU_PRESSURE_PATH)?,
        memory_pressure: read_optional_pressure(MEMORY_PRESSURE_PATH)?,
        io_pressure: read_optional_pressure(IO_PRESSURE_PATH)?,
        power: read_power_state(Path::new(POWER_SUPPLY_PATH)),
    })
}

/// Formats a snapshot as a small dependency-free JSON document.
#[must_use]
pub fn snapshot_as_json(snapshot: &ResourceSnapshot) -> String {
    format!(
        concat!(
            "{{\"profile\":\"{}\",\"logical_cpus\":{},",
            "\"memory_total_kib\":{},\"memory_available_kib\":{},",
            "\"pressure\":{{\"cpu\":{},\"memory\":{},\"io\":{}}},",
            "\"power\":{{\"battery_present\":{},\"battery_percent\":{},\"ac_online\":{}}}}}"
        ),
        snapshot.initial_profile.as_str(),
        snapshot.capacity.logical_cpus,
        snapshot.capacity.memory_total_kib,
        optional_u64_json(snapshot.memory_available_kib),
        optional_pressure_json(snapshot.cpu_pressure),
        optional_pressure_json(snapshot.memory_pressure),
        optional_pressure_json(snapshot.io_pressure),
        snapshot.power.battery_present,
        optional_u8_json(snapshot.power.battery_percent),
        optional_bool_json(snapshot.power.ac_online),
    )
}

fn available_logical_cpus() -> u16 {
    thread::available_parallelism()
        .ok()
        .and_then(|count| u16::try_from(count.get()).ok())
        .filter(|count| *count > 0)
        .unwrap_or(1)
}

fn read_required(path: &str) -> Result<String, SnapshotError> {
    fs::read_to_string(path).map_err(|source| SnapshotError::Read {
        path: PathBuf::from(path),
        source,
    })
}

fn read_optional_pressure(path: &str) -> Result<Option<PressureMetrics>, SnapshotError> {
    match fs::read_to_string(path) {
        Ok(contents) => parse_pressure(&contents).map(Some),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SnapshotError::Read {
            path: PathBuf::from(path),
            source,
        }),
    }
}

fn parse_meminfo(contents: &str) -> Result<(u64, Option<u64>), SnapshotError> {
    let mut total = None;
    let mut available = None;

    for line in contents.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value_kib = value
            .split_whitespace()
            .next()
            .and_then(|value| value.parse::<u64>().ok());

        match key {
            "MemTotal" => total = value_kib,
            "MemAvailable" => available = value_kib,
            _ => {}
        }
    }

    total
        .map(|total| (total, available))
        .ok_or_else(|| SnapshotError::Malformed {
            source: MEMINFO_PATH,
            detail: "MemTotal is missing or invalid".to_owned(),
        })
}

fn parse_pressure(contents: &str) -> Result<PressureMetrics, SnapshotError> {
    let mut some_avg10_milli_percent = None;
    let mut full_avg10_milli_percent = None;

    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        let Some(kind) = fields.next() else {
            continue;
        };
        let average = fields.find_map(|field| field.strip_prefix("avg10="));
        let Some(average) = average else {
            continue;
        };
        let average = parse_percent_milli(average).map_err(|detail| SnapshotError::Malformed {
            source: "PSI",
            detail,
        })?;

        match kind {
            "some" => some_avg10_milli_percent = Some(average),
            "full" => full_avg10_milli_percent = Some(average),
            _ => {}
        }
    }

    some_avg10_milli_percent
        .map(|some_avg10_milli_percent| PressureMetrics {
            some_avg10_milli_percent,
            full_avg10_milli_percent,
        })
        .ok_or_else(|| SnapshotError::Malformed {
            source: "PSI",
            detail: "some avg10 is missing or invalid".to_owned(),
        })
}

fn parse_percent_milli(value: &str) -> Result<u32, String> {
    let (whole, fraction) = match value.split_once('.') {
        Some(parts) => parts,
        None => (value, ""),
    };
    let whole = whole
        .parse::<u32>()
        .map_err(|_| format!("invalid pressure value {value:?}"))?;
    let mut fraction_milli = 0_u32;

    for (index, character) in fraction.chars().take(3).enumerate() {
        let digit = character
            .to_digit(10)
            .ok_or_else(|| format!("invalid pressure value {value:?}"))?;
        fraction_milli += match index {
            0 => digit * 100,
            1 => digit * 10,
            _ => digit,
        };
    }

    let milli_percent = whole
        .checked_mul(1000)
        .and_then(|whole| whole.checked_add(fraction_milli))
        .ok_or_else(|| format!("pressure value overflows {value:?}"))?;

    if milli_percent > 100_000 {
        return Err(format!("pressure exceeds 100 percent: {value:?}"));
    }

    Ok(milli_percent)
}

fn read_power_state(root: &Path) -> PowerState {
    let Ok(entries) = fs::read_dir(root) else {
        return PowerState::default();
    };

    let mut state = PowerState::default();
    for entry in entries.flatten() {
        let path = entry.path();
        let kind = read_trimmed(path.join("type"));
        match kind.as_deref() {
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

fn optional_u64_json(value: Option<u64>) -> String {
    value.map_or_else(|| "null".to_owned(), |value| value.to_string())
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

fn optional_pressure_json(value: Option<PressureMetrics>) -> String {
    value.map_or_else(
        || "null".to_owned(),
        |value| {
            format!(
                "{{\"some_avg10_milli_percent\":{},\"full_avg10_milli_percent\":{}}}",
                value.some_avg10_milli_percent,
                optional_u64_json(value.full_avg10_milli_percent.map(u64::from)),
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{
        PowerState, PressureMetrics, ResourceSnapshot, parse_meminfo, parse_online,
        parse_percent_milli, parse_pressure, snapshot_as_json,
    };
    use crate::{HardwareCapacity, MachineProfile};

    #[test]
    fn parses_required_and_optional_memory_values() {
        let contents =
            "MemTotal:       8388608 kB\nMemFree:         123456 kB\nMemAvailable:   4194304 kB\n";

        assert_eq!(
            parse_meminfo(contents).unwrap(),
            (8_388_608, Some(4_194_304))
        );
    }

    #[test]
    fn rejects_memory_data_without_total() {
        let error = parse_meminfo("MemAvailable: 5 kB\n").unwrap_err();

        assert!(error.to_string().contains("MemTotal"));
    }

    #[test]
    fn parses_psi_without_assuming_full_cpu_pressure() {
        let cpu = "some avg10=12.34 avg60=10.00 avg300=9.99 total=42\n";
        let memory = "some avg10=1.2 avg60=0.00 avg300=0.00 total=2\nfull avg10=0.01 avg60=0.00 avg300=0.00 total=1\n";

        assert_eq!(
            parse_pressure(cpu).unwrap(),
            PressureMetrics {
                some_avg10_milli_percent: 12_340,
                full_avg10_milli_percent: None,
            }
        );
        assert_eq!(
            parse_pressure(memory).unwrap(),
            PressureMetrics {
                some_avg10_milli_percent: 1_200,
                full_avg10_milli_percent: Some(10),
            }
        );
    }

    #[test]
    fn rejects_invalid_or_impossible_pressure_values() {
        assert!(parse_percent_milli("100.001").is_err());
        assert!(parse_percent_milli("not-a-number").is_err());
    }

    #[test]
    fn serializes_optional_values_as_json_nulls() {
        let snapshot = ResourceSnapshot {
            capacity: HardwareCapacity::new(4, 8 * 1024 * 1024),
            initial_profile: MachineProfile::Balanced,
            memory_available_kib: None,
            cpu_pressure: None,
            memory_pressure: None,
            io_pressure: None,
            power: PowerState::default(),
        };

        assert_eq!(
            snapshot_as_json(&snapshot),
            "{\"profile\":\"balanced\",\"logical_cpus\":4,\"memory_total_kib\":8388608,\"memory_available_kib\":null,\"pressure\":{\"cpu\":null,\"memory\":null,\"io\":null},\"power\":{\"battery_present\":false,\"battery_percent\":null,\"ac_online\":null}}"
        );
    }

    #[test]
    fn parses_kernel_online_values() {
        assert_eq!(parse_online("1"), Some(true));
        assert_eq!(parse_online("0"), Some(false));
        assert_eq!(parse_online("unknown"), None);
    }
}
