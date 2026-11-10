//! GPU discovery and sampling.
//!
//! GPUs are discovered once from `/sys/class/drm/card*`. Each sample first
//! reads the PCI runtime-PM status: a runtime-suspended GPU (typical for the
//! discrete GPU of a hybrid laptop) is reported as asleep and **not**
//! queried, because NVML or amdgpu sysfs reads would power it up.
//!
//! - NVIDIA: NVML (`nvml-wrapper`), opened for the duration of one sample
//!   only. Holding the NVML handle open keeps `/dev/nvidia*` in use, which
//!   prevents the GPU from runtime-suspending again.
//!
//! Polling itself keeps a GPU awake: measured on an RTX 5070 Laptop, one
//! NVML query per second prevented runtime suspend indefinitely (it
//! otherwise suspends ~19 s after the last client). So, for devices with
//! runtime PM enabled (`power/control = auto`), when a measurement shows the
//! GPU idle (0 % and no process on it) polling pauses for [`REST`]: the GPU
//! is reported as `Resting`, then `Asleep` once the driver powers it down.
//! A GPU in real use stays awake anyway and is measured every sample.
//! - AMD: amdgpu sysfs (`gpu_busy_percent`, `mem_info_vram_*`, hwmon).
//! - Others: name only; usage reported as unavailable.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dotmon_core::model::{GpuState, GpuStats};
use nvml_wrapper::Nvml;
use nvml_wrapper::enum_wrappers::device::TemperatureSensor;

use super::procfs::{marketing_name, pci_name, read_u64, runtime_status};

/// How long an idle runtime-PM GPU is left alone; longer than the ~19 s
/// the NVIDIA driver needed to suspend after the last client went away.
pub const REST: Duration = Duration::from_secs(30);

const PCI_IDS_PATHS: &[&str] = &["/usr/share/hwdata/pci.ids", "/usr/share/misc/pci.ids"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Nvidia,
    Amd,
    Intel,
    Other,
}

impl Vendor {
    fn from_id(id: u16) -> Self {
        match id {
            0x10de => Vendor::Nvidia,
            0x1002 => Vendor::Amd,
            0x8086 => Vendor::Intel,
            _ => Vendor::Other,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Vendor::Nvidia => "NVIDIA",
            Vendor::Amd => "AMD",
            Vendor::Intel => "Intel",
            Vendor::Other => "GPU",
        }
    }
}

#[derive(Debug, Clone)]
pub struct GpuDevice {
    pub vendor: Vendor,
    /// `/sys/class/drm/cardN/device` (resolved PCI device directory).
    pub device_dir: PathBuf,
    /// PCI address, e.g. `0000:64:00.0`.
    pub pci_address: String,
    pub name: String,
    /// Runtime power management enabled (`power/control = auto`): the
    /// device can power down, so idle polling must back off.
    pub runtime_pm: bool,
}

fn parse_hex_u16(s: &str) -> Option<u16> {
    u16::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
}

/// Lists display adapters, discrete NVIDIA first, then in card order.
pub fn discover(sys_root: &Path) -> Vec<GpuDevice> {
    let Ok(entries) = std::fs::read_dir(sys_root.join("class/drm")) else { return Vec::new() };
    let mut cards: Vec<(u32, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let n: u32 = name.strip_prefix("card")?.parse().ok()?; // skips connectors like card1-DP-1
            Some((n, e.path()))
        })
        .collect();
    cards.sort();

    let pci_db = PCI_IDS_PATHS.iter().find_map(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
    let mut out: Vec<GpuDevice> = Vec::new();
    for (_, card) in cards {
        let Ok(device_dir) = std::fs::canonicalize(card.join("device")) else { continue };
        let Some(vendor_id) =
            std::fs::read_to_string(device_dir.join("vendor")).ok().and_then(|s| parse_hex_u16(&s))
        else {
            continue;
        };
        let device_id =
            std::fs::read_to_string(device_dir.join("device")).ok().and_then(|s| parse_hex_u16(&s));
        let pci_address =
            device_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if out.iter().any(|g| g.pci_address == pci_address) {
            continue;
        }
        let vendor = Vendor::from_id(vendor_id);
        let name = device_id
            .and_then(|d| pci_name(&pci_db, vendor_id, d))
            .map(|(_, dev)| format!("{} {}", vendor.label(), marketing_name(&dev)))
            .unwrap_or_else(|| format!("{} {:04x}", vendor.label(), device_id.unwrap_or(0)));
        let runtime_pm =
            std::fs::read_to_string(device_dir.join("power/control")).is_ok_and(|c| c.trim() == "auto");
        out.push(GpuDevice { vendor, device_dir, pci_address, name, runtime_pm });
    }
    out.sort_by_key(|g| g.vendor != Vendor::Nvidia); // stable: keeps card order otherwise
    out
}

/// Parses `0000:64:00.0` into (domain, bus, device).
fn pci_triplet(addr: &str) -> Option<(u32, u32, u32)> {
    let mut it = addr.split(':');
    let domain = u32::from_str_radix(it.next()?, 16).ok()?;
    let bus = u32::from_str_radix(it.next()?, 16).ok()?;
    let dev = u32::from_str_radix(it.next()?.split('.').next()?, 16).ok()?;
    Some((domain, bus, dev))
}

/// Returns whether the GPU is in use (busy or with processes on it).
fn sample_nvidia(dev: &GpuDevice, stats: &mut GpuStats) -> bool {
    // Scoped: the NVML handle is shut down when `nvml` drops at the end.
    let Ok(nvml) = Nvml::init() else { return true };
    let Some(target) = pci_triplet(&dev.pci_address) else { return true };
    let count = nvml.device_count().unwrap_or(0);
    for i in 0..count {
        let Ok(d) = nvml.device_by_index(i) else { continue };
        let matches = d.pci_info().is_ok_and(|p| (p.domain, p.bus, p.device) == target);
        if !matches {
            continue;
        }
        if let Ok(name) = d.name() {
            stats.name = name;
        }
        stats.usage = d.utilization_rates().ok().map(|u| u.gpu as f32);
        if let Ok(m) = d.memory_info() {
            stats.vram_total = Some(m.total);
            stats.vram_used = Some(m.used);
        }
        stats.temperature_c = d.temperature(TemperatureSensor::Gpu).ok().map(|t| t as f32);
        stats.power_w = d.power_usage().ok().map(|mw| mw as f32 / 1000.0);
        // Unknown counts are treated as "in use": never back off blindly.
        let procs = d.running_compute_processes_count().unwrap_or(1)
            + d.running_graphics_processes_count().unwrap_or(1);
        return procs > 0 || stats.usage.is_none_or(|u| u > 0.0);
    }
    true
}

/// First `hwmon*` directory of a device.
fn hwmon_dir(device_dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(device_dir.join("hwmon")).ok()?.filter_map(Result::ok).map(|e| e.path()).next()
}

/// An amdgpu device is an APU when its hwmon exposes the Northbridge
/// voltage (`in1_*`, "vddnb"), which the kernel documents as APU-only.
/// Sensors of a discrete card measure the card's own chip.
fn is_apu(hwmon: &Path) -> bool {
    hwmon.join("in1_input").exists()
        || std::fs::read_to_string(hwmon.join("in1_label")).is_ok_and(|l| l.trim() == "vddnb")
}

fn sample_amd(dev: &GpuDevice, stats: &mut GpuStats) -> bool {
    let d = &dev.device_dir;
    stats.usage = read_u64(&d.join("gpu_busy_percent")).map(|v| v as f32);
    stats.vram_total = read_u64(&d.join("mem_info_vram_total"));
    stats.vram_used = read_u64(&d.join("mem_info_vram_used"));
    if let Some(hw) = hwmon_dir(d) {
        stats.temperature_c = read_u64(&hw.join("temp1_input")).map(|m| m as f32 / 1000.0);
        let watts = read_u64(&hw.join("power1_average"))
            .or_else(|| read_u64(&hw.join("power1_input")))
            .map(|uw| uw as f32 / 1_000_000.0);
        // amdgpu documents power1_* as the power "used by the SoC"; "on APUs
        // this includes the CPU" (kernel docs, gpu/amdgpu/thermal). Such a
        // figure is not the GPU's: keep it apart, whatever its label ("PPT").
        if is_apu(&hw) {
            stats.soc_power_w = watts;
        } else {
            stats.power_w = watts;
        }
    }
    stats.usage.is_none_or(|u| u > 0.0)
}

/// Samples GPUs while respecting their power state (see module docs).
pub struct GpuMonitor {
    devices: Vec<GpuDevice>,
    rest_until: Vec<Option<Instant>>,
}

impl GpuMonitor {
    pub fn new(devices: Vec<GpuDevice>) -> Self {
        let rest_until = vec![None; devices.len()];
        Self { devices, rest_until }
    }

    pub fn devices(&self) -> &[GpuDevice] {
        &self.devices
    }

    pub fn sample(&mut self, now: Instant) -> Vec<GpuStats> {
        self.devices
            .iter()
            .zip(self.rest_until.iter_mut())
            .map(|(d, rest)| sample_one(d, rest, now))
            .collect()
    }
}

fn sample_one(dev: &GpuDevice, rest_until: &mut Option<Instant>, now: Instant) -> GpuStats {
    let mut stats =
        GpuStats { name: dev.name.clone(), vendor: dev.vendor.label().to_string(), ..Default::default() };
    // Missing runtime PM support means the device is always powered.
    let status = runtime_status(&dev.device_dir).unwrap_or_else(|| "active".into());
    if status != "active" {
        *rest_until = None;
        stats.state = GpuState::Asleep;
        return stats;
    }
    if rest_until.is_some_and(|t| now < t) {
        stats.state = GpuState::Resting;
        return stats;
    }
    *rest_until = None;
    let in_use = match dev.vendor {
        Vendor::Nvidia => sample_nvidia(dev, &mut stats),
        Vendor::Amd => sample_amd(dev, &mut stats),
        Vendor::Intel | Vendor::Other => true,
    };
    if dev.runtime_pm && !in_use {
        *rest_until = Some(now + REST);
    }
    stats.state = GpuState::Active;
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pci_address_parsing() {
        assert_eq!(pci_triplet("0000:64:00.0"), Some((0, 0x64, 0)));
        assert_eq!(pci_triplet("0001:0a:1f.3"), Some((1, 0x0a, 0x1f)));
        assert_eq!(pci_triplet("bogus"), None);
    }

    #[test]
    fn hex_ids() {
        assert_eq!(parse_hex_u16("0x10de\n"), Some(0x10de));
        assert_eq!(parse_hex_u16("zz"), None);
    }

    fn fake_device(tag: &str, runtime_pm: bool) -> GpuDevice {
        let dir = std::env::temp_dir().join(format!("dotmon-gpu-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("power")).unwrap();
        GpuDevice {
            vendor: Vendor::Amd,
            device_dir: dir,
            pci_address: "0000:01:00.0".into(),
            name: "AMD X".into(),
            runtime_pm,
        }
    }

    fn set(dev: &GpuDevice, file: &str, value: &str) {
        std::fs::write(dev.device_dir.join(file), format!("{value}\n")).unwrap();
    }

    #[test]
    fn suspended_device_is_not_queried() {
        let dev = fake_device("sleep", true);
        set(&dev, "power/runtime_status", "suspended");
        // A busy file that must NOT be read while suspended.
        set(&dev, "gpu_busy_percent", "99");
        let mut mon = GpuMonitor::new(vec![dev.clone()]);
        let s = &mon.sample(Instant::now())[0];
        assert_eq!(s.state, GpuState::Asleep);
        assert_eq!(s.usage, None);

        set(&dev, "power/runtime_status", "active");
        set(&dev, "mem_info_vram_total", "1000");
        let s = &mon.sample(Instant::now())[0];
        assert_eq!(s.state, GpuState::Active);
        assert_eq!(s.usage, Some(99.0));
        assert_eq!(s.vram_total, Some(1000));
        assert_eq!(s.vram_used, None, "missing files stay unavailable, not zero");
        std::fs::remove_dir_all(&dev.device_dir).unwrap();
    }

    #[test]
    fn idle_runtime_pm_gpu_is_left_alone_so_it_can_sleep() {
        let dev = fake_device("rest", true);
        set(&dev, "power/runtime_status", "active");
        set(&dev, "gpu_busy_percent", "0");
        let mut mon = GpuMonitor::new(vec![dev.clone()]);
        let t0 = Instant::now();
        assert_eq!(mon.sample(t0)[0].state, GpuState::Active, "measured once");
        // Now busy, but we are resting: not queried, metrics unavailable.
        set(&dev, "gpu_busy_percent", "80");
        let s = &mon.sample(t0 + Duration::from_secs(1))[0];
        assert_eq!(s.state, GpuState::Resting);
        assert_eq!(s.usage, None);
        // The driver powered it down meanwhile.
        set(&dev, "power/runtime_status", "suspended");
        assert_eq!(mon.sample(t0 + Duration::from_secs(20))[0].state, GpuState::Asleep);
        // Woken up again by someone else: measured right away.
        set(&dev, "power/runtime_status", "active");
        let s = &mon.sample(t0 + Duration::from_secs(21))[0];
        assert_eq!((s.state, s.usage), (GpuState::Active, Some(80.0)));
        // Idle again: rests, then is polled again once the rest period ends.
        set(&dev, "gpu_busy_percent", "0");
        mon.sample(t0 + Duration::from_secs(22));
        assert_eq!(mon.sample(t0 + Duration::from_secs(23))[0].state, GpuState::Resting);
        assert_eq!(mon.sample(t0 + Duration::from_secs(22) + REST)[0].state, GpuState::Active);
        std::fs::remove_dir_all(&dev.device_dir).unwrap();
    }

    #[test]
    fn always_on_gpu_is_polled_every_sample() {
        let dev = fake_device("on", false);
        set(&dev, "power/runtime_status", "active");
        set(&dev, "gpu_busy_percent", "0");
        let mut mon = GpuMonitor::new(vec![dev.clone()]);
        let t0 = Instant::now();
        for i in 0..5 {
            assert_eq!(mon.sample(t0 + Duration::from_secs(i))[0].state, GpuState::Active);
        }
        std::fs::remove_dir_all(&dev.device_dir).unwrap();
    }

    fn fake_amd_with_hwmon(tag: &str, apu: bool) -> GpuDevice {
        let dev = fake_device(tag, false);
        set(&dev, "power/runtime_status", "active");
        set(&dev, "gpu_busy_percent", "10");
        let hw = dev.device_dir.join("hwmon/hwmon3");
        std::fs::create_dir_all(&hw).unwrap();
        std::fs::write(hw.join("power1_average"), "54500000\n").unwrap();
        std::fs::write(hw.join("power1_label"), "PPT\n").unwrap();
        std::fs::write(hw.join("temp1_input"), "61000\n").unwrap();
        if apu {
            // Northbridge voltage: only APUs have it (kernel amdgpu docs).
            std::fs::write(hw.join("in1_input"), "0\n").unwrap();
            std::fs::write(hw.join("in1_label"), "vddnb\n").unwrap();
        }
        dev
    }

    #[test]
    fn apu_power_is_the_socs_and_never_the_gpus() {
        let dev = fake_amd_with_hwmon("apu", true);
        let s = &GpuMonitor::new(vec![dev.clone()]).sample(Instant::now())[0];
        assert_eq!(s.state, GpuState::Active);
        assert_eq!(s.power_w, None, "the APU figure includes the CPU: not the GPU's power");
        assert_eq!(s.soc_power_w, Some(54.5));
        assert_eq!(s.temperature_c, Some(61.0), "the edge temperature is the GPU's own");
        std::fs::remove_dir_all(&dev.device_dir).unwrap();
    }

    #[test]
    fn discrete_card_power_is_the_gpus() {
        let dev = fake_amd_with_hwmon("dgpu", false);
        let s = &GpuMonitor::new(vec![dev.clone()]).sample(Instant::now())[0];
        assert_eq!(s.power_w, Some(54.5));
        assert_eq!(s.soc_power_w, None);
        std::fs::remove_dir_all(&dev.device_dir).unwrap();
    }

    #[test]
    fn sleeping_gpu_reports_no_power_of_either_kind() {
        let dev = fake_amd_with_hwmon("apu-asleep", true);
        set(&dev, "power/runtime_status", "suspended");
        let s = &GpuMonitor::new(vec![dev.clone()]).sample(Instant::now())[0];
        assert_eq!((s.state, s.power_w, s.soc_power_w), (GpuState::Asleep, None, None));
        std::fs::remove_dir_all(&dev.device_dir).unwrap();
    }
}
