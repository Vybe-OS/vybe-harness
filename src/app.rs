use ratatui::{Frame, widgets::{Block}};
use crate::{core::screen::{Action, Screen}, screens::welcome_screen::WelcomeScreen};

pub struct App {
    pub screen: Box<dyn Screen>,
}

impl App {
    pub fn new() -> Self {
        App {
            screen: Box::new(WelcomeScreen::new()),
        }
    }

    pub fn apply(&mut self, action: Action) -> bool {
        match action {
            Action::None => {},
            Action::Goto(next) => {
                self.screen = next;
            },
            Action::Quit => return true,
        }

        false
    }

    pub fn render_ui(frame: &mut Frame, app: &mut App) {
        let body = Block::default();
        let inner_body = body.inner(frame.area());
        frame.render_widget(body, frame.area());

        app.screen.render(frame, inner_body);
    }
}