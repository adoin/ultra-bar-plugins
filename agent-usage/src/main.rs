#![cfg_attr(not(test), windows_subsystem = "windows")]
mod claude;
mod codex;
use anyhow::{Result, ensure};
use notify::{RecursiveMode, Watcher};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use ultra_bar_plugin_ui::{Command, HostEvent, Page, PluginEvent, Row, send};
#[derive(Clone, Debug, Default)]
struct Quota {
    remaining: Option<f64>,
    resets: Option<u64>,
    windows: Vec<String>,
    error: String,
}
fn foreground_provider(executable: &str, title: &str) -> Option<usize> {
    let name = std::path::Path::new(executable)
        .file_name()?
        .to_str()?
        .to_ascii_lowercase();
    match name.as_str() {
        "codex.exe" => Some(0),
        "claude.exe" => Some(1),
        "windowsterminal.exe"
        | "openconsole.exe"
        | "conhost.exe"
        | "cmd.exe"
        | "pwsh.exe"
        | "powershell.exe" => {
            let title = title.to_ascii_lowercase();
            if title.contains("codex") {
                Some(0)
            } else if title.contains("claude") {
                Some(1)
            } else {
                None
            }
        }
        _ => None,
    }
}
fn window(used: &Value, label: &str, reset: &Value) -> Option<(f64, String)> {
    let used = used.as_f64()?;
    if !used.is_finite() || !(0.0..=100.0).contains(&used) {
        return None;
    }
    let remaining = 100.0 - used;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let suffix = reset
        .as_u64()
        .map(|t| {
            let mins = t.saturating_sub(now) / 60;
            format!(" · {}h {}m 后重置", mins / 60, mins % 60)
        })
        .unwrap_or_default();
    Some((remaining, format!("{label} 剩余 {remaining:.0}%{suffix}")))
}
fn quota(value: &Value, is_codex: bool) -> Quota {
    let mut windows = Vec::new();
    if is_codex {
        let limits = value
            .get("rateLimitsByLimitId")
            .and_then(|v| v.get("codex"))
            .unwrap_or(&value["rateLimits"]);
        for key in ["primary", "secondary"] {
            let w = &limits[key];
            let mins = w["windowDurationMins"].as_u64().unwrap_or(0);
            let label = match mins {
                300 => "5 小时".into(),
                10080 => "每周".into(),
                0 => "额度".into(),
                _ => format!("{mins} 分钟"),
            };
            if let Some(v) = window(&w["usedPercent"], &label, &w["resetsAt"]) {
                windows.push(v);
            }
        }
    } else {
        for (key, label) in [
            ("five_hour", "5 小时"),
            ("seven_day", "每周"),
            ("seven_day_sonnet", "Sonnet 每周"),
            ("seven_day_opus", "Opus 每周"),
        ] {
            let w = &value[key];
            let used = w.get("used_percentage").unwrap_or(&w["utilization"]);
            if let Some(v) = window(used, label, &w["resets_at"]) {
                windows.push(v);
            }
        }
    }
    let resets = is_codex
        .then(|| value["rateLimitResetCredits"]["availableCount"].as_u64())
        .flatten();
    let remaining = windows.iter().map(|w| w.0).reduce(f64::min);
    let mut windows: Vec<String> = windows.into_iter().map(|w| w.1).collect();
    if let Some(count) = resets {
        windows.push(format!("手动重置剩余 {count} 次"));
    }
    Quota {
        remaining,
        resets,
        windows,
        error: String::new(),
    }
}
fn page(name: &str, q: &Quota) -> Page {
    let mut text = q
        .remaining
        .map(|r| format!("{name} {r:.0}%"))
        .unwrap_or_else(|| format!("{name} —"));
    if let Some(count) = q.resets {
        text.push_str(&format!(" ↺{count}"));
    }
    let detail = if !q.error.is_empty() {
        q.error.clone()
    } else if q.windows.is_empty() {
        "当前账户未提供订阅额度".into()
    } else {
        q.windows.join("\n")
    };
    Page {
        id: name.into(),
        text,
        tooltip: format!("{name}\n{detail}"),
        progress: q.remaining.map(|v| v / 100.0),
    }
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "--claude-statusline") {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::io::stdin().take(1024 * 1024).read_to_end(&mut bytes)?;
        let value: Value = serde_json::from_slice(&bytes)?;
        let dir = args.get(2).map(PathBuf::from).unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default())
                .join("UltraBar/plugins/com.emssion.agent-usage")
        });
        ensure!(dir.is_absolute(), "Invalid data directory");
        std::fs::create_dir_all(&dir)?;
        // Discard prompts, paths, model data and transcripts from the incoming statusLine payload.
        std::fs::write(
            dir.join("claude-rate-limits.json"),
            serde_json::to_vec(&json!({"rate_limits":value["rate_limits"]}))?,
        )?;
        return Ok(());
    }
    if args.get(1).is_some_and(|s| s == "--probe-codex") {
        let mut client = codex::Client::start(None)?;
        let q = quota(&client.read()?, true);
        ensure!(
            q.remaining.is_some(),
            "No subscription rate limits returned"
        );
        std::fs::write(
            args.get(2)
                .ok_or_else(|| anyhow::anyhow!("Missing output"))?,
            serde_json::to_vec_pretty(
                &json!({"remaining":q.remaining,"resets":q.resets,"windows":q.windows}),
            )?,
        )?;
        return Ok(());
    }
    ultra_bar_plugin_ui::run("Agent Usage", false, |panel, input| {
        let names = ["Codex", "Claude"];
        let mut data = PathBuf::new();
        let mut client: Option<codex::Client> = None;
        let mut home: Option<PathBuf> = None;
        let mut watcher = None;
        let (events, event_rx) = mpsc::sync_channel(32);
        let mut active = [None, None];
        let mut foreground_agent = None;
        let mut due = [Instant::now(), Instant::now()];
        let mut quotas = [Quota::default(), Quota::default()];
        let mut previous = Vec::new();
        let mut stopped = false;
        while !stopped {
            let mut force = false;
            let mut show = false;
            let mut refreshed = false;
            match input.recv_timeout(Duration::from_secs(1)) {
                Ok(Command::Host(HostEvent::Initialize { data_dir, .. })) => {
                    data = data_dir.into();
                    let config = data.join("connections.json");
                    let saved: Value = std::fs::read(&config)
                        .ok()
                        .and_then(|b| serde_json::from_slice(&b).ok())
                        .unwrap_or(Value::Null);
                    home = std::env::var_os("CODEX_HOME")
                        .map(PathBuf::from)
                        .or_else(|| saved["codex_home"].as_str().map(PathBuf::from));
                    if home.is_none() {
                        home = Some(
                            PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default())
                                .join(".codex"),
                        );
                    }
                    if let Some(h) = &home {
                        let _ = std::fs::write(
                            config,
                            serde_json::to_vec(&json!({"codex_home":h})).unwrap_or_default(),
                        );
                    }
                    let codex_sessions = home.as_ref().unwrap().join("sessions");
                    let claude_projects = claude::home().join("projects");
                    let c = codex_sessions.clone();
                    let a = claude_projects.clone();
                    let tx = events.clone();
                    watcher =
                        notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                            if let Ok(e) = event
                                && !matches!(e.kind, notify::EventKind::Access(_))
                            {
                                for p in e.paths {
                                    if p.extension().is_some_and(|s| s == "jsonl") {
                                        let i = if p.starts_with(&c) {
                                            0
                                        } else if p.starts_with(&a) {
                                            1
                                        } else {
                                            continue;
                                        };
                                        let _ = tx.try_send(i);
                                    }
                                }
                            }
                        })
                        .ok();
                    if let Some(w) = watcher.as_mut() {
                        if codex_sessions.exists() {
                            let _ = w.watch(&codex_sessions, RecursiveMode::Recursive);
                        }
                        if claude_projects.exists() {
                            let _ = w.watch(&claude_projects, RecursiveMode::Recursive);
                        }
                    }
                    let _ = send(&PluginEvent::Ready);
                }
                Ok(Command::Host(HostEvent::Foreground { executable, title })) => {
                    foreground_agent = foreground_provider(&executable, &title);
                    if let Some(i) = foreground_agent {
                        active[i] = Some(Instant::now());
                    }
                }
                Ok(Command::Host(HostEvent::Show)) => {
                    show = true;
                    force = true;
                }
                Ok(Command::Refresh) => {
                    force = true;
                    show = true;
                }
                Ok(Command::Host(HostEvent::Shutdown))
                | Err(mpsc::RecvTimeoutError::Disconnected) => stopped = true,
                _ => {}
            }
            while let Ok(i) = event_rx.try_recv() {
                active[i] = Some(Instant::now());
            }
            if data.as_os_str().is_empty() || stopped {
                continue;
            }
            for i in 0..2 {
                let running = foreground_agent == Some(i)
                    || active[i].is_some_and(|t| t.elapsed().as_secs() < 300);
                if (running || show) && (force || Instant::now() >= due[i]) {
                    refreshed = true;
                    let result = if i == 0 {
                        if client.is_none() {
                            client = codex::Client::start(home.as_deref()).ok();
                        }
                        client
                            .as_mut()
                            .ok_or_else(|| anyhow::anyhow!("无法连接 Codex，请检查 CLI 登录"))
                            .and_then(|c| c.read())
                    } else {
                        claude::read(&data)
                    };
                    let failed = result.is_err();
                    quotas[i] = match result {
                        Ok(v) => quota(&v, i == 0),
                        Err(e) => {
                            if i == 0 {
                                client = None;
                            }
                            Quota {
                                error: e.to_string(),
                                ..Quota::default()
                            }
                        }
                    };
                    due[i] = Instant::now() + Duration::from_secs(if failed { 300 } else { 60 });
                }
            }
            let pages: Vec<_> = (0..2)
                .filter(|i| {
                    foreground_agent == Some(*i)
                        || active[*i].is_some_and(|t| t.elapsed().as_secs() < 300)
                })
                .map(|i| page(names[i], &quotas[i]))
                .collect();
            if pages != previous {
                let _ = send(&PluginEvent::Widget {
                    pages: pages.clone(),
                });
                previous = pages;
            }
            if !show
                && foreground_agent != Some(0)
                && active[0].is_none_or(|t| t.elapsed().as_secs() >= 300)
            {
                client = None;
            }
            if force || show || refreshed {
                let rows = (0..2)
                    .flat_map(|i| {
                        let q = &quotas[i];
                        let p = page(names[i], q);
                        if q.windows.is_empty() {
                            vec![Row {
                                title: p.text,
                                detail: if q.error.is_empty() {
                                    "当前账户未提供订阅额度".into()
                                } else {
                                    q.error.clone()
                                },
                                path: None,
                            }]
                        } else {
                            q.windows
                                .iter()
                                .map(|w| Row {
                                    title: names[i].into(),
                                    detail: w.clone(),
                                    path: None,
                                })
                                .collect()
                        }
                    })
                    .collect();
                panel.update(rows, "显示剩余额度 · 每分钟更新");
            }
        }
        drop(watcher);
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reset_count_is_authoritative_and_missing_is_unknown() {
        let q = quota(
            &json!({"rateLimitResetCredits":{"availableCount":2,"credits":[]}}),
            true,
        );
        assert_eq!(q.resets, Some(2));
        assert_eq!(q.remaining, None);
        assert!(page("Codex", &q).text.ends_with("↺2"));
        assert_eq!(quota(&json!({}), true).resets, None);
        assert_eq!(
            quota(&json!({"rateLimitResetCredits":{"availableCount":0}}), true).resets,
            Some(0)
        );
    }
    #[test]
    fn editing_an_agent_named_file_is_not_agent_activity() {
        assert_eq!(
            foreground_provider("C:/VSCode/Code.exe", "claude.rs — project"),
            None
        );
        assert_eq!(foreground_provider("C:/Apps/Codex.exe", "Project"), Some(0));
        assert_eq!(
            foreground_provider("C:/Windows/WindowsTerminal.exe", "Claude Code"),
            Some(1)
        );
    }
    #[test]
    fn subscription_windows_use_lowest_remaining_not_token_cost() {
        let q = quota(
            &json!({"rateLimits":{"primary":{"usedPercent":64,"windowDurationMins":10080},"secondary":{"usedPercent":10}}}),
            true,
        );
        assert_eq!(q.remaining, Some(36.0));
        assert_eq!(page("Codex", &q).progress, Some(0.36));
    }
    #[test]
    fn missing_limits_are_unknown_never_zero() {
        assert_eq!(
            quota(&json!({"cost":{"total_cost_usd":5}}), false).remaining,
            None
        );
        assert_eq!(page("Claude", &Quota::default()).text, "Claude —");
    }
    #[test]
    fn claude_official_feed_and_compatibility_shapes() {
        assert_eq!(
            quota(
                &json!({"five_hour":{"used_percentage":25},"seven_day":{"used_percentage":40}}),
                false
            )
            .remaining,
            Some(60.0)
        );
        assert_eq!(
            quota(&json!({"five_hour":{"utilization":20}}), false).remaining,
            Some(80.0)
        );
        assert_eq!(
            quota(&json!({"five_hour":{"utilization":101}}), false).remaining,
            None
        );
    }
}
