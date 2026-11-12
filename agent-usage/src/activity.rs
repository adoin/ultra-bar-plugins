//! Activity metadata only: never open session transcripts or the desktop history database.
use notify::{
    Event, EventKind,
    event::{CreateKind, ModifyKind},
};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
pub const TTL: Duration = Duration::from_secs(300);
const HISTORY: &[&str] = &["thread_history_1.sqlite", "thread_history_1.sqlite-wal"];
fn milliseconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn recent(home: &Path) -> Option<Instant> {
    HISTORY
        .iter()
        .filter_map(|name| {
            std::fs::metadata(home.join(name))
                .ok()?
                .modified()
                .ok()?
                .elapsed()
                .ok()
        })
        .filter(|age| *age < TTL)
        .min()
        .and_then(|age| Instant::now().checked_sub(age))
}
#[derive(Clone, Default)]
pub struct Quiet(Arc<AtomicU64>);
pub struct Query(Quiet);
impl Quiet {
    pub fn query(&self) -> Query {
        self.0.store(u64::MAX, Ordering::Release);
        Query(self.clone())
    }
    pub fn suppressed(&self) -> bool {
        milliseconds() < self.0.load(Ordering::Acquire)
    }
}
impl Drop for Query {
    fn drop(&mut self) {
        self.0
            .0
            .store(milliseconds().saturating_add(2000), Ordering::Release);
    }
}
fn history_path(home: &Path, path: &Path) -> bool {
    path.parent() == Some(home)
        && path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| HISTORY.contains(&s))
}
pub fn sources(
    event: &Event,
    home: &Path,
    sessions: &Path,
    claude: &Path,
    quiet: &Quiet,
) -> [bool; 2] {
    let mut result = [false; 2];
    if !matches!(
        event.kind,
        EventKind::Modify(ModifyKind::Data(_))
            | EventKind::Create(CreateKind::File | CreateKind::Any)
    ) {
        return result;
    }
    for path in &event.paths {
        if path.extension().is_some_and(|s| s == "jsonl") {
            if path.starts_with(sessions) {
                result[0] = true;
            } else if path.starts_with(claude) {
                result[1] = true;
            }
        } else if !quiet.suppressed() && history_path(home, path) {
            result[0] = true;
        }
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_history_activity_is_distinct_from_auth_or_plugin_queries() {
        let home = Path::new("D:/Codex");
        let sessions = home.join("sessions");
        let claude = Path::new("D:/Claude/projects");
        let quiet = Quiet::default();
        let mut event = Event::new(EventKind::Modify(ModifyKind::Data(
            notify::event::DataChange::Any,
        )))
        .add_path(home.join("thread_history_1.sqlite-wal"));
        assert_eq!(
            sources(&event, home, &sessions, claude, &quiet),
            [true, false]
        );
        {
            let _query = quiet.query();
            assert_eq!(
                sources(&event, home, &sessions, claude, &quiet),
                [false, false]
            );
        }
        assert_eq!(
            sources(&event, home, &sessions, claude, &quiet),
            [false, false]
        );
        event.paths = vec![home.join("auth.json")];
        assert_eq!(
            sources(&event, home, &sessions, claude, &Quiet::default()),
            [false, false]
        );
        event.paths = vec![sessions.join("2026/11/10/ongoing.jsonl")];
        assert_eq!(
            sources(&event, home, &sessions, claude, &quiet),
            [true, false]
        );
        event.kind = EventKind::Remove(notify::event::RemoveKind::File);
        assert_eq!(
            sources(&event, home, &sessions, claude, &quiet),
            [false, false]
        );
    }
}
