//! Dotmon — native system monitor and task manager for Linux / Wayland.
//!
//! Threading model:
//! - the *engine* thread samples the system, executes confirmed actions and
//!   watches their effect;
//! - the *UI* thread owns the Slint window and the [`presenter::Presenter`].
//!
//! Engine events reach the UI thread through `slint::invoke_from_event_loop`.

mod app;
mod bridge;
mod devshot;
mod engine;
mod outcomes;
mod presenter;
#[cfg(test)]
mod ui_tests;

use std::time::Duration;

use dotmon_core::protection::ProtectionPolicy;
use dotmon_core::settings::Settings;
use dotmon_core::source::{MetricsSource, ProcessController};

use crate::app::App;
use crate::engine::Engine;

slint::include_modules!();

const APP_ID: &str = "dotmon";
/// Seed of the simulated machine (prototype only).
pub const MOCK_SEED: u64 = 0x05EE_DD07;

fn current_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    // /proc/self is owned by the effective UID of the running process.
    std::fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(u32::MAX)
}

fn print_help() {
    println!(
        "dotmon {}\n\nUsage : dotmon [--mock] [--help] [--version]\n\n\
         Par défaut : métriques réelles ; fermeture de processus avec confirmation (SIGTERM/SIGKILL).\n\
         --mock     machine simulée déterministe (démonstration, tests)\n\n\
         Raccourcis : Ctrl+1/2/3 pages · Ctrl+F recherche · Échap annuler/fermer",
        env!("CARGO_PKG_VERSION")
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut use_mock = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--mock" => use_mock = true,
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            "-V" | "--version" => {
                println!("dotmon {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            other => {
                eprintln!("dotmon: argument inconnu « {other} » (voir --help)");
                std::process::exit(2);
            }
        }
    }

    let config_path = dirs::config_dir().map(|d| d.join("dotmon").join("settings.toml"));
    let settings = config_path.as_deref().map(Settings::load).unwrap_or_default();

    let self_pid = std::process::id();
    let uid = current_uid();
    let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
    let policy = ProtectionPolicy::new(self_pid, uid).with_extra_names(settings.protected_names.clone());

    // Software rendering by default: measured at the same CPU cost as
    // OpenGL for this UI (~3 % of one core at 1 Hz) but ~4× less memory,
    // since no GPU driver is loaded. `SLINT_BACKEND=winit-femtovg` restores
    // OpenGL rendering.
    if std::env::var_os("SLINT_BACKEND").is_none() {
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name("software".into())
            .select()?;
    }
    let ui = MainWindow::new()?;
    // Needs an initialised platform (done by `MainWindow::new`) and must
    // happen before the window is shown. Lets Hyprland rules target
    // `class:^(dotmon)$`.
    slint::set_xdg_app_id(APP_ID)?;

    // Backend. The real one never falls back to the simulation silently:
    // if it cannot start, Dotmon says so and exits.
    let (source, controller, backend, actions): (Box<dyn MetricsSource>, Box<dyn ProcessController>, _, _) =
        if use_mock {
            let (s, c) = dotmon_sys::mock::mock_backend(MOCK_SEED, self_pid, uid, &user);
            (Box::new(s), Box::new(c), "SIMULATION", true)
        } else {
            match dotmon_sys::linux::LinuxSource::new() {
                Ok(s) => (Box::new(s), Box::new(dotmon_sys::linux::LinuxController::new()), "LINUX", true),
                Err(e) => {
                    eprintln!("dotmon: impossible de lire les métriques système : {e}");
                    eprintln!("dotmon: utilisez --mock pour la machine simulée.");
                    std::process::exit(1);
                }
            }
        };
    let interval = Duration::from_millis(settings.refresh_ms as u64);
    let app = App::install(&ui, settings, policy.clone(), backend, config_path);
    app.presenter.borrow_mut().set_actions_enabled(actions);

    let engine = Engine::spawn(source, controller, policy, interval, |event| {
        // Runs on the engine thread; hop to the UI thread.
        let _ = slint::invoke_from_event_loop(move || app::with_app(|app| app.on_event(event)));
    });
    *app.engine.borrow_mut() = Some(engine);

    let a = app.clone();
    devshot::install(&ui, move |name| a.presenter.borrow().rows().iter().position(|r| r.name == name));

    ui.run()?;

    // Stop the engine before tearing down the UI state.
    app.shutdown();
    Ok(())
}
