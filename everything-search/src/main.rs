#![cfg_attr(not(test), windows_subsystem = "windows")]
mod apps;
mod commands;
mod ipc;
mod settings;
use ultra_bar_plugin_ui::{Command, HostEvent, PluginEvent, Row, send};
fn main() -> anyhow::Result<()> {
    let logs =
        ultra_bar_native_ui::diagnostics::init("everything-search", env!("CARGO_PKG_VERSION"))?;
    let result = run_main();
    ultra_bar_native_ui::diagnostics::event(
        "lifecycle",
        "process.end",
        serde_json::json!({"ok":result.is_ok(),"error":result.as_ref().err().map(|e|format!("{e:#}"))}),
    );
    drop(logs);
    result
}
fn run_main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "--probe-input-sequence") {
        let settings = settings::Settings::default();
        let searches: Vec<_> = ["i", "im", "ima", "imag", "image"]
            .into_iter()
            .map(|text| settings.query(text))
            .collect();
        let result = ipc::probe_latest(&searches)?;
        std::fs::write(
            args.get(2)
                .ok_or_else(|| anyhow::anyhow!("Missing probe output"))?,
            serde_json::to_vec_pretty(&result)?,
        )?;
        return Ok(());
    }
    if args.get(1).is_some_and(|s| s == "--probe-apps") {
        std::fs::write(
            args.get(2)
                .ok_or_else(|| anyhow::anyhow!("Missing output"))?,
            serde_json::to_vec_pretty(&apps::load())?,
        )?;
        return Ok(());
    }
    if args.get(1).is_some_and(|s| s == "--probe") {
        let mut probe_settings = settings::Settings::default();
        if args.iter().any(|s| s == "--include-dependencies") {
            probe_settings.ignore_dependencies = false;
        }
        let (total, paths) = ipc::query(
            &probe_settings.query(args.get(2).map(String::as_str).unwrap_or("ultra-bar")),
        )?;
        let output = serde_json::json!({"total":total,"paths":paths});
        std::fs::write(
            args.get(3)
                .ok_or_else(|| anyhow::anyhow!("Missing output"))?,
            serde_json::to_vec_pretty(&output)?,
        )?;
        return Ok(());
    }
    ultra_bar_plugin_ui::run("Everything", true, |panel, input| {
        let mut query = String::new();
        let mut data = std::path::PathBuf::new();
        let mut settings = settings::Settings::default();
        let mut apps = Vec::new();
        let mut commands = commands::Commands::default();
        let mut client = ipc::Client::new();
        let mut pending: Option<PendingSearch> = None;
        loop {
            let command = if pending.is_some() {
                match commands.next_timeout(&input, std::time::Duration::from_millis(20)) {
                    Ok(command) => Some(command),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            } else {
                match commands.next(&input) {
                    Ok(command) => Some(command),
                    Err(_) => break,
                }
            };
            let refresh = if let Some(command) = command {
                match panel.settings_command(command) {
                    Command::Host(HostEvent::Initialize { data_dir, .. }) => {
                        data = data_dir.into();
                        settings = settings::Settings::load(&data);
                        panel.settings(settings.fields());
                        apps = apps::load();
                        let _ = send(&PluginEvent::Ready);
                        panel.update_query("", 0, vec![], "搜索软件或文件");
                        if std::env::var_os("ULTRA_BAR_PLUGIN_CAPTURE_DIR").is_some() {
                            query =
                                std::env::var("ULTRA_BAR_PLUGIN_SMOKE_QUERY").unwrap_or_default();
                        }
                        !query.trim().is_empty()
                    }
                    Command::Query(text) => {
                        query = text;
                        true
                    }
                    Command::Refresh => {
                        apps = apps::load();
                        true
                    }
                    Command::SettingsSaved(values) => {
                        match settings::Settings::from_values(&values).and_then(|next| {
                            next.save(&data)?;
                            Ok(next)
                        }) {
                            Ok(next) => {
                                ultra_bar_native_ui::diagnostics::event(
                                    "settings",
                                    "search.saved",
                                    serde_json::json!({"ignore_dependencies":next.ignore_dependencies,"ignored_directories":next.ignored_directories,"exclude_regex":next.exclude_regex}),
                                );
                                settings = next;
                                panel.settings(settings.fields());
                                true
                            }
                            Err(error) => {
                                panel.settings_error(error.to_string());
                                panel.update_query(
                                    &query,
                                    panel.query_revision(),
                                    vec![],
                                    error.to_string(),
                                );
                                client.cancel();
                                pending = None;
                                false
                            }
                        }
                    }
                    Command::Host(HostEvent::Shutdown) => break,
                    _ => false,
                }
            } else {
                false
            };
            if refresh {
                if query.trim().is_empty() {
                    client.cancel();
                    pending = None;
                    panel.update_query(&query, panel.query_revision(), vec![], "搜索软件或文件");
                    continue;
                }
                if let Some(old) = &pending {
                    ultra_bar_native_ui::diagnostics::event(
                        "search",
                        "query.replaced",
                        serde_json::json!({"old_chars":old.query.chars().count(),"new_chars":query.chars().count(),"elapsed_ms":old.started.elapsed().as_millis()}),
                    );
                }
                let excluded = (!settings.exclude_regex.is_empty())
                    .then(|| regex::Regex::new(&settings.exclude_regex).ok())
                    .flatten();
                let installed: Vec<_> = apps::search(&apps, &query)
                    .into_iter()
                    .filter(|row| {
                        row.path
                            .as_ref()
                            .is_none_or(|p| excluded.as_ref().is_none_or(|r| !r.is_match(p)))
                    })
                    .collect();
                let context = PendingSearch {
                    query: query.clone(),
                    revision: panel.query_revision(),
                    installed,
                    started: std::time::Instant::now(),
                };
                match client.start(&settings.query(&query)) {
                    Ok(()) => {
                        panel.update_query(
                            &query,
                            context.revision,
                            context.installed.clone(),
                            format!("{} 个软件 · 正在搜索文件…", context.installed.len()),
                        );
                        pending = Some(context);
                    }
                    Err(error) => {
                        finish(&panel, context, Err(error), &settings);
                        pending = None;
                    }
                }
            }
            if let Some(result) = client.poll()
                && let Some(context) = pending.take()
            {
                finish(&panel, context, result, &settings);
            }
        }
    })
}
struct PendingSearch {
    query: String,
    revision: u64,
    installed: Vec<Row>,
    started: std::time::Instant,
}
fn finish(
    panel: &ultra_bar_plugin_ui::Panel,
    context: PendingSearch,
    result: anyhow::Result<(u32, Vec<std::path::PathBuf>)>,
    settings: &settings::Settings,
) {
    let count = context.installed.len();
    ultra_bar_native_ui::diagnostics::event(
        "search",
        "query.completed",
        serde_json::json!({"query_chars":context.query.chars().count(),"software_results":count,"file_results":result.as_ref().ok().map(|v|v.0),"elapsed_ms":context.started.elapsed().as_millis(),"ignore_dependencies":settings.ignore_dependencies,"extra_directories":settings.ignored_directories.len(),"custom_regex_chars":settings.exclude_regex.len(),"error":result.as_ref().err().map(ToString::to_string)}),
    );
    match result {
        Ok((total, paths)) => panel.update_query(
            &context.query,
            context.revision,
            context
                .installed
                .into_iter()
                .chain(paths.into_iter().map(|path| {
                    Row {
                        title: path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        detail: path
                            .parent()
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .into_owned(),
                        path: Some(path.to_string_lossy().into_owned()),
                    }
                }))
                .collect(),
            format!("{count} 个软件 · {total} 个文件 · Enter 打开 · Ctrl+Enter 定位"),
        ),
        Err(error) => panel.update_query(
            &context.query,
            context.revision,
            context.installed,
            format!("{count} 个软件 · {error}"),
        ),
    }
}
