//! Async event loop: crossterm events + progress messages

use std::io;
use std::time::Duration;

use crossterm::event::{self, Event};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc;

use super::app::AppState;
use super::draw::draw_ui;
use super::keybindings::{handle_key, handle_mouse};
use super::message::AppMessage;

/// Run the main TUI event loop
pub async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut AppState,
    tx: mpsc::Sender<AppMessage>,
    mut rx: mpsc::Receiver<AppMessage>,
) -> io::Result<()> {
    loop {
        // 1. Refresh items from the manager
        app.refresh_items();
        app.clamp_selection();
        app.clear_expired_status();

        // 2. Render
        terminal.draw(|f| draw_ui(f, app))?;

        // 3. Check quit
        if app.should_quit {
            // Save queue before exiting
            app.manager.save_queue(app.config.conversion.persist_queue).ok();
            break;
        }

        // 4. Drain async messages
        while let Ok(msg) = rx.try_recv() {
            handle_message(app, msg);
        }

        // 5. Poll for crossterm events (100ms timeout for responsive UI updates)
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => handle_key(app, key, &tx),
                Event::Mouse(mouse) => handle_mouse(app, mouse, &tx),
                Event::Resize(_, _) => {} // redraw handled by loop
                _ => {}
            }
        }
    }

    Ok(())
}

/// Handle async messages from background tasks
fn handle_message(app: &mut AppState, msg: AppMessage) {
    match msg {
        AppMessage::ConversionProgress { item_id, status } => {
            app.manager.update_item_status(&item_id, status, 0.0);
        }
        AppMessage::ConversionComplete { completed, failed } => {
            app.processing_active = false;
            if failed > 0 {
                app.set_status(format!(
                    "Conversion done: {} completed, {} failed",
                    completed, failed
                ));
            } else {
                app.set_status(format!("Conversion complete: {} files", completed));
            }
            // Save queue after completion
            app.manager.save_queue(app.config.conversion.persist_queue).ok();
        }
        AppMessage::ConversionError { message } => {
            app.processing_active = false;
            app.set_status(format!("Error: {}", message));
        }
        AppMessage::FilesScanned { paths } => {
            let mut options = crate::convert::ConversionOptions::default();
            options.append_lineage_to_comment = app.config.conversion.append_lineage_to_comment;
            options.write_log_file = app.config.conversion.write_log_file;
            options.generate_cue_files = app.config.conversion.generate_cue_files;
            options.cue_generation_mode = app.config.conversion.cue_generation_mode.clone();

            let mut count = 0;
            for path in paths {
                if app.manager.add_file_blocking(path, options.clone()).is_ok() {
                    count += 1;
                }
            }
            app.set_status(format!("Added {} files", count));
            app.manager.save_queue(app.config.conversion.persist_queue).ok();
        }
        AppMessage::StatusMessage(msg) => {
            app.set_status(msg);
        }
        AppMessage::Redraw => {} // Just triggers a redraw via the loop
    }
}
