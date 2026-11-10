//! Background sampling thread.
//!
//! The engine owns the [`MetricsSource`] and the [`ProcessController`]. The
//! UI thread talks to it only through [`Command`]s and receives [`Event`]s
//! through a callback, so a slow system call can never freeze the interface.
//!
//! Safety:
//! - the engine never ends a process on its own: it only acts on an explicit
//!   [`Command::End`], which carries the exact identity and the exact signal
//!   the user confirmed;
//! - it re-applies the protection policy against its most recent snapshot
//!   before forwarding to the controller, which in turn re-verifies the
//!   identity with the kernel;
//! - a delivered signal is only a request: the engine then *watches* the
//!   target and reports whether it really ended or is still running. It never
//!   escalates SIGTERM to SIGKILL by itself.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dotmon_core::model::{ProcessId, Snapshot};
use dotmon_core::protection::{Protection, ProtectionPolicy};
use dotmon_core::source::{ControlError, EndMode, EndOutcome, MetricsSource, ProcessController};

#[derive(Debug, Clone)]
pub enum Command {
    SetInterval(Duration),
    /// End `target` with `mode`. `batch` groups the commands of one
    /// confirmation so their results can be reported together.
    End {
        batch: u64,
        target: ProcessId,
        mode: EndMode,
    },
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum Event {
    Snapshot(Arc<Snapshot>),
    /// The signal was delivered (or refused): nothing more is implied.
    EndResult {
        batch: u64,
        target: ProcessId,
        name: String,
        result: Result<(), ControlError>,
    },
    /// What became of the process after a delivered signal.
    EndOutcome {
        batch: u64,
        target: ProcessId,
        name: String,
        outcome: EndOutcome,
        waited: Duration,
    },
}

/// How long the engine watches a target after signalling it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// Interval between exit checks while something is being watched.
    pub poll: Duration,
    /// Observation window after SIGTERM (applications may save their work).
    pub graceful_wait: Duration,
    /// Observation window after SIGKILL.
    pub force_wait: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            poll: Duration::from_millis(100),
            graceful_wait: Duration::from_secs(5),
            force_wait: Duration::from_secs(3),
        }
    }
}

struct Watch {
    batch: u64,
    target: ProcessId,
    name: String,
    mode: EndMode,
    since: Instant,
}

pub struct Engine {
    tx: Sender<Command>,
    thread: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn spawn(
        source: Box<dyn MetricsSource>,
        controller: Box<dyn ProcessController>,
        policy: ProtectionPolicy,
        interval: Duration,
        sink: impl Fn(Event) + Send + 'static,
    ) -> Self {
        Self::spawn_with(source, controller, policy, interval, Timing::default(), sink)
    }

    pub fn spawn_with(
        source: Box<dyn MetricsSource>,
        controller: Box<dyn ProcessController>,
        policy: ProtectionPolicy,
        interval: Duration,
        timing: Timing,
        sink: impl Fn(Event) + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<Command>();
        let thread = std::thread::Builder::new()
            .name("dotmon-engine".into())
            .spawn(move || run(source, controller, policy, interval, timing, rx, sink))
            .expect("failed to spawn engine thread");
        Self { tx, thread: Some(thread) }
    }

    pub fn send(&self, cmd: Command) {
        // The engine only stops on Shutdown/Drop; a send error would mean it
        // panicked, in which case there is nothing useful left to do.
        let _ = self.tx.send(cmd);
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(
    mut source: Box<dyn MetricsSource>,
    mut controller: Box<dyn ProcessController>,
    policy: ProtectionPolicy,
    mut interval: Duration,
    timing: Timing,
    rx: Receiver<Command>,
    sink: impl Fn(Event),
) {
    source.set_interval_hint(interval);
    let mut watches: Vec<Watch> = Vec::new();
    // Sample, then wait for the interval, a command or the next exit check.
    // Any command (and any observed exit) triggers an immediate refresh so
    // its effect is visible without waiting for the next tick.
    loop {
        let last = Arc::new(source.sample());
        sink(Event::Snapshot(last.clone()));
        let next_sample = Instant::now() + interval;

        loop {
            let now = Instant::now();
            let mut wake = next_sample;
            if !watches.is_empty() {
                wake = wake.min(now + timing.poll);
            }
            let mut resample = false;
            match rx.recv_timeout(wake.saturating_duration_since(now)) {
                Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
                Ok(Command::SetInterval(d)) => {
                    interval = d;
                    source.set_interval_hint(d);
                    resample = true;
                }
                Ok(Command::End { batch, target, mode }) => {
                    // Commands of one confirmation arrive together: handle
                    // them all against the same snapshot, then refresh once.
                    let mut next = Some(Command::End { batch, target, mode });
                    while let Some(cmd) = next.take() {
                        match cmd {
                            Command::End { batch, target, mode } => {
                                start_end(
                                    &last,
                                    &policy,
                                    controller.as_mut(),
                                    &mut watches,
                                    &sink,
                                    batch,
                                    target,
                                    mode,
                                );
                            }
                            Command::SetInterval(d) => {
                                interval = d;
                                source.set_interval_hint(d);
                            }
                            Command::Shutdown => return,
                        }
                        next = rx.try_recv().ok();
                    }
                    resample = true;
                }
                Err(RecvTimeoutError::Timeout) => {}
            }

            if check_watches(controller.as_mut(), &mut watches, &timing, &sink) {
                resample = true;
            }
            if resample || Instant::now() >= next_sample {
                break;
            }
        }
    }
}

/// Validates and sends one signal; on success the target is watched.
#[allow(clippy::too_many_arguments)]
fn start_end(
    last: &Snapshot,
    policy: &ProtectionPolicy,
    controller: &mut dyn ProcessController,
    watches: &mut Vec<Watch>,
    sink: &impl Fn(Event),
    batch: u64,
    target: ProcessId,
    mode: EndMode,
) {
    let (name, result) = guarded_end(Some(last), policy, controller, target, mode);
    let delivered = result.is_ok();
    sink(Event::EndResult { batch, target, name: name.clone(), result });
    if delivered {
        watches.push(Watch { batch, target, name, mode, since: Instant::now() });
    }
}

/// Reports every watched target that ended or whose window closed. Returns
/// whether anything was reported (the list changed: refresh it).
fn check_watches(
    controller: &mut dyn ProcessController,
    watches: &mut Vec<Watch>,
    timing: &Timing,
    sink: &impl Fn(Event),
) -> bool {
    let mut reported = false;
    watches.retain(|w| {
        let waited = w.since.elapsed();
        let window = match w.mode {
            EndMode::Graceful => timing.graceful_wait,
            EndMode::Force => timing.force_wait,
        };
        let outcome = if controller.has_exited(w.target) {
            EndOutcome::Exited
        } else if waited >= window {
            EndOutcome::StillRunning
        } else {
            return true;
        };
        controller.forget(w.target);
        sink(Event::EndOutcome { batch: w.batch, target: w.target, name: w.name.clone(), outcome, waited });
        reported = true;
        false
    });
    reported
}

/// Second line of defence: re-check the target against the latest snapshot
/// and the protection policy before signalling it.
fn guarded_end(
    last: Option<&Snapshot>,
    policy: &ProtectionPolicy,
    controller: &mut dyn ProcessController,
    target: ProcessId,
    mode: EndMode,
) -> (String, Result<(), ControlError>) {
    let Some(info) = last.and_then(|s| s.process(target)) else {
        return (format!("PID {}", target.pid), Err(ControlError::NotFound));
    };
    let name = info.display_name().to_string();
    if let Protection::Protected(reason) = policy.assess(info) {
        return (name, Err(ControlError::Protected(reason)));
    }
    (name, controller.end(target, mode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dotmon_sys::mock::mock_backend;
    use std::sync::Mutex;

    const ME: u32 = 1000;
    const SELF_PID: u32 = 77_777;

    fn fast() -> Timing {
        Timing {
            poll: Duration::from_millis(10),
            graceful_wait: Duration::from_millis(300),
            force_wait: Duration::from_millis(300),
        }
    }

    fn start_with(ctl: Box<dyn ProcessController>, interval_ms: u64) -> (Engine, Receiver<Event>) {
        let (src, _) = mock_backend(3, SELF_PID, ME, "tester");
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let engine = Engine::spawn_with(
            Box::new(src),
            ctl,
            ProtectionPolicy::new(SELF_PID, ME),
            Duration::from_millis(interval_ms),
            fast(),
            move |e| {
                let _ = tx.lock().unwrap().send(e);
            },
        );
        (engine, rx)
    }

    /// Engine on the simulation (controller and source share their state).
    fn start_sim(interval_ms: u64) -> (Engine, Receiver<Event>) {
        let (src, ctl) = mock_backend(3, SELF_PID, ME, "tester");
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let engine = Engine::spawn_with(
            Box::new(src),
            Box::new(ctl),
            ProtectionPolicy::new(SELF_PID, ME),
            Duration::from_millis(interval_ms),
            fast(),
            move |e| {
                let _ = tx.lock().unwrap().send(e);
            },
        );
        (engine, rx)
    }

    fn next_snapshot(rx: &Receiver<Event>) -> Arc<Snapshot> {
        loop {
            if let Event::Snapshot(s) = rx.recv_timeout(Duration::from_secs(5)).expect("engine stalled") {
                return s;
            }
        }
    }

    fn next_result(rx: &Receiver<Event>) -> (String, Result<(), ControlError>) {
        loop {
            if let Event::EndResult { name, result, .. } = rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                return (name, result);
            }
        }
    }

    fn next_outcome(rx: &Receiver<Event>) -> (u64, ProcessId, EndOutcome) {
        loop {
            if let Event::EndOutcome { batch, target, outcome, .. } =
                rx.recv_timeout(Duration::from_secs(5)).unwrap()
            {
                return (batch, target, outcome);
            }
        }
    }

    fn find(s: &Snapshot, name: &str) -> ProcessId {
        s.processes.iter().find(|p| p.name == name).unwrap_or_else(|| panic!("{name}")).id()
    }

    #[test]
    fn emits_snapshots_periodically() {
        let (_engine, rx) = start_sim(10);
        let a = next_snapshot(&rx);
        let b = next_snapshot(&rx);
        assert!(b.seq > a.seq);
    }

    #[test]
    fn interval_change_is_applied() {
        let (engine, rx) = start_sim(10_000);
        next_snapshot(&rx);
        engine.send(Command::SetInterval(Duration::from_millis(10)));
        // With a 10 s interval we would time out; the new interval wakes it.
        next_snapshot(&rx);
        next_snapshot(&rx);
    }

    #[test]
    fn never_ends_without_a_command() {
        let (_engine, rx) = start_sim(5);
        let mut last = next_snapshot(&rx);
        for _ in 0..30 {
            last = next_snapshot(&rx);
        }
        for name in ["firefox", "blender", "kitty", "mpv", "obsidian"] {
            assert!(last.processes.iter().any(|p| p.name == name), "{name} disappeared on its own");
        }
        // Nothing was reported either: no result, no outcome.
        assert!(rx.try_iter().all(|e| matches!(e, Event::Snapshot(_))));
    }

    #[test]
    fn end_command_on_user_app_succeeds() {
        let (engine, rx) = start_sim(50);
        let mpv = find(&next_snapshot(&rx), "mpv");
        engine.send(Command::End { batch: 1, target: mpv, mode: EndMode::Force });
        let (name, result) = next_result(&rx);
        assert_eq!((name.as_str(), result), ("mpv", Ok(())));
        assert_eq!(next_outcome(&rx), (1, mpv, EndOutcome::Exited));
        assert!(next_snapshot(&rx).process(mpv).is_none());
    }

    #[test]
    fn protected_processes_are_refused_by_the_engine() {
        let (engine, rx) = start_sim(50);
        let s = next_snapshot(&rx);
        for name in ["Hyprland", "qs", "pipewire", "systemd", "dotmon"] {
            engine.send(Command::End { batch: 1, target: find(&s, name), mode: EndMode::Force });
            let (_, result) = next_result(&rx);
            assert!(matches!(result, Err(ControlError::Protected(_))), "{name}: {result:?}");
        }
        assert!(next_snapshot(&rx).processes.iter().any(|p| p.name == "Hyprland"));
    }

    #[test]
    fn unknown_target_reports_not_found() {
        let (engine, rx) = start_sim(50);
        next_snapshot(&rx);
        engine.send(Command::End {
            batch: 1,
            target: ProcessId { pid: 3_999_999, start_time: 1 },
            mode: EndMode::Graceful,
        });
        assert_eq!(next_result(&rx).1, Err(ControlError::NotFound));
    }

    #[test]
    fn other_users_processes_are_refused_by_the_engine() {
        let (engine, rx) = start_sim(50);
        let cups = find(&next_snapshot(&rx), "cupsd");
        engine.send(Command::End { batch: 1, target: cups, mode: EndMode::Graceful });
        assert!(matches!(next_result(&rx).1, Err(ControlError::Protected(_))));
    }

    #[test]
    fn graceful_end_is_observed_as_exited() {
        let (engine, rx) = start_sim(30);
        let mpv = find(&next_snapshot(&rx), "mpv");
        engine.send(Command::End { batch: 5, target: mpv, mode: EndMode::Graceful });
        assert_eq!(next_result(&rx).1, Ok(()));
        assert_eq!(next_outcome(&rx), (5, mpv, EndOutcome::Exited));
    }

    #[test]
    fn process_ignoring_sigterm_is_reported_still_running_and_never_killed() {
        let (engine, rx) = start_sim(30);
        let s = next_snapshot(&rx);
        let stubborn = find(&s, "obsidian");
        engine.send(Command::End { batch: 9, target: stubborn, mode: EndMode::Graceful });
        assert_eq!(next_result(&rx).1, Ok(()), "SIGTERM was delivered");
        assert_eq!(next_outcome(&rx), (9, stubborn, EndOutcome::StillRunning));
        // Long after the window: no escalation happened behind the user's back.
        std::thread::sleep(Duration::from_millis(400));
        let mut last = next_snapshot(&rx);
        for _ in 0..3 {
            last = next_snapshot(&rx);
        }
        assert!(last.process(stubborn).is_some(), "obsidian must still be running");
        assert!(
            rx.try_iter().all(|e| !matches!(e, Event::EndResult { .. } | Event::EndOutcome { .. })),
            "no automatic second signal"
        );
        // An explicit, separate force works.
        engine.send(Command::End { batch: 10, target: stubborn, mode: EndMode::Force });
        assert_eq!(next_result(&rx).1, Ok(()));
        assert_eq!(next_outcome(&rx), (10, stubborn, EndOutcome::Exited));
    }

    #[test]
    fn simultaneous_actions_are_all_handled_and_keep_their_batch() {
        let (engine, rx) = start_sim(30);
        let s = next_snapshot(&rx);
        let (mpv, thunar, blender) = (find(&s, "mpv"), find(&s, "thunar"), find(&s, "blender"));
        engine.send(Command::End { batch: 1, target: mpv, mode: EndMode::Graceful });
        engine.send(Command::End { batch: 2, target: thunar, mode: EndMode::Force });
        engine.send(Command::End { batch: 2, target: blender, mode: EndMode::Force });
        let mut ok = 0;
        let mut outcomes = Vec::new();
        while outcomes.len() < 3 {
            match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::EndResult { result, .. } => {
                    assert_eq!(result, Ok(()));
                    ok += 1;
                }
                Event::EndOutcome { batch, target, outcome, .. } => outcomes.push((batch, target, outcome)),
                Event::Snapshot(_) => {}
            }
        }
        assert_eq!(ok, 3);
        outcomes.sort_by_key(|o| o.1.pid);
        let mut expected = vec![
            (1, mpv, EndOutcome::Exited),
            (2, thunar, EndOutcome::Exited),
            (2, blender, EndOutcome::Exited),
        ];
        expected.sort_by_key(|o| o.1.pid);
        assert_eq!(outcomes, expected);
    }

    /// A controller that records every call, to prove what the engine sends.
    #[derive(Clone, Default)]
    struct Spy(Arc<Mutex<Vec<(ProcessId, EndMode)>>>);
    impl ProcessController for Spy {
        fn end(&mut self, target: ProcessId, mode: EndMode) -> Result<(), ControlError> {
            self.0.lock().unwrap().push((target, mode));
            Ok(())
        }
        fn has_exited(&mut self, _target: ProcessId) -> bool {
            false
        }
    }

    #[test]
    fn a_sigterm_command_only_ever_produces_one_sigterm() {
        let spy = Spy::default();
        let (engine, rx) = start_with(Box::new(spy.clone()), 20);
        let s = next_snapshot(&rx);
        let mpv = find(&s, "mpv");
        engine.send(Command::End { batch: 1, target: mpv, mode: EndMode::Graceful });
        assert_eq!(next_outcome(&rx).2, EndOutcome::StillRunning);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(*spy.0.lock().unwrap(), vec![(mpv, EndMode::Graceful)], "no SIGKILL, no repeat");
    }

    #[test]
    fn dropping_the_engine_with_pending_watches_is_clean() {
        let (engine, rx) = start_with(Box::new(Spy::default()), 20);
        let mpv = find(&next_snapshot(&rx), "mpv");
        engine.send(Command::End { batch: 1, target: mpv, mode: EndMode::Graceful });
        drop(engine);
    }

    #[test]
    fn drop_stops_the_thread() {
        let (engine, rx) = start_sim(5);
        next_snapshot(&rx);
        drop(engine);
        // Drain anything in flight, then the channel must be closed.
        while rx.recv_timeout(Duration::from_millis(200)).is_ok() {}
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
    }

    // ---- Real Linux backend, throw-away targets only --------------------------
    //
    // The targets are `sleep` processes spawned by the test itself. Nothing of
    // the user's session is ever selected as a target.

    mod real {
        use super::*;
        use dotmon_sys::linux::{LinuxController, LinuxSource};
        use std::process::{Child, Command as Proc};

        struct Jetable(Child);
        impl Jetable {
            fn spawn(ignore_term: bool) -> Self {
                let script = if ignore_term { "trap '' TERM; exec sleep 300" } else { "exec sleep 300" };
                let c = Proc::new("sh").args(["-c", script]).spawn().unwrap();
                let me = Jetable(c);
                let comm = format!("/proc/{}/comm", me.0.id());
                for _ in 0..200 {
                    if std::fs::read_to_string(&comm).is_ok_and(|c| c.trim() == "sleep") {
                        return me;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                panic!("never became sleep");
            }
            fn alive(&mut self) -> bool {
                self.0.try_wait().unwrap().is_none()
            }
        }
        impl Drop for Jetable {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        fn uid() -> u32 {
            std::fs::metadata("/proc/self").map(|m| std::os::unix::fs::MetadataExt::uid(&m)).unwrap()
        }

        fn start_real() -> (Engine, Receiver<Event>) {
            let (tx, rx) = mpsc::channel();
            let tx = Mutex::new(tx);
            let engine = Engine::spawn_with(
                Box::new(LinuxSource::new().expect("linux source")),
                Box::new(LinuxController::new()),
                ProtectionPolicy::new(std::process::id(), uid()),
                Duration::from_millis(100),
                Timing {
                    poll: Duration::from_millis(20),
                    graceful_wait: Duration::from_millis(600),
                    force_wait: Duration::from_secs(3),
                },
                move |e| {
                    let _ = tx.lock().unwrap().send(e);
                },
            );
            (engine, rx)
        }

        /// First snapshot that lists `pid`, and its identity.
        fn wait_listed(rx: &Receiver<Event>, pid: u32) -> ProcessId {
            for _ in 0..100 {
                if let Some(p) = next_snapshot(rx).processes.iter().find(|p| p.pid == pid) {
                    return p.id();
                }
            }
            panic!("pid {pid} never listed");
        }

        #[test]
        fn sigterm_then_explicit_sigkill_on_real_processes() {
            let mut polite = Jetable::spawn(false);
            let mut stubborn = Jetable::spawn(true);
            let (engine, rx) = start_real();
            let polite_id = wait_listed(&rx, polite.0.id());
            let stubborn_id = wait_listed(&rx, stubborn.0.id());

            engine.send(Command::End { batch: 1, target: polite_id, mode: EndMode::Graceful });
            engine.send(Command::End { batch: 2, target: stubborn_id, mode: EndMode::Graceful });
            let mut outcomes = std::collections::HashMap::new();
            while outcomes.len() < 2 {
                match rx.recv_timeout(Duration::from_secs(10)).expect("engine stalled") {
                    Event::EndResult { result, .. } => assert_eq!(result, Ok(())),
                    Event::EndOutcome { batch, outcome, .. } => {
                        outcomes.insert(batch, outcome);
                    }
                    Event::Snapshot(_) => {}
                }
            }
            assert_eq!(outcomes[&1], EndOutcome::Exited, "cooperative process ended");
            assert_eq!(outcomes[&2], EndOutcome::StillRunning, "SIGTERM was ignored");
            assert!(!polite.alive());
            assert!(stubborn.alive(), "no automatic SIGKILL");

            // Explicit second action.
            engine.send(Command::End { batch: 3, target: stubborn_id, mode: EndMode::Force });
            loop {
                if let Event::EndOutcome { batch: 3, outcome, .. } =
                    rx.recv_timeout(Duration::from_secs(10)).expect("engine stalled")
                {
                    assert_eq!(outcome, EndOutcome::Exited);
                    break;
                }
            }
            assert!(!stubborn.alive());
        }

        #[test]
        fn engine_refuses_dotmon_init_and_session_processes_for_real() {
            let (engine, rx) = start_real();
            let s = next_snapshot(&rx);
            // Dotmon itself (here: the test binary), and PID 1.
            let me = s.processes.iter().find(|p| p.pid == std::process::id()).expect("self listed").id();
            let init = s.processes.iter().find(|p| p.pid == 1).expect("init listed").id();
            for target in [me, init] {
                engine.send(Command::End { batch: 1, target, mode: EndMode::Force });
                let (_, r) = next_result(&rx);
                assert!(matches!(r, Err(ControlError::Protected(_))), "{r:?}");
            }
        }

        #[test]
        fn forged_identity_is_refused_for_real_and_leaves_the_process_alone() {
            let mut victim = Jetable::spawn(false);
            let (engine, rx) = start_real();
            let id = wait_listed(&rx, victim.0.id());
            let forged = ProcessId { pid: id.pid, start_time: id.start_time + 7 };
            // Not in the snapshot under that identity: refused before any signal.
            engine.send(Command::End { batch: 1, target: forged, mode: EndMode::Force });
            assert_eq!(next_result(&rx).1, Err(ControlError::NotFound));
            std::thread::sleep(Duration::from_millis(200));
            assert!(victim.alive());
        }
    }
}
