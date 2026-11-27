use ratatui::{
    buffer::Buffer, crossterm::event::{KeyCode, KeyEvent, KeyModifiers}, layout::{Position, Rect}, style::{Color, Modifier, Style}, text::Span, widgets::{Block, BorderType, Borders, Padding, Paragraph, StatefulWidget, Widget},
};

#[derive(Default)]
pub struct InputState {
    value: String,
    cursor: usize,
    pub cursor_pos: Option<Position>,
}

impl InputState {
    pub fn value(&self) -> &str {
        &self.value
    }

    fn byte_index(&self) -> usize {
        self.value
            .char_indices()
            .nth(self.cursor)
            .map_or(self.value.len(), |(i, _)| i)
    }

    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        let len = self.value.chars().count();
        match key.code {
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let i = self.byte_index();
                self.value.insert(i, c);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                let i = self.byte_index();
                self.value.remove(i);
            }
            KeyCode::Delete if self.cursor < len => {
                let i = self.byte_index();
                self.value.remove(i);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(len),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = len,
            _ => return false,
        }
        true
    }
}

pub struct Input<'a> {
    placeholder: &'a str,
    bg_color: Option<Color>,
    focused: bool,
}

impl<'a> Input<'a> {
    pub fn new(placeholder: &'a str) -> Self {
        Self {
            placeholder,
            bg_color: None,
            focused: false,
        }
    }


    pub fn bg(mut self, color: Color) -> Self {
        self.bg_color = Some(color);
        self
    }

    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }
}

impl StatefulWidget for Input<'_> {
    type State = InputState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut InputState) {
        let mut block_style = Style::new();
        if let Some(bg) = self.bg_color {
            block_style = block_style.bg(bg);
        }

        let block = Block::default()
            .style(block_style)
            .borders(Borders::LEFT)
            .border_type(BorderType::Thick)
            .padding(Padding::new(2, 2, 1, 1));

        let inner = block.inner(area);

        let content = if state.value.is_empty() {
            Span::styled(
                self.placeholder,
                Style::new()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )
        } else {
            Span::styled(&state.value, Style::new().fg(Color::White))
        };

        Paragraph::new(content).block(block).render(area, buf);

        state.cursor_pos = self.focused.then(|| {
            // Clamp agar kursor tidak keluar batas kanan kotak saat teks kepanjangan
            let max_offset = inner.width.saturating_sub(1);
            let offset = (state.cursor as u16).min(max_offset);
            Position::new(inner.x + offset, inner.y)
        });
    }
}