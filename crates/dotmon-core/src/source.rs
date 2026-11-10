//! Boundaries between the core and the system backends.
//!
//! `dotmon-sys` implements these traits (mock today, Linux tomorrow); the
//! engine in `dotmon-app` only ever sees the traits.

use crate::model::{ProcessId, Snapshot};
use crate::protection::ProtectionReason;

/// Produces snapshots of the machine. Called from a background thread.
pub trait MetricsSource: Send {
    /// Human-readable backend name (shown in the UI footer).
    fn name(&self) -> &str;
    /// Take one sample. Rates (CPU %, bytes/s) are computed against the
    /// previous call, so the first sample may report zeros.
    fn sample(&mut self) -> Snapshot;
    /// Informs the source of the sampling interval. Real backends measure
    /// elapsed time themselves and can ignore it; the simulation uses it to
    /// advance its clock.
    fn set_interval_hint(&mut self, _interval: std::time::Duration) {}
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndMode {
    /// SIGTERM: ask the application to close cleanly.
    Graceful,
    /// SIGKILL: stop immediately, unsaved data is lost.
    Force,
}

impl EndMode {
    pub fn signal_name(self) -> &'static str {
        match self {
            EndMode::Graceful => "SIGTERM",
            EndMode::Force => "SIGKILL",
        }
    }
}

/// What actually became of a process after a signal was delivered. Sending
/// a signal only *asks*: the process may ignore SIGTERM, or take its time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndOutcome {
    /// The process (this exact identity) has terminated.
    Exited,
    /// Still running when the observation window closed. Dotmon does not
    /// escalate by itself: forcing is a separate, confirmed action.
    StillRunning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlError {
    /// The process no longer exists (it had already ended).
    NotFound,
    /// The PID now belongs to a different process (start time mismatch).
    PidReused,
    /// The kernel refused the signal (not our process, no privileges).
    PermissionDenied,
    Protected(ProtectionReason),
    /// The identity (PID + start time) could not be established reliably:
    /// nothing is sent.
    Unverifiable(String),
    /// Ending processes is switched off for this backend.
    Disabled,
    Other(String),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlError::NotFound => write!(f, "Processus déjà terminé"),
            ControlError::PidReused => {
                write!(f, "Identité du processus modifiée (PID réattribué) : rien n'a été envoyé")
            }
            ControlError::PermissionDenied => write!(f, "Accès refusé par le système"),
            ControlError::Protected(r) => write!(f, "Processus protégé : {}", r.message()),
            ControlError::Unverifiable(why) => {
                write!(f, "Identité non vérifiable ({why}) : rien n'a été envoyé")
            }
            ControlError::Disabled => write!(f, "Fermeture des processus désactivée"),
            ControlError::Other(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ControlError {}

/// Sends termination signals. Implementations must verify that `target`
/// still designates the same process (PID *and* start time) before acting,
/// and refuse when that cannot be established.
pub trait ProcessController: Send {
    /// Sends the signal of `mode` to exactly `target`. `Ok` means the signal
    /// was **delivered**, not that the process ended: see [`Self::has_exited`].
    fn end(&mut self, target: ProcessId, mode: EndMode) -> Result<(), ControlError>;

    /// After a successful [`Self::end`]: has that exact process terminated?
    /// A process that cannot be found any more counts as exited.
    fn has_exited(&mut self, target: ProcessId) -> bool;

    /// Releases whatever was kept to watch `target` (e.g. a pidfd).
    fn forget(&mut self, _target: ProcessId) {}
}
