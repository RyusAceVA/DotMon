//! Prints what the Linux backend measures, to compare with system tools.
//!
//! ```fish
//! cargo run --release -p dotmon-sys --example probe -- 5      # 5 samples, 1 s apart
//! ```

use std::time::{Duration, Instant};

use dotmon_core::format;
use dotmon_core::source::MetricsSource;
use dotmon_sys::linux::LinuxSource;

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map(|v| v.to_string()).unwrap_or_else(|| "Indisponible".into())
}

fn main() {
    let n: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(3);
    let t0 = Instant::now();
    let mut src = LinuxSource::new().expect("Linux backend");
    println!("init: {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);

    for i in 0..n {
        std::thread::sleep(Duration::from_secs(1));
        let t = Instant::now();
        let s = src.sample();
        let cost = t.elapsed().as_secs_f64() * 1000.0;
        println!("\n=== sample {} (collecte {cost:.1} ms) ===", i + 1);
        println!("host {} · kernel {} · uptime {} s", s.hostname, s.kernel, s.uptime_s);
        let c = &s.cpu;
        println!(
            "CPU {:.1} % · {} · freq {} MHz · temp {} °C · load {:?}",
            c.usage,
            c.model,
            opt(c.frequency_mhz),
            opt(c.temperature_c),
            c.load_avg
        );
        println!("  cores {:?}", c.per_core.iter().map(|v| format!("{v:.0}")).collect::<Vec<_>>());
        let m = &s.memory;
        println!(
            "MEM total {} KiB · used {} KiB · avail {} KiB · buff/cache {} KiB · swap {}/{} KiB",
            m.total / 1024,
            m.used / 1024,
            m.available / 1024,
            opt(m.cached.map(|c| c / 1024)),
            m.swap_used / 1024,
            m.swap_total / 1024
        );
        for g in &s.gpus {
            println!(
                "GPU {} [{}] état={:?} usage={} vram={}/{} temp={} puissance GPU={} · puissance SoC={}",
                g.name,
                g.vendor,
                g.state,
                opt(g.usage),
                opt(g.vram_used),
                opt(g.vram_total),
                opt(g.temperature_c),
                opt(g.power_w),
                opt(g.soc_power_w)
            );
        }
        for d in &s.disks {
            println!(
                "DISK {} on {} ({}) used {} / {} · r {} w {}",
                d.name,
                d.mount_point,
                d.filesystem,
                d.used,
                d.total,
                opt(d.read_bps),
                opt(d.write_bps)
            );
        }
        println!(
            "DISK total (physique) r {} w {}",
            format::rate(s.disk_read_bps()),
            format::rate(s.disk_write_bps())
        );
        for net in &s.networks {
            println!(
                "NET {} rx {} tx {} · totals rx {} tx {} bytes",
                net.interface,
                format::rate(net.rx_bps),
                format::rate(net.tx_bps),
                net.rx_total,
                net.tx_total
            );
        }
        let threads: u64 = s.processes.iter().map(|p| p.threads as u64).sum();
        println!("PROC {} processus · {} threads", s.processes.len(), threads);
        let mut by_cpu: Vec<_> = s.processes.iter().collect();
        by_cpu.sort_by(|a, b| b.cpu.partial_cmp(&a.cpu).unwrap_or(std::cmp::Ordering::Equal));
        for p in by_cpu.iter().take(5) {
            let (machine, one_core) = match p.cpu {
                Some(v) => (format!("{v:>5.1} %"), format!("{:>6.1} %", v * c.per_core.len() as f32)),
                None => ("    — ".to_string(), "     — ".to_string()),
            };
            println!(
                "  pid {:>7} {:<16} cpu {machine} (machine) = {one_core} (1 cœur) rss {} KiB thr {}",
                p.pid,
                p.name,
                p.memory / 1024,
                p.threads
            );
        }
    }
}
