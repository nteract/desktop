mod state;
mod ui;

use std::io::stdout;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use notebook_protocol::protocol::NotebookRequest;
use notebook_sync::connect::{connect_open, OpenResult};

use state::App;

pub async fn run(path: PathBuf) -> Result<()> {
    // Resolve socket path and blob paths for output resolution
    let socket_path = runtimed::daemon_paths::get_socket_path();
    let (blob_base_url, blob_store_path) =
        runtimed::daemon_paths::get_blob_paths_async(&socket_path).await;

    // Connect to daemon
    let OpenResult {
        handle,
        mut broadcast_rx,
        cells,
        ..
    } = connect_open(socket_path, path.clone(), "tui:runt").await?;

    // Build initial state
    let mut app = App::from_cells(
        cells,
        &path.display().to_string(),
        &blob_base_url,
        &blob_store_path,
    )
    .await;

    // Read initial runtime state
    if let Ok(rs) = handle.get_runtime_state() {
        app.update_runtime_state(rs);
    }

    // Install panic hook to restore terminal
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        crossterm::terminal::disable_raw_mode().ok();
        crossterm::execute!(std::io::stderr(), LeaveAlternateScreen).ok();
        original_hook(panic);
    }));

    // Setup terminal
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    // Event loop
    let mut crossterm_events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(33)); // ~30fps
    let mut refresh = tokio::time::interval(Duration::from_millis(500));

    loop {
        tokio::select! {
            maybe_event = crossterm_events.next() => {
                if let Some(Ok(Event::Key(key))) = maybe_event {
                    handle_key(key, &mut app, &handle).await;
                }
            }
            Some(broadcast) = broadcast_rx.recv() => {
                app.apply_broadcast(broadcast);
            }
            _ = refresh.tick() => {
                if let Ok(rs) = handle.get_runtime_state() {
                    app.update_runtime_state(rs);
                }
            }
            _ = tick.tick() => {
                terminal.draw(|f| ui::render(f, &app))?;
            }
        }

        if app.should_quit {
            break;
        }
    }

    // Cleanup
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(stdout(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

async fn handle_key(key: KeyEvent, app: &mut App, handle: &notebook_sync::DocHandle) {
    match (key.modifiers, key.code) {
        // Quit
        (_, KeyCode::Char('q')) if key.modifiers == KeyModifiers::NONE => {
            app.should_quit = true;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
            app.should_quit = true;
        }

        // Navigation
        (_, KeyCode::Char('j')) | (_, KeyCode::Down) => app.select_next(),
        (_, KeyCode::Char('k')) | (_, KeyCode::Up) => app.select_prev(),
        (_, KeyCode::Char('g')) if key.modifiers == KeyModifiers::NONE => app.select_first(),
        (_, KeyCode::Char('G')) | (KeyModifiers::SHIFT, KeyCode::Char('g')) => app.select_last(),

        // Execute: Ctrl+Enter
        (KeyModifiers::CONTROL, KeyCode::Enter) => {
            if let Some(cell_id) = app.selected_cell_id() {
                let cell_id = cell_id.to_string();
                // Ensure daemon has latest source
                let _ = handle.confirm_sync().await;
                let _ = handle
                    .send_request(NotebookRequest::ExecuteCell { cell_id })
                    .await;
            }
        }

        _ => {}
    }
}
