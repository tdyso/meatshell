//! Remote / local resource sampling for the "运行状态" side panel.
//!
//! The upstream desktop app parses a `/proc` snapshot streamed over a second
//! SSH channel (`src/ssh/impls/ssh.rs::parse_monitor_block`). This module keeps
//! the same wire format — `awk`-filtered `/proc/stat` + `/proc/meminfo`,
//! `/proc/net/dev`, then `df -kP` after a `__DF__` marker — but samples on
//! demand once per tick, so the OHOS port needs no persistent monitor channel.
//!
//! Only Linux targets produce data; on anything else `local_block` returns
//! `None` and the panel keeps its last values.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use meatshell_core::sftp_facade::format_size;
use slint::{ModelRc, SharedString, VecModel};

use crate::{AppWindow, DiskInfo};

/// Number of samples kept for the network sparklines (upstream uses 60).
pub const NET_HISTORY_LEN: usize = 60;

/// Sampling cadence for the local machine, mirroring the remote channel.
const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);

/// One parsed `/proc` snapshot.
#[derive(Clone, Default)]
pub struct StatsSample {
    pub cpu_percent: f32,
    pub mem_used_kib: u64,
    pub mem_total_kib: u64,
    pub swap_used_kib: u64,
    pub swap_total_kib: u64,
    /// `(interface, rx bytes/s, tx bytes/s)`, busiest first.
    pub net: Vec<(String, u64, u64)>,
    /// `(mount point, available bytes, total bytes)`.
    pub disks: Vec<(String, u64, u64)>,
}

/// Raw counters from the previous sample, used for the deltas.
pub struct PrevState {
    cpu: Option<(u64, u64)>,
    net: HashMap<String, (u64, u64)>,
    at: Instant,
}

impl Default for PrevState {
    fn default() -> Self {
        Self {
            cpu: None,
            net: HashMap::new(),
            at: Instant::now(),
        }
    }
}

fn parse_meminfo_kib(value: &str) -> u64 {
    value
        .split_whitespace()
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

fn parse_df_line(line: &str) -> Option<(String, u64, u64)> {
    // Filesystem 1024-blocks Used Available Capacity Mounted-on
    let mut parts = line.split_whitespace();
    let _fs = parts.next()?;
    let blocks: u64 = parts.next()?.parse().ok()?;
    let _used: u64 = parts.next()?.parse().ok()?;
    let avail: u64 = parts.next()?.parse().ok()?;
    let _cap = parts.next()?;
    let mount = parts.collect::<Vec<_>>().join(" ");
    if blocks == 0 || mount.is_empty() {
        return None;
    }
    Some((mount, avail.saturating_mul(1024), blocks.saturating_mul(1024)))
}

fn parse_net_dev_line(line: &str) -> Option<(String, u64, u64)> {
    let (iface, rest) = line.split_once(':')?;
    let iface = iface.trim();
    if iface.is_empty() || iface == "lo" || iface == "Inter-|" || iface == "face" {
        return None;
    }
    let nums: Vec<u64> = rest
        .split_whitespace()
        .filter_map(|n| n.parse().ok())
        .collect();
    if nums.len() < 9 {
        return None;
    }
    Some((iface.to_string(), nums[0], nums[8]))
}

/// Parse one snapshot in the upstream wire format.
pub fn parse_block(block: &str, prev: &mut PrevState) -> Option<StatsSample> {
    let mut cpu_total = 0u64;
    let mut cpu_idle = 0u64;
    let mut have_cpu = false;
    let mut mem_total = 0u64;
    let mut mem_avail = 0u64;
    let mut swap_total = 0u64;
    let mut swap_free = 0u64;
    let mut net_now: Vec<(String, u64, u64)> = Vec::new();
    let mut disks: Vec<(String, u64, u64)> = Vec::new();
    let mut seen_fs: std::collections::HashSet<(u64, u64)> = std::collections::HashSet::new();
    let mut in_df = false;

    for line in block.lines() {
        if line.trim() == "__DF__" {
            in_df = true;
            continue;
        }
        if in_df {
            if let Some(row) = parse_df_line(line) {
                // OHOS mounts every system module image read-only at 100 %;
                // they would flood the list with red rows and hide the disks
                // the user actually cares about.
                if row.0.starts_with("/module_update") {
                    continue;
                }
                if seen_fs.insert((row.2, row.1)) {
                    disks.push(row);
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("cpu ") {
            let nums: Vec<u64> = rest
                .split_whitespace()
                .filter_map(|n| n.parse().ok())
                .collect();
            if nums.len() >= 4 {
                cpu_total = nums.iter().copied().fold(0u64, u64::saturating_add);
                cpu_idle = nums[3].saturating_add(nums.get(4).copied().unwrap_or(0));
                have_cpu = true;
            }
        } else if let Some(v) = line.strip_prefix("MemTotal:") {
            mem_total = parse_meminfo_kib(v);
        } else if let Some(v) = line.strip_prefix("MemAvailable:") {
            mem_avail = parse_meminfo_kib(v);
        } else if let Some(v) = line.strip_prefix("SwapTotal:") {
            swap_total = parse_meminfo_kib(v);
        } else if let Some(v) = line.strip_prefix("SwapFree:") {
            swap_free = parse_meminfo_kib(v);
        } else if let Some(row) = parse_net_dev_line(line) {
            net_now.push(row);
        }
    }

    if mem_total == 0 {
        return None;
    }
    // Biggest volumes first, so the list starts with the disks worth watching.
    disks.sort_by(|a, b| b.2.cmp(&a.2));

    let now = Instant::now();
    let elapsed = now.duration_since(prev.at).as_secs_f64().max(0.001);
    let mut net: Vec<(String, u64, u64)> = Vec::new();
    for (iface, rx, tx) in &net_now {
        if let Some((prx, ptx)) = prev.net.get(iface) {
            net.push((
                iface.clone(),
                (rx.saturating_sub(*prx) as f64 / elapsed) as u64,
                (tx.saturating_sub(*ptx) as f64 / elapsed) as u64,
            ));
        }
    }
    prev.net = net_now.into_iter().map(|(i, rx, tx)| (i, (rx, tx))).collect();
    prev.at = now;
    net.sort_by(|a, b| (b.1 + b.2).cmp(&(a.1 + a.2)));

    let cpu_percent = if have_cpu {
        let result = match prev.cpu {
            Some((ptotal, pidle)) => {
                let dt = cpu_total.saturating_sub(ptotal);
                let di = cpu_idle.saturating_sub(pidle);
                if dt > 0 {
                    (1.0 - di as f32 / dt as f32).clamp(0.0, 1.0)
                } else {
                    0.0
                }
            }
            None => 0.0,
        };
        prev.cpu = Some((cpu_total, cpu_idle));
        result
    } else {
        0.0
    };

    Some(StatsSample {
        cpu_percent,
        mem_used_kib: mem_total.saturating_sub(mem_avail),
        mem_total_kib: mem_total,
        swap_used_kib: swap_total.saturating_sub(swap_free),
        swap_total_kib: swap_total,
        net,
        disks,
    })
}

fn read_proc(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn df_output() -> String {
    // The HAP process starts with an empty PATH, so a bare `df` is not found.
    // OHOS keeps toybox in /system/bin and symlinks it as /bin/df.
    for cmd in ["/bin/df", "/system/bin/df", "/usr/bin/df", "df"] {
        let Ok(out) = std::process::Command::new(cmd)
            .arg("-kP")
            .env("PATH", "/bin:/system/bin:/usr/bin:/usr/sbin:/sbin")
            .output()
        else {
            continue;
        };
        if !out.stdout.is_empty() {
            return String::from_utf8_lossy(&out.stdout).into_owned();
        }
    }
    String::new()
}

/// Snapshot of *this* device in the same format as the remote channel.
pub fn local_block() -> Option<String> {
    let stat = read_proc("/proc/stat")?;
    let mem = read_proc("/proc/meminfo").unwrap_or_default();
    let net = read_proc("/proc/net/dev").unwrap_or_default();
    let df = df_output();
    let mut block = String::with_capacity(stat.len() + mem.len() + net.len() + df.len() + 16);
    for line in stat.lines().filter(|l| l.starts_with("cpu ")) {
        block.push_str(line);
        block.push('\n');
    }
    for line in mem.lines().filter(|l| {
        l.starts_with("MemTotal:")
            || l.starts_with("MemAvailable:")
            || l.starts_with("SwapTotal:")
            || l.starts_with("SwapFree:")
    }) {
        block.push_str(line);
        block.push('\n');
    }
    block.push_str(&net);
    block.push_str("\n__DF__\n");
    block.push_str(&df);
    Some(block)
}

/// `used/total` in MiB, rendered like upstream's narrow-sidebar format.
fn format_mem(used_mib: u64, total_mib: u64) -> String {
    if total_mib < 1024 {
        return format!("{used_mib}/{total_mib}M");
    }
    fn gib(mib: u64) -> String {
        let g = mib as f64 / 1024.0;
        if g.fract() == 0.0 || g >= 100.0 {
            (g as u64).to_string()
        } else {
            format!("{g:.1}")
        }
    }
    format!("{}G/{}G", gib(used_mib), gib(total_mib))
}

/// Human-readable throughput (`"1.2 MB/s"`).
pub fn format_rate(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B/s", "KB/s", "MB/s", "GB/s"];
    let mut value = bytes as f64;
    let mut idx = 0;
    while value >= 1024.0 && idx < UNITS.len() - 1 {
        value /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{} {}", bytes, UNITS[idx])
    } else {
        format!("{:.1} {}", value, UNITS[idx])
    }
}

fn disk_model(disks: &[(String, u64, u64)]) -> ModelRc<DiskInfo> {
    let rows: Vec<DiskInfo> = disks
        .iter()
        .map(|(mount, avail, total)| {
            let used = total.saturating_sub(*avail);
            let percent = if *total > 0 {
                used as f32 / *total as f32
            } else {
                0.0
            };
            DiskInfo {
                path: mount.clone().into(),
                detail: format!("{}/{}", format_size(*avail), format_size(*total)).into(),
                percent,
            }
        })
        .collect();
    ModelRc::from(std::rc::Rc::new(VecModel::from(rows)))
}

fn normalized_model(hist: &[f32]) -> ModelRc<f32> {
    let max = hist.iter().cloned().fold(1.0_f32, f32::max);
    let scaled: Vec<f32> = hist.iter().map(|v| (v / max).clamp(0.0, 1.0)).collect();
    ModelRc::from(std::rc::Rc::new(VecModel::from(scaled)))
}

fn push_ring(hist: &mut Vec<f32>, value: f32) {
    if hist.len() != NET_HISTORY_LEN {
        *hist = vec![0.0; NET_HISTORY_LEN];
    }
    hist.remove(0);
    hist.push(value);
}

fn iface_model(net: &[(String, u64, u64)]) -> ModelRc<SharedString> {
    let rows: Vec<SharedString> = net.iter().map(|(name, ..)| name.as_str().into()).collect();
    ModelRc::from(std::rc::Rc::new(VecModel::from(rows)))
}

/// Side-panel state shared by the local sampler and the active remote tab.
pub struct ResourceState {
    prev: PrevState,
    local: Option<StatsSample>,
    last_local: Option<Instant>,
    /// Sparkline history for the local machine (bottom graph).
    local_hist: Vec<f32>,
    /// Sparkline history for whatever the top graph currently shows.
    top_hist: Vec<f32>,
    /// Tab whose remote sample is currently displayed ("" = local).
    active: String,
}

impl Default for ResourceState {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceState {
    pub fn new() -> Self {
        Self {
            prev: PrevState::default(),
            local: None,
            last_local: None,
            local_hist: vec![0.0; NET_HISTORY_LEN],
            top_hist: vec![0.0; NET_HISTORY_LEN],
            active: String::new(),
        }
    }

    /// Sample this device at most every [`SAMPLE_INTERVAL`]; always refreshes
    /// the bottom (local) network graph. Returns `true` when a new sample was
    /// taken so the caller can decide whether to also fill the top graph.
    pub fn tick_local(&mut self, ui: &AppWindow) -> bool {
        let due = self
            .last_local
            .map(|t| t.elapsed() >= SAMPLE_INTERVAL)
            .unwrap_or(true);
        if !due {
            return false;
        }
        self.last_local = Some(Instant::now());
        let Some(block) = local_block() else {
            return false;
        };
        let Some(sample) = parse_block(&block, &mut self.prev) else {
            return false;
        };
        let rx: u64 = sample.net.iter().map(|(_, rx, _)| *rx).sum();
        let tx: u64 = sample.net.iter().map(|(_, _, tx)| *tx).sum();
        push_ring(&mut self.local_hist, (rx + tx) as f32);
        ui.set_net_bot_up(format_rate(tx).into());
        ui.set_net_bot_down(format_rate(rx).into());
        ui.set_net_bot_history(normalized_model(&self.local_hist));
        self.local = Some(sample);
        true
    }

    /// Fill the panel with this device's numbers (welcome tab / not connected).
    pub fn show_local(&mut self, ui: &AppWindow) {
        let Some(sample) = self.local.clone() else {
            ui.set_cpu_percent(0.0);
            ui.set_mem_percent(0.0);
            ui.set_swap_percent(0.0);
            ui.set_mem_detail("".into());
            ui.set_swap_detail("".into());
            ui.set_disks(ModelRc::from(std::rc::Rc::new(VecModel::<DiskInfo>::default())));
            return;
        };
        self.active.clear();
        let pct = |used: u64, total: u64| if total > 0 { used as f32 / total as f32 } else { 0.0 };
        ui.set_resource_title("本机资源".into());
        ui.set_connection_state("未连接".into());
        ui.set_conn_host("".into());
        ui.set_conn_state(0);
        ui.set_cpu_percent(sample.cpu_percent);
        ui.set_mem_percent(pct(sample.mem_used_kib, sample.mem_total_kib));
        ui.set_swap_percent(pct(sample.swap_used_kib, sample.swap_total_kib));
        ui.set_mem_detail(format_mem(sample.mem_used_kib / 1024, sample.mem_total_kib / 1024).into());
        ui.set_swap_detail(format_mem(sample.swap_used_kib / 1024, sample.swap_total_kib / 1024).into());
        let rx: u64 = sample.net.iter().map(|(_, rx, _)| *rx).sum();
        let tx: u64 = sample.net.iter().map(|(_, _, tx)| *tx).sum();
        ui.set_net_top_up(format_rate(tx).into());
        ui.set_net_top_down(format_rate(rx).into());
        ui.set_net_top_history(normalized_model(&self.local_hist));
        ui.set_net_show_selector(false);
        ui.set_net_selected("".into());
        ui.set_net_ifaces(ModelRc::from(std::rc::Rc::new(VecModel::<SharedString>::default())));
        ui.set_disks(disk_model(&sample.disks));
    }

    /// Fill the panel from the active tab's remote sample. `preferred` is the
    /// NIC the user picked in the selector ("" = busiest NIC, the first row).
    pub fn show_remote(
        &mut self,
        ui: &AppWindow,
        sample: &StatsSample,
        host: &str,
        tab_id: &str,
        preferred: &str,
    ) {
        self.active = tab_id.to_string();
        let pct = |used: u64, total: u64| if total > 0 { used as f32 / total as f32 } else { 0.0 };
        ui.set_resource_title("服务器资源".into());
        ui.set_connection_state(host.into());
        ui.set_conn_host(host.into());
        ui.set_conn_state(1);
        ui.set_cpu_percent(sample.cpu_percent);
        ui.set_mem_percent(pct(sample.mem_used_kib, sample.mem_total_kib));
        ui.set_swap_percent(pct(sample.swap_used_kib, sample.swap_total_kib));
        ui.set_mem_detail(format_mem(sample.mem_used_kib / 1024, sample.mem_total_kib / 1024).into());
        ui.set_swap_detail(format_mem(sample.swap_used_kib / 1024, sample.swap_total_kib / 1024).into());

        let rx: u64 = sample.net.iter().map(|(_, rx, _)| *rx).sum();
        let tx: u64 = sample.net.iter().map(|(_, _, tx)| *tx).sum();
        push_ring(&mut self.top_hist, (rx + tx) as f32);
        // Top graph follows the selected NIC, or the busiest one (the list is
        // already sorted) when nothing was picked.
        let chosen = sample
            .net
            .iter()
            .find(|(name, ..)| name.as_str() == preferred)
            .or_else(|| sample.net.first());
        let (up, down) = chosen
            .map(|(_, rx, tx)| (*tx, *rx))
            .unwrap_or((tx, rx));
        ui.set_net_top_up(format_rate(up).into());
        ui.set_net_top_down(format_rate(down).into());
        ui.set_net_top_history(normalized_model(&self.top_hist));
        ui.set_net_show_selector(!sample.net.is_empty());
        ui.set_net_selected(chosen.map(|(n, ..)| n.as_str()).unwrap_or("").into());
        ui.set_net_ifaces(iface_model(&sample.net));
        ui.set_disks(disk_model(&sample.disks));
    }

    /// The tab id whose remote sample is on screen ("" when showing local).
    pub fn active(&self) -> &str {
        &self.active
    }
}

/// Unit-test friendly re-exports for the parsers.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_mem_net_and_df() {
        let block = "cpu  100 0 100 800 0 0 0 0\n\
MemTotal:       2048000 kB\n\
MemAvailable:    512000 kB\n\
SwapTotal:       1024000 kB\n\
SwapFree:         512000 kB\n\
  eth0: 1000 0 0 0 0 0 0 0 500 0 0 0 0 0 0 0\n\
__DF__\n\
/dev/sda1 100000 40000 60000 40% /\n";
        let mut prev = PrevState::default();
        let sample = parse_block(block, &mut prev).expect("sample");
        assert_eq!(sample.mem_total_kib, 2_048_000);
        assert_eq!(sample.mem_used_kib, 1_536_000);
        assert_eq!(sample.swap_used_kib, 512_000);
        assert_eq!(sample.net.len(), 1);
        assert_eq!(sample.net[0].0, "eth0");
        assert_eq!(sample.disks.len(), 1);
        assert_eq!(sample.disks[0].1, 60_000 * 1024);
    }

    #[test]
    fn formats_compact_memory() {
        assert_eq!(format_mem(512, 2048), "0.5G/2G");
        assert_eq!(format_mem(300, 800), "300/800M");
    }
}
