//! Parsers for `/proc` and helpers for `/sys`.
//!
//! Parsers are pure functions over text so they can be tested with fixed
//! samples; sysfs helpers take the sysfs root as a parameter so tests can
//! point them at a fake tree.

use std::collections::HashMap;
use std::path::Path;

use dotmon_core::model::ProcState;

/// Fields of `/proc/<pid>/stat` that sysinfo does not expose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stat {
    pub comm: String,
    pub state: ProcState,
    pub ppid: u32,
    pub flags: u32,
    /// CPU time spent in user mode, in clock ticks (field 14), all threads
    /// of the process, including the ones that already exited.
    pub utime: u64,
    /// CPU time spent in kernel mode, in clock ticks (field 15).
    pub stime: u64,
    pub num_threads: u32,
    /// Clock ticks since boot (field 22).
    pub starttime: u64,
}

/// `PF_KTHREAD` from `include/linux/sched.h`.
pub const PF_KTHREAD: u32 = 0x0020_0000;

impl Stat {
    pub fn is_kernel_thread(&self) -> bool {
        self.flags & PF_KTHREAD != 0
    }

    /// Total CPU time consumed so far (user + kernel), in clock ticks.
    pub fn cpu_ticks(&self) -> u64 {
        self.utime.saturating_add(self.stime)
    }
}

pub fn proc_state(c: char) -> ProcState {
    match c {
        'R' => ProcState::Running,
        'S' => ProcState::Sleeping,
        'I' => ProcState::Idle,
        'D' => ProcState::DiskSleep,
        'T' | 't' => ProcState::Stopped,
        'Z' | 'X' => ProcState::Zombie,
        _ => ProcState::Unknown,
    }
}

/// Parses one `/proc/<pid>/stat` line. `comm` may contain spaces and
/// parentheses, so the fields are located from the *last* `)`.
pub fn parse_stat(line: &str) -> Option<Stat> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    if close < open {
        return None;
    }
    let comm = line[open + 1..close].to_string();
    let rest: Vec<&str> = line[close + 1..].split_whitespace().collect();
    // rest[0] is field 3 (state); field N is rest[N - 3].
    let field = |n: usize| rest.get(n - 3).copied();
    Some(Stat {
        comm,
        state: proc_state(field(3)?.chars().next()?),
        ppid: field(4)?.parse().ok()?,
        flags: field(9)?.parse().ok()?,
        utime: field(14)?.parse().ok()?,
        stime: field(15)?.parse().ok()?,
        num_threads: field(20)?.parse().ok()?,
        starttime: field(22)?.parse().ok()?,
    })
}

/// Parses `/proc/meminfo` into bytes.
pub fn parse_meminfo(text: &str) -> HashMap<String, u64> {
    text.lines()
        .filter_map(|l| {
            let (key, rest) = l.split_once(':')?;
            let mut parts = rest.split_whitespace();
            let value: u64 = parts.next()?.parse().ok()?;
            let bytes = if parts.next() == Some("kB") { value * 1024 } else { value };
            Some((key.trim().to_string(), bytes))
        })
        .collect()
}

/// `free`'s "buff/cache": Buffers + Cached + SReclaimable.
pub fn buff_cache(meminfo: &HashMap<String, u64>) -> Option<u64> {
    let cached = *meminfo.get("Cached")?;
    Some(
        cached
            + meminfo.get("Buffers").copied().unwrap_or(0)
            + meminfo.get("SReclaimable").copied().unwrap_or(0),
    )
}

/// Cumulative counters of one block device from `/proc/diskstats`, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskCounters {
    pub read_bytes: u64,
    pub written_bytes: u64,
}

/// Parses `/proc/diskstats`. Sector counts there are always in 512-byte
/// units, whatever the device's real sector size.
pub fn parse_diskstats(text: &str) -> HashMap<String, DiskCounters> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let name = f.get(2)?;
            let read: u64 = f.get(5)?.parse().ok()?;
            let written: u64 = f.get(9)?.parse().ok()?;
            Some((name.to_string(), DiskCounters { read_bytes: read * 512, written_bytes: written * 512 }))
        })
        .collect()
}

/// Cumulative counters of one interface from `/proc/net/dev`, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetCounters {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

pub fn parse_net_dev(text: &str) -> HashMap<String, NetCounters> {
    text.lines()
        .skip(2)
        .filter_map(|l| {
            let (name, rest) = l.split_once(':')?;
            let f: Vec<&str> = rest.split_whitespace().collect();
            Some((
                name.trim().to_string(),
                NetCounters { rx_bytes: f.first()?.parse().ok()?, tx_bytes: f.get(8)?.parse().ok()? },
            ))
        })
        .collect()
}

/// Cgroup v2 path from `/proc/<pid>/cgroup` (`0::/user.slice/...`).
pub fn parse_cgroup(text: &str) -> Option<String> {
    text.lines().find_map(|l| l.strip_prefix("0::")).map(str::to_string)
}

/// Rate in bytes/second from two cumulative readings. A counter that went
/// backwards (device removed and re-added, driver reset) yields `None`
/// rather than a bogus huge value.
pub fn rate(prev: u64, cur: u64, dt_s: f64) -> Option<u64> {
    if cur < prev || dt_s <= 0.0 {
        return None;
    }
    Some(((cur - prev) as f64 / dt_s).round() as u64)
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

/// Whole physical block devices (`/sys/block/*` not under
/// `devices/virtual`): excludes loop, zram, device-mapper and md, and,
/// because partitions are not listed in `/sys/block`, never counts a
/// partition and its parent twice.
pub fn physical_block_devices(sys_root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(sys_root.join("block")) else { return Vec::new() };
    let mut out: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| {
            std::fs::read_link(e.path())
                .map(|target| !target.to_string_lossy().contains("/virtual/"))
                .unwrap_or(false)
        })
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// Active physical network interfaces: they have a `device` link (so not
/// lo, bridges, veth, tun/wireguard…) and `operstate` is `up`. Excluding
/// virtual interfaces avoids counting VPN or container traffic twice.
pub fn active_physical_interfaces(sys_root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(sys_root.join("class/net")) else { return Vec::new() };
    let mut out: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| {
            let p = e.path();
            p.join("device").exists() && read_trimmed(&p.join("operstate")).as_deref() == Some("up")
        })
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// Runtime PM status of a PCI device (`active`, `suspended`, …). Reading
/// it never wakes the device.
pub fn runtime_status(device_dir: &Path) -> Option<String> {
    read_trimmed(&device_dir.join("power/runtime_status"))
}

pub fn read_u64(path: &Path) -> Option<u64> {
    read_trimmed(path)?.parse().ok()
}

/// Looks up a human name in a `pci.ids` database.
pub fn pci_name(db: &str, vendor: u16, device: u16) -> Option<(String, String)> {
    let vendor_hex = format!("{vendor:04x}");
    let device_hex = format!("{device:04x}");
    let mut vendor_name: Option<String> = None;
    for line in db.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if !line.starts_with('\t') {
            if vendor_name.is_some() {
                break; // left our vendor's block without finding the device
            }
            if let Some(name) = line.strip_prefix(&vendor_hex).and_then(|r| r.strip_prefix("  ")) {
                vendor_name = Some(name.to_string());
            }
            continue;
        }
        if let Some(v) = &vendor_name
            && !line.starts_with("\t\t")
            && let Some(name) = line[1..].strip_prefix(&device_hex).and_then(|r| r.strip_prefix("  "))
        {
            return Some((v.clone(), name.to_string()));
        }
    }
    None
}

/// "GB206M [GeForce RTX 5070 Max-Q / Mobile]" → "GeForce RTX 5070 Max-Q / Mobile".
pub fn marketing_name(device_name: &str) -> &str {
    match (device_name.find('['), device_name.rfind(']')) {
        (Some(a), Some(b)) if b > a => &device_name[a + 1..b],
        _ => device_name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_with_spaces_and_parens_in_comm() {
        let line = "4242 (Web (Content) x) S 4000 4000 4000 0 -1 4194560 1 0 0 0 10 5 0 0 20 0 27 0 123456 1000 50 0";
        let s = parse_stat(line).unwrap();
        assert_eq!(s.comm, "Web (Content) x");
        assert_eq!(s.state, ProcState::Sleeping);
        assert_eq!(s.ppid, 4000);
        assert_eq!(s.num_threads, 27);
        assert_eq!(s.starttime, 123456);
        assert_eq!((s.utime, s.stime, s.cpu_ticks()), (10, 5, 15));
        assert!(!s.is_kernel_thread());
    }

    #[test]
    fn stat_kernel_thread_flag() {
        let line = "12 (kworker/0:1-events) I 2 0 0 0 -1 69238880 0 0 0 0 0 3 0 0 20 0 1 0 15 0 0";
        let s = parse_stat(line).unwrap();
        assert!(s.is_kernel_thread());
        assert_eq!(s.state, ProcState::Idle);
        assert_eq!(s.ppid, 2);
    }

    #[test]
    fn stat_rejects_truncated_lines() {
        assert!(parse_stat("1 (systemd) S 0 1").is_none());
        assert!(parse_stat("garbage").is_none());
    }

    #[test]
    fn meminfo_and_buff_cache() {
        let m = parse_meminfo(
            "MemTotal:       32000000 kB\nMemAvailable:   20000000 kB\nBuffers:            1000 kB\nCached:          5000000 kB\nSReclaimable:     400000 kB\nHugePages_Total:       0\n",
        );
        assert_eq!(m["MemTotal"], 32_000_000 * 1024);
        assert_eq!(m["HugePages_Total"], 0);
        assert_eq!(buff_cache(&m), Some((1000 + 5_000_000 + 400_000) * 1024));
        assert_eq!(buff_cache(&HashMap::new()), None);
    }

    #[test]
    fn diskstats_in_bytes() {
        let d = parse_diskstats(
            " 259       0 nvme1n1 1000 0 2048 0 500 0 4096 0 0 0 0 0 0 0 0 0 0\n 259       1 nvme1n1p1 10 0 16 0 0 0 0 0 0 0 0\n",
        );
        assert_eq!(d["nvme1n1"], DiskCounters { read_bytes: 2048 * 512, written_bytes: 4096 * 512 });
        assert!(d.contains_key("nvme1n1p1"));
    }

    #[test]
    fn net_dev_counters() {
        let text = "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    lo:  1000      10    0    0    0     0          0         0     1000      10    0    0    0     0       0          0\n wlan0: 987654   900    0    0    0     0          0         0   123456     800    0    0    0     0       0          0\n";
        let n = parse_net_dev(text);
        assert_eq!(n["wlan0"], NetCounters { rx_bytes: 987_654, tx_bytes: 123_456 });
        assert_eq!(n["lo"].rx_bytes, 1000);
    }

    #[test]
    fn cgroup_v2_line() {
        assert_eq!(parse_cgroup("0::/user.slice/a.scope\n").as_deref(), Some("/user.slice/a.scope"));
        assert_eq!(parse_cgroup("1:name=systemd:/x\n"), None);
    }

    #[test]
    fn rates_handle_resets() {
        assert_eq!(rate(100, 300, 2.0), Some(100));
        assert_eq!(rate(300, 100, 1.0), None);
        assert_eq!(rate(1, 2, 0.0), None);
    }

    #[test]
    fn pci_ids_lookup() {
        let db = "# comment\n1002  Advanced Micro Devices, Inc. [AMD/ATI]\n\t1114  Strix [Radeon 880M / 890M]\n\t\t1234 5678  Subsystem\n10de  NVIDIA Corporation\n\t2d18  GB206M [GeForce RTX 5070 Max-Q / Mobile]\n";
        let (v, d) = pci_name(db, 0x10de, 0x2d18).unwrap();
        assert_eq!(v, "NVIDIA Corporation");
        assert_eq!(marketing_name(&d), "GeForce RTX 5070 Max-Q / Mobile");
        assert_eq!(marketing_name(&pci_name(db, 0x1002, 0x1114).unwrap().1), "Radeon 880M / 890M");
        assert!(pci_name(db, 0x1002, 0x9999).is_none());
        assert!(pci_name(db, 0x8086, 0x1).is_none());
    }

    /// Builds a fake sysfs tree with symlinks like the kernel's.
    fn fake_sysfs() -> std::path::PathBuf {
        use std::os::unix::fs::symlink;
        let root = std::env::temp_dir().join(format!(
            "dotmon-sysfs-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let mk = |p: &str| std::fs::create_dir_all(root.join(p)).unwrap();
        mk("devices/pci0000:00/nvme/nvme1n1");
        mk("devices/virtual/block/zram0");
        mk("devices/virtual/block/loop0");
        mk("block");
        symlink("../devices/pci0000:00/nvme/nvme1n1", root.join("block/nvme1n1")).unwrap();
        symlink("../devices/virtual/block/zram0", root.join("block/zram0")).unwrap();
        symlink("../devices/virtual/block/loop0", root.join("block/loop0")).unwrap();
        for (name, phys, state) in
            [("wlan0", true, "up"), ("eno1", true, "down"), ("lo", false, "unknown"), ("virbr0", false, "up")]
        {
            let d = root.join("class/net").join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("operstate"), format!("{state}\n")).unwrap();
            if phys {
                std::fs::create_dir_all(d.join("device")).unwrap();
            }
        }
        root
    }

    #[test]
    fn sysfs_filters_physical_devices() {
        let root = fake_sysfs();
        assert_eq!(physical_block_devices(&root), vec!["nvme1n1"]);
        assert_eq!(active_physical_interfaces(&root), vec!["wlan0"]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
