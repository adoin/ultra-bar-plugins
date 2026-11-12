//! Counts acknowledged messages only. Bodies and pending sends are not deserialized.
use anyhow::{Result, ensure};
use chrono::{DateTime, Datelike, Local};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    io::Read,
    path::Path,
};
use ultra_bar_plugin_ui::statistics::{ConversationDay, Conversations};
#[derive(Deserialize)]
struct Snapshot {
    #[serde(rename = "schemaVersion")]
    schema: u32,
    value: Replica,
}
#[derive(Deserialize)]
struct Replica {
    entries: Vec<Entry>,
    #[serde(default, rename = "precedingTimestampMs")]
    preceding: Option<i64>,
}
#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    id: String,
    kind: String,
    #[serde(default)]
    role: String,
    #[serde(default, rename = "timestampMs")]
    timestamp: Option<i64>,
    #[serde(default)]
    seq: i64,
}
pub fn session(path: &Path) -> Option<String> {
    if path.extension()? != "blob" {
        return None;
    }
    let label = super::grok::decode(path.file_stem()?.to_str()?)?;
    let (_, id) = label.rsplit_once(".replicas.")?;
    (id.len() == 36 && id.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-'))
        .then(|| id.to_owned())
}
fn collect(
    snapshot: Snapshot,
    session: &str,
    today: i32,
    seen: &mut HashSet<String>,
) -> Result<Conversations> {
    ensure!(snapshot.schema == 1, "未知 Grok 对话缓存版本");
    ensure!(snapshot.value.entries.len() <= 50000, "Grok 对话缓存过大");
    let partial = snapshot.value.preceding.is_some()
        || snapshot
            .value
            .entries
            .iter()
            .map(|e| e.seq)
            .min()
            .is_some_and(|s| s > 1);
    let mut messages = HashMap::<String, Entry>::new();
    for entry in snapshot.value.entries {
        if entry.kind != "message"
            || entry.id.is_empty()
            || entry.id.len() > 512
            || !matches!(entry.role.as_str(), "user" | "assistant")
        {
            continue;
        }
        if messages
            .get(&entry.id)
            .is_none_or(|old| entry.seq >= old.seq)
        {
            messages.insert(entry.id.clone(), entry);
        }
    }
    ensure!(!messages.is_empty(), "缓存没有已确认的对话消息");
    let mut days = BTreeMap::<i32, ConversationDay>::new();
    for entry in messages.into_values() {
        let Some(at) = entry.timestamp.and_then(DateTime::from_timestamp_millis) else {
            continue;
        };
        let date = at.with_timezone(&Local).date_naive();
        let day = date.num_days_from_ce();
        if day > today || day <= today - 30 {
            continue;
        }
        if !seen.insert(entry.id) {
            continue;
        }
        let row = days.entry(day).or_insert_with(|| ConversationDay {
            day,
            date: date.to_string(),
            session: session.into(),
            ..Default::default()
        });
        if entry.role == "user" {
            row.sent += 1;
        } else {
            row.received += 1;
        }
    }
    Ok(Conversations {
        rows: days.into_values().collect(),
        partial,
    })
}
pub fn read() -> Result<Option<Conversations>> {
    let mut result = Conversations::default();
    let mut loaded = 0;
    let mut total = 0;
    let mut seen = HashSet::new();
    let today = Local::now().date_naive().num_days_from_ce();
    let mut files: Vec<_> = std::fs::read_dir(super::grok::snapshot_dir())?
        .take(4096)
        .collect::<std::io::Result<_>>()?;
    files.sort_by_key(|item| item.file_name());
    for item in files {
        let path = item.path();
        let Some(id) = session(&path) else {
            continue;
        };
        let length = item.metadata()?.len();
        total += length;
        if length > 8 * 1024 * 1024 || total > 32 * 1024 * 1024 {
            result.partial = true;
            continue;
        }
        let parsed = (|| -> Result<Conversations> {
            let snapshot = serde_json::from_reader(File::open(path)?.take(8 * 1024 * 1024 + 1))?;
            collect(snapshot, &id, today, &mut seen)
        })();
        match parsed {
            Ok(data) => {
                loaded += 1;
                result.partial |= data.partial;
                result.rows.extend(data.rows);
            }
            Err(_) => {
                result.partial = true;
            }
        }
    }
    if loaded == 0 {
        return Ok(None);
    }
    ensure!(result.rows.len() <= 32768, "Grok 对话统计过大");
    Ok(Some(result))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_acknowledged_messages_once_without_pending_sends_or_message_bodies() {
        let now = Local::now();
        let at = now.timestamp_millis();
        let today = now.date_naive().num_days_from_ce();
        let value = serde_json::json!({"schemaVersion":1,"value":{"precedingTimestampMs":at-1,"entries":[
            {"kind":"send-message","id":"pending","timestampMs":at,"message":{"content":"private"}},
            {"kind":"message","id":"user","role":"user","timestampMs":at,"seq":3,"content":"private"},
            {"kind":"message","id":"reply","role":"assistant","timestampMs":at,"seq":4,"isStreaming":true},
            {"kind":"message","id":"reply","role":"assistant","timestampMs":at,"seq":5},
            {"kind":"event","id":"event","timestampMs":at},
            {"kind":"message","id":"old","role":"user","timestampMs":at-40*86400000_i64}
        ]}});
        let mut seen = HashSet::new();
        let snapshot = serde_json::from_value(value.clone()).unwrap();
        let result = collect(snapshot, "session", today, &mut seen).unwrap();
        assert!(result.partial);
        assert_eq!(result.rows.len(), 1);
        assert_eq!((result.rows[0].sent, result.rows[0].received), (1, 1));
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(!serialized.contains("private") && !serialized.contains("content"));
        let mirror = collect(
            serde_json::from_value(value).unwrap(),
            "mirror",
            today,
            &mut seen,
        )
        .unwrap();
        assert!(mirror.rows.is_empty(), "跨副本镜像消息重复计数");
    }
    #[test]
    fn pending_commands_without_confirmed_messages_are_not_a_zero_usage_source() {
        let snapshot = serde_json::from_value(serde_json::json!({"schemaVersion":1,"value":{"entries":[{"kind":"send-message","id":"pending"}]}})).unwrap();
        assert!(
            collect(
                snapshot,
                "session",
                Local::now().date_naive().num_days_from_ce(),
                &mut HashSet::new()
            )
            .is_err()
        );
    }
}
