//! Conservative hardware-capacity classification.
//!
//! This is an initial capacity label for defaults and test fixtures. Runtime
//! resource policy must also account for live PSI, workload class, battery
//! state, and thermal state.

use serde::{Deserialize, Serialize};

/// The machine classes DevCore must continuously validate.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum MachineProfile {
    /// At most two logical CPUs or at most 4 GiB of memory.
    Low,
    /// At most four logical CPUs or at most 8 GiB of memory.
    Balanced,
    /// At most eight logical CPUs or at most 16 GiB of memory.
    Standard,
    /// More capable systems that can tolerate higher default concurrency.
    Workstation,
}

impl MachineProfile {
    /// Returns the stable identifier used by configuration and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Balanced => "balanced",
            Self::Standard => "standard",
            Self::Workstation => "workstation",
        }
    }
}

/// Capacity facts used only to choose conservative initial defaults.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HardwareCapacity {
    /// Logical CPUs available to the current system or cgroup.
    pub logical_cpus: u16,
    /// Total memory visible to the current system or cgroup, in KiB.
    pub memory_total_kib: u64,
}

impl HardwareCapacity {
    /// Creates a capacity record from the observed logical CPU and memory totals.
    #[must_use]
    pub const fn new(logical_cpus: u16, memory_total_kib: u64) -> Self {
        Self {
            logical_cpus,
            memory_total_kib,
        }
    }

    /// Chooses the most conservative matching initial profile.
    ///
    /// This deliberately selects the lower capability dimension. A machine with
    /// many CPUs but little memory should not receive workstation defaults.
    #[must_use]
    pub const fn initial_profile(self) -> MachineProfile {
        const GIB_IN_KIB: u64 = 1024 * 1024;

        if self.logical_cpus <= 2 || self.memory_total_kib <= 4 * GIB_IN_KIB {
            MachineProfile::Low
        } else if self.logical_cpus <= 4 || self.memory_total_kib <= 8 * GIB_IN_KIB {
            MachineProfile::Balanced
        } else if self.logical_cpus <= 8 || self.memory_total_kib <= 16 * GIB_IN_KIB {
            MachineProfile::Standard
        } else {
            MachineProfile::Workstation
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{HardwareCapacity, MachineProfile};

    const GIB_IN_KIB: u64 = 1024 * 1024;

    #[test]
    fn prompt_reference_profiles_classify_as_expected() {
        let cases = [
            (2, 4, MachineProfile::Low),
            (4, 8, MachineProfile::Balanced),
            (8, 16, MachineProfile::Standard),
            (16, 64, MachineProfile::Workstation),
        ];

        for (cpus, memory_gib, expected) in cases {
            let capacity = HardwareCapacity::new(cpus, memory_gib * GIB_IN_KIB);
            assert_eq!(capacity.initial_profile(), expected);
        }
    }

    #[test]
    fn low_memory_keeps_high_cpu_machine_conservative() {
        let capacity = HardwareCapacity::new(32, 4 * GIB_IN_KIB);

        assert_eq!(capacity.initial_profile(), MachineProfile::Low);
    }

    #[test]
    fn profile_identifiers_are_stable() {
        assert_eq!(MachineProfile::Low.as_str(), "low");
        assert_eq!(MachineProfile::Balanced.as_str(), "balanced");
        assert_eq!(MachineProfile::Standard.as_str(), "standard");
        assert_eq!(MachineProfile::Workstation.as_str(), "workstation");
    }
}
