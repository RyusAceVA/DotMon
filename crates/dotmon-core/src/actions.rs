//! Planning of "end" actions: which exact processes a confirmation covers.
//!
//! An [`EndPlan`] is what the user is asked to confirm. It names every
//! process the action will touch **by identity** (PID + start time) and the
//! single signal that will be sent to them, so the confirmation cannot be
//! stretched to other processes or to a harsher signal afterwards.
//!
//! Planning refuses rather than guesses:
//! - a protected process is never a target (individually or in a group);
//! - a group whose main process cannot be identified without guessing is
//!   refused (a launcher script starting several programs);
//! - a group containing Dotmon itself is refused (ending it would end us).
//!
//! Group semantics (see `docs/PHASE3.md`):
//! - **SIGTERM** on an application is sent to its *main process only*: the
//!   application closes its own children, as it does when its window closes.
//! - **SIGKILL** on an application is sent to the main process and to the
//!   processes verified as its descendants at planning time, each one named
//!   in the plan. Nothing is resolved recursively later: a process that
//!   appeared after the confirmation is not touched.

use std::collections::HashSet;

use crate::apps::{AppGroup, SHELLS, is_launcher_shell};
use crate::model::{ProcessId, ProcessInfo, Snapshot};
use crate::protection::{Protection, ProtectionPolicy, ProtectionReason};
use crate::query::RowKey;
use crate::source::EndMode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndTarget {
    pub id: ProcessId,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EndPlan {
    /// Process or application name, as shown in the confirmation.
    pub name: String,
    /// PID of the main process (the process itself for a single one).
    pub pid: u32,
    pub mode: EndMode,
    /// Exactly the processes that will receive the signal.
    pub targets: Vec<EndTarget>,
    /// Application members that are protected and therefore left alone.
    pub excluded_protected: usize,
    pub is_group: bool,
}

impl EndPlan {
    pub fn ids(&self) -> Vec<ProcessId> {
        self.targets.iter().map(|t| t.id).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndRefusal {
    /// The process or application is not in the latest snapshot.
    Gone,
    Protected(ProtectionReason),
    KernelThreads,
    /// The members of the application cannot be told apart reliably.
    Ambiguous(String),
    /// The application contains Dotmon itself.
    ContainsSelf,
}

impl EndRefusal {
    /// French explanation shown to the user.
    pub fn message(&self) -> String {
        match self {
            EndRefusal::Gone => "Le processus n'existe plus".into(),
            EndRefusal::Protected(r) => format!("Processus protégé : {}", r.message()),
            EndRefusal::KernelThreads => "Threads du noyau : protégés".into(),
            EndRefusal::Ambiguous(why) => format!(
                "Fermeture groupée refusée : {why}. Utilisez la vue PROCESSUS pour viser un processus précis."
            ),
            EndRefusal::ContainsSelf => "Fermeture groupée refusée : ce groupe contient Dotmon lui-même. \
                 Utilisez la vue PROCESSUS."
                .into(),
        }
    }
}

fn target(p: &ProcessInfo) -> EndTarget {
    EndTarget { id: p.id(), name: p.display_name().to_string() }
}

/// Builds the plan for ending `key` with `mode`, or says why not.
pub fn plan_end(
    snap: &Snapshot,
    policy: &ProtectionPolicy,
    groups: &[AppGroup],
    key: RowKey,
    mode: EndMode,
) -> Result<EndPlan, EndRefusal> {
    match key {
        RowKey::Process(id) => {
            let p = snap.process(id).ok_or(EndRefusal::Gone)?;
            if let Protection::Protected(r) = policy.assess(p) {
                return Err(EndRefusal::Protected(r));
            }
            Ok(EndPlan {
                name: p.display_name().to_string(),
                pid: p.pid,
                mode,
                targets: vec![target(p)],
                excluded_protected: 0,
                is_group: false,
            })
        }
        RowKey::App(root) => plan_group(snap, policy, groups, root, mode),
    }
}

fn plan_group(
    snap: &Snapshot,
    policy: &ProtectionPolicy,
    groups: &[AppGroup],
    root: ProcessId,
    mode: EndMode,
) -> Result<EndPlan, EndRefusal> {
    let group = groups.iter().find(|g| g.root == root).ok_or(EndRefusal::Gone)?;
    if group.kind == crate::apps::ProcessKind::Kernel {
        return Err(EndRefusal::KernelThreads);
    }
    let root_info = snap.process(root).ok_or(EndRefusal::Gone)?;
    if let Protection::Protected(r) = policy.assess(root_info) {
        return Err(EndRefusal::Protected(r));
    }
    let members: Vec<&ProcessInfo> = group.members.iter().filter_map(|m| snap.process(*m)).collect();

    // The application's main process: the root, unless the root is a launcher
    // script, in which case it is the one program that script started.
    let main = if is_launcher_shell(root_info) {
        let programs: Vec<&ProcessInfo> = members
            .iter()
            .copied()
            .filter(|c| c.ppid == Some(root_info.pid) && !SHELLS.contains(&c.name.as_str()))
            .collect();
        match programs.as_slice() {
            [only] => *only,
            [] => {
                return Err(EndRefusal::Ambiguous("script de lancement sans programme identifiable".into()));
            }
            _ => {
                return Err(EndRefusal::Ambiguous(format!(
                    "le script de lancement « {} » a démarré {} programmes",
                    root_info.display_name(),
                    programs.len()
                )));
            }
        }
    } else {
        root_info
    };
    if let Protection::Protected(r) = policy.assess(main) {
        return Err(EndRefusal::Protected(r));
    }

    // Verified descendants of the main process, within the group.
    let in_group: HashSet<u32> = members.iter().map(|p| p.pid).collect();
    let mut family: Vec<&ProcessInfo> = vec![main];
    let mut seen: HashSet<u32> = HashSet::from([main.pid]);
    let mut i = 0;
    while i < family.len() {
        let parent = family[i].pid;
        for c in &members {
            if c.ppid == Some(parent) && in_group.contains(&c.pid) && seen.insert(c.pid) {
                family.push(c);
            }
        }
        i += 1;
    }
    if family.iter().any(|p| p.pid == policy.self_pid()) {
        return Err(EndRefusal::ContainsSelf);
    }

    let (targets, excluded_protected) = match mode {
        EndMode::Graceful => (vec![target(main)], 0),
        EndMode::Force => {
            let (allowed, protected): (Vec<&ProcessInfo>, Vec<&ProcessInfo>) =
                family.iter().copied().partition(|p| policy.assess(p).is_allowed());
            (allowed.into_iter().map(target).collect(), protected.len())
        }
    };
    Ok(EndPlan { name: group.name.clone(), pid: main.pid, mode, targets, excluded_protected, is_group: true })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::group_apps;
    use crate::model::fixtures::proc;

    const ME: u32 = 1000;
    const SELF_PID: u32 = 9000;

    fn policy() -> ProtectionPolicy {
        ProtectionPolicy::new(SELF_PID, ME)
    }

    fn plan(snap: &Snapshot, key: RowKey, mode: EndMode) -> Result<EndPlan, EndRefusal> {
        let groups = group_apps(snap, &policy());
        plan_end(snap, &policy(), &groups, key, mode)
    }

    fn app_key(snap: &Snapshot, name: &str) -> RowKey {
        let groups = group_apps(snap, &policy());
        RowKey::App(groups.iter().find(|g| g.name == name).unwrap_or_else(|| panic!("group {name}")).root)
    }

    fn session() -> Snapshot {
        let mut c = vec![
            proc(1, None, "systemd", 0),
            proc(100, Some(1), "Hyprland", ME),
            proc(200, Some(100), "firefox", ME),
            proc(201, Some(200), "Web Content", ME),
            proc(202, Some(200), "Web Content", ME),
            proc(203, Some(201), "RDD Process", ME),
            proc(300, Some(100), "kitty", ME),
            proc(301, Some(300), "fish", ME),
            proc(SELF_PID, Some(301), "dotmon", ME),
            proc(400, Some(100), "mpv", ME),
        ];
        // A root-owned helper spawned by mpv: protected, must be spared.
        let mut helper = proc(401, Some(400), "mpv-helper", 0);
        helper.user = "root".into();
        c.push(helper);
        Snapshot { processes: c, ..Default::default() }
    }

    fn ids(p: &EndPlan) -> Vec<u32> {
        p.targets.iter().map(|t| t.id.pid).collect()
    }

    #[test]
    fn single_process_plan_names_exactly_one_identity() {
        let s = session();
        let id = s.processes.iter().find(|p| p.pid == 400).unwrap().id();
        let p = plan(&s, RowKey::Process(id), EndMode::Graceful).unwrap();
        assert_eq!((p.pid, p.mode, p.is_group), (400, EndMode::Graceful, false));
        assert_eq!(p.targets, vec![EndTarget { id, name: "mpv".into() }]);
        let p = plan(&s, RowKey::Process(id), EndMode::Force).unwrap();
        assert_eq!(p.mode, EndMode::Force, "the mode is part of the plan");
    }

    #[test]
    fn single_process_never_includes_its_children() {
        let s = session();
        let id = s.processes.iter().find(|p| p.pid == 200).unwrap().id();
        for mode in [EndMode::Graceful, EndMode::Force] {
            assert_eq!(ids(&plan(&s, RowKey::Process(id), mode).unwrap()), vec![200]);
        }
    }

    #[test]
    fn protected_processes_are_refused_one_by_one() {
        let s = session();
        for pid in [1, 100] {
            let id = s.processes.iter().find(|p| p.pid == pid).unwrap().id();
            assert!(matches!(plan(&s, RowKey::Process(id), EndMode::Force), Err(EndRefusal::Protected(_))));
        }
        let me = s.processes.iter().find(|p| p.pid == SELF_PID).unwrap().id();
        assert_eq!(
            plan(&s, RowKey::Process(me), EndMode::Graceful),
            Err(EndRefusal::Protected(ProtectionReason::SelfProcess))
        );
    }

    #[test]
    fn other_users_processes_are_refused() {
        let mut s = session();
        s.processes.push(proc(500, Some(1), "cupsd", 0));
        let id = s.processes.iter().find(|p| p.pid == 500).unwrap().id();
        assert!(matches!(
            plan(&s, RowKey::Process(id), EndMode::Graceful),
            Err(EndRefusal::Protected(ProtectionReason::OtherUser(_)))
        ));
    }

    #[test]
    fn unknown_process_is_gone() {
        let s = session();
        let ghost = ProcessId { pid: 4242, start_time: 1 };
        assert_eq!(plan(&s, RowKey::Process(ghost), EndMode::Force), Err(EndRefusal::Gone));
        assert_eq!(plan(&s, RowKey::App(ghost), EndMode::Force), Err(EndRefusal::Gone));
    }

    #[test]
    fn graceful_on_an_application_targets_only_its_main_process() {
        let s = session();
        let p = plan(&s, app_key(&s, "firefox"), EndMode::Graceful).unwrap();
        assert_eq!(ids(&p), vec![200]);
        assert!(p.is_group);
    }

    #[test]
    fn force_on_an_application_lists_every_verified_descendant() {
        let s = session();
        let p = plan(&s, app_key(&s, "firefox"), EndMode::Force).unwrap();
        let mut got = ids(&p);
        got.sort();
        assert_eq!(got, vec![200, 201, 202, 203]);
        assert_eq!(p.pid, 200);
        assert_eq!(p.excluded_protected, 0);
    }

    #[test]
    fn protected_members_are_excluded_never_targeted() {
        let s = session();
        let p = plan(&s, app_key(&s, "mpv"), EndMode::Force).unwrap();
        assert_eq!(ids(&p), vec![400], "the root-owned helper is spared");
        assert_eq!(p.excluded_protected, 1);
    }

    #[test]
    fn group_containing_dotmon_is_refused() {
        let s = session();
        for mode in [EndMode::Graceful, EndMode::Force] {
            assert_eq!(plan(&s, app_key(&s, "kitty"), mode), Err(EndRefusal::ContainsSelf));
        }
        // ...but the other processes of the session are unaffected.
        assert!(plan(&s, app_key(&s, "firefox"), EndMode::Force).is_ok());
    }

    #[test]
    fn session_and_kernel_groups_are_refused() {
        let mut s = session();
        let mut k = proc(77, Some(2), "kworker/0:1", 0);
        k.kernel_thread = true;
        s.processes.push(k);
        let groups = group_apps(&s, &policy());
        let kernel = groups.iter().find(|g| g.kind == crate::apps::ProcessKind::Kernel).unwrap();
        assert_eq!(
            plan_end(&s, &policy(), &groups, RowKey::App(kernel.root), EndMode::Force),
            Err(EndRefusal::KernelThreads)
        );
        let hypr = groups.iter().find(|g| g.name == "Hyprland").unwrap();
        assert!(matches!(
            plan_end(&s, &policy(), &groups, RowKey::App(hypr.root), EndMode::Force),
            Err(EndRefusal::Protected(_))
        ));
    }

    fn launcher_snapshot(programs: &[(u32, &str)]) -> Snapshot {
        let mut script = proc(600, Some(100), "bash", ME);
        script.cmdline = "bash /home/u/launch_first_available.sh".into();
        let mut procs = vec![proc(1, None, "systemd", 0), proc(100, Some(1), "Hyprland", ME), script];
        for (pid, name) in programs {
            procs.push(proc(*pid, Some(600), name, ME));
        }
        procs.push(proc(699, Some(*programs.first().map(|p| &p.0).unwrap_or(&600)), "child", ME));
        Snapshot { processes: procs, ..Default::default() }
    }

    #[test]
    fn launcher_script_group_targets_the_program_not_the_script() {
        let s = launcher_snapshot(&[(601, "kitty")]);
        let key = app_key(&s, "kitty");
        let p = plan(&s, key, EndMode::Graceful).unwrap();
        assert_eq!(ids(&p), vec![601], "SIGTERM goes to the program, not to the wrapper script");
        assert_eq!(p.pid, 601);
        let mut f = ids(&plan(&s, key, EndMode::Force).unwrap());
        f.sort();
        assert_eq!(f, vec![601, 699], "the script itself is not part of the application");
    }

    #[test]
    fn launcher_script_starting_several_programs_is_ambiguous() {
        let s = launcher_snapshot(&[(601, "kitty"), (602, "firefox")]);
        for mode in [EndMode::Graceful, EndMode::Force] {
            let r = plan(&s, app_key(&s, "kitty"), mode);
            assert!(matches!(r, Err(EndRefusal::Ambiguous(_))), "{r:?}");
        }
        assert!(EndRefusal::Ambiguous("x".into()).message().contains("vue PROCESSUS"));
    }

    #[test]
    fn launcher_with_a_protected_program_is_refused() {
        let mut s = launcher_snapshot(&[(601, "pipewire")]);
        s.processes.iter_mut().find(|p| p.pid == 601).unwrap().name = "pipewire".into();
        let groups = group_apps(&s, &policy());
        let g = groups.iter().find(|g| g.root.pid == 600).unwrap();
        assert!(matches!(
            plan_end(&s, &policy(), &groups, RowKey::App(g.root), EndMode::Force),
            Err(EndRefusal::Protected(_))
        ));
    }

    #[test]
    fn a_process_that_appears_later_is_never_part_of_an_existing_plan() {
        let mut s = session();
        let key = app_key(&s, "firefox");
        let before = plan(&s, key, EndMode::Force).unwrap();
        s.processes.push(proc(204, Some(200), "Web Content", ME));
        assert!(!before.ids().iter().any(|id| id.pid == 204), "plans are fixed lists of identities");
    }
}
