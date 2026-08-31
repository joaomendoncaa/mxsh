use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub(crate) const LINE_VERT: char = '\u{2502}';
const CORNER_BL: char = '\u{2514}';
const WORKTREE: char = '\u{2442}';
const CHECKED: char = '\u{2713}';
const SPINNER: &[char] = &[
    '\u{280b}', '\u{2819}', '\u{2839}', '\u{2838}', '\u{283c}', '\u{2834}', '\u{2826}', '\u{2827}',
    '\u{2807}', '\u{280f}',
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FeedbackType {
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackEntry {
    pub level: FeedbackType,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct TmuxSession {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct TmuxPane {
    pub session_name: String,
    pub window_index: usize,
    pub pane_index: usize,
    pub current_command: String,
    pub current_path: PathBuf,
    pub activity: i64,
}

#[derive(Debug, Clone)]
pub struct Opencode {
    pub id: String,
    pub title: String,
    pub directory: PathBuf,
    pub time_updated: i64,
    pub is_running: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub is_main: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Changes {
    pub additions: i64,
    pub deletions: i64,
}

impl Changes {
    pub fn add(&self, other: &Changes) -> Changes {
        Changes {
            additions: self.additions + other.additions,
            deletions: self.deletions + other.deletions,
        }
    }

    pub fn has_none(&self) -> bool {
        self.additions == 0 && self.deletions == 0
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EntryType {
    Dir,
    Worktree,
    #[serde(alias = "Session", alias = "Clanker")]
    Agent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Goto {
    pub session: String,
    pub path: PathBuf,
    pub window: Option<usize>,
    pub pane: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub kind: EntryType,
    pub label: String,
    pub path: PathBuf,
    #[serde(alias = "diff")]
    pub changes: Option<Changes>,
    #[serde(default)]
    pub branch: Option<String>,
    pub is_open: bool,
    pub is_running: bool,
    pub depth: usize,
    pub ancestors: Vec<bool>,
    pub is_last: bool,
    pub search_text: String,
    pub goto: Option<Goto>,
    pub parent: Option<usize>,
    pub connector: String,
    pub search_text_lower: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Payload {
    pub entries: Vec<Entry>,
    pub config: Config,
    #[serde(default)]
    pub feedbacks: Vec<FeedbackEntry>,
    #[serde(default)]
    pub entries_found: usize,
}

impl Entry {
    pub fn connector(&self) -> &str {
        &self.connector
    }

    pub fn compute_connector(&mut self) {
        let mut s = String::new();
        for &last in &self.ancestors {
            if last {
                s.push_str("  ");
            } else {
                s.push(LINE_VERT);
                s.push(' ');
            }
        }
        if self.depth > 0 {
            s.push(if self.is_last { CORNER_BL } else { LINE_VERT });
            s.push(' ');
        }
        self.connector = s;
    }

    pub fn marker(&self, frame: usize) -> char {
        match self.kind {
            EntryType::Dir => {
                if self.is_open {
                    '*'
                } else {
                    ' '
                }
            }
            EntryType::Worktree => WORKTREE,
            EntryType::Agent => {
                if self.is_running {
                    SPINNER[frame % SPINNER.len()]
                } else {
                    CHECKED
                }
            }
        }
    }
}
