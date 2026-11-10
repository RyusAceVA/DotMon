//! Converts presenter state into the Slint globals (`Sys`, `Api`, `Theme`).
//!
//! All functions here run on the UI thread. Models are updated in place when
//! their length is unchanged, so scroll positions and animations survive
//! refreshes.

use std::rc::Rc;

use dotmon_core::chart;
use dotmon_core::format;
use dotmon_core::model::{GpuState, Snapshot};
use dotmon_core::query::{Filter, Row, RowKey, SortDir, SortKey};
use dotmon_core::settings::{ChartStyle, HISTORY_CHOICES, REFRESH_CHOICES_MS, Settings};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::presenter::{Detail as PDetail, PendingEnd, Presenter, ViewMode};
use crate::{Api, Confirm, Detail, DiskRow, MainWindow, ProcRow, Sys, Theme};

/// Number of dot columns in the "dots" chart style.
const DOT_COLUMNS: usize = 48;

/// Slint-side models kept alive across updates.
pub struct Models {
    rows: Rc<VecModel<ProcRow>>,
    cores: Rc<VecModel<f32>>,
    cpu_cols: Rc<VecModel<f32>>,
    mem_cols: Rc<VecModel<f32>>,
    gpu_cols: Rc<VecModel<f32>>,
    disks: Rc<VecModel<DiskRow>>,
}

impl Models {
    pub fn install(ui: &MainWindow) -> Self {
        let m = Self {
            rows: Rc::new(VecModel::default()),
            cores: Rc::new(VecModel::default()),
            cpu_cols: Rc::new(VecModel::default()),
            mem_cols: Rc::new(VecModel::default()),
            gpu_cols: Rc::new(VecModel::default()),
            disks: Rc::new(VecModel::default()),
        };
        let sys = ui.global::<Sys>();
        sys.set_cores(ModelRc::from(m.cores.clone()));
        sys.set_cpu_cols(ModelRc::from(m.cpu_cols.clone()));
        sys.set_mem_cols(ModelRc::from(m.mem_cols.clone()));
        sys.set_gpu_cols(ModelRc::from(m.gpu_cols.clone()));
        sys.set_disks(ModelRc::from(m.disks.clone()));
        ui.global::<Api>().set_rows(ModelRc::from(m.rows.clone()));
        m
    }
}

/// Replace the contents of `model` with `items`, in place when possible.
fn sync<T: Clone + PartialEq + 'static>(model: &VecModel<T>, items: Vec<T>) {
    if model.row_count() == items.len() {
        for (i, item) in items.into_iter().enumerate() {
            if model.row_data(i).as_ref() != Some(&item) {
                model.set_row_data(i, item);
            }
        }
    } else {
        model.set_vec(items);
    }
}

fn s(v: impl Into<SharedString>) -> SharedString {
    v.into()
}

/// Shown wherever a real measurement is not available.
const NA: &str = "Indisponible";
/// Narrow-column placeholder (process table, detail panel).
const NO_VALUE: &str = "—";

fn opt_temp(t: Option<f32>) -> SharedString {
    t.map(|t| format!("{t:.0} °C")).unwrap_or_else(|| NA.into()).into()
}

/// CPU share of a process or group: "—" while there is no measurement yet
/// (a process younger than one sample), never a made-up 0.
fn opt_cpu(v: Option<f32>) -> SharedString {
    v.map(format::percent).unwrap_or_else(|| NO_VALUE.into()).into()
}

/// GPU power with its scope spelled out: the figure of an APU covers the
/// CPU cores too, so it is labelled "SOC" and never passed off as the GPU's.
fn gpu_power_text(g: &dotmon_core::model::GpuStats) -> String {
    match (g.power_w, g.soc_power_w) {
        (Some(w), _) => format!("{w:.1} W"),
        (None, Some(w)) => format!("{w:.1} W · SOC"),
        (None, None) => NA.into(),
    }
}

fn opt_rate(v: Option<u64>) -> SharedString {
    v.map(format::rate).unwrap_or_else(|| NA.into()).into()
}

pub fn row_id(key: RowKey) -> String {
    match key {
        RowKey::Process(id) => format!("p{}:{}", id.pid, id.start_time),
        RowKey::App(id) => format!("a{}:{}", id.pid, id.start_time),
    }
}

/// Dashboard: everything in the `Sys` global.
pub fn push_system(ui: &MainWindow, models: &Models, p: &Presenter, backend: &str) {
    let Some(snap) = p.snapshot() else { return };
    let h = p.history();
    let cap = h.cpu.capacity();
    let sys = ui.global::<Sys>();

    sys.set_tick(snap.seq as i32);
    sys.set_hostname(s(snap.hostname.to_uppercase()));
    sys.set_kernel(s(snap.kernel.clone()));
    sys.set_uptime(s(format::uptime(snap.uptime_s)));
    sys.set_backend(s(backend));
    sys.set_interval(s(format!("{:.1} s", p.settings().refresh_ms as f32 / 1000.0)));
    sys.set_view_w(chart::view_width(cap));
    sys.set_proc_count(s(snap.processes.len().to_string()));
    sys.set_app_count(s(p.app_count().to_string()));
    sys.set_thread_count(s(snap.processes.iter().map(|p| p.threads as u64).sum::<u64>().to_string()));

    // CPU
    let cpu_vals: Vec<f32> = h.cpu.values().collect();
    sys.set_cpu(s(format::percent(snap.cpu.usage)));
    sys.set_cpu_frac(snap.cpu.usage / 100.0);
    sys.set_cpu_model(s(snap.cpu.model.to_uppercase()));
    sys.set_cpu_freq(s(snap
        .cpu
        .frequency_mhz
        .map(|f| format!("{:.2} GHz", f as f32 / 1000.0))
        .unwrap_or_else(|| NA.into())));
    sys.set_cpu_temp(opt_temp(snap.cpu.temperature_c));
    sys.set_cpu_load(s(snap
        .cpu
        .load_avg
        .map(|l| format!("{:.1} {:.1} {:.1}", l[0], l[1], l[2]))
        .unwrap_or_else(|| NA.into())));
    sys.set_cpu_line(s(chart::line(&cpu_vals, cap, 100.0)));
    sys.set_cpu_area(s(chart::area(&cpu_vals, cap, 100.0)));
    sync(&models.cpu_cols, chart::columns(&cpu_vals, DOT_COLUMNS, 100.0));
    sync(&models.cores, snap.cpu.per_core.iter().map(|c| c / 100.0).collect());

    // Memory
    let m = &snap.memory;
    let mem_vals: Vec<f32> = h.memory.values().collect();
    sys.set_mem(s(format::bytes_value(m.used)));
    sys.set_mem_unit(s(format::bytes_unit(m.used)));
    sys.set_mem_total(s(format::bytes(m.total)));
    sys.set_mem_avail(s(format::bytes(m.available)));
    sys.set_mem_cached(s(m.cached.map(format::bytes).unwrap_or_else(|| NA.into())));
    sys.set_mem_frac(m.used_percent() / 100.0);
    sys.set_swap(s(format!("{} / {}", format::bytes(m.swap_used), format::bytes(m.swap_total))));
    sys.set_swap_frac(m.swap_percent() / 100.0);
    sys.set_mem_line(s(chart::line(&mem_vals, cap, 100.0)));
    sys.set_mem_area(s(chart::area(&mem_vals, cap, 100.0)));
    sync(&models.mem_cols, chart::columns(&mem_vals, DOT_COLUMNS, 100.0));

    // GPU (first device)
    match snap.gpus.first() {
        Some(g) => {
            let gpu_vals: Vec<f32> = h.gpu.values().collect();
            sys.set_gpu_present(true);
            sys.set_gpu_name(s(g.name.to_uppercase()));
            let usage = g.usage.unwrap_or(0.0);
            // Asleep / resting GPUs are deliberately not queried (polling
            // would keep them powered): no figure rather than a fake one.
            let (value, unit) = match (g.state, g.usage) {
                (GpuState::Asleep, _) => ("VEILLE".to_string(), ""),
                (GpuState::Resting, _) => ("REPOS".to_string(), ""),
                (GpuState::Active, Some(u)) => (format::percent(u), "%"),
                (GpuState::Active, None) => ("N/D".to_string(), ""),
            };
            sys.set_gpu(s(value));
            sys.set_gpu_unit(s(unit));
            sys.set_gpu_frac(usage / 100.0);
            match (g.vram_used, g.vram_total) {
                (Some(u), Some(t)) => {
                    sys.set_vram(s(format!("{} / {}", format::bytes(u), format::bytes(t))));
                    sys.set_vram_frac(dotmon_core::model::percent(u, t) / 100.0);
                }
                _ => {
                    sys.set_vram(s(NA));
                    sys.set_vram_frac(0.0);
                }
            }
            sys.set_gpu_temp(match g.state {
                GpuState::Asleep => s("EN VEILLE"),
                GpuState::Resting => s("AU REPOS"),
                GpuState::Active => opt_temp(g.temperature_c),
            });
            sys.set_gpu_power(s(gpu_power_text(g)));
            sys.set_gpu_line(s(chart::line(&gpu_vals, cap, 100.0)));
            sys.set_gpu_area(s(chart::area(&gpu_vals, cap, 100.0)));
            sync(&models.gpu_cols, chart::columns(&gpu_vals, DOT_COLUMNS, 100.0));
        }
        None => sys.set_gpu_present(false),
    }

    // Network — both directions share one scale.
    let rx: Vec<f32> = h.net_rx.values().collect();
    let tx: Vec<f32> = h.net_tx.values().collect();
    let net_max = chart::nice_max(h.net_rx.max().max(h.net_tx.max()), 64.0 * 1024.0);
    sys.set_net_iface(s(snap
        .networks
        .iter()
        .map(|n| n.interface.to_uppercase())
        .collect::<Vec<_>>()
        .join(" · ")));
    sys.set_net_rx(s(format::rate(snap.net_rx_bps())));
    sys.set_net_tx(s(format::rate(snap.net_tx_bps())));
    sys.set_net_rx_total(s(format::bytes(snap.networks.iter().map(|n| n.rx_total).sum())));
    sys.set_net_tx_total(s(format::bytes(snap.networks.iter().map(|n| n.tx_total).sum())));
    sys.set_net_rx_line(s(chart::line(&rx, cap, net_max)));
    sys.set_net_tx_line(s(chart::line(&tx, cap, net_max)));
    sys.set_net_max(s(format::rate(net_max as u64)));

    // Disks
    let rd: Vec<f32> = h.disk_read.values().collect();
    let wr: Vec<f32> = h.disk_write.values().collect();
    let disk_max = chart::nice_max(h.disk_read.max().max(h.disk_write.max()), 1024.0 * 1024.0);
    sys.set_disk_read(s(format::rate(snap.disk_read_bps())));
    sys.set_disk_write(s(format::rate(snap.disk_write_bps())));
    sys.set_disk_read_line(s(chart::line(&rd, cap, disk_max)));
    sys.set_disk_write_line(s(chart::line(&wr, cap, disk_max)));
    sys.set_disk_max(s(format::rate(disk_max as u64)));
    sync(&models.disks, disk_rows(snap));
}

fn disk_rows(snap: &Snapshot) -> Vec<DiskRow> {
    snap.disks
        .iter()
        .map(|d| DiskRow {
            mount: s(d.mount_point.clone()),
            detail: s(format!("{} · {}", d.name, d.filesystem)),
            used: s(format!("{} / {}", format::bytes(d.used), format::bytes(d.total))),
            frac: d.used_percent() / 100.0,
            read: opt_rate(d.read_bps),
            write: opt_rate(d.write_bps),
        })
        .collect()
}

fn proc_row(r: &Row) -> ProcRow {
    ProcRow {
        id: s(row_id(r.key)),
        name: s(r.name.clone()),
        pid: s(r.pid.to_string()),
        user: s(r.user.clone()),
        kind: s(r.kind.label()),
        cpu: opt_cpu(r.cpu),
        // Bar scale: a process using one full core out of 16 is ~6 %;
        // scale so 25 % of the machine fills the meter.
        cpu_frac: (r.cpu.unwrap_or(0.0) / 25.0).clamp(0.0, 1.0),
        mem: s(format::bytes(r.memory)),
        threads: s(r.threads.to_string()),
        // Narrow column: "—" when the I/O of the process is not readable.
        io: r.io_bps.map(|v| s(format::rate(v))).unwrap_or_else(|| s(NO_VALUE)),
        count: r.count as i32,
        protected: r.is_protected(),
    }
}

/// Process list, query state and detail panel.
pub fn push_list(ui: &MainWindow, models: &Models, p: &Presenter) {
    let api = ui.global::<Api>();
    sync(&models.rows, p.rows().iter().map(proc_row).collect());
    let q = p.query();
    api.set_mode(match p.mode() {
        ViewMode::Apps => 0,
        ViewMode::Processes => 1,
    });
    api.set_filter(match q.filter {
        Filter::All => 0,
        Filter::Apps => 1,
        Filter::Services => 2,
        Filter::System => 3,
    });
    api.set_sort_key(match q.sort {
        SortKey::Name => 0,
        SortKey::Pid => 1,
        SortKey::Cpu => 2,
        SortKey::Memory => 3,
        SortKey::Threads => 4,
        SortKey::Io => 5,
    });
    api.set_sort_desc(q.dir == SortDir::Desc);
    api.set_actions_enabled(p.actions_enabled());
    let n = p.rows().len();
    api.set_result_count(s(format!("{n} {}", if n > 1 { "RÉSULTATS" } else { "RÉSULTAT" })));
    api.set_selected_id(s(p.selected().map(row_id).unwrap_or_default()));
    push_detail(ui, p);
}

fn push_detail(ui: &MainWindow, p: &Presenter) {
    let api = ui.global::<Api>();
    let Some(d) = p.detail() else {
        api.set_detail(Detail::default());
        return;
    };
    let uptime = p.snapshot().map(|s| s.uptime_s).unwrap_or(0);
    api.set_detail(detail(&d, uptime, p.history().cpu.capacity()));
}

fn detail(d: &PDetail, uptime_s: u64, cap: usize) -> Detail {
    let cpu_max = chart::nice_max(d.cpu_series.iter().copied().fold(0.0, f32::max), 5.0);
    let mem_max = chart::nice_max(d.mem_series.iter().copied().fold(0.0, f32::max), 16.0 * 1024.0 * 1024.0);
    let (reason, protected) = match &d.protection {
        dotmon_core::protection::Protection::Allowed => (String::new(), false),
        dotmon_core::protection::Protection::Protected(r) => (r.message(), true),
    };
    Detail {
        present: true,
        name: s(d.name.clone()),
        kind: s(d.kind.label()),
        pid: s(d.pid.to_string()),
        user: s(d.user.clone()),
        state: s(d.state.label()),
        cmdline: s(d.cmdline.clone()),
        cpu: opt_cpu(d.cpu),
        mem: s(format::bytes_value(d.memory)),
        mem_unit: s(format::bytes_unit(d.memory)),
        threads: s(d.threads.to_string()),
        io: opt_rate(d.io_bps),
        count: s(d.count.to_string()),
        started: s(format!("il y a {}", format::uptime(uptime_s.saturating_sub(d.started_s)))),
        protected,
        reason: s(reason),
        cpu_line: s(chart::line(&d.cpu_series, cap, cpu_max)),
        cpu_area: s(chart::area(&d.cpu_series, cap, cpu_max)),
        cpu_max: s(format!("{} %", format::percent(cpu_max))),
        mem_line: s(chart::line(&d.mem_series, cap, mem_max)),
        mem_area: s(chart::area(&d.mem_series, cap, mem_max)),
        mem_max: s(format::bytes(mem_max as u64)),
    }
}

pub fn push_confirm(ui: &MainWindow, pending: Option<&PendingEnd>) {
    let api = ui.global::<Api>();
    let Some(pe) = pending else {
        api.set_confirm(Confirm::default());
        return;
    };
    let force = pe.mode == dotmon_core::source::EndMode::Force;
    api.set_confirm(Confirm {
        open: true,
        force,
        // The title keeps the process name only (a 6-digit PID would be
        // truncated); the PID is stated in the body.
        name: s(pe.name.clone()),
        body: s(confirm_body(pe)),
        count: pe.targets.len() as i32,
    });
}

/// Text of the confirmation: the exact signal, who receives it, and what to
/// expect. The wording follows the action: closing is polite, forcing warns.
pub fn confirm_body(pe: &PendingEnd) -> String {
    let sig = pe.mode.signal_name();
    let n = pe.targets.len();
    let mut body = match (pe.mode, pe.is_group, n) {
        (dotmon_core::source::EndMode::Graceful, false, _) => format!(
            "Envoyer {sig} à « {} » (PID {}). Le processus peut enregistrer son travail avant de quitter. \
             S'il ne se ferme pas, rien ne sera forcé : vous pourrez choisir FORCER.",
            pe.name, pe.pid
        ),
        (dotmon_core::source::EndMode::Graceful, true, _) => format!(
            "Envoyer {sig} au processus principal de « {} » (PID {}). L'application ferme elle-même ses \
             processus secondaires. S'il ne se ferme pas, rien ne sera forcé.",
            pe.name, pe.pid
        ),
        (dotmon_core::source::EndMode::Force, false, _) | (dotmon_core::source::EndMode::Force, true, 1) => {
            format!(
                "Envoyer {sig} à « {} » (PID {}) : arrêt immédiat. Les données non enregistrées seront perdues.",
                pe.name, pe.pid
            )
        }
        (dotmon_core::source::EndMode::Force, true, n) => format!(
            "Envoyer {sig} à {n} processus de « {} » (processus principal : PID {}) : arrêt immédiat. \
             Les données non enregistrées seront perdues.",
            pe.name, pe.pid
        ),
    };
    if pe.excluded_protected > 0 {
        body.push_str(&format!(" {} processus protégé(s) ne seront pas touchés.", pe.excluded_protected));
    }
    body
}

pub fn push_settings(ui: &MainWindow, settings: &Settings, config_path: &str) {
    let theme = ui.global::<Theme>();
    theme.set_accent_index(settings.accent.index() as i32);
    theme.set_animations(settings.animations);
    theme.set_dot_grid(settings.dot_grid);
    let api = ui.global::<Api>();
    api.set_refresh_index(
        REFRESH_CHOICES_MS.iter().position(|v| *v == settings.refresh_ms).map(|i| i as i32).unwrap_or(-1),
    );
    api.set_history_index(
        HISTORY_CHOICES.iter().position(|v| *v == settings.history_len).map(|i| i as i32).unwrap_or(-1),
    );
    api.set_chart_style(match settings.chart_style {
        ChartStyle::Line => 0,
        ChartStyle::Dots => 1,
    });
    api.set_show_kernel(settings.show_kernel_threads);
    api.set_config_path(s(config_path));
}

pub fn toast(ui: &MainWindow, text: &str, error: bool) {
    let api = ui.global::<Api>();
    api.set_toast(s(text));
    api.set_toast_error(error);
    api.set_toast_seq(api.get_toast_seq().wrapping_add(1));
}

#[cfg(test)]
mod tests {
    use super::*;
    use dotmon_core::model::ProcessId;

    #[test]
    fn row_ids_distinguish_apps_processes_and_reuse() {
        let a = ProcessId { pid: 5, start_time: 9 };
        let b = ProcessId { pid: 5, start_time: 10 };
        assert_ne!(row_id(RowKey::App(a)), row_id(RowKey::Process(a)));
        assert_ne!(row_id(RowKey::Process(a)), row_id(RowKey::Process(b)));
    }

    #[test]
    fn sync_updates_in_place_and_resizes() {
        let m = VecModel::from(vec![1, 2, 3]);
        sync(&m, vec![1, 5, 3]);
        assert_eq!(m.iter().collect::<Vec<_>>(), vec![1, 5, 3]);
        sync(&m, vec![7]);
        assert_eq!(m.iter().collect::<Vec<_>>(), vec![7]);
    }

    #[test]
    fn unmeasured_cpu_is_a_dash_not_zero() {
        assert_eq!(opt_cpu(None).as_str(), "—");
        assert_eq!(opt_cpu(Some(0.0)).as_str(), "0.0", "a measured zero stays 0.0");
        assert_eq!(opt_cpu(Some(42.4)).as_str(), "42");
    }

    #[test]
    fn soc_power_is_labelled_and_never_shown_as_gpu_power() {
        use dotmon_core::model::GpuStats;
        let mut g = GpuStats::default();
        assert_eq!(gpu_power_text(&g), NA);
        g.soc_power_w = Some(54.5);
        let text = gpu_power_text(&g);
        assert!(text.contains("SOC") && text.starts_with("54.5 W"), "{text}");
        g.power_w = Some(9.0);
        assert_eq!(gpu_power_text(&g), "9.0 W", "a genuine GPU figure wins and carries no label");
    }
}
