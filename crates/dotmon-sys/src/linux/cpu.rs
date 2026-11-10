//! Per-process CPU usage, computed from the processes' own CPU time and the
//! wall-clock time that really elapsed.
//!
//! # Conventions (checked on the target machine, see `docs/VALIDATION.md`)
//!
//! * `top` (procps-ng): per process, Δ(utime + stime) / (CLK_TCK × elapsed
//!   wall-clock time), in percent of **one** CPU; with the Solaris mode (`I`)
//!   it is divided by the number of CPUs. The machine-wide figure is the
//!   only one that comes from the `/proc/stat` counters.
//! * `sysinfo` 0.40: Δ(utime + stime) divided by the growth of the summed
//!   `/proc/stat` counters **divided by the number of CPUs**. That divisor
//!   is not wall-clock time: on this machine the counters advance at
//!   97.7–98.1 % of `CLK_TCK` × wall-clock time on every core, idle or loaded
//!   (measured over 8 s windows), so every process came out 2–3 % too high.
//!   sysinfo also reports 0 for a process it has just seen.
//!
//! Dotmon follows `top`, expressed for the whole machine:
//!
//! ```text
//! cpu % = Δticks / (CLK_TCK × Δt × logical CPUs) × 100        (0 – 100)
//! ```
//!
//! where Δt is the time between the two readings of **that process**
//! (monotonic clock). A process with no earlier reading — one born since the
//! previous sample — has no measurement: `None`, never 0.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use dotmon_core::model::ProcessId;

/// Shorter windows are rejected: the kernel counts CPU time in clock ticks
/// (10 ms), so a window of a few milliseconds is only quantisation noise.
const MIN_WINDOW_S: f64 = 0.05;

#[derive(Debug, Clone, Copy)]
struct Reading {
    ticks: u64,
    at: Instant,
}

#[derive(Debug)]
pub struct CpuTracker {
    logical_cpus: u64,
    ticks_per_s: u64,
    prev: HashMap<ProcessId, Reading>,
}

impl CpuTracker {
    pub fn new(logical_cpus: usize, ticks_per_s: u64) -> Self {
        Self {
            logical_cpus: logical_cpus.max(1) as u64,
            ticks_per_s: ticks_per_s.max(1),
            prev: HashMap::new(),
        }
    }

    /// Stores the reading and returns the machine share (0–100) used since
    /// the previous reading of the same process, or `None` if there is no
    /// reliable figure: first sighting, window too short, or a counter that
    /// went backwards.
    pub fn observe(&mut self, id: ProcessId, ticks: u64, at: Instant) -> Option<f32> {
        let prev = self.prev.insert(id, Reading { ticks, at })?;
        let window_s = at.checked_duration_since(prev.at)?.as_secs_f64();
        if window_s < MIN_WINDOW_S || ticks < prev.ticks {
            return None;
        }
        let capacity = self.ticks_per_s as f64 * window_s * self.logical_cpus as f64;
        let share = (ticks - prev.ticks) as f64 / capacity * 100.0;
        // One tick of jitter can push a fully busy single thread slightly
        // over its CPU; the machine total cannot exceed 100 %.
        Some(share.clamp(0.0, 100.0) as f32)
    }

    /// Forgets the processes that no longer exist (bounded memory).
    pub fn retain(&mut self, alive: &HashSet<ProcessId>) {
        self.prev.retain(|id, _| alive.contains(id));
    }

    pub fn len(&self) -> usize {
        self.prev.len()
    }

    pub fn is_empty(&self) -> bool {
        self.prev.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn id(pid: u32) -> ProcessId {
        ProcessId { pid, start_time: 1000 + pid as u64 }
    }

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn new_process_has_no_measurement_instead_of_zero() {
        let t0 = Instant::now();
        let mut c = CpuTracker::new(16, 100);
        assert_eq!(c.observe(id(1), 500, t0), None);
        assert!(c.observe(id(1), 600, at(t0, 1000)).is_some());
    }

    #[test]
    fn one_busy_core_of_sixteen_is_6_25_percent() {
        let t0 = Instant::now();
        let mut c = CpuTracker::new(16, 100);
        c.observe(id(1), 0, t0);
        // 100 ticks of CPU time in exactly one second = one full core.
        let v = c.observe(id(1), 100, at(t0, 1000)).unwrap();
        assert!((v - 6.25).abs() < 1e-4, "{v}");
    }

    #[test]
    fn uses_real_elapsed_time_not_the_nominal_interval() {
        // The sampler was late: 1.25 s elapsed instead of 1 s. The same
        // 100 ticks are 80 % of a core over that window, not 100 %.
        let t0 = Instant::now();
        let mut c = CpuTracker::new(4, 100);
        c.observe(id(1), 0, t0);
        let v = c.observe(id(1), 100, at(t0, 1250)).unwrap();
        assert!((v - 20.0).abs() < 1e-4, "{v}"); // 80 % of one core / 4 CPUs
    }

    #[test]
    fn does_not_depend_on_cpu_counters() {
        // Regression: sysinfo divided by the growth of /proc/stat, which
        // advances at ~97.7 % of wall-clock time here, inflating every
        // process by ~2.4 %. Only ticks and wall-clock time are used now.
        let t0 = Instant::now();
        let mut c = CpuTracker::new(16, 100);
        c.observe(id(1), 1000, t0);
        let v = c.observe(id(1), 1100, at(t0, 1000)).unwrap();
        assert_eq!(v, 6.25);
    }

    #[test]
    fn idle_process_measures_zero_and_busy_is_capped() {
        let t0 = Instant::now();
        let mut c = CpuTracker::new(2, 100);
        c.observe(id(1), 50, t0);
        c.observe(id(2), 0, t0);
        assert_eq!(c.observe(id(1), 50, at(t0, 1000)), Some(0.0), "measured idle is a real 0");
        // 450 ticks in 1 s on 2 CPUs would be 225 %: capped at the machine.
        assert_eq!(c.observe(id(2), 450, at(t0, 1000)), Some(100.0));
    }

    #[test]
    fn unreliable_windows_and_counters_give_none() {
        let t0 = Instant::now();
        let mut c = CpuTracker::new(8, 100);
        c.observe(id(1), 100, t0);
        assert_eq!(c.observe(id(1), 101, at(t0, 10)), None, "10 ms window is noise");
        assert_eq!(c.observe(id(1), 50, at(t0, 1100)), None, "counter went backwards");
        // The next window is measured from the last reading: recovered.
        assert!(c.observe(id(1), 150, at(t0, 2100)).is_some());
    }

    #[test]
    fn pid_reuse_is_a_new_process() {
        let t0 = Instant::now();
        let mut c = CpuTracker::new(8, 100);
        let old = ProcessId { pid: 7, start_time: 100 };
        let reused = ProcessId { pid: 7, start_time: 900 };
        c.observe(old, 5000, t0);
        assert_eq!(c.observe(reused, 3, at(t0, 1000)), None);
    }

    #[test]
    fn exited_processes_are_forgotten() {
        let t0 = Instant::now();
        let mut c = CpuTracker::new(8, 100);
        for pid in 1..=5 {
            c.observe(id(pid), 0, t0);
        }
        let alive: HashSet<ProcessId> = [id(2), id(4)].into_iter().collect();
        c.retain(&alive);
        assert_eq!(c.len(), 2);
        assert!(!c.is_empty());
        assert_eq!(c.observe(id(1), 10, at(t0, 1000)), None, "forgotten: new again");
    }
}
