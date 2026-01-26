//! TUI Renderer
//!
//! Handles terminal setup, event loop, and rendering.

use super::app::PerfApp;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::{self, Stdout};
use std::time::{Duration, Instant};

pub type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

/// Initialize terminal for TUI
pub fn init_terminal() -> io::Result<TuiTerminal> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend)
}

/// Restore terminal to normal state
pub fn restore_terminal(terminal: &mut TuiTerminal) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

/// Main event loop for the performance monitor
pub fn run_event_loop(terminal: &mut TuiTerminal, app: &mut PerfApp) -> io::Result<()> {
    let tick_rate = Duration::from_millis(16); // ~60 FPS
    let mut last_tick = Instant::now();

    while app.running {
        // Render
        terminal.draw(|f| app.render(f))?;

        // Handle events with timeout
        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if crossterm::event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                app.handle_key(key);
            }
        }

        // Tick update
        if last_tick.elapsed() >= tick_rate {
            app.tick();
            last_tick = Instant::now();
        }
    }

    Ok(())
}

/// Run the performance monitor in standalone mode
pub fn run_standalone(backend_url: &str) -> io::Result<()> {
    let mut terminal = init_terminal()?;
    let mut app = PerfApp::new(backend_url.to_string());

    let result = run_event_loop(&mut terminal, &mut app);

    // Restore terminal regardless of result
    restore_terminal(&mut terminal)?;

    result
}

/// Run with connection to live backend
pub async fn run_with_backend(backend_url: &str) -> io::Result<()> {
    use tokio::sync::mpsc;

    let mut terminal = init_terminal()?;
    let mut app = PerfApp::new(backend_url.to_string());

    // Spawn backend polling task
    let (tx, mut rx) = mpsc::channel(100);
    let url = backend_url.to_string();

    tokio::spawn(async move {
        loop {
            // Poll backend performance endpoint
            if let Ok(response) = reqwest::get(&format!("{}/api/performance/report", url)).await {
                if let Ok(report) = response.json::<serde_json::Value>().await {
                    let _ = tx.send(report).await;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });

    let tick_rate = Duration::from_millis(16);
    let mut last_tick = Instant::now();

    while app.running {
        terminal.draw(|f| app.render(f))?;

        // Check for backend updates
        if let Ok(report) = rx.try_recv() {
            app.backend_connected = true;
            // Update metrics from report
            if let Some(pipeline) = report.get("pipeline") {
                // Parse and update metrics
            }
        }

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if crossterm::event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                app.handle_key(key);
            }
        }

        if last_tick.elapsed() >= tick_rate {
            app.tick();
            last_tick = Instant::now();
        }
    }

    restore_terminal(&mut terminal)?;
    Ok(())
}
