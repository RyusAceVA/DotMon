//! Live tests: sample the machine running the tests and cross-check the
//! results against the kernel interfaces read independently.

use std::collections::HashSet;
use std::time::Duration;

use super::*;
use dotmon_core::protection::ProtectionReason;
use dotmon_core::source::{ControlError, EndMode, ProcessController};

fn source() -> LinuxSource {
    LinuxSource::new().expect("Linux backend must initialise on Linux")
}

fn meminfo_kib(key: &str) -> u64 {
    let text = std::fs::read_to_string("/proc/meminfo").unwrap();
    procfs::parse_meminfo(&text)[key] / 1024
}

/// (busy, total) jiffies of the aggregate `cpu` line of /proc/stat.
fn proc_stat_cpu() -> (u64, u64) {
    let text = std::fs::read_to_string("/proc/stat").unwrap();
    let line = text.lines().next().unwrap();
    let v: Vec<u64> = line.split_whitespace().skip(1).map(|x| x.parse().unwrap()).collect();
    // user nice system idle iowait irq softirq steal (guest is included in user)
    let idle = v[3] + v[4];
    let total: u64 = v[..8].iter().sum();
    (total - idle, total)
}

#[test]
fn samples_advance_and_stay_sane() {
    let mut s = source();
    let mut prev = s.sample();
    for _ in 0..5 {
        let snap = s.sample();
        assert_eq!(snap.seq, prev.seq + 1);
        assert!((0.0..=100.0).contains(&snap.cpu.usage), "cpu {}", snap.cpu.usage);
        assert!(snap.cpu.per_core.iter().all(|c| (0.0..=100.0).contains(c)));
        assert!(snap.memory.used <= snap.memory.total);
        assert!(
            snap.processes.iter().all(|p| p.cpu.is_none_or(|c| c.is_finite() && (0.0..=100.0).contains(&c)))
        );
        assert!(snap.uptime_s >= prev.uptime_s);
        prev = snap;
    }
}

#[test]
fn cpu_count_matches_proc_stat() {
    let mut s = source();
    let snap = s.sample();
    let text = std::fs::read_to_string("/proc/stat").unwrap();
    let online = text
        .lines()
        .filter(|l| l.starts_with("cpu") && l.as_bytes().get(3).is_some_and(u8::is_ascii_digit))
        .count();
    assert_eq!(snap.cpu.per_core.len(), online);
    assert!(snap.cpu.model.contains("THREADS"));
}

#[test]
fn cpu_usage_close_to_proc_stat() {
    let mut s = source();
    s.sample();
    let (b0, t0) = proc_stat_cpu();
    std::thread::sleep(Duration::from_millis(1000));
    let (b1, t1) = proc_stat_cpu();
    let snap = s.sample();
    let reference = (b1 - b0) as f32 / (t1 - t0).max(1) as f32 * 100.0;
    assert!(
        (snap.cpu.usage - reference).abs() < 15.0,
        "dotmon {:.1} % vs /proc/stat {:.1} %",
        snap.cpu.usage,
        reference
    );
}

#[test]
fn memory_matches_meminfo() {
    let mut s = source();
    let snap = s.sample();
    assert_eq!(snap.memory.total / 1024, meminfo_kib("MemTotal"));
    // `available` moves between the two reads; allow 64 MiB of drift.
    let avail = meminfo_kib("MemAvailable") as i64;
    assert!(((snap.memory.available / 1024) as i64 - avail).abs() < 64 * 1024);
    assert_eq!(snap.memory.used, snap.memory.total - snap.memory.available);
    assert!(snap.memory.cached.is_some());
    assert_eq!(snap.memory.swap_total / 1024, meminfo_kib("SwapTotal"));
}

#[test]
fn process_table_matches_proc() {
    let mut s = source();
    let snap = s.sample();
    let listed: HashSet<u32> = std::fs::read_dir("/proc")
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
        .collect();
    let ours: HashSet<u32> = snap.processes.iter().map(|p| p.pid).collect();
    // Only processes (thread-group leaders) are listed in /proc's root, so
    // any thread wrongly reported as a process would show up here.
    let foreign: Vec<_> = ours.difference(&listed).collect();
    assert!(foreign.len() <= 3, "pids not in /proc (threads?): {foreign:?}");
    let missing = listed.difference(&ours).count();
    assert!(missing <= listed.len() / 20 + 3, "{missing} of {} processes missing", listed.len());
    assert_eq!(ours.len(), snap.processes.len(), "no duplicates");
}

#[test]
fn self_process_is_accurate() {
    let mut s = source();
    let snap = s.sample();
    let me = std::process::id();
    let p = snap.processes.iter().find(|p| p.pid == me).expect("own process listed");
    let stat = procfs::parse_stat(&std::fs::read_to_string("/proc/self/stat").unwrap()).unwrap();
    assert_eq!(p.ppid, Some(stat.ppid));
    assert_eq!(p.start_time, stat.starttime);
    assert_eq!(p.started_s, stat.starttime / rustix::param::clock_ticks_per_second());
    assert_eq!(p.uid, s.my_uid);
    assert!(p.memory > 0);
    assert!(p.threads >= 1);
    assert!(!p.kernel_thread);
    assert!(p.disk_read_bps.is_some(), "own I/O is readable");
    assert!(p.cgroup.is_some());
}

#[test]
fn init_and_kernel_threads() {
    let mut s = source();
    let snap = s.sample();
    let init = snap.processes.iter().find(|p| p.pid == 1).expect("pid 1");
    assert_eq!(init.ppid, None);
    assert!(!init.kernel_thread);
    assert_eq!(init.uid, 0);
    if s.my_uid != 0 {
        assert_eq!(init.disk_read_bps, None, "root's I/O is not readable: unavailable, not zero");
    }
    // kthreadd's children are kernel threads.
    if snap.processes.iter().any(|p| p.pid == 2) {
        assert!(snap.processes.iter().filter(|p| p.ppid == Some(2)).all(|p| p.kernel_thread));
    }
}

#[test]
fn disks_are_deduplicated() {
    let mut s = source();
    let snap = s.sample();
    let names: Vec<&str> = snap.disks.iter().map(|d| d.name.as_str()).collect();
    let unique: HashSet<&str> = names.iter().copied().collect();
    assert_eq!(names.len(), unique.len(), "one row per device: {names:?}");
    // Only a root filesystem backed by a block device is expected in the list: in a
    // container or build chroot "/" is an overlay or bind mount, which is not a disk.
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    let root_is_block_device = mounts
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            Some((f.next()?, f.next()?))
        })
        .any(|(source, mount_point)| mount_point == "/" && source.starts_with("/dev/"));
    if root_is_block_device {
        assert!(snap.disks.iter().any(|d| d.mount_point == "/"), "root filesystem listed: {names:?}");
    }
    assert!(snap.disks.iter().all(|d| d.used <= d.total));
    if !s.physical_disks.is_empty() {
        assert!(snap.disk_io.is_some());
    }
    assert!(s.physical_disks.iter().all(|d| !d.starts_with("zram") && !d.starts_with("loop")));
}

#[test]
fn network_interfaces_are_physical() {
    let mut s = source();
    let snap = s.sample();
    assert!(snap.networks.iter().all(|n| n.interface != "lo"));
    let dev = procfs::parse_net_dev(&std::fs::read_to_string("/proc/net/dev").unwrap());
    for n in &snap.networks {
        assert!(dev[&n.interface].rx_bytes >= n.rx_total, "counters only grow");
    }
}

#[test]
fn gpus_never_report_fake_values() {
    let mut s = source();
    let snap = s.sample();
    assert_eq!(snap.gpus.len(), s.gpu_devices().len());
    for g in &snap.gpus {
        if g.state != dotmon_core::model::GpuState::Active {
            assert_eq!(
                (g.usage, g.vram_used, g.temperature_c, g.power_w, g.soc_power_w),
                (None, None, None, None, None)
            );
        }
        if let (Some(u), Some(t)) = (g.vram_used, g.vram_total) {
            assert!(u <= t);
        }
        assert!(!g.name.is_empty());
    }
}

#[test]
fn stable_over_many_cycles() {
    let mut s = source();
    for _ in 0..15 {
        let snap = s.sample();
        assert!(!snap.processes.is_empty());
        assert!(snap.cpu.usage.is_finite());
        assert!(snap.net_rx_bps() < 100 * 1024 * 1024 * 1024, "absurd rate means a counter bug");
        assert!(snap.disk_read_bps() < 100 * 1024 * 1024 * 1024);
    }
    // Per-process caches are pruned with the process table.
    assert!(s.cgroups.len() <= s.sys.processes().len());
}

fn busy_child() -> std::process::Child {
    std::process::Command::new("sh")
        .args(["-c", "while :; do :; done"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .expect("sh")
}

#[test]
fn new_process_shows_no_cpu_then_a_measure_consistent_with_wall_clock() {
    let mut s = source();
    s.sample();
    let mut child = busy_child();
    let pid = child.id();
    let first = s.sample();
    let p = first.processes.iter().find(|p| p.pid == pid).expect("child listed");
    assert_eq!(p.cpu, None, "born since the previous sample: no earlier reading, not 0 %");

    let ncpu = first.cpu.per_core.len() as f32;
    let t0 = Instant::now();
    let ticks0 = proc_ticks(pid);
    let second = s.sample();
    let wall = t0.elapsed().as_secs_f32();
    let ticks1 = proc_ticks(pid);
    let cpu = second.processes.iter().find(|p| p.pid == pid).unwrap().cpu.expect("measured now");
    child.kill().unwrap();
    child.wait().unwrap();

    // One busy loop = one CPU = 100/ncpu % of the machine, whatever /proc/stat says.
    let expected = 100.0 / ncpu;
    assert!(cpu > expected * 0.5 && cpu <= expected * 1.1, "{cpu} % vs {expected} %");
    // Same quantity computed here from the process' own ticks and wall time
    // (the sample's window is a bit longer than ours: allow 10 %).
    let reference =
        (ticks1 - ticks0) as f32 / (rustix::param::clock_ticks_per_second() as f32 * wall * ncpu) * 100.0;
    assert!(reference > 0.0 && cpu <= reference * 1.1 + 0.2, "{cpu} % vs own reading {reference} %");
}

fn proc_ticks(pid: u32) -> u64 {
    let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    procfs::parse_stat(&text).unwrap().cpu_ticks()
}

#[test]
fn processes_present_at_startup_are_measured_from_the_first_sample() {
    // Init seeds a baseline: a process that already existed is not a "new" one.
    let mut s = source();
    let me = std::process::id();
    let snap = s.sample();
    let p = snap.processes.iter().find(|p| p.pid == me).unwrap();
    assert!(p.cpu.is_some(), "own process was there before the first sample");
    // Per-process state is pruned with the process table.
    assert!(s.cpu_tracker.len() <= snap.processes.len() + 8);
}

// ---- Ending real processes (phase 3) -----------------------------------------
//
// Targets are ONLY processes created here, by this test, as its own children
// (`sleep`). Nothing from the user's session is ever signalled.

use std::os::unix::process::ExitStatusExt;

/// A throw-away child process. Always killed and reaped on drop, even if an
/// assertion fails, so a failing test leaves nothing behind.
struct Sleeper {
    child: std::process::Child,
}

impl Sleeper {
    /// `sleep 300`; with `ignore_term`, a `sleep` that ignores SIGTERM
    /// (the shell sets the disposition, which `exec` preserves).
    fn spawn(ignore_term: bool) -> Self {
        let script = if ignore_term { "trap '' TERM; exec sleep 300" } else { "exec sleep 300" };
        let child = std::process::Command::new("sh").args(["-c", script]).spawn().expect("sh");
        let me = Sleeper { child };
        // Wait until the shell has become `sleep` (and so has set its signals).
        let comm = format!("/proc/{}/comm", me.pid());
        for _ in 0..200 {
            if std::fs::read_to_string(&comm).is_ok_and(|c| c.trim() == "sleep") {
                return me;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("child never became `sleep`");
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Identity as the sampler reports it (PID + start time in ticks).
    fn id(&self) -> ProcessId {
        let text = std::fs::read_to_string(format!("/proc/{}/stat", self.pid())).unwrap();
        ProcessId { pid: self.pid(), start_time: procfs::parse_stat(&text).unwrap().starttime }
    }

    /// Exit signal if the child ended by a signal (reaps it).
    fn wait_signal(&mut self) -> Option<i32> {
        self.child.wait().unwrap().signal()
    }

    fn is_alive(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }
}

impl Drop for Sleeper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_exited(c: &mut LinuxController, id: ProcessId) -> bool {
    for _ in 0..200 {
        if c.has_exited(id) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn sigterm_ends_a_cooperative_process_and_the_exit_is_observed() {
    let mut victim = Sleeper::spawn(false);
    let id = victim.id();
    let mut c = LinuxController::new();
    assert_eq!(c.end(id, EndMode::Graceful), Ok(()));
    assert!(wait_exited(&mut c, id), "pidfd reports the exit");
    assert_eq!(victim.wait_signal(), Some(15), "it really received SIGTERM, not SIGKILL");
    c.forget(id);
    assert_eq!(c.watched(), 0);
}

#[test]
fn sigkill_ends_the_process() {
    let mut victim = Sleeper::spawn(false);
    let id = victim.id();
    let mut c = LinuxController::new();
    assert_eq!(c.end(id, EndMode::Force), Ok(()));
    assert!(wait_exited(&mut c, id));
    assert_eq!(victim.wait_signal(), Some(9));
}

#[test]
fn sigterm_ignored_leaves_the_process_running_and_nothing_escalates() {
    let mut victim = Sleeper::spawn(true);
    let id = victim.id();
    let mut c = LinuxController::new();
    assert_eq!(c.end(id, EndMode::Graceful), Ok(()), "the signal is delivered...");
    std::thread::sleep(Duration::from_millis(400));
    assert!(!c.has_exited(id), "...but the process is still there");
    assert!(victim.is_alive());
    // Only an explicit, separate Force ends it.
    assert_eq!(c.end(id, EndMode::Force), Ok(()));
    assert!(wait_exited(&mut c, id));
    assert_eq!(victim.wait_signal(), Some(9));
}

#[test]
fn stale_identity_is_refused_and_the_real_process_is_untouched() {
    let mut victim = Sleeper::spawn(false);
    let real = victim.id();
    let mut c = LinuxController::new();
    let forged = ProcessId { pid: real.pid, start_time: real.start_time + 1 };
    assert_eq!(c.end(forged, EndMode::Force), Err(ControlError::PidReused));
    assert_eq!(
        c.end(ProcessId { pid: real.pid, start_time: 0 }, EndMode::Graceful),
        Err(ControlError::PidReused)
    );
    assert!(victim.is_alive(), "a PID with the wrong start time is never signalled");
    assert_eq!(c.watched(), 0);
}

#[test]
fn process_that_ended_before_the_action_is_reported_as_such() {
    let mut victim = Sleeper::spawn(false);
    let id = victim.id();
    victim.child.kill().unwrap();
    // Zombie (not reaped yet): already dead.
    for _ in 0..200 {
        let text = std::fs::read_to_string(format!("/proc/{}/stat", id.pid)).unwrap();
        if procfs::parse_stat(&text).unwrap().state == dotmon_core::model::ProcState::Zombie {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut c = LinuxController::new();
    assert_eq!(c.end(id, EndMode::Graceful), Err(ControlError::NotFound));
    // Reaped: the PID is gone (or, in the unlikely event it was recycled
    // within these milliseconds, refused as another identity).
    victim.child.wait().unwrap();
    let r = c.end(id, EndMode::Force);
    assert!(matches!(r, Err(ControlError::NotFound | ControlError::PidReused)), "{r:?}");
}

#[test]
fn many_simultaneous_actions_each_hit_only_their_own_target() {
    let mut victims: Vec<Sleeper> = (0..8).map(|_| Sleeper::spawn(false)).collect();
    let mut stubborn = Sleeper::spawn(true);
    let mut bystander = Sleeper::spawn(false);
    let mut c = LinuxController::new();
    let ids: Vec<ProcessId> = victims.iter().map(Sleeper::id).collect();
    let stubborn_id = stubborn.id();

    for (i, id) in ids.iter().enumerate() {
        // Mixed requests in flight at once.
        let mode = if i % 2 == 0 { EndMode::Graceful } else { EndMode::Force };
        assert_eq!(c.end(*id, mode), Ok(()));
    }
    assert_eq!(c.end(stubborn_id, EndMode::Graceful), Ok(()));
    assert_eq!(c.watched(), 9);

    for (i, (id, v)) in ids.iter().zip(victims.iter_mut()).enumerate() {
        assert!(wait_exited(&mut c, *id), "target {i}");
        assert_eq!(v.wait_signal(), Some(if i % 2 == 0 { 15 } else { 9 }), "target {i}");
        c.forget(*id);
    }
    std::thread::sleep(Duration::from_millis(200));
    assert!(!c.has_exited(stubborn_id) && stubborn.is_alive(), "ignored SIGTERM: still running");
    assert!(bystander.is_alive(), "a process nobody asked about is never touched");
}

#[test]
fn the_controller_refuses_init_itself_and_kernel_threads_for_real() {
    let mut c = LinuxController::new();
    let me = ProcessId { pid: std::process::id(), start_time: 0 };
    assert_eq!(c.end(me, EndMode::Force), Err(ControlError::Protected(ProtectionReason::SelfProcess)));
    assert_eq!(
        c.end(ProcessId { pid: 1, start_time: 0 }, EndMode::Graceful),
        Err(ControlError::Protected(ProtectionReason::Init))
    );
    // kthreadd is PID 2: refused before anything is opened.
    assert_eq!(
        c.end(ProcessId { pid: 2, start_time: 0 }, EndMode::Force),
        Err(ControlError::Protected(ProtectionReason::Init))
    );
    assert_eq!(c.watched(), 0);
}
