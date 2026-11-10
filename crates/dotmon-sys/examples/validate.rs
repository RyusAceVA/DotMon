//! Compares the Linux backend with CachyOS tools over the *same* time
//! windows and prints a verdict per measurement (OK / ÉCART), then a summary
//! that separates repeated discrepancies (CONFIRMÉ) from one-off noise.
//!
//! References: `top` (CPU global et par processus), `free`, `ps`, `df`,
//! `sensors`, `/proc/loadavg`, `/proc/cpuinfo`, and the kernel's sysfs
//! counters (`/sys/block/*/stat`, `/sys/class/net/*/statistics`) — a
//! different interface from the `/proc/diskstats` and `/proc/net/dev` that
//! Dotmon parses.
//!
//! The NVIDIA GPU is never woken: its state is read from sysfs only, and
//! `nvidia-smi` runs only with `--nvidia-smi` *and* when the card is already
//! powered (`runtime_status = active`).
//!
//! ```fish
//! cargo run --release -p dotmon-sys --example validate -- 5
//! cargo run --release -p dotmon-sys --example validate -- 5 --nvidia-smi
//! ```

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use dotmon_core::format;
use dotmon_core::model::GpuState;
use dotmon_core::source::MetricsSource;
use dotmon_sys::linux::LinuxSource;
use dotmon_sys::linux::gpu::Vendor;

const MIB: u64 = 1024 * 1024;

#[derive(Clone, Copy, PartialEq)]
enum Verdict {
    Ok,
    Ecart,
    Info,
}

struct Check {
    key: String,
    label: String,
    tool: &'static str,
    reference: String,
    dotmon: String,
    diff: String,
    tol: String,
    verdict: Verdict,
}

#[derive(Default)]
struct Report {
    checks: Vec<Check>,
}

impl Report {
    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        key: impl Into<String>,
        label: impl Into<String>,
        tool: &'static str,
        reference: String,
        dotmon: String,
        diff: String,
        tol: String,
        ok: Option<bool>,
    ) {
        let verdict = match ok {
            Some(true) => Verdict::Ok,
            Some(false) => Verdict::Ecart,
            None => Verdict::Info,
        };
        self.checks.push(Check {
            key: key.into(),
            label: label.into(),
            tool,
            reference,
            dotmon,
            diff,
            tol,
            verdict,
        });
    }

    fn bytes(&mut self, key: &str, label: &str, tool: &'static str, r: u64, d: u64, tol: u64) {
        let diff = d as i128 - r as i128;
        self.push(
            key,
            label,
            tool,
            format::bytes(r),
            format::bytes(d),
            signed_bytes(diff),
            format!("± {}", format::bytes(tol)),
            Some(diff.unsigned_abs() <= tol as u128),
        );
    }

    /// Rates: tolerance = max(relative × reference, absolute floor).
    #[allow(clippy::too_many_arguments)]
    fn rate(&mut self, key: &str, label: &str, tool: &'static str, r: u64, d: u64, rel: f64, floor: u64) {
        let tol = ((r as f64 * rel) as u64).max(floor);
        let diff = d as i128 - r as i128;
        self.push(
            key,
            label,
            tool,
            format::rate(r),
            format::rate(d),
            format!("{}/s", signed_bytes(diff)),
            format!("± {}", format::rate(tol)),
            Some(diff.unsigned_abs() <= tol as u128),
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn num(&mut self, key: &str, label: &str, tool: &'static str, r: f64, d: f64, tol: f64, unit: &str) {
        let diff = d - r;
        self.push(
            key,
            label,
            tool,
            format!("{r:.1}{unit}"),
            format!("{d:.1}{unit}"),
            format!("{diff:+.1}{unit}"),
            format!("± {tol:.1}{unit}"),
            Some(diff.abs() <= tol),
        );
    }

    fn unavailable(&mut self, key: &str, label: &str, tool: &'static str, why: &str) {
        self.push(key, label, tool, why.into(), "—".into(), "".into(), "".into(), None);
    }

    fn print(&self) {
        println!(
            "{:<34} {:<16} {:>16} {:>16} {:>14} {:>14}  VERDICT",
            "MESURE", "RÉFÉRENCE", "OUTIL", "DOTMON", "ÉCART", "TOLÉRANCE"
        );
        for c in &self.checks {
            let v = match c.verdict {
                Verdict::Ok => "OK",
                Verdict::Ecart => "ÉCART  <<<",
                Verdict::Info => "info",
            };
            println!(
                "{:<34} {:<16} {:>16} {:>16} {:>14} {:>14}  {v}",
                trunc(&c.label, 34),
                c.tool,
                trunc(&c.reference, 16),
                trunc(&c.dotmon, 16),
                c.diff,
                c.tol
            );
        }
    }
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { s.chars().take(n - 1).collect::<String>() + "…" }
}

fn signed_bytes(v: i128) -> String {
    let sign = if v < 0 { "-" } else { "+" };
    format!("{sign}{}", format::bytes(v.unsigned_abs() as u64))
}

/// Runs a tool in the C locale (decimal points, English labels).
fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).env("LC_ALL", "C").output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn read(p: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

fn read_u64(p: impl AsRef<Path>) -> Option<u64> {
    read(p)?.parse().ok()
}

// ---------------------------------------------------------------- references

/// Whole physical disks (not virtual: no zram, loop, dm).
fn physical_disks() -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir("/sys/block")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| fs::read_link(e.path()).is_ok_and(|l| !l.to_string_lossy().contains("/virtual/")))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// (read, written) bytes of a block device or partition, from its sysfs
/// `stat` file (sectors are always 512 bytes there).
fn block_bytes(dev: &str) -> Option<(u64, u64)> {
    let s = read(format!("/sys/class/block/{dev}/stat"))?;
    let f: Vec<u64> = s.split_whitespace().filter_map(|x| x.parse().ok()).collect();
    Some((f.get(2)? * 512, f.get(6)? * 512))
}

/// Physical, up interfaces — the same rule Dotmon documents, rederived.
fn physical_ifaces() -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir("/sys/class/net")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.path().join("device").exists() && read(e.path().join("operstate")).as_deref() == Some("up")
        })
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn net_bytes(iface: &str) -> Option<(u64, u64)> {
    let d = format!("/sys/class/net/{iface}/statistics");
    Some((read_u64(format!("{d}/rx_bytes"))?, read_u64(format!("{d}/tx_bytes"))?))
}

struct Counters {
    at: Instant,
    blocks: HashMap<String, (u64, u64)>,
    nets: HashMap<String, (u64, u64)>,
}

fn counters(blocks: &[String], ifaces: &[String]) -> Counters {
    Counters {
        at: Instant::now(),
        blocks: blocks.iter().filter_map(|b| Some((b.clone(), block_bytes(b)?))).collect(),
        nets: ifaces.iter().filter_map(|i| Some((i.clone(), net_bytes(i)?))).collect(),
    }
}

fn rate(a: u64, b: u64, secs: f64) -> Option<u64> {
    (b >= a).then(|| ((b - a) as f64 / secs) as u64)
}

/// Second iteration of `top -b -n 2 -d 1`: global busy % and per-process %CPU
/// (top's default "Irix" mode: percent of one core).
struct Top {
    busy: Option<f64>,
    procs: Vec<(u32, f64, String)>,
}

fn top_window() -> Option<Top> {
    let out = run("top", &["-b", "-n", "2", "-d", "1", "-w", "512"])?;
    let second = out.rmatch_indices("%Cpu(s):").next()?.0;
    let text = &out[second..];
    let cpu_line = text.lines().next()?;
    let field = |name: &str| -> Option<f64> {
        cpu_line
            .split(',')
            .find(|p| p.trim_end().ends_with(name))?
            .split_whitespace()
            .rev()
            .nth(1)?
            .parse()
            .ok()
    };
    let busy = match (field(" id"), field(" wa")) {
        (Some(id), Some(wa)) => Some(100.0 - id - wa),
        _ => None,
    };
    let mut lines = text.lines().skip_while(|l| !l.trim_start().starts_with("PID"));
    let header: Vec<&str> = lines.next()?.split_whitespace().collect();
    let pid_col = header.iter().position(|h| *h == "PID")?;
    let cpu_col = header.iter().position(|h| *h == "%CPU")?;
    let cmd_col = header.iter().position(|h| *h == "COMMAND")?;
    let procs = lines
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            Some((f.get(pid_col)?.parse().ok()?, f.get(cpu_col)?.parse().ok()?, f.get(cmd_col..)?.join(" ")))
        })
        .collect();
    Some(Top { busy, procs })
}

/// `free -b`: (total, used, buff/cache, available, swap total, swap used).
fn free() -> Option<[u64; 6]> {
    let out = run("free", &["-b"])?;
    let nums = |prefix: &str| -> Option<Vec<u64>> {
        let l = out.lines().find(|l| l.starts_with(prefix))?;
        Some(l.split_whitespace().skip(1).filter_map(|x| x.parse().ok()).collect())
    };
    let m = nums("Mem:")?;
    let s = nums("Swap:")?;
    Some([*m.first()?, *m.get(1)?, *m.get(4)?, *m.get(5)?, *s.first()?, *s.get(1)?])
}

/// `ps -e`: pid → (rss bytes, threads).
fn ps() -> Option<HashMap<u32, (u64, u32)>> {
    let out = run("ps", &["-e", "-o", "pid=,rss=,nlwp="])?;
    Some(
        out.lines()
            .filter_map(|l| {
                let f: Vec<u64> = l.split_whitespace().filter_map(|x| x.parse().ok()).collect();
                Some((*f.first()? as u32, (*f.get(1)? * 1024, *f.get(2)? as u32)))
            })
            .collect(),
    )
}

/// `df -B1`: mount → (size, avail, df's own "used").
fn df(mounts: &[&str]) -> HashMap<String, (u64, u64, u64)> {
    let mut args = vec!["-B1", "--output=target,size,used,avail"];
    args.extend_from_slice(mounts);
    run("df", &args)
        .unwrap_or_default()
        .lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let n = f.len();
            Some((
                f.get(..n.checked_sub(3)?)?.join(" "),
                (f[n - 3].parse().ok()?, f[n - 1].parse().ok()?, f[n - 2].parse().ok()?),
            ))
        })
        .collect()
}

/// `sensors -u <chip>`: value of the first `<field>` line.
fn sensors(chip: &str, field: &str) -> Option<f64> {
    run("sensors", &["-u", chip])?
        .lines()
        .find_map(|l| l.trim().strip_prefix(field)?.strip_prefix(':')?.trim().parse().ok())
}

fn cpuinfo_mhz() -> Option<f64> {
    let text = read("/proc/cpuinfo")?;
    let v: Vec<f64> = text
        .lines()
        .filter(|l| l.starts_with("cpu MHz"))
        .filter_map(|l| l.split(':').nth(1)?.trim().parse().ok())
        .collect();
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

// ------------------------------------------------------------------- NVIDIA

struct PowerState {
    status: String,
    active_ms: u64,
}

fn runtime_pm(dev: &Path) -> Option<PowerState> {
    Some(PowerState {
        status: read(dev.join("power/runtime_status"))?,
        active_ms: read_u64(dev.join("power/runtime_active_time"))?,
    })
}

/// Own processes holding an NVIDIA device node open: a hint for who woke
/// the card (the compositor keeps one open without preventing sleep).
fn nvidia_holders() -> Vec<String> {
    let mut v = Vec::new();
    for e in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
        let Ok(fds) = fs::read_dir(e.path().join("fd")) else { continue };
        if fds.flatten().any(|fd| fs::read_link(fd.path()).is_ok_and(|l| l.starts_with("/dev/nvidia"))) {
            v.push(format!("{pid} {}", read(e.path().join("comm")).unwrap_or_default()));
        }
    }
    v
}

// -------------------------------------------------------------------- round

fn round(src: &mut LinuxSource, report: &mut Report, nvidia_smi: bool) {
    let disks = physical_disks();
    let ifaces = physical_ifaces();

    // Window: Dotmon sample A → top (2 iterations, 1 s apart) → sample B.
    let start = src.sample();
    // Whole disks for the totals, mounted partitions for the per-row rates.
    let mut blocks = disks.clone();
    blocks.extend(start.disks.iter().map(|d| d.name.clone()));
    let c0 = counters(&blocks, &ifaces);
    let top = top_window();
    let snap = src.sample();
    let c1 = counters(&blocks, &ifaces);
    let secs = c1.at.duration_since(c0.at).as_secs_f64();
    let ncpu = snap.cpu.per_core.len() as f64;

    // ---- CPU
    match top.as_ref().and_then(|t| t.busy) {
        Some(busy) => report.num("cpu.global", "CPU global", "top", busy, snap.cpu.usage as f64, 5.0, " %"),
        None => report.unavailable("cpu.global", "CPU global", "top", "illisible"),
    }
    report.num(
        "cpu.cores",
        "Cœurs logiques",
        "/proc/cpuinfo",
        read("/proc/cpuinfo").map(|t| t.lines().filter(|l| l.starts_with("processor")).count()).unwrap_or(0)
            as f64,
        ncpu,
        0.0,
        "",
    );
    match (cpuinfo_mhz(), snap.cpu.frequency_mhz) {
        (Some(r), Some(d)) => report.num(
            "cpu.freq",
            "Fréquence moyenne (indicatif)",
            "/proc/cpuinfo",
            r,
            d as f64,
            r * 0.25,
            " MHz",
        ),
        (_, None) => report.unavailable("cpu.freq", "Fréquence moyenne", "/proc/cpuinfo", "Dotmon: Indisp."),
        (None, _) => report.unavailable("cpu.freq", "Fréquence moyenne", "/proc/cpuinfo", "illisible"),
    }
    match (sensors("k10temp-*", "temp1_input"), snap.cpu.temperature_c) {
        (Some(r), Some(d)) => {
            report.num("cpu.temp", "Température CPU (Tctl)", "sensors", r, d as f64, 3.0, " °C")
        }
        (r, d) => report.unavailable(
            "cpu.temp",
            "Température CPU",
            "sensors",
            &format!("outil {} · Dotmon {}", r.is_some(), d.is_some()),
        ),
    }
    if let (Some(l), Some(d)) = (read("/proc/loadavg"), snap.cpu.load_avg) {
        let r: f64 = l.split_whitespace().next().and_then(|x| x.parse().ok()).unwrap_or(0.0);
        report.num("cpu.load", "Charge 1 min", "/proc/loadavg", r, d[0] as f64, 0.3 + r * 0.1, "");
    }

    // Per-process CPU: the busiest processes according to top.
    if let Some(t) = &top {
        let mut busiest: Vec<_> = t.procs.iter().filter(|(_, c, _)| *c >= 5.0).collect();
        busiest.sort_by(|a, b| b.1.total_cmp(&a.1));
        for (pid, cpu, cmd) in busiest.into_iter().take(5) {
            let Some(p) = snap.processes.iter().find(|p| p.pid == *pid) else { continue };
            let known_before = start.processes.iter().any(|s| s.id() == p.id());
            match (known_before, p.cpu) {
                // Born during the window: there is no earlier reading, so
                // Dotmon must say "—" (top counts the process' whole life).
                (false, got) => report.push(
                    "proc.cpu.new",
                    format!("CPU pid {pid} {cmd} (nouveau)"),
                    "top",
                    format!("{cpu:.1} %"),
                    got.map(|v| format!("{:.1} %", v as f64 * ncpu)).unwrap_or_else(|| "—".into()),
                    "doit être —".into(),
                    "".into(),
                    Some(got.is_none()),
                ),
                (true, None) => report.push(
                    "proc.cpu.missing",
                    format!("CPU pid {pid} {cmd}"),
                    "top",
                    format!("{cpu:.1} %"),
                    "—".into(),
                    "mesure manquante".into(),
                    "".into(),
                    Some(false),
                ),
                // Dotmon: % of the whole machine; top: % of one core.
                (true, Some(v)) => report.num(
                    &format!("proc.cpu.{pid}"),
                    &format!("CPU pid {pid} {cmd} (×{ncpu:.0})"),
                    "top",
                    *cpu,
                    v as f64 * ncpu,
                    (cpu * 0.25).max(4.0),
                    " %",
                ),
            }
        }
        // Systematic bias, measurable only on busy processes (≥ 50 % of a
        // core: one tick of jitter is then ≤ 2 %), averaged over all of them.
        let ratios: Vec<f64> = t
            .procs
            .iter()
            .filter(|(_, c, _)| *c >= 50.0)
            .filter_map(|(pid, c, _)| {
                let p = snap.processes.iter().find(|p| p.pid == *pid)?;
                let known_before = start.processes.iter().any(|s| s.id() == p.id());
                known_before.then_some(p.cpu? as f64 * ncpu / c)
            })
            .collect();
        if !ratios.is_empty() {
            let bias = (ratios.iter().sum::<f64>() / ratios.len() as f64 - 1.0) * 100.0;
            report.push(
                "proc.cpu.bias",
                format!("Biais CPU/processus ({} à ≥ 50 %)", ratios.len()),
                "top",
                "0.0 %".into(),
                format!("{bias:+.1} %"),
                format!("{bias:+.1} %"),
                "± 1.5 %".into(),
                Some(bias.abs() <= 1.5),
            );
        }
    }

    // ---- Memory (instantaneous, read right after sample B)
    match free() {
        Some([total, used, cache, avail, swap_total, swap_used]) => {
            let m = &snap.memory;
            report.bytes("mem.total", "RAM totale", "free", total, m.total, 0);
            report.bytes("mem.used", "RAM utilisée", "free", used, m.used, 64 * MIB);
            report.bytes("mem.avail", "RAM disponible", "free", avail, m.available, 64 * MIB);
            match m.cached {
                Some(c) => report.bytes("mem.cache", "Cache (buff/cache)", "free", cache, c, 64 * MIB),
                None => report.unavailable("mem.cache", "Cache", "free", "Dotmon: Indisp."),
            }
            report.bytes("mem.swap_total", "Swap total", "free", swap_total, m.swap_total, 0);
            report.bytes("mem.swap_used", "Swap utilisé", "free", swap_used, m.swap_used, 16 * MIB);
        }
        None => report.unavailable("mem", "RAM", "free", "illisible"),
    }

    // ---- Processes
    if let Some(table) = ps() {
        report.num(
            "proc.count",
            "Nombre de processus",
            "ps",
            table.len() as f64,
            snap.processes.len() as f64,
            6.0,
            "",
        );
        let threads: u64 = table.values().map(|(_, t)| *t as u64).sum();
        let ours: u64 = snap.processes.iter().map(|p| p.threads as u64).sum();
        report.num("proc.threads", "Nombre de threads", "ps", threads as f64, ours as f64, 30.0, "");
        // RSS of every process present in both: count the outliers.
        let (mut compared, mut off, mut worst) = (0, 0, (0u32, 0i128));
        for p in &snap.processes {
            if let Some((rss, _)) = table.get(&p.pid) {
                compared += 1;
                let d = p.memory as i128 - *rss as i128;
                if d.unsigned_abs() > (4 * MIB).max(rss / 20) as u128 {
                    off += 1;
                }
                if d.abs() > worst.1.abs() {
                    worst = (p.pid, d);
                }
            }
        }
        report.push(
            "proc.rss",
            format!("RSS ({compared} processus comparés)"),
            "ps",
            format!("{off} hors tol."),
            format!("pire pid {}", worst.0),
            signed_bytes(worst.1),
            "± max(4 Mo, 5 %)".into(),
            Some(off <= compared / 50 + 1),
        );
    }

    // ---- Disks: space
    let mounts: Vec<&str> = snap.disks.iter().map(|d| d.mount_point.as_str()).collect();
    let dfs = df(&mounts);
    for d in &snap.disks {
        match dfs.get(&d.mount_point) {
            Some((size, avail, df_used)) => {
                report.bytes(
                    &format!("disk.total.{}", d.mount_point),
                    &format!("{} taille", d.mount_point),
                    "df",
                    *size,
                    d.total,
                    0,
                );
                report.bytes(
                    &format!("disk.used.{}", d.mount_point),
                    &format!("{} utilisé (taille − dispo)", d.mount_point),
                    "df",
                    size - avail,
                    d.used,
                    64 * MIB,
                );
                report.push(
                    format!("disk.reserve.{}", d.mount_point),
                    format!("{} réserve non dispo (df)", d.mount_point),
                    "df",
                    format::bytes(size - avail - df_used),
                    "".into(),
                    "".into(),
                    "".into(),
                    None,
                );
            }
            None => {
                report.unavailable(&format!("disk.{}", d.mount_point), &d.mount_point, "df", "absent de df")
            }
        }
    }

    // ---- Disks: throughput over the window
    let rates = |name: &str| -> Option<(u64, u64)> {
        let (r0, w0) = c0.blocks.get(name)?;
        let (r1, w1) = c1.blocks.get(name)?;
        Some((rate(*r0, *r1, secs)?, rate(*w0, *w1, secs)?))
    };
    let (mut tr, mut tw) = (0, 0);
    for d in &disks {
        if let Some((r, w)) = rates(d) {
            tr += r;
            tw += w;
        }
    }
    match snap.disk_io {
        Some(io) => {
            report.rate("disk.read", "Disques: lecture totale", "/sys/block", tr, io.read_bps, 0.2, MIB);
            report.rate("disk.write", "Disques: écriture totale", "/sys/block", tw, io.write_bps, 0.2, MIB);
        }
        None => report.unavailable("disk.io", "Disques: débit total", "/sys/block", "Dotmon: Indisp."),
    }
    for d in &snap.disks {
        if let (Some((r, w)), Some(dr), Some(dw)) = (rates(&d.name), d.read_bps, d.write_bps) {
            report.rate(
                &format!("disk.read.{}", d.name),
                &format!("{} lecture", d.name),
                "/sys/block",
                r,
                dr,
                0.2,
                MIB,
            );
            report.rate(
                &format!("disk.write.{}", d.name),
                &format!("{} écriture", d.name),
                "/sys/block",
                w,
                dw,
                0.2,
                MIB,
            );
        }
    }

    // ---- Network
    let listed: Vec<&str> = snap.networks.iter().map(|n| n.interface.as_str()).collect();
    report.push(
        "net.ifaces",
        "Interfaces physiques actives",
        "/sys/class/net",
        ifaces.join(" "),
        listed.join(" "),
        "".into(),
        "identiques".into(),
        Some(listed.len() == ifaces.len() && ifaces.iter().all(|i| listed.contains(&i.as_str()))),
    );
    for n in &snap.networks {
        let (Some((rx0, tx0)), Some((rx1, tx1))) = (c0.nets.get(&n.interface), c1.nets.get(&n.interface))
        else {
            continue;
        };
        let (Some(rx), Some(tx)) = (rate(*rx0, *rx1, secs), rate(*tx0, *tx1, secs)) else { continue };
        let i = &n.interface;
        report.rate(
            &format!("net.rx.{i}"),
            &format!("{i} réception"),
            "ip -s link",
            rx,
            n.rx_bps,
            0.15,
            32 * 1024,
        );
        report.rate(
            &format!("net.tx.{i}"),
            &format!("{i} émission"),
            "ip -s link",
            tx,
            n.tx_bps,
            0.15,
            32 * 1024,
        );
        // Totals: Dotmon's counter was read inside sample B, just before c1.
        report.bytes(
            &format!("net.total.{i}"),
            &format!("{i} total reçu"),
            "ip -s link",
            *rx1,
            n.rx_total,
            MIB,
        );
    }

    // ---- GPUs (order of `gpu_devices()` = order of `snap.gpus`)
    for (dev, g) in src.gpu_devices().iter().zip(&snap.gpus) {
        let name = format!("GPU {}", dev.pci_address);
        let invented = g.state != GpuState::Active
            && (g.usage.is_some()
                || g.vram_used.is_some()
                || g.temperature_c.is_some()
                || g.power_w.is_some()
                || g.soc_power_w.is_some());
        let pm = runtime_pm(&dev.device_dir);
        let status = pm.as_ref().map(|p| p.status.as_str()).unwrap_or("?");
        // A suspended card must be shown asleep; an awake one may be Active
        // or Resting (deliberately not polled).
        let coherent = match (status, g.state) {
            ("suspended", GpuState::Asleep) => true,
            ("suspended", _) => false,
            (_, GpuState::Asleep) => status == "suspending" || status == "resuming",
            _ => true,
        };
        report.push(
            format!("gpu.state.{}", dev.pci_address),
            format!("{name} état"),
            "sysfs",
            status.to_string(),
            format!("{:?}", g.state),
            "".into(),
            "cohérent".into(),
            Some(coherent && !invented),
        );
        match dev.vendor {
            Vendor::Amd => {
                let d = &dev.device_dir;
                if let (Some(r), Some(u)) = (read_u64(d.join("gpu_busy_percent")), g.usage) {
                    // Instantaneous counter, noisy: indicative.
                    report.num(
                        &format!("gpu.usage.{}", dev.pci_address),
                        &format!("{name} utilisation (indicatif)"),
                        "sysfs",
                        r as f64,
                        u as f64,
                        25.0,
                        " %",
                    );
                }
                if let (Some(r), Some(u)) = (read_u64(d.join("mem_info_vram_used")), g.vram_used) {
                    report.bytes(
                        &format!("gpu.vram.{}", dev.pci_address),
                        &format!("{name} VRAM utilisée"),
                        "sysfs",
                        r,
                        u,
                        32 * MIB,
                    );
                }
                if let (Some(r), Some(t)) = (sensors("amdgpu-*", "temp1_input"), g.temperature_c) {
                    report.num(
                        &format!("gpu.temp.{}", dev.pci_address),
                        &format!("{name} température"),
                        "sensors",
                        r,
                        t as f64,
                        3.0,
                        " °C",
                    );
                }
                // amdgpu's power1_* is the SoC's (CPU included on an APU): it
                // must reach Dotmon as `soc_power_w`, never as the GPU's own.
                let apu = dev
                    .device_dir
                    .join("hwmon")
                    .read_dir()
                    .is_ok_and(|mut d| d.any(|h| h.is_ok_and(|h| h.path().join("in1_input").exists())));
                if g.state == GpuState::Active && (apu || g.power_w.is_some() || g.soc_power_w.is_some()) {
                    report.push(
                        format!("gpu.power.scope.{}", dev.pci_address),
                        format!("{name} puissance : périmètre"),
                        "sysfs",
                        if apu { "APU → SoC".into() } else { "discret → GPU".into() },
                        format!(
                            "GPU {} · SoC {}",
                            g.power_w.map_or("—".into(), |w| format!("{w:.1}")),
                            g.soc_power_w.map_or("—".into(), |w| format!("{w:.1}"))
                        ),
                        "".into(),
                        "périmètre exact".into(),
                        Some(if apu { g.power_w.is_none() } else { g.soc_power_w.is_none() }),
                    );
                }
                if let (Some(r), Some(p)) =
                    (sensors("amdgpu-*", "power1_average"), g.soc_power_w.or(g.power_w))
                {
                    report.num(
                        &format!("gpu.power.{}", dev.pci_address),
                        &format!("{name} puissance {}", if apu { "SoC" } else { "GPU" }),
                        "sensors",
                        r,
                        p as f64,
                        5.0,
                        " W",
                    );
                }
            }
            Vendor::Nvidia if nvidia_smi && status == "active" && g.state == GpuState::Active => {
                // The card is already powered: querying it does not wake it.
                let q = run(
                    "nvidia-smi",
                    &[
                        &format!("--id={}", dev.pci_address),
                        "--query-gpu=utilization.gpu,memory.used,memory.total,temperature.gpu,power.draw",
                        "--format=csv,noheader,nounits",
                    ],
                );
                let v: Vec<f64> =
                    q.unwrap_or_default().split(',').filter_map(|x| x.trim().parse().ok()).collect();
                if let [u, mu, mt, t, p] = v[..] {
                    let pci = &dev.pci_address;
                    if let Some(d) = g.usage {
                        report.num(
                            &format!("gpu.usage.{pci}"),
                            &format!("{name} utilisation"),
                            "nvidia-smi",
                            u,
                            d as f64,
                            15.0,
                            " %",
                        );
                    }
                    if let Some(d) = g.vram_used {
                        report.bytes(
                            &format!("gpu.vram.{pci}"),
                            &format!("{name} VRAM utilisée"),
                            "nvidia-smi",
                            (mu * MIB as f64) as u64,
                            d,
                            16 * MIB,
                        );
                    }
                    if let Some(d) = g.vram_total {
                        report.bytes(
                            &format!("gpu.vramt.{pci}"),
                            &format!("{name} VRAM totale"),
                            "nvidia-smi",
                            (mt * MIB as f64) as u64,
                            d,
                            MIB,
                        );
                    }
                    if let Some(d) = g.temperature_c {
                        report.num(
                            &format!("gpu.temp.{pci}"),
                            &format!("{name} température"),
                            "nvidia-smi",
                            t,
                            d as f64,
                            2.0,
                            " °C",
                        );
                    }
                    if let Some(d) = g.power_w {
                        report.num(
                            &format!("gpu.power.{pci}"),
                            &format!("{name} puissance"),
                            "nvidia-smi",
                            p,
                            d as f64,
                            5.0,
                            " W",
                        );
                    }
                }
            }
            Vendor::Nvidia => report.unavailable(
                &format!("gpu.values.{}", dev.pci_address),
                &format!("{name} valeurs"),
                "nvidia-smi",
                "non interrogé",
            ),
            _ => {}
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rounds: usize = args.iter().find_map(|a| a.parse().ok()).unwrap_or(5).max(1);
    let nvidia_smi = args.iter().any(|a| a == "--nvidia-smi");

    let mut src = LinuxSource::new().expect("Linux backend");
    let nvidia: Vec<PathBuf> = src
        .gpu_devices()
        .iter()
        .filter(|d| d.vendor == Vendor::Nvidia)
        .map(|d| d.device_dir.clone())
        .collect();
    let pm0: Vec<_> = nvidia.iter().map(|d| runtime_pm(d)).collect();
    for (d, pm) in nvidia.iter().zip(&pm0) {
        if let Some(pm) = pm {
            println!("NVIDIA {} au départ : {}", d.display(), pm.status);
        }
    }

    let mut all: Vec<Check> = Vec::new();
    for i in 1..=rounds {
        let mut report = Report::default();
        round(&mut src, &mut report, nvidia_smi);
        println!("\n=== Fenêtre {i}/{rounds} ===");
        report.print();
        all.extend(report.checks);
    }

    // ---- Summary: a discrepancy seen in most windows is confirmed.
    let mut by_key: BTreeMap<&str, (usize, usize, &str)> = BTreeMap::new();
    for c in &all {
        let e = by_key.entry(&c.key).or_insert((0, 0, &c.label));
        if c.verdict != Verdict::Info {
            e.0 += 1;
            if c.verdict == Verdict::Ecart {
                e.1 += 1;
            }
        }
    }
    let threshold = (rounds * 3).div_ceil(5).max(2);
    println!("\n=== Synthèse ({rounds} fenêtres, CONFIRMÉ = écart dans ≥ {threshold} fenêtres) ===");
    let mut confirmed = 0;
    for (key, (n, bad, label)) in &by_key {
        if *bad == 0 {
            continue;
        }
        let verdict = if *bad >= threshold && rounds >= 2 {
            confirmed += 1;
            "CONFIRMÉ"
        } else {
            "ponctuel (bruit probable)"
        };
        println!("  {label:<40} {bad}/{n} fenêtres en écart → {verdict}   [{key}]");
    }
    if by_key.values().all(|(_, bad, _)| *bad == 0) {
        println!("  Aucun écart hors tolérance.");
    }

    // ---- NVIDIA power: did anything wake the card during the run?
    for ((d, before), after) in nvidia.iter().zip(&pm0).zip(nvidia.iter().map(|d| runtime_pm(d))) {
        if let (Some(b), Some(a)) = (before, after) {
            let woke = a.active_ms.saturating_sub(b.active_ms);
            println!(
                "\nNVIDIA {} : {} → {}, temps actif pendant la validation : {} ms",
                d.display(),
                b.status,
                a.status,
                woke
            );
            if b.status == "suspended" && woke > 0 {
                println!(
                    "  ATTENTION : la carte a été réveillée. Processus ayant un /dev/nvidia* ouvert (indice) :"
                );
                for h in nvidia_holders() {
                    println!("    {h}");
                }
            }
        }
    }
    std::process::exit(if confirmed > 0 { 2 } else { 0 });
}
