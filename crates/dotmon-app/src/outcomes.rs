//! Turns the engine's results into what the user is told.
//!
//! A delivered signal is only a request, so the story has up to three
//! chapters, each reported honestly:
//!
//! 1. the signal was **sent** (or refused, with the exact reason);
//! 2. the process **ended**, or
//! 3. it is **still running** after the observation window (nothing is
//!    escalated: forcing is a separate, confirmed action).
//!
//! The results of one confirmation (a batch) are grouped, so that forcing an
//! application made of twelve processes produces one readable message and
//! not twelve toasts replacing one another.

use std::collections::HashMap;
use std::time::Duration;

use dotmon_core::source::{ControlError, EndMode, EndOutcome};

use crate::presenter::PendingEnd;

/// A message for the toast: `warning` marks anything the user should look at
/// (refusals, processes still running).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub text: String,
    pub warning: bool,
}

impl Note {
    fn info(text: String) -> Option<Self> {
        Some(Self { text, warning: false })
    }
    fn warn(text: String) -> Option<Self> {
        Some(Self { text, warning: true })
    }
}

#[derive(Debug)]
struct Batch {
    label: String,
    mode: EndMode,
    total: usize,
    /// Results received (delivered or refused).
    answered: usize,
    delivered: usize,
    refused: Vec<String>,
    /// Outcomes received for delivered signals.
    finished: usize,
    exited: usize,
    still_running: usize,
    waited: Duration,
}

impl Batch {
    fn done(&self) -> bool {
        self.answered == self.total && self.finished == self.delivered
    }
}

#[derive(Debug, Default)]
pub struct Tracker {
    batches: HashMap<u64, Batch>,
}

fn who(name: &str, pid: u32) -> String {
    format!("« {name} » (PID {pid})")
}

impl Tracker {
    /// Registers a confirmed request before its commands are sent.
    pub fn begin(&mut self, pending: &PendingEnd) {
        self.batches.insert(
            pending.batch,
            Batch {
                label: pending.name.clone(),
                mode: pending.mode,
                total: pending.targets.len(),
                answered: 0,
                delivered: 0,
                refused: Vec::new(),
                finished: 0,
                exited: 0,
                still_running: 0,
                waited: Duration::ZERO,
            },
        );
    }

    /// Number of requests still being followed.
    #[cfg(test)]
    pub fn open_batches(&self) -> usize {
        self.batches.len()
    }

    pub fn on_result(
        &mut self,
        batch: u64,
        pid: u32,
        name: &str,
        result: &Result<(), ControlError>,
    ) -> Option<Note> {
        let b = self.batches.get_mut(&batch)?;
        b.answered += 1;
        match result {
            Ok(()) => b.delivered += 1,
            Err(e) => b.refused.push(format!("{} : {e}", who(name, pid))),
        }
        let sig = b.mode.signal_name();
        let note = if b.total == 1 {
            match result {
                Ok(()) => {
                    Note::info(format!("{sig} envoyé à {} — en attente de sa fermeture", who(name, pid)))
                }
                Err(e) => Note::warn(format!("{} : {e}", who(name, pid))),
            }
        } else if b.answered == b.total {
            let refused = b.refused.len();
            match (b.delivered, refused) {
                (0, _) => Note::warn(format!("« {} » : aucun signal envoyé — {}", b.label, b.refused[0])),
                (n, 0) => Note::info(format!("{sig} envoyé à {n} processus de « {} »", b.label)),
                (n, r) => Note::warn(format!(
                    "{sig} envoyé à {n} processus de « {} », {r} refusé(s) — {}",
                    b.label, b.refused[0]
                )),
            }
        } else {
            None
        };
        if b.done() {
            self.batches.remove(&batch);
        }
        note
    }

    pub fn on_outcome(
        &mut self,
        batch: u64,
        pid: u32,
        name: &str,
        outcome: EndOutcome,
        waited: Duration,
    ) -> Option<Note> {
        let b = self.batches.get_mut(&batch)?;
        b.finished += 1;
        b.waited = b.waited.max(waited);
        match outcome {
            EndOutcome::Exited => b.exited += 1,
            EndOutcome::StillRunning => b.still_running += 1,
        }
        let sig = b.mode.signal_name();
        let secs = waited.as_secs().max(1);
        let note = if b.total == 1 {
            match (outcome, b.mode) {
                (EndOutcome::Exited, _) => Note::info(format!("{} s'est terminé", who(name, pid))),
                (EndOutcome::StillRunning, EndMode::Graceful) => Note::warn(format!(
                    "{} est toujours actif {secs} s après {sig}. Rien n'a été forcé : utilisez FORCER si nécessaire",
                    who(name, pid)
                )),
                (EndOutcome::StillRunning, EndMode::Force) => Note::warn(format!(
                    "{} est toujours actif {secs} s après {sig} (le noyau ne l'a pas encore terminé)",
                    who(name, pid)
                )),
            }
        } else if b.answered == b.total && b.finished == b.delivered {
            let (e, s) = (b.exited, b.still_running);
            if s == 0 {
                Note::info(format!("« {} » : {e} processus terminés", b.label))
            } else {
                Note::warn(format!(
                    "« {} » : {e} terminé(s), {s} toujours actif(s) {secs} s après {sig}",
                    b.label
                ))
            }
        } else {
            None
        };
        if b.done() {
            self.batches.remove(&batch);
        }
        note
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dotmon_core::model::ProcessId;
    use dotmon_core::query::RowKey;

    fn pending(batch: u64, mode: EndMode, n: u32) -> PendingEnd {
        let targets: Vec<ProcessId> = (0..n).map(|i| ProcessId { pid: 100 + i, start_time: 1 }).collect();
        PendingEnd {
            batch,
            key: RowKey::Process(targets[0]),
            name: "firefox".into(),
            pid: 100,
            mode,
            targets,
            is_group: n > 1,
            excluded_protected: 0,
        }
    }

    const SECS_5: Duration = Duration::from_secs(5);

    #[test]
    fn single_process_story_sent_then_ended() {
        let mut t = Tracker::default();
        t.begin(&pending(1, EndMode::Graceful, 1));
        let n = t.on_result(1, 100, "firefox", &Ok(())).unwrap();
        assert!(n.text.contains("SIGTERM envoyé") && n.text.contains("PID 100"), "{}", n.text);
        assert!(!n.warning);
        let n = t.on_outcome(1, 100, "firefox", EndOutcome::Exited, Duration::from_millis(800)).unwrap();
        assert!(n.text.contains("s'est terminé") && !n.warning, "{}", n.text);
        assert_eq!(t.open_batches(), 0, "finished batches are forgotten");
    }

    #[test]
    fn a_sent_signal_is_not_reported_as_an_ended_process() {
        let mut t = Tracker::default();
        t.begin(&pending(1, EndMode::Graceful, 1));
        let n = t.on_result(1, 100, "firefox", &Ok(())).unwrap();
        assert!(!n.text.contains("terminé"), "sending is not ending: {}", n.text);
        assert_eq!(t.open_batches(), 1, "still waiting for the outcome");
    }

    #[test]
    fn still_running_after_sigterm_says_so_and_does_not_force() {
        let mut t = Tracker::default();
        t.begin(&pending(1, EndMode::Graceful, 1));
        t.on_result(1, 100, "firefox", &Ok(()));
        let n = t.on_outcome(1, 100, "firefox", EndOutcome::StillRunning, SECS_5).unwrap();
        assert!(n.warning);
        assert!(n.text.contains("toujours actif") && n.text.contains("SIGTERM"), "{}", n.text);
        assert!(n.text.contains("Rien n'a été forcé"), "{}", n.text);
    }

    #[test]
    fn still_running_after_sigkill_is_reported_too() {
        let mut t = Tracker::default();
        t.begin(&pending(1, EndMode::Force, 1));
        t.on_result(1, 100, "x", &Ok(()));
        let n = t.on_outcome(1, 100, "x", EndOutcome::StillRunning, Duration::from_secs(3)).unwrap();
        assert!(n.warning && n.text.contains("SIGKILL"), "{}", n.text);
    }

    #[test]
    fn every_refusal_has_its_own_precise_message() {
        use dotmon_core::protection::ProtectionReason;
        let cases: Vec<(ControlError, &str)> = vec![
            (ControlError::PermissionDenied, "Accès refusé"),
            (ControlError::NotFound, "déjà terminé"),
            (ControlError::PidReused, "Identité du processus modifiée"),
            (ControlError::Protected(ProtectionReason::Init), "protégé"),
            (ControlError::Unverifiable("x".into()), "non vérifiable"),
        ];
        for (err, expect) in cases {
            let mut t = Tracker::default();
            t.begin(&pending(1, EndMode::Force, 1));
            let n = t.on_result(1, 100, "app", &Err(err.clone())).unwrap();
            assert!(n.warning, "{err:?}");
            assert!(n.text.contains(expect), "{err:?} → {}", n.text);
            assert!(n.text.contains("PID 100"));
            assert_eq!(t.open_batches(), 0, "nothing was delivered: nothing to wait for");
        }
    }

    #[test]
    fn group_is_reported_once_not_once_per_process() {
        let mut t = Tracker::default();
        t.begin(&pending(7, EndMode::Force, 3));
        assert!(t.on_result(7, 100, "a", &Ok(())).is_none());
        assert!(t.on_result(7, 101, "b", &Ok(())).is_none());
        let n = t.on_result(7, 102, "c", &Ok(())).unwrap();
        assert!(n.text.contains("SIGKILL envoyé à 3 processus de « firefox »"), "{}", n.text);
        assert!(t.on_outcome(7, 100, "a", EndOutcome::Exited, SECS_5).is_none());
        assert!(t.on_outcome(7, 101, "b", EndOutcome::Exited, SECS_5).is_none());
        let n = t.on_outcome(7, 102, "c", EndOutcome::Exited, SECS_5).unwrap();
        assert!(n.text.contains("3 processus terminés") && !n.warning, "{}", n.text);
        assert_eq!(t.open_batches(), 0);
    }

    #[test]
    fn group_with_survivors_and_refusals_is_flagged() {
        let mut t = Tracker::default();
        t.begin(&pending(7, EndMode::Force, 3));
        t.on_result(7, 100, "a", &Ok(()));
        t.on_result(7, 101, "b", &Err(ControlError::PermissionDenied));
        let n = t.on_result(7, 102, "c", &Ok(())).unwrap();
        assert!(n.warning && n.text.contains("1 refusé"), "{}", n.text);
        t.on_outcome(7, 100, "a", EndOutcome::Exited, SECS_5);
        let n = t.on_outcome(7, 102, "c", EndOutcome::StillRunning, Duration::from_secs(3)).unwrap();
        assert!(n.warning && n.text.contains("1 terminé(s), 1 toujours actif"), "{}", n.text);
    }

    #[test]
    fn group_where_nothing_was_sent_says_why() {
        let mut t = Tracker::default();
        t.begin(&pending(7, EndMode::Force, 2));
        t.on_result(7, 100, "a", &Err(ControlError::NotFound));
        let n = t.on_result(7, 101, "b", &Err(ControlError::NotFound)).unwrap();
        assert!(n.warning && n.text.contains("aucun signal envoyé"), "{}", n.text);
    }

    #[test]
    fn concurrent_batches_do_not_mix() {
        let mut t = Tracker::default();
        t.begin(&pending(1, EndMode::Graceful, 1));
        t.begin(&pending(2, EndMode::Force, 1));
        let a = t.on_result(1, 100, "one", &Ok(())).unwrap();
        let b = t.on_result(2, 100, "two", &Ok(())).unwrap();
        assert!(a.text.contains("SIGTERM") && b.text.contains("SIGKILL"));
        assert!(t.on_outcome(2, 100, "two", EndOutcome::Exited, SECS_5).unwrap().text.contains("two"));
        assert_eq!(t.open_batches(), 1);
        // Unknown batch (e.g. from before): ignored.
        assert!(t.on_outcome(99, 1, "x", EndOutcome::Exited, SECS_5).is_none());
    }
}
