//! Which processes the user may end from the UI.
//!
//! The policy is deliberately conservative: anything the desktop session or
//! the operating system depends on is refused, as is anything owned by
//! another user (Dotmon never holds root privileges). The policy is applied
//! twice: once by the UI (to disable buttons and explain why) and once by the
//! engine right before a signal is sent.

use crate::model::ProcessInfo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Protection {
    Allowed,
    Protected(ProtectionReason),
}

impl Protection {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Protection::Allowed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtectionReason {
    /// PID 1 / PID 2 — the init system and the kernel thread daemon.
    Init,
    KernelThread,
    /// Dotmon itself.
    SelfProcess,
    /// Component of the graphical session or of the core system.
    Critical(String),
    /// Owned by another user; would require privileges Dotmon never requests.
    OtherUser(String),
}

impl ProtectionReason {
    /// Short French explanation displayed in the UI.
    pub fn message(&self) -> String {
        match self {
            ProtectionReason::Init => "Processus d'initialisation du système".into(),
            ProtectionReason::KernelThread => "Thread du noyau".into(),
            ProtectionReason::SelfProcess => "Dotmon lui-même".into(),
            ProtectionReason::Critical(name) => format!("Composant critique de la session ({name})"),
            ProtectionReason::OtherUser(user) => {
                format!("Appartient à « {user} » — privilèges requis")
            }
        }
    }
}

/// Names (as `comm` — 15 chars max — or executable basename) that are never
/// offered for termination.
const CRITICAL_NAMES: &[&str] = &[
    // init / core system
    "init",
    "systemd",
    "kthreadd",
    "dbus-daemon",
    "dbus-broker",
    "dbus-broker-launch",
    "dbus-broker-lau",
    "polkitd",
    "udevd",
    "seatd",
    "elogind",
    "NetworkManager",
    "iwd",
    "wpa_supplicant",
    "sshd",
    "Xorg",
    "Xwayland",
    // display managers / greeters
    "sddm",
    "sddm-helper",
    "gdm",
    "greetd",
    "ly",
    "login",
    "agetty",
    // compositors and session managers
    "Hyprland",
    "start-hyprland",
    "uwsm",
    "sway",
    "niri",
    "kwin_wayland",
    "gnome-shell",
    // desktop shell (Quickshell / illogical-impulse)
    "qs",
    "quickshell",
    // audio graph
    "pipewire",
    "pipewire-pulse",
    "wireplumber",
    // portals (session breaks without them)
    "xdg-desktop-portal",
    "xdg-desktop-portal-hyprland",
    "xdg-desktop-por",
    "xdg-document-portal",
    "xdg-permission-store",
];

/// Prefixes of names that are always critical (e.g. every `systemd-*` daemon).
const CRITICAL_PREFIXES: &[&str] = &["systemd-"];

#[derive(Debug, Clone)]
pub struct ProtectionPolicy {
    self_pid: u32,
    current_uid: u32,
    extra_names: Vec<String>,
}

impl ProtectionPolicy {
    pub fn new(self_pid: u32, current_uid: u32) -> Self {
        Self { self_pid, current_uid, extra_names: Vec::new() }
    }

    /// Add user-configured names to the critical list.
    pub fn with_extra_names(mut self, names: impl IntoIterator<Item = String>) -> Self {
        self.extra_names.extend(names);
        self
    }

    pub fn current_uid(&self) -> u32 {
        self.current_uid
    }

    pub fn self_pid(&self) -> u32 {
        self.self_pid
    }

    pub fn assess(&self, p: &ProcessInfo) -> Protection {
        use ProtectionReason::*;
        if p.pid <= 2 {
            return Protection::Protected(Init);
        }
        if p.kernel_thread || p.ppid == Some(2) {
            return Protection::Protected(KernelThread);
        }
        if p.pid == self.self_pid {
            return Protection::Protected(SelfProcess);
        }
        if let Some(name) = self.critical_name(p) {
            return Protection::Protected(Critical(name.to_string()));
        }
        if p.uid != self.current_uid {
            return Protection::Protected(OtherUser(p.user.clone()));
        }
        Protection::Allowed
    }

    fn critical_name<'a>(&self, p: &'a ProcessInfo) -> Option<&'a str> {
        let candidates = std::iter::once(p.name.as_str()).chain(p.exe_name.as_deref());
        for name in candidates {
            let hit = CRITICAL_NAMES.contains(&name)
                || CRITICAL_PREFIXES.iter().any(|pre| name.starts_with(pre))
                || self.extra_names.iter().any(|n| n == name);
            if hit {
                return Some(name);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::fixtures::proc;

    const ME: u32 = 1000;

    fn policy() -> ProtectionPolicy {
        ProtectionPolicy::new(4242, ME)
    }

    #[test]
    fn init_and_kthreadd_are_protected() {
        let p = policy();
        assert_eq!(p.assess(&proc(1, None, "systemd", 0)), Protection::Protected(ProtectionReason::Init));
        assert_eq!(p.assess(&proc(2, None, "kthreadd", 0)), Protection::Protected(ProtectionReason::Init));
    }

    #[test]
    fn kernel_threads_are_protected() {
        let mut k = proc(77, Some(2), "kworker/0:1", 0);
        assert_eq!(policy().assess(&k), Protection::Protected(ProtectionReason::KernelThread));
        k.ppid = Some(5);
        k.kernel_thread = true;
        assert_eq!(policy().assess(&k), Protection::Protected(ProtectionReason::KernelThread));
    }

    #[test]
    fn self_is_protected() {
        let me = proc(4242, Some(100), "dotmon", ME);
        assert_eq!(policy().assess(&me), Protection::Protected(ProtectionReason::SelfProcess));
    }

    #[test]
    fn session_components_are_protected_even_for_own_user() {
        for name in ["Hyprland", "qs", "pipewire", "wireplumber", "Xwayland", "systemd-logind"] {
            let p = proc(500, Some(1), name, ME);
            assert!(
                matches!(policy().assess(&p), Protection::Protected(ProtectionReason::Critical(_))),
                "{name} should be protected"
            );
        }
    }

    #[test]
    fn truncated_comm_matched_through_exe_name() {
        let mut p = proc(600, Some(1), "xdg-desktop-po", ME);
        p.exe_name = Some("xdg-desktop-portal-hyprland".into());
        assert!(!policy().assess(&p).is_allowed());
    }

    #[test]
    fn other_users_processes_are_protected() {
        let p = proc(700, Some(1), "postgres", 970);
        assert!(matches!(policy().assess(&p), Protection::Protected(ProtectionReason::OtherUser(_))));
    }

    #[test]
    fn ordinary_user_apps_are_allowed() {
        for name in ["firefox", "kitty", "nvim", "blender", "Web Content"] {
            assert_eq!(policy().assess(&proc(900, Some(800), name, ME)), Protection::Allowed);
        }
    }

    #[test]
    fn extra_names_extend_the_list() {
        let pol = policy().with_extra_names(["syncthing".to_string()]);
        assert!(!pol.assess(&proc(901, Some(1), "syncthing", ME)).is_allowed());
    }

    #[test]
    fn messages_are_human_readable() {
        assert!(ProtectionReason::Critical("qs".into()).message().contains("qs"));
        assert!(ProtectionReason::OtherUser("root".into()).message().contains("root"));
    }
}
