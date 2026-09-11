use crate::clickable::{Action, Clickable};
use crate::config::Config;
use crate::daemon;
use crate::git;
use crate::logs;
use crate::model::{Entry, EntryType, FeedbackEntry, FeedbackType, Goto, Payload};
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

enum OpResult {
    Created { dest: std::path::PathBuf },
    Failed { dest: std::path::PathBuf, msg: String },
}

#[derive(Debug, Clone)]
struct PendingCreate {
    dest: std::path::PathBuf,
    branch: String,
    dir_path: std::path::PathBuf,
    dir_label: String,
    started: Instant,
}

// ponytail: 60s ceiling, git is instant or hung — a spinner must never stick forever
const PENDING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

pub struct Picker {
    pub(crate) entries: Vec<Entry>,
    pub(crate) filtered: Vec<usize>,
    pub(crate) cursor: usize,
    pub(crate) input: String,
    pub(crate) input_cursor: usize,
    pub(crate) spinner: usize,
    started_at: Instant,
    last_spinner: Instant,
    pub quit: bool,
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
    op_tx: mpsc::Sender<OpResult>,
    op_rx: mpsc::Receiver<OpResult>,
    pending_create: Option<PendingCreate>,
}

impl Picker {
    pub fn new(payload: Payload) -> Self {
        logs::init("picker").ok();

        let (tx, rx) = mpsc::channel::<Option<Payload>>();
        let (op_tx, op_rx) = mpsc::channel::<OpResult>();
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
            op_tx,
            op_rx,
            pending_create: None,
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

        while let Ok(op) = self.op_rx.try_recv() {
            self.handle_op(op);
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

        self.reconcile_pending();

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

    pub fn schedule_initial_fetch(&mut self, overrides: Vec<(String, Option<String>)>) {
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
        if entry.pending {
            return vec![];
        }
        let mut buttons = Vec::new();
        if entry.is_open || entry.kind == EntryType::Agent {
            let key = self.config.bind_command_session_kill.to_uppercase();
            buttons.push((format!("{key} Kill Session"), Action::KillSession));
        } else if entry.goto.is_some() {
            let key = self.config.bind_command_open_detached.to_uppercase();
            buttons.push((format!("{key} Open Detached"), Action::OpenDetached));
        }
        if entry.kind == EntryType::Dir || entry.kind == EntryType::Worktree {
            let key = self.config.bind_command_worktree_new.to_uppercase();
            buttons.push((format!("{key} New Worktree"), Action::WorktreeNew));
        }
        if entry.kind == EntryType::Worktree {
            let key = self.config.bind_command_worktree_delete.to_uppercase();
            buttons.push((format!("{key} Delete Worktree"), Action::WorktreeDelete));
        }
        buttons
    }

    pub fn execute_kill_session(&mut self) {
        let Some(&idx) = self.filtered.get(self.cursor) else {
            return;
        };
        let entry = self.entries[idx].clone();
        let Some(goto) = entry.goto.clone() else {
            return;
        };
        if !(entry.is_open || entry.kind == EntryType::Agent) {
            self.feedbacks.push(FeedbackEntry {
                level: FeedbackType::Warning,
                message: format!("no live session on '{}'", entry.label),
            });
            return;
        }
        if let Some(window) = goto.window {
            tmux::kill_window(&goto.session, window);
        } else {
            let sanitized = goto.session.replace([':', '.'], "_");
            tmux::kill_session(&sanitized);
        }
        self.mode = Mode::Normal;
        self.schedule_refresh();
    }

    pub fn open_detached(&mut self) {
        let Some(&idx) = self.filtered.get(self.cursor) else {
            return;
        };
        let entry = self.entries[idx].clone();
        let Some(goto) = entry.goto.clone() else {
            return;
        };
        if entry.is_open || entry.kind == EntryType::Agent || entry.pending {
            self.feedbacks.push(FeedbackEntry {
                level: FeedbackType::Warning,
                message: format!("nothing to open on '{}'", entry.label),
            });
            return;
        }
        tmux::open_detached(&goto);
        let mut cur = Some(idx);
        while let Some(i) = cur {
            self.entries[i].is_open = true;
            cur = self.entries[i].parent;
        }
        self.mode = Mode::Normal;
        self.schedule_refresh();
    }

    pub fn create_worktree(&mut self) {
        let Some(&idx) = self.filtered.get(self.cursor) else {
            return;
        };
        let entry = self.entries[idx].clone();
        if entry.kind == EntryType::Agent || entry.pending {
            self.feedbacks.push(FeedbackEntry {
                level: FeedbackType::Warning,
                message: "cannot create a worktree here".into(),
            });
            return;
        }
        let origin = if entry.kind == EntryType::Dir {
            entry
        } else if let Some(dir) = entry.parent.and_then(|p| self.entries.get(p)).filter(|e| e.kind == EntryType::Dir) {
            dir.clone()
        } else {
            return;
        };
        // The filter input doubles as the branch name; empty input auto-names.
        let mut branch = git::sanitize_branch_name(self.input.trim());
        if branch.is_empty() {
            branch = git::auto_branch_name();
        }
        let dest = git::unique_dest(git::plan_worktree(
            &origin.path,
            &branch,
            &self.config.path_worktrees,
        ));
        self.pending_create = Some(PendingCreate {
            dest: dest.clone(),
            branch: branch.clone(),
            dir_path: origin.path.clone(),
            dir_label: origin.label.clone(),
            started: Instant::now(),
        });
        if let Some(p) = self.pending_create.clone() {
            self.insert_placeholder(&p);
        }
        self.mode = Mode::Normal;
        let tx = self.op_tx.clone();
        let repo = origin.path.clone();
        thread::spawn(move || {
            let res = git::worktree_add(&repo, &dest, &branch);
            let _ = tx.send(match res {
                Ok(()) => OpResult::Created { dest },
                Err(e) => OpResult::Failed { dest, msg: e },
            });
        });
    }

    fn is_in_subtree(&self, mut idx: usize, root: usize) -> bool {
        while let Some(p) = self.entries.get(idx).and_then(|e| e.parent) {
            if p == root {
                return true;
            }
            idx = p;
        }
        false
    }

    // Optimistic row at the position the real worktree will take: last
    // child of its dir. Cursor stays on its entry; viewport reveals the row.
    fn insert_placeholder(&mut self, p: &PendingCreate) {
        let Some(dir_idx) = self.entries.iter().position(|e| e.kind == EntryType::Dir && e.path == p.dir_path)
        else {
            self.pending_create = None;
            return;
        };
        let cursor_id = self
            .filtered
            .get(self.cursor)
            .and_then(|&i| self.entries.get(i))
            .map(|e| (e.kind.clone(), e.goto.clone()));
        if let Some(last) = (0..self.entries.len())
            .rev()
            .find(|&i| self.entries[i].parent == Some(dir_idx) && self.entries[i].depth == 1)
        {
            self.entries[last].is_last = false;
            self.entries[last].compute_connector();
        }
        let label = p
            .dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.branch.clone());
        let mut ph = Entry {
            kind: EntryType::Worktree,
            label: label.clone(),
            path: p.dest.clone(),
            changes: None,
            branch: Some(p.branch.clone()),
            is_open: false,
            is_running: false,
            pending: true,
            depth: 1,
            ancestors: vec![],
            is_last: true,
            search_text: format!("{} {} {}", p.dir_label, label, p.branch),
            goto: Some(Goto {
                session: label,
                path: p.dest.clone(),
                window: None,
                pane: None,
            }),
            parent: Some(dir_idx),
            connector: String::new(),
            search_text_lower: String::new(),
        };
        ph.compute_connector();
        ph.search_text_lower = ph.search_text.to_lowercase();
        let mut at = dir_idx + 1;
        while at < self.entries.len() && self.is_in_subtree(at, dir_idx) {
            at += 1;
        }
        self.entries.insert(at, ph);
        self.filtered = self.filtered();
        if let Some((kind, goto)) = cursor_id
            && let Some(pos) = self
                .filtered
                .iter()
                .position(|&i| self.entries[i].kind == kind && self.entries[i].goto == goto)
        {
            self.cursor = pos;
        }
        self.reveal_path(&p.dest);
    }

    // Keep the cursor where it is and scroll down just enough to show the
    // row at `dest`. If both don't fit, render snaps back to the cursor:
    // it always wins.
    fn reveal_path(&mut self, dest: &std::path::PathBuf) {
        let h = self.slot_entries.height as usize;
        if h == 0 {
            return;
        }
        let rows = self.rows();
        let row = self
            .filtered
            .iter()
            .position(|&i| self.entries[i].path == *dest)
            .and_then(|p| rows.iter().position(|&r| r == Some(p)));
        let cursor_row = rows.iter().position(|&r| r == Some(self.cursor));
        if let Some(row) = row
            && cursor_row.is_some()
        {
            self.scroll = row.saturating_add(1).saturating_sub(h);
        }
    }

    fn handle_op(&mut self, op: OpResult) {
        match op {
            OpResult::Created { dest } => {
                if self.pending_create.as_ref().is_some_and(|p| p.dest == dest) {
                    self.schedule_refresh();
                }
            }
            OpResult::Failed { dest, msg } => {
                let branch = self
                    .pending_create
                    .as_ref()
                    .filter(|p| p.dest == dest)
                    .map(|p| p.branch.clone());
                if let Some(branch) = branch {
                    self.fail_pending(&dest, format!("cannot create worktree '{branch}': {msg}"));
                }
            }
        }
    }

    fn fail_pending(&mut self, dest: &std::path::PathBuf, message: String) {
        if self.pending_create.as_ref().is_none_or(|p| &p.dest != dest) {
            return;
        }
        let p = self.pending_create.take().unwrap();
        self.entries.retain(|e| !(e.pending && e.path == p.dest));
        self.feedbacks.push(FeedbackEntry {
            level: FeedbackType::Error,
            message,
        });
        self.filtered = self.filtered();
        // Cursor never moved, but the rows around it did: re-anchor to the origin dir.
        if let Some(pos) = self
            .filtered
            .iter()
            .position(|&i| self.entries[i].kind == EntryType::Dir && self.entries[i].path == p.dir_path)
        {
            self.cursor = pos;
        } else if self.cursor >= self.filtered.len() {
            self.cursor = self.filtered.len().saturating_sub(1);
        }
    }

    // Daemon payloads replace `entries`, wiping the placeholder: re-add it
    // while the op is in flight. Once the real entry arrives the cursor
    // stays where it was — the viewport reveals the new child and we
    // travel straight into it.
    fn reconcile_pending(&mut self) {
        if let Some((dest, branch)) = self
            .pending_create
            .as_ref()
            .filter(|p| p.started.elapsed() > PENDING_TIMEOUT)
            .map(|p| (p.dest.clone(), p.branch.clone()))
        {
            self.fail_pending(&dest, format!("timed out creating worktree '{branch}' — check `git worktree list`"));
            return;
        }
        let Some(p) = self.pending_create.clone() else {
            return;
        };
        if self
            .entries
            .iter()
            .any(|e| e.kind == EntryType::Worktree && !e.pending && e.path == p.dest)
        {
            self.pending_create = None;
            self.entries.retain(|e| !e.pending);
            self.filtered = self.filtered();
            if self
                .filtered
                .iter()
                .any(|&i| self.entries[i].kind == EntryType::Worktree && self.entries[i].path == p.dest)
            {
                self.reveal_path(&p.dest);
            }
            if let Some(pos) = self.filtered.iter().position(|&i| {
                self.entries[i].kind == EntryType::Worktree && self.entries[i].path == p.dest
            }) {
                let idx = self.filtered[pos];
                self.activate_entry(idx);
            }
            self.feedbacks.push(FeedbackEntry {
                level: FeedbackType::Warning,
                message: format!("✓ worktree '{}' created", p.branch),
            });
            return;
        }
        if !self.entries.iter().any(|e| e.pending && e.path == p.dest) {
            self.insert_placeholder(&p);
        }
    }

    pub fn delete_worktree(&mut self) {
        let Some(&idx) = self.filtered.get(self.cursor) else {
            return;
        };
        let entry = self.entries[idx].clone();
        if entry.kind != EntryType::Worktree || entry.pending {
            self.feedbacks.push(FeedbackEntry {
                level: FeedbackType::Warning,
                message: "select a worktree entry to delete".into(),
            });
            return;
        }
        let wt = entry.path.clone();
        let repo = entry
            .parent
            .and_then(|p| self.entries.get(p))
            .map(|e| e.path.clone())
            .unwrap_or_else(|| wt.clone());
        for e in &self.entries {
            if e.parent == Some(idx)
                && e.kind == EntryType::Agent
                && let Some(g) = &e.goto
            {
                if let Some(w) = g.window {
                    tmux::kill_window(&g.session, w);
                } else {
                    tmux::kill_session(&g.session.replace([':', '.'], "_"));
                }
            }
        }
        if let Some(name) = wt.file_name().map(|n| n.to_string_lossy().replace([':', '.'], "_")) {
            tmux::kill_session(&name);
        }
        match git::worktree_remove(&repo, &wt) {
            Ok(()) => self.feedbacks.push(FeedbackEntry {
                level: FeedbackType::Warning,
                message: format!("✓ worktree {} removed", wt.display()),
            }),
            Err(e) => self.feedbacks.push(FeedbackEntry {
                level: FeedbackType::Error,
                message: format!("cannot remove worktree: {e}"),
            }),
        }
        self.mode = Mode::Normal;
        self.schedule_refresh();
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

