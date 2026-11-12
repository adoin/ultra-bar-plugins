//! Short-lived quota cache scoped by CLI home and auth-file metadata. Never stores credentials.
use super::Quota;
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
pub const MAX_AGE: u64 = 900;
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
#[derive(Serialize, Deserialize)]
struct Saved {
    scope: String,
    fetched: u64,
    remaining: f64,
    resets: Option<u64>,
    #[serde(default)]
    next_reset: Option<u64>,
    #[serde(default)]
    reset_credits: Option<Vec<ultra_bar_plugin_ui::statistics::ResetCredit>>,
    windows: Vec<String>,
}
fn scope(home: &Path) -> Option<String> {
    let meta = std::fs::metadata(home.join("auth.json")).ok()?;
    let stamp = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(format!("{}|{stamp}|{}", home.display(), meta.len()))
}
fn valid(saved: &Saved, scope: &str, now: u64) -> bool {
    saved.scope == scope
        && now
            .checked_sub(saved.fetched)
            .is_some_and(|age| age <= MAX_AGE)
        && saved.remaining.is_finite()
        && (0.0..=100.).contains(&saved.remaining)
        && saved.reset_credits.as_ref().is_none_or(|credits| {
            credits.len() <= 128
                && credits
                    .iter()
                    .all(|c| c.title.chars().count() <= 128 && c.status.len() <= 32)
        })
        && saved.windows.len() <= 8
        && saved.windows.iter().all(|s| s.chars().count() <= 512)
}
pub fn load(data: &Path, home: &Path) -> Option<Quota> {
    let scope = scope(home)?;
    let file = data.join("codex-quota-cache.json");
    if std::fs::metadata(&file).ok()?.len() > 16384 {
        return None;
    }
    let saved: Saved = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
    if !valid(&saved, &scope, now()) {
        return None;
    }
    Some(Quota {
        remaining: Some(saved.remaining),
        resets: saved.resets,
        next_reset: saved.next_reset,
        reset_credits: saved.reset_credits,
        windows: saved.windows,
        error: "正在刷新额度（显示最近成功结果）".into(),
        fetched: Some(saved.fetched),
    })
}
pub fn save(data: &Path, home: &Path, q: &Quota) {
    let Some(scope) = scope(home) else { return };
    let (Some(remaining), Some(fetched)) = (q.remaining, q.fetched) else {
        return;
    };
    let value = Saved {
        scope,
        fetched,
        remaining,
        resets: q.resets,
        next_reset: q.next_reset,
        reset_credits: q.reset_credits.clone(),
        windows: q.windows.clone(),
    };
    let pending = data.join("codex-quota-cache.json.new");
    if let Ok(bytes) = serde_json::to_vec(&value)
        && std::fs::write(&pending, bytes).is_ok()
    {
        let _ = std::fs::rename(pending, data.join("codex-quota-cache.json"));
    }
}
pub fn failure(previous: &Quota, error: String) -> Quota {
    if previous.remaining.is_some()
        && previous
            .fetched
            .is_some_and(|at| now().checked_sub(at).is_some_and(|age| age <= MAX_AGE))
        && (error.contains("超时") || error.contains("连接关闭"))
    {
        let mut next = previous.clone();
        next.error = format!("{error}\n显示最近成功结果");
        next
    } else {
        Quota {
            error,
            ..Default::default()
        }
    }
}
pub fn retry(failures: u32) -> u64 {
    (15u64 << failures.saturating_sub(1).min(5)).min(300)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_rejects_another_account_expiry_future_and_invalid_quota() {
        let mut saved = Saved {
            scope: "a".into(),
            fetched: 100,
            remaining: 27.,
            resets: Some(2),
            next_reset: None,
            reset_credits: None,
            windows: vec![],
        };
        assert!(valid(&saved, "a", 101));
        assert!(!valid(&saved, "b", 101));
        assert!(!valid(&saved, "a", 1001));
        assert!(!valid(&saved, "a", 99));
        saved.remaining = f64::NAN;
        assert!(!valid(&saved, "a", 101));
    }
    #[test]
    fn transient_failure_keeps_recent_data_but_auth_failure_does_not() {
        let old = Quota {
            remaining: Some(27.),
            fetched: Some(now()),
            ..Default::default()
        };
        assert_eq!(failure(&old, "查询超时".into()).remaining, Some(27.));
        assert_eq!(
            failure(&old, "请求失败：Unauthorized".into()).remaining,
            None
        );
        assert_eq!((retry(1), retry(2), retry(20)), (15, 30, 300));
    }
}
