//! Theme Configuration for Backtest Output
//!
//! Supports both dark (AMOLED-black) and light themes for console output
//! and report generation.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};

// Global theme setting (atomic for thread safety)
static CURRENT_THEME: AtomicU8 = AtomicU8::new(0); // 0 = Dark, 1 = Light

/// Theme mode for backtest output
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ThemeMode {
    /// Dark theme (AMOLED-black) - default
    #[default]
    Dark,
    /// Light theme (white/cream background friendly)
    Light,
}

impl ThemeMode {
    /// Set the global theme
    pub fn set_global(mode: ThemeMode) {
        CURRENT_THEME.store(mode as u8, Ordering::SeqCst);
    }

    /// Get the current global theme
    pub fn current() -> ThemeMode {
        match CURRENT_THEME.load(Ordering::SeqCst) {
            0 => ThemeMode::Dark,
            1 => ThemeMode::Light,
            _ => ThemeMode::Dark,
        }
    }

    /// Get theme from environment variable BACKTEST_THEME
    pub fn from_env() -> ThemeMode {
        match std::env::var("BACKTEST_THEME").as_deref() {
            Ok("light") | Ok("Light") | Ok("LIGHT") => ThemeMode::Light,
            Ok("dark") | Ok("Dark") | Ok("DARK") => ThemeMode::Dark,
            _ => ThemeMode::Dark,
        }
    }

    /// Initialize theme from environment (call once at startup)
    pub fn init_from_env() {
        Self::set_global(Self::from_env());
    }
}

/// ANSI color codes
pub mod ansi {
    // Reset
    pub const RESET: &str = "\x1b[0m";

    // Dark theme colors (for dark/black backgrounds)
    pub mod dark {
        pub const FG_BRIGHT: &str = "\x1b[97m";      // Bright white
        pub const FG_DIM: &str = "\x1b[90m";         // Gray
        pub const FG_CYAN: &str = "\x1b[96m";        // Bright cyan
        pub const FG_GREEN: &str = "\x1b[92m";       // Bright green
        pub const FG_RED: &str = "\x1b[91m";         // Bright red
        pub const FG_YELLOW: &str = "\x1b[93m";      // Bright yellow
        pub const FG_PURPLE: &str = "\x1b[95m";      // Bright magenta
        pub const FG_BLUE: &str = "\x1b[94m";        // Bright blue

        pub const BOLD: &str = "\x1b[1m";
        pub const UNDERLINE: &str = "\x1b[4m";

        // Box drawing stays the same
        pub const BOX_TL: &str = "╔";
        pub const BOX_TR: &str = "╗";
        pub const BOX_BL: &str = "╚";
        pub const BOX_BR: &str = "╝";
        pub const BOX_H: &str = "═";
        pub const BOX_V: &str = "║";
        pub const BOX_CROSS: &str = "╬";
        pub const BOX_T_DOWN: &str = "╦";
        pub const BOX_T_UP: &str = "╩";
        pub const BOX_T_LEFT: &str = "╣";
        pub const BOX_T_RIGHT: &str = "╠";

        // Status icons
        pub const CHECK: &str = "✓";
        pub const CROSS: &str = "✗";
        pub const WARN: &str = "⚠️";
        pub const INFO: &str = "ℹ";
        pub const BULLET: &str = "•";
    }

    // Light theme colors (for white/light backgrounds)
    pub mod light {
        pub const FG_BRIGHT: &str = "\x1b[30m";      // Black
        pub const FG_DIM: &str = "\x1b[90m";         // Dark gray
        pub const FG_CYAN: &str = "\x1b[36m";        // Dark cyan
        pub const FG_GREEN: &str = "\x1b[32m";       // Dark green
        pub const FG_RED: &str = "\x1b[31m";         // Dark red
        pub const FG_YELLOW: &str = "\x1b[33m";      // Dark yellow/olive
        pub const FG_PURPLE: &str = "\x1b[35m";      // Dark magenta
        pub const FG_BLUE: &str = "\x1b[34m";        // Dark blue

        pub const BOLD: &str = "\x1b[1m";
        pub const UNDERLINE: &str = "\x1b[4m";

        // Box drawing (same characters, but we could use lighter variants)
        pub const BOX_TL: &str = "╔";
        pub const BOX_TR: &str = "╗";
        pub const BOX_BL: &str = "╚";
        pub const BOX_BR: &str = "╝";
        pub const BOX_H: &str = "═";
        pub const BOX_V: &str = "║";
        pub const BOX_CROSS: &str = "╬";
        pub const BOX_T_DOWN: &str = "╦";
        pub const BOX_T_UP: &str = "╩";
        pub const BOX_T_LEFT: &str = "╣";
        pub const BOX_T_RIGHT: &str = "╠";

        // Status icons (same, they're unicode)
        pub const CHECK: &str = "✓";
        pub const CROSS: &str = "✗";
        pub const WARN: &str = "⚠️";
        pub const INFO: &str = "ℹ";
        pub const BULLET: &str = "•";
    }
}

/// Theme-aware color helper
pub struct Theme;

impl Theme {
    /// Get foreground bright color
    pub fn fg_bright() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::FG_BRIGHT,
            ThemeMode::Light => ansi::light::FG_BRIGHT,
        }
    }

    /// Get foreground dim color
    pub fn fg_dim() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::FG_DIM,
            ThemeMode::Light => ansi::light::FG_DIM,
        }
    }

    /// Get cyan accent color
    pub fn fg_cyan() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::FG_CYAN,
            ThemeMode::Light => ansi::light::FG_CYAN,
        }
    }

    /// Get green accent color (success, good values)
    pub fn fg_green() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::FG_GREEN,
            ThemeMode::Light => ansi::light::FG_GREEN,
        }
    }

    /// Get red accent color (error, bad values)
    pub fn fg_red() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::FG_RED,
            ThemeMode::Light => ansi::light::FG_RED,
        }
    }

    /// Get yellow accent color (warning, caution)
    pub fn fg_yellow() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::FG_YELLOW,
            ThemeMode::Light => ansi::light::FG_YELLOW,
        }
    }

    /// Get purple accent color
    pub fn fg_purple() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::FG_PURPLE,
            ThemeMode::Light => ansi::light::FG_PURPLE,
        }
    }

    /// Get blue accent color
    pub fn fg_blue() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::FG_BLUE,
            ThemeMode::Light => ansi::light::FG_BLUE,
        }
    }

    /// Get bold modifier
    pub fn bold() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::BOLD,
            ThemeMode::Light => ansi::light::BOLD,
        }
    }

    /// Get reset code
    pub fn reset() -> &'static str {
        ansi::RESET
    }

    /// Get check mark
    pub fn check() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::CHECK,
            ThemeMode::Light => ansi::light::CHECK,
        }
    }

    /// Get cross mark
    pub fn cross() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::CROSS,
            ThemeMode::Light => ansi::light::CROSS,
        }
    }

    /// Get warning icon
    pub fn warn() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::WARN,
            ThemeMode::Light => ansi::light::WARN,
        }
    }

    /// Get box top-left corner
    pub fn box_tl() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::BOX_TL,
            ThemeMode::Light => ansi::light::BOX_TL,
        }
    }

    /// Get box top-right corner
    pub fn box_tr() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::BOX_TR,
            ThemeMode::Light => ansi::light::BOX_TR,
        }
    }

    /// Get box bottom-left corner
    pub fn box_bl() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::BOX_BL,
            ThemeMode::Light => ansi::light::BOX_BL,
        }
    }

    /// Get box bottom-right corner
    pub fn box_br() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::BOX_BR,
            ThemeMode::Light => ansi::light::BOX_BR,
        }
    }

    /// Get box horizontal line
    pub fn box_h() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::BOX_H,
            ThemeMode::Light => ansi::light::BOX_H,
        }
    }

    /// Get box vertical line
    pub fn box_v() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::BOX_V,
            ThemeMode::Light => ansi::light::BOX_V,
        }
    }

    /// Get box T-right (left side junction)
    pub fn box_t_right() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::BOX_T_RIGHT,
            ThemeMode::Light => ansi::light::BOX_T_RIGHT,
        }
    }

    /// Get box T-left (right side junction)
    pub fn box_t_left() -> &'static str {
        match ThemeMode::current() {
            ThemeMode::Dark => ansi::dark::BOX_T_LEFT,
            ThemeMode::Light => ansi::light::BOX_T_LEFT,
        }
    }
}

/// Format a themed banner (80-char wide box)
pub fn format_banner(title: &str, content_lines: &[&str], style: BannerStyle) -> String {
    let mut out = String::new();
    let width = 78; // Inner width (80 - 2 for borders)

    // Top border
    out.push_str(&format!(
        "{}{}{}{}",
        style.color(),
        Theme::box_tl(),
        Theme::box_h().repeat(width),
        Theme::box_tr()
    ));
    out.push_str(Theme::reset());
    out.push('\n');

    // Title line
    let title_padded = format!("  {}  ", title);
    let padding = width.saturating_sub(title_padded.len());
    out.push_str(&format!(
        "{}{}{}{}{}{}{}",
        style.color(),
        Theme::box_v(),
        Theme::reset(),
        style.title_color(),
        Theme::bold(),
        title_padded,
        " ".repeat(padding)
    ));
    out.push_str(&format!("{}{}{}", Theme::reset(), style.color(), Theme::box_v()));
    out.push_str(Theme::reset());
    out.push('\n');

    // Separator
    out.push_str(&format!(
        "{}{}{}{}",
        style.color(),
        Theme::box_t_right(),
        Theme::box_h().repeat(width),
        Theme::box_t_left()
    ));
    out.push_str(Theme::reset());
    out.push('\n');

    // Content lines
    for line in content_lines {
        let line_len = line.chars().count();
        let padding = width.saturating_sub(line_len + 2); // +2 for leading spaces
        out.push_str(&format!(
            "{}{}{}  {}{}{}{}",
            style.color(),
            Theme::box_v(),
            Theme::reset(),
            line,
            " ".repeat(padding),
            style.color(),
            Theme::box_v()
        ));
        out.push_str(Theme::reset());
        out.push('\n');
    }

    // Bottom border
    out.push_str(&format!(
        "{}{}{}{}",
        style.color(),
        Theme::box_bl(),
        Theme::box_h().repeat(width),
        Theme::box_br()
    ));
    out.push_str(Theme::reset());
    out.push('\n');

    out
}

/// Banner style presets
#[derive(Debug, Clone, Copy)]
pub enum BannerStyle {
    /// Success/production-grade (green)
    Success,
    /// Warning/exploratory (yellow)
    Warning,
    /// Error/simulation-only (red)
    Error,
    /// Info/neutral (cyan)
    Info,
    /// Primary heading (purple)
    Primary,
}

impl BannerStyle {
    fn color(&self) -> &'static str {
        match self {
            BannerStyle::Success => Theme::fg_green(),
            BannerStyle::Warning => Theme::fg_yellow(),
            BannerStyle::Error => Theme::fg_red(),
            BannerStyle::Info => Theme::fg_cyan(),
            BannerStyle::Primary => Theme::fg_purple(),
        }
    }

    fn title_color(&self) -> &'static str {
        match self {
            BannerStyle::Success => Theme::fg_green(),
            BannerStyle::Warning => Theme::fg_yellow(),
            BannerStyle::Error => Theme::fg_red(),
            BannerStyle::Info => Theme::fg_cyan(),
            BannerStyle::Primary => Theme::fg_purple(),
        }
    }
}

/// Format a status line with theme colors
pub fn format_status_line(label: &str, value: &str, is_good: bool) -> String {
    let status_color = if is_good { Theme::fg_green() } else { Theme::fg_red() };
    let icon = if is_good { Theme::check() } else { Theme::cross() };
    format!(
        "{}{}  {}{}: {}{}{}",
        status_color,
        icon,
        Theme::fg_dim(),
        label,
        Theme::fg_bright(),
        value,
        Theme::reset()
    )
}

/// Format a metric value with appropriate coloring
pub fn format_metric(label: &str, value: f64, unit: &str, thresholds: (f64, f64)) -> String {
    let (warn_threshold, error_threshold) = thresholds;
    let color = if value < warn_threshold {
        Theme::fg_green()
    } else if value < error_threshold {
        Theme::fg_yellow()
    } else {
        Theme::fg_red()
    };

    format!(
        "{}{}:{} {}{:.2}{} {}",
        Theme::fg_dim(),
        label,
        Theme::reset(),
        color,
        value,
        Theme::reset(),
        unit
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_theme_mode_default() {
        assert_eq!(ThemeMode::default(), ThemeMode::Dark);
    }

    #[test]
    fn test_theme_set_and_get() {
        ThemeMode::set_global(ThemeMode::Light);
        assert_eq!(ThemeMode::current(), ThemeMode::Light);

        ThemeMode::set_global(ThemeMode::Dark);
        assert_eq!(ThemeMode::current(), ThemeMode::Dark);
    }

    #[test]
    fn test_banner_format() {
        ThemeMode::set_global(ThemeMode::Dark);
        let banner = format_banner(
            "TEST BANNER",
            &["Line 1", "Line 2"],
            BannerStyle::Info,
        );
        assert!(banner.contains("TEST BANNER"));
        assert!(banner.contains("Line 1"));
        assert!(banner.contains("╔"));
        assert!(banner.contains("╚"));
    }

    #[test]
    fn test_status_line_format() {
        ThemeMode::set_global(ThemeMode::Dark);
        let good = format_status_line("Status", "OK", true);
        let bad = format_status_line("Status", "FAIL", false);
        assert!(good.contains("OK"));
        assert!(bad.contains("FAIL"));
    }
}
