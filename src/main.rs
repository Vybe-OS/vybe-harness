mod app;
mod core;
mod screens;
mod components;

use anyhow::{Result};
use ratatui::{
    DefaultTerminal, crossterm::event::{self, Event, KeyEventKind}
};

use crate::app::App;

fn main() -> Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal) -> Result<()> {
    let mut app = App::new();

    loop {
        terminal.draw(|frame| App::render_ui(frame, &mut app))?;

        if event::poll(std::time::Duration::from_millis(10))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    let action = app.screen.handle_key(key);
                    if app.apply(action) {
                        return Ok(());
                    }
                }
            }
        }

        let action = app.screen.tick();
        if app.apply(action) {
            return Ok(());
        }
    }
}