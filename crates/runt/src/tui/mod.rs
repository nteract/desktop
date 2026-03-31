mod state;
mod ui;

use std::io::stdout;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyModifiers, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use notebook_doc::presence::{encode_cursor_update_labeled, CursorPosition};
use notebook_protocol::protocol::NotebookRequest;
use notebook_sync::connect::{connect_open, OpenResult};

use state::App;

/// Get username for actor label: "$USER:console"
fn actor_label() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string());
    format!("{}:console", user)
}

pub async fn run(path: PathBuf) -> Result<()> {
    let label = actor_label();

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
    } = connect_open(socket_path, path.clone(), &label).await?;

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

    // Get our peer ID for presence broadcasts
    let peer_id = handle
        .get_actor_id()
        .unwrap_or_else(|_| label.clone());

    // Install panic hook to restore terminal
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        crossterm::execute!(std::io::stderr(), PopKeyboardEnhancementFlags).ok();
        crossterm::terminal::disable_raw_mode().ok();
        crossterm::execute!(std::io::stderr(), LeaveAlternateScreen).ok();
        original_hook(panic);
    }));

    // Setup terminal
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(stdout(), EnterAlternateScreen)?;
    // Enable kitty keyboard protocol for Shift+Enter detection.
    // Silently ignored if the terminal doesn't support it.
    let keyboard_enhanced = crossterm::execute!(
        stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
    .is_ok();
    eprintln!("[tui] keyboard_enhanced={keyboard_enhanced}");
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
    terminal.clear()?;

    // Subscribe to document changes (fires on both local and remote CRDT mutations)
    let mut snapshot_rx = handle.subscribe();

    // Event loop
    let mut crossterm_events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(33)); // ~30fps
    let mut refresh = tokio::time::interval(Duration::from_millis(500));

    loop {
        tokio::select! {
            maybe_event = crossterm_events.next() => {
                if let Some(Ok(Event::Key(key))) = maybe_event {
                    // Debug: log Enter-related key events to stderr
                    if key.code == KeyCode::Enter {
                        eprintln!("[tui] Enter key: modifiers={:?} kind={:?} state={:?}", key.modifiers, key.kind, key.state);
                    }
                    handle_key(key, &mut app, &handle, &peer_id, &label).await;
                }
            }
            Some(broadcast) = broadcast_rx.recv() => {
                app.apply_broadcast(broadcast);
            }
            Ok(()) = snapshot_rx.changed() => {
                let snapshot = snapshot_rx.borrow_and_update().clone();
                app.apply_snapshot(snapshot, &blob_base_url, &blob_store_path).await;
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
    if keyboard_enhanced {
        crossterm::execute!(stdout(), PopKeyboardEnhancementFlags).ok();
    }
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(stdout(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

async fn handle_key(
    key: KeyEvent,
    app: &mut App,
    handle: &notebook_sync::DocHandle,
    peer_id: &str,
    label: &str,
) {
    use state::Mode;

    match app.mode {
        Mode::Normal => handle_normal_key(key, app, handle).await,
        Mode::Edit => handle_edit_key(key, app, handle).await,
    }

    // Emit cursor presence when in edit mode
    if app.mode == Mode::Edit {
        emit_cursor_presence(app, handle, peer_id, label).await;
    }
}

async fn emit_cursor_presence(
    app: &App,
    handle: &notebook_sync::DocHandle,
    peer_id: &str,
    label: &str,
) {
    if let Some(cell_id) = app.selected_cell_id() {
        let pos = CursorPosition {
            cell_id: cell_id.to_string(),
            line: app.cursor.line as u32,
            column: app.cursor.col as u32,
        };
        let data = encode_cursor_update_labeled(peer_id, Some(label), &pos);
        let _ = handle.send_presence(data).await;
    }
}

/// Execute the selected cell: confirm sync, send request, move down.
/// Spawns the actual request as a background task so the TUI doesn't block.
fn execute_selected(app: &mut App, handle: &notebook_sync::DocHandle) {
    if let Some(cell_id) = app.selected_cell_id() {
        let cell_id = cell_id.to_string();
        let handle = handle.clone();
        tokio::spawn(async move {
            if let Err(e) = handle.confirm_sync().await {
                eprintln!("[tui] confirm_sync failed: {e}");
            }
            match handle
                .send_request(NotebookRequest::ExecuteCell {
                    cell_id: cell_id.clone(),
                })
                .await
            {
                Ok(_resp) => {}
                Err(e) => {
                    eprintln!("[tui] execute request failed for {cell_id}: {e}");
                }
            }
        });
        app.select_next();
    }
}

async fn handle_normal_key(key: KeyEvent, app: &mut App, handle: &notebook_sync::DocHandle) {
    match (key.modifiers, key.code) {
        // Quit
        (KeyModifiers::NONE, KeyCode::Char('q')) => {
            app.should_quit = true;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
            app.should_quit = true;
        }

        // Navigation
        (_, KeyCode::Char('j')) | (KeyModifiers::NONE, KeyCode::Down) => app.select_next(),
        (_, KeyCode::Char('k')) | (KeyModifiers::NONE, KeyCode::Up) => app.select_prev(),
        (KeyModifiers::NONE, KeyCode::Char('g')) => app.select_first(),
        (KeyModifiers::SHIFT, KeyCode::Char('G')) => app.select_last(),

        // Execute: Shift+Enter
        (KeyModifiers::SHIFT, KeyCode::Enter) => {
            execute_selected(app, handle);
        }

        // Enter edit mode: Enter or i
        (KeyModifiers::NONE, KeyCode::Enter) => {
            app.enter_edit_mode();
        }
        (KeyModifiers::NONE, KeyCode::Char('i')) => {
            app.enter_edit_mode();
        }

        _ => {}
    }
}

async fn handle_edit_key(key: KeyEvent, app: &mut App, handle: &notebook_sync::DocHandle) {
    use state::CursorDir;

    // Helper: apply a splice to the CRDT if the edit produced one
    let apply_splice =
        |splice: Option<state::Splice>, app: &App, handle: &notebook_sync::DocHandle| {
            if let (Some(splice), Some(cell_id)) = (splice, app.selected_cell_id()) {
                let _ =
                    handle.splice_source(cell_id, splice.index, splice.delete_count, &splice.text);
            }
        };

    match (key.modifiers, key.code) {
        // Exit edit mode
        (_, KeyCode::Esc) => {
            app.exit_edit_mode();
        }

        // Shift+Enter: save + execute + move down
        (KeyModifiers::SHIFT, KeyCode::Enter) => {
            if let Some(_cell_id) = app.exit_edit_mode() {
                execute_selected(app, handle);
            }
        }

        // Newline
        (KeyModifiers::NONE, KeyCode::Enter) => {
            let splice = app.edit_insert_newline();
            apply_splice(splice, app, handle);
        }

        // Backspace
        (_, KeyCode::Backspace) => {
            let splice = app.edit_backspace();
            apply_splice(splice, app, handle);
        }

        // Delete
        (_, KeyCode::Delete) => {
            let splice = app.edit_delete();
            apply_splice(splice, app, handle);
        }

        // Cursor movement (no CRDT change)
        (_, KeyCode::Left) => app.edit_move_cursor(CursorDir::Left),
        (_, KeyCode::Right) => app.edit_move_cursor(CursorDir::Right),
        (_, KeyCode::Up) => app.edit_move_cursor(CursorDir::Up),
        (_, KeyCode::Down) => app.edit_move_cursor(CursorDir::Down),
        (_, KeyCode::Home) => app.edit_move_cursor(CursorDir::Home),
        (_, KeyCode::End) => app.edit_move_cursor(CursorDir::End),

        // Tab → 4 spaces
        (_, KeyCode::Tab) => {
            let splice = app.edit_insert_str("    ");
            apply_splice(splice, app, handle);
        }

        // Regular character input
        (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(c)) => {
            let splice = app.edit_insert_char(c);
            apply_splice(splice, app, handle);
        }

        _ => {}
    }
}
