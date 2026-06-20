//! Device-tier auto-tuning.
//!
//! ripsync runs on everything from a 2-core NAS to a 32-core workstation. A fixed
//! set of defaults is wrong at both ends: too many threads thrash a small box, too
//! few leave a big one idle. [`detect`] probes the machine (CPU count, and total
//! RAM where it can be read without `unsafe` or extra dependencies) and picks a
//! [`Profile`]; [`Profile::params`] turns that into concrete [`TuneParams`] —
//! worker threads, copy-buffer size, `io_uring` queue depth, and zstd level —
//! consumed across the walk, copy, and remote paths.

/// A device performance tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Small / constrained device (≤2 cores or ≤2 GiB RAM): conserve memory and
    /// avoid oversubscription.
    Low,
    /// Typical desktop / laptop: balanced defaults.
    Balanced,
    /// Workstation / server (≥8 cores and ≥16 GiB RAM): use the hardware fully.
    High,
}

/// Concrete knobs derived from a [`Profile`] for a given core count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TuneParams {
    /// Worker threads for the parallel walk and apply phases.
    pub threads: usize,
    /// Buffer size for the portable copy path, in bytes.
    pub copy_buffer: usize,
    /// `io_uring` submission/completion queue depth (Linux backend).
    pub uring_queue_depth: usize,
    /// Default zstd level for wire compression.
    pub zstd_level: i32,
}

impl Profile {
    /// Turn this tier into concrete parameters for a machine with `cores` CPUs.
    #[must_use]
    pub fn params(self, cores: usize) -> TuneParams {
        let cores = cores.max(1);
        match self {
            Profile::Low => TuneParams {
                threads: cores.min(2),
                copy_buffer: 256 * 1024,
                uring_queue_depth: 32,
                zstd_level: 1,
            },
            Profile::Balanced => TuneParams {
                threads: cores,
                copy_buffer: 1024 * 1024,
                uring_queue_depth: 128,
                zstd_level: 3,
            },
            Profile::High => TuneParams {
                threads: cores,
                copy_buffer: 8 * 1024 * 1024,
                uring_queue_depth: 512,
                zstd_level: 6,
            },
        }
    }

    /// The tier's lowercase name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Profile::Low => "low",
            Profile::Balanced => "balanced",
            Profile::High => "high",
        }
    }
}

/// Classify a machine from its core count and (optionally known) total RAM in GiB.
///
/// `Low` when ≤2 cores or ≤2 GiB. `High` when ≥8 cores and (RAM unknown or
/// ≥16 GiB). `Balanced` otherwise.
#[must_use]
pub fn resolve(cores: usize, ram_gib: Option<f64>) -> Profile {
    let low = cores <= 2 || ram_gib.is_some_and(|r| r <= 2.0);
    if low {
        return Profile::Low;
    }
    let high = cores >= 8 && ram_gib.is_none_or(|r| r >= 16.0);
    if high {
        Profile::High
    } else {
        Profile::Balanced
    }
}

/// Auto-detect the device profile for the current machine.
#[must_use]
pub fn detect() -> Profile {
    resolve(num_cpus::get(), total_ram_gib())
}

/// Best-effort total physical RAM in GiB. Returns `None` where it cannot be read
/// without `unsafe` or extra dependencies (the classifier then uses cores alone).
#[must_use]
pub fn total_ram_gib() -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:") {
                let kib: f64 = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
                return Some(kib / (1024.0 * 1024.0));
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_tiers() {
        assert_eq!(resolve(1, Some(16.0)), Profile::Low); // few cores
        assert_eq!(resolve(16, Some(1.0)), Profile::Low); // tiny RAM
        assert_eq!(resolve(4, Some(8.0)), Profile::Balanced);
        assert_eq!(resolve(8, Some(16.0)), Profile::High);
        assert_eq!(resolve(12, None), Profile::High); // unknown RAM, many cores
        assert_eq!(resolve(4, None), Profile::Balanced);
    }

    #[test]
    fn params_scale_with_tier() {
        let low = Profile::Low.params(16);
        let high = Profile::High.params(16);
        assert_eq!(low.threads, 2);
        assert_eq!(high.threads, 16);
        assert!(high.copy_buffer > low.copy_buffer);
        assert!(high.uring_queue_depth > low.uring_queue_depth);
    }

    #[test]
    fn params_never_zero_threads() {
        assert_eq!(Profile::Balanced.params(0).threads, 1);
    }
}
