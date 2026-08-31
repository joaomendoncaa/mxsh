use crate::config::Config;
use crate::git::GitCache;
use crate::model::{
    Changes, Entry, EntryType, Goto, Opencode, TmuxPane, TmuxSession, WorktreeInfo,
};
use crate::opencode;
use crate::tmux;
use crate::util;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub struct TreeBuilder {
    git_cache: GitCache,
}

impl TreeBuilder {
    pub fn new() -> Self {
        TreeBuilder {
            git_cache: GitCache::new(),
        }
    }

    pub fn load_disk_cache(&self, cache: &crate::git::DiskCache) {
        self.git_cache.load_disk(cache);
    }
    pub fn to_disk_cache(&self) -> crate::git::DiskCache {
        self.git_cache.to_disk()
    }

    pub fn build(&self, config: &Config) -> Vec<Entry> {
        let (tmux_snapshot, oc_sessions) = std::thread::scope(|s| {
            let tmux_h = s.spawn(tmux::snapshot);
            let oc_h = s.spawn(opencode::list_sessions);
            (tmux_h.join().unwrap(), oc_h.join().unwrap())
        });
        let sessions = tmux_snapshot.sessions;
        let panes = tmux_snapshot.panes;

        let oc_panes = tmux::opencode_panes(&panes);
        let pane_sessions = match_panes_to_sessions(&oc_panes, &oc_sessions);

        let dirs = self.parse_directories(&config.path);
        let open: HashSet<PathBuf> = dirs
            .iter()
            .filter(|d| dir_is_open(d, &sessions, &panes))
            .map(|d| d.path.clone())
            .collect();

        let git_data = self.git_phase(&dirs, &open, config);

        let covered_paths: Vec<PathBuf> = dirs.iter().map(|d| d.path.clone()).collect();
        let covered_names: Vec<String> = dirs.iter().map(|d| d.name.clone()).collect();

        let branches: Vec<Option<String>> = {
            let n = dirs.len();
            if n == 0 {
                Vec::new()
            } else if n == 1 {
                vec![
                    self.git_cache
                        .branch(&dirs[0].path)
                        .filter(|b| b != "master" && b != "main")
                        .filter(|_| {
                            let active = open.contains(&dirs[0].path);
                            !(active && config.hide_hints_branches_active
                                || !active && config.hide_hints_branches_inactive)
                        }),
                ]
            } else {
                let threads = std::thread::available_parallelism()
                    .map(|p| p.get())
                    .unwrap_or(4)
                    .min(n)
                    .min(8);
                let chunk = n.div_ceil(threads);
                let mut out: Vec<Option<Option<String>>> = (0..n).map(|_| None).collect();
                std::thread::scope(|s| {
                    for (chunk_idx, out_chunk) in out.chunks_mut(chunk).enumerate() {
                        let start = chunk_idx * chunk;
                        let dirs = &dirs;
                        let open = &open;
                        let config = config;
                        let cache = &self.git_cache;
                        s.spawn(move || {
                            for (i, slot) in out_chunk.iter_mut().enumerate() {
                                let idx = start + i;
                                if idx >= dirs.len() {
                                    break;
                                }
                                let b = cache
                                    .branch(&dirs[idx].path)
                                    .filter(|v| v != "master" && v != "main")
                                    .filter(|_| {
                                        let active = open.contains(&dirs[idx].path);
                                        !(active && config.hide_hints_branches_active
                                            || !active && config.hide_hints_branches_inactive)
                                    });
                                *slot = Some(b);
                            }
                        });
                    }
                });
                out.into_iter().map(|o| o.unwrap()).collect()
            }
        };

        let mut dir_entries: Vec<DirEntry> = Vec::with_capacity(dirs.len());
        for ((d, git), branch) in dirs.iter().zip(git_data.iter()).zip(branches) {
            dir_entries.push(build_dir_entry(
                d,
                git,
                branch,
                &sessions,
                &panes,
                &pane_sessions,
            ));
        }
        for s in tmux::list_external_sessions(&sessions, &covered_paths, &covered_names) {
            dir_entries.push(external_dir_entry(&s, &panes));
        }

        let mut open_entries: Vec<&DirEntry> = dir_entries.iter().filter(|e| e.is_open).collect();
        let mut closed: Vec<&DirEntry> = dir_entries.iter().filter(|e| !e.is_open).collect();
        open_entries.sort_by(|a, b| b.name.cmp(&a.name));
        closed.sort_by(|a, b| b.name.cmp(&a.name));

        let total = open_entries.len() + closed.len();
        let mut rows = Vec::new();
        let mut pos = 0;
        for entry in &closed {
            push_entry(entry, pos == total - 1, &mut rows);
            pos += 1;
        }
        for entry in &open_entries {
            push_entry(entry, pos == total - 1, &mut rows);
            pos += 1;
        }

        rows
    }

    fn git_phase(&self, dirs: &[DirInfo], open: &HashSet<PathBuf>, config: &Config) -> Vec<DirGit> {
        let n = dirs.len();
        if n == 0 {
            return Vec::new();
        }
        if n == 1 {
            let dir = &dirs[0];
            let worktrees = self.git_cache.worktrees(&dir.path);
            let worktree_diffs: HashMap<PathBuf, Changes> = worktrees
                .iter()
                .filter(|wt| !wt.is_main)
                .map(|wt| (wt.path.clone(), self.git_cache.diff(&wt.path)))
                .collect();
            let main_diff = if !open.contains(&dir.path) && config.hide_changes_inactive {
                Changes::default()
            } else {
                self.git_cache.diff(&dir.path)
            };
            return vec![DirGit {
                worktrees,
                worktree_diffs,
                main_diff,
            }];
        }
        let threads = std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(4)
            .min(n)
            .min(8);
        let chunk = n.div_ceil(threads);
        let mut out: Vec<Option<DirGit>> = (0..n).map(|_| None).collect();
        std::thread::scope(|s| {
            for (chunk_idx, out_chunk) in out.chunks_mut(chunk).enumerate() {
                let start = chunk_idx * chunk;
                let dirs = dirs;
                let open = open;
                let config = config;
                let cache = &self.git_cache;
                s.spawn(move || {
                    for (i, slot) in out_chunk.iter_mut().enumerate() {
                        let idx = start + i;
                        if idx >= dirs.len() {
                            break;
                        }
                        let dir = &dirs[idx];
                        let worktrees = cache.worktrees(&dir.path);
                        let worktree_diffs: HashMap<PathBuf, Changes> = worktrees
                            .iter()
                            .filter(|wt| !wt.is_main)
                            .map(|wt| (wt.path.clone(), cache.diff(&wt.path)))
                            .collect();
                        let main_diff = if !open.contains(&dir.path) && config.hide_changes_inactive
                        {
                            Changes::default()
                        } else {
                            cache.diff(&dir.path)
                        };
                        *slot = Some(DirGit {
                            worktrees,
                            worktree_diffs,
                            main_diff,
                        });
                    }
                });
            }
        });
        out.into_iter().map(|o| o.unwrap()).collect()
    }

    fn parse_directories(&self, path_config: &str) -> Vec<DirInfo> {
        let mut dirs = Vec::new();
        for raw in path_config.split(':') {
            if raw.is_empty() {
                continue;
            }
            let is_glob = raw.ends_with("/*");
            let base = if is_glob {
                raw.trim_end_matches("/*")
            } else {
                raw
            };
            let expanded = util::expand_tilde(base);
            if !expanded.is_dir() {
                continue;
            }
            if is_glob {
                if let Ok(entries) = std::fs::read_dir(&expanded) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.is_dir() {
                            dirs.push(DirInfo {
                                name: path.file_name().unwrap().to_string_lossy().into(),
                                path,
                            });
                        }
                    }
                }
            } else {
                dirs.push(DirInfo {
                    name: expanded.file_name().unwrap().to_string_lossy().into(),
                    path: expanded,
                });
            }
        }
        dirs.sort_by(|a, b| a.name.cmp(&b.name));
        dirs
    }
}

struct DirGit {
    worktrees: Vec<WorktreeInfo>,
    worktree_diffs: HashMap<PathBuf, Changes>,
    main_diff: Changes,
}

fn build_dir_entry(
    dir: &DirInfo,
    git: &DirGit,
    branch: Option<String>,
    sessions: &[TmuxSession],
    panes: &[TmuxPane],
    pane_sessions: &[PaneSession],
) -> DirEntry {
    let worktrees: Vec<WorktreeInfo> = git
        .worktrees
        .iter()
        .filter(|wt| !wt.is_main)
        .filter(|wt| {
            let diff = git
                .worktree_diffs
                .get(&wt.path)
                .cloned()
                .unwrap_or_default();
            sessions.iter().any(|s| s.path == wt.path)
                || pane_sessions
                    .iter()
                    .any(|ps| is_in(&ps.session.directory, &wt.path))
                || !diff.has_none()
        })
        .cloned()
        .collect();
    let worktree_diffs: Vec<Changes> = worktrees
        .iter()
        .map(|wt| {
            git.worktree_diffs
                .get(&wt.path)
                .cloned()
                .unwrap_or_default()
        })
        .collect();
    let changes = worktree_diffs
        .iter()
        .fold(git.main_diff.clone(), |acc, d| acc.add(d));
    let sessions_here = dir_sessions(dir, &worktrees, pane_sessions);
    let worktree_sessions = worktree_sessions(&worktrees, pane_sessions);
    DirEntry {
        name: dir.name.clone(),
        path: dir.path.clone(),
        is_open: dir_is_open(dir, sessions, panes),
        changes,
        branch,
        worktrees,
        worktree_diffs,
        sessions: sessions_here,
        worktree_sessions,
    }
}

fn external_dir_entry(s: &TmuxSession, _panes: &[TmuxPane]) -> DirEntry {
    DirEntry {
        name: s.name.clone(),
        path: s.path.clone(),
        is_open: true,
        changes: Changes::default(),
        branch: None,
        worktrees: vec![],
        worktree_diffs: vec![],
        sessions: vec![],
        worktree_sessions: vec![],
    }
}

fn dir_is_open(dir: &DirInfo, sessions: &[TmuxSession], panes: &[TmuxPane]) -> bool {
    sessions
        .iter()
        .any(|s| s.name == dir.name || s.path == dir.path)
        || panes.iter().any(|p| is_in(&p.current_path, &dir.path))
}

fn dir_sessions(
    dir: &DirInfo,
    worktrees: &[WorktreeInfo],
    pane_sessions: &[PaneSession],
) -> Vec<PaneSession> {
    pane_sessions
        .iter()
        .filter(|ps| {
            ps.session.directory == dir.path
                || (is_in(&ps.session.directory, &dir.path)
                    && !worktrees
                        .iter()
                        .any(|wt| is_in(&ps.session.directory, &wt.path)))
        })
        .cloned()
        .collect()
}

fn worktree_sessions(
    worktrees: &[WorktreeInfo],
    pane_sessions: &[PaneSession],
) -> Vec<Vec<PaneSession>> {
    worktrees
        .iter()
        .map(|wt| {
            pane_sessions
                .iter()
                .filter(|ps| is_in(&ps.session.directory, &wt.path))
                .cloned()
                .collect()
        })
        .collect()
}

fn push_entry(entry: &DirEntry, is_last_dir: bool, rows: &mut Vec<Entry>) {
    let dir_idx = rows.len();
    let search_text = entry
        .branch
        .as_ref()
        .map(|b| format!("{} {b}", entry.name))
        .unwrap_or_else(|| entry.name.clone());
    rows.push(finalize_entry(Entry {
        kind: EntryType::Dir,
        label: entry.name.clone(),
        path: entry.path.clone(),
        changes: if entry.changes.has_none() {
            None
        } else {
            Some(entry.changes.clone())
        },
        branch: entry.branch.clone(),
        is_open: entry.is_open,
        is_running: false,
        depth: 0,
        ancestors: vec![],
        is_last: is_last_dir,
        search_text,
        goto: Some(Goto {
            session: entry.name.clone(),
            path: entry.path.clone(),
            window: None,
            pane: None,
        }),
        parent: None,
        connector: String::new(),
        search_text_lower: String::new(),
    }));
    let total_children = entry.sessions.len() + entry.worktrees.len();
    if total_children == 0 {
        return;
    }
    let mut child = 0;
    for ps in &entry.sessions {
        let is_last = child == total_children - 1;
        child += 1;
        rows.push(finalize_entry(Entry {
            kind: EntryType::Agent,
            label: ps.session.title.clone(),
            path: ps.session.directory.clone(),
            changes: None,
            branch: None,
            is_open: false,
            is_running: ps.session.is_running,
            depth: 1,
            ancestors: vec![],
            is_last,
            search_text: format!("{} {}", entry.name, ps.session.title),
            goto: Some(Goto {
                session: ps.pane.session_name.clone(),
                path: ps.session.directory.clone(),
                window: Some(ps.pane.window_index),
                pane: Some(ps.pane.pane_index),
            }),
            parent: Some(dir_idx),
            connector: String::new(),
            search_text_lower: String::new(),
        }));
    }
    for (wi, wt) in entry.worktrees.iter().enumerate() {
        let is_last = child == total_children - 1;
        child += 1;
        let wt_idx = rows.len();
        let wt_name = wt
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let wt_diff = &entry.worktree_diffs[wi];
        rows.push(finalize_entry(Entry {
            kind: EntryType::Worktree,
            label: wt_name.clone(),
            path: wt.path.clone(),
            changes: if wt_diff.has_none() {
                None
            } else {
                Some(wt_diff.clone())
            },
            branch: None,
            is_open: false,
            is_running: false,
            depth: 1,
            ancestors: vec![],
            is_last,
            search_text: format!("{} {}", entry.name, wt_name),
            goto: Some(Goto {
                session: wt_name.clone(),
                path: wt.path.clone(),
                window: None,
                pane: None,
            }),
            parent: Some(dir_idx),
            connector: String::new(),
            search_text_lower: String::new(),
        }));
        let wt_sessions: &_ = &entry.worktree_sessions[wi];
        let s_total = wt_sessions.len();
        for (si, ps) in wt_sessions.iter().enumerate() {
            rows.push(finalize_entry(Entry {
                kind: EntryType::Agent,
                label: ps.session.title.clone(),
                path: ps.session.directory.clone(),
                changes: None,
                branch: None,
                is_open: false,
                is_running: ps.session.is_running,
                depth: 2,
                ancestors: vec![is_last_dir],
                is_last: si == s_total - 1,
                search_text: format!("{} {} {}", entry.name, wt_name, ps.session.title),
                goto: Some(Goto {
                    session: ps.pane.session_name.clone(),
                    path: ps.session.directory.clone(),
                    window: Some(ps.pane.window_index),
                    pane: Some(ps.pane.pane_index),
                }),
                parent: Some(wt_idx),
                connector: String::new(),
                search_text_lower: String::new(),
            }));
        }
    }
}

struct DirInfo {
    name: String,
    path: PathBuf,
}

struct DirEntry {
    name: String,
    path: PathBuf,
    is_open: bool,
    changes: Changes,
    branch: Option<String>,
    worktrees: Vec<WorktreeInfo>,
    worktree_diffs: Vec<Changes>,
    sessions: Vec<PaneSession>,
    worktree_sessions: Vec<Vec<PaneSession>>,
}

#[derive(Clone)]
struct PaneSession {
    pane: TmuxPane,
    session: Opencode,
}

fn synthetic(p: &TmuxPane) -> Opencode {
    Opencode {
        id: format!(
            "synthetic:{}:{}:{}",
            p.session_name, p.window_index, p.pane_index
        ),
        title: "New session".into(),
        directory: p.current_path.clone(),
        time_updated: p.activity,
        is_running: false,
    }
}

fn match_panes_to_sessions(panes: &[&TmuxPane], sessions: &[Opencode]) -> Vec<PaneSession> {
    // a pane is an opencode pane iff `tmux::opencode_panes`
    // classified it by `pane_current_command` the correlation to
    // opencode's api is done with the path and recency. pick the most
    // recently updated session whose `directory` contains the pane's cwd.
    // The session's own `title`/`is_running` are used verbatim
    let mut used = HashSet::new();
    let mut sorted = panes.to_vec();
    sorted.sort_by_key(|b| std::cmp::Reverse(b.activity));
    let mut out = Vec::with_capacity(sorted.len());
    for p in &sorted {
        let best = sessions
            .iter()
            .filter(|s| !used.contains(&s.id) && is_in(&p.current_path, &s.directory))
            .max_by_key(|s| s.time_updated);
        if let Some(s) = best {
            used.insert(s.id.clone());
            out.push(PaneSession {
                pane: (*p).clone(),
                session: s.clone(),
            });
        } else {
            out.push(PaneSession {
                pane: (*p).clone(),
                session: synthetic(p),
            });
        }
    }
    out
}

fn is_in(path: &Path, base: &Path) -> bool {
    path == base || path.starts_with(base)
}

fn finalize_entry(mut entry: Entry) -> Entry {
    entry.compute_connector();
    entry.search_text_lower = entry.search_text.to_lowercase();
    entry
}
