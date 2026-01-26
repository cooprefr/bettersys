//! Performance TUI Application
//!
//! Main application state and event handling for the HFT performance monitor.

use super::hardware::{HardwareMonitor, HardwareSnapshot};
use super::hft_metrics::{HftMetricsCollector, HftMetricsSnapshot};
use super::widgets::*;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Tabs, Widget},
    Frame, Terminal,
};
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Tab views in the performance monitor
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Latency,
    Network,
    Hardware,
    Jitter,
}

impl Tab {
    pub fn all() -> &'static [Tab] {
        &[
            Tab::Overview,
            Tab::Latency,
            Tab::Network,
            Tab::Hardware,
            Tab::Jitter,
        ]
    }

    pub fn title(&self) -> &'static str {
        match self {
            Tab::Overview => "OVERVIEW",
            Tab::Latency => "LATENCY",
            Tab::Network => "NETWORK",
            Tab::Hardware => "HARDWARE",
            Tab::Jitter => "JITTER",
        }
    }
}

/// Application state
pub struct PerfApp {
    pub running: bool,
    pub current_tab: Tab,

    // Data sources
    pub metrics: Arc<HftMetricsCollector>,
    pub hardware: Arc<HardwareMonitor>,

    // Cached snapshots (refreshed at tick)
    pub metrics_snapshot: HftMetricsSnapshot,
    pub hardware_snapshot: HardwareSnapshot,

    // Time series data for charts
    pub tick_latency_history: Vec<u64>,
    pub t2t_latency_history: Vec<u64>,
    pub throughput_history: Vec<u64>,
    pub rx_history: Vec<u64>,
    pub tx_history: Vec<u64>,

    // UI state
    pub show_help: bool,
    pub target_fps: u32,
    pub last_frame: Instant,
    pub frame_times: Vec<Duration>,

    // Connection state
    pub backend_connected: bool,
    pub backend_url: String,
}

impl PerfApp {
    pub fn new(backend_url: String) -> Self {
        let metrics = HftMetricsCollector::new();
        let hardware = HardwareMonitor::new();

        Self {
            running: true,
            current_tab: Tab::Overview,
            metrics: metrics.clone(),
            hardware: hardware.clone(),
            metrics_snapshot: HftMetricsSnapshot::default(),
            hardware_snapshot: HardwareSnapshot::default(),
            tick_latency_history: vec![0; 60],
            t2t_latency_history: vec![0; 60],
            throughput_history: vec![0; 60],
            rx_history: vec![0; 60],
            tx_history: vec![0; 60],
            show_help: false,
            target_fps: 60,
            last_frame: Instant::now(),
            frame_times: Vec::with_capacity(100),
            backend_connected: false,
            backend_url,
        }
    }

    /// Update snapshots from data sources
    pub fn tick(&mut self) {
        // Refresh hardware metrics
        self.hardware.refresh();
        self.hardware_snapshot = self.hardware.snapshot();

        // Get HFT metrics snapshot
        self.metrics_snapshot = self.metrics.snapshot();

        // Update time series
        self.tick_latency_history.remove(0);
        self.tick_latency_history
            .push(self.metrics_snapshot.tick_latency.p99);

        self.t2t_latency_history.remove(0);
        self.t2t_latency_history
            .push(self.metrics_snapshot.t2t_latency.p99);

        self.throughput_history.remove(0);
        self.throughput_history
            .push(self.metrics_snapshot.ticks_per_sec);

        // Track frame time
        let frame_time = self.last_frame.elapsed();
        self.frame_times.push(frame_time);
        if self.frame_times.len() > 100 {
            self.frame_times.remove(0);
        }
        self.last_frame = Instant::now();
    }

    /// Handle keyboard input
    pub fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.running = false,
            KeyCode::Char('?') | KeyCode::F(1) => self.show_help = !self.show_help,
            KeyCode::Tab | KeyCode::Right => self.next_tab(),
            KeyCode::BackTab | KeyCode::Left => self.prev_tab(),
            KeyCode::Char('1') => self.current_tab = Tab::Overview,
            KeyCode::Char('2') => self.current_tab = Tab::Latency,
            KeyCode::Char('3') => self.current_tab = Tab::Network,
            KeyCode::Char('4') => self.current_tab = Tab::Hardware,
            KeyCode::Char('5') => self.current_tab = Tab::Jitter,
            KeyCode::Char('r') => self.reset_metrics(),
            _ => {}
        }
    }

    fn next_tab(&mut self) {
        let tabs = Tab::all();
        let idx = tabs
            .iter()
            .position(|t| *t == self.current_tab)
            .unwrap_or(0);
        self.current_tab = tabs[(idx + 1) % tabs.len()];
    }

    fn prev_tab(&mut self) {
        let tabs = Tab::all();
        let idx = tabs
            .iter()
            .position(|t| *t == self.current_tab)
            .unwrap_or(0);
        self.current_tab = tabs[(idx + tabs.len() - 1) % tabs.len()];
    }

    fn reset_metrics(&mut self) {
        // Reset time series
        self.tick_latency_history = vec![0; 60];
        self.t2t_latency_history = vec![0; 60];
        self.throughput_history = vec![0; 60];
    }

    /// Calculate average frame time
    pub fn avg_frame_time(&self) -> Duration {
        if self.frame_times.is_empty() {
            Duration::from_millis(16)
        } else {
            let sum: Duration = self.frame_times.iter().sum();
            sum / self.frame_times.len() as u32
        }
    }

    /// Render the application
    pub fn render(&self, frame: &mut Frame) {
        let area = frame.size();

        // Main layout
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Header
                Constraint::Min(10),   // Content
                Constraint::Length(1), // Footer
            ])
            .split(area);

        self.render_header(frame, chunks[0]);
        self.render_content(frame, chunks[1]);
        self.render_footer(frame, chunks[2]);

        // Help overlay
        if self.show_help {
            self.render_help(frame, area);
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let titles: Vec<_> = Tab::all()
            .iter()
            .map(|t| Line::from(format!(" {} ", t.title())))
            .collect();

        let idx = Tab::all()
            .iter()
            .position(|t| *t == self.current_tab)
            .unwrap_or(0);

        let tabs = Tabs::new(titles)
            .block(
                Block::default()
                    .title(" BETTER PERFORMANCE MONITOR ")
                    .title_style(
                        Style::default()
                            .fg(ACCENT_CYAN)
                            .add_modifier(Modifier::BOLD),
                    )
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(BORDER_DIM)),
            )
            .select(idx)
            .style(Style::default().fg(TEXT_DIM))
            .highlight_style(
                Style::default()
                    .fg(ACCENT_CYAN)
                    .add_modifier(Modifier::BOLD)
                    .add_modifier(Modifier::UNDERLINED),
            );

        frame.render_widget(tabs, area);
    }

    fn render_content(&self, frame: &mut Frame, area: Rect) {
        match self.current_tab {
            Tab::Overview => self.render_overview(frame, area),
            Tab::Latency => self.render_latency(frame, area),
            Tab::Network => self.render_network(frame, area),
            Tab::Hardware => self.render_hardware(frame, area),
            Tab::Jitter => self.render_jitter(frame, area),
        }
    }

    fn render_overview(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);

        let left_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(8),
                Constraint::Min(5),
            ])
            .split(chunks[0]);

        let right_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(12), Constraint::Min(5)])
            .split(chunks[1]);

        // Status indicators
        self.render_status_row(frame, left_chunks[0]);

        // Tick-to-trade waterfall
        let waterfall = WaterfallChart::new("TICK-TO-TRADE BREAKDOWN")
            .stage(
                "Receive",
                self.metrics_snapshot.tick_latency.p50,
                ACCENT_GREEN,
            )
            .stage(
                "Signal",
                self.metrics_snapshot.signal_latency.p50,
                ACCENT_CYAN,
            )
            .stage("Decision", 500, ACCENT_YELLOW)
            .stage("Serialize", 100, ACCENT_PURPLE)
            .stage("Send", self.metrics_snapshot.order_latency.p50, ACCENT_RED);
        frame.render_widget(waterfall, left_chunks[1]);

        // Throughput sparkline
        let throughput_block = Block::default()
            .title(" THROUGHPUT ")
            .title_style(Style::default().fg(ACCENT_GREEN))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER_DIM));
        let throughput_inner = throughput_block.inner(left_chunks[2]);
        frame.render_widget(throughput_block, left_chunks[2]);

        let sparkline = ratatui::widgets::Sparkline::default()
            .data(&self.throughput_history)
            .style(Style::default().fg(ACCENT_CYAN));
        frame.render_widget(sparkline, throughput_inner);

        // Latency histogram
        let percentiles = LatencyPercentileData {
            min: self.metrics_snapshot.t2t_latency.min,
            p50: self.metrics_snapshot.t2t_latency.p50,
            p90: self.metrics_snapshot.t2t_latency.p90,
            p95: self.metrics_snapshot.t2t_latency.p95,
            p99: self.metrics_snapshot.t2t_latency.p99,
            p999: self.metrics_snapshot.t2t_latency.p999,
            max: self.metrics_snapshot.t2t_latency.max,
            mean: self.metrics_snapshot.t2t_latency.mean,
            count: self.metrics_snapshot.t2t_latency.count,
        };
        let histogram = LatencyHistogram::new("T2T LATENCY", &[0; 10], percentiles);
        frame.render_widget(histogram, right_chunks[0]);

        // Memory gauge
        let mem_gauge = MemoryGauge::new(
            "MEMORY",
            self.hardware_snapshot.mem_used_mb,
            self.hardware_snapshot.mem_total_mb,
        );
        frame.render_widget(mem_gauge, right_chunks[1]);
    }

    fn render_status_row(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(25),
                Constraint::Percentage(25),
                Constraint::Percentage(25),
                Constraint::Percentage(25),
            ])
            .split(area);

        let backend_status = if self.backend_connected {
            StatusIndicator::new("Backend", Status::Ok).with_detail(&self.backend_url)
        } else {
            StatusIndicator::new("Backend", Status::Disconnected).with_detail("offline")
        };
        frame.render_widget(backend_status, chunks[0]);

        let fpga_status = if self.metrics_snapshot.fpga_connected {
            StatusIndicator::new("FPGA", Status::Ok).with_detail("connected")
        } else {
            StatusIndicator::new("FPGA", Status::Disconnected).with_detail("not detected")
        };
        frame.render_widget(fpga_status, chunks[1]);

        let nic_status = if self
            .hardware_snapshot
            .net_interfaces
            .iter()
            .any(|n| n.hw_timestamp_capable)
        {
            StatusIndicator::new("NIC HW-TS", Status::Ok)
        } else {
            StatusIndicator::new("NIC HW-TS", Status::Warning).with_detail("software ts")
        };
        frame.render_widget(nic_status, chunks[2]);

        let jitter_status = if self.metrics_snapshot.max_jitter_ns < 1_000_000 {
            StatusIndicator::new("Jitter", Status::Ok)
        } else if self.metrics_snapshot.max_jitter_ns < 10_000_000 {
            StatusIndicator::new("Jitter", Status::Warning)
        } else {
            StatusIndicator::new("Jitter", Status::Error)
        };
        frame.render_widget(jitter_status, chunks[3]);
    }

    fn render_latency(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);

        let top_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[0]);

        let bottom_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[1]);

        // Tick receive latency
        let tick_percentiles = LatencyPercentileData {
            min: self.metrics_snapshot.tick_latency.min,
            p50: self.metrics_snapshot.tick_latency.p50,
            p90: self.metrics_snapshot.tick_latency.p90,
            p95: self.metrics_snapshot.tick_latency.p95,
            p99: self.metrics_snapshot.tick_latency.p99,
            p999: self.metrics_snapshot.tick_latency.p999,
            max: self.metrics_snapshot.tick_latency.max,
            mean: self.metrics_snapshot.tick_latency.mean,
            count: self.metrics_snapshot.tick_latency.count,
        };
        frame.render_widget(
            LatencyHistogram::new("TICK RECEIVE", &[0; 10], tick_percentiles),
            top_chunks[0],
        );

        // Signal generation latency
        let signal_percentiles = LatencyPercentileData {
            min: self.metrics_snapshot.signal_latency.min,
            p50: self.metrics_snapshot.signal_latency.p50,
            p90: self.metrics_snapshot.signal_latency.p90,
            p95: self.metrics_snapshot.signal_latency.p95,
            p99: self.metrics_snapshot.signal_latency.p99,
            p999: self.metrics_snapshot.signal_latency.p999,
            max: self.metrics_snapshot.signal_latency.max,
            mean: self.metrics_snapshot.signal_latency.mean,
            count: self.metrics_snapshot.signal_latency.count,
        };
        frame.render_widget(
            LatencyHistogram::new("SIGNAL GEN", &[0; 10], signal_percentiles),
            top_chunks[1],
        );

        // Order execution latency
        let order_percentiles = LatencyPercentileData {
            min: self.metrics_snapshot.order_latency.min,
            p50: self.metrics_snapshot.order_latency.p50,
            p90: self.metrics_snapshot.order_latency.p90,
            p95: self.metrics_snapshot.order_latency.p95,
            p99: self.metrics_snapshot.order_latency.p99,
            p999: self.metrics_snapshot.order_latency.p999,
            max: self.metrics_snapshot.order_latency.max,
            mean: self.metrics_snapshot.order_latency.mean,
            count: self.metrics_snapshot.order_latency.count,
        };
        frame.render_widget(
            LatencyHistogram::new("ORDER EXEC", &[0; 10], order_percentiles),
            bottom_chunks[0],
        );

        // Tick-to-trade latency
        let t2t_percentiles = LatencyPercentileData {
            min: self.metrics_snapshot.t2t_latency.min,
            p50: self.metrics_snapshot.t2t_latency.p50,
            p90: self.metrics_snapshot.t2t_latency.p90,
            p95: self.metrics_snapshot.t2t_latency.p95,
            p99: self.metrics_snapshot.t2t_latency.p99,
            p999: self.metrics_snapshot.t2t_latency.p999,
            max: self.metrics_snapshot.t2t_latency.max,
            mean: self.metrics_snapshot.t2t_latency.mean,
            count: self.metrics_snapshot.t2t_latency.count,
        };
        frame.render_widget(
            LatencyHistogram::new("TICK-TO-TRADE", &[0; 10], t2t_percentiles),
            bottom_chunks[1],
        );
    }

    fn render_network(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(6), Constraint::Min(5)])
            .split(area);

        // Network throughput
        frame.render_widget(
            NetworkThroughput::new(
                "NETWORK I/O",
                &self.rx_history,
                &self.tx_history,
                self.metrics_snapshot.nic_rx_bytes as f64,
                self.metrics_snapshot.nic_tx_bytes as f64,
            ),
            chunks[0],
        );

        // Interface list
        let block = Block::default()
            .title(" INTERFACES ")
            .title_style(Style::default().fg(ACCENT_CYAN))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER_DIM));
        let inner = block.inner(chunks[1]);
        frame.render_widget(block, chunks[1]);

        let mut lines = Vec::new();
        for iface in &self.hardware_snapshot.net_interfaces {
            let hw_ts = if iface.hw_timestamp_capable {
                "HW-TS"
            } else {
                "SW-TS"
            };
            let dpdk = if iface.dpdk_bound { "DPDK" } else { "" };

            lines.push(Line::from(vec![
                Span::styled(&iface.name, Style::default().fg(TEXT_BRIGHT)),
                Span::raw("  "),
                Span::styled(
                    hw_ts,
                    Style::default().fg(if iface.hw_timestamp_capable {
                        ACCENT_GREEN
                    } else {
                        TEXT_DIM
                    }),
                ),
                Span::raw(" "),
                Span::styled(dpdk, Style::default().fg(ACCENT_PURPLE)),
                Span::raw("  RX: "),
                Span::styled(
                    format!("{}", iface.rx_packets),
                    Style::default().fg(ACCENT_GREEN),
                ),
                Span::raw(" TX: "),
                Span::styled(
                    format!("{}", iface.tx_packets),
                    Style::default().fg(ACCENT_CYAN),
                ),
            ]));
        }
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_hardware(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(10),
                Constraint::Length(3),
                Constraint::Min(5),
            ])
            .split(area);

        // CPU grid
        frame.render_widget(
            CpuGrid::new(
                "CPU CORES",
                &self.hardware_snapshot.cpu_usage,
                &self.hardware_snapshot.cpu_freq_mhz,
                &self.hardware_snapshot.pinned_cores,
            ),
            chunks[0],
        );

        // Memory gauge
        frame.render_widget(
            MemoryGauge::new(
                "MEMORY",
                self.hardware_snapshot.mem_used_mb,
                self.hardware_snapshot.mem_total_mb,
            ),
            chunks[1],
        );

        // FPGA status
        let fpga_block = Block::default()
            .title(" FPGA ")
            .title_style(Style::default().fg(ACCENT_PURPLE))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER_DIM));
        let fpga_inner = fpga_block.inner(chunks[2]);
        frame.render_widget(fpga_block, chunks[2]);

        let fpga_status = if self.hardware_snapshot.fpga_detected {
            vec![
                Line::from(vec![
                    Span::styled("● ", Style::default().fg(ACCENT_GREEN)),
                    Span::styled("FPGA Detected", Style::default().fg(TEXT_BRIGHT)),
                ]),
                Line::from(vec![
                    Span::styled("  Temp: ", Style::default().fg(TEXT_DIM)),
                    Span::styled(
                        format!("{:.1}°C", self.hardware_snapshot.fpga_temp_c),
                        Style::default().fg(TEXT_BRIGHT),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("  Power: ", Style::default().fg(TEXT_DIM)),
                    Span::styled(
                        format!("{} mW", self.hardware_snapshot.fpga_power_mw),
                        Style::default().fg(TEXT_BRIGHT),
                    ),
                ]),
            ]
        } else {
            vec![Line::from(vec![
                Span::styled("○ ", Style::default().fg(TEXT_DIM)),
                Span::styled("No FPGA detected", Style::default().fg(TEXT_DIM)),
            ])]
        };
        frame.render_widget(Paragraph::new(fpga_status), fpga_inner);
    }

    fn render_jitter(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(8), Constraint::Min(5)])
            .split(area);

        // Jitter stats
        let jitter_block = Block::default()
            .title(" JITTER ANALYSIS ")
            .title_style(Style::default().fg(ACCENT_YELLOW))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER_DIM));
        let jitter_inner = jitter_block.inner(chunks[0]);
        frame.render_widget(jitter_block, chunks[0]);

        let max_jitter = self.metrics_snapshot.max_jitter_ns;
        let jitter_color = if max_jitter < 1_000_000 {
            ACCENT_GREEN
        } else if max_jitter < 10_000_000 {
            ACCENT_YELLOW
        } else {
            ACCENT_RED
        };

        let lines = vec![
            Line::from(vec![
                Span::styled("Max Jitter: ", Style::default().fg(TEXT_DIM)),
                Span::styled(
                    format_duration_ns(max_jitter),
                    Style::default().fg(jitter_color),
                ),
            ]),
            Line::from(vec![
                Span::styled("Gap Warnings: ", Style::default().fg(TEXT_DIM)),
                Span::styled(
                    format!("{}", self.metrics_snapshot.gap_warnings),
                    Style::default().fg(if self.metrics_snapshot.gap_warnings > 0 {
                        ACCENT_RED
                    } else {
                        ACCENT_GREEN
                    }),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Target Tick Rate: ", Style::default().fg(TEXT_DIM)),
                Span::styled("1ms (1000 Hz)", Style::default().fg(TEXT_BRIGHT)),
            ]),
        ];
        frame.render_widget(Paragraph::new(lines), jitter_inner);

        // Jitter histogram would go here
        let hist_block = Block::default()
            .title(" JITTER DISTRIBUTION ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER_DIM));
        frame.render_widget(hist_block, chunks[1]);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let avg_frame = self.avg_frame_time();
        let fps = 1.0 / avg_frame.as_secs_f64();

        let footer = Line::from(vec![
            Span::styled(" [Q]", Style::default().fg(ACCENT_CYAN)),
            Span::styled(" Quit ", Style::default().fg(TEXT_DIM)),
            Span::styled("[TAB]", Style::default().fg(ACCENT_CYAN)),
            Span::styled(" Switch Tab ", Style::default().fg(TEXT_DIM)),
            Span::styled("[?]", Style::default().fg(ACCENT_CYAN)),
            Span::styled(" Help ", Style::default().fg(TEXT_DIM)),
            Span::styled("[R]", Style::default().fg(ACCENT_CYAN)),
            Span::styled(" Reset ", Style::default().fg(TEXT_DIM)),
            Span::raw("  │  "),
            Span::styled(format!("{:.0} FPS", fps), Style::default().fg(ACCENT_GREEN)),
            Span::raw("  │  "),
            Span::styled(
                format!("Frame: {:.2}ms", avg_frame.as_secs_f64() * 1000.0),
                Style::default().fg(TEXT_DIM),
            ),
        ]);

        frame.render_widget(Paragraph::new(footer), area);
    }

    fn render_help(&self, frame: &mut Frame, area: Rect) {
        let help_area = centered_rect(60, 50, area);
        frame.render_widget(Clear, help_area);

        let block = Block::default()
            .title(" KEYBOARD SHORTCUTS ")
            .title_style(
                Style::default()
                    .fg(ACCENT_CYAN)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(ACCENT_CYAN))
            .style(Style::default().bg(Color::Rgb(16, 16, 16)));

        let inner = block.inner(help_area);
        frame.render_widget(block, help_area);

        let help_text = vec![
            Line::from(vec![
                Span::styled("Q / Esc", Style::default().fg(ACCENT_CYAN)),
                Span::raw("     Quit application"),
            ]),
            Line::from(vec![
                Span::styled("Tab / →", Style::default().fg(ACCENT_CYAN)),
                Span::raw("     Next tab"),
            ]),
            Line::from(vec![
                Span::styled("Shift+Tab / ←", Style::default().fg(ACCENT_CYAN)),
                Span::raw(" Previous tab"),
            ]),
            Line::from(vec![
                Span::styled("1-5", Style::default().fg(ACCENT_CYAN)),
                Span::raw("         Jump to tab"),
            ]),
            Line::from(vec![
                Span::styled("R", Style::default().fg(ACCENT_CYAN)),
                Span::raw("           Reset metrics"),
            ]),
            Line::from(vec![
                Span::styled("?", Style::default().fg(ACCENT_CYAN)),
                Span::raw("           Toggle help"),
            ]),
        ];

        frame.render_widget(Paragraph::new(help_text), inner);
    }
}

fn format_duration_ns(ns: u64) -> String {
    if ns < 1_000 {
        format!("{}ns", ns)
    } else if ns < 1_000_000 {
        format!("{:.1}μs", ns as f64 / 1_000.0)
    } else if ns < 1_000_000_000 {
        format!("{:.2}ms", ns as f64 / 1_000_000.0)
    } else {
        format!("{:.2}s", ns as f64 / 1_000_000_000.0)
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
