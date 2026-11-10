//! UI state machine, independent of Slint so it can be unit-tested.
//!
//! The presenter holds everything the interface needs to remember between
//! two snapshots: histories, the current query, the selection and the
//! pending (not yet confirmed) termination request.

use std::sync::Arc;

use dotmon_core::actions::plan_end;
use dotmon_core::apps::{AppGroup, ProcessKind, group_apps};
use dotmon_core::history::{ProcessHistory, SystemHistory};
use dotmon_core::model::{ProcState, ProcessId, Snapshot};
use dotmon_core::protection::{Protection, ProtectionPolicy};
use dotmon_core::query::{Filter, ProcessQuery, Row, RowKey, SortKey, app_rows, process_rows};
use dotmon_core::settings::Settings;
use dotmon_core::source::EndMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Apps,
    Processes,
}

/// A termination waiting for the user's explicit confirmation. It is bound
/// to one signal (`mode`) and to exact identities (`targets`): confirming it
/// can neither switch to another signal nor reach another process.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingEnd {
    /// Unique per request; ties the engine's results to this confirmation.
    pub batch: u64,
    pub key: RowKey,
    pub name: String,
    /// PID of the process (or of the application's main process).
    pub pid: u32,
    pub mode: EndMode,
    pub targets: Vec<ProcessId>,
    pub is_group: bool,
    /// Application members that are protected and will not be touched.
    pub excluded_protected: usize,
}

/// Everything the detail panel shows about the selection.
#[derive(Debug, Clone, PartialEq)]
pub struct Detail {
    pub name: String,
    pub kind: ProcessKind,
    pub pid: u32,
    pub user: String,
    pub state: ProcState,
    pub cmdline: String,
    pub cpu: Option<f32>,
    pub memory: u64,
    pub threads: u32,
    pub io_bps: Option<u64>,
    pub count: usize,
    /// Seconds since boot.
    pub started_s: u64,
    pub protection: Protection,
    pub cpu_series: Vec<f32>,
    pub mem_series: Vec<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SettingsChange {
    pub interval: bool,
    pub history: bool,
}

pub struct Presenter {
    policy: ProtectionPolicy,
    settings: Settings,
    snapshot: Option<Arc<Snapshot>>,
    history: SystemHistory,
    proc_history: ProcessHistory,
    query: ProcessQuery,
    mode: ViewMode,
    groups: Vec<AppGroup>,
    rows: Vec<Row>,
    selected: Option<RowKey>,
    pending: Option<PendingEnd>,
    next_batch: u64,
    /// Whether ending processes is allowed with the current backend.
    actions_enabled: bool,
}

impl Presenter {
    pub fn new(settings: Settings, policy: ProtectionPolicy) -> Self {
        let cap = settings.history_len as usize;
        let query = ProcessQuery { show_kernel: settings.show_kernel_threads, ..Default::default() };
        Self {
            policy,
            settings,
            snapshot: None,
            history: SystemHistory::new(cap),
            proc_history: ProcessHistory::new(cap),
            query,
            mode: ViewMode::default(),
            groups: Vec::new(),
            rows: Vec::new(),
            selected: None,
            pending: None,
            next_batch: 0,
            actions_enabled: true,
        }
    }

    // ----- inputs -------------------------------------------------------

    pub fn ingest(&mut self, snap: Arc<Snapshot>) {
        self.history.record(&snap);
        self.proc_history.record(&snap);
        self.groups = group_apps(&snap, &self.policy);
        self.snapshot = Some(snap);
        // A selection whose process vanished is dropped, and so is a pending
        // request: the user confirmed nothing about whatever comes next.
        if let Some(key) = self.selected
            && !self.exists(key)
        {
            self.selected = None;
        }
        if let Some(p) = &self.pending
            && !self.exists(p.key)
        {
            self.pending = None;
        }
        self.rebuild_rows();
    }

    pub fn set_search(&mut self, text: &str) {
        self.query.search = text.to_string();
        self.rebuild_rows();
    }

    pub fn set_filter(&mut self, filter: Filter) {
        self.query.filter = filter;
        self.rebuild_rows();
    }

    pub fn toggle_sort(&mut self, key: SortKey) {
        self.query.toggle_sort(key);
        self.rebuild_rows();
    }

    pub fn set_mode(&mut self, mode: ViewMode) {
        if self.mode != mode {
            self.mode = mode;
            self.selected = None;
            self.rebuild_rows();
        }
    }

    /// Selects the row at `index` in the current list; out of range clears.
    pub fn select_index(&mut self, index: usize) {
        self.selected = self.rows.get(index).map(|r| r.key);
    }

    pub fn clear_selection(&mut self) {
        self.selected = None;
    }

    pub fn update_settings(&mut self, new: Settings) -> SettingsChange {
        let new = new.sanitized();
        let change = SettingsChange {
            interval: new.refresh_ms != self.settings.refresh_ms,
            history: new.history_len != self.settings.history_len,
        };
        if change.history {
            self.history.resize(new.history_len as usize);
            self.proc_history.resize(new.history_len as usize);
        }
        if new.show_kernel_threads != self.settings.show_kernel_threads {
            self.query.show_kernel = new.show_kernel_threads;
            self.settings = new;
            self.rebuild_rows();
        } else {
            self.settings = new;
        }
        change
    }

    // ----- termination flow ----------------------------------------------

    /// Prepares a termination of the selection. Nothing is sent until
    /// [`Presenter::confirm`] is called. What is confirmed is fixed here: the
    /// signal and the exact identities (PID + start time) of the targets.
    pub fn request_end(&mut self, mode: EndMode) -> Result<&PendingEnd, String> {
        if !self.actions_enabled {
            return Err("Fermeture des processus désactivée".into());
        }
        let key = self.selected.ok_or("Aucune sélection")?;
        let snap = self.snapshot.as_ref().ok_or("Aucune donnée")?;
        let plan = plan_end(snap, &self.policy, &self.groups, key, mode).map_err(|r| r.message())?;
        self.next_batch += 1;
        self.pending = Some(PendingEnd {
            batch: self.next_batch,
            key,
            name: plan.name.clone(),
            pid: plan.pid,
            mode,
            targets: plan.ids(),
            is_group: plan.is_group,
            excluded_protected: plan.excluded_protected,
        });
        Ok(self.pending.as_ref().unwrap())
    }

    /// The user confirmed: hands over the signals to send (once).
    pub fn confirm(&mut self) -> Option<PendingEnd> {
        self.pending.take()
    }

    pub fn cancel(&mut self) {
        self.pending = None;
    }

    // ----- outputs --------------------------------------------------------

    /// Enables or disables the termination flow (disabled for real
    /// processes until that feature is validated separately).
    pub fn set_actions_enabled(&mut self, enabled: bool) {
        self.actions_enabled = enabled;
        if !enabled {
            self.pending = None;
        }
    }

    pub fn actions_enabled(&self) -> bool {
        self.actions_enabled
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }
    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_deref()
    }
    pub fn history(&self) -> &SystemHistory {
        &self.history
    }
    pub fn query(&self) -> &ProcessQuery {
        &self.query
    }
    pub fn mode(&self) -> ViewMode {
        self.mode
    }
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }
    pub fn selected(&self) -> Option<RowKey> {
        self.selected
    }
    pub fn pending(&self) -> Option<&PendingEnd> {
        self.pending.as_ref()
    }
    pub fn app_count(&self) -> usize {
        self.groups.iter().filter(|g| g.kind == ProcessKind::App).count()
    }

    pub fn detail(&self) -> Option<Detail> {
        let key = self.selected?;
        let snap = self.snapshot.as_ref()?;
        match key {
            RowKey::Process(id) => {
                let p = snap.process(id)?;
                let s = self.proc_history.get(id);
                Some(Detail {
                    name: p.display_name().to_string(),
                    kind: dotmon_core::apps::classify(p, &self.policy),
                    pid: p.pid,
                    user: p.user.clone(),
                    state: p.state,
                    cmdline: p.cmdline.clone(),
                    cpu: p.cpu,
                    memory: p.memory,
                    threads: p.threads,
                    io_bps: p.io_bps(),
                    count: 1,
                    started_s: p.started_s,
                    protection: self.policy.assess(p),
                    cpu_series: s.map(|s| s.cpu.values().collect()).unwrap_or_default(),
                    mem_series: s.map(|s| s.memory.values().collect()).unwrap_or_default(),
                })
            }
            RowKey::App(root) => {
                let g = self.group(root)?;
                let r = snap.process(root)?;
                let (cpu_series, mem_series) = self.proc_history.sum(&g.members);
                let protection = if g.kind == ProcessKind::Kernel {
                    Protection::Protected(dotmon_core::protection::ProtectionReason::KernelThread)
                } else {
                    self.policy.assess(r)
                };
                Some(Detail {
                    name: g.name.clone(),
                    kind: g.kind,
                    pid: root.pid,
                    user: g.user.clone(),
                    state: r.state,
                    cmdline: r.cmdline.clone(),
                    cpu: g.cpu,
                    memory: g.memory,
                    threads: g.threads,
                    io_bps: g.io_bps,
                    count: g.count(),
                    started_s: r.started_s,
                    protection,
                    cpu_series,
                    mem_series,
                })
            }
        }
    }

    // ----- internals ------------------------------------------------------

    fn group(&self, root: ProcessId) -> Option<&AppGroup> {
        self.groups.iter().find(|g| g.root == root)
    }

    fn exists(&self, key: RowKey) -> bool {
        match key {
            RowKey::Process(id) => self.snapshot.as_ref().is_some_and(|s| s.process(id).is_some()),
            RowKey::App(root) => self.group(root).is_some(),
        }
    }

    fn rebuild_rows(&mut self) {
        let Some(snap) = &self.snapshot else {
            self.rows.clear();
            return;
        };
        let mut rows = match self.mode {
            ViewMode::Apps => app_rows(snap, &self.groups, &self.policy),
            ViewMode::Processes => process_rows(snap, &self.policy),
        };
        self.query.apply(&mut rows);
        self.rows = rows;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dotmon_core::source::MetricsSource;
    use dotmon_sys::mock::mock_backend;

    const ME: u32 = 1000;
    const SELF_PID: u32 = 55_555;

    fn setup() -> (Presenter, dotmon_sys::mock::MockSource, dotmon_sys::mock::MockController) {
        let (mut src, ctl) = mock_backend(11, SELF_PID, ME, "tester");
        let mut p = Presenter::new(Settings::default(), ProtectionPolicy::new(SELF_PID, ME));
        p.ingest(Arc::new(src.sample()));
        (p, src, ctl)
    }

    fn select_named(p: &mut Presenter, name: &str) {
        let i = p.rows().iter().position(|r| r.name == name).unwrap_or_else(|| panic!("{name} not listed"));
        p.select_index(i);
    }

    #[test]
    fn apps_mode_groups_children() {
        let (p, ..) = setup();
        let ff = p.rows().iter().find(|r| r.name == "firefox").unwrap();
        assert_eq!(ff.count, 5);
        assert!(p.rows().iter().all(|r| r.name != "Web Content"));
        assert!(p.app_count() >= 5);
    }

    #[test]
    fn processes_mode_lists_everything_but_kernel_threads() {
        let (mut p, ..) = setup();
        p.set_mode(ViewMode::Processes);
        assert!(p.rows().iter().any(|r| r.name == "Web Content"));
        assert!(p.rows().iter().all(|r| r.kind != ProcessKind::Kernel));
        let mut s = p.settings().clone();
        s.show_kernel_threads = true;
        p.update_settings(s);
        assert!(p.rows().iter().any(|r| r.kind == ProcessKind::Kernel));
    }

    #[test]
    fn search_filters_rows() {
        let (mut p, ..) = setup();
        p.set_search("blend");
        assert_eq!(p.rows().len(), 1);
        assert_eq!(p.rows()[0].name, "blender");
        p.set_search("");
        assert!(p.rows().len() > 5);
    }

    #[test]
    fn selection_survives_resort_and_detail_has_history() {
        let (mut p, mut src, _) = setup();
        select_named(&mut p, "blender");
        p.toggle_sort(SortKey::Name);
        p.ingest(Arc::new(src.sample()));
        p.ingest(Arc::new(src.sample()));
        let d = p.detail().unwrap();
        assert_eq!(d.name, "blender");
        assert_eq!(d.cpu_series.len(), 3);
        assert!(d.protection.is_allowed());
    }

    #[test]
    fn app_detail_aggregates_members() {
        let (mut p, mut src, _) = setup();
        select_named(&mut p, "firefox");
        p.ingest(Arc::new(src.sample()));
        let d = p.detail().unwrap();
        assert_eq!(d.count, 5);
        assert_eq!(d.cpu_series.len(), 2);
        let last = *d.cpu_series.last().unwrap();
        assert!((last - d.cpu.unwrap()).abs() < 1e-3, "summed history must match current total");
    }

    #[test]
    fn end_requires_confirmation_and_is_sent_once() {
        let (mut p, ..) = setup();
        select_named(&mut p, "mpv");
        let pending = p.request_end(EndMode::Graceful).unwrap().clone();
        assert_eq!(pending.name, "mpv");
        assert_eq!(pending.targets.len(), 1);
        assert_eq!(p.confirm(), Some(pending));
        assert_eq!(p.confirm(), None, "a confirmation must not be replayable");
    }

    #[test]
    fn cancel_discards_request() {
        let (mut p, ..) = setup();
        select_named(&mut p, "mpv");
        p.request_end(EndMode::Force).unwrap();
        p.cancel();
        assert_eq!(p.confirm(), None);
    }

    #[test]
    fn protected_selection_cannot_be_ended() {
        let (mut p, ..) = setup();
        for name in ["Hyprland", "qs", "pipewire", "systemd", "cupsd"] {
            select_named(&mut p, name);
            assert!(p.request_end(EndMode::Force).is_err(), "{name} must be protected");
            assert!(p.pending().is_none());
        }
    }

    #[test]
    fn self_is_protected_in_process_mode() {
        let (mut p, ..) = setup();
        p.set_mode(ViewMode::Processes);
        select_named(&mut p, "dotmon");
        let err = p.request_end(EndMode::Force).unwrap_err();
        assert!(err.contains("Dotmon"));
    }

    #[test]
    fn force_on_app_targets_the_main_process_and_its_verified_descendants() {
        let (mut p, ..) = setup();
        select_named(&mut p, "firefox");
        let g = p.request_end(EndMode::Force).unwrap().clone();
        assert_eq!(g.targets.len(), 5);
        assert!(g.is_group && g.excluded_protected == 0);
        p.cancel();
        let g = p.request_end(EndMode::Graceful).unwrap().clone();
        assert_eq!(g.targets.len(), 1, "graceful close targets the main process only");
    }

    #[test]
    fn vanished_selection_clears_pending_request() {
        use dotmon_core::source::ProcessController;
        let (mut p, mut src, mut ctl) = setup();
        p.set_mode(ViewMode::Processes);
        select_named(&mut p, "thunar");
        let pending = p.request_end(EndMode::Graceful).unwrap().clone();
        // Process ends by other means before the user confirms.
        ctl.end(pending.targets[0], EndMode::Force).unwrap();
        p.ingest(Arc::new(src.sample()));
        assert!(p.selected().is_none());
        assert!(p.pending().is_none());
    }

    #[test]
    fn confirmation_is_bound_to_its_signal_and_cannot_become_sigkill() {
        let (mut p, ..) = setup();
        select_named(&mut p, "mpv");
        p.request_end(EndMode::Graceful).unwrap();
        // Asking for Force replaces the request: it must be confirmed anew.
        let forced = p.request_end(EndMode::Force).unwrap().clone();
        assert_eq!(forced.mode, EndMode::Force);
        p.cancel();
        let term = p.request_end(EndMode::Graceful).unwrap().clone();
        let got = p.confirm().expect("confirmed");
        assert_eq!(got.mode, EndMode::Graceful, "a SIGTERM confirmation yields SIGTERM only");
        assert_eq!(got, term);
        assert!(p.confirm().is_none());
    }

    #[test]
    fn every_request_gets_a_fresh_batch_and_a_replayed_one_is_empty() {
        let (mut p, ..) = setup();
        select_named(&mut p, "mpv");
        let a = p.request_end(EndMode::Graceful).unwrap().batch;
        let first = p.confirm().unwrap();
        assert_eq!(first.batch, a);
        let b = p.request_end(EndMode::Graceful).unwrap().batch;
        assert!(b > a, "batches are monotonic: an old confirmation cannot be mistaken for a new one");
        p.cancel();
        assert!(p.confirm().is_none(), "cancelled request cannot be confirmed afterwards");
    }

    #[test]
    fn pending_targets_are_exact_identities_with_start_time() {
        let (mut p, ..) = setup();
        select_named(&mut p, "mpv");
        let pe = p.request_end(EndMode::Force).unwrap().clone();
        let snap = p.snapshot().unwrap();
        let mpv = snap.processes.iter().find(|x| x.name == "mpv").unwrap();
        assert_eq!(pe.targets, vec![mpv.id()]);
        assert_eq!(pe.pid, mpv.pid);
        assert_ne!(mpv.id().start_time, 0);
    }

    #[test]
    fn reused_pid_with_a_new_identity_cancels_the_request() {
        let (mut p, mut src, _) = setup();
        p.set_mode(ViewMode::Processes);
        select_named(&mut p, "mpv");
        let pe = p.request_end(EndMode::Force).unwrap().clone();
        // Same PID, different start time: another process now owns that number.
        let mut snap = src.sample();
        for q in snap.processes.iter_mut().filter(|q| q.pid == pe.pid) {
            q.start_time += 1;
        }
        p.ingest(Arc::new(snap));
        assert!(p.pending().is_none(), "the confirmed identity no longer exists");
        assert!(p.confirm().is_none());
    }

    #[test]
    fn application_closure_never_targets_dotmon_itself() {
        let (mut p, ..) = setup();
        select_named(&mut p, "kitty");
        let err = p.request_end(EndMode::Force).unwrap_err();
        assert!(err.contains("Dotmon"), "{err}");
        assert!(p.request_end(EndMode::Graceful).is_err());
        assert!(p.pending().is_none());
    }

    #[test]
    fn settings_change_reports_what_changed() {
        let (mut p, ..) = setup();
        let mut s = p.settings().clone();
        s.refresh_ms = 2000;
        assert_eq!(p.update_settings(s.clone()), SettingsChange { interval: true, history: false });
        s.history_len = 60;
        assert_eq!(p.update_settings(s), SettingsChange { interval: false, history: true });
    }

    #[test]
    fn disabled_actions_never_open_a_confirmation() {
        let (mut p, ..) = setup();
        p.set_actions_enabled(false);
        select_named(&mut p, "mpv");
        assert!(p.request_end(EndMode::Graceful).is_err());
        assert!(p.request_end(EndMode::Force).is_err());
        assert!(p.pending().is_none());
        assert!(p.confirm().is_none());
    }

    #[test]
    fn mode_switch_clears_selection() {
        let (mut p, ..) = setup();
        select_named(&mut p, "mpv");
        p.set_mode(ViewMode::Processes);
        assert!(p.selected().is_none());
    }
}
