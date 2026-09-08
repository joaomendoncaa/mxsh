use ratatui::layout::Rect;
use ratatui::style::Color;

pub const HOVER_BG: Color = Color::Red;
pub const HOVER_FG: Color = Color::White;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    CommandMode,
    ExitCommandMode,
    HelpMode,
    ExitHelp,
    KillSession,
    OpenDetached,
    WorktreeNew,
    WorktreeDelete,
}

#[derive(Debug, Clone)]
pub struct Clickable {
    pub rect: Rect,
    pub action: Action,
}

impl Clickable {
    pub fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.rect.x
            && col < self.rect.x.saturating_add(self.rect.width)
            && row >= self.rect.y
            && row < self.rect.y.saturating_add(self.rect.height)
    }
}
