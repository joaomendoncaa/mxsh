use crate::builder::TreeBuilder;
use crate::config::Config;
use crate::logs;
use crate::model::{Entry, FeedbackEntry, FeedbackType, Payload};
use crate::report;
use crate::service;
use serde::{Deserialize, Serialize};

pub fn preflight(config: &Config, feedbacks: Vec<FeedbackEntry>) -> Payload {
    Payload {
        entries: Vec::new(),
        config: config.clone(),
        feedbacks,
        entries_found: 0,
    }
}

pub fn kill() {
    if service::is_installed() {
        let _ = service::stop();
    }
    let pid_path = pid_path();
    let sock_path = sock_path();

    if let Ok(pid_str) = std::fs::read_to_string(&pid_path)
        && let Ok(pid) = pid_str.trim().parse::<i32>()
    {
        let _ = Command::new("kill")
            .arg(pid.to_string())
            .stderr(Stdio::null())
            .status();
    }
    let _ = std::fs::remove_file(&sock_path);
    let _ = std::fs::remove_file(&pid_path);
}

pub fn start_daemon(overrides: Vec<(String, Option<String>)>) -> std::io::Result<()> {
    start(overrides)
}

use log::{error, info, warn};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, RwLock, mpsc};
use std::thread::{self};
use std::time::{Duration, Instant};

const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const IDLE_KEEP_WARM_INTERVAL: Duration = Duration::from_secs(30);
const CONFIG_POLL_INTERVAL: Duration = Duration::from_millis(400);
const ACCEPT_POLL: Duration = Duration::from_millis(20);
const ATTACH_POLL: Duration = Duration::from_millis(250);
const SPAWN_POLL_ATTEMPTS: usize = 120;
const TAIL_HISTORY: usize = 30;
const CACHE_SAVE_DEBOUNCE: Duration = Duration::from_secs(10);

#[derive(Serialize, Deserialize, Default)]
struct PersistedCache {
    git: crate::git::DiskCache,
}

fn cache_path() -> PathBuf {
    logs::state_dir().join("cache.json")
}

fn load_persisted_cache() -> Option<PersistedCache> {
    let bytes = std::fs::read(cache_path()).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn save_persisted_cache(builder: &TreeBuilder) {
    let cache = PersistedCache {
        git: builder.to_disk_cache(),
    };
    let path = cache_path();
    let Some(dir) = path.parent() else {
        return;
    };
    let _ = std::fs::create_dir_all(dir);
    let tmp = path.with_extension("json.tmp");
    if let Ok(bytes) = serde_json::to_vec(&cache) {
        let _ = std::fs::write(&tmp, bytes);
        let _ = std::fs::rename(&tmp, path);
    }
}

pub fn print_daemon_info() {
    let pid_path = self::pid_path();
    let sock_path = self::sock_path();
    let running = std::fs::read_to_string(&pid_path)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .map(|pid| std::path::Path::new(&format!("/proc/{pid}")).exists())
        .unwrap_or(false);
    let pid = std::fs::read_to_string(&pid_path)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok());
    print!("ramo daemon");
    if let Some(pid) = pid {
        print!(" pid={pid}");
    }
    println!(" running={running}");
    println!("  sock  {}", sock_path.display());
    println!("  pid   {}", pid_path.display());
    println!("  cache {}", cache_path().display());
    if service::is_installed() {
        println!("  sysd  {}", service::unit_path().display());
    }
}

pub fn sock_path() -> PathBuf {
    logs::state_dir().join("daemon.sock")
}

pub fn pid_path() -> PathBuf {
    logs::state_dir().join("daemon.pid")
}

pub fn is_daemon_running() -> bool {
    if let Ok(pid_str) = std::fs::read_to_string(pid_path())
        && let Ok(pid) = pid_str.trim().parse::<i32>()
    {
        std::path::Path::new(&format!("/proc/{}", pid)).exists()
    } else {
        false
    }
}

fn show_last_n_lines(path: &Path, n: usize) {
    if let Ok(content) = std::fs::read_to_string(path) {
        let lines: Vec<&str> = content.lines().collect();
        let start = lines.len().saturating_sub(n);
        for line in &lines[start..] {
            println!("{line}");
        }
    }
}

pub fn attach() -> ! {
    let pid = std::fs::read_to_string(pid_path())
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .map(|p| p.to_string())
        .unwrap_or_else(|| "?".to_string());

    let log_path = logs::state_dir().join("daemon.log");
    eprintln!("daemon already running, attaching to {pid}");
    eprintln!("{}~HEAD-{TAIL_HISTORY}", log_path.display());

    show_last_n_lines(&log_path, TAIL_HISTORY);

    let mut file = loop {
        if let Ok(f) = std::fs::OpenOptions::new().read(true).open(&log_path) {
            break f;
        }
        thread::sleep(ATTACH_POLL);
    };

    let _ = file.seek(SeekFrom::End(0));

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let mut buf = [0u8; 4096];

    loop {
        match file.read(&mut buf) {
            Ok(0) => {
                if !is_daemon_running() {
                    eprintln!("daemon exited");
                    std::process::exit(0);
                }
                thread::sleep(ATTACH_POLL);
            }
            Ok(n) => {
                handle.write_all(&buf[..n]).ok();
                handle.flush().ok();
            }
            Err(_) => break,
        }
    }

    std::process::exit(0);
}

type ConfigLock = Arc<RwLock<Config>>;
type FeedbackLock = Arc<RwLock<Vec<FeedbackEntry>>>;
type ClientList = Arc<Mutex<Vec<UnixStream>>>;
type PayloadBytes = Arc<RwLock<Vec<u8>>>;

fn serialize_payload(
    entries: Vec<Entry>,
    config: &Config,
    feedbacks: &[FeedbackEntry],
    entries_found: usize,
) -> Vec<u8> {
    serde_json::to_vec(&Payload {
        entries,
        config: config.clone(),
        feedbacks: feedbacks.to_vec(),
        entries_found,
    })
    .unwrap_or_default()
}

fn build_payload(
    config_lock: &ConfigLock,
    feedback_lock: &FeedbackLock,
    builder: &TreeBuilder,
) -> (Vec<u8>, usize) {
    let config = config_lock.read().unwrap();
    let feedbacks = feedback_lock.read().unwrap();
    let entries = builder.build(&config);
    let entry_count = entries.len();
    let bytes = serialize_payload(entries, &config, &feedbacks, entry_count);
    (bytes, entry_count)
}

fn broadcast(data: &PayloadBytes, clients: &ClientList) -> bool {
    let bytes = data.read().unwrap().clone();
    let mut list = clients.lock().unwrap();
    let mut sent = false;
    list.retain_mut(|c| {
        if write_frame(c, &bytes).is_ok() {
            sent = true;
            true
        } else {
            false
        }
    });
    sent
}

fn prune_dead(clients: &ClientList) -> bool {
    let mut list = clients.lock().unwrap();
    list.retain_mut(|c| !is_dead(c));
    !list.is_empty()
}

fn is_dead(stream: &mut UnixStream) -> bool {
    let _ = stream.set_nonblocking(true);
    let mut b = [0u8; 1];
    let dead = match stream.read(&mut b) {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => false,
        Err(_) => true,
    };
    let _ = stream.set_nonblocking(false);
    dead
}

pub fn start(overrides: Vec<(String, Option<String>)>) -> std::io::Result<()> {
    let under_systemd = service::under_systemd();
    if is_daemon_running() {
        if under_systemd {
            return Ok(());
        }
        attach();
    }

    logs::init("daemon").ok();
    let dir = logs::state_dir();
    std::fs::create_dir_all(&dir).ok();

    let pid_path = pid_path();
    std::fs::write(&pid_path, std::process::id().to_string()).ok();
    if under_systemd {
        info!("running under systemd, idle timeout disabled");
    }

    let sock_path = sock_path();
    let _ = std::fs::remove_file(&sock_path);

    let (config, feedbacks) = Config::load(&overrides);
    let config_file = Config::config_path();
    let config_lock = Arc::new(RwLock::new(config));
    let feedback_lock = Arc::new(RwLock::new(feedbacks));
    let reports = crate::report::ReportMap::default();
    spawn_listener(reports.clone());
    let builder = Arc::new(TreeBuilder::new(reports));
    let clients: ClientList = Arc::new(Mutex::new(Vec::new()));

    info!(
        "daemon starting (timeout={}s)",
        config_lock.read().unwrap().daemon_timeout
    );

    // Start with empty entries — empty is preferable to stale. The first
    // client sees the loader until the first fresh build completes, then
    // the refresh thread broadcasts. Git caches are warmed from disk so
    // that first build is fast (entries themselves are never persisted).
    let initial_entries: Vec<Entry> = Vec::new();
    let initial_count = 0usize;
    if let Some(cache) = load_persisted_cache() {
        builder.load_disk_cache(&cache.git);
        info!(
            "disk cache loaded ({} diffs, {} branches, {} worktrees)",
            cache.git.diffs.len(),
            cache.git.branches.len(),
            cache.git.worktrees.len(),
        );
    } else {
        info!("no disk cache, cold start");
    }
    let data: PayloadBytes = {
        let (config, feedbacks) = (
            config_lock.read().unwrap().clone(),
            feedback_lock.read().unwrap().clone(),
        );
        Arc::new(RwLock::new(serialize_payload(
            initial_entries,
            &config,
            &feedbacks,
            initial_count,
        )))
    };

    let listener = UnixListener::bind(&sock_path)?;
    listener.set_nonblocking(true)?;

    // Wakes the refresh thread as soon as a first client connects, so it
    // rebuilds immediately instead of waiting for the next interval.
    let wake: Arc<(Mutex<bool>, Condvar)> = Arc::new((Mutex::new(false), Condvar::new()));

    if let Some(ref path) = config_file {
        let lock = config_lock.clone();
        let data = data.clone();
        let builder = builder.clone();
        let ov = overrides.clone();
        let path = path.clone();
        let fb_lock = feedback_lock.clone();
        let clients = clients.clone();
        let wake = wake.clone();
        thread::spawn(move || {
            let mut last_mtime = std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok());
            loop {
                thread::sleep(CONFIG_POLL_INTERVAL);
                let current_mtime = std::fs::metadata(&path)
                    .ok()
                    .and_then(|m| m.modified().ok());
                if current_mtime != last_mtime {
                    last_mtime = current_mtime;
                    match std::fs::read_to_string(&path) {
                        Ok(content) => {
                            let (mut c, mut fb) = Config::parse_content(&path, &content);
                            fb.extend(c.apply_overrides(&ov));
                            info!(
                                "config reloaded\n{}",
                                serde_json::to_string_pretty(&c).unwrap_or_default()
                            );
                            if let Ok(mut current) = lock.write() {
                                *current = c;
                            }
                            if let Ok(mut f) = fb_lock.write() {
                                *f = fb;
                            }
                            let (bytes, _count) = build_payload(&lock, &fb_lock, &builder);
                            if let Ok(mut d) = data.write() {
                                *d = bytes;
                            }
                            broadcast(&data, &clients);
                            wake.1.notify_all();
                        }
                        Err(e) => {
                            warn!("failed to read config: {e}");
                            if let Ok(mut f) = fb_lock.write() {
                                f.push(FeedbackEntry {
                                    level: FeedbackType::Error,
                                    message: format!(
                                        "cannot read config file '{}': {e}",
                                        path.display()
                                    ),
                                });
                            }
                        }
                    }
                }
            }
        });
    }

    {
        let data = data.clone();
        let config_lock = config_lock.clone();
        let feedback_lock = feedback_lock.clone();
        let builder = builder.clone();
        let clients = clients.clone();
        let wake = wake.clone();
        thread::spawn(move || {
            let mut last_cache_save = Instant::now() - CACHE_SAVE_DEBOUNCE;
            loop {
                let has_clients = prune_dead(&clients);
                let interval = if has_clients {
                    REFRESH_INTERVAL
                } else {
                    IDLE_KEEP_WARM_INTERVAL
                };
                {
                    let (lock, cvar) = &*wake;
                    let pending = lock.lock().unwrap();
                    let (mut guard, _) = cvar.wait_timeout(pending, interval).unwrap();
                    *guard = false;
                }
                let start = Instant::now();
                let (bytes, _count) = build_payload(&config_lock, &feedback_lock, &builder);
                let changed = {
                    let mut d = data.write().unwrap();
                    if *d == bytes {
                        false
                    } else {
                        *d = bytes;
                        true
                    }
                };
                if changed {
                    broadcast(&data, &clients);
                    if last_cache_save.elapsed() > CACHE_SAVE_DEBOUNCE {
                        save_persisted_cache(&builder);
                        last_cache_save = Instant::now();
                    }
                }
                info!(
                    "refresh in {}ms (changed={changed})",
                    start.elapsed().as_millis()
                );
            }
        });
    }

    let mut last_connection = std::time::Instant::now();

    {
        let log_path = logs::state_dir().join("daemon.log");
        eprintln!(
            "daemon ready (pid {}), logging to {}",
            std::process::id(),
            log_path.display()
        );
        show_last_n_lines(&log_path, TAIL_HISTORY);

        thread::spawn(move || {
            let mut file = loop {
                if let Ok(f) = std::fs::OpenOptions::new().read(true).open(&log_path) {
                    break f;
                }
                thread::sleep(ACCEPT_POLL);
            };
            let _ = file.seek(SeekFrom::End(0));
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            let mut buf = [0u8; 4096];
            loop {
                match file.read(&mut buf) {
                    Ok(0) => thread::sleep(ATTACH_POLL),
                    Ok(n) => {
                        handle.write_all(&buf[..n]).ok();
                        handle.flush().ok();
                    }
                    Err(_) => break,
                }
            }
        });
    }

    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                last_connection = std::time::Instant::now();
                info!("serving client");
                if let Ok(bytes) = data.read().map(|d| d.clone())
                    && write_frame(&mut stream, &bytes).is_ok()
                {
                    let mut list = clients.lock().unwrap();
                    let first = list.is_empty();
                    list.push(stream);
                    if first {
                        let (lock, cvar) = &*wake;
                        let mut pending = lock.lock().unwrap();
                        *pending = true;
                        cvar.notify_all();
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                let idle_timeout = config_lock
                    .read()
                    .map(|c| c.daemon_timeout_duration())
                    .unwrap_or_else(|_| Duration::from_secs(600));
                if !under_systemd
                    && last_connection.elapsed() > idle_timeout
                    && clients.lock().unwrap().is_empty()
                {
                    info!("idle timeout reached, shutting down daemon");
                    save_persisted_cache(&builder);
                    break;
                }
                thread::sleep(ACCEPT_POLL);
            }
            Err(e) => {
                error!("accept error: {}", e);
                let _ = std::fs::remove_file(&sock_path);
                let _ = std::fs::remove_file(&pid_path);
                return Err(e);
            }
        }
    }

    let _ = std::fs::remove_file(&sock_path);
    let _ = std::fs::remove_file(&pid_path);
    Ok(())
}

fn write_frame(stream: &mut UnixStream, bytes: &[u8]) -> std::io::Result<()> {
    let len: u32 = bytes
        .len()
        .try_into()
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "payload too large"))?;
    stream.write_all(&len.to_le_bytes())?;
    stream.write_all(bytes)?;
    stream.flush()?;
    Ok(())
}

pub fn read_frame(stream: &mut UnixStream) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    Ok(buf)
}

pub fn fetch_once() -> Option<Vec<u8>> {
    let mut stream = UnixStream::connect(sock_path()).ok()?;
    read_frame(&mut stream).ok()
}

pub fn listen(tx: mpsc::Sender<Option<Payload>>) {
    thread::spawn(move || {
        let mut backoff = Duration::from_millis(100);
        loop {
            match UnixStream::connect(sock_path()) {
                Ok(mut stream) => {
                    backoff = Duration::from_millis(100);
                    while let Ok(bytes) = read_frame(&mut stream) {
                        if let Ok(payload) = serde_json::from_slice(&bytes)
                            && tx.send(Some(payload)).is_err()
                        {
                            return;
                        }
                    }
                }
                Err(_) => {
                    thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
            }
        }
    });
}

fn report_sock_path() -> PathBuf {
    logs::state_dir().join("report.sock")
}

/// Receive `{pane_id, agent_session_id}` reports. Reporter-agnostic: the
/// daemon owns the socket, whoever can frame JSON gets to report.
fn spawn_listener(reports: report::ReportMap) {
    thread::spawn(move || {
        let sock = report_sock_path();
        let _ = std::fs::remove_file(&sock);
        let listener = match UnixListener::bind(&sock) {
            Ok(l) => l,
            Err(e) => {
                warn!("report listener unavailable ({}): {e}", sock.display());
                return;
            }
        };
        info!("report listener on {}", sock.display());
        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    match read_frame(&mut stream) {
                        Ok(bytes) => insert_report(&reports, &bytes),
                        Err(e) => warn!("bad report frame: {e}"),
                    }
                }
                Err(e) => {
                    warn!("report accept error: {e}");
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
    });
}

fn insert_report(reports: &report::ReportMap, bytes: &[u8]) {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return;
    };
    let (Some(pane), Some(session)) = (
        v.get("pane_id").and_then(|p| p.as_str()),
        v.get("agent_session_id").and_then(|s| s.as_str()),
    ) else {
        return;
    };
    if !pane.is_empty() {
        report::insert(reports, pane, session);
    }
}

pub fn spawn(overrides: &[(String, Option<String>)]) -> Option<()> {
    let exe = std::env::current_exe().ok()?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon").arg("start");
    for (key, val) in overrides {
        match val {
            Some(v) => cmd.arg(format!("--{key}={v}")),
            None => cmd.arg(format!("--{key}")),
        };
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    Some(())
}

pub fn fetch_or_spawn<F>(overrides: &[(String, Option<String>)], mut on_poll: F) -> Option<Vec<u8>>
where
    F: FnMut(usize),
{
    if overrides.is_empty() {
        if let Some(b) = fetch_once() {
            info!("daemon cache hit");
            return Some(b);
        }
    } else {
        kill();
    }

    let (config, _) = Config::load(overrides);
    info!("daemon cold-start (timeout={}s)", config.daemon_timeout);
    spawn(overrides)?;

    let spawn_start = std::time::Instant::now();
    for _ in 0..SPAWN_POLL_ATTEMPTS {
        on_poll(0);
        if let Some(b) = fetch_once() {
            info!("daemon ready after {}ms", spawn_start.elapsed().as_millis());
            return Some(b);
        }
    }

    warn!("daemon never became reachable after spawn");
    None
}

pub fn initial_fetch(overrides: &[(String, Option<String>)]) -> Payload {
    let fetch_start = Instant::now();

    let poll = || thread::sleep(Duration::from_millis(50));

    if let Some(bytes) = fetch_or_spawn(overrides, |_| poll())
        && let Ok(payload) = serde_json::from_slice::<Payload>(&bytes)
    {
        info!("fetch: {}ms", fetch_start.elapsed().as_millis());
        return payload;
    }

    info!("fetch: {}ms", fetch_start.elapsed().as_millis());
    warn!("daemon unavailable, using empty state");
    let (config, feedbacks) = Config::load(overrides);
    Payload {
        entries: Vec::new(),
        config,
        feedbacks,
        entries_found: 0,
    }
}

pub fn print_config() {
    match Config::config_path() {
        Some(path) => {
            println!("config: {}", path.display());
            match std::fs::read_to_string(&path) {
                Ok(content) => print!("{content}"),
                Err(e) => eprintln!("error reading config: {e}"),
            }
        }
        None => eprintln!("ramo: no config file found"),
    }
}

pub fn show_logs() {
    if is_daemon_running() {
        attach();
    } else {
        eprintln!("ramo: daemon is not running");
    }
}
