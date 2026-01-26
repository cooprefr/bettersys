//! Custom TUI Widgets for HFT Performance Visualization
//!
//! BETTER-themed widgets with AMOLED-black aesthetic

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{
        Axis, Block, Borders, Chart, Dataset, Gauge, GraphType, Paragraph, Row, Sparkline, Table,
        Widget,
    },
};

// BETTER color palette (AMOLED-black theme)
pub const BG_BLACK: Color = Color::Rgb(0, 0, 0);
pub const ACCENT_CYAN: Color = Color::Rgb(0, 255, 255);
pub const ACCENT_GREEN: Color = Color::Rgb(0, 255, 136);
pub const ACCENT_RED: Color = Color::Rgb(255, 68, 68);
pub const ACCENT_YELLOW: Color = Color::Rgb(255, 204, 0);
pub const ACCENT_PURPLE: Color = Color::Rgb(168, 85, 247);
pub const TEXT_DIM: Color = Color::Rgb(128, 128, 128);
pub const TEXT_BRIGHT: Color = Color::Rgb(255, 255, 255);
pub const BORDER_DIM: Color = Color::Rgb(48, 48, 48);

/// Latency histogram widget with logarithmic buckets
pub struct LatencyHistogram<'a> {
    title: &'a str,
    buckets: &'a [u64],
    percentiles: LatencyPercentileData,
    target_ns: Option<u64>,
}

#[derive(Clone, Default)]
pub struct LatencyPercentileData {
    pub min: u64,
    pub p50: u64,
    pub p90: u64,
    pub p95: u64,
    pub p99: u64,
    pub p999: u64,
    pub max: u64,
    pub mean: u64,
    pub count: u64,
}

impl<'a> LatencyHistogram<'a> {
    pub fn new(title: &'a str, buckets: &'a [u64], percentiles: LatencyPercentileData) -> Self {
        Self {
            title,
            buckets,
            percentiles,
            target_ns: None,
        }
    }

    pub fn with_target(mut self, target_ns: u64) -> Self {
        self.target_ns = Some(target_ns);
        self
    }
}

impl<'a> Widget for LatencyHistogram<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .title(format!(" {} ", self.title))
            .title_style(
                Style::default()
                    .fg(ACCENT_CYAN)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER_DIM));

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height < 3 || inner.width < 20 {
            return;
        }

        // Split into histogram and stats
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(inner);

        // Render histogram bars
        self.render_bars(chunks[0], buf);

        // Render percentile stats
        self.render_stats(chunks[1], buf);
    }
}

impl<'a> LatencyHistogram<'a> {
    fn render_bars(&self, area: Rect, buf: &mut Buffer) {
        let max_count = self.buckets.iter().max().copied().unwrap_or(1).max(1);
        let bar_height = area.height.saturating_sub(2) as f64;

        // Bucket labels (logarithmic: 1μs, 10μs, 100μs, 1ms, 10ms, 100ms, 1s)
        let labels = ["1μ", "10μ", "100μ", "1m", "10m", "100m", "1s"];

        for (i, &count) in self
            .buckets
            .iter()
            .take(area.width as usize / 3)
            .enumerate()
        {
            let x = area.x + (i as u16 * 3);
            if x >= area.x + area.width - 2 {
                break;
            }

            let height = ((count as f64 / max_count as f64) * bar_height) as u16;
            let color = self.bucket_color(i);

            // Draw bar
            for dy in 0..height {
                let y = area.y + area.height - 2 - dy;
                if y > area.y {
                    buf.get_mut(x, y).set_char('█').set_fg(color);
                    buf.get_mut(x + 1, y).set_char('█').set_fg(color);
                }
            }

            // Draw label
            if i < labels.len() {
                let label_y = area.y + area.height - 1;
                for (j, ch) in labels[i].chars().enumerate() {
                    let label_x = x + j as u16;
                    if label_x < area.x + area.width {
                        buf.get_mut(label_x, label_y).set_char(ch).set_fg(TEXT_DIM);
                    }
                }
            }
        }
    }

    fn render_stats(&self, area: Rect, buf: &mut Buffer) {
        let p = &self.percentiles;

        let lines = vec![
            Line::from(vec![
                Span::styled("min  ", Style::default().fg(TEXT_DIM)),
                Span::styled(format_ns(p.min), Style::default().fg(ACCENT_GREEN)),
            ]),
            Line::from(vec![
                Span::styled("p50  ", Style::default().fg(TEXT_DIM)),
                Span::styled(format_ns(p.p50), Style::default().fg(TEXT_BRIGHT)),
            ]),
            Line::from(vec![
                Span::styled("p90  ", Style::default().fg(TEXT_DIM)),
                Span::styled(format_ns(p.p90), Style::default().fg(TEXT_BRIGHT)),
            ]),
            Line::from(vec![
                Span::styled("p99  ", Style::default().fg(TEXT_DIM)),
                Span::styled(format_ns(p.p99), Style::default().fg(ACCENT_YELLOW)),
            ]),
            Line::from(vec![
                Span::styled("p999 ", Style::default().fg(TEXT_DIM)),
                Span::styled(format_ns(p.p999), Style::default().fg(ACCENT_RED)),
            ]),
            Line::from(vec![
                Span::styled("max  ", Style::default().fg(TEXT_DIM)),
                Span::styled(format_ns(p.max), Style::default().fg(ACCENT_RED)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("n=", Style::default().fg(TEXT_DIM)),
                Span::styled(format!("{}", p.count), Style::default().fg(ACCENT_CYAN)),
            ]),
        ];

        Paragraph::new(lines).render(area, buf);
    }

    fn bucket_color(&self, bucket_idx: usize) -> Color {
        match bucket_idx {
            0..=2 => ACCENT_GREEN,  // < 100μs: great
            3..=4 => ACCENT_YELLOW, // 100μs - 10ms: ok
            _ => ACCENT_RED,        // > 10ms: bad
        }
    }
}

/// Format nanoseconds as human-readable
fn format_ns(ns: u64) -> String {
    if ns == 0 {
        return "0".to_string();
    }
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

/// Tick-to-trade waterfall visualization
pub struct WaterfallChart<'a> {
    title: &'a str,
    stages: Vec<(&'a str, u64, Color)>,
}

impl<'a> WaterfallChart<'a> {
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            stages: Vec::new(),
        }
    }

    pub fn stage(mut self, name: &'a str, duration_ns: u64, color: Color) -> Self {
        self.stages.push((name, duration_ns, color));
        self
    }
}

impl<'a> Widget for WaterfallChart<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .title(format!(" {} ", self.title))
            .title_style(
                Style::default()
                    .fg(ACCENT_PURPLE)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER_DIM));

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height < 2 || self.stages.is_empty() {
            return;
        }

        let total: u64 = self.stages.iter().map(|(_, d, _)| d).sum();
        if total == 0 {
            return;
        }

        let bar_width = inner.width.saturating_sub(15); // Leave room for labels
        let mut x_offset: u16 = 0;

        for (i, (name, duration, color)) in self.stages.iter().enumerate() {
            let y = inner.y + (i as u16 * 2);
            if y >= inner.y + inner.height {
                break;
            }

            // Label
            let label = format!("{:>8}", name);
            for (j, ch) in label.chars().enumerate() {
                let label_x = inner.x + j as u16;
                if label_x < inner.x + inner.width {
                    buf.get_mut(label_x, y).set_char(ch).set_fg(TEXT_DIM);
                }
            }

            // Bar
            let width = ((*duration as f64 / total as f64) * bar_width as f64) as u16;
            let bar_x = inner.x + 9;

            for dx in 0..width {
                let cell_x = bar_x + dx;
                if cell_x < inner.x + inner.width {
                    buf.get_mut(cell_x, y).set_char('█').set_fg(*color);
                }
            }

            // Duration label
            let dur_label = format_ns(*duration);
            let label_x = bar_x + width + 1;
            for (j, ch) in dur_label.chars().enumerate() {
                let cell_x = label_x + j as u16;
                if cell_x < inner.x + inner.width {
                    buf.get_mut(cell_x, y).set_char(ch).set_fg(TEXT_BRIGHT);
                }
            }
        }

        // Total line
        let total_y = inner.y + inner.height - 1;
        let total_label = format!("TOTAL: {}", format_ns(total));
        for (j, ch) in total_label.chars().enumerate() {
            let cell_x = inner.x + j as u16;
            if cell_x < inner.x + inner.width {
                buf.get_mut(cell_x, total_y).set_char(ch).set_style(
                    Style::default()
                        .fg(ACCENT_CYAN)
                        .add_modifier(Modifier::BOLD),
                );
            }
        }
    }
}

/// Network throughput sparkline with RX/TX
pub struct NetworkThroughput<'a> {
    title: &'a str,
    rx_data: &'a [u64],
    tx_data: &'a [u64],
    rx_rate: f64,
    tx_rate: f64,
}

impl<'a> NetworkThroughput<'a> {
    pub fn new(
        title: &'a str,
        rx_data: &'a [u64],
        tx_data: &'a [u64],
        rx_rate: f64,
        tx_rate: f64,
    ) -> Self {
        Self {
            title,
            rx_data,
            tx_data,
            rx_rate,
            tx_rate,
        }
    }
}

impl<'a> Widget for NetworkThroughput<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .title(format!(" {} ", self.title))
            .title_style(
                Style::default()
                    .fg(ACCENT_GREEN)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER_DIM));

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height < 4 {
            return;
        }

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Length(2)])
            .split(inner);

        // RX sparkline
        let rx_label = format!("RX {}/s", format_bytes(self.rx_rate as u64));
        Paragraph::new(Line::from(vec![
            Span::styled("▼ ", Style::default().fg(ACCENT_GREEN)),
            Span::styled(rx_label, Style::default().fg(TEXT_BRIGHT)),
        ]))
        .render(Rect::new(chunks[0].x, chunks[0].y, chunks[0].width, 1), buf);

        Sparkline::default()
            .data(self.rx_data)
            .style(Style::default().fg(ACCENT_GREEN))
            .render(
                Rect::new(chunks[0].x, chunks[0].y + 1, chunks[0].width, 1),
                buf,
            );

        // TX sparkline
        let tx_label = format!("TX {}/s", format_bytes(self.tx_rate as u64));
        Paragraph::new(Line::from(vec![
            Span::styled("▲ ", Style::default().fg(ACCENT_CYAN)),
            Span::styled(tx_label, Style::default().fg(TEXT_BRIGHT)),
        ]))
        .render(Rect::new(chunks[1].x, chunks[1].y, chunks[1].width, 1), buf);

        Sparkline::default()
            .data(self.tx_data)
            .style(Style::default().fg(ACCENT_CYAN))
            .render(
                Rect::new(chunks[1].x, chunks[1].y + 1, chunks[1].width, 1),
                buf,
            );
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// CPU core utilization grid
pub struct CpuGrid<'a> {
    title: &'a str,
    usage: &'a [f64],
    freq_mhz: &'a [u64],
    pinned: &'a [usize],
}

impl<'a> CpuGrid<'a> {
    pub fn new(title: &'a str, usage: &'a [f64], freq_mhz: &'a [u64], pinned: &'a [usize]) -> Self {
        Self {
            title,
            usage,
            freq_mhz,
            pinned,
        }
    }
}

impl<'a> Widget for CpuGrid<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let block = Block::default()
            .title(format!(" {} ", self.title))
            .title_style(
                Style::default()
                    .fg(ACCENT_YELLOW)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(BORDER_DIM));

        let inner = block.inner(area);
        block.render(area, buf);

        if inner.height < 2 {
            return;
        }

        let cols = (inner.width / 8).max(1) as usize;
        let cell_width = inner.width / cols as u16;

        for (i, usage) in self.usage.iter().enumerate() {
            let row = i / cols;
            let col = i % cols;

            let x = inner.x + (col as u16 * cell_width);
            let y = inner.y + (row as u16 * 2);

            if y >= inner.y + inner.height {
                break;
            }

            let is_pinned = self.pinned.contains(&i);
            let color = usage_color(*usage);
            let pin_marker = if is_pinned { "●" } else { " " };

            // Core number and pin marker
            let label = format!("{:2}{}", i, pin_marker);
            for (j, ch) in label.chars().enumerate() {
                let cell_x = x + j as u16;
                if cell_x < inner.x + inner.width {
                    let style = if is_pinned {
                        Style::default()
                            .fg(ACCENT_PURPLE)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(TEXT_DIM)
                    };
                    buf.get_mut(cell_x, y).set_char(ch).set_style(style);
                }
            }

            // Usage bar
            let bar_width = ((cell_width - 4) as f64 * (*usage / 100.0)) as u16;
            for dx in 0..bar_width {
                let bar_cell_x = x + 4 + dx;
                let bar_cell_y = y;
                if bar_cell_x < inner.x + inner.width && bar_cell_y + 1 < inner.y + inner.height {
                    buf.get_mut(bar_cell_x, bar_cell_y)
                        .set_char('▄')
                        .set_fg(color);
                }
            }
        }
    }
}

fn usage_color(usage: f64) -> Color {
    if usage < 30.0 {
        ACCENT_GREEN
    } else if usage < 70.0 {
        ACCENT_YELLOW
    } else {
        ACCENT_RED
    }
}

/// Status indicator with heartbeat
pub struct StatusIndicator<'a> {
    label: &'a str,
    status: Status,
    detail: Option<&'a str>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Status {
    Ok,
    Warning,
    Error,
    Disconnected,
}

impl<'a> StatusIndicator<'a> {
    pub fn new(label: &'a str, status: Status) -> Self {
        Self {
            label,
            status,
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: &'a str) -> Self {
        self.detail = Some(detail);
        self
    }
}

impl<'a> Widget for StatusIndicator<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (icon, color) = match self.status {
            Status::Ok => ("●", ACCENT_GREEN),
            Status::Warning => ("●", ACCENT_YELLOW),
            Status::Error => ("●", ACCENT_RED),
            Status::Disconnected => ("○", TEXT_DIM),
        };

        let detail_text = self.detail.unwrap_or("");
        let line = Line::from(vec![
            Span::styled(icon, Style::default().fg(color)),
            Span::raw(" "),
            Span::styled(self.label, Style::default().fg(TEXT_BRIGHT)),
            Span::raw(" "),
            Span::styled(detail_text, Style::default().fg(TEXT_DIM)),
        ]);

        Paragraph::new(line).render(area, buf);
    }
}

/// Memory gauge with NUMA awareness
pub struct MemoryGauge<'a> {
    title: &'a str,
    used_mb: u64,
    total_mb: u64,
    numa_node: Option<usize>,
}

impl<'a> MemoryGauge<'a> {
    pub fn new(title: &'a str, used_mb: u64, total_mb: u64) -> Self {
        Self {
            title,
            used_mb,
            total_mb,
            numa_node: None,
        }
    }

    pub fn with_numa(mut self, node: usize) -> Self {
        self.numa_node = Some(node);
        self
    }
}

impl<'a> Widget for MemoryGauge<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let pct = if self.total_mb > 0 {
            (self.used_mb as f64 / self.total_mb as f64 * 100.0) as u16
        } else {
            0
        };

        let color = if pct < 60 {
            ACCENT_GREEN
        } else if pct < 85 {
            ACCENT_YELLOW
        } else {
            ACCENT_RED
        };

        let numa_label = self
            .numa_node
            .map(|n| format!(" [NUMA{}]", n))
            .unwrap_or_default();
        let label = format!(
            "{}{} {}/{} MB ({}%)",
            self.title, numa_label, self.used_mb, self.total_mb, pct
        );

        Gauge::default()
            .block(Block::default().borders(Borders::NONE))
            .gauge_style(Style::default().fg(color).bg(BORDER_DIM))
            .label(label)
            .ratio(pct as f64 / 100.0)
            .render(area, buf);
    }
}
