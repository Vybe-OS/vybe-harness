use ratatui::{Frame, layout::{Constraint, Layout, Rect}, style::{Color, Modifier, Style}, text::{Line, Span}, widgets::{Block, Borders, Paragraph}};
use serde::{Deserialize, Serialize};

use crate::core::screen::Screen;

#[derive(Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

pub struct MessagesScreen {
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub scroll: u16,
    pub is_streaming: bool,
}

impl MessagesScreen {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            input: String::new(),
            scroll: 0,
            is_streaming: false,
        }
    }
}

impl Screen for MessagesScreen {
    fn render(&mut self, frame: &mut Frame, area: Rect) {
        let [messages_area, input_area] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(3)
        ])
        .areas(area);

        let messages: Vec<Line> = self.messages.iter().map(|msg| {
            Line::from(vec![
                Span::styled(
                    format!("{}: ", msg.role),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                ),
                Span::raw(&msg.content)
            ])
        }).collect();

        let messages_paragraph = Paragraph::new(messages)
            .block(Block::default().borders(Borders::ALL).title("Messages"))
            .scroll((self.scroll, 0));
        frame.render_widget(messages_paragraph, messages_area);

        let input_paragraph = Paragraph::new(self.input.as_str())
            .block(Block::default().borders(Borders::ALL).title("Input"));
        frame.render_widget(input_paragraph, input_area);
    }

    fn handle_key(&mut self, key: ratatui::crossterm::event::KeyEvent) -> crate::core::screen::Action {
        match key.code {
            ratatui::crossterm::event::KeyCode::Char(c) => {
                self.input.push(c);
                crate::core::screen::Action::None
            }
            ratatui::crossterm::event::KeyCode::Backspace => {
                self.input.pop();
                crate::core::screen::Action::None
            }
            ratatui::crossterm::event::KeyCode::Enter => {
                let message = ChatMessage {
                    role: "user".to_string(),
                    content: self.input.clone(),
                };
                self.messages.push(message);
                self.input.clear();
                crate::core::screen::Action::None
            }
            _ => crate::core::screen::Action::None,
        }
    }
}