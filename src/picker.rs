use crate::clickable::{Action, Clickable};
use crate::config::Config;
use crate::daemon;
use crate::logs;
use crate::model::{Entry, EntryType, FeedbackEntry, Goto, Payload};
use crate::tmux;
use ratatui::layout::Rect;
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

const SPINNER_MS: u128 = 30;
const SPINUP_TAU_MS: f64 = 2000.0;

#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    Close,
    Goto(Goto),
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Command,
    Help,
    HelpEditing,
}

#[derive(Debug, Clone)]
pub(crate) struct HelpEdit {
    pub key: String,
    pub saved_filter: String,
    pub saved_cursor: usize,
}

pub struct Picker {
    pub(crate) entries: Vec<Entry>,
    pub(crate) filtered: Vec<usize>,
    pub(crate) cursor: usize,
    pub(crate) input: String,
    pub(crate) input_cursor: usize,
    pub(crate) spinner: usize,
    started_at: Instant,
    last_spinner: Instant,
    pub(crate) quit: bool,
    pub(crate) pending_goto: Option<Goto>,
    tx: mpsc::Sender<Option<Payload>>,
    rx: mpsc::Receiver<Option<Payload>>,
    pub(crate) slot_entries: Rect,
    pub(crate) scroll: usize,
    pub(crate) mouse_hover: bool,
    pub(crate) auto_close: bool,
    pub(crate) mode: Mode,
    pub(crate) config: Config,
    pub(crate) feedbacks: Vec<FeedbackEntry>,
    pub(crate) entries_found: usize,
    pub(crate) clickables: Vec<Clickable>,
    pub(crate) last_mouse_col: u16,
    pub(crate) last_mouse_row: u16,
    pub(crate) help_cursor: usize,
    pub(crate) help_scroll: usize,
    pub(crate) stashed_input: String,
    pub(crate) stashed_cursor: usize,
    pub(crate) help_edit: Option<HelpEdit>,
    pub(crate) touched: bool,
}

impl Picker {
    pub fn new(payload: Payload) -> Self {
        logs::init("picker").ok();

        let (tx, rx) = mpsc::channel::<Option<Payload>>();
        let auto_close = payload.config.auto_close;
        let entries_found = payload.entries_found.max(payload.entries.len());
        daemon::listen(tx.clone());
        let mut picker = Picker {
            cursor: 0,
            entries: payload.entries,
            filtered: Vec::new(),
            input: String::new(),
            input_cursor: 0,
            spinner: 0,
            started_at: Instant::now(),
            last_spinner: Instant::now(),
            quit: false,
            pending_goto: None,
            tx,
            rx,
            slot_entries: Rect::default(),
            scroll: 0,
            mouse_hover: false,
            mode: Mode::Normal,
            clickables: Vec::new(),
            last_mouse_col: 0,
            last_mouse_row: 0,
            auto_close,
            config: payload.config,
            feedbacks: payload.feedbacks,
            entries_found,
            help_cursor: 0,
            help_scroll: 0,
            stashed_input: String::new(),
            stashed_cursor: 0,
            help_edit: None,
            touched: false,
        };
        picker.filtered = picker.filtered();
        picker.cursor = picker.find_initial_cursor();
        picker
    }

    pub fn tick(&mut self) -> Signal {
        let now = Instant::now();
        let spinner_ms = if self.entries.is_empty() {
            let elapsed = now.duration_since(self.started_at).as_millis() as f64;
            (10.0 + 70.0 * (-elapsed / SPINUP_TAU_MS).exp()) as u128
        } else {
            SPINNER_MS
        };
        if now.duration_since(self.last_spinner).as_millis() >= spinner_ms {
            self.spinner = self.spinner.wrapping_add(1);
            self.last_spinner = now;
        }

        while let Ok(result) = self.rx.try_recv() {
            if let Some(payload) = result {
                let path_changed = self.config.path != payload.config.path
                    || self.config.path_worktrees != payload.config.path_worktrees;
                let old_key = self
                    .filtered
                    .get(self.cursor)
                    .and_then(|&i| self.entries.get(i))
                    .map(|e| (e.kind.clone(), e.goto.clone()));
                self.entries_found = payload.entries_found.max(payload.entries.len());
                self.entries = payload.entries;
                self.feedbacks = payload.feedbacks.clone();
                self.config = payload.config;
                self.auto_close = self.config.auto_close;
                if self.is_help() {
                    let saved_input =
                        std::mem::replace(&mut self.input, self.stashed_input.clone());
                    let saved_cursor =
                        std::mem::replace(&mut self.input_cursor, self.stashed_cursor);
                    self.filtered = self.filtered();
                    self.input = saved_input;
                    self.input_cursor = saved_cursor;
                } else {
                    self.filtered = self.filtered();
                }
                if path_changed
                    || (self.input.is_empty() && !self.touched)
                {
                    self.cursor = self.find_initial_cursor();
                } else if let Some((kind, goto)) = old_key.as_ref()
                    && let Some(pos) = self.filtered.iter().position(|&i| {
                        let e = &self.entries[i];
                        &e.kind == kind && &e.goto == goto
                    })
                {
                    self.cursor = pos;
                } else if self.cursor >= self.filtered.len() {
                    self.cursor = self.filtered.len().saturating_sub(1);
                }
            }
        }

        if let Some(goto) = self.pending_goto.take() {
            return Signal::Goto(goto);
        }
        if self.quit {
            Signal::Close
        } else {
            Signal::None
        }
    }

    pub(crate) fn schedule_refresh(&self) {
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = daemon::fetch_once()
                .and_then(|bytes| serde_json::from_slice::<Payload>(&bytes).ok());
            let _ = tx.send(result);
        });
    }

    pub fn render(&mut self, frame: &mut ratatui::Frame, _config: &Config) {
        crate::renderer::render(frame, self);
    }

    pub(crate) fn schedule_initial_fetch(&mut self, overrides: Vec<(String, Option<String>)>) {
        let tx = self.tx.clone();
        thread::spawn(move || {
            let payload = daemon::initial_fetch(&overrides);
            let _ = tx.send(Some(payload));
        });
    }

    // Virtual row layout of the current view: `Some(p)` renders filtered
    // position p, `None` is a decorative gap row (style-entries-gap). Gaps
    // precede dirs and worktrees only — agents hug their parent — and stay
    // outside navigation entirely: the cursor and every visibility check
    // speak in entries, never in gap rows.
    pub(crate) fn rows(&self) -> Vec<Option<usize>> {
        let gap = self.config.style_entries_gap as usize;
        let mut rows = Vec::with_capacity(self.filtered.len());
        for p in 0..self.filtered.len() {
            if gap > 0 && p > 0 && self.entries[self.filtered[p]].kind != EntryType::Agent {
                for _ in 0..gap {
                    rows.push(None);
                }
            }
            rows.push(Some(p));
        }
        rows
    }

    pub fn is_command_mode(&self) -> bool {
        self.mode == Mode::Command
    }
    pub fn is_help(&self) -> bool {
        self.mode == Mode::Help || self.mode == Mode::HelpEditing
    }

    pub fn cursor_entry_buttons(&self) -> Vec<(String, Action)> {
        if !self.is_command_mode() {
            return vec![];
        }
        let Some(&idx) = self.filtered.get(self.cursor) else {
            return vec![];
        };
        let entry = &self.entries[idx];
        let mut buttons = Vec::new();
        if entry.is_open || entry.kind == EntryType::Agent {
            let key = self.config.bind_command_session_kill.to_uppercase();
            buttons.push((format!("{key} Kill Session"), Action::KillSession));
        } else if entry.goto.is_some() {
            let key = self.config.bind_command_open_detached.to_uppercase();
            buttons.push((format!("{key} Open Detached"), Action::OpenDetached));
        }
        buttons
    }

    pub fn execute_kill_session(&mut self) {
        if let Some(&idx) = self.filtered.get(self.cursor) {
            let entry = &self.entries[idx];
            if (entry.is_open || entry.kind == EntryType::Agent)
                && let Some(goto) = &entry.goto
            {
                if let Some(window) = &goto.window {
                    tmux::kill_window(&goto.session, *window);
                } else {
                    let sanitized = goto.session.replace([':', '.'], "_");
                    tmux::kill_session(&sanitized);
                }
                self.mode = Mode::Normal;
                self.schedule_refresh();
            }
        }
    }

    pub fn open_detached(&mut self) {
        if let Some(&idx) = self.filtered.get(self.cursor) {
            let entry = &self.entries[idx];
            if !entry.is_open
                && entry.kind != EntryType::Agent
                && let Some(goto) = &entry.goto
            {
                tmux::open_detached(goto);
                let mut cur = Some(idx);
                while let Some(i) = cur {
                    self.entries[i].is_open = true;
                    cur = self.entries[i].parent;
                }
                self.mode = Mode::Normal;
                self.schedule_refresh();
            }
        }
    }

    pub fn enter_help(&mut self) {
        if self.is_help() {
            return;
        }
        self.stashed_input = self.input.clone();
        self.stashed_cursor = self.input_cursor;
        self.input.clear();
        self.input_cursor = 0;
        self.help_cursor = 0;
        self.help_scroll = 0;
        self.help_edit = None;
        self.mode = Mode::Help;
        self.mouse_hover = false;
    }

    pub fn exit_help(&mut self) {
        if !self.is_help() {
            return;
        }
        self.mode = Mode::Normal;
        self.help_edit = None;
        self.input = self.stashed_input.clone();
        self.input_cursor = self.stashed_cursor;
        self.mouse_hover = false;
        self.filter();
    }

    pub fn start_help_edit(&mut self) {
        if self.mode != Mode::Help {
            return;
        }
        let filtered = self.help_filtered_line_indices();
        if filtered.is_empty() {
            return;
        }
        let line_idx = filtered[self.help_cursor.min(filtered.len() - 1)];
        let lines = crate::help::template_lines();
        if let Some(k) = crate::help::key_at(&lines, line_idx) {
            let raw = crate::help::raw_file_map();
            let raw_v = raw
                .get(&k)
                .cloned()
                .unwrap_or_else(|| self.config.value_string(&k).unwrap_or_default());
            let v = raw_v.split('#').next().unwrap_or(&raw_v).trim().to_string();
            let saved_filter = self.input.clone();
            let saved_cursor = self.input_cursor;
            self.help_edit = Some(HelpEdit { key: k, saved_filter, saved_cursor });
            self.input = v;
            self.input_cursor = self.input.len();
            self.mode = Mode::HelpEditing;
        }
    }

    pub fn cancel_help_edit(&mut self) {
        if self.mode != Mode::HelpEditing {
            return;
        }
        if let Some(edit) = self.help_edit.take() {
            self.input = edit.saved_filter;
            self.input_cursor = edit.saved_cursor;
        }
        self.mode = Mode::Help;
        self.help_clamp_cursor();
    }

    pub fn commit_help_edit(&mut self) {
        if self.mode != Mode::HelpEditing {
            return;
        }
        let Some(edit) = self.help_edit.clone() else {
            self.cancel_help_edit();
            return;
        };
        let key = edit.key.clone();
        let new_value = self.input.clone();
        let res = crate::help::commit_to_disk(&key, &new_value);
        if let Err(msg) = res {
            self.feedbacks.push(crate::model::FeedbackEntry {
                level: crate::model::FeedbackType::Error,
                message: msg,
            });
        } else if let Some(path) = crate::config::Config::config_path().or(Some(crate::config::Config::write_target())) {
            if let Ok(content) = std::fs::read_to_string(&path) {
                let (cfg, fbs) = crate::config::Config::parse_content(&path, &content);
                self.config = cfg;
                self.feedbacks = fbs;
            } else {
                let dummy_path = std::path::Path::new("config");
                let (cfg, fbs) = crate::config::Config::parse_content(dummy_path, &format!("{key} = {new_value}"));
                self.config = cfg;
                self.feedbacks = fbs;
            }
        }
        if let Some(edit) = self.help_edit.take() {
            self.input = edit.saved_filter;
            self.input_cursor = edit.saved_cursor;
        }
        self.mode = Mode::Help;
        self.help_clamp_cursor();
    }

    pub fn help_move_cursor(&mut self, amount: i32) {
        let len = self.help_filtered_line_indices().len();
        if len == 0 || amount == 0 {
            return;
        }
        self.mouse_hover = false;
        let len_i = len as i32;
        let cur = self.help_cursor as i32;
        let new_cur = (cur + amount).rem_euclid(len_i);
        self.help_cursor = new_cur as usize;
    }

    pub fn help_rows(&self) -> Vec<Option<usize>> {
        if self.help_is_filtered() {
            let lines = crate::help::template_lines();
            let blocks = crate::help::parse_blocks(&lines);
            let filter = self.help_filter_str().trim().to_lowercase();
            let words: Vec<String> = filter.split_whitespace().map(|s| s.to_string()).collect();
            let mut rows = Vec::new();
            let mut ord = 0;
            for block in blocks {
                let mut matching = Vec::new();
                for &li in &block.entries {
                    if let Some(k) = crate::help::key_at(&lines, li) {
                        let kl = k.to_lowercase();
                        if words.iter().all(|w| kl.contains(w)) {
                            matching.push(li);
                        }
                    }
                }
                if matching.is_empty() {
                    continue;
                }
                for _ in &block.header {
                    rows.push(None);
                }
                for _ in matching {
                    rows.push(Some(ord));
                    ord += 1;
                }
            }
            rows
        } else {
            let lines = crate::help::template_lines();
            let idxs = crate::help::selectable_indices(&lines);
            let mut map = vec![None; lines.len()];
            for (ord, &li) in idxs.iter().enumerate() {
                map[li] = Some(ord);
            }
            map.into_iter().collect()
        }
    }

    pub fn help_cursor_line(&self) -> usize {
        self.help_rows()
            .iter()
            .position(|r| *r == Some(self.help_cursor))
            .unwrap_or(0)
    }

    pub fn help_visible_lines(&self) -> Vec<String> {
        if self.help_is_filtered() {
            crate::help::filtered_visible_lines(self.help_filter_str(), &self.config)
        } else {
            crate::help::display_lines(&self.config)
        }
    }

    pub fn help_filter_str(&self) -> &str {
        if let Some(edit) = &self.help_edit {
            &edit.saved_filter
        } else {
            &self.input
        }
    }

    pub fn help_filtered_line_indices(&self) -> Vec<usize> {
        let lines = crate::help::template_lines();
        let idxs = crate::help::selectable_indices(&lines);
        let filter = self.help_filter_str().trim().to_lowercase();
        if filter.is_empty() {
            return idxs;
        }
        let words: Vec<String> = filter.split_whitespace().map(|s| s.to_string()).collect();
        idxs.into_iter()
            .filter(|&li| {
                if let Some(k) = crate::help::key_at(&lines, li) {
                    let kl = k.to_lowercase();
                    words.iter().all(|w| kl.contains(w))
                } else {
                    false
                }
            })
            .collect()
    }

    pub(crate) fn help_clamp_cursor(&mut self) {
        let len = self.help_filtered_line_indices().len();
        if len == 0 {
            self.help_cursor = 0;
            self.help_scroll = 0;
        } else if self.help_cursor >= len {
            self.help_cursor = len.saturating_sub(1);
        }
    }

    pub fn help_is_filtered(&self) -> bool {
        !self.help_filter_str().trim().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Entry, EntryType};
    use std::path::PathBuf;

    fn entry(kind: EntryType, name: &str, path: PathBuf) -> Entry {
        Entry {
            kind,
            label: name.to_string(),
            path: path.clone(),
            changes: None,
            branch: None,
            is_open: false,
            is_running: false,
            depth: 0,
            ancestors: vec![],
            is_last: false,
            search_text: name.to_string(),
            goto: Some(crate::model::Goto {
                session: name.to_string(),
                path,
                window: None,
                pane: None,
            }),
            parent: None,
            connector: String::new(),
            search_text_lower: name.to_lowercase(),
        }
    }

    fn picker_with(kinds: &[EntryType], gap: u64) -> Picker {
        let (tx, rx) = mpsc::channel();
        let config = Config {
            style_entries_gap: gap,
            ..Config::default()
        };
        Picker {
            entries: kinds
                .iter()
                .map(|k| entry(k.clone(), "", PathBuf::from("/tmp")))
                .collect(),
            filtered: (0..kinds.len()).collect(),
            cursor: 0,
            input: String::new(),
            input_cursor: 0,
            spinner: 0,
            started_at: Instant::now(),
            last_spinner: Instant::now(),
            quit: false,
            pending_goto: None,
            tx,
            rx,
            slot_entries: Rect::default(),
            scroll: 0,
            mouse_hover: false,
            auto_close: true,
            mode: Mode::Normal,
            config,
            feedbacks: vec![],
            entries_found: 0,
            clickables: vec![],
            last_mouse_col: 0,
            last_mouse_row: 0,
            help_cursor: 0,
            help_scroll: 0,
            stashed_input: String::new(),
            stashed_cursor: 0,
            help_edit: None,
            touched: false,
        }
    }

    fn row_marks(rows: &[Option<usize>]) -> String {
        rows.iter()
            .map(|r| match r {
                Some(_) => 'e',
                None => '.',
            })
            .collect()
    }

    // Dirs and worktrees get gaps before them, agents hug their parent.
    #[test]
    fn gaps_skip_agents() {
        let picker = picker_with(
            &[
                EntryType::Dir,
                EntryType::Worktree,
                EntryType::Agent,
                EntryType::Agent,
                EntryType::Worktree,
                EntryType::Dir,
            ],
            1,
        );
        assert_eq!(row_marks(&picker.rows()), "e.eee.e.e");
    }

    #[test]
    fn no_first_gap_and_multi_row_gaps() {
        let picker = picker_with(&[EntryType::Dir, EntryType::Dir], 2);
        assert_eq!(row_marks(&picker.rows()), "e..e");
    }

    #[test]
    fn zero_gap_is_plain_list() {
        let picker = picker_with(&[EntryType::Dir, EntryType::Worktree, EntryType::Agent], 0);
        assert_eq!(row_marks(&picker.rows()), "eee");
    }

    #[test]
    fn help_enter_exit_stashes_input() {
        let mut p = picker_with(&[EntryType::Dir], 0);
        p.input = "foo".to_string();
        p.input_cursor = 3;
        p.enter_help();
        assert_eq!(p.mode, Mode::Help);
        assert_eq!(p.input, "");
        assert_eq!(p.stashed_input, "foo");
        p.exit_help();
        assert_eq!(p.mode, Mode::Normal);
        assert_eq!(p.input, "foo");
        assert_eq!(p.input_cursor, 3);
    }

    #[test]
    fn help_move_wraps() {
        let mut p = picker_with(&[EntryType::Dir], 0);
        p.enter_help();
        let count = crate::help::selectable_indices(&crate::help::template_lines()).len();
        assert!(count > 0);
        p.help_cursor = 0;
        p.help_move_cursor(-1);
        assert_eq!(p.help_cursor, count - 1);
        p.help_move_cursor(1);
        assert_eq!(p.help_cursor, 0);
        p.help_move_cursor(5);
        assert_eq!(p.help_cursor, 5 % count);
    }

    #[test]
    fn help_start_edit_sets_buffer() {
        let mut p = picker_with(&[EntryType::Dir], 0);
        p.enter_help();
        // help_cursor 0 should be first selectable key (likely path)
        p.start_help_edit();
        assert_eq!(p.mode, Mode::HelpEditing);
        assert!(p.help_edit.is_some());
        let key = p.help_edit.as_ref().unwrap().key.clone();
        let raw = crate::help::raw_file_map();
        let expected = raw
            .get(&key)
            .cloned()
            .unwrap_or_else(|| p.config.value_string(&key).unwrap_or_default());
        assert_eq!(p.input, expected);
        p.cancel_help_edit();
        assert_eq!(p.mode, Mode::Help);
        assert!(p.help_edit.is_none());
    }

    #[test]
    fn help_start_edit_shows_raw_invalid() {
        use std::sync::{Mutex, OnceLock};
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let lock = ENV_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let tmp =
            std::env::temp_dir().join(format!("ramo_test_help_edit_raw_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".config/ramo")).unwrap();
        let orig_home = std::env::var("HOME").ok();
        let orig_xdg = std::env::var("XDG_CONFIG_HOME").ok();
        unsafe {
            std::env::set_var("HOME", &tmp);
            std::env::set_var("XDG_CONFIG_HOME", "");
        }
        let target = crate::config::Config::write_target();
        std::fs::write(&target, "auto-close = trudwadawda\n").unwrap();
        let mut p = picker_with(&[EntryType::Dir], 0);
        // reload config from file to ensure picker.config is default but file has invalid
        let (cfg, _) = crate::config::Config::load(&[]);
        p.config = cfg;
        p.enter_help();
        // find auto-close index
        let lines = crate::help::template_lines();
        let idxs = crate::help::selectable_indices(&lines);
        let auto_idx = idxs
            .iter()
            .position(|&li| crate::help::key_at(&lines, li).as_deref() == Some("auto-close"))
            .unwrap();
        p.help_cursor = auto_idx;
        p.start_help_edit();
        assert_eq!(p.help_edit.as_ref().map(|e| e.key.as_str()), Some("auto-close"));
        assert_eq!(p.input, "trudwadawda");
        let _ = std::fs::remove_dir_all(&tmp);
        unsafe {
            if let Some(v) = orig_home {
                std::env::set_var("HOME", v);
            } else {
                std::env::remove_var("HOME");
            }
            if let Some(v) = orig_xdg {
                std::env::set_var("XDG_CONFIG_HOME", v);
            } else {
                std::env::remove_var("XDG_CONFIG_HOME");
            }
        }
        drop(lock);
    }

    #[test]
    fn help_filter_fuzzy_search() {
        let mut p = picker_with(&[EntryType::Dir], 0);
        p.enter_help();
        assert!(!p.help_is_filtered());
        assert_eq!(
            p.help_filtered_line_indices().len(),
            crate::help::selectable_indices(&crate::help::template_lines()).len()
        );
        // filter for "auto" should match auto-close only (maybe) + its header
        p.input = "auto".to_string();
        p.input_cursor = 4;
        let filtered = p.help_filtered_line_indices();
        assert!(filtered.len() >= 1);
        let lines = crate::help::template_lines();
        for &li in &filtered {
            let k = crate::help::key_at(&lines, li).unwrap();
            assert!(k.to_lowercase().contains("auto"));
        }
        // visible includes header for auto-close block
        let visible = p.help_visible_lines();
        assert!(visible.len() >= filtered.len());
        assert!(visible.iter().any(|l| l.contains("auto-close")));
        // header should be present for auto-close
        assert!(visible.iter().any(|l| l.contains("defines if the picker")));
        // hide-changes has no header
        p.input = "hide-changes-inactive".to_string();
        let visible2 = p.help_visible_lines();
        assert!(visible2.iter().any(|l| l.contains("hide-changes-inactive")));
        assert!(!visible2.iter().any(|l| l.trim().starts_with('#')));
        // check that unknown filter yields 0
        p.input = "nonexistentkey123".to_string();
        assert_eq!(p.help_filtered_line_indices().len(), 0);
        assert!(p.help_visible_lines().is_empty());
        p.input.clear();
        assert!(!p.help_is_filtered());
    }

    #[test]
    fn help_filter_typing_resets_cursor() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
        let mut p = picker_with(&[EntryType::Dir], 0);
        p.enter_help();
        p.help_cursor = 5;
        // simulate typing 'a' in help
        let key = KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        p.handle_input(key);
        assert_eq!(p.input, "a");
        assert_eq!(p.help_cursor, 0);
        assert_eq!(p.help_scroll, 0);
    }

    fn payload_with(entries: Vec<Entry>) -> crate::model::Payload {
        let n = entries.len();
        crate::model::Payload {
            entries,
            config: Config::default(),
            feedbacks: vec![],
            entries_found: n,
        }
    }

    // Boot keeps re-anchoring to cwd until the user moves: an early
    // incomplete list must not pin the cursor once the correct list arrives.
    #[test]
    fn boot_reanchors_until_touched() {
        let cwd = std::env::current_dir().unwrap();
        let mut p = picker_with(&[EntryType::Dir], 0);
        p.entries = vec![entry(EntryType::Dir, "a", std::path::PathBuf::from("/tmp/ramo-boot-a-xyz"))];
        p.filtered = p.filtered();
        p.cursor = p.find_initial_cursor();
        assert_eq!(p.cursor, 0);

        // Correct list arrives (cwd dir sorts after a): cursor must follow it,
        // not stick to the index placed for the incomplete list.
        p.tx
            .send(Some(payload_with(vec![
                entry(EntryType::Dir, "a", std::path::PathBuf::from("/tmp/ramo-boot-a-xyz")),
                entry(EntryType::Dir, "home", cwd.clone()),
            ])))
            .unwrap();
        p.tick();
        assert_eq!(p.cursor, 1);
        assert_eq!(p.entries[p.filtered[p.cursor]].label, "home");

        // A later refresh that only appends must keep the cursor on cwd.
        p.tx
            .send(Some(payload_with(vec![
                entry(EntryType::Dir, "a", std::path::PathBuf::from("/tmp/ramo-boot-a-xyz")),
                entry(EntryType::Dir, "home", cwd),
                entry(EntryType::Dir, "z", std::path::PathBuf::from("/tmp/ramo-boot-z-xyz")),
            ])))
            .unwrap();
        p.tick();
        assert_eq!(p.entries[p.filtered[p.cursor]].label, "home");
    }

    // After the user moves, refreshes preserve the selected entry by
    // identity (navigation target), not by numeric index.
    #[test]
    fn touched_cursor_follows_entry_across_reorder() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
        let mut p = picker_with(&[EntryType::Dir], 0);
        p.entries = vec![
            entry(EntryType::Dir, "a", std::path::PathBuf::from("/tmp/ramo-reorder-a-xyz")),
            entry(EntryType::Dir, "b", std::path::PathBuf::from("/tmp/ramo-reorder-b-xyz")),
        ];
        p.filtered = p.filtered();
        p.cursor = 0;
        assert!(!p.touched);

        // User navigates: down from 0 wraps... use down to reach b.
        let down = KeyEvent {
            code: KeyCode::Down,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        };
        p.handle_input(down);
        assert!(p.touched);
        assert_eq!(p.cursor, 1);
        assert_eq!(p.entries[p.filtered[p.cursor]].label, "b");

        // Daemon reorders the list: cursor must stay on b (now at 0).
        p.tx
            .send(Some(payload_with(vec![
                entry(EntryType::Dir, "b", std::path::PathBuf::from("/tmp/ramo-reorder-b-xyz")),
                entry(EntryType::Dir, "a", std::path::PathBuf::from("/tmp/ramo-reorder-a-xyz")),
            ])))
            .unwrap();
        p.tick();
        assert_eq!(p.cursor, 0);
        assert_eq!(p.entries[p.filtered[p.cursor]].label, "b");
    }
}
