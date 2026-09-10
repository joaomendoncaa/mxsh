use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// tmux pane id -> (session id, reported at). Fed by the daemon's report
/// listener; read by the tree builder. Reporter-agnostic.
pub type ReportMap = Arc<Mutex<HashMap<String, (String, Instant)>>>;

// A pane id reused by a restarted tmux server must not ghost an old session.
const REPORT_TTL: Duration = Duration::from_secs(6 * 3600);
// Empty verdicts ("no session") expire fast so a dead reporter stops pinning.
const EMPTY_TTL: Duration = Duration::from_secs(30);

fn is_fresh(id: &str, at: &Instant) -> bool {
    at.elapsed() < if id.is_empty() { EMPTY_TTL } else { REPORT_TTL }
}

/// Record that `pane_id` shows `session_id` (empty = no session).
pub fn insert(reports: &ReportMap, pane_id: &str, session_id: &str) {
    if let Ok(mut map) = reports.lock() {
        map.insert(pane_id.to_string(), (session_id.to_string(), Instant::now()));
    }
}

/// Exact session id displayed in `pane_id`, if freshly reported.
/// `Some("")` means the pane is known session-less: show the synthetic row,
/// never a stale title.
pub fn reported_session(reports: &ReportMap, pane_id: &str) -> Option<String> {
    let map = reports.lock().ok()?;
    let (id, at) = map.get(pane_id)?;
    is_fresh(id, at).then(|| id.clone())
}

pub fn prune(reports: &ReportMap, live_panes: &HashSet<String>) {
    if let Ok(mut map) = reports.lock() {
        map.retain(|pane, (id, at)| is_fresh(id, at) && live_panes.contains(pane));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_reports_are_ignored() {
        let reports: ReportMap = ReportMap::default();
        reports.lock().unwrap().insert(
            "%1".into(),
            (
                "ses_old".into(),
                Instant::now() - REPORT_TTL - Duration::from_secs(1),
            ),
        );
        assert_eq!(reported_session(&reports, "%1"), None);
        prune(&reports, &HashSet::from(["%1".to_string()]));
        assert!(reports.lock().unwrap().is_empty());
    }
}
