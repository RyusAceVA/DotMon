//! Deterministic simulated machine, used by the prototype UI and by tests.
//!
//! Values evolve smoothly (random walks with mean reversion, occasional
//! bursts) so charts look alive. The process table models a realistic
//! Hyprland + Quickshell session. Ending a process through
//! [`MockController`] removes it (and its descendants) from later samples:
//! gracefully on the next tick, or immediately when forced. Unlike the real
//! kernel, the simulation takes the whole tree down with SIGKILL, which keeps
//! its sessions tidy. "obsidian" ignores SIGTERM (like an application stuck
//! on an unsaved-changes dialog), so the "still running" path can be seen
//! without touching a real process.

use std::sync::{Arc, Mutex, MutexGuard};

use dotmon_core::model::{
    CpuStats, DiskStats, GpuStats, MemoryStats, NetStats, ProcState, ProcessId, ProcessInfo, Snapshot,
};
use dotmon_core::source::{ControlError, EndMode, MetricsSource, ProcessController};

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;
const CORES: usize = 16;

/// Small, fast, deterministic PRNG (SplitMix64) — no external dependency.
#[derive(Debug, Clone)]
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    /// Uniform in [-1, 1).
    fn signed(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }
    fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }
}

/// Mean-reverting random walk clamped to [lo, hi].
fn walk(rng: &mut Rng, v: f32, target: f32, step: f32, lo: f32, hi: f32) -> f32 {
    (v + (target - v) * 0.15 + rng.signed() * step).clamp(lo, hi)
}

#[derive(Debug, Clone)]
struct SimProc {
    info: ProcessInfo,
    /// Simulated CPU share, kept apart from `info.cpu` which can be `None`.
    cpu_now: f32,
    /// Created this tick: reported without a CPU figure, like a real new process.
    fresh: bool,
    base_cpu: f32,
    volatility: f32,
    base_mem: u64,
    /// Remaining ticks for short-lived processes.
    ttl: Option<u32>,
    /// Graceful end requested: disappears on the next tick.
    ending: bool,
    /// Ignores SIGTERM (only SIGKILL ends it).
    stubborn: bool,
}

#[derive(Debug)]
struct State {
    rng: Rng,
    seq: u64,
    uptime_s: u64,
    next_pid: u32,
    self_pid: u32,
    user: String,
    uid: u32,
    cores: Vec<f32>,
    mem_used: u64,
    gpu_usage: f32,
    vram_used: u64,
    gpu_temp: f32,
    cpu_temp: f32,
    net_rx: f32,
    net_tx: f32,
    net_rx_total: u64,
    net_tx_total: u64,
    disk_r: f32,
    disk_w: f32,
    procs: Vec<SimProc>,
}

/// Static description of the simulated session.
struct Template {
    name: &'static str,
    exe: Option<&'static str>,
    parent: Option<&'static str>,
    root_owned: bool,
    kernel: bool,
    cpu: f32,
    vol: f32,
    mem_mib: u64,
    threads: u32,
    cgroup: &'static str,
    cmd: &'static str,
}

#[allow(clippy::too_many_arguments)]
const fn t(
    name: &'static str,
    parent: Option<&'static str>,
    cpu: f32,
    vol: f32,
    mem_mib: u64,
    threads: u32,
    cgroup: &'static str,
    cmd: &'static str,
) -> Template {
    Template {
        name,
        exe: None,
        parent,
        root_owned: false,
        kernel: false,
        cpu,
        vol,
        mem_mib,
        threads,
        cgroup,
        cmd,
    }
}

const SESSION: &str = "/user.slice/user-1000.slice/session-2.scope";
const USER_SVC: &str = "/user.slice/user-1000.slice/user@1000.service/session.slice";
const USER_APP: &str = "/user.slice/user-1000.slice/user@1000.service/app.slice";
const SYSTEM: &str = "/system.slice";

fn templates() -> Vec<Template> {
    let root = |mut t: Template| {
        t.root_owned = true;
        t
    };
    let kern = |name: &'static str| Template {
        name,
        exe: None,
        parent: Some("kthreadd"),
        root_owned: true,
        kernel: true,
        cpu: 0.02,
        vol: 0.05,
        mem_mib: 0,
        threads: 1,
        cgroup: "/",
        cmd: "",
    };
    let mut portal = t(
        "xdg-desktop-por",
        Some("systemd@user"),
        0.0,
        0.02,
        24,
        6,
        USER_SVC,
        "/usr/lib/xdg-desktop-portal-hyprland",
    );
    portal.exe = Some("xdg-desktop-portal-hyprland");

    vec![
        root(t("systemd", None, 0.05, 0.05, 14, 1, "/init.scope", "/sbin/init")),
        root(t("kthreadd", None, 0.0, 0.0, 0, 1, "/", "")),
        kern("kworker/0:1-events"),
        kern("kworker/3:2-mm_percpu_wq"),
        kern("ksoftirqd/0"),
        kern("rcu_preempt"),
        kern("nvidia-modeset/kthread_q"),
        root(t(
            "systemd-journald",
            Some("systemd"),
            0.02,
            0.03,
            48,
            1,
            SYSTEM,
            "/usr/lib/systemd/systemd-journald",
        )),
        root(t("systemd-udevd", Some("systemd"), 0.0, 0.02, 12, 1, SYSTEM, "/usr/lib/systemd/systemd-udevd")),
        root(t(
            "NetworkManager",
            Some("systemd"),
            0.03,
            0.04,
            22,
            4,
            SYSTEM,
            "/usr/bin/NetworkManager --no-daemon",
        )),
        root(t("bluetoothd", Some("systemd"), 0.0, 0.01, 6, 1, SYSTEM, "/usr/lib/bluetooth/bluetoothd")),
        root(t("cupsd", Some("systemd"), 0.0, 0.01, 9, 1, SYSTEM, "/usr/bin/cupsd -l")),
        root(t("sddm", Some("systemd"), 0.0, 0.01, 18, 3, SYSTEM, "/usr/bin/sddm")),
        t(
            "systemd@user",
            Some("systemd"),
            0.01,
            0.02,
            11,
            1,
            "/user.slice/user-1000.slice/user@1000.service/init.scope",
            "/usr/lib/systemd/systemd --user",
        ),
        t("start-hyprland", Some("sddm"), 0.0, 0.0, 4, 1, SESSION, "start-hyprland"),
        t("Hyprland", Some("start-hyprland"), 3.2, 1.8, 310, 18, SESSION, "Hyprland"),
        t("Xwayland", Some("Hyprland"), 0.2, 0.3, 64, 4, SESSION, "Xwayland :0 -rootless"),
        t("qs", Some("Hyprland"), 1.6, 1.2, 420, 24, SESSION, "qs -c ii"),
        t("hypridle", Some("Hyprland"), 0.0, 0.01, 8, 2, SESSION, "hypridle"),
        t("pipewire", Some("systemd@user"), 0.8, 0.6, 26, 3, USER_SVC, "/usr/bin/pipewire"),
        t("wireplumber", Some("systemd@user"), 0.2, 0.1, 34, 5, USER_SVC, "/usr/bin/wireplumber"),
        t("pipewire-pulse", Some("systemd@user"), 0.1, 0.1, 18, 3, USER_SVC, "/usr/bin/pipewire-pulse"),
        portal,
        t(
            "dbus-broker",
            Some("systemd@user"),
            0.05,
            0.05,
            6,
            1,
            USER_SVC,
            "dbus-broker --log 4 --controller 9",
        ),
        t(
            "syncthing",
            Some("systemd@user"),
            0.4,
            0.6,
            96,
            14,
            "/user.slice/user-1000.slice/user@1000.service/app.slice/syncthing.service",
            "/usr/bin/syncthing serve --no-browser",
        ),
        t("kdeconnectd", Some("systemd@user"), 0.05, 0.05, 42, 6, USER_APP, "/usr/lib/kdeconnectd"),
        t("firefox", Some("Hyprland"), 4.0, 3.0, 610, 92, SESSION, "/usr/lib/firefox/firefox"),
        t(
            "Web Content",
            Some("firefox"),
            2.5,
            3.5,
            380,
            28,
            SESSION,
            "/usr/lib/firefox/firefox -contentproc -childID 4 tab",
        ),
        t(
            "Web Content",
            Some("firefox"),
            0.8,
            1.2,
            240,
            24,
            SESSION,
            "/usr/lib/firefox/firefox -contentproc -childID 7 tab",
        ),
        t(
            "Isolated Web Co",
            Some("firefox"),
            1.2,
            2.0,
            310,
            26,
            SESSION,
            "/usr/lib/firefox/firefox -contentproc -isolatedWeb",
        ),
        t(
            "RDD Process",
            Some("firefox"),
            0.3,
            0.5,
            70,
            8,
            SESSION,
            "/usr/lib/firefox/firefox -contentproc rdd",
        ),
        t("kitty", Some("Hyprland"), 0.6, 0.6, 140, 12, SESSION, "kitty"),
        t("fish", Some("kitty"), 0.0, 0.05, 12, 1, SESSION, "/usr/bin/fish"),
        t("nvim", Some("fish"), 0.3, 0.4, 88, 3, SESSION, "nvim src/main.rs"),
        t("rust-analyzer", Some("nvim"), 2.0, 3.0, 1450, 22, SESSION, "rust-analyzer"),
        t("cargo", Some("fish"), 0.2, 0.2, 60, 4, SESSION, "cargo watch -x test"),
        t("blender", Some("Hyprland"), 6.0, 5.0, 2300, 40, SESSION, "blender ~/projets/scene.blend"),
        t("mpv", Some("Hyprland"), 1.8, 0.8, 190, 14, SESSION, "mpv ~/Vidéos/concert.mkv"),
        t("thunar", Some("Hyprland"), 0.0, 0.05, 72, 5, SESSION, "thunar"),
        t("obsidian", Some("Hyprland"), 0.5, 0.9, 520, 30, SESSION, "obsidian"),
    ]
}

impl State {
    fn new(seed: u64, self_pid: u32, uid: u32, user: &str) -> Self {
        let mut s = State {
            rng: Rng(seed),
            seq: 0,
            uptime_s: 3 * 3600 + 17 * 60 + 42,
            next_pid: 300,
            self_pid,
            user: user.to_string(),
            uid,
            cores: vec![8.0; CORES],
            mem_used: 9 * GIB,
            gpu_usage: 12.0,
            vram_used: 2 * GIB,
            gpu_temp: 48.0,
            cpu_temp: 52.0,
            net_rx: 200_000.0,
            net_tx: 40_000.0,
            net_rx_total: 3_400 * MIB,
            net_tx_total: 420 * MIB,
            disk_r: 2_000_000.0,
            disk_w: 800_000.0,
            procs: Vec::new(),
        };
        s.populate();
        s
    }

    fn alloc_pid(&mut self) -> u32 {
        loop {
            let pid = self.next_pid;
            self.next_pid += 1 + (self.rng.next_u64() % 7) as u32;
            if pid != self.self_pid {
                return pid;
            }
        }
    }

    fn populate(&mut self) {
        let mut by_name: std::collections::HashMap<&'static str, u32> = Default::default();
        for tpl in templates() {
            let pid = match tpl.name {
                "systemd" => 1,
                "kthreadd" => 2,
                _ => self.alloc_pid(),
            };
            let ppid = tpl.parent.and_then(|p| by_name.get(p).copied());
            by_name.entry(tpl.name).or_insert(pid);
            let start =
                if pid <= 2 { 0 } else { (self.rng.next_u64() % 600) + if tpl.root_owned { 1 } else { 30 } };
            // Template keys like "systemd@user" disambiguate parents; the
            // process itself is just "systemd", as on a real system.
            let name = tpl.name.split('@').next().unwrap_or(tpl.name);
            self.procs.push(self.make(
                pid,
                ppid,
                name,
                tpl.exe,
                tpl.cmd,
                tpl.root_owned,
                tpl.kernel,
                tpl.cgroup,
                tpl.cpu,
                tpl.vol,
                tpl.mem_mib * MIB,
                tpl.threads,
                start,
            ));
        }
        // Dotmon itself, launched from kitty/fish, with its real PID so the
        // self-protection rule is exercised.
        let fish = by_name.get("fish").copied();
        let me = self.self_pid;
        let up = self.uptime_s;
        self.procs.push(self.make(
            me,
            fish,
            "dotmon",
            None,
            "dotmon --mock",
            false,
            false,
            SESSION,
            0.4,
            0.2,
            48 * MIB,
            6,
            up,
        ));
    }

    #[allow(clippy::too_many_arguments)]
    fn make(
        &self,
        pid: u32,
        ppid: Option<u32>,
        name: &str,
        exe: Option<&str>,
        cmd: &str,
        root_owned: bool,
        kernel: bool,
        cgroup: &str,
        cpu: f32,
        vol: f32,
        mem: u64,
        threads: u32,
        start_time: u64,
    ) -> SimProc {
        SimProc {
            info: ProcessInfo {
                pid,
                ppid,
                name: name.to_string(),
                exe_name: exe.map(str::to_string),
                cmdline: if cmd.is_empty() { format!("[{name}]") } else { cmd.to_string() },
                user: if root_owned { "root".into() } else { self.user.clone() },
                uid: if root_owned { 0 } else { self.uid },
                state: ProcState::Sleeping,
                cpu: Some(cpu),
                memory: mem,
                threads,
                // Same convention as Linux: identity in clock ticks (100 Hz).
                start_time: start_time * 100,
                started_s: start_time,
                disk_read_bps: Some(0),
                disk_write_bps: Some(0),
                kernel_thread: kernel,
                cgroup: Some(cgroup.to_string()),
            },
            cpu_now: cpu,
            fresh: false,
            base_cpu: cpu,
            volatility: vol,
            base_mem: mem,
            ttl: None,
            ending: false,
            stubborn: name == "obsidian",
        }
    }

    fn remove_tree(&mut self, pid: u32) {
        let mut doomed = vec![pid];
        let mut i = 0;
        while i < doomed.len() {
            let parent = doomed[i];
            doomed.extend(self.procs.iter().filter(|p| p.info.ppid == Some(parent)).map(|p| p.info.pid));
            i += 1;
        }
        self.procs.retain(|p| !doomed.contains(&p.info.pid));
    }

    fn step(&mut self, dt_s: f32) -> Snapshot {
        self.seq += 1;
        self.uptime_s += dt_s.round().max(1.0) as u64;

        // Graceful ends requested during the previous interval complete now.
        let ending: Vec<u32> = self.procs.iter().filter(|p| p.ending).map(|p| p.info.pid).collect();
        for pid in ending {
            self.remove_tree(pid);
        }

        // Short-lived compiler jobs come and go under `cargo`.
        for p in &mut self.procs {
            if let Some(ttl) = &mut p.ttl {
                *ttl = ttl.saturating_sub(1);
            }
        }
        self.procs.retain(|p| p.ttl != Some(0));
        let cargo = self.procs.iter().find(|p| p.info.name == "cargo").map(|p| p.info.pid);
        if let Some(cargo) = cargo
            && self.rng.chance(0.08)
        {
            let jobs = 1 + (self.rng.next_u64() % 3) as usize;
            for _ in 0..jobs {
                let pid = self.alloc_pid();
                let up = self.uptime_s;
                let mut job = self.make(
                    pid,
                    Some(cargo),
                    "rustc",
                    None,
                    "rustc --crate-name dotmon_core --edition=2024",
                    false,
                    false,
                    SESSION,
                    5.5,
                    1.5,
                    700 * MIB,
                    1,
                    up,
                );
                job.ttl = Some(3 + (self.rng.next_u64() % 6) as u32);
                // Like a real new process: no earlier reading, so no CPU yet.
                job.fresh = true;
                job.info.state = ProcState::Running;
                self.procs.push(job);
            }
        }

        // Processes.
        let rng = &mut self.rng;
        for p in &mut self.procs {
            let i = &mut p.info;
            if i.kernel_thread {
                p.cpu_now = (rng.unit() * p.volatility).max(0.0);
                i.cpu = Some(p.cpu_now);
                i.state = if p.cpu_now > 0.03 { ProcState::Running } else { ProcState::Idle };
                continue;
            }
            p.cpu_now = walk(rng, p.cpu_now, p.base_cpu, p.volatility, 0.0, 100.0 / CORES as f32 * 4.0);
            i.cpu = if p.fresh { None } else { Some(p.cpu_now) };
            p.fresh = false;
            let mem_target = p.base_mem as f32;
            let mem =
                i.memory as f32 + (mem_target - i.memory as f32) * 0.1 + rng.signed() * mem_target * 0.01;
            i.memory = mem.max(0.0) as u64;
            i.state = if p.cpu_now > 1.0 { ProcState::Running } else { ProcState::Sleeping };
            let io = p.cpu_now * 40_000.0 * rng.unit();
            i.disk_read_bps = Some(io as u64);
            i.disk_write_bps = Some((io * 0.4 * rng.unit()) as u64);
        }

        let proc_cpu: f32 = self.procs.iter().map(|p| p.cpu_now).sum();
        let compiling = self.procs.iter().filter(|p| p.info.name == "rustc").count() as f32;

        // CPU cores follow total process load, with per-core noise.
        let target = proc_cpu.clamp(2.0, 95.0);
        for (k, c) in self.cores.iter_mut().enumerate() {
            let bias = if k < 2 { 6.0 } else { 0.0 } + compiling * 9.0 * (k % 3 == 0) as u8 as f32;
            *c = walk(&mut self.rng, *c, target + bias, 6.0, 0.0, 100.0);
        }
        let usage = self.cores.iter().sum::<f32>() / CORES as f32;
        self.cpu_temp = walk(&mut self.rng, self.cpu_temp, 42.0 + usage * 0.5, 0.8, 35.0, 98.0);

        // Memory: processes + caches/kernel.
        let proc_mem: u64 = self.procs.iter().map(|p| p.info.memory).sum();
        self.mem_used = proc_mem + 2 * GIB + (self.rng.unit() * 200.0) as u64 * MIB;

        // GPU follows Blender/mpv with bursts.
        let gpu_target = if self.rng.chance(0.06) { 85.0 } else { 18.0 + compiling * 2.0 };
        self.gpu_usage = walk(&mut self.rng, self.gpu_usage, gpu_target, 4.0, 0.0, 100.0);
        self.vram_used = ((2.0 + self.gpu_usage / 100.0 * 4.5) * GIB as f32) as u64;
        self.gpu_temp = walk(&mut self.rng, self.gpu_temp, 40.0 + self.gpu_usage * 0.4, 0.7, 30.0, 92.0);

        // Network bursts (downloads) and disk activity.
        let rx_target = if self.rng.chance(0.1) { 9_000_000.0 } else { 150_000.0 };
        self.net_rx = walk(&mut self.rng, self.net_rx, rx_target, 60_000.0, 0.0, 120_000_000.0);
        self.net_tx =
            walk(&mut self.rng, self.net_tx, self.net_rx * 0.08 + 20_000.0, 8_000.0, 0.0, 40_000_000.0);
        self.net_rx_total += (self.net_rx * dt_s) as u64;
        self.net_tx_total += (self.net_tx * dt_s) as u64;
        let disk_target =
            if self.rng.chance(0.12) { 140_000_000.0 } else { 1_500_000.0 + compiling * 20_000_000.0 };
        self.disk_r = walk(&mut self.rng, self.disk_r, disk_target, 500_000.0, 0.0, 3_000_000_000.0);
        self.disk_w = walk(&mut self.rng, self.disk_w, self.disk_r * 0.35, 300_000.0, 0.0, 3_000_000_000.0);

        let total_mem = 32 * GIB;
        Snapshot {
            seq: self.seq,
            hostname: "cachyos-sim".into(),
            kernel: "simulé".into(),
            uptime_s: self.uptime_s,
            cpu: CpuStats {
                model: "CPU SIMULÉ · 16 THREADS".into(),
                usage,
                per_core: self.cores.clone(),
                frequency_mhz: Some((2400.0 + usage * 22.0) as u32),
                temperature_c: Some(self.cpu_temp),
                load_avg: Some([usage / 100.0 * CORES as f32 * 0.6, 3.1, 2.7]),
            },
            memory: MemoryStats {
                total: total_mem,
                used: self.mem_used.min(total_mem),
                available: total_mem.saturating_sub(self.mem_used),
                cached: Some(6 * GIB),
                swap_total: 16 * GIB,
                swap_used: 312 * MIB,
            },
            gpus: vec![GpuStats {
                name: "GPU SIMULÉ · 8 Go".into(),
                vendor: "sim".into(),
                usage: Some(self.gpu_usage),
                vram_total: Some(8 * GIB),
                vram_used: Some(self.vram_used),
                temperature_c: Some(self.gpu_temp),
                power_w: Some(12.0 + self.gpu_usage * 0.9),
                soc_power_w: None,
                state: Default::default(),
            }],
            disks: vec![
                DiskStats {
                    name: "nvme0n1p2".into(),
                    mount_point: "/".into(),
                    filesystem: "btrfs".into(),
                    total: 953 * GIB,
                    used: 412 * GIB,
                    read_bps: Some(self.disk_r as u64),
                    write_bps: Some(self.disk_w as u64),
                },
                DiskStats {
                    name: "nvme0n1p1".into(),
                    mount_point: "/boot".into(),
                    filesystem: "vfat".into(),
                    total: 2 * GIB,
                    used: 380 * MIB,
                    read_bps: Some(0),
                    write_bps: Some(0),
                },
            ],
            disk_io: None,
            networks: vec![NetStats {
                interface: "wlan0".into(),
                rx_bps: self.net_rx as u64,
                tx_bps: self.net_tx as u64,
                rx_total: self.net_rx_total,
                tx_total: self.net_tx_total,
            }],
            processes: self.procs.iter().map(|p| p.info.clone()).collect(),
        }
    }

    fn end(&mut self, target: ProcessId, mode: EndMode) -> Result<(), ControlError> {
        let Some(p) = self.procs.iter_mut().find(|p| p.info.pid == target.pid) else {
            return Err(ControlError::NotFound);
        };
        if p.info.start_time != target.start_time {
            return Err(ControlError::PidReused);
        }
        if p.info.uid != self.uid {
            return Err(ControlError::PermissionDenied);
        }
        match mode {
            // Delivered, but ignored: the process stays.
            EndMode::Graceful if p.stubborn => {}
            EndMode::Graceful => p.ending = true,
            EndMode::Force => self.remove_tree(target.pid),
        }
        Ok(())
    }

    fn has_exited(&self, target: ProcessId) -> bool {
        // A graceful end completes on the next tick: until then it still runs.
        !self.procs.iter().any(|p| p.info.id() == target)
    }
}

/// Simulated source; create with [`mock_backend`].
pub struct MockSource {
    state: Arc<Mutex<State>>,
    interval_s: f32,
}

/// Simulated controller sharing state with its [`MockSource`].
pub struct MockController {
    state: Arc<Mutex<State>>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    // A panic while holding the lock cannot leave the simulation in a state
    // worse than "slightly odd numbers"; keep going.
    state.lock().unwrap_or_else(|e| e.into_inner())
}

/// Creates a simulated machine. `self_pid` / `uid` / `user` should be the
/// real values of the running process so protection rules behave as in
/// production.
pub fn mock_backend(seed: u64, self_pid: u32, uid: u32, user: &str) -> (MockSource, MockController) {
    let state = Arc::new(Mutex::new(State::new(seed, self_pid, uid, user)));
    (MockSource { state: state.clone(), interval_s: 1.0 }, MockController { state })
}

impl MockSource {
    /// Seconds of simulated time per sample (affects totals and uptime).
    pub fn set_interval(&mut self, seconds: f32) {
        self.interval_s = seconds.max(0.05);
    }
}

impl MetricsSource for MockSource {
    fn name(&self) -> &str {
        "SIMULATION"
    }

    fn sample(&mut self) -> Snapshot {
        lock(&self.state).step(self.interval_s)
    }

    fn set_interval_hint(&mut self, interval: std::time::Duration) {
        self.set_interval(interval.as_secs_f32());
    }
}

impl ProcessController for MockController {
    fn end(&mut self, target: ProcessId, mode: EndMode) -> Result<(), ControlError> {
        lock(&self.state).end(target, mode)
    }

    fn has_exited(&mut self, target: ProcessId) -> bool {
        lock(&self.state).has_exited(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: u32 = 1000;

    fn backend() -> (MockSource, MockController) {
        mock_backend(7, 99_999, ME, "tester")
    }

    fn find<'a>(s: &'a Snapshot, name: &str) -> &'a ProcessInfo {
        s.processes.iter().find(|p| p.name == name).unwrap()
    }

    #[test]
    fn deterministic_for_same_seed() {
        let (mut a, _) = backend();
        let (mut b, _) = backend();
        for _ in 0..20 {
            assert_eq!(a.sample(), b.sample());
        }
    }

    #[test]
    fn values_stay_in_range() {
        let (mut src, _) = backend();
        for _ in 0..500 {
            let s = src.sample();
            assert!((0.0..=100.0).contains(&s.cpu.usage));
            assert_eq!(s.cpu.per_core.len(), CORES);
            assert!(s.cpu.per_core.iter().all(|c| (0.0..=100.0).contains(c)));
            assert!(s.memory.used <= s.memory.total);
            let g = &s.gpus[0];
            assert!((0.0..=100.0).contains(&g.usage.unwrap()));
            assert!(g.vram_used.unwrap() <= g.vram_total.unwrap());
            assert!(s.processes.iter().all(|p| p.cpu.is_none_or(|c| c.is_finite() && c >= 0.0)));
        }
    }

    #[test]
    fn sequence_and_uptime_advance() {
        let (mut src, _) = backend();
        let a = src.sample();
        let b = src.sample();
        assert_eq!(b.seq, a.seq + 1);
        assert!(b.uptime_s > a.uptime_s);
    }

    #[test]
    fn includes_self_with_real_pid() {
        let (mut src, _) = backend();
        let s = src.sample();
        assert_eq!(find(&s, "dotmon").pid, 99_999);
        assert_eq!(find(&s, "systemd").pid, 1);
    }

    #[test]
    fn graceful_end_removes_tree_on_next_tick() {
        let (mut src, mut ctl) = backend();
        let s = src.sample();
        let ff = find(&s, "firefox").clone();
        ctl.end(ff.id(), EndMode::Graceful).unwrap();
        let s2 = src.sample();
        assert!(s2.processes.iter().all(|p| p.name != "firefox" && p.name != "Web Content"));
    }

    #[test]
    fn force_end_is_immediate() {
        let (mut src, mut ctl) = backend();
        let s = src.sample();
        let kitty = find(&s, "kitty").clone();
        ctl.end(kitty.id(), EndMode::Force).unwrap();
        let s2 = src.sample();
        // kitty → fish → nvim, cargo, dotmon(!) all gone in the simulation.
        assert!(s2.processes.iter().all(|p| p.name != "kitty" && p.name != "nvim"));
    }

    #[test]
    fn stale_identity_is_refused() {
        let (mut src, mut ctl) = backend();
        let s = src.sample();
        let mut id = find(&s, "mpv").id();
        id.start_time += 1;
        assert_eq!(ctl.end(id, EndMode::Force), Err(ControlError::PidReused));
        assert_eq!(
            ctl.end(ProcessId { pid: 4_000_000, start_time: 0 }, EndMode::Force),
            Err(ControlError::NotFound)
        );
    }

    #[test]
    fn other_users_processes_are_refused() {
        let (mut src, mut ctl) = backend();
        let s = src.sample();
        let cups = find(&s, "cupsd").id();
        assert_eq!(ctl.end(cups, EndMode::Force), Err(ControlError::PermissionDenied));
    }

    #[test]
    fn compile_jobs_appear_and_expire() {
        let (mut src, _) = backend();
        let mut seen = false;
        let mut max_alive = 0;
        for _ in 0..400 {
            let n = src.sample().processes.iter().filter(|p| p.name == "rustc").count();
            seen |= n > 0;
            max_alive = max_alive.max(n);
        }
        assert!(seen, "the simulation should spawn compiler jobs");
        assert!(max_alive < 20, "jobs must expire");
    }

    #[test]
    fn new_processes_have_no_cpu_figure_on_their_first_sample() {
        let (mut src, _) = backend();
        let mut seen: std::collections::HashMap<dotmon_core::model::ProcessId, bool> = Default::default();
        let mut fresh = 0;
        for _ in 0..200 {
            let s = src.sample();
            for p in &s.processes {
                match seen.insert(p.id(), true) {
                    None if p.name == "rustc" => {
                        assert_eq!(p.cpu, None, "first sighting of rustc {}", p.pid);
                        fresh += 1;
                    }
                    Some(_) => assert!(p.cpu.is_some(), "{} measured from its second sample", p.name),
                    None => {}
                }
            }
        }
        assert!(fresh > 0, "the simulation does spawn compiler jobs");
    }
}
