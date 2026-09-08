//! Conservative workload budgets derived from capacity and live telemetry.
//!
//! These decisions are inputs to future cgroup/slice enforcement. They do not
//! change process state themselves, which keeps the first policy milestone
//! safe to test and easy to replace with measured thresholds.

use std::fmt;

use crate::{MachineProfile, ResourceSnapshot};

/// Workload classes with different interactive and reclaimability priorities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkloadClass {
    /// Shell, editor, terminal, debugger, and other user-facing work.
    Interactive,
    /// Restartable compilation and packaging work.
    Build,
    /// Background project indexing and language-server preparation.
    Indexer,
    /// High-memory device or virtual-device simulation.
    Emulator,
    /// Rebuildable caches and other lowest-priority background work.
    Cache,
}

impl WorkloadClass {
    /// Returns the stable identifier used in scope names and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Build => "build",
            Self::Indexer => "indexer",
            Self::Emulator => "emulator",
            Self::Cache => "cache",
        }
    }
}

/// Relative memory treatment for a workload scope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemoryProtection {
    /// Preserve this workload under reclaim pressure where possible.
    Protected,
    /// Keep available while allowing the kernel to reclaim it before protected work.
    Normal,
    /// Reclaim or defer this workload before normal work.
    Reclaimable,
}

/// The safe initial budget for one workload class.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkloadBudget {
    /// Maximum number of parallel workers allowed by the current decision.
    pub max_workers: u16,
    /// Relative CPU weight for a future cgroup scope, in the systemd range.
    pub cpu_weight: u32,
    /// Relative memory protection for a future cgroup scope.
    pub memory_protection: MemoryProtection,
    /// Whether the caller should defer this workload to another worker/device.
    pub deferred: bool,
}

/// A validated, non-executing resource scope decision for one job.
///
/// The service can translate this into a transient systemd scope or an
/// equivalent cgroup-v2 operation. Keeping the decision separate from the
/// executor prevents an ordinary build/test request from gaining authority to
/// create or modify cgroups by itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkloadScopePlan {
    /// Stable systemd unit name for this job.
    pub unit_name: String,
    /// Workload class from which the policy was derived.
    pub workload: WorkloadClass,
    /// Maximum local parallel workers allowed by the current policy.
    pub max_workers: u16,
    /// Relative CPU weight in the systemd/cgroup-v2 range.
    pub cpu_weight: u32,
    /// Optional protected memory floor in bytes.
    pub memory_low_bytes: Option<u64>,
    /// Optional reclaim/throttling threshold in bytes.
    pub memory_high_bytes: Option<u64>,
    /// Optional hard memory ceiling in bytes.
    pub memory_max_bytes: Option<u64>,
    /// Whether the caller must defer this job instead of starting it.
    pub deferred: bool,
}

impl WorkloadScopePlan {
    /// Wraps a validated direct argv command in a user systemd scope argv.
    ///
    /// This only creates an argv vector. It never invokes `systemd-run`,
    /// changes cgroups, or starts a process.
    pub fn systemd_run_argv(&self, command: &[String]) -> Result<Vec<String>, ScopePlanError> {
        validate_direct_command(command)?;

        let mut argv = vec![
            "systemd-run".to_owned(),
            "--user".to_owned(),
            "--scope".to_owned(),
            "--quiet".to_owned(),
            "--collect".to_owned(),
            format!("--unit={}", self.unit_name),
            format!("--property=CPUWeight={}", self.cpu_weight),
        ];
        if let Some(memory_low_bytes) = self.memory_low_bytes {
            argv.push(format!("--property=MemoryLow={memory_low_bytes}"));
        }
        if let Some(memory_high_bytes) = self.memory_high_bytes {
            argv.push(format!("--property=MemoryHigh={memory_high_bytes}"));
        }
        if let Some(memory_max_bytes) = self.memory_max_bytes {
            argv.push(format!("--property=MemoryMax={memory_max_bytes}"));
        }
        argv.push("--".to_owned());
        argv.extend(command.iter().cloned());
        Ok(argv)
    }
}

/// Failure while constructing a resource scope plan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScopePlanError {
    /// Job identifiers are restricted before they become unit names.
    InvalidJobId(String),
    /// A scope cannot wrap an empty command.
    EmptyCommand,
    /// An argv entry contains a NUL byte.
    NulArgument,
}

impl fmt::Display for ScopePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJobId(id) => write!(formatter, "invalid scope job id {id:?}"),
            Self::EmptyCommand => formatter.write_str("scope command is empty"),
            Self::NulArgument => formatter.write_str("scope command contains a NUL byte"),
        }
    }
}

impl std::error::Error for ScopePlanError {}

impl MachineProfile {
    /// Returns conservative, unmeasured startup budgets for a workload class.
    ///
    /// These are intentionally small defaults. Live PSI and power state are
    /// applied by [`ResourceSnapshot::workload_budget`] before a caller uses
    /// them to schedule work.
    #[must_use]
    pub const fn base_workload_budget(self, workload: WorkloadClass) -> WorkloadBudget {
        let (max_workers, memory_protection, cpu_weight) = match workload {
            WorkloadClass::Interactive => (1, MemoryProtection::Protected, 1_000),
            WorkloadClass::Build => match self {
                Self::Low => (1, MemoryProtection::Normal, 300),
                Self::Balanced => (2, MemoryProtection::Normal, 300),
                Self::Standard => (4, MemoryProtection::Normal, 300),
                Self::Workstation => (8, MemoryProtection::Normal, 300),
            },
            WorkloadClass::Indexer => match self {
                Self::Low | Self::Balanced => (1, MemoryProtection::Normal, 150),
                Self::Standard => (2, MemoryProtection::Normal, 150),
                Self::Workstation => (4, MemoryProtection::Normal, 150),
            },
            WorkloadClass::Emulator => match self {
                Self::Low => (0, MemoryProtection::Reclaimable, 100),
                Self::Balanced | Self::Standard => (1, MemoryProtection::Reclaimable, 100),
                Self::Workstation => (2, MemoryProtection::Reclaimable, 100),
            },
            WorkloadClass::Cache => (1, MemoryProtection::Reclaimable, 50),
        };

        WorkloadBudget {
            max_workers,
            cpu_weight,
            memory_protection,
            deferred: max_workers == 0,
        }
    }
}

impl ResourceSnapshot {
    /// Calculates a safe workload budget from capacity, PSI, and power state.
    ///
    /// A 1% ten-second PSI signal starts reducing restartable background work;
    /// a 5% signal reduces it to one worker. These provisional thresholds are
    /// deliberately conservative and must be benchmarked before enforcement.
    #[must_use]
    pub fn workload_budget(&self, workload: WorkloadClass) -> WorkloadBudget {
        const CONTENTION_MILLI_PERCENT: u32 = 1_000;
        const SEVERE_CONTENTION_MILLI_PERCENT: u32 = 5_000;

        let mut budget = self.initial_profile.base_workload_budget(workload);
        let pressure = self.max_pressure_milli_percent();
        let on_battery = self.power.battery_present && self.power.ac_online == Some(false);

        if workload == WorkloadClass::Interactive {
            return budget;
        }

        if pressure >= SEVERE_CONTENTION_MILLI_PERCENT {
            budget.max_workers = if workload == WorkloadClass::Emulator {
                0
            } else {
                1
            };
        } else if pressure >= CONTENTION_MILLI_PERCENT {
            budget.max_workers = if workload == WorkloadClass::Emulator {
                0
            } else {
                budget.max_workers.div_ceil(2).max(1)
            };
        }

        if on_battery && workload == WorkloadClass::Build {
            budget.max_workers = budget.max_workers.div_ceil(2).max(1);
        }

        if workload == WorkloadClass::Emulator
            && (self.initial_profile == MachineProfile::Low || on_battery)
        {
            budget.max_workers = 0;
        }

        budget.deferred = budget.max_workers == 0;
        budget
    }

    /// Produces the cgroup/systemd settings for one workload without applying them.
    pub fn workload_scope(
        &self,
        job_id: &str,
        workload: WorkloadClass,
    ) -> Result<WorkloadScopePlan, ScopePlanError> {
        validate_scope_job_id(job_id)?;
        let budget = self.workload_budget(workload);
        let total_memory_bytes = self.capacity.memory_total_kib.saturating_mul(1024);

        let memory_low_bytes = match budget.memory_protection {
            MemoryProtection::Protected => Some(clamped_budget(
                total_memory_bytes,
                8,
                64 * 1024 * 1024,
                512 * 1024 * 1024,
            )),
            MemoryProtection::Normal | MemoryProtection::Reclaimable => None,
        };
        let memory_high_bytes = match budget.memory_protection {
            MemoryProtection::Reclaimable => Some(clamped_budget(
                total_memory_bytes,
                2,
                256 * 1024 * 1024,
                2 * 1024 * 1024 * 1024,
            )),
            MemoryProtection::Protected | MemoryProtection::Normal => None,
        };
        let memory_max_bytes = (workload == WorkloadClass::Emulator).then(|| {
            clamped_budget(
                total_memory_bytes,
                2,
                256 * 1024 * 1024,
                4 * 1024 * 1024 * 1024,
            )
        });

        Ok(WorkloadScopePlan {
            unit_name: format!("devcore-job-{job_id}"),
            workload,
            max_workers: budget.max_workers,
            cpu_weight: budget.cpu_weight,
            memory_low_bytes,
            memory_high_bytes,
            memory_max_bytes,
            deferred: budget.deferred,
        })
    }

    fn max_pressure_milli_percent(&self) -> u32 {
        [self.cpu_pressure, self.memory_pressure, self.io_pressure]
            .into_iter()
            .flatten()
            .map(|pressure| pressure.some_avg10_milli_percent)
            .max()
            .unwrap_or(0)
    }
}

fn validate_scope_job_id(job_id: &str) -> Result<(), ScopePlanError> {
    if job_id.is_empty()
        || job_id.len() > 63
        || !job_id.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        })
        || job_id.starts_with('-')
    {
        return Err(ScopePlanError::InvalidJobId(job_id.to_owned()));
    }
    Ok(())
}

fn validate_direct_command(command: &[String]) -> Result<(), ScopePlanError> {
    if command.is_empty() || command[0].is_empty() {
        return Err(ScopePlanError::EmptyCommand);
    }
    if command.iter().any(|argument| argument.contains('\0')) {
        return Err(ScopePlanError::NulArgument);
    }
    Ok(())
}

fn clamped_budget(total_bytes: u64, divisor: u64, floor: u64, ceiling: u64) -> u64 {
    let fraction = total_bytes / divisor;
    fraction
        .max(floor.min(total_bytes))
        .min(ceiling.min(total_bytes))
}

#[cfg(test)]
mod tests {
    use super::{MemoryProtection, ScopePlanError, WorkloadClass};
    use crate::{HardwareCapacity, MachineProfile, PowerState, PressureMetrics, ResourceSnapshot};

    fn snapshot(
        profile: MachineProfile,
        pressure: Option<PressureMetrics>,
        power: PowerState,
    ) -> ResourceSnapshot {
        ResourceSnapshot {
            capacity: HardwareCapacity::new(8, 16 * 1024 * 1024),
            initial_profile: profile,
            memory_available_kib: Some(8 * 1024 * 1024),
            cpu_pressure: pressure,
            memory_pressure: None,
            io_pressure: None,
            power,
        }
    }

    #[test]
    fn low_profile_defers_emulator_and_keeps_build_single_threaded() {
        let snapshot = snapshot(MachineProfile::Low, None, PowerState::default());

        assert_eq!(
            snapshot.workload_budget(WorkloadClass::Build).max_workers,
            1
        );
        assert!(snapshot.workload_budget(WorkloadClass::Emulator).deferred);
    }

    #[test]
    fn interactive_work_stays_protected_under_pressure() {
        let snapshot = snapshot(
            MachineProfile::Workstation,
            Some(PressureMetrics {
                some_avg10_milli_percent: 9_000,
                full_avg10_milli_percent: None,
            }),
            PowerState::default(),
        );
        let budget = snapshot.workload_budget(WorkloadClass::Interactive);

        assert_eq!(budget.max_workers, 1);
        assert_eq!(budget.memory_protection, MemoryProtection::Protected);
        assert!(!budget.deferred);
    }

    #[test]
    fn severe_pressure_defers_emulator_and_caps_build() {
        let snapshot = snapshot(
            MachineProfile::Workstation,
            Some(PressureMetrics {
                some_avg10_milli_percent: 5_000,
                full_avg10_milli_percent: None,
            }),
            PowerState::default(),
        );

        assert_eq!(
            snapshot.workload_budget(WorkloadClass::Build).max_workers,
            1
        );
        assert!(snapshot.workload_budget(WorkloadClass::Emulator).deferred);
    }

    #[test]
    fn battery_build_budget_is_reduced_without_affecting_cache_priority() {
        let snapshot = snapshot(
            MachineProfile::Workstation,
            None,
            PowerState {
                battery_present: true,
                battery_percent: Some(60),
                ac_online: Some(false),
            },
        );

        assert_eq!(
            snapshot.workload_budget(WorkloadClass::Build).max_workers,
            4
        );
        assert_eq!(
            snapshot.workload_budget(WorkloadClass::Cache).cpu_weight,
            50
        );
    }

    #[test]
    fn scope_plan_is_shell_free_and_does_not_apply_cgroups() {
        let snapshot = snapshot(MachineProfile::Workstation, None, PowerState::default());
        let plan = snapshot
            .workload_scope("compile-job", WorkloadClass::Build)
            .unwrap();
        let argv = plan
            .systemd_run_argv(&["podman".to_owned(), "exec".to_owned(), "cargo".to_owned()])
            .unwrap();

        assert_eq!(plan.unit_name, "devcore-job-compile-job");
        assert_eq!(plan.cpu_weight, 300);
        assert!(plan.memory_low_bytes.is_none());
        assert_eq!(argv[0..3], ["systemd-run", "--user", "--scope"]);
        assert!(
            argv.iter()
                .any(|argument| argument == "--property=CPUWeight=300")
        );
        assert_eq!(argv.last(), Some(&"cargo".to_owned()));
    }

    #[test]
    fn protected_and_emulator_scopes_have_explicit_memory_controls() {
        let snapshot = snapshot(MachineProfile::Workstation, None, PowerState::default());
        let interactive = snapshot
            .workload_scope("shell", WorkloadClass::Interactive)
            .unwrap();
        let emulator = snapshot
            .workload_scope("android-emulator", WorkloadClass::Emulator)
            .unwrap();

        assert_eq!(interactive.memory_low_bytes, Some(512 * 1024 * 1024));
        assert!(emulator.memory_high_bytes.is_some());
        assert!(emulator.memory_max_bytes.is_some());
    }

    #[test]
    fn scope_plan_rejects_unsafe_ids_and_commands() {
        let snapshot = snapshot(MachineProfile::Low, None, PowerState::default());

        assert!(matches!(
            snapshot.workload_scope("bad id", WorkloadClass::Build),
            Err(ScopePlanError::InvalidJobId(_))
        ));
        let plan = snapshot
            .workload_scope("job", WorkloadClass::Build)
            .unwrap();
        assert!(matches!(
            plan.systemd_run_argv(&["sh\0".to_owned()]),
            Err(ScopePlanError::NulArgument)
        ));
        assert!(matches!(
            plan.systemd_run_argv(&[]),
            Err(ScopePlanError::EmptyCommand)
        ));
    }
}
