//! Grouping processes into "applications" and classifying them.
//!
//! On a Hyprland session started without uwsm, most graphical apps share the
//! same `session-N.scope` cgroup, so cgroups alone cannot tell apps apart.
//! We therefore use the process tree: an application is the top-most
//! ancestor directly below a *session host* (init, the user systemd instance,
//! the compositor, the shell launcher…) together with all its descendants.
//! `kitty → fish → nvim` is one app (kitty); `firefox` + its content
//! processes is one app (firefox).

use std::collections::HashMap;

use crate::model::{ProcessId, ProcessInfo, Snapshot};
use crate::protection::ProtectionPolicy;

/// Processes whose children are independent applications.
const SESSION_HOSTS: &[&str] = &[
    "systemd",
    "init",
    "kthreadd",
    "Hyprland",
    "start-hyprland",
    "uwsm",
    "sway",
    "niri",
    "kwin_wayland",
    "gnome-shell",
    "qs",
    "quickshell",
    "sddm",
    "sddm-helper",
    "greetd",
    "login",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcessKind {
    /// User application (graphical or launched from a terminal).
    App,
    /// Background user service (systemd user unit, daemon).
    Service,
    /// Core of the desktop session (compositor, shell, audio…).
    Session,
    /// Owned by root or another user.
    System,
    Kernel,
}

impl ProcessKind {
    pub fn label(self) -> &'static str {
        match self {
            ProcessKind::App => "APP",
            ProcessKind::Service => "SERVICE",
            ProcessKind::Session => "SESSION",
            ProcessKind::System => "SYSTÈME",
            ProcessKind::Kernel => "NOYAU",
        }
    }
}

pub(crate) const SHELLS: &[&str] = &["sh", "bash", "dash", "zsh", "fish", "ksh", "mksh"];

/// A shell running a script or `-c` command (e.g. a launcher script that
/// starts an app and waits for it). Interactive shells have no arguments.
pub fn is_launcher_shell(p: &ProcessInfo) -> bool {
    SHELLS.contains(&p.name.as_str()) && p.cmdline.split_whitespace().nth(1).is_some()
}

pub fn is_session_host(p: &ProcessInfo) -> bool {
    p.pid <= 2 || SESSION_HOSTS.contains(&p.name.as_str())
}

pub fn classify(p: &ProcessInfo, policy: &ProtectionPolicy) -> ProcessKind {
    if p.kernel_thread || p.ppid == Some(2) || p.pid == 2 {
        return ProcessKind::Kernel;
    }
    if p.uid != policy.current_uid() {
        return ProcessKind::System;
    }
    if let crate::protection::Protection::Protected(crate::protection::ProtectionReason::Critical(_)) =
        policy.assess(p)
    {
        return ProcessKind::Session;
    }
    let is_unit =
        p.cgroup.as_deref().and_then(|c| c.rsplit('/').next()).is_some_and(|leaf| leaf.ends_with(".service"));
    if is_unit { ProcessKind::Service } else { ProcessKind::App }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppGroup {
    /// Identity of the root process (or of the first kernel thread for the
    /// synthetic kernel group).
    pub root: ProcessId,
    pub name: String,
    pub user: String,
    pub uid: u32,
    pub kind: ProcessKind,
    /// Members in snapshot order, root included.
    pub members: Vec<ProcessId>,
    /// Sum over the members that have a measurement; `None` when none has.
    pub cpu: Option<f32>,
    pub memory: u64,
    pub threads: u32,
    /// `None` when no member's I/O is readable.
    pub io_bps: Option<u64>,
}

impl AppGroup {
    pub fn count(&self) -> usize {
        self.members.len()
    }
}

/// Root of the application `idx` belongs to (index into `procs`).
fn app_root(idx: usize, procs: &[ProcessInfo], by_pid: &HashMap<u32, usize>) -> usize {
    let mut current = idx;
    // Bounded walk: protects against malformed trees (cycles, races).
    for _ in 0..128 {
        let p = &procs[current];
        if is_session_host(p) {
            return current;
        }
        match p.ppid.and_then(|pp| by_pid.get(&pp)) {
            Some(&parent) if !is_session_host(&procs[parent]) => current = parent,
            _ => return current,
        }
    }
    current
}

/// Groups every process of `snap` into applications.
///
/// Kernel threads are collected in a single synthetic group. Groups are
/// returned in order of first appearance of their root in the snapshot.
pub fn group_apps(snap: &Snapshot, policy: &ProtectionPolicy) -> Vec<AppGroup> {
    let procs = &snap.processes;
    let by_pid: HashMap<u32, usize> = procs.iter().enumerate().map(|(i, p)| (p.pid, i)).collect();

    let mut order: Vec<usize> = Vec::new();
    let mut groups: HashMap<usize, AppGroup> = HashMap::new();
    let mut kernel: Option<AppGroup> = None;

    for (i, p) in procs.iter().enumerate() {
        let is_kernel = p.kernel_thread || p.ppid == Some(2) || p.pid == 2;
        let group = if is_kernel {
            kernel.get_or_insert_with(|| AppGroup {
                root: p.id(),
                name: "Threads du noyau".into(),
                user: p.user.clone(),
                uid: p.uid,
                kind: ProcessKind::Kernel,
                members: Vec::new(),
                cpu: None,
                memory: 0,
                threads: 0,
                io_bps: None,
            })
        } else {
            let root = app_root(i, procs, &by_pid);
            groups.entry(root).or_insert_with(|| {
                order.push(root);
                let r = &procs[root];
                AppGroup {
                    root: r.id(),
                    name: r.display_name().to_string(),
                    user: r.user.clone(),
                    uid: r.uid,
                    kind: classify(r, policy),
                    members: Vec::new(),
                    cpu: None,
                    memory: 0,
                    threads: 0,
                    io_bps: None,
                }
            })
        };
        group.members.push(p.id());
        if let Some(cpu) = p.cpu {
            group.cpu = Some(group.cpu.unwrap_or(0.0) + cpu);
        }
        group.memory += p.memory;
        group.threads += p.threads;
        if let Some(io) = p.io_bps() {
            group.io_bps = Some(group.io_bps.unwrap_or(0) + io);
        }
    }

    // `order` follows first appearance of each root as a *group key*, which
    // may come after a child; re-sort by the root's snapshot position.
    // Launcher scripts (`bash launch.sh firefox`) would otherwise name the
    // app "bash": name the group after the first non-shell child instead.
    for (&root, group) in groups.iter_mut() {
        let r = &procs[root];
        if !is_launcher_shell(r) {
            continue;
        }
        let child = procs.iter().find(|c| c.ppid == Some(r.pid) && !SHELLS.contains(&c.name.as_str()));
        if let Some(c) = child {
            group.name = c.display_name().to_string();
            group.kind = classify(c, policy);
        }
    }

    order.sort_unstable();
    let mut out: Vec<AppGroup> = order.into_iter().filter_map(|i| groups.remove(&i)).collect();
    out.extend(kernel);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixtures::proc;

    const ME: u32 = 1000;

    fn snapshot() -> Snapshot {
        let mut procs = vec![
            proc(1, None, "systemd", 0),
            proc(2, None, "kthreadd", 0),
            proc(3, Some(2), "kworker/0:0", 0),
            proc(4, Some(2), "kworker/1:0", 0),
            proc(100, Some(1), "Hyprland", ME),
            proc(110, Some(100), "firefox", ME),
            proc(111, Some(110), "Web Content", ME),
            proc(112, Some(110), "Web Content", ME),
            proc(120, Some(100), "kitty", ME),
            proc(121, Some(120), "fish", ME),
            proc(122, Some(121), "nvim", ME),
            proc(130, Some(1), "pipewire", ME),
            proc(140, Some(1), "postgres", 970),
            proc(141, Some(140), "postgres", 970),
            proc(150, Some(999), "orphan", ME), // parent not in snapshot
        ];
        for p in &mut procs {
            p.cpu = Some(1.0);
            p.memory = 10;
        }
        procs[2].kernel_thread = true;
        procs[3].kernel_thread = true;
        Snapshot { processes: procs, ..Default::default() }
    }

    fn find<'a>(groups: &'a [AppGroup], name: &str) -> &'a AppGroup {
        groups.iter().find(|g| g.name == name).unwrap_or_else(|| panic!("no group {name}"))
    }

    #[test]
    fn children_are_grouped_under_their_app() {
        let pol = ProtectionPolicy::new(9999, ME);
        let groups = group_apps(&snapshot(), &pol);

        let ff = find(&groups, "firefox");
        assert_eq!(ff.count(), 3);
        assert_eq!(ff.cpu, Some(3.0));
        assert_eq!(ff.memory, 30);
        assert_eq!(ff.kind, ProcessKind::App);

        let kitty = find(&groups, "kitty");
        let pids: Vec<u32> = kitty.members.iter().map(|m| m.pid).collect();
        assert_eq!(pids, vec![120, 121, 122]);
    }

    #[test]
    fn session_hosts_are_their_own_group() {
        let pol = ProtectionPolicy::new(9999, ME);
        let groups = group_apps(&snapshot(), &pol);
        let hypr = find(&groups, "Hyprland");
        assert_eq!(hypr.count(), 1);
        assert_eq!(hypr.kind, ProcessKind::Session);
        assert_eq!(find(&groups, "pipewire").kind, ProcessKind::Session);
    }

    #[test]
    fn kernel_threads_share_one_group() {
        let pol = ProtectionPolicy::new(9999, ME);
        let groups = group_apps(&snapshot(), &pol);
        let k = groups.last().unwrap();
        assert_eq!(k.kind, ProcessKind::Kernel);
        assert_eq!(k.count(), 3); // kthreadd + 2 kworkers
    }

    #[test]
    fn other_users_and_orphans() {
        let pol = ProtectionPolicy::new(9999, ME);
        let groups = group_apps(&snapshot(), &pol);
        let pg = find(&groups, "postgres");
        assert_eq!(pg.count(), 2);
        assert_eq!(pg.kind, ProcessKind::System);
        assert_eq!(find(&groups, "orphan").count(), 1);
    }

    #[test]
    fn every_process_lands_in_exactly_one_group() {
        let snap = snapshot();
        let pol = ProtectionPolicy::new(9999, ME);
        let groups = group_apps(&snap, &pol);
        let total: usize = groups.iter().map(|g| g.count()).sum();
        assert_eq!(total, snap.processes.len());
    }

    #[test]
    fn cycles_do_not_hang() {
        let snap = Snapshot {
            processes: vec![proc(10, Some(11), "a", ME), proc(11, Some(10), "b", ME)],
            ..Default::default()
        };
        let groups = group_apps(&snap, &ProtectionPolicy::new(1, ME));
        assert_eq!(groups.iter().map(|g| g.count()).sum::<usize>(), 2);
    }

    #[test]
    fn launcher_scripts_take_the_name_of_their_app() {
        let mut launcher = proc(50, Some(1), "bash", ME);
        launcher.cmdline = "bash ~/.config/hypr/scripts/launch_first_available.sh kitty foot".into();
        let mut interactive = proc(60, Some(1), "bash", ME);
        interactive.cmdline = "bash".into();
        let snap = Snapshot {
            processes: vec![
                proc(1, None, "systemd", 0),
                launcher,
                proc(51, Some(50), "kitty", ME),
                proc(52, Some(51), "fish", ME),
                interactive,
            ],
            ..Default::default()
        };
        let groups = group_apps(&snap, &ProtectionPolicy::new(9999, ME));
        let kitty = find(&groups, "kitty");
        assert_eq!(kitty.root.pid, 50, "the launcher stays the root (and a member)");
        assert_eq!(kitty.count(), 3);
        assert_eq!(kitty.kind, ProcessKind::App);
        assert_eq!(find(&groups, "bash").root.pid, 60, "interactive shells keep their name");
    }

    #[test]
    fn services_detected_from_cgroup() {
        let mut p = proc(300, Some(1), "syncthing", ME);
        p.cgroup = Some("/user.slice/user-1000.slice/user@1000.service/app.slice/syncthing.service".into());
        assert_eq!(classify(&p, &ProtectionPolicy::new(1, ME)), ProcessKind::Service);
    }

    #[test]
    fn group_cpu_is_unknown_only_when_no_member_is_measured() {
        let pol = ProtectionPolicy::new(9999, ME);
        let mut s = snapshot();
        for p in s.processes.iter_mut().filter(|p| p.name == "firefox" || p.ppid == Some(30)) {
            p.cpu = None;
        }
        let groups = group_apps(&s, &pol);
        let ff = groups.iter().filter(|g| g.name == "firefox").collect::<Vec<_>>();
        assert_eq!(ff.len(), 1);
        // Mixed group: the sum of the measured members, never a made-up 0.
        let measured: f32 =
            s.processes.iter().filter(|p| ff[0].members.contains(&p.id())).filter_map(|p| p.cpu).sum();
        let any = s.processes.iter().any(|p| ff[0].members.contains(&p.id()) && p.cpu.is_some());
        assert_eq!(ff[0].cpu, any.then_some(measured));
        // Nobody measured: unknown.
        for p in s.processes.iter_mut() {
            p.cpu = None;
        }
        assert!(group_apps(&s, &pol).iter().all(|g| g.cpu.is_none()));
    }
}
