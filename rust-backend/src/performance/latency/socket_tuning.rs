//! Low-Latency Socket Tuning Module
//!
//! Provides socket-level optimizations for high-frequency market data feeds:
//! - TCP_NODELAY (disable Nagle's algorithm)
//! - SO_RCVBUF / SO_SNDBUF sizing
//! - SO_BUSY_POLL (Linux-specific busy polling)
//! - TCP_QUICKACK (immediate ACKs)
//! - SO_PRIORITY (traffic prioritization)
//!
//! These settings complement kernel-level tuning (sysctl, IRQ affinity, etc.)

use std::io;
use std::net::TcpStream;
use std::os::unix::io::AsRawFd;
use tracing::{debug, warn};

/// Socket tuning configuration
#[derive(Debug, Clone)]
pub struct SocketTuningConfig {
    /// Receive buffer size in bytes (default: 4MB)
    pub recv_buffer_size: usize,
    /// Send buffer size in bytes (default: 1MB)
    pub send_buffer_size: usize,
    /// Busy-poll timeout in microseconds (0 = disabled, 50 = good default)
    pub busy_poll_us: u32,
    /// Enable TCP_NODELAY (disable Nagle's algorithm)
    pub tcp_nodelay: bool,
    /// Enable TCP_QUICKACK (immediate ACKs, reduces latency)
    pub tcp_quickack: bool,
    /// Socket priority (0-6, higher = more priority)
    pub socket_priority: Option<i32>,
    /// TCP keepalive interval in seconds (0 = system default)
    pub keepalive_secs: u32,
}

impl Default for SocketTuningConfig {
    fn default() -> Self {
        Self {
            recv_buffer_size: 4 * 1024 * 1024,  // 4MB
            send_buffer_size: 1 * 1024 * 1024,  // 1MB
            busy_poll_us: 50,                    // 50us busy-poll
            tcp_nodelay: true,
            tcp_quickack: true,
            socket_priority: Some(6),            // High priority
            keepalive_secs: 60,
        }
    }
}

impl SocketTuningConfig {
    /// Configuration optimized for market data reception
    pub fn market_data() -> Self {
        Self {
            recv_buffer_size: 8 * 1024 * 1024,  // 8MB for burst absorption
            send_buffer_size: 256 * 1024,       // 256KB (mostly receiving)
            busy_poll_us: 50,
            tcp_nodelay: true,
            tcp_quickack: true,
            socket_priority: Some(6),
            keepalive_secs: 30,
        }
    }

    /// Configuration for WebSocket connections (TLS overhead considered)
    pub fn websocket() -> Self {
        Self {
            recv_buffer_size: 4 * 1024 * 1024,  // 4MB
            send_buffer_size: 512 * 1024,       // 512KB
            busy_poll_us: 50,
            tcp_nodelay: true,
            tcp_quickack: true,
            socket_priority: Some(5),
            keepalive_secs: 60,
        }
    }

    /// Conservative configuration (lower resource usage)
    pub fn conservative() -> Self {
        Self {
            recv_buffer_size: 1 * 1024 * 1024,  // 1MB
            send_buffer_size: 256 * 1024,       // 256KB
            busy_poll_us: 0,                     // Disabled
            tcp_nodelay: true,
            tcp_quickack: false,
            socket_priority: None,
            keepalive_secs: 120,
        }
    }
}

/// Result of applying socket tuning
#[derive(Debug, Clone)]
pub struct SocketTuningResult {
    pub tcp_nodelay_set: bool,
    pub recv_buffer_actual: Option<usize>,
    pub send_buffer_actual: Option<usize>,
    pub busy_poll_set: bool,
    pub tcp_quickack_set: bool,
    pub priority_set: bool,
    pub keepalive_set: bool,
    pub errors: Vec<String>,
}

impl SocketTuningResult {
    /// Check if tuning was fully successful
    pub fn is_fully_applied(&self) -> bool {
        self.errors.is_empty()
    }

    /// Log the tuning results
    pub fn log_summary(&self) {
        if self.errors.is_empty() {
            debug!(
                "Socket tuning applied: nodelay={}, rcvbuf={:?}, sndbuf={:?}, busy_poll={}, quickack={}",
                self.tcp_nodelay_set,
                self.recv_buffer_actual,
                self.send_buffer_actual,
                self.busy_poll_set,
                self.tcp_quickack_set
            );
        } else {
            warn!(
                "Socket tuning partially applied: {} errors: {:?}",
                self.errors.len(),
                self.errors
            );
        }
    }
}

/// Apply low-latency tuning to a raw file descriptor
///
/// # Safety
/// The fd must be a valid socket file descriptor
pub fn apply_socket_tuning_fd(fd: i32, config: &SocketTuningConfig) -> SocketTuningResult {
    let mut result = SocketTuningResult {
        tcp_nodelay_set: false,
        recv_buffer_actual: None,
        send_buffer_actual: None,
        busy_poll_set: false,
        tcp_quickack_set: false,
        priority_set: false,
        keepalive_set: false,
        errors: Vec::new(),
    };

    // TCP_NODELAY - Disable Nagle's algorithm
    if config.tcp_nodelay {
        let val: libc::c_int = 1;
        let ret = unsafe {
            libc::setsockopt(
                fd,
                libc::IPPROTO_TCP,
                libc::TCP_NODELAY,
                &val as *const _ as *const libc::c_void,
                std::mem::size_of_val(&val) as libc::socklen_t,
            )
        };
        if ret == 0 {
            result.tcp_nodelay_set = true;
        } else {
            result.errors.push(format!("TCP_NODELAY: {}", io::Error::last_os_error()));
        }
    }

    // SO_RCVBUF - Receive buffer size
    {
        let val: libc::c_int = config.recv_buffer_size as libc::c_int;
        let ret = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                &val as *const _ as *const libc::c_void,
                std::mem::size_of_val(&val) as libc::socklen_t,
            )
        };
        if ret == 0 {
            // Read back actual value (kernel may adjust)
            let mut actual: libc::c_int = 0;
            let mut len: libc::socklen_t = std::mem::size_of_val(&actual) as libc::socklen_t;
            unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_RCVBUF,
                    &mut actual as *mut _ as *mut libc::c_void,
                    &mut len,
                );
            }
            result.recv_buffer_actual = Some(actual as usize);
        } else {
            result.errors.push(format!("SO_RCVBUF: {}", io::Error::last_os_error()));
        }
    }

    // SO_SNDBUF - Send buffer size
    {
        let val: libc::c_int = config.send_buffer_size as libc::c_int;
        let ret = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                &val as *const _ as *const libc::c_void,
                std::mem::size_of_val(&val) as libc::socklen_t,
            )
        };
        if ret == 0 {
            let mut actual: libc::c_int = 0;
            let mut len: libc::socklen_t = std::mem::size_of_val(&actual) as libc::socklen_t;
            unsafe {
                libc::getsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    &mut actual as *mut _ as *mut libc::c_void,
                    &mut len,
                );
            }
            result.send_buffer_actual = Some(actual as usize);
        } else {
            result.errors.push(format!("SO_SNDBUF: {}", io::Error::last_os_error()));
        }
    }

    // SO_BUSY_POLL - Linux-specific busy polling
    #[cfg(target_os = "linux")]
    if config.busy_poll_us > 0 {
        const SO_BUSY_POLL: libc::c_int = 46;
        let val: libc::c_int = config.busy_poll_us as libc::c_int;
        let ret = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                SO_BUSY_POLL,
                &val as *const _ as *const libc::c_void,
                std::mem::size_of_val(&val) as libc::socklen_t,
            )
        };
        if ret == 0 {
            result.busy_poll_set = true;
        } else {
            // Busy-poll may not be available on all kernels
            result.errors.push(format!("SO_BUSY_POLL: {}", io::Error::last_os_error()));
        }
    }

    // TCP_QUICKACK - Immediate ACKs (Linux-specific)
    #[cfg(target_os = "linux")]
    if config.tcp_quickack {
        const TCP_QUICKACK: libc::c_int = 12;
        let val: libc::c_int = 1;
        let ret = unsafe {
            libc::setsockopt(
                fd,
                libc::IPPROTO_TCP,
                TCP_QUICKACK,
                &val as *const _ as *const libc::c_void,
                std::mem::size_of_val(&val) as libc::socklen_t,
            )
        };
        if ret == 0 {
            result.tcp_quickack_set = true;
        } else {
            result.errors.push(format!("TCP_QUICKACK: {}", io::Error::last_os_error()));
        }
    }

    // SO_PRIORITY - Traffic prioritization (Linux-specific)
    #[cfg(target_os = "linux")]
    if let Some(priority) = config.socket_priority {
        const SO_PRIORITY: libc::c_int = 12;
        let val: libc::c_int = priority;
        let ret = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                SO_PRIORITY,
                &val as *const _ as *const libc::c_void,
                std::mem::size_of_val(&val) as libc::socklen_t,
            )
        };
        if ret == 0 {
            result.priority_set = true;
        } else {
            // May require CAP_NET_ADMIN
            result.errors.push(format!("SO_PRIORITY: {}", io::Error::last_os_error()));
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = config.socket_priority; // Suppress unused warning
    }

    // SO_KEEPALIVE + TCP_KEEPIDLE
    if config.keepalive_secs > 0 {
        let val: libc::c_int = 1;
        let ret = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_KEEPALIVE,
                &val as *const _ as *const libc::c_void,
                std::mem::size_of_val(&val) as libc::socklen_t,
            )
        };
        if ret == 0 {
            // Set keepalive interval
            #[cfg(target_os = "linux")]
            {
                const TCP_KEEPIDLE: libc::c_int = 4;
                const TCP_KEEPINTVL: libc::c_int = 5;
                const TCP_KEEPCNT: libc::c_int = 6;

                let idle_val: libc::c_int = config.keepalive_secs as libc::c_int;
                let intvl_val: libc::c_int = 10; // 10 second intervals
                let cnt_val: libc::c_int = 6;    // 6 probes

                unsafe {
                    libc::setsockopt(
                        fd,
                        libc::IPPROTO_TCP,
                        TCP_KEEPIDLE,
                        &idle_val as *const _ as *const libc::c_void,
                        std::mem::size_of_val(&idle_val) as libc::socklen_t,
                    );
                    libc::setsockopt(
                        fd,
                        libc::IPPROTO_TCP,
                        TCP_KEEPINTVL,
                        &intvl_val as *const _ as *const libc::c_void,
                        std::mem::size_of_val(&intvl_val) as libc::socklen_t,
                    );
                    libc::setsockopt(
                        fd,
                        libc::IPPROTO_TCP,
                        TCP_KEEPCNT,
                        &cnt_val as *const _ as *const libc::c_void,
                        std::mem::size_of_val(&cnt_val) as libc::socklen_t,
                    );
                }
            }
            result.keepalive_set = true;
        } else {
            result.errors.push(format!("SO_KEEPALIVE: {}", io::Error::last_os_error()));
        }
    }

    result
}

/// Apply low-latency tuning to a TcpStream
pub fn apply_socket_tuning(stream: &TcpStream, config: &SocketTuningConfig) -> SocketTuningResult {
    let fd = stream.as_raw_fd();
    apply_socket_tuning_fd(fd, config)
}

/// Get current socket buffer sizes
pub fn get_socket_buffer_sizes(fd: i32) -> (Option<usize>, Option<usize>) {
    let mut recv_buf: libc::c_int = 0;
    let mut send_buf: libc::c_int = 0;
    let mut len: libc::socklen_t = std::mem::size_of::<libc::c_int>() as libc::socklen_t;

    let recv_result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &mut recv_buf as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };

    let send_result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            &mut send_buf as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };

    (
        if recv_result == 0 { Some(recv_buf as usize) } else { None },
        if send_result == 0 { Some(send_buf as usize) } else { None },
    )
}

/// Check if TCP_NODELAY is enabled
pub fn is_tcp_nodelay_enabled(fd: i32) -> Option<bool> {
    let mut val: libc::c_int = 0;
    let mut len: libc::socklen_t = std::mem::size_of::<libc::c_int>() as libc::socklen_t;

    let ret = unsafe {
        libc::getsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_NODELAY,
            &mut val as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };

    if ret == 0 {
        Some(val != 0)
    } else {
        None
    }
}

/// Diagnostic: Get all socket tuning parameters
#[derive(Debug, Clone)]
pub struct SocketDiagnostics {
    pub recv_buffer: Option<usize>,
    pub send_buffer: Option<usize>,
    pub tcp_nodelay: Option<bool>,
    pub tcp_quickack: Option<bool>,
    pub busy_poll: Option<u32>,
    pub priority: Option<i32>,
}

pub fn diagnose_socket(fd: i32) -> SocketDiagnostics {
    let (recv_buffer, send_buffer) = get_socket_buffer_sizes(fd);
    let tcp_nodelay = is_tcp_nodelay_enabled(fd);

    // TCP_QUICKACK
    #[cfg(target_os = "linux")]
    let tcp_quickack = {
        const TCP_QUICKACK: libc::c_int = 12;
        let mut val: libc::c_int = 0;
        let mut len: libc::socklen_t = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let ret = unsafe {
            libc::getsockopt(
                fd,
                libc::IPPROTO_TCP,
                TCP_QUICKACK,
                &mut val as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        if ret == 0 { Some(val != 0) } else { None }
    };
    #[cfg(not(target_os = "linux"))]
    let tcp_quickack = None;

    // SO_BUSY_POLL
    #[cfg(target_os = "linux")]
    let busy_poll = {
        const SO_BUSY_POLL: libc::c_int = 46;
        let mut val: libc::c_int = 0;
        let mut len: libc::socklen_t = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let ret = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                SO_BUSY_POLL,
                &mut val as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        if ret == 0 { Some(val as u32) } else { None }
    };
    #[cfg(not(target_os = "linux"))]
    let busy_poll = None;

    // SO_PRIORITY (Linux-specific)
    #[cfg(target_os = "linux")]
    let priority = {
        const SO_PRIORITY: libc::c_int = 12;
        let mut val: libc::c_int = 0;
        let mut len: libc::socklen_t = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let ret = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                SO_PRIORITY,
                &mut val as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        if ret == 0 { Some(val) } else { None }
    };
    #[cfg(not(target_os = "linux"))]
    let priority: Option<i32> = None;

    SocketDiagnostics {
        recv_buffer,
        send_buffer,
        tcp_nodelay,
        tcp_quickack,
        busy_poll,
        priority,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn test_socket_tuning_config_defaults() {
        let config = SocketTuningConfig::default();
        assert_eq!(config.recv_buffer_size, 4 * 1024 * 1024);
        assert!(config.tcp_nodelay);
        assert_eq!(config.busy_poll_us, 50);
    }

    #[test]
    fn test_apply_tuning_to_listener() {
        // Create a listener to get a valid socket fd
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let fd = listener.as_raw_fd();

        let config = SocketTuningConfig::conservative();
        let result = apply_socket_tuning_fd(fd, &config);

        // TCP_NODELAY should work on any socket
        // Buffer sizes should be set (actual values may differ from requested)
        assert!(result.recv_buffer_actual.is_some());
        assert!(result.send_buffer_actual.is_some());

        result.log_summary();
    }

    #[test]
    fn test_diagnose_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let fd = listener.as_raw_fd();

        let diag = diagnose_socket(fd);
        assert!(diag.recv_buffer.is_some());
        assert!(diag.send_buffer.is_some());
    }
}
