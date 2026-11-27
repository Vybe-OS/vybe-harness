use ratatui::{Frame, crossterm::event::{KeyCode, KeyEvent, KeyModifiers}, layout::{Alignment, Constraint, Layout, Rect}, style::{Color, Modifier, Style, Styled}, text::{Line, Span, Text}, widgets::{Block, Paragraph}};
use tui_popup::Popup;

use crate::{components::input::{Input, InputState}, core::screen::{Action, Screen}, screens::messages_screen::MessagesScreen};

pub struct WelcomeScreen {
    pub prompt_field: InputState,
    show_popup: bool,
}

impl WelcomeScreen {
    pub fn new() -> Self {
        Self {
            prompt_field: Default::default(),
            show_popup: false,
        }
    }

    fn render_popup(frame: &mut Frame) {
        let popup = Popup::new("Press any key to exit");
        frame.render_widget(popup, frame.area());
    }
}

impl Screen for WelcomeScreen {
    fn render(&mut self, frame: &mut Frame, area: Rect) {
        let ascii_logo = include_str!("../assets/logo.txt");
        let [_, content_area, _, footer_text] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(12),
            Constraint::Fill(1),
            Constraint::Length(1)
        ])
        .areas(area);

        let [_, prompt_area, _] = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(80),
            Constraint::Fill(1)
        ])
        .areas(content_area);

        let [logo_area, _, field_area] = Layout::vertical([
            Constraint::Length(5),
            Constraint::Length(2),
            Constraint::Length(5)
        ])
        .areas(prompt_area);

        let [prompt_field_area, _, prompt_foot_area] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Length(1)
        ])
        .areas(field_area);

        let logo = Paragraph::new(ascii_logo)
            .block(Block::default()).alignment(Alignment::Center).style(Style::new().add_modifier(Modifier::BOLD));
        frame.render_widget(logo, logo_area);

        let input_prompt = Input::new("How can i help you today?")
            .focused(true)
            .bg(Color::Rgb(30, 30, 30));
        frame.render_stateful_widget(input_prompt, prompt_field_area, &mut self.prompt_field);
        if let Some(pos) = self.prompt_field.cursor_pos {
            frame.set_cursor_position(pos);
        }

        let prompt_foot_text = Text::from(vec![
            Line::from(vec![
                Span::styled("ctrl+p", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::raw(" settings"),
            ])
        ]);
        let prompt_foot = Paragraph::new(prompt_foot_text)
            .block(Block::default())
            .alignment(Alignment::Right)
            .style(Style::new().fg(Color::DarkGray));
        frame.render_widget(prompt_foot, prompt_foot_area);

        let main_footer_text = Text::from(vec![
            Line::from(vec![
                Span::raw("press "),
                Span::styled("esc", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::raw(" to exit"),
            ])
        ]);
        let footer = Paragraph::new(main_footer_text)
            .block(Block::default())
            .alignment(Alignment::Center)
            .style(Style::new().fg(Color::DarkGray));
        frame.render_widget(footer, footer_text);

        if self.show_popup {
            Self::render_popup(frame);
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Action {
        if self.prompt_field.handle_key(key) {
            return Action::None;
        }

        if key.code == KeyCode::Char('p') && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.show_popup = !self.show_popup;
            return Action::None;
        }

        match key.code {
            KeyCode::Enter => {
                let input = self.prompt_field.value();
                if !input.is_empty() {
                    self.prompt_field.clear();
                    return Action::Goto(Box::new(MessagesScreen::new()));
                }
                Action::None
            }
            KeyCode::Esc if self.show_popup => {
                self.show_popup = false;
                Action::None
            }
            KeyCode::Esc => Action::Quit,
            _ => Action::None,
        }
    }
}