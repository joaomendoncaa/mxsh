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

        // A scanned dir that is itself a linked worktree of another scanned
        // repo is no top-level dir: it already shows as that repo's child
        // (otherwise the glob re-adds every new sibling worktree on top,
        // each even listing itself as its own child).
        let keep = keep_mask(&dirs, &git_data);
        let mut kept_dirs = Vec::with_capacity(dirs.len());
        let mut kept_git = Vec::with_capacity(git_data.len());
        for (k, (d, g)) in keep.into_iter().zip(dirs.into_iter().zip(git_data)) {
            if k {
                kept_dirs.push(d);
                kept_git.push(g);
            }
        }
        let (dirs, git_data) = (kept_dirs, kept_git);

        let mut covered_paths: Vec<PathBuf> = dirs.iter().map(|d| d.path.clone()).collect();
        // Sessions living in a worktree are represented as its child, never as external.
        covered_paths.extend(git_data.iter().flat_map(|g| g.worktrees.iter().map(|w| w.path.clone())));
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

        let (mut open_roots, mut quiet_roots) = split_roots(&mut dir_entries);
        open_roots.sort_by(|a, b| b.label.cmp(&a.label));
        quiet_roots.sort_by(|a, b| b.label.cmp(&a.label));

        let mut open_entries: Vec<&DirEntry> = dir_entries.iter().filter(|e| e.is_open).collect();
        let mut closed: Vec<&DirEntry> = dir_entries.iter().filter(|e| !e.is_open).collect();
        open_entries.sort_by(|a, b| b.name.cmp(&a.name));
        closed.sort_by(|a, b| b.name.cmp(&a.name));

        let total = open_entries.len() + closed.len() + open_roots.len() + quiet_roots.len();
        let mut rows = Vec::new();
        let mut pos = 0;
        // Closed dirs and quiet worktree roots share the closed section, by name.
        let (mut i, mut j) = (0, 0);
        while i < closed.len() || j < quiet_roots.len() {
            let take_dir = match (closed.get(i), quiet_roots.get(j)) {
                (Some(d), Some(w)) => d.name >= w.label,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            if take_dir {
                push_entry(closed[i], pos == total - 1, &mut rows);
                i += 1;
            } else {
                push_wt_root(&quiet_roots[j], pos == total - 1, &mut rows);
                j += 1;
            }
            pos += 1;
        }
        // Open dirs and hoisted worktree roots share the open section, by name.
        let (mut i, mut j) = (0, 0);
        while i < open_entries.len() || j < open_roots.len() {
            let take_dir = match (open_entries.get(i), open_roots.get(j)) {
                (Some(d), Some(w)) => d.name >= w.label,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            if take_dir {
                push_entry(open_entries[i], pos == total - 1, &mut rows);
                i += 1;
            } else {
                push_wt_root(&open_roots[j], pos == total - 1, &mut rows);
                j += 1;
            }
            pos += 1;
        }

        rows
    }

    fn git_phase(&self, dirs: &[DirInfo], open: &HashSet<PathBuf>, config: &Config) -> Vec<DirGit> {
        // Non-main worktrees are first-class: always listed, like directories.
        let wts_of = |dir: &DirInfo| -> (Vec<WorktreeInfo>, HashMap<PathBuf, Changes>, Vec<Option<String>>) {
            let worktrees: Vec<WorktreeInfo> = self
                .git_cache
                .worktrees(&dir.path)
                .into_iter()
                .filter(|wt| !wt.is_main)
                .collect();
            let worktree_diffs: HashMap<PathBuf, Changes> = if config.hide_changes_worktree {
                HashMap::new()
            } else {
                worktrees
                    .iter()
                    .map(|wt| (wt.path.clone(), self.git_cache.diff(&wt.path)))
                    .collect()
            };
            let hide_branch = if open.contains(&dir.path) {
                config.hide_hints_branches_active
            } else {
                config.hide_hints_branches_inactive
            };
            let worktree_branches: Vec<Option<String>> = worktrees
                .iter()
                .map(|wt| {
                    self.git_cache
                        .branch(&wt.path)
                        .filter(|b| b != "master" && b != "main")
                        .filter(|_| !hide_branch)
                })
                .collect();
            (worktrees, worktree_diffs, worktree_branches)
        };
        let n = dirs.len();
        if n == 0 {
            return Vec::new();
        }
        if n == 1 {
            let dir = &dirs[0];
            let (worktrees, worktree_diffs, worktree_branches) = wts_of(dir);
            let main_diff = if !open.contains(&dir.path) && config.hide_changes_inactive {
                Changes::default()
            } else {
                self.git_cache.diff(&dir.path)
            };
            return vec![DirGit {
                worktrees,
                worktree_diffs,
                worktree_branches,
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
                        let (worktrees, worktree_diffs, worktree_branches) = wts_of(dir);
                        let main_diff = if !open.contains(&dir.path) && config.hide_changes_inactive
                        {
                            Changes::default()
                        } else {
                            cache.diff(&dir.path)
                        };
                        *slot = Some(DirGit {
                            worktrees,
                            worktree_diffs,
                            worktree_branches,
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
    worktree_branches: Vec<Option<String>>,
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
    let mut worktrees: Vec<WtEntry> = git
        .worktrees
        .iter()
        .enumerate()
        .map(|(wi, wt)| {
            let wt_sessions: Vec<PaneSession> = pane_sessions
                .iter()
                .filter(|ps| is_in(&ps.session.directory, &wt.path))
                .cloned()
                .collect();
            WtEntry {
                is_open: sessions.iter().any(|s| s.path == wt.path) || !wt_sessions.is_empty(),
                diff: git.worktree_diffs.get(&wt.path).cloned().unwrap_or_default(),
                branch: git.worktree_branches.get(wi).cloned().flatten(),
                sessions: wt_sessions,
                info: wt.clone(),
            }
        })
        .collect();
    worktrees.sort_by_key(|w| wt_label(&w.info));
    let changes = worktrees
        .iter()
        .map(|w| &w.diff)
        .fold(git.main_diff.clone(), |acc, d| acc.add(d));
    DirEntry {
        name: dir.name.clone(),
        path: dir.path.clone(),
        is_open: dir_is_open(dir, sessions, panes),
        changes,
        branch,
        sessions: dir_sessions(dir, &git.worktrees, pane_sessions),
        worktrees,
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
        sessions: vec![],
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

fn wt_label(wt: &WorktreeInfo) -> String {
    wt.path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn push_agent_row(
    rows: &mut Vec<Entry>,
    ps: &PaneSession,
    depth: usize,
    ancestors: Vec<bool>,
    is_last: bool,
    search_text: String,
    parent: Option<usize>,
) {
    rows.push(finalize_entry(Entry {
        kind: EntryType::Agent,
        label: ps.session.title.clone(),
        path: ps.session.directory.clone(),
        changes: None,
        branch: None,
        is_open: false,
        is_running: ps.session.is_running,
        pending: false,
        depth,
        ancestors,
        is_last,
        search_text,
        goto: Some(Goto {
            session: ps.pane.session_name.clone(),
            path: ps.session.directory.clone(),
            window: Some(ps.pane.window_index),
            pane: Some(ps.pane.pane_index),
        }),
        parent,
        connector: String::new(),
        search_text_lower: String::new(),
    }));
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
        pending: false,
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
        push_agent_row(
            rows,
            ps,
            1,
            vec![],
            is_last,
            format!("{} {}", entry.name, ps.session.title),
            Some(dir_idx),
        );
    }
    for w in &entry.worktrees {
        let is_last = child == total_children - 1;
        child += 1;
        let wt_idx = rows.len();
        let wt_name = wt_label(&w.info);
        rows.push(finalize_entry(Entry {
            kind: EntryType::Worktree,
            label: wt_name.clone(),
            path: w.info.path.clone(),
            changes: if w.diff.has_none() {
                None
            } else {
                Some(w.diff.clone())
            },
            branch: w.branch.clone(),
            is_open: w.is_open,
            is_running: false,
            pending: false,
            depth: 1,
            ancestors: vec![],
            is_last,
            search_text: format!(
                "{} {} {}",
                entry.name,
                wt_name,
                w.branch.clone().unwrap_or_default()
            ),
            goto: Some(Goto {
                session: wt_name.clone(),
                path: w.info.path.clone(),
                window: None,
                pane: None,
            }),
            parent: Some(dir_idx),
            connector: String::new(),
            search_text_lower: String::new(),
        }));
        let s_total = w.sessions.len();
        for (si, ps) in w.sessions.iter().enumerate() {
            push_agent_row(
                rows,
                ps,
                2,
                vec![is_last_dir],
                si == s_total - 1,
                format!("{} {} {}", entry.name, wt_name, ps.session.title),
                Some(wt_idx),
            );
        }
    }
}

// A worktree nests under its parent only when their open-state matches;
// otherwise it becomes a root in the matching section: open worktree of a
// closed dir → open `* ⑂` root, quiet worktree of an open dir → closed `⑂` root.
struct WtRoot {
    from_dir: String,
    label: String,
    wt: WtEntry,
}

fn split_roots(dir_entries: &mut [DirEntry]) -> (Vec<WtRoot>, Vec<WtRoot>) {
    let mut open_roots = Vec::new();
    let mut quiet_roots = Vec::new();
    for de in dir_entries.iter_mut() {
        let (stay, go): (Vec<WtEntry>, Vec<WtEntry>) = std::mem::take(&mut de.worktrees)
            .into_iter()
            .partition(|w| w.is_open == de.is_open);
        de.worktrees = stay;
        for w in go {
            let root = WtRoot {
                from_dir: de.name.clone(),
                label: wt_label(&w.info),
                wt: w,
            };
            if de.is_open {
                quiet_roots.push(root);
            } else {
                open_roots.push(root);
            }
        }
    }
    (open_roots, quiet_roots)
}

fn push_wt_root(root: &WtRoot, is_last_root: bool, rows: &mut Vec<Entry>) {
    let wt_idx = rows.len();
    rows.push(finalize_entry(Entry {
        kind: EntryType::Worktree,
        label: root.label.clone(),
        path: root.wt.info.path.clone(),
        changes: if root.wt.diff.has_none() {
            None
        } else {
            Some(root.wt.diff.clone())
        },
        branch: root.wt.branch.clone(),
        is_open: root.wt.is_open,
        is_running: false,
        pending: false,
        depth: 0,
        ancestors: vec![],
        is_last: is_last_root,
        search_text: format!(
            "{} {} {}",
            root.from_dir,
            root.label,
            root.wt.branch.clone().unwrap_or_default()
        ),
        goto: Some(Goto {
            session: root.label.clone(),
            path: root.wt.info.path.clone(),
            window: None,
            pane: None,
        }),
        parent: None,
        connector: String::new(),
        search_text_lower: String::new(),
    }));
    let s_total = root.wt.sessions.len();
    for (si, ps) in root.wt.sessions.iter().enumerate() {
        push_agent_row(
            rows,
            ps,
            1,
            vec![],
            si == s_total - 1,
            format!("{} {} {}", root.from_dir, root.label, ps.session.title),
            Some(wt_idx),
        );
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
    worktrees: Vec<WtEntry>,
    sessions: Vec<PaneSession>,
}

struct WtEntry {
    info: WorktreeInfo,
    diff: Changes,
    branch: Option<String>,
    is_open: bool,
    sessions: Vec<PaneSession>,
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

fn keep_mask(dirs: &[DirInfo], git_data: &[DirGit]) -> Vec<bool> {
    let linked: HashSet<PathBuf> = git_data
        .iter()
        .flat_map(|g| g.worktrees.iter().map(|w| w.path.clone()))
        .collect();
    dirs.iter().map(|d| !linked.contains(&d.path)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirinfo(path: &str) -> DirInfo {
        DirInfo {
            name: PathBuf::from(path).file_name().unwrap().to_string_lossy().into_owned(),
            path: PathBuf::from(path),
        }
    }

    fn dirgit(worktrees: &[&str]) -> DirGit {
        DirGit {
            worktrees: worktrees
                .iter()
                .map(|w| WorktreeInfo {
                    path: PathBuf::from(w),
                    is_main: false,
                })
                .collect(),
            worktree_diffs: HashMap::new(),
            worktree_branches: vec![None; worktrees.len()],
            main_diff: Changes::default(),
        }
    }

    #[test]
    fn linked_worktree_dirs_are_dropped_from_top_level() {
        let dirs = vec![dirinfo("/p/ramo"), dirinfo("/p/ramo--wt-x")];
        let git_data = vec![dirgit(&["/p/ramo--wt-x"]), dirgit(&[])];
        assert_eq!(keep_mask(&dirs, &git_data), vec![true, false]);
    }

    #[test]
    fn unrelated_dirs_are_kept() {
        let dirs = vec![dirinfo("/p/a"), dirinfo("/p/b")];
        let git_data = vec![dirgit(&[]), dirgit(&[])];
        assert_eq!(keep_mask(&dirs, &git_data), vec![true, true]);
    }

    fn direntry(name: &str, open: bool, wts: Vec<(&str, bool)>) -> DirEntry {
        DirEntry {
            name: name.into(),
            path: PathBuf::from(format!("/p/{name}")),
            is_open: open,
            changes: Changes::default(),
            branch: None,
            worktrees: wts
                .into_iter()
                .map(|(p, o)| WtEntry {
                    info: WorktreeInfo {
                        path: PathBuf::from(p),
                        is_main: false,
                    },
                    diff: Changes::default(),
                    branch: None,
                    is_open: o,
                    sessions: vec![],
                })
                .collect(),
            sessions: vec![],
        }
    }

    #[test]
    fn mismatched_worktrees_hoist_to_their_section() {
        let mut entries = vec![
            direntry("proj", false, vec![("/p/proj--a", true), ("/p/proj--b", false)]),
            direntry("open", true, vec![("/p/open--c", true), ("/p/open--d", false)]),
        ];
        let (open_roots, quiet_roots) = split_roots(&mut entries);
        assert_eq!(open_roots.len(), 1);
        assert_eq!(open_roots[0].label, "proj--a");
        assert_eq!(open_roots[0].from_dir, "proj");
        assert_eq!(quiet_roots.len(), 1);
        assert_eq!(quiet_roots[0].label, "open--d");
        assert_eq!(quiet_roots[0].from_dir, "open");
        // Matched children stay nested.
        assert_eq!(entries[0].worktrees.len(), 1);
        assert_eq!(wt_label(&entries[0].worktrees[0].info), "proj--b");
        assert_eq!(entries[1].worktrees.len(), 1);
        assert_eq!(wt_label(&entries[1].worktrees[0].info), "open--c");
    }

    #[test]
    fn worktrees_sort_alphabetically() {
        let git = dirgit(&["/p/proj--z", "/p/proj--a", "/p/proj--m"]);
        let e = build_dir_entry(&dirinfo("/p/proj"), &git, None, &[], &[], &[]);
        let labels: Vec<_> = e.worktrees.iter().map(|w| wt_label(&w.info)).collect();
        assert_eq!(labels, vec!["proj--a", "proj--m", "proj--z"]);
    }

    #[test]
    fn killed_session_flips_child_quiet_but_listed() {
        use crate::model::TmuxSession;
        let wt = "/p/proj--a";
        let git = dirgit(&[wt]);
        let dir = dirinfo("/p/proj");
        let live = vec![TmuxSession {
            name: "proj--a".into(),
            path: PathBuf::from(wt),
        }];
        let open = build_dir_entry(&dir, &git, None, &live, &[], &[]);
        assert!(open.worktrees[0].is_open);
        let dead = build_dir_entry(&dir, &git, None, &[], &[], &[]);
        assert!(!dead.worktrees[0].is_open);
        assert_eq!(dead.worktrees.len(), 1);
    }
}

fn finalize_entry(mut entry: Entry) -> Entry {
    entry.compute_connector();
    entry.search_text_lower = entry.search_text.to_lowercase();
    entry
}
