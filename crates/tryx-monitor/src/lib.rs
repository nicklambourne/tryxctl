//! Host metrics for the display overlay.
//!
//! Linux only: `/proc`, `/sys/class/hwmon`, `/sys/class/drm`, cpufreq, and
//! `nvidia-smi` when present. Every value is optional; what the host cannot
//! measure stays `None` and the overlay shows nothing for it. Sources follow
//! DXVSI/Tryx-Linux-GUI `src/systemmonitor.cpp` (AMD hwmon and amdgpu sysfs)
//! plus `nvidia-smi` for NVIDIA cards.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CpuSample {
    pub name: Option<String>,
    pub temperature_c: Option<f64>,
    pub usage_percent: Option<f64>,
    pub frequency_mhz: Option<f64>,
    pub power_w: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct GpuSample {
    pub name: Option<String>,
    pub temperature_c: Option<f64>,
    pub usage_percent: Option<f64>,
    pub frequency_mhz: Option<f64>,
    pub power_w: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MemorySample {
    pub usage_percent: Option<f64>,
    pub total_bytes: Option<u64>,
    pub used_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DiskSample {
    pub temperature_c: Option<f64>,
    pub usage_percent: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Sample {
    pub cpu: CpuSample,
    pub gpu: GpuSample,
    pub memory: MemorySample,
    pub disk: DiskSample,
    /// Milliseconds since the Unix epoch.
    pub timestamp_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct CpuTimes {
    idle: u64,
    total: u64,
}

/// Reads samples; keep one instance so rates can be computed between calls.
pub struct Monitor {
    proc_root: PathBuf,
    sys_root: PathBuf,
    nvidia_smi: Option<PathBuf>,
    previous_cpu: Option<CpuTimes>,
    previous_energy: Option<(u64, Instant)>,
}

impl Default for Monitor {
    fn default() -> Self {
        Self::new()
    }
}

impl Monitor {
    pub fn new() -> Self {
        Monitor::with_roots(
            Path::new("/proc"),
            Path::new("/sys"),
            which::which("nvidia-smi").ok(),
        )
    }

    /// For tests: read from fake `/proc` and `/sys` trees.
    pub fn with_roots(proc_root: &Path, sys_root: &Path, nvidia_smi: Option<PathBuf>) -> Self {
        Monitor {
            proc_root: proc_root.to_path_buf(),
            sys_root: sys_root.to_path_buf(),
            nvidia_smi,
            previous_cpu: None,
            previous_energy: None,
        }
    }

    /// Whether this host can be measured at all.
    pub fn supported() -> bool {
        cfg!(target_os = "linux")
    }

    /// Takes a sample. CPU usage and RAPL power need a previous sample, so
    /// the first call leaves them `None`.
    pub fn sample(&mut self) -> Sample {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Sample {
            cpu: self.cpu(),
            gpu: self.gpu(),
            memory: self.memory(),
            disk: self.disk(),
            timestamp_ms,
        }
    }

    fn cpu(&mut self) -> CpuSample {
        let usage_percent = self.cpu_usage();
        let hwmon = self.hwmon_devices();
        let cpu_hwmon = hwmon
            .iter()
            .find(|dev| matches!(dev.name.as_str(), "k10temp" | "zenpower" | "coretemp"));
        let temperature_c = cpu_hwmon.and_then(|dev| {
            // k10temp: Tctl/Tdie; coretemp: the package sensor; else temp1.
            let preferred = ["Tdie", "Tctl", "Package id 0"];
            preferred
                .iter()
                .find_map(|label| dev.temp_by_label(label))
                .or_else(|| dev.milli("temp1_input").map(|v| v / 1000.0))
        });
        let power_w = cpu_hwmon
            .and_then(|dev| dev.milli("power1_average").map(|v| v / 1_000_000.0))
            .or_else(|| self.rapl_power());
        CpuSample {
            name: self.cpu_name(),
            temperature_c,
            usage_percent,
            frequency_mhz: self.cpu_frequency_mhz(),
            power_w,
        }
    }

    fn cpu_name(&self) -> Option<String> {
        let text = read(&self.proc_root.join("cpuinfo"))?;
        text.lines()
            .find(|line| line.starts_with("model name"))
            .and_then(|line| line.split_once(':'))
            .map(|(_, name)| collapse_spaces(name.trim()))
    }

    fn cpu_usage(&mut self) -> Option<f64> {
        let text = read(&self.proc_root.join("stat"))?;
        let line = text.lines().find(|line| line.starts_with("cpu "))?;
        let fields: Vec<u64> = line
            .split_whitespace()
            .skip(1)
            .filter_map(|f| f.parse().ok())
            .collect();
        if fields.len() < 5 {
            return None;
        }
        let idle = fields[3] + fields[4];
        let total: u64 = fields.iter().sum();
        let current = CpuTimes { idle, total };
        let previous = self.previous_cpu.replace(current)?;
        let total_delta = current.total.checked_sub(previous.total)?;
        let idle_delta = current.idle.checked_sub(previous.idle)?;
        (total_delta > 0).then(|| (1.0 - idle_delta as f64 / total_delta as f64) * 100.0)
    }

    fn cpu_frequency_mhz(&self) -> Option<f64> {
        let cpus = std::fs::read_dir(self.sys_root.join("devices/system/cpu")).ok()?;
        let mut frequencies = Vec::new();
        for entry in cpus.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("cpu") || !name[3..].chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            if let Some(khz) = read_f64(&entry.path().join("cpufreq/scaling_cur_freq")) {
                frequencies.push(khz / 1000.0);
            }
        }
        (!frequencies.is_empty())
            .then(|| frequencies.iter().sum::<f64>() / frequencies.len() as f64)
    }

    /// Package power from the RAPL energy counter, when it is readable.
    fn rapl_power(&mut self) -> Option<f64> {
        let entries = std::fs::read_dir(self.sys_root.join("class/powercap")).ok()?;
        let energy_path = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                name.contains("rapl") && name.matches(':').count() == 1
            })
            .map(|path| path.join("energy_uj"))
            .find(|path| path.is_file())?;
        let energy_uj = read_u64(&energy_path)?;
        let now = Instant::now();
        let previous = self.previous_energy.replace((energy_uj, now))?;
        let elapsed = now.duration_since(previous.1).as_secs_f64();
        let delta = energy_uj.checked_sub(previous.0)?;
        (elapsed > 0.0).then(|| delta as f64 / 1e6 / elapsed)
    }

    fn gpu(&self) -> GpuSample {
        if let Some(nvidia_smi) = &self.nvidia_smi
            && let Some(sample) = nvidia_gpu(nvidia_smi)
        {
            return sample;
        }
        self.amd_gpu().unwrap_or_default()
    }

    fn amd_gpu(&self) -> Option<GpuSample> {
        let cards = std::fs::read_dir(self.sys_root.join("class/drm")).ok()?;
        let mut names: Vec<PathBuf> = cards
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                name.starts_with("card")
                    && !name.contains('-')
                    && name[4..].chars().all(|c| c.is_ascii_digit())
            })
            .collect();
        names.sort();
        for card in names {
            let device = card.join("device");
            let driver = std::fs::read_link(device.join("driver"))
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
            let is_amd =
                driver.as_deref() == Some("amdgpu") || device.join("gpu_busy_percent").is_file();
            if !is_amd {
                continue;
            }
            let hwmon = hwmon_under(&device.join("hwmon"));
            let frequency_mhz = read(&device.join("pp_dpm_sclk")).and_then(|text| {
                text.lines()
                    .find(|line| line.trim_end().ends_with('*'))
                    .and_then(|line| line.split_whitespace().nth(1))
                    .and_then(|mhz| {
                        mhz.trim_end_matches("Mhz")
                            .trim_end_matches("MHz")
                            .parse()
                            .ok()
                    })
            });
            return Some(GpuSample {
                name: Some(self.amd_gpu_name(&device)),
                temperature_c: hwmon
                    .as_ref()
                    .and_then(|dev| dev.milli("temp1_input"))
                    .map(|v| v / 1000.0),
                usage_percent: read_f64(&device.join("gpu_busy_percent")),
                frequency_mhz,
                power_w: hwmon
                    .as_ref()
                    .and_then(|dev| dev.milli("power1_average"))
                    .map(|v| v / 1_000_000.0),
            });
        }
        None
    }

    fn amd_gpu_name(&self, device: &Path) -> String {
        let id = read(&device.join("device"))
            .map(|s| s.trim().trim_start_matches("0x").to_ascii_lowercase());
        let revision = read(&device.join("revision"))
            .map(|s| s.trim().trim_start_matches("0x").to_ascii_lowercase());
        if let (Some(id), Some(revision)) = (id, revision)
            && let Some(ids) = read(Path::new("/usr/share/libdrm/amdgpu.ids"))
        {
            for line in ids.lines().filter(|line| !line.starts_with('#')) {
                let mut parts = line.split(',').map(str::trim);
                if let (Some(line_id), Some(line_revision), Some(name)) =
                    (parts.next(), parts.next(), parts.next())
                    && line_id.eq_ignore_ascii_case(&id)
                    && line_revision.eq_ignore_ascii_case(&revision)
                {
                    return name.to_string();
                }
            }
        }
        "AMD Radeon".to_string()
    }

    fn memory(&self) -> MemorySample {
        let Some(text) = read(&self.proc_root.join("meminfo")) else {
            return MemorySample::default();
        };
        let field = |key: &str| -> Option<u64> {
            text.lines()
                .find(|line| line.starts_with(key))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<u64>().ok())
                .map(|kb| kb * 1024)
        };
        let total = field("MemTotal:");
        let available = field("MemAvailable:");
        let used = total
            .zip(available)
            .map(|(total, available)| total.saturating_sub(available));
        MemorySample {
            usage_percent: total
                .zip(used)
                .filter(|(t, _)| *t > 0)
                .map(|(t, u)| u as f64 / t as f64 * 100.0),
            total_bytes: total,
            used_bytes: used,
        }
    }

    fn disk(&self) -> DiskSample {
        let temperature_c = self
            .hwmon_devices()
            .iter()
            .find(|dev| dev.name == "nvme")
            .and_then(|dev| dev.milli("temp1_input"))
            .map(|v| v / 1000.0);
        DiskSample {
            temperature_c,
            usage_percent: root_usage_percent(),
        }
    }

    fn hwmon_devices(&self) -> Vec<Hwmon> {
        let Ok(entries) = std::fs::read_dir(self.sys_root.join("class/hwmon")) else {
            return Vec::new();
        };
        let mut devices: Vec<Hwmon> = entries
            .flatten()
            .filter_map(|entry| Hwmon::read(&entry.path()))
            .collect();
        devices.sort_by(|a, b| a.path.cmp(&b.path));
        devices
    }
}

struct Hwmon {
    path: PathBuf,
    name: String,
}

impl Hwmon {
    fn read(path: &Path) -> Option<Hwmon> {
        let name = read(&path.join("name"))?.trim().to_string();
        Some(Hwmon {
            path: path.to_path_buf(),
            name,
        })
    }

    fn milli(&self, attribute: &str) -> Option<f64> {
        read_f64(&self.path.join(attribute))
    }

    /// The `tempN_input` whose `tempN_label` equals `label`, in °C.
    fn temp_by_label(&self, label: &str) -> Option<f64> {
        let entries = std::fs::read_dir(&self.path).ok()?;
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if let Some(index) = file
                .strip_prefix("temp")
                .and_then(|rest| rest.strip_suffix("_label"))
                && read(&entry.path()).is_some_and(|text| text.trim() == label)
            {
                return read_f64(&self.path.join(format!("temp{index}_input"))).map(|v| v / 1000.0);
            }
        }
        None
    }
}

fn hwmon_under(dir: &Path) -> Option<Hwmon> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find_map(|entry| Hwmon::read(&entry.path()))
}

/// Parses one line of
/// `nvidia-smi --query-gpu=name,temperature.gpu,utilization.gpu,clocks.sm,power.draw --format=csv,noheader,nounits`.
pub fn parse_nvidia_smi(line: &str) -> Option<GpuSample> {
    let fields: Vec<&str> = line.split(',').map(str::trim).collect();
    if fields.len() < 5 {
        return None;
    }
    let number = |text: &str| text.parse::<f64>().ok();
    Some(GpuSample {
        name: Some(fields[0].to_string()),
        temperature_c: number(fields[1]),
        usage_percent: number(fields[2]),
        frequency_mhz: number(fields[3]),
        power_w: number(fields[4]),
    })
}

fn nvidia_gpu(nvidia_smi: &Path) -> Option<GpuSample> {
    let output = std::process::Command::new(nvidia_smi)
        .args([
            "--query-gpu=name,temperature.gpu,utilization.gpu,clocks.sm,power.draw",
            "--format=csv,noheader,nounits",
            "--id=0",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_nvidia_smi(text.lines().next()?)
}

#[cfg(target_os = "linux")]
fn root_usage_percent() -> Option<f64> {
    let stats = nix::sys::statvfs::statvfs("/").ok()?;
    let total = stats.blocks() as f64 * stats.fragment_size() as f64;
    let free = stats.blocks_available() as f64 * stats.fragment_size() as f64;
    (total > 0.0).then(|| (1.0 - free / total) * 100.0)
}

#[cfg(not(target_os = "linux"))]
fn root_usage_percent() -> Option<f64> {
    None
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn read_f64(path: &Path) -> Option<f64> {
    read(path)?.trim().parse().ok()
}

fn read_u64(path: &Path) -> Option<u64> {
    read(path)?.trim().parse().ok()
}

fn collapse_spaces(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_roots() -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "tryx-monitor-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        let proc_root = base.join("proc");
        let sys_root = base.join("sys");
        std::fs::create_dir_all(&proc_root).unwrap();
        std::fs::create_dir_all(&sys_root).unwrap();
        (base, proc_root, sys_root)
    }

    fn rand_suffix() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn reads_cpu_memory_and_hwmon_from_fake_trees() {
        let (base, proc_root, sys_root) = fake_roots();
        write(
            &proc_root.join("cpuinfo"),
            "processor\t: 0\nmodel name\t: AMD Ryzen 9   9950X3D 16-Core Processor\n",
        );
        write(
            &proc_root.join("stat"),
            "cpu  100 0 100 800 0 0 0 0 0 0\ncpu0 1 2 3 4 5 6 7 8 9 10\n",
        );
        write(
            &proc_root.join("meminfo"),
            "MemTotal:       65536000 kB\nMemFree:        1000 kB\nMemAvailable:   32768000 kB\n",
        );
        write(
            &sys_root.join("devices/system/cpu/cpu0/cpufreq/scaling_cur_freq"),
            "4000000\n",
        );
        write(
            &sys_root.join("devices/system/cpu/cpu1/cpufreq/scaling_cur_freq"),
            "5000000\n",
        );
        write(&sys_root.join("devices/system/cpu/cpufreq/boost"), "1\n");
        write(&sys_root.join("class/hwmon/hwmon3/name"), "k10temp\n");
        write(&sys_root.join("class/hwmon/hwmon3/temp1_label"), "Tctl\n");
        write(&sys_root.join("class/hwmon/hwmon3/temp1_input"), "61250\n");
        write(&sys_root.join("class/hwmon/hwmon3/temp2_label"), "Tdie\n");
        write(&sys_root.join("class/hwmon/hwmon3/temp2_input"), "58000\n");
        write(&sys_root.join("class/hwmon/hwmon5/name"), "nvme\n");
        write(&sys_root.join("class/hwmon/hwmon5/temp1_input"), "41000\n");

        let mut monitor = Monitor::with_roots(&proc_root, &sys_root, None);
        let first = monitor.sample();
        assert_eq!(
            first.cpu.name.as_deref(),
            Some("AMD Ryzen 9 9950X3D 16-Core Processor")
        );
        assert_eq!(first.cpu.usage_percent, None, "needs two samples");
        assert_eq!(
            first.cpu.temperature_c,
            Some(58.0),
            "Tdie preferred over Tctl"
        );
        assert_eq!(first.cpu.frequency_mhz, Some(4500.0));
        assert_eq!(first.memory.total_bytes, Some(65_536_000 * 1024));
        assert!((first.memory.usage_percent.unwrap() - 50.0).abs() < 0.01);
        assert_eq!(first.disk.temperature_c, Some(41.0));
        assert_eq!(first.gpu, GpuSample::default());

        write(&proc_root.join("stat"), "cpu  300 0 200 1000 0 0 0 0 0 0\n");
        let second = monitor.sample();
        // busy delta 300 of total delta 500.
        assert!((second.cpu.usage_percent.unwrap() - 60.0).abs() < 0.01);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn reads_an_amd_gpu_from_drm_sysfs() {
        let (base, proc_root, sys_root) = fake_roots();
        let device = sys_root.join("class/drm/card1/device");
        write(&device.join("gpu_busy_percent"), "37\n");
        write(
            &device.join("pp_dpm_sclk"),
            "0: 500Mhz\n1: 1500Mhz *\n2: 2100Mhz\n",
        );
        write(&device.join("hwmon/hwmon7/name"), "amdgpu\n");
        write(&device.join("hwmon/hwmon7/temp1_input"), "52000\n");
        write(&device.join("hwmon/hwmon7/power1_average"), "123456789\n");
        write(&sys_root.join("class/drm/card1-DP-1/status"), "connected\n");
        let monitor = Monitor::with_roots(&proc_root, &sys_root, None);
        let gpu = monitor.gpu();
        assert_eq!(gpu.usage_percent, Some(37.0));
        assert_eq!(gpu.frequency_mhz, Some(1500.0));
        assert_eq!(gpu.temperature_c, Some(52.0));
        assert!((gpu.power_w.unwrap() - 123.456789).abs() < 1e-6);
        assert_eq!(gpu.name.as_deref(), Some("AMD Radeon"));
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn parses_nvidia_smi_csv() {
        let gpu = parse_nvidia_smi("NVIDIA GeForce RTX 5090, 41, 3, 210, 28.42").unwrap();
        assert_eq!(gpu.name.as_deref(), Some("NVIDIA GeForce RTX 5090"));
        assert_eq!(gpu.temperature_c, Some(41.0));
        assert_eq!(gpu.usage_percent, Some(3.0));
        assert_eq!(gpu.frequency_mhz, Some(210.0));
        assert_eq!(gpu.power_w, Some(28.42));
        let partial = parse_nvidia_smi("Tesla, 40, [N/A], 100, [N/A]").unwrap();
        assert_eq!(partial.usage_percent, None);
        assert!(parse_nvidia_smi("garbage").is_none());
    }
}
