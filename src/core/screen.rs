use ratatui::{Frame, layout::Rect, crossterm::event::KeyEvent};

pub enum Action {
    None,
    Goto(Box<dyn Screen>),
    Quit,
}

pub trait Screen {
    fn render(&mut self, frame: &mut Frame, area: Rect);
    fn handle_key(&mut self, key: KeyEvent) -> Action;
    fn tick(&mut self) -> Action {
        Action::None
    }
}