#![cfg_attr(not(test), windows_subsystem = "windows")]
mod activity;
mod cache;
mod claude;
mod codex;
mod grok;
mod statistics;
use anyhow::{Result, ensure};
use notify::{RecursiveMode, Watcher};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use ultra_bar_plugin_ui::{Command, HostEvent, Page, PluginEvent, send};
#[derive(Clone, Debug, Default)]
struct Quota {
    fetched: Option<u64>,
    remaining: Option<f64>,
    resets: Option<u64>,
    next_reset: Option<u64>,
    reset_credits: Option<Vec<ultra_bar_plugin_ui::statistics::ResetCredit>>,
    windows: Vec<String>,
    error: String,
}
fn foreground_provider(executable: &str, title: &str) -> Option<usize> {
    // The packaged Codex desktop client uses ChatGPT.exe. Its package identity
    // distinguishes it from the separate ChatGPT desktop app sharing that filename.
    let normalized = executable.replace('\\', "/").to_ascii_lowercase();
    let components: Vec<_> = normalized.rsplit('/').take(3).collect();
    if components.len() == 3
        && components[0] == "chatgpt.exe"
        && components[1] == "app"
        && components[2].starts_with("openai.codex_")
    {
        return Some(0);
    }
    let name = std::path::Path::new(executable)
        .file_name()?
        .to_str()?
        .to_ascii_lowercase();
    match name.as_str() {
        "codex.exe" => Some(0),
        "claude.exe" => Some(1),
        "grok bot.exe" => Some(2),
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
                windows.push((v.0, v.1, w["resetsAt"].as_u64()));
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
                windows.push((v.0, v.1, w["resets_at"].as_u64()));
            }
        }
    }
    let resets = is_codex
        .then(|| value["rateLimitResetCredits"]["availableCount"].as_u64())
        .flatten();
    let limiting = windows
        .iter()
        .min_by(|a, b| a.0.total_cmp(&b.0).then(a.2.cmp(&b.2)));
    let remaining = limiting.map(|w| w.0);
    let next_reset = limiting.and_then(|w| w.2);
    let reset_credits = is_codex
        .then(|| value["rateLimitResetCredits"]["credits"].as_array())
        .flatten()
        .map(|credits| {
            credits
                .iter()
                .take(128)
                .map(|credit| ultra_bar_plugin_ui::statistics::ResetCredit {
                    title: credit["title"]
                        .as_str()
                        .unwrap_or("额度重置")
                        .chars()
                        .take(128)
                        .collect(),
                    status: credit["status"].as_str().unwrap_or("unknown").into(),
                    expires_at: credit["expiresAt"].as_i64(),
                    expiry_known: credit.get("expiresAt").is_some(),
                    granted_at: credit["grantedAt"].as_i64(),
                })
                .collect()
        });
    let mut windows: Vec<String> = windows.into_iter().map(|w| w.1).collect();
    if let Some(count) = resets {
        windows.push(format!("手动重置剩余 {count} 次"));
    }
    Quota {
        fetched: remaining.map(|_| cache::now()),
        remaining,
        resets,
        next_reset,
        reset_credits,
        windows,
        error: String::new(),
    }
}
fn countdown(at: u64, now: u64) -> String {
    let seconds = at.saturating_sub(now);
    if seconds == 0 {
        return "待刷新".into();
    }
    let minutes = seconds.div_ceil(60);
    if minutes >= 1440 {
        let hours = minutes.div_ceil(60);
        format!("{}d{}h", hours / 24, hours % 24)
    } else if minutes >= 60 {
        format!("{}h{:02}m", minutes / 60, minutes % 60)
    } else {
        format!("{minutes}m")
    }
}
fn page(name: &str, q: &Quota) -> Page {
    let text = q
        .remaining
        .map(|r| format!("{name} {r:.0}%"))
        .unwrap_or_else(|| format!("{name} —"));
    let mut detail = if q.windows.is_empty() {
        "当前账户未提供订阅额度".into()
    } else {
        q.windows.join("\n")
    };
    if !q.error.is_empty() {
        detail = if q.windows.is_empty() {
            q.error.clone()
        } else {
            format!("{detail}\n{}", q.error)
        };
    }
    if let Some(at) = q.fetched {
        detail.push_str(&format!(
            "\n上次成功读取：{} 分钟前",
            cache::now().saturating_sub(at) / 60
        ));
    }
    Page {
        id: name.into(),
        text,
        trailing: if name == "Codex" {
            q.next_reset
                .map(|at| countdown(at, cache::now()))
                .unwrap_or_default()
        } else {
            String::new()
        },
        tooltip: format!("{name}\n{detail}"),
        progress: q.remaining.map(|v| v / 100.0),
    }
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "--statistics-smoke") {
        ultra_bar_plugin_ui::statistics::smoke(std::path::Path::new(
            args.get(2)
                .ok_or_else(|| anyhow::anyhow!("Missing output directory"))?,
        ))?;
        return Ok(());
    }
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
                &json!({"remaining":q.remaining,"resets":q.resets,"next_reset":q.next_reset,"reset_credits":q.reset_credits,"windows":q.windows}),
            )?,
        )?;
        return Ok(());
    }
    ultra_bar_plugin_ui::run("Agent Usage", false, |panel, input| {
        let names = ["Codex", "Claude", "Grok Bot"];
        let mut data = PathBuf::new();
        let mut enabled = [true; 3];
        let mut client: Option<codex::Client> = None;
        let mut home: Option<PathBuf> = None;
        let mut watcher = None;
        let (events, event_rx) = mpsc::sync_channel(32);
        let mut active = [None, None, None];
        let mut foreground_agent = None;
        let mut due = [Instant::now(), Instant::now(), Instant::now()];
        let mut quotas = [Quota::default(), Quota::default(), Quota::default()];
        let mut previous = Vec::new();
        let mut stopped = false;
        let mut failures = [0u32; 3];
        let mut statistics: Option<statistics::Service> = None;
        let mut statistics_dirty = false;
        let mut statistics_at = Instant::now();
        let quiet = activity::Quiet::default();
        while !stopped {
            let mut force = false;
            let mut show = false;
            let mut refreshed = false;
            match input
                .recv_timeout(Duration::from_secs(1))
                .map(|command| panel.settings_command(command))
            {
                Ok(Command::Host(HostEvent::Initialize { data_dir, .. })) => {
                    data = data_dir.into();
                    enabled = std::fs::read(data.join("preferences.json"))
                        .ok()
                        .and_then(|b| serde_json::from_slice(&b).ok())
                        .unwrap_or([true; 3]);
                    panel.settings(agent_fields(&enabled));
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
                    if let Some(saved) = cache::load(&data, home.as_ref().unwrap()) {
                        quotas[0] = saved;
                    }
                    statistics = Some(statistics::Service::start(
                        home.as_ref().unwrap().clone(),
                        data.clone(),
                        panel.clone(),
                    ));
                    active[0] = activity::recent(home.as_ref().unwrap());
                    let claude_projects = claude::home().join("projects");
                    let c = codex_sessions.clone();
                    let a = claude_projects.clone();
                    let tx = events.clone();
                    let h = home.as_ref().unwrap().clone();
                    let q = quiet.clone();
                    watcher =
                        notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                            if let Ok(e) = event {
                                for (i, changed) in activity::sources(&e, &h, &c, &a, &q)
                                    .into_iter()
                                    .enumerate()
                                {
                                    if changed {
                                        let _ = tx.try_send(i);
                                    }
                                }
                            }
                        })
                        .ok();
                    if let Some(w) = watcher.as_mut() {
                        let _ = w.watch(home.as_ref().unwrap(), RecursiveMode::NonRecursive);
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
                    if let Some(stats) = &statistics {
                        stats.show();
                    }
                    show = true;
                    force = true;
                }
                Ok(Command::SettingsSaved(values)) => {
                    if values.len() == 3 {
                        let next = std::array::from_fn(|i| values[i] == "1");
                        let saved = (|| -> anyhow::Result<()> {
                            let pending = data.join("preferences.json.new");
                            std::fs::write(&pending, serde_json::to_vec_pretty(&next)?)?;
                            std::fs::rename(pending, data.join("preferences.json"))?;
                            Ok(())
                        })();
                        if saved.is_ok() {
                            enabled = next;
                            panel.settings(agent_fields(&enabled));
                        } else if let Err(error) = saved {
                            panel.settings_error(error.to_string());
                            panel.update(vec![], error.to_string());
                        }
                    }
                }
                Ok(Command::Refresh) => {
                    if let Some(stats) = &statistics {
                        stats.show();
                    }
                    force = true;
                    show = true;
                }
                Ok(Command::Host(HostEvent::Shutdown))
                | Err(mpsc::RecvTimeoutError::Disconnected) => stopped = true,
                _ => {}
            }
            while let Ok(i) = event_rx.try_recv() {
                statistics_dirty = true;
                active[i] = Some(Instant::now());
            }
            if statistics_dirty
                && statistics_at.elapsed().as_secs() >= 5
                && panel.visible()
                && let Some(stats) = &statistics
            {
                stats.show();
                statistics_dirty = false;
                statistics_at = Instant::now();
            }
            if data.as_os_str().is_empty() || stopped {
                continue;
            }
            let initial: Vec<_> = (0..3)
                .filter(|i| {
                    enabled[*i]
                        && (foreground_agent == Some(*i)
                            || active[*i].is_some_and(|t| t.elapsed() < activity::TTL))
                })
                .map(|i| {
                    if quotas[i].remaining.is_none() && quotas[i].error.is_empty() {
                        quotas[i].error = "正在读取额度".into();
                    }
                    page(names[i], &quotas[i])
                })
                .collect();
            if initial != previous {
                let _ = send(&PluginEvent::Widget {
                    pages: initial.clone(),
                });
                previous = initial;
            }
            for i in 0..3 {
                let running = foreground_agent == Some(i)
                    || active[i].is_some_and(|t| t.elapsed().as_secs() < 300);
                if (running || show) && (force || Instant::now() >= due[i]) {
                    let _own_query = (i == 0).then(|| quiet.query());
                    refreshed = true;
                    let query_started = Instant::now();
                    let result = if i == 0 {
                        let connected = if client.is_none() {
                            codex::Client::start(home.as_deref()).map(|next| client = Some(next))
                        } else {
                            Ok(())
                        };
                        connected
                            .and_then(|_| client.as_mut().unwrap().read())
                            .map(|v| quota(&v, true))
                    } else if i == 1 {
                        claude::read(&data).map(|v| quota(&v, false))
                    } else {
                        grok::read()
                    };
                    ultra_bar_native_ui::diagnostics::event(
                        "usage",
                        "quota.read",
                        serde_json::json!({"agent":names[i],"elapsed_ms":query_started.elapsed().as_millis(),"ok":result.is_ok(),"error":result.as_ref().err().map(|e|format!("{e:#}")),"previous_failures":failures[i]}),
                    );
                    let failed = result.is_err();
                    quotas[i] = match result {
                        Ok(q) => {
                            if i == 0 {
                                cache::save(&data, home.as_ref().unwrap(), &q);
                            }
                            q
                        }
                        Err(e) => {
                            if i == 0 && client.as_mut().is_some_and(|c| !c.alive()) {
                                client = None;
                            }
                            cache::failure(&quotas[i], format!("{e:#}"))
                        }
                    };
                    failures[i] = if failed {
                        failures[i].saturating_add(1)
                    } else {
                        0
                    };
                    due[i] = Instant::now()
                        + Duration::from_secs(if failed {
                            cache::retry(failures[i])
                        } else {
                            60
                        });
                }
            }
            let pages: Vec<_> = (0..3)
                .filter(|i| {
                    enabled[*i]
                        && (foreground_agent == Some(*i)
                            || active[*i].is_some_and(|t| t.elapsed().as_secs() < 300))
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
                && client.is_some()
                && foreground_agent != Some(0)
                && active[0].is_none_or(|t| t.elapsed().as_secs() >= 300)
            {
                let _own_query = quiet.query();
                client = None;
            }
            if (force || show || refreshed)
                && let Some(stats) = &statistics
            {
                stats.quota(&names, &quotas);
            }
        }
        drop(watcher);
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn codex_msix_client_is_recognized_without_matching_the_chatgpt_client() {
        assert_eq!(
            foreground_provider(
                r"C:\Program Files\WindowsApps\OpenAI.Codex_26.1103.7773.0_x64__2p2nqsd0c76g0\app\ChatGPT.exe",
                "Ultra Bar"
            ),
            Some(0)
        );
        assert_eq!(
            foreground_provider(
                "D:/WindowsApps/OpenAI.Codex_27.1.0_x64__publisher/app/ChatGPT.EXE",
                ""
            ),
            Some(0)
        );
        assert_eq!(
            foreground_provider(
                r"C:\Program Files\WindowsApps\OpenAI.ChatGPT_26.1_x64__publisher\app\ChatGPT.exe",
                "Codex"
            ),
            None
        );
        assert_eq!(
            foreground_provider(r"D:\Applications\ChatGPT.exe", "Codex"),
            None
        );
    }
    #[test]
    fn reset_details_and_countdown_use_the_limiting_window() {
        let now = cache::now();
        let q = quota(
            &json!({"rateLimits":{"primary":{"usedPercent":10,"resetsAt":now+600},"secondary":{"usedPercent":87,"resetsAt":now+15360}},"rateLimitResetCredits":{"availableCount":3,"credits":[{"status":"available","expiresAt":1800000000},{"status":"available","expiresAt":null}]}}),
            true,
        );
        assert_eq!(q.next_reset, Some(now + 15360));
        assert_eq!(page("Codex", &q).trailing, "4h16m");
        assert_eq!(q.resets, Some(3));
        let credits = q.reset_credits.unwrap();
        assert_eq!(credits.len(), 2);
        assert_eq!(credits[0].expires_at, Some(1800000000));
        assert!(credits[1].expiry_known);
        assert_eq!(credits[1].expires_at, None);
        assert!(
            quota(
                &json!({"rateLimitResetCredits":{"availableCount":2,"credits":null}}),
                true
            )
            .reset_credits
            .is_none()
        );
        assert_eq!(countdown(100, 100), "待刷新");
        assert_eq!(countdown(101, 100), "1m");
        assert_eq!(countdown(100 + 86399, 100), "1d0h");
    }
    #[test]
    fn reset_count_is_authoritative_and_missing_is_unknown() {
        let q = quota(
            &json!({"rateLimitResetCredits":{"availableCount":2,"credits":[]}}),
            true,
        );
        assert_eq!(q.resets, Some(2));
        assert_eq!(q.remaining, None);
        assert_eq!(page("Codex", &q).trailing, "");
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

fn agent_fields(enabled: &[bool; 3]) -> Vec<ultra_bar_native_ui::form::Field> {
    [
        "在任务栏显示 Codex",
        "在任务栏显示 Claude",
        "在任务栏显示 Grok Bot",
    ]
    .iter()
    .enumerate()
    .map(|(i, name)| {
        ultra_bar_native_ui::form::Field::choice(name, usize::from(enabled[i]), &["关闭", "开启"])
    })
    .collect()
}
