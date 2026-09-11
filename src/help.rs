use crate::config::Config;

pub const TEMPLATE: &str = include_str!("../config");

pub fn is_selectable_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return false;
    }
    // Must look like `key = value` — split at first '='
    if let Some((k, _v)) = trimmed.split_once('=') {
        !k.trim().is_empty()
    } else {
        false
    }
}

pub fn template_lines() -> Vec<String> {
    TEMPLATE.lines().map(|l| l.to_string()).collect()
}

pub fn selectable_indices(lines: &[String]) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| is_selectable_line(l))
        .map(|(i, _)| i)
        .collect()
}

pub fn key_at_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    trimmed.split_once('=').map(|(k, _)| k.trim().to_string())
}

pub fn key_at(lines: &[String], idx: usize) -> Option<String> {
    key_at_line(&lines[idx])
}

fn inline_suffix(line: &str) -> Option<String> {
    // return "# comment" part from "key = value # comment", preserving "#"
    let eq_pos = line.find('=')?;
    let after_eq = &line[eq_pos + 1..];
    let hash_pos = after_eq.find('#')?;
    Some(after_eq[hash_pos..].trim().to_string())
}

fn format_kv(k: &str, config: &Config, raw: &std::collections::HashMap<String, String>, tmpl: &str) -> String {
    if let Some(rv) = raw.get(k) {
        return format!("{k} = {rv}");
    }
    if let Some(v) = config.value_string(k) {
        if let Some(suf) = inline_suffix(tmpl) {
            return format!("{k} = {v} {suf}");
        }
        return format!("{k} = {v}");
    }
    tmpl.to_string()
}

pub fn raw_map_from_content(content: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = trimmed.split_once('=') {
            let key = k.trim().to_string();
            let value = v.trim().to_string();
            map.insert(key, value);
        } else if !trimmed.is_empty() {
            map.remove(trimmed);
        }
    }
    map
}

pub fn raw_file_map() -> std::collections::HashMap<String, String> {
    let Some(path) = Config::config_path() else {
        return Default::default();
    };
    let Ok(content) = std::fs::read_to_string(&path) else {
        return Default::default();
    };
    raw_map_from_content(&content)
}

pub fn display_lines(config: &Config) -> Vec<String> {
    let raw = raw_file_map();
    template_lines().iter().map(|l| if let Some(k) = key_at_line(l) { format_kv(&k, config, &raw, l) } else { l.clone() }).collect()
}

#[derive(Debug, Clone)]
pub struct Block {
    pub header: Vec<String>,
    pub entries: Vec<usize>, // line indices in template
}

pub fn parse_blocks(lines: &[String]) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        while i < lines.len() && lines[i].trim().is_empty() {
            i += 1;
        }
        if i >= lines.len() {
            break;
        }
        let mut j = i;
        while j < lines.len() && !lines[j].trim().is_empty() {
            j += 1;
        }
        let segment = &lines[i..j];
        let mut selectable = Vec::new();
        for (k, line) in segment.iter().enumerate() {
            if is_selectable_line(line) {
                selectable.push(i + k);
            }
        }
        if !selectable.is_empty() {
            let first_sel_offset = selectable[0] - i;
            let mut header = Vec::new();
            for k in 0..first_sel_offset {
                let line = &segment[k];
                if line.trim().starts_with('#') {
                    header.push(line.clone());
                }
            }
            // Only keep header if it's contiguous comments directly before first selectable
            // (which it is by construction, since segment has no blank)
            blocks.push(Block {
                header,
                entries: selectable,
            });
        }
        i = j + 1;
    }
    blocks
}

pub fn filtered_visible_lines(filter: &str, config: &Config) -> Vec<String> {
    let lines = template_lines();
    let blocks = parse_blocks(&lines);
    let filter_trim = filter.trim().to_lowercase();
    if filter_trim.is_empty() {
        return display_lines(config);
    }
    let words: Vec<String> = filter_trim
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    let raw = raw_file_map();
    let mut out = Vec::new();
    for block in blocks {
        let mut matching_entries = Vec::new();
        for &li in &block.entries {
            if let Some(k) = key_at(&lines, li) {
                let kl = k.to_lowercase();
                if words.iter().all(|w| kl.contains(w)) {
                    matching_entries.push((li, k));
                }
            }
        }
        if matching_entries.is_empty() {
            continue;
        }
        for h in &block.header {
            out.push(h.clone());
        }
        for (li, k) in matching_entries {
            out.push(format_kv(&k, config, &raw, &lines[li]));
        }
    }
    out
}

/// Surgical write: update only the edited key in the user's real file.
/// - If file content is `None` (no file yet), create a minimal file with just that key.
/// - If key exists, replace first occurrence's value (`key = <new>`). Preserve surrounding lines and inline comment.
/// - If key missing, append `key = <new>` at EOF (ensuring newline).
pub fn surgical_write(existing: Option<&str>, key: &str, new_value: &str) -> String {
    let new_line_base = format!("{key} = {new_value}");
    let Some(content) = existing else {
        return format!("{new_line_base}\n");
    };

    // Fast path: empty file
    if content.is_empty() {
        return format!("{new_line_base}\n");
    }

    let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
    let mut found = false;
    for line in &mut lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((k, _)) = trimmed.split_once('=') {
            if k.trim() == key {
                // preserve inline suffix from original line if any
                if let Some(suf) = inline_suffix(line) {
                    *line = format!("{new_line_base} {suf}");
                } else {
                    *line = new_line_base.clone();
                }
                found = true;
                break;
            }
        } else if trimmed == key {
            // bare key without `=` -> reset case, treat as match
            *line = new_line_base.clone();
            found = true;
            break;
        }
    }
    if !found {
        // Ensure we append after existing content, preserving final newline semantics.
        lines.push(new_line_base);
    }
    let mut out = lines.join("\n");
    // Preserve trailing newline if original had it or we appended.
    if content.ends_with('\n') || !found {
        out.push('\n');
    }
    out
}

pub fn surgical_delete(existing: Option<&str>, key: &str) -> Option<String> {
    let Some(content) = existing else {
        return None;
    };
    let mut lines: Vec<String> = Vec::new();
    let mut removed = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            lines.push(line.to_string());
            continue;
        }
        if let Some((k, _)) = trimmed.split_once('=') {
            if k.trim() == key {
                removed = true;
                continue;
            }
        } else if trimmed == key {
            removed = true;
            continue;
        }
        lines.push(line.to_string());
    }
    if !removed {
        return Some(content.to_string());
    }
    // Check if any selectable remains
    let has_selectable = lines.iter().any(|l| is_selectable_line(l));
    if !has_selectable {
        // No effective config left — signal to delete file.
        // Keep comments? Spec says delete local config if all defaults, so delete file.
        return None;
    }
    let mut out = lines.join("\n");
    if content.ends_with('\n') {
        out.push('\n');
    } else if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

fn write_atomic(target: &std::path::Path, content: &str) -> Result<(), String> {
    if let Some(parent) = target.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Err(format!("cannot write config file '{}': {e}", target.display()));
        }
    }
    let tmp = target.with_extension("tmp");
    if let Err(e) = std::fs::write(&tmp, content) {
        return Err(format!("cannot write config file '{}': {e}", target.display()));
    }
    if let Err(e) = std::fs::rename(&tmp, target) {
        if let Err(e2) = std::fs::write(target, content) {
            return Err(format!("cannot write config file '{}': {e2} (rename also failed: {e})", target.display()));
        }
    }
    Ok(())
}

pub fn commit_to_disk(key: &str, new_value: &str) -> Result<(), String> {
    let target = Config::write_target();
    let existing = std::fs::read_to_string(&target).ok();
    if Config::is_default_value(key, new_value) {
        let Some(new_content) = surgical_delete(existing.as_deref(), key) else {
            if target.exists() { let _ = std::fs::remove_file(&target); }
            return Ok(());
        };
        if let Some(ref ec) = existing { if &new_content == ec { return Ok(()); } }
        return write_atomic(&target, &new_content);
    }
    let new_content = surgical_write(existing.as_deref(), key, new_value);
    write_atomic(&target, &new_content)
}

