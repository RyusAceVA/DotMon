//! Headless UI integration test.
//!
//! Renders the real `MainWindow` with Slint's software renderer on a fake
//! platform (no compositor, controlled clock), drives it through the same
//! callbacks the user triggers, and checks the resulting UI state. Frames are
//! written to `target/shots/*.ppm` for visual review.
//!
//! Slint allows a single platform per process, so everything lives in one
//! test function.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use dotmon_core::protection::ProtectionPolicy;
use dotmon_core::settings::Settings;
use dotmon_core::source::MetricsSource;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, WindowAdapter};
use slint::{ComponentHandle, Model, PhysicalSize, Rgb8Pixel};

use crate::app::App;
use crate::engine::Event;
use crate::{Api, MainWindow, Theme};

thread_local! {
    static CLOCK: Cell<Duration> = const { Cell::new(Duration::ZERO) };
}

struct HeadlessPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for HeadlessPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }
    fn duration_since_start(&self) -> Duration {
        CLOCK.with(|c| c.get())
    }
}

/// Advance the fake clock so timers fire and animations complete.
fn advance(ms: u64) {
    CLOCK.with(|c| c.set(c.get() + Duration::from_millis(ms)));
    slint::platform::update_timers_and_animations();
}

struct Frame {
    w: usize,
    h: usize,
    px: Vec<Rgb8Pixel>,
}

impl Frame {
    fn save(&self, name: &str) {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/shots");
        std::fs::create_dir_all(&dir).unwrap();
        let mut data = format!("P6\n{} {}\n255\n", self.w, self.h).into_bytes();
        for p in &self.px {
            data.extend_from_slice(&[p.r, p.g, p.b]);
        }
        std::fs::write(dir.join(format!("{name}.ppm")), data).unwrap();
    }

    fn count(&self, pred: impl Fn(&Rgb8Pixel) -> bool) -> usize {
        self.px.iter().filter(|p| pred(p)).count()
    }
}

fn render(window: &MinimalSoftwareWindow, w: u32, h: u32) -> Frame {
    window.set_size(PhysicalSize::new(w, h));
    advance(2000);
    let mut px = vec![Rgb8Pixel { r: 0, g: 0, b: 0 }; (w * h) as usize];
    window.request_redraw();
    window.draw_if_needed(|r| {
        r.render(&mut px, w as usize);
    });
    Frame { w: w as usize, h: h as usize, px }
}

fn row_index(ui: &MainWindow, name: &str) -> i32 {
    let rows = ui.global::<Api>().get_rows();
    rows.iter().position(|r| r.name == name).unwrap_or_else(|| panic!("{name} not in list")) as i32
}

#[test]
fn headless_ui_flow() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(HeadlessPlatform { window: window.clone() })).unwrap();

    const ME: u32 = 1000;
    const SELF_PID: u32 = 424_242;
    let ui = MainWindow::new().unwrap();
    ui.show().unwrap();
    let policy = ProtectionPolicy::new(SELF_PID, ME);
    // No config path: the test never writes the user's settings.
    let app = App::install(&ui, Settings::default(), policy, "SIMULATION", None);
    let (mut src, _ctl) = dotmon_sys::mock::mock_backend(crate::MOCK_SEED, SELF_PID, ME, "tester");
    for _ in 0..90 {
        app.on_event(Event::Snapshot(Arc::new(src.sample())));
    }
    let api = ui.global::<Api>();

    // --- Dashboard -----------------------------------------------------------
    ui.set_page(0);
    let f = render(&window, 1380, 880);
    f.save("01-dashboard");
    assert!(f.count(|p| p.r > 200 && p.g > 200 && p.b > 190) > 2000, "off-white content should be drawn");
    assert!(f.count(|p| p.r > 180 && p.g < 90 && p.b < 90) > 20, "red accent should be visible");

    // --- Processes: list, selection, detail -------------------------------------
    ui.set_page(1);
    assert!(api.get_rows().row_count() > 5);
    api.invoke_row_clicked(row_index(&ui, "firefox"));
    let d = api.get_detail();
    assert!(d.present);
    assert_eq!(d.name, "firefox");
    assert_eq!(d.count, "5");
    assert!(!d.protected);
    render(&window, 1380, 880).save("02-processes");

    // --- Force end needs confirmation; cancel discards it ---------------------
    api.invoke_request_end(true);
    let c = api.get_confirm();
    assert!(c.open && c.force);
    assert_eq!(c.count, 5);
    render(&window, 1380, 880).save("03-confirm-force");
    api.invoke_cancel_end();
    assert!(!api.get_confirm().open);
    assert!(app.presenter.borrow().pending().is_none());

    // --- Graceful close of a single process, confirmed ---------------------------
    api.invoke_row_clicked(row_index(&ui, "mpv"));
    api.invoke_request_end(false);
    let c = api.get_confirm();
    assert!(c.open && !c.force);
    assert_eq!(c.name, "mpv");
    assert!(c.body.contains("« mpv » (PID "), "name and PID are shown: {}", c.body);
    assert!(c.body.contains("SIGTERM") && !c.body.contains("SIGKILL"), "{}", c.body);
    api.invoke_confirm_end();
    assert!(!api.get_confirm().open);
    assert!(app.presenter.borrow().pending().is_none(), "confirmation consumed");
    api.invoke_confirm_end(); // replaying a confirmation does nothing
    assert!(!api.get_confirm().open);

    // The forced variant is a different, explicit dialog with its own wording.
    api.invoke_row_clicked(row_index(&ui, "mpv"));
    api.invoke_request_end(true);
    let c = api.get_confirm();
    assert!(c.open && c.force && c.body.contains("SIGKILL") && c.body.contains("perdues"), "{}", c.body);
    api.invoke_cancel_end();

    // Engine outcomes are reported precisely (sent ≠ ended; still running is a warning).
    {
        use crate::engine::Event as Ev;
        use dotmon_core::model::ProcessId;
        use dotmon_core::source::EndOutcome;
        let id = ProcessId { pid: 1234, start_time: 1 };
        api.invoke_row_clicked(row_index(&ui, "mpv"));
        api.invoke_request_end(false);
        let batch = app.presenter.borrow().pending().unwrap().batch;
        // Not sending to the engine here (none attached): feed the tracker by hand.
        let pending = app.presenter.borrow_mut().confirm().unwrap();
        app.begin_for_test(&pending);
        api.invoke_cancel_end(); // close the dialog (the request was taken above)
        app.on_event(Ev::EndResult { batch, target: id, name: "mpv".into(), result: Ok(()) });
        assert!(api.get_toast().contains("SIGTERM envoyé") && !api.get_toast_error());
        app.on_event(Ev::EndOutcome {
            batch,
            target: id,
            name: "mpv".into(),
            outcome: EndOutcome::StillRunning,
            waited: Duration::from_secs(5),
        });
        assert!(api.get_toast().contains("toujours actif") && api.get_toast_error(), "{}", api.get_toast());
        render(&window, 1380, 880).save("15-still-running");
        advance(9000);
    }

    // --- Protected process: no dialog, explanation instead ------------------------
    api.invoke_row_clicked(row_index(&ui, "Hyprland"));
    assert!(api.get_detail().protected);
    api.invoke_request_end(true);
    assert!(!api.get_confirm().open, "protected processes never get a confirmation dialog");
    assert!(api.get_toast_error());
    render(&window, 1380, 880).save("04-protected");
    advance(5000);
    assert!(!api.get_toast().is_empty(), "warnings stay long enough to be read");
    advance(5000);
    assert_eq!(api.get_toast(), "", "toast disappears");

    // --- Search, filter, sort ----------------------------------------------------
    api.invoke_search_changed("blend".into());
    assert_eq!(api.get_rows().row_count(), 1);
    api.invoke_search_changed("".into());
    api.invoke_filter_changed(3);
    assert!(api.get_rows().iter().all(|r| r.kind != "APP"));
    api.invoke_filter_changed(0);
    api.invoke_mode_changed(1);
    api.invoke_sort_clicked(0);
    let names: Vec<String> = api.get_rows().iter().map(|r| r.name.to_lowercase()).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted, "name sort ascending");
    render(&window, 1380, 880).save("05-processes-flat");

    // --- Settings ----------------------------------------------------------------
    ui.set_page(2);
    api.invoke_set_accent(1);
    assert_eq!(ui.global::<Theme>().get_accent_index(), 1);
    api.invoke_set_refresh(2);
    assert_eq!(app.presenter.borrow().settings().refresh_ms, 2000);
    assert_eq!(api.get_refresh_index(), 2);
    render(&window, 1380, 880).save("06-settings");
    api.invoke_set_accent(0);
    api.invoke_set_chart_style(1);
    ui.set_page(0);
    render(&window, 1380, 880).save("07-dashboard-dots");
    api.invoke_set_chart_style(0);

    // --- Short window: the fixed grid must not overflow -------------------------
    render(&window, 1270, 770).save("10-dashboard-short");

    // --- Narrow tiled window (Hyprland split) -----------------------------------
    render(&window, 640, 900).save("08-dashboard-narrow");
    ui.set_page(1);
    api.invoke_row_clicked(row_index(&ui, "firefox"));
    render(&window, 640, 900).save("09-processes-narrow");

    // --- Real-backend conditions: unavailable metrics, actions disabled ---------
    // Same bridge as with the Linux backend: values that cannot be measured
    // must read "Indisponible"/"VEILLE", never a number.
    let mut snap = src.sample();
    snap.cpu.temperature_c = None;
    snap.cpu.frequency_mhz = None;
    snap.memory.cached = None;
    let g = &mut snap.gpus[0];
    g.state = dotmon_core::model::GpuState::Asleep;
    (g.usage, g.vram_used, g.vram_total, g.temperature_c, g.power_w) = (None, None, None, None, None);
    g.soc_power_w = None;
    app.presenter.borrow_mut().set_actions_enabled(false);
    app.on_event(Event::Snapshot(Arc::new(snap)));
    let sys = ui.global::<crate::Sys>();
    assert_eq!(sys.get_gpu(), "VEILLE");
    assert_eq!(sys.get_gpu_unit(), "");
    assert_eq!(sys.get_gpu_temp(), "EN VEILLE");
    assert_eq!(sys.get_vram(), "Indisponible");
    assert_eq!(sys.get_gpu_power(), "Indisponible");
    assert_eq!(sys.get_cpu_temp(), "Indisponible");
    assert_eq!(sys.get_cpu_freq(), "Indisponible");
    assert_eq!(sys.get_mem_cached(), "Indisponible");
    ui.set_page(0);
    render(&window, 1380, 880).save("11-unavailable");

    ui.set_page(1);
    api.invoke_mode_changed(0);
    api.invoke_row_clicked(row_index(&ui, "blender"));
    assert!(!api.get_actions_enabled());
    api.invoke_request_end(true);
    assert!(!api.get_confirm().open, "no confirmation when actions are disabled");
    assert!(app.presenter.borrow().pending().is_none());
    render(&window, 1380, 880).save("12-actions-disabled");

    // --- Phase 2.1: no CPU figure yet for a new process; SoC power labelled ------
    let mut snap = src.sample();
    for p in snap.processes.iter_mut().filter(|p| p.name == "blender") {
        p.cpu = None;
    }
    let g = &mut snap.gpus[0];
    (g.power_w, g.soc_power_w) = (None, Some(54.5));
    app.on_event(Event::Snapshot(Arc::new(snap)));
    assert_eq!(ui.global::<crate::Sys>().get_gpu_power(), "54.5 W · SOC");
    let blender = api.get_rows().iter().find(|r| r.name == "blender").expect("blender row");
    assert_eq!(blender.cpu, "—", "not 0.0");
    assert_eq!(api.get_detail().cpu, "—");
    render(&window, 1380, 880).save("13-new-process-no-cpu");
    ui.set_page(0);
    render(&window, 1380, 880).save("14-soc-power");

    app.shutdown();
}
