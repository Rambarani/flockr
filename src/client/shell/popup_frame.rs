//! Shared chrome for the client-side popups (which-key, command palette, agent
//! preview, agent dashboard). Colors come from the active palette only.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Widget};

use crate::app::state::Palette;

/// Clear `area`, draw a bordered panel with `title`, and return the inner area.
pub(super) fn draw_panel(
    buffer: &mut Buffer,
    area: Rect,
    title: &str,
    palette: &Palette,
) -> Option<Rect> {
    let area = area.intersection(buffer.area);
    if area.width < 3 || area.height < 3 {
        return None;
    }
    let base = Style::default().fg(palette.text).bg(palette.panel_bg);
    Clear.render(area, buffer);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(palette.accent).bg(palette.panel_bg))
        .style(base);
    if !title.is_empty() {
        block = block.title(Span::styled(
            format!(" {title} "),
            Style::default().fg(palette.accent).bg(palette.panel_bg),
        ));
    }
    let inner = block.inner(area);
    block.render(area, buffer);
    Some(inner)
}

pub(super) fn base_style(palette: &Palette) -> Style {
    Style::default().fg(palette.text).bg(palette.panel_bg)
}

pub(super) fn dim_style(palette: &Palette) -> Style {
    Style::default().fg(palette.overlay0).bg(palette.panel_bg)
}

pub(super) fn key_style(palette: &Palette) -> Style {
    Style::default()
        .fg(palette.accent)
        .bg(palette.panel_bg)
        .add_modifier(Modifier::BOLD)
}

pub(super) fn heading_style(palette: &Palette) -> Style {
    Style::default()
        .fg(palette.mauve)
        .bg(palette.panel_bg)
        .add_modifier(Modifier::BOLD)
}

/// Display text of one buffer row, for assertions and smoke captures.
#[cfg(test)]
pub(super) fn buffer_row(buffer: &Buffer, y: u16) -> String {
    (buffer.area.x..buffer.area.right())
        .map(|x| buffer[(x, y)].symbol().to_owned())
        .collect()
}
