use crate::model::{FeedbackEntry, FeedbackType};
use crate::util;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub type CSV = Vec<String>;

fn default_path() -> String {
    std::env::var("HOME")
        .map(|h| format!("{h}/Projects/*"))
        .unwrap_or_else(|_| "~/Projects/*".to_string())
}

fn strip_inline_value(s: &str) -> String {
    if let Some(idx) = s.find('#') {
        s[..idx].trim().to_string()
    } else {
        s.trim().to_string()
    }
}

fn canonical_key(key: &str) -> String {
    key.replace('-', "_")
}

macro_rules! config {
    ( $( $field:ident : $ty:ident = $default:expr ),* $(,)? ) => {
        #[derive(Clone, Debug, Serialize, Deserialize)]
        pub struct Config {
            $(
                pub $field: $ty,
            )*
        }

        impl Default for Config {
            fn default() -> Self {
                Self {
                    $($field: $default,)*
                }
            }
        }

        impl Config {
            pub fn value_string(&self, key: &str) -> Option<String> {
                let key = canonical_key(key);
                match key.as_str() {
                    $(stringify!($field) => Some(config!(@str $field : $ty, &self.$field)),)*
                    _ => None,
                }
            }

            fn set_field(&mut self, key: &str, value: &str, feedbacks: &mut Vec<FeedbackEntry>) -> bool {
                let normalized = canonical_key(key);
                $(
                    if normalized == stringify!($field) {
                        return config!(@set $field : $ty, &mut self.$field, key, value, feedbacks);
                    }
                )*
                false
            }

            fn reset_field(&mut self, key: &str) -> bool {
                let d = Config::default();
                let key = canonical_key(key);
                match key.as_str() {
                    $(stringify!($field) => { self.$field = d.$field; true },)*
                    _ => false,
                }
            }
        }
    };
    (@set path : String, $slot:expr, $key:expr, $value:expr, $fb:expr) => { {
        match parse_path_list($key, $value) {
            Ok(v) => *$slot = v,
            Err(m) => $fb.push(FeedbackEntry { level: FeedbackType::Error, message: m }),
        }
        true
    } };
    (@set $field:ident : CSV, $slot:expr, $key:expr, $value:expr, $fb:expr) => { { *$slot = parse_csv($value); true } };
    (@set $field:ident : String, $slot:expr, $key:expr, $value:expr, $fb:expr) => { { *$slot = parse_string($value); true } };
    (@set $field:ident : char, $slot:expr, $key:expr, $value:expr, $fb:expr) => { {
        match parse_char($key, $value) {
            Ok(v) => *$slot = v,
            Err(m) => $fb.push(FeedbackEntry { level: FeedbackType::Error, message: m }),
        }
        true
    } };
    (@set $field:ident : bool, $slot:expr, $key:expr, $value:expr, $fb:expr) => { {
        match parse_bool($key, $value) {
            Ok(v) => *$slot = v,
            Err(m) => $fb.push(FeedbackEntry { level: FeedbackType::Error, message: m }),
        }
        true
    } };
    (@set $field:ident : u64, $slot:expr, $key:expr, $value:expr, $fb:expr) => { {
        match parse_u64($key, $value) {
            Ok(v) => *$slot = v,
            Err(m) => $fb.push(FeedbackEntry { level: FeedbackType::Error, message: m }),
        }
        true
    } };
    (@str $field:ident : CSV, $slot:expr) => { $slot.join(", ") };
    (@str $field:ident : $ty:ident, $slot:expr) => { $slot.to_string() };
}

config! {
    path                        : String = default_path(),
    path_worktrees              : String = String::new(),
    bind_jumpto                 : String = "enter".to_string(),
    bind_command_mode           : char   = ':',
    bind_help                   : char   = '?',
    bind_command_session_kill   : String = "k".to_string(),
    bind_command_worktree_new   : String = "n".to_string(),
    bind_command_worktree_delete: String = "d".to_string(),
    bind_quit                   : String = "esc,ctrl-c,ctrl-q".to_string(),
    bind_nav_up                 : String = "up,ctrl-p".to_string(),
    bind_nav_down               : String = "down,ctrl-n".to_string(),
    bind_nav_page_up            : String = "ctrl-u".to_string(),
    bind_nav_page_down          : String = "ctrl-d".to_string(),
    bind_input_left             : String = "left".to_string(),
    bind_input_right            : String = "right".to_string(),
    bind_input_home             : String = "ctrl-a".to_string(),
    bind_input_end              : String = "ctrl-e".to_string(),
    bind_input_kill_line        : String = "ctrl-k".to_string(),
    bind_input_delete_word      : String = "ctrl-w".to_string(),
    bind_input_clear            : String = "ctrl-r".to_string(),
    bind_input_word_left        : String = "alt-b".to_string(),
    bind_input_word_right       : String = "alt-f".to_string(),
    bind_input_backspace        : String = "backspace".to_string(),
    bind_input_delete           : String = "delete".to_string(),
    bind_command_exit           : String = "esc,backspace".to_string(),
    bind_command_open_detached  : String = "o".to_string(),
    bind_help_enter             : String = "enter".to_string(),
    bind_help_exit              : String = "esc".to_string(),
    auto_close                  : bool   = true,
    daemon_timeout              : u64    = 1800,
    hide_changes_inactive       : bool   = false,
    hide_changes_active         : bool   = false,
    hide_changes_worktree       : bool   = false,
    hide_hints_footer           : bool   = false,
    hide_hints_branches_active  : bool   = false,
    hide_hints_branches_inactive: bool   = false,
    hide_hints_remotes_active   : bool   = false,
    hide_hints_remotes_inactive : bool   = false,
    style_icon_daemon_loading   : String = "─╲│╱".to_string(),
    style_icon_daemon_ready     : String = "✓".to_string(),
    style_icon_active           : String = "*".to_string(),
    style_icon_worktree         : String = "⑂".to_string(),
    style_icon_agent_idle       : String = "✓".to_string(),
    style_icon_agent_running    : String = "⠋⠙⠹⠸⢰⣰⣠⣄⣆⡆⠇⠏".to_string(),
    style_icon_input            : String = "▸".to_string(),
    style_entries_gap           : u64    = 0,
    opencode_agents_ignored     : CSV    = Vec::new(),
}

impl Config {
    pub fn daemon_timeout_duration(&self) -> Duration {
        Duration::from_secs(self.daemon_timeout)
    }

    pub(crate) fn config_base() -> PathBuf {
        match std::env::var("XDG_CONFIG_HOME") {
            Ok(x) if !x.is_empty() => PathBuf::from(x),
            _ => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"),
        }
    }

    pub fn config_path() -> Option<PathBuf> {
        let ramo_dir = Self::config_base().join("ramo");
        for name in ["config", "config.ramo"] {
            let p = ramo_dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
        None
    }

    pub fn config_dir() -> Option<PathBuf> {
        let dir = Self::config_base().join("ramo");
        dir.is_dir().then_some(dir)
    }

    pub fn write_target() -> PathBuf {
        Self::config_path().unwrap_or_else(|| Self::config_base().join("ramo/config"))
    }

    pub fn is_default_value(key: &str, new_value: &str) -> bool {
        let def = Config::default();
        let Some(def_val) = def.value_string(key) else {
            return false;
        };
        let new_trim = new_value.trim();
        if matches!(canonical_key(key).as_str(), "path" | "path_worktrees") {
            return util::expand_tilde(new_trim) == util::expand_tilde(&def_val);
        }
        def_val == new_trim
    }

    pub fn new() -> (Self, Vec<FeedbackEntry>) {
        Self::load(&[])
    }

    pub fn load(overrides: &[(String, Option<String>)]) -> (Self, Vec<FeedbackEntry>) {
        match Self::config_path() {
            Some(path) => {
                let (mut c, mut fb) = Self::load_from_file(&path);
                fb.extend(c.apply_overrides(overrides));
                (c, fb)
            }
            None => {
                eprintln!(
                    "ramo: no config file found in $XDG_CONFIG_HOME/ramo/ or $HOME/.config/ramo/ \
                     — using defaults"
                );
                let mut c = Config::default();
                let fb = c.apply_overrides(overrides);
                (c, fb)
            }
        }
    }

    pub fn load_from_file(path: &Path) -> (Self, Vec<FeedbackEntry>) {
        match std::fs::read_to_string(path) {
            Ok(content) => Self::parse_content(path, &content),
            Err(e) => (
                Config::default(),
                vec![FeedbackEntry {
                    level: FeedbackType::Error,
                    message: format!("cannot read config file '{}': {e}", path.display()),
                }],
            ),
        }
    }

    pub(crate) fn parse_content(path: &Path, content: &str) -> (Self, Vec<FeedbackEntry>) {
        let mut config = Config::default();
        let mut feedbacks = Vec::new();

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let (key, value) = match trimmed.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => (trimmed, ""),
            };

            let value = strip_inline_value(value);
            if value.is_empty() {
                if !config.reset_field(key) {
                    feedbacks.push(FeedbackEntry {
                        level: FeedbackType::Warning,
                        message: format!("'{key}' isn't a valid config key in {}", path.display()),
                    });
                }
                continue;
            }

            config.set_field(key, &value, &mut feedbacks);
        }

        (config, feedbacks)
    }

    pub fn apply_overrides(
        &mut self,
        overrides: &[(String, Option<String>)],
    ) -> Vec<FeedbackEntry> {
        let mut feedbacks = Vec::new();
        for (key, val) in overrides {
            match val {
                Some(v) => {
                    if !self.set_field(key, v, &mut feedbacks) {
                        feedbacks.push(FeedbackEntry {
                            level: FeedbackType::Warning,
                            message: format!("'{key}' isn't a valid flag"),
                        });
                    }
                }
                None => match canonical_key(key).as_str() {
                    "auto_close"
                    | "hide_changes_inactive"
                    | "hide_changes_active"
                    | "hide_changes_worktree"
                    | "hide_hints_footer"
                    | "hide_hints_branches_active"
                    | "hide_hints_branches_inactive"
                    | "hide_hints_remotes_active"
                    | "hide_hints_remotes_inactive" => {
                        self.set_field(key, "true", &mut feedbacks);
                    }
                    _ => feedbacks.push(FeedbackEntry {
                        level: FeedbackType::Error,
                        message: format!("--{key} requires a value (e.g. --{key}=value)"),
                    }),
                },
            }
        }
        feedbacks
    }

    pub fn key_matches(spec: &str, key: KeyEvent) -> bool {
        for token in spec.split(',') {
            let t = token.trim();
            if t.is_empty() {
                continue;
            }
            if token_matches(t, key) {
                return true;
            }
        }
        false
    }
}

fn token_matches(token: &str, key: KeyEvent) -> bool {
    let t = token.trim().to_lowercase();
    if t.is_empty() {
        return false;
    }
    let parts: Vec<&str> = t.split('-').collect();
    let (mods, key_part) = if parts.len() == 1 {
        (Vec::new(), parts[0])
    } else {
        (parts[..parts.len() - 1].to_vec(), parts[parts.len() - 1])
    };
    let mut need_ctrl = false;
    let mut need_alt = false;
    let mut need_shift = false;
    for m in mods {
        match m {
            "ctrl" | "control" => need_ctrl = true,
            "alt" => need_alt = true,
            "shift" => need_shift = true,
            _ => return false,
        }
    }
    let has_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let has_alt = key.modifiers.contains(KeyModifiers::ALT);
    let has_shift = key.modifiers.contains(KeyModifiers::SHIFT);
    if need_ctrl != has_ctrl {
        return false;
    }
    if need_alt != has_alt {
        return false;
    }
    if need_shift && !has_shift {
        return false;
    }
    match key_part {
        "enter" => key.code == KeyCode::Enter,
        "esc" | "escape" => key.code == KeyCode::Esc,
        "up" => key.code == KeyCode::Up,
        "down" => key.code == KeyCode::Down,
        "left" => key.code == KeyCode::Left,
        "right" => key.code == KeyCode::Right,
        "backspace" | "bs" => key.code == KeyCode::Backspace,
        "delete" | "del" => key.code == KeyCode::Delete,
        "tab" => key.code == KeyCode::Tab,
        "space" => key.code == KeyCode::Char(' '),
        _ => {
            if key_part.chars().count() == 1 {
                let ch = key_part.chars().next().unwrap();
                if let KeyCode::Char(c) = key.code {
                    c.to_ascii_lowercase() == ch
                } else {
                    false
                }
            } else {
                false
            }
        }
    }
}

fn parse_string(value: &str) -> String {
    value.to_string()
}

fn parse_csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_path_list(name: &str, value: &str) -> Result<String, String> {
    for raw in value.split(':') {
        if raw.is_empty() {
            continue;
        }
        let base = raw.trim_end_matches("/*");
        let expanded = util::expand_tilde(base);
        if !expanded.is_dir() {
            return Err(format!(
                "'{name}' has invalid value {value:?} — segment '{raw}' does not point to a real directory (resolved to {}), falling back to default",
                expanded.display()
            ));
        }
    }
    Ok(value.to_string())
}

fn parse_char(name: &str, value: &str) -> Result<char, String> {
    let mut chars = value.chars();
    let Some(c) = chars.next() else {
        return Err(format!(
            "'{name}' has invalid value {value:?} — expected a single character, falling back to default"
        ));
    };
    if chars.next().is_some() {
        return Err(format!(
            "'{name}' has invalid value {value:?} — expected a single character, falling back to default"
        ));
    }
    Ok(c)
}

fn parse_bool(name: &str, value: &str) -> Result<bool, String> {
    match value.trim() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!(
            "'{name}' has invalid value {value:?} — expected true/false, 1/0, yes/no, or on/off, falling back to default"
        )),
    }
}

fn parse_u64(name: &str, value: &str) -> Result<u64, String> {
    match value.trim().parse::<u64>() {
        Ok(n) => Ok(n),
        Err(_) => Err(format!(
            "'{name}' has invalid value {value:?} — expected a non-negative integer, falling back to default"
        )),
    }
}
