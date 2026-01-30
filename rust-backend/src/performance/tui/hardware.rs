//! Hardware Monitoring
//!
//! System-level monitoring for HFT infrastructure:
//! - CPU affinity and frequency scaling
//! - Memory bandwidth and NUMA topology
//! - NIC statistics and hardware timestamping
//! - FPGA integration points

use parking_lot::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use sysinfo::{Networks, System};

/// Hardware monitoring subsystem
pub struct HardwareMonitor {
    system: RwLock<System>,

    // CPU metrics
    pub cpu_usage: Vec<AtomicU64>, // Per-core usage (scaled 0-10000 for 0.00-100.00%)
    pub cpu_freq_mhz: Vec<AtomicU64>,
    pub cpu_temp_c: AtomicU64, // Scaled by 100 (e.g., 6500 = 65.00°C)

    // Memory
    pub mem_total_kb: AtomicU64,
    pub mem_used_kb: AtomicU64,
    pub mem_available_kb: AtomicU64,
    pub swap_total_kb: AtomicU64,
    pub swap_used_kb: AtomicU64,

    // Disk I/O
    pub disk_read_bytes: AtomicU64,
    pub disk_write_bytes: AtomicU64,

    // Network (per-interface snapshots)
    pub net_interfaces: RwLock<Vec<NetInterfaceStats>>,

    // CPU affinity (which cores are pinned)
    pub pinned_cores: RwLock<Vec<usize>>,

    // FPGA status
    pub fpga_detected: AtomicU64,
    pub fpga_temp_c: AtomicU64,
    pub fpga_power_mw: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct NetInterfaceStats {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_errors: u64,
    pub tx_errors: u64,
    pub rx_dropped: u64,
    pub tx_dropped: u64,
    // Hardware timestamping support
    pub hw_timestamp_capable: bool,
    // Kernel bypass status
    pub dpdk_bound: bool,
    pub io_uring_enabled: bool,
}

impl HardwareMonitor {
    pub fn new() -> Arc<Self> {
        let mut system = System::new_all();
        system.refresh_all();

        let cpu_count = system.cpus().len();

        let monitor = Arc::new(Self {
            system: RwLock::new(system),
            cpu_usage: (0..cpu_count).map(|_| AtomicU64::new(0)).collect(),
            cpu_freq_mhz: (0..cpu_count).map(|_| AtomicU64::new(0)).collect(),
            cpu_temp_c: AtomicU64::new(0),
            mem_total_kb: AtomicU64::new(0),
            mem_used_kb: AtomicU64::new(0),
            mem_available_kb: AtomicU64::new(0),
            swap_total_kb: AtomicU64::new(0),
            swap_used_kb: AtomicU64::new(0),
            disk_read_bytes: AtomicU64::new(0),
            disk_write_bytes: AtomicU64::new(0),
            net_interfaces: RwLock::new(Vec::new()),
            pinned_cores: RwLock::new(Vec::new()),
            fpga_detected: AtomicU64::new(0),
            fpga_temp_c: AtomicU64::new(0),
            fpga_power_mw: AtomicU64::new(0),
        });

        monitor.refresh();
        monitor
    }

    /// Refresh all hardware metrics
    pub fn refresh(&self) {
        let mut system = self.system.write();
        system.refresh_all();

        // CPU metrics
        for (i, cpu) in system.cpus().iter().enumerate() {
            if i < self.cpu_usage.len() {
                self.cpu_usage[i].store((cpu.cpu_usage() * 100.0) as u64, Ordering::Relaxed);
                self.cpu_freq_mhz[i].store(cpu.frequency(), Ordering::Relaxed);
            }
        }

        // Memory
        self.mem_total_kb
            .store(system.total_memory() / 1024, Ordering::Relaxed);
        self.mem_used_kb
            .store(system.used_memory() / 1024, Ordering::Relaxed);
        self.mem_available_kb
            .store(system.available_memory() / 1024, Ordering::Relaxed);
        self.swap_total_kb
            .store(system.total_swap() / 1024, Ordering::Relaxed);
        self.swap_used_kb
            .store(system.used_swap() / 1024, Ordering::Relaxed);

        // Network interfaces
        let networks = Networks::new_with_refreshed_list();
        let mut net_stats = Vec::new();
        for (name, data) in &networks {
            net_stats.push(NetInterfaceStats {
                name: name.clone(),
                rx_bytes: data.total_received(),
                tx_bytes: data.total_transmitted(),
                rx_packets: data.total_packets_received(),
                tx_packets: data.total_packets_transmitted(),
                rx_errors: data.total_errors_on_received(),
                tx_errors: data.total_errors_on_transmitted(),
                rx_dropped: 0, // sysinfo doesn't expose this
                tx_dropped: 0,
                hw_timestamp_capable: Self::check_hw_timestamp(name),
                dpdk_bound: Self::check_dpdk_bound(name),
                io_uring_enabled: Self::check_io_uring(),
            });
        }
        *self.net_interfaces.write() = net_stats;

        // Check for FPGA (placeholder - would integrate with vendor SDK)
        self.detect_fpga();
    }

    /// Check if interface supports hardware timestamping
    fn check_hw_timestamp(interface: &str) -> bool {
        // On Linux, would check /sys/class/net/{interface}/device/
        // For now, return false as placeholder
        #[cfg(target_os = "linux")]
        {
            let path = format!("/sys/class/net/{}/device/driver", interface);
            if let Ok(driver) = std::fs::read_to_string(&path) {
                // Known HW timestamp capable drivers
                return driver.contains("mlx5")
                    || driver.contains("ixgbe")
                    || driver.contains("i40e")
                    || driver.contains("ice");
            }
        }
        let _ = interface; // silence unused warning on non-linux
        false
    }

    /// Check if interface is bound to DPDK
    fn check_dpdk_bound(interface: &str) -> bool {
        #[cfg(target_os = "linux")]
        {
            // Would check /sys/class/net/{interface} doesn't exist
            // but hugepages are allocated
            let net_path = format!("/sys/class/net/{}", interface);
            if !std::path::Path::new(&net_path).exists() {
                // Could be DPDK bound if hugepages are configured
                return std::path::Path::new("/dev/hugepages").exists();
            }
        }
        let _ = interface; // silence unused warning on non-linux
        false
    }

    /// Check if io_uring is available
    fn check_io_uring() -> bool {
        #[cfg(target_os = "linux")]
        {
            // Check kernel version >= 5.1
            if let Ok(version) = std::fs::read_to_string("/proc/version") {
                // Simple check - production would parse version properly
                return version.contains("Linux version 5.")
                    || version.contains("Linux version 6.");
            }
        }
        false
    }

    /// Detect FPGA hardware (placeholder for vendor SDK integration)
    fn detect_fpga(&self) {
        // Would integrate with:
        // - Xilinx/AMD: xbutil examine
        // - Intel: fpgainfo
        // - Solarflare: sfptpd

        // Check for common FPGA device files
        #[cfg(target_os = "linux")]
        {
            let fpga_paths = [
                "/dev/xdma0_user",       // Xilinx DMA
                "/dev/fpga0",            // Generic FPGA
                "/dev/intel-fpga-fme.0", // Intel FPGA
            ];

            for path in fpga_paths {
                if std::path::Path::new(path).exists() {
                    self.fpga_detected.store(1, Ordering::Relaxed);
                    return;
                }
            }
        }

        self.fpga_detected.store(0, Ordering::Relaxed);
    }

    /// Get snapshot for TUI rendering
    pub fn snapshot(&self) -> HardwareSnapshot {
        let net_interfaces = self.net_interfaces.read().clone();
        let pinned_cores = self.pinned_cores.read().clone();

        HardwareSnapshot {
            cpu_usage: self
                .cpu_usage
                .iter()
                .map(|u| u.load(Ordering::Relaxed) as f64 / 100.0)
                .collect(),
            cpu_freq_mhz: self
                .cpu_freq_mhz
                .iter()
                .map(|f| f.load(Ordering::Relaxed))
                .collect(),
            cpu_temp_c: self.cpu_temp_c.load(Ordering::Relaxed) as f64 / 100.0,
            mem_total_mb: self.mem_total_kb.load(Ordering::Relaxed) / 1024,
            mem_used_mb: self.mem_used_kb.load(Ordering::Relaxed) / 1024,
            mem_available_mb: self.mem_available_kb.load(Ordering::Relaxed) / 1024,
            swap_total_mb: self.swap_total_kb.load(Ordering::Relaxed) / 1024,
            swap_used_mb: self.swap_used_kb.load(Ordering::Relaxed) / 1024,
            net_interfaces,
            pinned_cores,
            fpga_detected: self.fpga_detected.load(Ordering::Relaxed) > 0,
            fpga_temp_c: self.fpga_temp_c.load(Ordering::Relaxed) as f64 / 100.0,
            fpga_power_mw: self.fpga_power_mw.load(Ordering::Relaxed),
        }
    }

    /// Pin current thread to a specific CPU core
    #[cfg(target_os = "linux")]
    pub fn pin_to_core(core: usize) -> std::io::Result<()> {
        use libc::{cpu_set_t, sched_setaffinity, CPU_SET, CPU_ZERO};
        use std::mem::MaybeUninit;

        unsafe {
            let mut set = MaybeUninit::<cpu_set_t>::uninit();
            let set_ptr = set.as_mut_ptr();
            CPU_ZERO(&mut *set_ptr);
            CPU_SET(core, &mut *set_ptr);

            let result = sched_setaffinity(0, std::mem::size_of::<cpu_set_t>(), set.as_ptr());
            if result == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn pin_to_core(_core: usize) -> std::io::Result<()> {
        // CPU pinning not supported on this platform
        Ok(())
    }

    /// Set thread priority to real-time (requires elevated privileges)
    #[cfg(target_os = "linux")]
    pub fn set_realtime_priority(priority: i32) -> std::io::Result<()> {
        use libc::{sched_param, sched_setscheduler, SCHED_FIFO};

        unsafe {
            let param = sched_param {
                sched_priority: priority,
            };

            let result = sched_setscheduler(0, SCHED_FIFO, &param);
            if result == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn set_realtime_priority(_priority: i32) -> std::io::Result<()> {
        Ok(())
    }
}

impl Default for HardwareMonitor {
    fn default() -> Self {
        Arc::try_unwrap(Self::new()).unwrap_or_else(|_| panic!("Failed to create HardwareMonitor"))
    }
}

/// Snapshot of hardware state for rendering
#[derive(Debug, Clone)]
pub struct HardwareSnapshot {
    pub cpu_usage: Vec<f64>,
    pub cpu_freq_mhz: Vec<u64>,
    pub cpu_temp_c: f64,
    pub mem_total_mb: u64,
    pub mem_used_mb: u64,
    pub mem_available_mb: u64,
    pub swap_total_mb: u64,
    pub swap_used_mb: u64,
    pub net_interfaces: Vec<NetInterfaceStats>,
    pub pinned_cores: Vec<usize>,
    pub fpga_detected: bool,
    pub fpga_temp_c: f64,
    pub fpga_power_mw: u64,
}

impl Default for HardwareSnapshot {
    fn default() -> Self {
        Self {
            cpu_usage: Vec::new(),
            cpu_freq_mhz: Vec::new(),
            cpu_temp_c: 0.0,
            mem_total_mb: 0,
            mem_used_mb: 0,
            mem_available_mb: 0,
            swap_total_mb: 0,
            swap_used_mb: 0,
            net_interfaces: Vec::new(),
            pinned_cores: Vec::new(),
            fpga_detected: false,
            fpga_temp_c: 0.0,
            fpga_power_mw: 0,
        }
    }
}

/// NUMA topology information
#[derive(Debug, Clone)]
pub struct NumaTopology {
    pub nodes: Vec<NumaNode>,
}

#[derive(Debug, Clone)]
pub struct NumaNode {
    pub id: usize,
    pub cpus: Vec<usize>,
    pub memory_mb: u64,
    pub distance: Vec<usize>, // Distance to other nodes
}

impl NumaTopology {
    /// Detect NUMA topology (Linux-specific)
    #[cfg(target_os = "linux")]
    pub fn detect() -> Option<Self> {
        let numa_path = std::path::Path::new("/sys/devices/system/node");
        if !numa_path.exists() {
            return None;
        }

        let mut nodes = Vec::new();

        for entry in std::fs::read_dir(numa_path).ok()? {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().to_string();

            if name.starts_with("node") {
                if let Ok(id) = name[4..].parse::<usize>() {
                    // Parse CPU list
                    let cpulist_path = entry.path().join("cpulist");
                    let cpus = if let Ok(content) = std::fs::read_to_string(&cpulist_path) {
                        Self::parse_cpulist(&content)
                    } else {
                        Vec::new()
                    };

                    // Parse memory info
                    let meminfo_path = entry.path().join("meminfo");
                    let memory_mb = if let Ok(content) = std::fs::read_to_string(&meminfo_path) {
                        Self::parse_node_memory(&content)
                    } else {
                        0
                    };

                    nodes.push(NumaNode {
                        id,
                        cpus,
                        memory_mb,
                        distance: Vec::new(),
                    });
                }
            }
        }

        if nodes.is_empty() {
            None
        } else {
            Some(Self { nodes })
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn detect() -> Option<Self> {
        None
    }

    #[cfg(target_os = "linux")]
    fn parse_cpulist(content: &str) -> Vec<usize> {
        let mut cpus = Vec::new();
        for part in content.trim().split(',') {
            if let Some((start, end)) = part.split_once('-') {
                if let (Ok(s), Ok(e)) = (start.parse::<usize>(), end.parse::<usize>()) {
                    cpus.extend(s..=e);
                }
            } else if let Ok(cpu) = part.parse::<usize>() {
                cpus.push(cpu);
            }
        }
        cpus
    }

    #[cfg(target_os = "linux")]
    fn parse_node_memory(content: &str) -> u64 {
        for line in content.lines() {
            if line.contains("MemTotal:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    if let Ok(kb) = parts[3].parse::<u64>() {
                        return kb / 1024; // Convert to MB
                    }
                }
            }
        }
        0
    }
}
