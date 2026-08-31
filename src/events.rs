use crate::clickable::Action;
use crate::config::Config;
use crate::picker::{Mode, Picker};
use crate::tmux;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

impl Picker {
    pub fn handle_input(&mut self, key: KeyEvent) {
        // HelpEditing has highest priority: editing buffer
        if self.mode == Mode::HelpEditing {
            if Config::key_matches(&self.config.bind_help_exit, key) {
                self.cancel_help_edit();
                return;
            }
            if Config::key_matches(&self.config.bind_help_enter, key) {
                self.commit_help_edit();
                return;
            }
            if self.edit_input(key).is_some() {
                return;
            }
            return;
        }

        if self.mode == Mode::Help {
            // Ignore bind keys in Help ( : disabled, ? already in help)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
                && (key.code == KeyCode::Char(self.config.bind_command_mode)
                    || key.code == KeyCode::Char(self.config.bind_help))
            {
                return;
            }
            if Config::key_matches(&self.config.bind_nav_up, key) {
                self.help_move_cursor(-1);
                return;
            }
            if Config::key_matches(&self.config.bind_nav_down, key) {
                self.help_move_cursor(1);
                return;
            }
            if Config::key_matches(&self.config.bind_nav_page_up, key) {
                self.help_move_cursor(-5);
                return;
            }
            if Config::key_matches(&self.config.bind_nav_page_down, key) {
                self.help_move_cursor(5);
                return;
            }
            if Config::key_matches(&self.config.bind_input_home, key) {
                self.input_cursor = 0;
                return;
            }
            if Config::key_matches(&self.config.bind_input_end, key) {
                self.input_cursor = self.input.len();
                return;
            }
            if Config::key_matches(&self.config.bind_input_kill_line, key) {
                self.input.drain(self.input_cursor..);
                self.help_cursor = 0;
                self.help_scroll = 0;
                self.help_clamp_cursor();
                return;
            }
            if Config::key_matches(&self.config.bind_input_delete_word, key) {
                let bytes = self.input.as_bytes();
                let mut i = self.input_cursor;
                while i > 0 && bytes[i - 1].is_ascii_whitespace() {
                    i -= 1;
                }
                while i > 0 && !bytes[i - 1].is_ascii_whitespace() {
                    i -= 1;
                }
                self.input.drain(i..self.input_cursor);
                self.input_cursor = i;
                self.help_cursor = 0;
                self.help_scroll = 0;
                self.help_clamp_cursor();
                return;
            }
            if Config::key_matches(&self.config.bind_help_exit, key) {
                if !self.input.is_empty() {
                    self.input.clear();
                    self.input_cursor = 0;
                    self.help_cursor = 0;
                    self.help_scroll = 0;
                    return;
                }
                self.exit_help();
                return;
            }
            if Config::key_matches(&self.config.bind_input_clear, key) {
                self.input.clear();
                self.input_cursor = 0;
                self.help_cursor = 0;
                self.help_scroll = 0;
                return;
            }
            if Config::key_matches(&self.config.bind_quit, key) {
                self.quit = true;
                return;
            }
            if Config::key_matches(&self.config.bind_input_word_left, key) {
                self.move_word(-1);
                return;
            }
            if Config::key_matches(&self.config.bind_input_word_right, key) {
                self.move_word(1);
                return;
            }
            if Config::key_matches(&self.config.bind_help_enter, key) {
                self.start_help_edit();
                return;
            }
            if let Some(needs_clamp) = self.edit_input(key) {
                if needs_clamp {
                    self.help_cursor = 0;
                    self.help_scroll = 0;
                    self.help_clamp_cursor();
                }
                return;
            }
            return;
        }

        if self.mode == Mode::Command {
            // check for help bind even in command mode
            if key.code == KeyCode::Char(self.config.bind_help)
                && !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT)
            {
                self.mode = Mode::Normal; // exit command before entering help? spec says ? enabled inside command_mode
                self.enter_help();
                return;
            }
            if Config::key_matches(&self.config.bind_nav_up, key) {
                self.move_cursor(-1);
                return;
            }
            if Config::key_matches(&self.config.bind_nav_down, key) {
                self.move_cursor(1);
                return;
            }
            if Config::key_matches(&self.config.bind_nav_page_up, key) {
                self.move_cursor(-5);
                return;
            }
            if Config::key_matches(&self.config.bind_nav_page_down, key) {
                self.move_cursor(5);
                return;
            }
            if Config::key_matches(&self.config.bind_input_clear, key) {
                self.input.clear();
                self.input_cursor = 0;
                self.filter();
                return;
            }
            if Config::key_matches(&self.config.bind_command_exit, key) {
                self.mode = Mode::Normal;
                return;
            }
            if Config::key_matches(&self.config.bind_quit, key) {
                self.quit = true;
                return;
            }
            self.handle_command_key(key);
            return;
        }

        // Normal mode
        self.mouse_hover = false;

        if key.code == KeyCode::Char(self.config.bind_help)
            && !key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
        {
            self.enter_help();
            return;
        }

        if key.code == KeyCode::Char(self.config.bind_command_mode)
            && !key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
        {
            self.mode = Mode::Command;
            return;
        }

        if Config::key_matches(&self.config.bind_nav_up, key) {
            self.move_cursor(-1);
            return;
        }
        if Config::key_matches(&self.config.bind_nav_down, key) {
            self.move_cursor(1);
            return;
        }
        if Config::key_matches(&self.config.bind_nav_page_up, key) {
            self.move_cursor(-5);
            return;
        }
        if Config::key_matches(&self.config.bind_nav_page_down, key) {
            self.move_cursor(5);
            return;
        }
        if Config::key_matches(&self.config.bind_quit, key) {
            self.quit = true;
            return;
        }
        if let Some(needs_filter) = self.edit_input(key) {
            if needs_filter {
                self.filter();
            }
            return;
        }
        if Config::key_matches(&self.config.bind_input_clear, key) {
            self.input.clear();
            self.input_cursor = 0;
            self.filter();
            return;
        }

        self.handle_key(key);
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) {
        self.last_mouse_col = event.column;
        self.last_mouse_row = event.row;

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.check_clickables(event.column, event.row) {
                    return;
                }
                if self.is_help() {
                    if let Some(ord) = self.get_help_index_from_mouse(event.column, event.row) {
                        self.help_cursor = ord;
                        self.start_help_edit();
                    }
                    return;
                }
                if let Some(idx) = self.get_index_from_mouse(event.column, event.row) {
                    self.cursor = idx;
                    self.touched = true;
                    self.goto();
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if self.check_clickables(event.column, event.row) {
                    return;
                }
                if self.is_help() {
                    return;
                }
                if self.get_index_from_mouse(event.column, event.row).is_some() {
                    self.mode = Mode::Command;
                }
            }
            MouseEventKind::Moved => {
                if self.is_help() {
                    if let Some(ord) = self.get_help_index_from_mouse(event.column, event.row) {
                        self.help_cursor = ord;
                        self.mouse_hover = true;
                    }
                    return;
                }
                if let Some(idx) = self.get_index_from_mouse(event.column, event.row) {
                    self.cursor = idx;
                    self.touched = true;
                    self.mouse_hover = true;
                }
            }
            MouseEventKind::ScrollDown => {
                self.mouse_hover = false;
                if self.is_help() {
                    self.help_scroll_view(1);
                } else {
                    self.scroll_view(1);
                }
            }
            MouseEventKind::ScrollUp => {
                self.mouse_hover = false;
                if self.is_help() {
                    self.help_scroll_view(-1);
                } else {
                    self.scroll_view(-1);
                }
            }
            _ => {}
        }
    }

    fn handle_command_key(&mut self, key: KeyEvent) {
        if Config::key_matches(&self.config.bind_command_exit, key) {
            self.mode = Mode::Normal;
            return;
        }
        if Config::key_matches(&self.config.bind_command_session_kill, key) {
            self.execute_kill_session();
            return;
        }
        if Config::key_matches(&self.config.bind_command_open_detached, key) {
            self.open_detached();
            return;
        }
    }

    fn check_clickables(&mut self, col: u16, row: u16) -> bool {
        let action = self
            .clickables
            .iter()
            .find(|c| c.contains(col, row))
            .map(|c| c.action);
        if let Some(action) = action {
            self.handle_click(action);
            true
        } else {
            false
        }
    }

    fn handle_click(&mut self, action: Action) {
        match action {
            Action::CommandMode => self.mode = Mode::Command,
            Action::ExitCommandMode => self.mode = Mode::Normal,
            Action::HelpMode => self.enter_help(),
            Action::ExitHelp => {
                if self.mode == Mode::HelpEditing {
                    self.cancel_help_edit();
                } else {
                    self.exit_help();
                }
            }
            Action::KillSession => self.execute_kill_session(),
            Action::OpenDetached => self.open_detached(),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if Config::key_matches(&self.config.bind_nav_up, key) {
            self.move_cursor(-1);
            return;
        }
        if Config::key_matches(&self.config.bind_nav_down, key) {
            self.move_cursor(1);
            return;
        }
        if Config::key_matches(&self.config.bind_jumpto, key) {
            self.goto();
            return;
        }
        if Config::key_matches(&self.config.bind_quit, key) {
            self.quit = true;
            return;
        }
        if let Some(needs_filter) = self.edit_input(key) {
            if needs_filter {
                self.filter();
            }
            return;
        }
    }

    fn scroll_view(&mut self, dir: i32) {
        let n = self.filtered.len();
        if n == 0 {
            return;
        }
        self.touched = true;
        // Scroll bookkeeping runs in virtual-row space, but the cursor keeps
        // stepping over entries only — gaps are never landed on.
        let max_row = self.rows().len().saturating_sub(1);
        if dir < 0 {
            let step = (-dir) as usize;
            self.scroll = self.scroll.saturating_sub(step);
            self.cursor = self.cursor.saturating_sub(step);
        }
        if dir > 0 {
            let step = dir as usize;
            self.scroll = (self.scroll + step).min(max_row);
            let max = n.saturating_sub(1);
            self.cursor = (self.cursor + step).min(max);
        }
    }

    fn help_scroll_view(&mut self, dir: i32) {
        let total = self.help_rows().len();
        if total == 0 {
            return;
        }
        let max_scroll = total.saturating_sub(1);
        if dir < 0 {
            let step = (-dir) as usize;
            self.help_scroll = self.help_scroll.saturating_sub(step);
            self.help_move_cursor(-(step as i32));
        }
        if dir > 0 {
            let step = dir as usize;
            self.help_scroll = (self.help_scroll + step).min(max_scroll);
            self.help_move_cursor(step as i32);
        }
    }

    fn move_cursor(&mut self, amount: i32) {
        self.mouse_hover = false;

        let len = self.filtered.len();
        if len == 0 || amount == 0 {
            return;
        }
        self.touched = true;

        if amount < 0 {
            let step = amount.unsigned_abs() as usize;
            self.cursor = self.cursor.checked_sub(step).unwrap_or(len - 1);
        }
        if amount > 0 {
            self.cursor = (self.cursor + amount as usize) % len;
        }
    }

    fn activate_entry(&mut self, idx: usize) {
        let goto = self.entries[idx].goto.clone();
        let Some(goto) = goto else { return };

        self.pending_goto = Some(goto.clone());

        let mut cur = Some(idx);
        while let Some(i) = cur {
            self.entries[i].is_open = true;
            cur = self.entries[i].parent;
        }

        let is_current = tmux::is_current_session(goto.session);
        if self.auto_close || is_current {
            self.quit = true;
        } else {
            self.schedule_refresh();
        }
    }

    fn goto(&mut self) {
        if let Some(&idx) = self.filtered.get(self.cursor) {
            self.activate_entry(idx);
        }
    }

    // Shared line-edit handler. Returns Some(true) if content changed (needs filter/clamp),
    // Some(false) if only cursor moved, None if not handled.
    fn edit_input(&mut self, key: KeyEvent) -> Option<bool> {
        if Config::key_matches(&self.config.bind_input_left, key) {
            if self.input_cursor > 0 { self.input_cursor -= 1; }
            return Some(false);
        }
        if Config::key_matches(&self.config.bind_input_right, key) {
            if self.input_cursor < self.input.len() { self.input_cursor += 1; }
            return Some(false);
        }
        if Config::key_matches(&self.config.bind_input_home, key) {
            self.input_cursor = 0;
            return Some(false);
        }
        if Config::key_matches(&self.config.bind_input_end, key) {
            self.input_cursor = self.input.len();
            return Some(false);
        }
        if Config::key_matches(&self.config.bind_input_kill_line, key) {
            self.input.drain(self.input_cursor..);
            return Some(true);
        }
        if Config::key_matches(&self.config.bind_input_delete_word, key) {
            let bytes = self.input.as_bytes();
            let mut i = self.input_cursor;
            while i > 0 && bytes[i - 1].is_ascii_whitespace() { i -= 1; }
            while i > 0 && !bytes[i - 1].is_ascii_whitespace() { i -= 1; }
            self.input.drain(i..self.input_cursor);
            self.input_cursor = i;
            return Some(true);
        }
        if Config::key_matches(&self.config.bind_input_word_left, key) {
            self.move_word(-1);
            return Some(false);
        }
        if Config::key_matches(&self.config.bind_input_word_right, key) {
            self.move_word(1);
            return Some(false);
        }
        if Config::key_matches(&self.config.bind_input_backspace, key) {
            if self.input_cursor > 0 {
                self.input_cursor -= 1;
                self.input.remove(self.input_cursor);
                return Some(true);
            }
            return Some(false);
        }
        if Config::key_matches(&self.config.bind_input_delete, key) && self.input_cursor < self.input.len() {
            self.input.remove(self.input_cursor);
            return Some(true);
        }
        if let KeyCode::Char(c) = key.code
            && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
        {
            self.input.insert(self.input_cursor, c);
            self.input_cursor += 1;
            return Some(true);
        }
        None
    }

    fn move_word(&mut self, dir: i32) {
        let bytes = self.input.as_bytes();
        if dir < 0 {
            let mut i = self.input_cursor;
            while i > 0 && bytes[i - 1].is_ascii_whitespace() {
                i -= 1;
            }
            while i > 0 && !bytes[i - 1].is_ascii_whitespace() {
                i -= 1;
            }
            self.input_cursor = i;
        } else {
            let mut i = self.input_cursor;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            self.input_cursor = i;
        }
    }

    fn get_index_from_mouse(&self, column: u16, row: u16) -> Option<usize> {
        let area = self.slot_entries;
        if column < area.x
            || column >= area.x.saturating_add(area.width)
            || row < area.y
            || row >= area.y.saturating_add(area.height)
        {
            return None;
        }

        let vh = area.height as usize;
        let rows = self.rows();
        if rows.is_empty() {
            return None;
        }

        let view_y = (row - area.y) as usize;
        let padding = vh.saturating_sub(rows.len());
        if view_y < padding {
            return None;
        }

        // Gap rows map to None: the pointer can't select or hover them,
        // just like the keyboard navigation skips them.
        match rows.get(self.scroll + (view_y - padding)) {
            Some(Some(p)) => Some(*p),
            _ => None,
        }
    }

    pub(crate) fn get_help_index_from_mouse(&self, column: u16, row: u16) -> Option<usize> {
        let area = self.slot_entries;
        if column < area.x
            || column >= area.x.saturating_add(area.width)
            || row < area.y
            || row >= area.y.saturating_add(area.height)
        {
            return None;
        }
        let vh = area.height as usize;
        let total = self.help_rows().len();
        if total == 0 {
            return None;
        }
        let view_y = (row - area.y) as usize;
        let padding = vh.saturating_sub(total);
        if view_y < padding {
            return None;
        }
        let line_idx = self.help_scroll + (view_y - padding);
        if line_idx >= total {
            return None;
        }
        let rows = self.help_rows();
        rows.get(line_idx).copied().flatten()
    }
}

#[cfg(test)]
mod esc_tests {
    use crate::config::Config;
    use crate::model::Payload;
    use crate::picker::{Mode, Picker};
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn esc() -> KeyEvent {
        KeyEvent { code: KeyCode::Esc, modifiers: KeyModifiers::empty(), kind: KeyEventKind::Press, state: KeyEventState::empty() }
    }

    #[test]
    fn esc_in_command_exits_not_quits() {
        let mut p = Picker::new(Payload { entries: vec![], config: Config::default(), feedbacks: vec![], entries_found: 0 });
        p.mode = Mode::Command;
        p.handle_input(esc());
        assert_eq!(p.mode, Mode::Normal);
        assert!(!p.quit);
        // second esc in Normal should quit
        p.handle_input(esc());
        assert!(p.quit);
    }

    #[test]
    fn esc_in_help_exits_not_quits() {
        let mut p = Picker::new(Payload { entries: vec![], config: Config::default(), feedbacks: vec![], entries_found: 0 });
        p.enter_help();
        assert_eq!(p.mode, Mode::Help);
        p.handle_input(esc());
        assert_eq!(p.mode, Mode::Normal);
        assert!(!p.quit);
    }

    #[test]
    fn esc_in_help_with_filter_clears_first() {
        let mut p = Picker::new(Payload { entries: vec![], config: Config::default(), feedbacks: vec![], entries_found: 0 });
        p.enter_help();
        p.input = "foo".to_string();
        p.input_cursor = 3;
        p.handle_input(esc());
        assert_eq!(p.mode, Mode::Help);
        assert!(p.input.is_empty());
        assert!(!p.quit);
        p.handle_input(esc());
        assert_eq!(p.mode, Mode::Normal);
    }
}
