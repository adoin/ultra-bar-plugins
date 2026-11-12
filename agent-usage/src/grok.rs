//! Read only Grok Bot's own persisted weekly-usage snapshot, never its auth/secrets stores.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::path::PathBuf;
fn root() -> PathBuf {
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default())
        .join("Grok Bot/sand-client-persistence")
}
pub fn installed() -> bool {
    root().is_dir()
}
fn decode(name: &str) -> Option<String> {
    let mut bytes = vec![];
    let mut bits = 0u32;
    let mut count = 0;
    for b in name.bytes() {
        let value = match b.to_ascii_uppercase() {
            b'A'..=b'Z' => (b.to_ascii_uppercase() - b'A') as u32,
            b'2'..=b'7' => (b - b'2' + 26) as u32,
            _ => return None,
        };
        bits = (bits << 5) | value;
        count += 5;
        if count >= 8 {
            count -= 8;
            bytes.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    String::from_utf8(bytes).ok()
}
pub fn read() -> Result<super::Quota> {
    let mut entries = vec![];
    for entry in std::fs::read_dir(root())?.filter_map(Result::ok).take(4096) {
        let path = entry.path();
        if path.extension().is_none_or(|s| s != "blob") {
            continue;
        }
        let Some(label) = path.file_stem().and_then(|s| s.to_str()).and_then(decode) else {
            continue;
        };
        if !label.ends_with(".weekly-usage.cache") {
            continue;
        }
        if entry.metadata()?.len() > 16384 {
            continue;
        }
        let value: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        if value["schemaVersion"] != 2 {
            continue;
        }
        entries.push(value);
    }
    ensure!(entries.len() == 1, "Grok Bot 没有唯一可用的额度快照");
    parse(&entries[0])
}
fn parse(value: &Value) -> Result<super::Quota> {
    let reading = &value["value"]["reading"];
    let usage = &reading["usage"];
    let used = usage["percentUsed"]
        .as_f64()
        .context("Grok Bot 未提供订阅余量")?;
    ensure!(used.is_finite() && used >= 0., "Grok Bot 额度数据无效");
    ensure!(
        usage["hasNonZeroIncludedLimit"].as_bool() == Some(true),
        "Grok Bot 没有可用订阅额度"
    );
    let at = reading["readAtMs"].as_u64().context("缺少快照时间")? / 1000;
    ensure!(
        super::cache::now()
            .checked_sub(at)
            .is_some_and(|age| age < 86400),
        "Grok Bot 额度快照已过期，请在该程序中刷新"
    );
    let remaining = (100. - used).clamp(0., 100.);
    ensure!(
        usage["nextResetMs"]
            .as_u64()
            .is_none_or(|reset| reset / 1000 > super::cache::now()),
        "Grok Bot 快照已跨重置周期，请在该程序中刷新"
    );
    let plan = usage["unifiedPlanName"]
        .as_str()
        .or_else(|| usage["grokPlanLabel"].as_str())
        .unwrap_or("Grok Bot");
    Ok(super::Quota {
        remaining: Some(remaining),
        fetched: Some(at),
        windows: vec![
            format!("{plan} · 剩余 {remaining:.1}%"),
            "Grok Bot 本地额度快照".into(),
        ],
        ..Default::default()
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grok_snapshot_is_remaining_percent_not_api_credit_balance() {
        let value = serde_json::json!({"value":{"reading":{"readAtMs":super::super::cache::now()*1000,"usage":{"percentUsed":6.7,"hasNonZeroIncludedLimit":true,"unifiedPlanName":"Premium"}}}});
        assert!((parse(&value).unwrap().remaining.unwrap() - 93.3).abs() < 0.001);
        assert!(parse(&serde_json::json!({})).is_err());
    }
}
