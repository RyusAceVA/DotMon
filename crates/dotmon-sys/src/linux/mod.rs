//! Real metrics from the running Linux system.
//!
//! One [`LinuxSource`] lives on the engine thread for the whole session: the
//! sysinfo `System`, the user table, the disk and sensor lists and the
//! previous counter readings are kept between samples, so rates are always
//! computed against the previous sample with the *measured* elapsed time.
//!
//! Cost control:
//! - per sample: CPU usage + frequency, memory, the process table (without
//!   per-thread entries; per-process CPU from `/proc/<pid>/stat`, see [`cpu`]), one temperature sensor, `/proc/diskstats`,
//!   `/proc/net/dev`, statvfs of the selected mounts, GPU (if awake);
//! - occasionally: interface list (10 samples), block devices and mounts
//!   (30), user table (60);
//! - once per process: cgroup; once per session: CPU model, host name,
//!   kernel, GPU list.
//!
//! No values are invented: anything that cannot be read is `None` and the UI
//! shows it as unavailable.

pub mod control;
pub mod cpu;
pub mod gpu;
pub mod procfs;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dotmon_core::model::{
    CpuStats, DiskIo, DiskStats, MemoryStats, NetStats, ProcessId, ProcessInfo, Snapshot,
};
use dotmon_core::source::MetricsSource;
use sysinfo::{
    Components, CpuRefreshKind, DiskRefreshKind, Disks, ProcessRefreshKind, ProcessesToUpdate, System,
    UpdateKind, Users,
};

pub use control::LinuxController;
use cpu::CpuTracker;
use procfs::{DiskCounters, NetCounters};

const RESCAN_NET_EVERY: u64 = 10;
const RESCAN_DISKS_EVERY: u64 = 30;
const RESCAN_USERS_EVERY: u64 = 60;
const UNAVAILABLE: &str = "Indisponible";

/// Labels (sysinfo: "<hwmon name> <label>") of the CPU package sensor, in
/// order of preference.
const CPU_SENSOR_HINTS: &[&str] =
    &["Tctl", "Tdie", "Package id 0", "k10temp", "coretemp", "zenpower", "cpu_thermal"];

#[derive(Debug)]
pub struct InitError(pub String);

impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for InitError {}

/// A mount shown in the storage card: one per block device.
#[derive(Debug, Clone)]
struct Mount {
    /// Index in `Disks`.
    index: usize,
    /// Name in `/proc/diskstats` (resolved through /dev symlinks).
    stat_name: Option<String>,
}

pub struct LinuxSource {
    sys: System,
    users: Users,
    disks: Disks,
    components: Components,
    cpu_sensor: Option<usize>,
    gpus: gpu::GpuMonitor,
    sys_root: PathBuf,
    proc_root: PathBuf,

    hostname: String,
    kernel: String,
    cpu_model: String,
    my_uid: u32,
    ticks_per_s: u64,
    /// Per-process CPU: own CPU time over the real elapsed time.
    cpu_tracker: CpuTracker,

    seq: u64,
    last: Instant,
    mounts: Vec<Mount>,
    physical_disks: Vec<String>,
    interfaces: Vec<String>,
    prev_disk: HashMap<String, DiskCounters>,
    prev_net: HashMap<String, NetCounters>,
    cgroups: HashMap<ProcessId, Option<String>>,
}

fn process_refresh_kind() -> ProcessRefreshKind {
    // `nothing()` includes tasks (threads listed as processes): disable them.
    ProcessRefreshKind::nothing()
        .without_tasks()
        .with_memory()
        .with_disk_usage()
        .with_user(UpdateKind::OnlyIfNotSet)
        .with_exe(UpdateKind::OnlyIfNotSet)
        .with_cmd(UpdateKind::OnlyIfNotSet)
}

impl LinuxSource {
    pub fn new() -> Result<Self, InitError> {
        Self::with_roots(Path::new("/sys"), Path::new("/proc"))
    }

    fn with_roots(sys_root: &Path, proc_root: &Path) -> Result<Self, InitError> {
        let err = |what: &str, e: sysinfo::Error| InitError(format!("{what} : {e:?}"));
        let mut sys = System::new().map_err(|e| err("sysinfo", e))?;
        sys.refresh_cpu_list(CpuRefreshKind::nothing().with_usage().with_frequency());
        sys.refresh_memory();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, process_refresh_kind());

        let users = Users::new_with_refreshed_list().map_err(|e| err("utilisateurs", e))?;
        let disks = Disks::new_with_refreshed_list_specifics(DiskRefreshKind::nothing().with_storage())
            .map_err(|e| err("disques", e))?;
        let components = Components::new_with_refreshed_list().map_err(|e| err("capteurs", e))?;
        let cpu_sensor =
            CPU_SENSOR_HINTS.iter().find_map(|hint| components.iter().position(|c| c.label().contains(hint)));

        let logical_cpus = sys.cpus().len();
        let cpu_model = sys
            .cpus()
            .first()
            .map(|c| c.brand().trim().to_string())
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| UNAVAILABLE.into());
        let my_uid = {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(proc_root.join("self")).map(|m| m.uid()).unwrap_or(u32::MAX)
        };

        let mut src = Self {
            sys,
            users,
            disks,
            components,
            cpu_sensor,
            gpus: gpu::GpuMonitor::new(gpu::discover(sys_root)),
            sys_root: sys_root.to_path_buf(),
            proc_root: proc_root.to_path_buf(),
            hostname: System::host_name().unwrap_or_else(|_| UNAVAILABLE.into()),
            kernel: System::kernel_version().unwrap_or_else(|_| UNAVAILABLE.into()),
            cpu_model,
            my_uid,
            ticks_per_s: rustix::param::clock_ticks_per_second().max(1),
            cpu_tracker: CpuTracker::new(logical_cpus, rustix::param::clock_ticks_per_second()),
            seq: 0,
            last: Instant::now(),
            mounts: Vec::new(),
            physical_disks: Vec::new(),
            interfaces: Vec::new(),
            prev_disk: HashMap::new(),
            prev_net: HashMap::new(),
            cgroups: HashMap::new(),
        };
        src.seed_cpu_baseline();
        src.rescan_disks(false);
        src.interfaces = procfs::active_physical_interfaces(&src.sys_root);
        src.prev_disk = src.read_diskstats();
        src.prev_net = src.read_net_dev();
        Ok(src)
    }

    /// First reading of every process' CPU time, so that processes that
    /// already exist have a reference at the first sample. Only processes
    /// born afterwards start without a CPU figure.
    fn seed_cpu_baseline(&mut self) {
        for pid in self.sys.processes().keys() {
            let pid = pid.as_u32();
            let Some(stat) = std::fs::read_to_string(self.proc_root.join(pid.to_string()).join("stat"))
                .ok()
                .and_then(|l| procfs::parse_stat(&l))
            else {
                continue;
            };
            let id = ProcessId { pid, start_time: stat.starttime };
            self.cpu_tracker.observe(id, stat.cpu_ticks(), Instant::now());
        }
    }

    pub fn gpu_devices(&self) -> &[gpu::GpuDevice] {
        self.gpus.devices()
    }

    fn read_diskstats(&self) -> HashMap<String, DiskCounters> {
        std::fs::read_to_string(self.proc_root.join("diskstats"))
            .map(|t| procfs::parse_diskstats(&t))
            .unwrap_or_default()
    }

    fn read_net_dev(&self) -> HashMap<String, NetCounters> {
        std::fs::read_to_string(self.proc_root.join("net/dev"))
            .map(|t| procfs::parse_net_dev(&t))
            .unwrap_or_default()
    }

    /// Re-reads mounts and keeps one per block device (btrfs subvolumes and
    /// bind mounts share a device: the shortest mount point wins, e.g. `/`).
    fn rescan_disks(&mut self, refresh_list: bool) {
        if refresh_list {
            self.disks.refresh_specifics(true, DiskRefreshKind::nothing().with_storage());
        }
        self.physical_disks = procfs::physical_block_devices(&self.sys_root);
        let mut best: HashMap<String, usize> = HashMap::new();
        for (i, d) in self.disks.iter().enumerate() {
            let dev = d.name().to_string_lossy();
            if !dev.starts_with("/dev/") || dev.starts_with("/dev/loop") || d.total_space() == 0 {
                continue;
            }
            let mount_len = d.mount_point().as_os_str().len();
            best.entry(dev.into_owned())
                .and_modify(|cur| {
                    if mount_len < self.disks[*cur].mount_point().as_os_str().len() {
                        *cur = i;
                    }
                })
                .or_insert(i);
        }
        let mut mounts: Vec<Mount> = best
            .into_iter()
            .map(|(dev, index)| {
                let stat_name = std::fs::canonicalize(&dev)
                    .ok()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
                Mount { index, stat_name }
            })
            .collect();
        mounts.sort_by_key(|m| self.disks[m.index].mount_point().to_path_buf());
        self.mounts = mounts;
    }

    fn user_name(&self, uid: u32) -> String {
        self.users
            .iter()
            .find(|u| **u.id() == uid)
            .map(|u| u.name().to_string())
            .unwrap_or_else(|| uid.to_string())
    }

    fn cpu_stats(&mut self) -> CpuStats {
        self.sys.refresh_cpu_specifics(CpuRefreshKind::nothing().with_usage().with_frequency());
        let cpus = self.sys.cpus();
        let freqs: Vec<u64> = cpus.iter().map(|c| c.frequency()).filter(|f| *f > 0).collect();
        let temperature_c = self.cpu_sensor.and_then(|i| {
            let c = &mut self.components[i];
            c.refresh();
            c.temperature()
        });
        CpuStats {
            model: format!("{} · {} THREADS", self.cpu_model, cpus.len()),
            usage: self.sys.global_cpu_usage(),
            per_core: cpus.iter().map(|c| c.usage()).collect(),
            frequency_mhz: (!freqs.is_empty())
                .then(|| (freqs.iter().sum::<u64>() / freqs.len() as u64) as u32),
            temperature_c,
            load_avg: System::load_average().ok().map(|l| [l.one as f32, l.five as f32, l.fifteen as f32]),
        }
    }

    fn memory_stats(&mut self) -> MemoryStats {
        self.sys.refresh_memory();
        let meminfo = std::fs::read_to_string(self.proc_root.join("meminfo"))
            .map(|t| procfs::parse_meminfo(&t))
            .unwrap_or_default();
        MemoryStats {
            total: self.sys.total_memory(),
            used: self.sys.used_memory(),
            available: self.sys.available_memory(),
            cached: procfs::buff_cache(&meminfo),
            swap_total: self.sys.total_swap(),
            swap_used: self.sys.used_swap(),
        }
    }

    fn processes(&mut self, dt_s: f64) -> Vec<ProcessInfo> {
        self.sys.refresh_processes_specifics(ProcessesToUpdate::All, true, process_refresh_kind());
        let mut out = Vec::with_capacity(self.sys.processes().len());
        let mut alive: HashSet<ProcessId> = HashSet::with_capacity(out.capacity());

        for (pid, p) in self.sys.processes() {
            let pid = pid.as_u32();
            // Identity, parent, state, threads and kernel flag come from one
            // read of /proc/<pid>/stat; a process that exited meanwhile is skipped.
            let Some(stat) = std::fs::read_to_string(self.proc_root.join(pid.to_string()).join("stat"))
                .ok()
                .and_then(|l| procfs::parse_stat(&l))
            else {
                continue;
            };
            let id = ProcessId { pid, start_time: stat.starttime };
            // Taken right after the read: the interval is measured per
            // process, from one reading of its own CPU time to the next.
            let cpu = self.cpu_tracker.observe(id, stat.cpu_ticks(), Instant::now());
            alive.insert(id);
            let uid = p.user_id().map(|u| **u).unwrap_or(u32::MAX);
            let cgroup = self
                .cgroups
                .entry(id)
                .or_insert_with(|| {
                    std::fs::read_to_string(self.proc_root.join(pid.to_string()).join("cgroup"))
                        .ok()
                        .and_then(|t| procfs::parse_cgroup(&t))
                })
                .clone();
            // /proc/<pid>/io is only readable for our own processes; sysinfo
            // reports 0 otherwise, which would be a false "no I/O".
            let io_readable = self.my_uid == 0 || uid == self.my_uid;
            let usage = p.disk_usage();
            let per_s = |bytes: u64| (bytes as f64 / dt_s.max(1e-3)).round() as u64;
            let cmd: Vec<String> = p.cmd().iter().map(|a| a.to_string_lossy().into_owned()).collect();
            let kernel_thread = stat.is_kernel_thread();
            out.push(ProcessInfo {
                pid,
                ppid: (stat.ppid != 0).then_some(stat.ppid),
                name: stat.comm.clone(),
                exe_name: p.exe().and_then(|e| e.file_name()).map(|n| n.to_string_lossy().into_owned()),
                cmdline: if cmd.is_empty() { format!("[{}]", stat.comm) } else { cmd.join(" ") },
                user: self.user_name(uid),
                uid,
                state: stat.state,
                cpu,
                memory: p.memory(),
                threads: stat.num_threads,
                start_time: stat.starttime,
                started_s: stat.starttime / self.ticks_per_s,
                disk_read_bps: io_readable.then(|| per_s(usage.read_bytes)),
                disk_write_bps: io_readable.then(|| per_s(usage.written_bytes)),
                kernel_thread,
                cgroup,
            });
        }
        self.cgroups.retain(|id, _| alive.contains(id));
        self.cpu_tracker.retain(&alive);
        out.sort_by_key(|p| p.pid);
        out
    }

    fn disk_stats(&mut self, dt_s: f64) -> (Vec<DiskStats>, Option<DiskIo>) {
        self.disks.refresh_specifics(false, DiskRefreshKind::nothing().with_storage());
        let cur = self.read_diskstats();
        let rate_of = |name: &str| -> Option<(u64, u64)> {
            let (p, c) = (self.prev_disk.get(name)?, cur.get(name)?);
            Some((
                procfs::rate(p.read_bytes, c.read_bytes, dt_s)?,
                procfs::rate(p.written_bytes, c.written_bytes, dt_s)?,
            ))
        };
        let rows = self
            .mounts
            .iter()
            .filter_map(|m| {
                let d = self.disks.get(m.index)?;
                let rates = m.stat_name.as_deref().and_then(rate_of);
                Some(DiskStats {
                    name: d.name().to_string_lossy().trim_start_matches("/dev/").to_string(),
                    mount_point: d.mount_point().to_string_lossy().into_owned(),
                    filesystem: d.file_system().to_string_lossy().into_owned(),
                    total: d.total_space(),
                    used: d.total_space().saturating_sub(d.available_space()),
                    read_bps: rates.map(|r| r.0),
                    write_bps: rates.map(|r| r.1),
                })
            })
            .collect();
        // Totals over whole physical devices only (no partitions, no zram/dm).
        let mut total = DiskIo::default();
        let mut any = false;
        for dev in &self.physical_disks {
            if let Some((r, w)) = rate_of(dev) {
                total.read_bps += r;
                total.write_bps += w;
                any = true;
            }
        }
        self.prev_disk = cur;
        (rows, any.then_some(total))
    }

    fn net_stats(&mut self, dt_s: f64) -> Vec<NetStats> {
        let cur = self.read_net_dev();
        let out = self
            .interfaces
            .iter()
            .filter_map(|iface| {
                let c = cur.get(iface)?;
                let p = self.prev_net.get(iface);
                Some(NetStats {
                    interface: iface.clone(),
                    // An interface that just appeared has no previous reading: 0 for one tick.
                    rx_bps: p.and_then(|p| procfs::rate(p.rx_bytes, c.rx_bytes, dt_s)).unwrap_or(0),
                    tx_bps: p.and_then(|p| procfs::rate(p.tx_bytes, c.tx_bytes, dt_s)).unwrap_or(0),
                    rx_total: c.rx_bytes,
                    tx_total: c.tx_bytes,
                })
            })
            .collect();
        self.prev_net = cur;
        out
    }
}

impl MetricsSource for LinuxSource {
    fn name(&self) -> &str {
        "LINUX"
    }

    fn sample(&mut self) -> Snapshot {
        // CPU percentages need a minimum window between two readings.
        let since = self.last.elapsed();
        if since < sysinfo::MINIMUM_CPU_UPDATE_INTERVAL {
            std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL - since);
        }
        let now = Instant::now();
        let dt_s = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.seq += 1;

        if self.seq.is_multiple_of(RESCAN_NET_EVERY) {
            self.interfaces = procfs::active_physical_interfaces(&self.sys_root);
        }
        if self.seq.is_multiple_of(RESCAN_DISKS_EVERY) {
            self.rescan_disks(true);
        }
        if self.seq.is_multiple_of(RESCAN_USERS_EVERY) {
            self.users.refresh();
        }

        let cpu = self.cpu_stats();
        let memory = self.memory_stats();
        let processes = self.processes(dt_s);
        let (disks, disk_io) = self.disk_stats(dt_s);
        let networks = self.net_stats(dt_s);
        let gpus = self.gpus.sample(now);

        Snapshot {
            seq: self.seq,
            hostname: self.hostname.clone(),
            kernel: self.kernel.clone(),
            uptime_s: System::uptime().unwrap_or(0),
            cpu,
            memory,
            gpus,
            disks,
            disk_io,
            networks,
            processes,
        }
    }

    fn set_interval_hint(&mut self, _interval: Duration) {
        // Rates use the measured elapsed time; nothing to do.
    }
}

#[cfg(test)]
mod tests;
