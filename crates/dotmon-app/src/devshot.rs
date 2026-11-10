//! Developer screenshots (used to verify the UI renders as intended).
//!
//! Activated only by environment variables, never in normal use:
//!
//! - `DOTMON_SNAPSHOT=/path/out.ppm` — render the window to a PPM file and quit;
//! - `DOTMON_PAGE=0|1|2` — page to show first;
//! - `DOTMON_SELECT=name` — select the first row with that name (page 1);
//! - `DOTMON_MODE=1` — process view instead of applications;
//! - `DOTMON_CONFIRM=force|close` — open the confirmation dialog;
//! - `DOTMON_DELAY_MS` — time to let data accumulate (default 4000).

use std::io::Write;
use std::time::Duration;

use slint::ComponentHandle;

use crate::{Api, MainWindow};

pub fn install(ui: &MainWindow, find_row: impl Fn(&str) -> Option<usize> + 'static) {
    let Ok(path) = std::env::var("DOTMON_SNAPSHOT") else { return };
    let delay = std::env::var("DOTMON_DELAY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(4000);
    let page: i32 = std::env::var("DOTMON_PAGE").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    ui.set_page(page);

    let weak = ui.as_weak();
    slint::Timer::single_shot(Duration::from_millis(delay), move || {
        let Some(ui) = weak.upgrade() else { return };
        let api = ui.global::<Api>();
        if std::env::var("DOTMON_MODE").as_deref() == Ok("1") {
            api.invoke_mode_changed(1);
        }
        if let Ok(name) = std::env::var("DOTMON_SELECT")
            && let Some(i) = find_row(&name)
        {
            api.invoke_row_clicked(i as i32);
        }
        match std::env::var("DOTMON_CONFIRM").as_deref() {
            Ok("force") => api.invoke_request_end(true),
            Ok("close") => api.invoke_request_end(false),
            _ => {}
        }
        let weak = ui.as_weak();
        // Let transitions finish before capturing.
        slint::Timer::single_shot(Duration::from_millis(900), move || {
            let Some(ui) = weak.upgrade() else { return };
            match ui.window().take_snapshot() {
                Ok(buf) => {
                    if let Err(e) = write_ppm(&path, buf.width(), buf.height(), buf.as_bytes()) {
                        eprintln!("devshot: {e}");
                    } else {
                        eprintln!("devshot: {path} ({}×{})", buf.width(), buf.height());
                    }
                }
                Err(e) => eprintln!("devshot: capture impossible: {e}"),
            }
            let _ = slint::quit_event_loop();
        });
    });
}

fn write_ppm(path: &str, w: u32, h: u32, rgba: &[u8]) -> std::io::Result<()> {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    write!(f, "P6\n{w} {h}\n255\n")?;
    for px in rgba.as_chunks::<4>().0 {
        f.write_all(&px[..3])?;
    }
    f.flush()
}
