//! Data model shared by every backend and by the UI.
//!
//! Units are always explicit in field names or docs: bytes, bytes/second,
//! percent (0–100), seconds.

/// Stable identity of a process.
///
/// A PID alone is not enough: the kernel recycles PIDs. Pairing it with the
/// start time (in clock ticks / seconds since boot, backend-defined but
/// monotonic per process) lets the controller refuse to signal a process
/// that merely reuses the PID of the one the user selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProcessId {
    pub pid: u32,
    pub start_time: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcState {
    Running,
    Sleeping,
    Idle,
    DiskSleep,
    Stopped,
    Zombie,
    Unknown,
}

impl ProcState {
    pub fn label(self) -> &'static str {
        match self {
            ProcState::Running => "ACTIF",
            ProcState::Sleeping => "VEILLE",
            ProcState::Idle => "INACTIF",
            ProcState::DiskSleep => "E/S",
            ProcState::Stopped => "STOPPÉ",
            ProcState::Zombie => "ZOMBIE",
            ProcState::Unknown => "—",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: Option<u32>,
    /// Short name (`comm`, may be truncated to 15 chars by the kernel).
    pub name: String,
    /// Basename of the executable, when known — used to recognise processes
    /// whose `comm` is truncated.
    pub exe_name: Option<String>,
    pub cmdline: String,
    pub user: String,
    pub uid: u32,
    pub state: ProcState,
    /// CPU usage normalised to the whole machine (0–100, all logical CPUs),
    /// averaged over the time actually elapsed since the previous sample.
    /// `None` while there is no reliable measurement: a process first seen in
    /// this sample has no earlier reading to subtract. Never shown as 0.
    pub cpu: Option<f32>,
    /// Resident memory in bytes.
    pub memory: u64,
    pub threads: u32,
    /// Opaque start stamp used for identity (Linux: clock ticks since boot,
    /// field 22 of `/proc/<pid>/stat`). Never shown to the user.
    pub start_time: u64,
    /// Start time in seconds since boot, for display.
    pub started_s: u64,
    /// Disk I/O rates; `None` when not readable (e.g. another user's process).
    pub disk_read_bps: Option<u64>,
    pub disk_write_bps: Option<u64>,
    pub kernel_thread: bool,
    /// Cgroup v2 path, when available.
    pub cgroup: Option<String>,
}

impl ProcessInfo {
    pub fn id(&self) -> ProcessId {
        ProcessId { pid: self.pid, start_time: self.start_time }
    }

    /// Best human-readable name: executable basename if `comm` looks truncated.
    pub fn display_name(&self) -> &str {
        match &self.exe_name {
            Some(exe) if self.name.len() >= 15 && exe.starts_with(&self.name) => exe,
            _ => &self.name,
        }
    }

    /// Combined read + write rate, if known.
    pub fn io_bps(&self) -> Option<u64> {
        match (self.disk_read_bps, self.disk_write_bps) {
            (None, None) => None,
            (r, w) => Some(r.unwrap_or(0) + w.unwrap_or(0)),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CpuStats {
    pub model: String,
    /// Total usage 0–100.
    pub usage: f32,
    pub per_core: Vec<f32>,
    /// Average current frequency across cores.
    pub frequency_mhz: Option<u32>,
    pub temperature_c: Option<f32>,
    pub load_avg: Option<[f32; 3]>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemoryStats {
    pub total: u64,
    pub used: u64,
    pub available: u64,
    /// Buffers + page cache + reclaimable slab (what `free` calls buff/cache).
    pub cached: Option<u64>,
    pub swap_total: u64,
    pub swap_used: u64,
}

impl MemoryStats {
    pub fn used_percent(&self) -> f32 {
        percent(self.used, self.total)
    }
    pub fn swap_percent(&self) -> f32 {
        percent(self.swap_used, self.swap_total)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GpuStats {
    pub name: String,
    pub vendor: String,
    /// Utilisation 0–100, `None` if the driver does not expose it.
    pub usage: Option<f32>,
    pub vram_total: Option<u64>,
    pub vram_used: Option<u64>,
    pub temperature_c: Option<f32>,
    /// Power drawn by the GPU alone (NVML: the GPU and its memory; amdgpu
    /// on a discrete card). `None` when unreadable **or when the only
    /// figure available covers more than the GPU**, see `soc_power_w`.
    pub power_w: Option<f32>,
    /// Power of the whole chip an integrated GPU lives in (AMD APU: CPU
    /// cores included). Never to be shown as the GPU's own power.
    pub soc_power_w: Option<f32>,
    pub state: GpuState,
}

/// Power/monitoring state of a GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GpuState {
    /// Powered and measured this sample.
    #[default]
    Active,
    /// Powered but idle at the last measurement: deliberately not queried
    /// for a while so the driver can power it down (polling would keep it
    /// awake). Metrics are unavailable meanwhile.
    Resting,
    /// Runtime-suspended (powered down); never queried, that would wake it.
    Asleep,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiskStats {
    pub name: String,
    pub mount_point: String,
    pub filesystem: String,
    pub total: u64,
    pub used: u64,
    /// Rates of the underlying block device, when it is known.
    pub read_bps: Option<u64>,
    pub write_bps: Option<u64>,
}

/// Machine-wide disk throughput, counted once per physical device.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiskIo {
    pub read_bps: u64,
    pub write_bps: u64,
}

impl DiskStats {
    pub fn used_percent(&self) -> f32 {
        percent(self.used, self.total)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NetStats {
    pub interface: String,
    pub rx_bps: u64,
    pub tx_bps: u64,
    pub rx_total: u64,
    pub tx_total: u64,
}

/// One complete sample of the machine.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// Monotonic sequence number, increases by one per sample.
    pub seq: u64,
    pub hostname: String,
    pub kernel: String,
    pub uptime_s: u64,
    pub cpu: CpuStats,
    pub memory: MemoryStats,
    pub gpus: Vec<GpuStats>,
    pub disks: Vec<DiskStats>,
    /// Physical-device totals. When `None`, totals are the sum of `disks`.
    pub disk_io: Option<DiskIo>,
    pub networks: Vec<NetStats>,
    pub processes: Vec<ProcessInfo>,
}

impl Snapshot {
    pub fn process(&self, id: ProcessId) -> Option<&ProcessInfo> {
        self.processes.iter().find(|p| p.id() == id)
    }

    pub fn net_rx_bps(&self) -> u64 {
        self.networks.iter().map(|n| n.rx_bps).sum()
    }
    pub fn net_tx_bps(&self) -> u64 {
        self.networks.iter().map(|n| n.tx_bps).sum()
    }
    pub fn disk_read_bps(&self) -> u64 {
        match self.disk_io {
            Some(io) => io.read_bps,
            None => self.disks.iter().filter_map(|d| d.read_bps).sum(),
        }
    }
    pub fn disk_write_bps(&self) -> u64 {
        match self.disk_io {
            Some(io) => io.write_bps,
            None => self.disks.iter().filter_map(|d| d.write_bps).sum(),
        }
    }
}

pub fn percent(part: u64, total: u64) -> f32 {
    if total == 0 { 0.0 } else { ((part as f64 / total as f64) * 100.0).clamp(0.0, 100.0) as f32 }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub fn proc(pid: u32, ppid: Option<u32>, name: &str, uid: u32) -> ProcessInfo {
        ProcessInfo {
            pid,
            ppid,
            name: name.to_string(),
            exe_name: None,
            cmdline: name.to_string(),
            user: if uid == 0 { "root".into() } else { "user".into() },
            uid,
            state: ProcState::Sleeping,
            cpu: Some(0.0),
            memory: 0,
            threads: 1,
            start_time: 100 + pid as u64,
            started_s: 1 + pid as u64,
            disk_read_bps: Some(0),
            disk_write_bps: Some(0),
            kernel_thread: false,
            cgroup: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::proc;
    use super::*;

    #[test]
    fn percent_handles_zero_total() {
        assert_eq!(percent(5, 0), 0.0);
        assert_eq!(percent(50, 200), 25.0);
        assert_eq!(percent(500, 200), 100.0);
    }

    #[test]
    fn display_name_prefers_exe_when_comm_truncated() {
        let mut p = proc(10, None, "xdg-desktop-por", 1000);
        p.exe_name = Some("xdg-desktop-portal-hyprland".into());
        assert_eq!(p.display_name(), "xdg-desktop-portal-hyprland");

        let mut q = proc(11, None, "fish", 1000);
        q.exe_name = Some("other".into());
        assert_eq!(q.display_name(), "fish");
    }

    #[test]
    fn io_bps_distinguishes_unknown_from_zero() {
        let mut p = proc(5, None, "x", 1000);
        assert_eq!(p.io_bps(), Some(0));
        p.disk_read_bps = None;
        p.disk_write_bps = None;
        assert_eq!(p.io_bps(), None);
        p.disk_write_bps = Some(7);
        assert_eq!(p.io_bps(), Some(7));
    }

    #[test]
    fn disk_totals_prefer_physical_counters() {
        let d = DiskStats { read_bps: Some(10), write_bps: Some(1), ..Default::default() };
        let mut s = Snapshot { disks: vec![d.clone(), d], ..Default::default() };
        assert_eq!(s.disk_read_bps(), 20);
        s.disk_io = Some(DiskIo { read_bps: 3, write_bps: 4 });
        assert_eq!((s.disk_read_bps(), s.disk_write_bps()), (3, 4));
    }

    #[test]
    fn identity_includes_start_time() {
        let a = proc(42, None, "a", 1000);
        let mut b = a.clone();
        b.start_time += 1;
        assert_ne!(a.id(), b.id());
    }
}
