use std::io;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, MouseEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    Terminal,
};

mod types;
mod ui;
mod events;
mod presets;


use types::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create wizard
    let mut wizard = SimpleWizard::new();
    
    // Main loop
    loop {
        let mut mouse_areas = ui::MouseAreas::new();
        terminal.draw(|f| {
            mouse_areas = ui::draw_wizard(f, &wizard);
        })?;

        match event::read()? {
            Event::Key(key) => {
                // Let wizard handle the key first (including Escape for help)
                if wizard.handle_key(key) {
                    if wizard.should_exit {
                        break;  // Exit when Cancel is pressed
                    }
                    if wizard.should_start_conversion {
                        // In standalone mode, just print the settings and exit
                        disable_raw_mode()?;
                        execute!(
                            terminal.backend_mut(),
                            LeaveAlternateScreen,
                            DisableMouseCapture
                        )?;
                        terminal.show_cursor()?;
                        
                        println!("\nConversion settings:");
                        println!("{:#?}", wizard.extract_settings());
                        break;
                    }
                }
            }
            Event::Mouse(mouse) => {
                match mouse.kind {
                    MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                        let button_id = mouse_areas.get_button_at(mouse.column, mouse.row);
                        wizard.handle_mouse(mouse, button_id);
                        
                        if wizard.should_exit {
                            // Debug logging
                            use std::fs::OpenOptions;
                            use std::io::Write;
                            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("wizard_areas.log") {
                                let _ = writeln!(file, "Main loop: Exiting wizard due to should_exit flag");
                            }
                            break; // Exit the wizard when Cancel is clicked
                        }
                        
                        if wizard.should_start_conversion {
                            // In standalone mode, just print the settings and exit
                            disable_raw_mode()?;
                            execute!(
                                terminal.backend_mut(),
                                LeaveAlternateScreen,
                                DisableMouseCapture
                            )?;
                            terminal.show_cursor()?;
                            
                            println!("\nConversion settings:");
                            println!("{:#?}", wizard.extract_settings());
                            break;
                        }
                    }
                    MouseEventKind::Moved => {
                        // Track hover state
                        wizard.hovered_button = mouse_areas.get_button_at(mouse.column, mouse.row);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}