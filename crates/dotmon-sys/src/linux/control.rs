//! Ending real processes, safely.
//!
//! # How a process is addressed
//!
//! A PID alone is not an identity: once a process exits, its number can be
//! given to an unrelated one. Signalling by PID (`kill(2)`) between the moment
//! the user looked at the list and the moment the signal is sent could hit the
//! wrong process. Dotmon therefore uses **pidfds** (Linux ≥ 5.3, see
//! `pidfd_open(2)` and `pidfd_send_signal(2)`) and an identity of
//! *(PID, start time)*:
//!
//! 1. `pidfd_open(pid)`: the descriptor is bound to the process that owns
//!    that PID *now*; it keeps designating that process even if the number is
//!    recycled later (a dead process then answers `ESRCH`);
//! 2. the start time is read from `/proc/<pid>/stat` **after** the pidfd was
//!    obtained and compared with the one the user confirmed. If the process
//!    died and the number was recycled in between, the start times differ:
//!    refused. If they match, the pidfd designates the confirmed process
//!    (the same PID cannot have started before the confirmed process
//!    existed);
//! 3. `pidfd_send_signal(pidfd, SIGTERM | SIGKILL)` signals that process, and
//!    only that one (the signal is process-directed, never a group, never
//!    recursive).
//!
//! The pidfd is then kept to watch the exit: it becomes readable when the
//! process terminates, so "has it ended?" is answered for the *same* process,
//! not for whatever holds the PID later.
//!
//! Anything that cannot be verified is refused: unreadable `/proc` entry
//! (e.g. `hidepid`), unparsable start time, kernel without pidfd support.
//!
//! No root, no sudo, no Polkit: a refusal by the kernel (`EPERM`) is reported
//! as such. PID 1, PID 2, kernel threads and Dotmon itself are refused here
//! too, whatever the caller's policy says (defence in depth).

use std::collections::HashMap;
use std::io;
use std::os::fd::OwnedFd;
use std::path::PathBuf;

use dotmon_core::model::{ProcState, ProcessId};
use dotmon_core::protection::ProtectionReason;
use dotmon_core::source::{ControlError, EndMode, ProcessController};
use rustix::io::Errno;

use super::procfs;

/// Kernel operations used to end a process, behind a trait so that every
/// failure mode can be exercised without touching a real process.
pub trait Kernel: Send {
    fn pidfd_open(&self, pid: u32) -> Result<OwnedFd, Errno>;
    /// Sends SIGTERM or SIGKILL to the process the pidfd designates.
    fn send_signal(&self, pidfd: &OwnedFd, mode: EndMode) -> Result<(), Errno>;
    /// Text of `/proc/<pid>/stat`.
    fn read_stat(&self, pid: u32) -> io::Result<String>;
    /// Whether the process behind the pidfd has terminated.
    fn has_terminated(&self, pidfd: &OwnedFd) -> Result<bool, Errno>;
}

/// The real kernel, through `rustix`.
pub struct RealKernel {
    proc_root: PathBuf,
}

impl RealKernel {
    pub fn new() -> Self {
        Self { proc_root: PathBuf::from("/proc") }
    }
}

impl Default for RealKernel {
    fn default() -> Self {
        Self::new()
    }
}

impl Kernel for RealKernel {
    fn pidfd_open(&self, pid: u32) -> Result<OwnedFd, Errno> {
        let pid = i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw).ok_or(Errno::INVAL)?;
        rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty())
    }

    fn send_signal(&self, pidfd: &OwnedFd, mode: EndMode) -> Result<(), Errno> {
        let sig = match mode {
            EndMode::Graceful => rustix::process::Signal::TERM,
            EndMode::Force => rustix::process::Signal::KILL,
        };
        rustix::process::pidfd_send_signal(pidfd, sig)
    }

    fn read_stat(&self, pid: u32) -> io::Result<String> {
        std::fs::read_to_string(self.proc_root.join(pid.to_string()).join("stat"))
    }

    fn has_terminated(&self, pidfd: &OwnedFd) -> Result<bool, Errno> {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        let mut fds = [PollFd::new(pidfd, PollFlags::IN)];
        let now = Timespec { tv_sec: 0, tv_nsec: 0 };
        poll(&mut fds, Some(&now))?;
        Ok(fds[0].revents().contains(PollFlags::IN))
    }
}

/// Upper bound on pidfds kept to watch exits (they are released as soon as
/// the engine is done with a target).
const MAX_WATCHED: usize = 512;

pub struct LinuxController<K: Kernel = RealKernel> {
    kernel: K,
    self_pid: u32,
    watched: HashMap<ProcessId, OwnedFd>,
}

impl LinuxController<RealKernel> {
    pub fn new() -> Self {
        Self::with_kernel(RealKernel::new(), std::process::id())
    }
}

impl Default for LinuxController<RealKernel> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Kernel> LinuxController<K> {
    pub fn with_kernel(kernel: K, self_pid: u32) -> Self {
        Self { kernel, self_pid, watched: HashMap::new() }
    }

    /// Number of processes currently being watched.
    pub fn watched(&self) -> usize {
        self.watched.len()
    }

    /// Reads the start time and state of `pid` right now.
    fn read_identity(&self, pid: u32) -> Result<procfs::Stat, ControlError> {
        let text = match self.kernel.read_stat(pid) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(ControlError::NotFound),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                return Err(ControlError::Unverifiable("lecture de /proc refusée".into()));
            }
            Err(e) => return Err(ControlError::Unverifiable(format!("lecture de /proc impossible : {e}"))),
        };
        procfs::parse_stat(&text)
            .ok_or_else(|| ControlError::Unverifiable("/proc/<pid>/stat illisible".into()))
    }
}

impl<K: Kernel> ProcessController for LinuxController<K> {
    fn end(&mut self, target: ProcessId, mode: EndMode) -> Result<(), ControlError> {
        // Never, whatever the caller decided.
        if target.pid == 0 || target.pid == 1 || target.pid == 2 {
            return Err(ControlError::Protected(ProtectionReason::Init));
        }
        if target.pid == self.self_pid {
            return Err(ControlError::Protected(ProtectionReason::SelfProcess));
        }

        // 1. Bind to the process that owns this PID now.
        let pidfd = match self.kernel.pidfd_open(target.pid) {
            Ok(fd) => fd,
            Err(Errno::SRCH) => return Err(ControlError::NotFound),
            Err(Errno::NOSYS | Errno::INVAL | Errno::PERM) => {
                return Err(ControlError::Unverifiable("pidfd indisponible".into()));
            }
            Err(Errno::MFILE | Errno::NFILE) => {
                return Err(ControlError::Other("Trop de fichiers ouverts : signal non envoyé".into()));
            }
            Err(e) => return Err(ControlError::Other(format!("pidfd_open : {e}"))),
        };

        // 2. Verify the identity after the pidfd exists (see module docs).
        let stat = self.read_identity(target.pid)?;
        if stat.starttime != target.start_time {
            return Err(ControlError::PidReused);
        }
        if stat.state == ProcState::Zombie {
            return Err(ControlError::NotFound);
        }
        if stat.is_kernel_thread() || stat.ppid == 2 {
            return Err(ControlError::Protected(ProtectionReason::KernelThread));
        }

        // 3. Signal that process through the descriptor.
        match self.kernel.send_signal(&pidfd, mode) {
            Ok(()) => {}
            Err(Errno::SRCH) => return Err(ControlError::NotFound),
            Err(Errno::PERM) => return Err(ControlError::PermissionDenied),
            Err(e) => return Err(ControlError::Other(format!("pidfd_send_signal : {e}"))),
        }
        if self.watched.len() >= MAX_WATCHED {
            let kernel = &self.kernel;
            self.watched.retain(|_, fd| !kernel.has_terminated(fd).unwrap_or(false));
        }
        self.watched.insert(target, pidfd);
        Ok(())
    }

    fn has_exited(&mut self, target: ProcessId) -> bool {
        if let Some(fd) = self.watched.get(&target) {
            // On a polling error the state is unknown: report "still running"
            // (the engine will say so) rather than claim an exit.
            return self.kernel.has_terminated(fd).unwrap_or(false);
        }
        // Not watched (never signalled by us): fall back to the identity.
        match self.read_identity(target.pid) {
            Ok(stat) => stat.starttime != target.start_time || stat.state == ProcState::Zombie,
            Err(ControlError::NotFound) => true,
            Err(_) => false,
        }
    }

    fn forget(&mut self, target: ProcessId) {
        self.watched.remove(&target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    // ---- A scripted kernel: every failure mode, nothing real touched ------

    #[derive(Default)]
    struct Script {
        open: Option<Errno>,
        stat: Option<io::ErrorKind>,
        stat_text: Option<String>,
        send: Option<Errno>,
        terminated: bool,
        opened: Vec<u32>,
        sent: Vec<EndMode>,
    }

    #[derive(Clone, Default)]
    struct Fake(Arc<Mutex<Script>>);

    impl Fake {
        fn with(f: impl FnOnce(&mut Script)) -> Self {
            let fake = Fake::default();
            f(&mut fake.0.lock().unwrap());
            fake
        }
        fn sent(&self) -> Vec<EndMode> {
            self.0.lock().unwrap().sent.clone()
        }
        fn opened(&self) -> Vec<u32> {
            self.0.lock().unwrap().opened.clone()
        }
    }

    impl Kernel for Fake {
        fn pidfd_open(&self, pid: u32) -> Result<OwnedFd, Errno> {
            let mut s = self.0.lock().unwrap();
            s.opened.push(pid);
            match s.open {
                Some(e) => Err(e),
                None => Ok(std::fs::File::open("/dev/null").unwrap().into()),
            }
        }
        fn send_signal(&self, _fd: &OwnedFd, mode: EndMode) -> Result<(), Errno> {
            let mut s = self.0.lock().unwrap();
            match s.send {
                Some(e) => Err(e),
                None => {
                    s.sent.push(mode);
                    Ok(())
                }
            }
        }
        fn read_stat(&self, _pid: u32) -> io::Result<String> {
            let s = self.0.lock().unwrap();
            match (&s.stat, &s.stat_text) {
                (Some(kind), _) => Err(io::Error::from(*kind)),
                (None, Some(t)) => Ok(t.clone()),
                (None, None) => Ok(stat_line(4000, 'S', 1, 0)),
            }
        }
        fn has_terminated(&self, _fd: &OwnedFd) -> Result<bool, Errno> {
            Ok(self.0.lock().unwrap().terminated)
        }
    }

    /// A `/proc/<pid>/stat` line with the given start time, state, ppid, flags.
    fn stat_line(start: u64, state: char, ppid: u32, flags: u32) -> String {
        format!("4242 (job) {state} {ppid} 4242 4242 0 -1 {flags} 0 0 0 0 1 1 0 0 20 0 1 0 {start} 0 0")
    }

    const ID: ProcessId = ProcessId { pid: 4242, start_time: 4000 };
    const SELF: u32 = 777;

    fn controller(fake: &Fake) -> LinuxController<Fake> {
        LinuxController::with_kernel(fake.clone(), SELF)
    }

    #[test]
    fn sigterm_and_sigkill_send_exactly_the_requested_signal() {
        let fake = Fake::default();
        let mut c = controller(&fake);
        assert_eq!(c.end(ID, EndMode::Graceful), Ok(()));
        assert_eq!(fake.sent(), vec![EndMode::Graceful], "a SIGTERM request never sends SIGKILL");
        assert_eq!(c.end(ID, EndMode::Force), Ok(()));
        assert_eq!(fake.sent(), vec![EndMode::Graceful, EndMode::Force]);
    }

    #[test]
    fn identity_mismatch_sends_nothing() {
        let fake = Fake::with(|s| s.stat_text = Some(stat_line(9999, 'S', 1, 0)));
        let mut c = controller(&fake);
        assert_eq!(c.end(ID, EndMode::Force), Err(ControlError::PidReused));
        assert!(fake.sent().is_empty());
        assert_eq!(c.watched(), 0);
    }

    #[test]
    fn vanished_process_is_reported_as_already_ended() {
        let gone = Fake::with(|s| s.open = Some(Errno::SRCH));
        assert_eq!(controller(&gone).end(ID, EndMode::Graceful), Err(ControlError::NotFound));
        // Died between pidfd_open and the identity check.
        let late = Fake::with(|s| s.stat = Some(io::ErrorKind::NotFound));
        assert_eq!(controller(&late).end(ID, EndMode::Graceful), Err(ControlError::NotFound));
        // Died between the check and the signal.
        let racing = Fake::with(|s| s.send = Some(Errno::SRCH));
        assert_eq!(controller(&racing).end(ID, EndMode::Force), Err(ControlError::NotFound));
        for f in [gone, late, racing] {
            assert!(f.sent().is_empty());
        }
    }

    #[test]
    fn zombie_is_already_ended() {
        let fake = Fake::with(|s| s.stat_text = Some(stat_line(4000, 'Z', 1, 0)));
        assert_eq!(controller(&fake).end(ID, EndMode::Force), Err(ControlError::NotFound));
        assert!(fake.sent().is_empty());
    }

    #[test]
    fn kernel_permission_refusal_is_reported_not_worked_around() {
        let fake = Fake::with(|s| s.send = Some(Errno::PERM));
        assert_eq!(controller(&fake).end(ID, EndMode::Graceful), Err(ControlError::PermissionDenied));
        assert!(fake.sent().is_empty(), "no retry, no escalation");
    }

    #[test]
    fn unverifiable_identity_is_refused() {
        for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::Other] {
            let fake = Fake::with(|s| s.stat = Some(kind));
            let r = controller(&fake).end(ID, EndMode::Force);
            assert!(matches!(r, Err(ControlError::Unverifiable(_))), "{kind:?}: {r:?}");
            assert!(fake.sent().is_empty());
        }
        let garbage = Fake::with(|s| s.stat_text = Some("not a stat line".into()));
        assert!(matches!(controller(&garbage).end(ID, EndMode::Force), Err(ControlError::Unverifiable(_))));
        let no_pidfd = Fake::with(|s| s.open = Some(Errno::NOSYS));
        assert!(matches!(controller(&no_pidfd).end(ID, EndMode::Force), Err(ControlError::Unverifiable(_))));
        assert!(garbage.sent().is_empty() && no_pidfd.sent().is_empty());
    }

    #[test]
    fn init_self_and_kernel_threads_are_refused_without_touching_them() {
        let fake = Fake::default();
        let mut c = controller(&fake);
        for pid in [0, 1, 2] {
            let r = c.end(ProcessId { pid, start_time: 1 }, EndMode::Force);
            assert_eq!(r, Err(ControlError::Protected(ProtectionReason::Init)), "pid {pid}");
        }
        let me = ProcessId { pid: SELF, start_time: 1 };
        assert_eq!(c.end(me, EndMode::Graceful), Err(ControlError::Protected(ProtectionReason::SelfProcess)));
        assert!(fake.opened().is_empty(), "not even a pidfd is opened for them");

        // A kernel thread, whatever the caller thought it was.
        let kthread = Fake::with(|s| s.stat_text = Some(stat_line(4000, 'I', 2, 0x0020_0000)));
        assert_eq!(
            controller(&kthread).end(ID, EndMode::Force),
            Err(ControlError::Protected(ProtectionReason::KernelThread))
        );
        assert!(kthread.sent().is_empty());
    }

    #[test]
    fn exit_is_observed_on_the_same_process_and_watch_is_released() {
        let fake = Fake::default();
        let mut c = controller(&fake);
        c.end(ID, EndMode::Graceful).unwrap();
        assert_eq!(c.watched(), 1);
        assert!(!c.has_exited(ID), "signal delivered ≠ process ended");
        fake.0.lock().unwrap().terminated = true;
        assert!(c.has_exited(ID));
        c.forget(ID);
        assert_eq!(c.watched(), 0);
    }

    #[test]
    fn unwatched_process_exit_falls_back_to_identity() {
        let alive = Fake::default();
        assert!(!controller(&alive).has_exited(ID));
        let recycled = Fake::with(|s| s.stat_text = Some(stat_line(5555, 'S', 1, 0)));
        assert!(controller(&recycled).has_exited(ID), "the PID now belongs to someone else");
        let gone = Fake::with(|s| s.stat = Some(io::ErrorKind::NotFound));
        assert!(controller(&gone).has_exited(ID));
        let unknown = Fake::with(|s| s.stat = Some(io::ErrorKind::PermissionDenied));
        assert!(!controller(&unknown).has_exited(ID), "cannot tell: never claim an exit");
    }
}
