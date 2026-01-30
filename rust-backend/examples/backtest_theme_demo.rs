//! Backtest Theme Demo
//!
//! Demonstrates the light and dark theme modes for backtest console output.
//!
//! Run with:
//!   cargo run --example backtest_theme_demo
//!
//! Or with light theme:
//!   BACKTEST_THEME=light cargo run --example backtest_theme_demo

use betterbot_backend::backtest_v2::{
    ThemeMode, Theme, BannerStyle, format_banner, format_status_line, format_metric,
    RunGrade, format_operating_mode_banner, BacktestOperatingMode,
};

fn main() {
    println!("=== Backtest Theme Demo ===\n");
    
    // Initialize theme from environment (BACKTEST_THEME=light or BACKTEST_THEME=dark)
    ThemeMode::init_from_env();
    let current = ThemeMode::current();
    println!("Current theme: {:?}\n", current);
    
    // Demo 1: Operating mode banners
    println!("--- Operating Mode Banners ---");
    print!("{}", format_operating_mode_banner(BacktestOperatingMode::ProductionGrade));
    print!("{}", format_operating_mode_banner(BacktestOperatingMode::ResearchGrade));
    print!("{}", format_operating_mode_banner(BacktestOperatingMode::TakerOnly));
    
    // Demo 2: Run grade banners  
    println!("\n--- Run Grade Banners ---");
    print!("{}", RunGrade::ProductionGrade.format_banner());
    print!("{}", RunGrade::ExploratoryGrade.format_banner());
    print!("{}", RunGrade::SimulationOnly.format_banner());
    
    // Demo 3: Custom banners using theme helpers
    println!("\n--- Custom Banners ---");
    print!("{}", format_banner(
        "BTC 15M BACKTEST RESULTS",
        &[
            "Strategy: FAST15M_Taker",
            "Window Count: 1,234",
            "Net PnL: +$2,345.67",
            "Sharpe: 1.85",
        ],
        BannerStyle::Success,
    ));
    
    print!("{}", format_banner(
        "WARNING: DATA QUALITY ISSUES",
        &[
            "Missing 15 windows (1.2%)",
            "3 gaps detected in price feed",
            "Results may be biased",
        ],
        BannerStyle::Warning,
    ));
    
    // Demo 4: Status lines
    println!("\n--- Status Lines ---");
    println!("{}", format_status_line("Production Grade", "ENABLED", true));
    println!("{}", format_status_line("Strict Accounting", "ENABLED", true));
    println!("{}", format_status_line("Maker Fills", "DISABLED", false));
    println!("{}", format_status_line("Gate Suite", "PASSED", true));
    
    // Demo 5: Metrics with thresholds
    println!("\n--- Metrics (green < 10, yellow < 100, red >= 100) ---");
    println!("{}", format_metric("Latency p50", 2.5, "ms", (10.0, 100.0)));
    println!("{}", format_metric("Latency p99", 45.0, "ms", (10.0, 100.0)));
    println!("{}", format_metric("Latency max", 250.0, "ms", (10.0, 100.0)));
    
    // Demo 6: Manual theme switching
    println!("\n--- Theme Switching Demo ---");
    println!("Switching to DARK theme...");
    ThemeMode::set_global(ThemeMode::Dark);
    println!("  {}This is DARK theme text{}", Theme::fg_cyan(), Theme::reset());
    
    println!("Switching to LIGHT theme...");
    ThemeMode::set_global(ThemeMode::Light);
    println!("  {}This is LIGHT theme text{}", Theme::fg_cyan(), Theme::reset());
    
    // Restore
    ThemeMode::set_global(current);
    
    println!("\n=== Demo Complete ===");
    println!("To use light mode in your terminal:");
    println!("  export BACKTEST_THEME=light");
    println!("Or set it in BacktestConfig:");
    println!("  ThemeMode::set_global(ThemeMode::Light);");
}
