//! BETTER Performance Monitor
//!
//! Standalone TUI for real-time HFT performance visualization.
//!
//! Usage:
//!   better-perf                    # Standalone mode (local metrics only)
//!   better-perf --url http://...   # Connect to running backend
//!   better-perf --console          # Enable tokio-console integration
//!
//! Keyboard:
//!   Q/Esc     - Quit
//!   Tab/←/→   - Switch tabs
//!   1-5       - Jump to tab
//!   R         - Reset metrics
//!   ?         - Show help

use std::env;
use std::io;

// Import from the main crate
use betterbot_backend::performance::tui::{renderer, PerfApp};

fn print_banner() {
    eprintln!(
        r#"
╔══════════════════════════════════════════════════════════════════╗
║                                                                  ║
║   ██████╗ ███████╗████████╗████████╗███████╗██████╗              ║
║   ██╔══██╗██╔════╝╚══██╔══╝╚══██╔══╝██╔════╝██╔══██╗             ║
║   ██████╔╝█████╗     ██║      ██║   █████╗  ██████╔╝             ║
║   ██╔══██╗██╔══╝     ██║      ██║   ██╔══╝  ██╔══██╗             ║
║   ██████╔╝███████╗   ██║      ██║   ███████╗██║  ██║             ║
║   ╚═════╝ ╚══════╝   ╚═╝      ╚═╝   ╚══════╝╚═╝  ╚═╝             ║
║                                                                  ║
║   P E R F O R M A N C E   M O N I T O R                         ║
║                                                                  ║
║   HFT-grade real-time performance visualization                  ║
║   Tick-to-trade latency • Hardware monitoring • Jitter analysis  ║
║                                                                  ║
╚══════════════════════════════════════════════════════════════════╝
"#
    );
}

fn print_usage() {
    eprintln!(
        r#"
USAGE:
    better-perf [OPTIONS]

OPTIONS:
    --url <URL>     Connect to running backend (default: http://localhost:3000)
    --standalone    Run without backend connection
    --console       Enable tokio-console integration (requires RUSTFLAGS)
    --help          Show this help message

KEYBOARD SHORTCUTS:
    Q / Esc         Quit application
    Tab / →         Next tab
    Shift+Tab / ←   Previous tab
    1-5             Jump to specific tab
    R               Reset metrics
    ?               Toggle help overlay

TABS:
    1. OVERVIEW     System overview with T2T waterfall
    2. LATENCY      Detailed latency histograms
    3. NETWORK      NIC statistics and throughput
    4. HARDWARE     CPU cores, memory, FPGA status
    5. JITTER       Tick jitter analysis

ENVIRONMENT:
    BETTER_PERF_URL     Default backend URL
    BETTER_PERF_FPS     Target frame rate (default: 60)
"#
    );
}

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();

    // Parse arguments
    let mut backend_url =
        env::var("BETTER_PERF_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
    let mut standalone = false;
    let mut enable_console = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                print_banner();
                print_usage();
                return Ok(());
            }
            "--url" => {
                i += 1;
                if i < args.len() {
                    backend_url = args[i].clone();
                }
            }
            "--standalone" => standalone = true,
            "--console" => enable_console = true,
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
                print_usage();
                return Ok(());
            }
        }
        i += 1;
    }

    // Initialize tracing
    if enable_console {
        // tokio-console requires specific build flags
        // RUSTFLAGS="--cfg tokio_unstable" cargo build
        eprintln!("Note: tokio-console requires RUSTFLAGS=\"--cfg tokio_unstable\"");

        // Would initialize console_subscriber here:
        // console_subscriber::init();
    }

    print_banner();
    eprintln!("Connecting to: {}", backend_url);
    eprintln!("Press any key to start...\n");

    // Small delay to show banner
    std::thread::sleep(std::time::Duration::from_millis(500));

    if standalone {
        // Run without backend connection
        renderer::run_standalone(&backend_url)
    } else {
        // Run with async backend polling
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(async { renderer::run_with_backend(&backend_url).await })
    }
}
