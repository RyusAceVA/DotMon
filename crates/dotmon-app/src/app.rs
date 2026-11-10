//! UI-thread application state: owns the presenter, wires Slint callbacks
//! and applies engine events.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use dotmon_core::protection::ProtectionPolicy;
use dotmon_core::query::{Filter, SortKey};
use dotmon_core::settings::{Accent, ChartStyle, HISTORY_CHOICES, REFRESH_CHOICES_MS, Settings};
use dotmon_core::source::EndMode;
use slint::ComponentHandle;

use crate::bridge;
use crate::engine::{Command, Engine, Event};
use crate::outcomes::Tracker;
use crate::presenter::{Presenter, ViewMode};
use crate::{Api, MainWindow};

const TOAST_MS: u64 = 3500;
/// Warnings (refusals, process still running) stay long enough to be read.
const TOAST_WARNING_MS: u64 = 8000;

pub struct App {
    ui: slint::Weak<MainWindow>,
    models: bridge::Models,
    pub presenter: RefCell<Presenter>,
    pub engine: RefCell<Option<Engine>>,
    tracker: RefCell<Tracker>,
    backend: String,
    config_path: Option<PathBuf>,
    toast_timer: slint::Timer,
}

thread_local! {
    /// The UI-thread application state, reachable from engine events.
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

pub fn with_app(f: impl FnOnce(&Rc<App>)) {
    let app = APP.with(|a| a.borrow().clone());
    if let Some(app) = app {
        f(&app);
    }
}

impl App {
    fn send(&self, cmd: Command) {
        if let Some(e) = self.engine.borrow().as_ref() {
            e.send(cmd);
        }
    }

    pub fn on_event(&self, event: Event) {
        let Some(ui) = self.ui.upgrade() else { return };
        match event {
            Event::Snapshot(snap) => {
                let had_pending = self.presenter.borrow().pending().is_some();
                self.presenter.borrow_mut().ingest(snap);
                let p = self.presenter.borrow();
                bridge::push_system(&ui, &self.models, &p, &self.backend);
                bridge::push_list(&ui, &self.models, &p);
                if had_pending && p.pending().is_none() {
                    // The target vanished while the dialog was open.
                    bridge::push_confirm(&ui, None);
                    drop(p);
                    self.toast("Le processus s'est terminé entre-temps", false);
                }
            }
            Event::EndResult { batch, target, name, result, .. } => {
                let note = self.tracker.borrow_mut().on_result(batch, target.pid, &name, &result);
                if let Some(n) = note {
                    self.toast(&n.text, n.warning);
                }
            }
            Event::EndOutcome { batch, target, name, outcome, waited, .. } => {
                let note = self.tracker.borrow_mut().on_outcome(batch, target.pid, &name, outcome, waited);
                if let Some(n) = note {
                    self.toast(&n.text, n.warning);
                }
            }
        }
    }

    #[cfg(test)]
    pub fn begin_for_test(&self, pending: &crate::presenter::PendingEnd) {
        self.tracker.borrow_mut().begin(pending);
    }

    fn toast(&self, text: &str, error: bool) {
        let Some(ui) = self.ui.upgrade() else { return };
        bridge::toast(&ui, text, error);
        let weak = self.ui.clone();
        self.toast_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(if error { TOAST_WARNING_MS } else { TOAST_MS }),
            move || {
                if let Some(ui) = weak.upgrade() {
                    ui.global::<Api>().set_toast("".into());
                }
            },
        );
    }

    fn refresh_list(&self) {
        if let Some(ui) = self.ui.upgrade() {
            bridge::push_list(&ui, &self.models, &self.presenter.borrow());
        }
    }

    fn change_settings(&self, edit: impl FnOnce(&mut Settings)) {
        let mut s = self.presenter.borrow().settings().clone();
        edit(&mut s);
        let change = self.presenter.borrow_mut().update_settings(s);
        let p = self.presenter.borrow();
        let settings = p.settings().clone();
        if change.interval {
            self.send(Command::SetInterval(Duration::from_millis(settings.refresh_ms as u64)));
        }
        if let Some(ui) = self.ui.upgrade() {
            bridge::push_settings(&ui, &settings, &self.config_display());
            bridge::push_system(&ui, &self.models, &p, &self.backend);
            bridge::push_list(&ui, &self.models, &p);
        }
        drop(p);
        if let Some(path) = &self.config_path
            && let Err(e) = settings.save(path)
        {
            eprintln!("dotmon: impossible d'enregistrer {}: {e}", path.display());
            self.toast("Réglages non enregistrés (voir le terminal)", true);
        }
    }

    fn config_display(&self) -> String {
        match &self.config_path {
            Some(p) => {
                let home = dirs::home_dir().unwrap_or_default();
                match p.strip_prefix(&home) {
                    Ok(rel) => format!("~/{}", rel.display()),
                    Err(_) => p.display().to_string(),
                }
            }
            None => "(non enregistré)".into(),
        }
    }
}

fn wire_callbacks(ui: &MainWindow, app: &Rc<App>) {
    let api = ui.global::<Api>();

    let a = app.clone();
    api.on_search_changed(move |text| {
        a.presenter.borrow_mut().set_search(&text);
        a.refresh_list();
    });
    let a = app.clone();
    api.on_mode_changed(move |i| {
        a.presenter.borrow_mut().set_mode(if i == 1 { ViewMode::Processes } else { ViewMode::Apps });
        a.refresh_list();
    });
    let a = app.clone();
    api.on_filter_changed(move |i| {
        let f = match i {
            1 => Filter::Apps,
            2 => Filter::Services,
            3 => Filter::System,
            _ => Filter::All,
        };
        a.presenter.borrow_mut().set_filter(f);
        a.refresh_list();
    });
    let a = app.clone();
    api.on_sort_clicked(move |i| {
        let k = match i {
            0 => SortKey::Name,
            1 => SortKey::Pid,
            3 => SortKey::Memory,
            4 => SortKey::Threads,
            5 => SortKey::Io,
            _ => SortKey::Cpu,
        };
        a.presenter.borrow_mut().toggle_sort(k);
        a.refresh_list();
    });
    let a = app.clone();
    api.on_row_clicked(move |i| {
        a.presenter.borrow_mut().select_index(i.max(0) as usize);
        a.refresh_list();
    });
    let a = app.clone();
    api.on_close_detail(move || {
        a.presenter.borrow_mut().clear_selection();
        a.refresh_list();
    });

    // Termination: request → explicit confirmation → engine (which re-checks).
    let a = app.clone();
    api.on_request_end(move |force| {
        let mode = if force { EndMode::Force } else { EndMode::Graceful };
        let result = a.presenter.borrow_mut().request_end(mode).cloned();
        match result {
            Ok(pending) => {
                if let Some(ui) = a.ui.upgrade() {
                    bridge::push_confirm(&ui, Some(&pending));
                }
            }
            Err(msg) => a.toast(&msg, true),
        }
    });
    let a = app.clone();
    api.on_confirm_end(move || {
        let pending = a.presenter.borrow_mut().confirm();
        if let Some(ui) = a.ui.upgrade() {
            bridge::push_confirm(&ui, None);
        }
        if let Some(p) = pending {
            // One confirmation = one signal, for the identities it named.
            a.tracker.borrow_mut().begin(&p);
            for target in &p.targets {
                a.send(Command::End { batch: p.batch, target: *target, mode: p.mode });
            }
        }
    });
    let a = app.clone();
    api.on_cancel_end(move || {
        a.presenter.borrow_mut().cancel();
        if let Some(ui) = a.ui.upgrade() {
            bridge::push_confirm(&ui, None);
        }
    });

    // Settings.
    let a = app.clone();
    api.on_set_refresh(move |i| {
        if let Some(ms) = REFRESH_CHOICES_MS.get(i as usize) {
            a.change_settings(|s| s.refresh_ms = *ms);
        }
    });
    let a = app.clone();
    api.on_set_history(move |i| {
        if let Some(n) = HISTORY_CHOICES.get(i as usize) {
            a.change_settings(|s| s.history_len = *n);
        }
    });
    let a = app.clone();
    api.on_set_accent(move |i| a.change_settings(|s| s.accent = Accent::from_index(i.max(0) as usize)));
    let a = app.clone();
    api.on_set_chart_style(move |i| {
        a.change_settings(|s| s.chart_style = if i == 1 { ChartStyle::Dots } else { ChartStyle::Line })
    });
    let a = app.clone();
    api.on_set_animations(move |v| a.change_settings(|s| s.animations = v));
    let a = app.clone();
    api.on_set_dot_grid(move |v| a.change_settings(|s| s.dot_grid = v));
    let a = app.clone();
    api.on_set_show_kernel(move |v| a.change_settings(|s| s.show_kernel_threads = v));
}

impl App {
    /// Builds the state, installs models and callbacks, and registers it as
    /// the UI thread's application. The engine is attached separately.
    pub fn install(
        ui: &MainWindow,
        settings: Settings,
        policy: ProtectionPolicy,
        backend: &str,
        config_path: Option<PathBuf>,
    ) -> Rc<App> {
        let app = Rc::new(App {
            ui: ui.as_weak(),
            models: bridge::Models::install(ui),
            presenter: RefCell::new(Presenter::new(settings.clone(), policy)),
            engine: RefCell::new(None),
            tracker: RefCell::new(Tracker::default()),
            backend: backend.to_string(),
            config_path,
            toast_timer: slint::Timer::default(),
        });
        APP.with(|a| *a.borrow_mut() = Some(app.clone()));
        bridge::push_settings(ui, &settings, &app.config_display());
        wire_callbacks(ui, &app);
        app
    }

    /// Unregisters the application and stops the engine.
    pub fn shutdown(&self) {
        self.engine.borrow_mut().take();
        APP.with(|a| a.borrow_mut().take());
    }
}
