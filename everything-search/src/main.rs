#![cfg_attr(not(test), windows_subsystem = "windows")]
mod apps;
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
        while let Ok(command) = input.recv() {
            let command = panel.settings_command(command);
            match command {
                Command::Host(HostEvent::Initialize { data_dir, .. }) => {
                    data = data_dir.into();
                    settings = settings::Settings::load(&data);
                    panel.settings(settings.fields());
                    apps = apps::load();
                    let _ = send(&PluginEvent::Ready);
                    panel.update(vec![], "搜索软件或文件");
                    if std::env::var_os("ULTRA_BAR_PLUGIN_CAPTURE_DIR").is_some() {
                        query = std::env::var("ULTRA_BAR_PLUGIN_SMOKE_QUERY").unwrap_or_default();
                    }
                }
                Command::Query(text) => query = text,
                Command::Refresh => {
                    apps = apps::load();
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
                        }
                        Err(error) => {
                            panel.settings_error(error.to_string());
                            panel.update(vec![], error.to_string());
                            continue;
                        }
                    }
                }
                _ => continue,
            }
            if query.trim().is_empty() {
                panel.update(vec![], "搜索软件或文件");
                continue;
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
            let app_count = installed.len();
            let started = std::time::Instant::now();
            let result = ipc::query(&settings.query(&query));
            ultra_bar_native_ui::diagnostics::event(
                "search",
                "query.completed",
                serde_json::json!({"query_chars":query.chars().count(),"software_results":app_count,"file_results":result.as_ref().ok().map(|v|v.0),"elapsed_ms":started.elapsed().as_millis(),"ignore_dependencies":settings.ignore_dependencies,"extra_directories":settings.ignored_directories.len(),"custom_regex_chars":settings.exclude_regex.len(),"error":result.as_ref().err().map(ToString::to_string)}),
            );
            match result {
                Ok((total, paths)) => panel.update(
                    installed
                        .into_iter()
                        .chain(paths.into_iter().map(|p| {
                            Row {
                                title: p
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into_owned(),
                                detail: p.parent().unwrap_or(&p).to_string_lossy().into_owned(),
                                path: Some(p.to_string_lossy().into_owned()),
                            }
                        }))
                        .collect(),
                    format!("{app_count} 个软件 · {total} 个文件 · Enter 打开 · Ctrl+Enter 定位"),
                ),
                Err(error) => panel.update(installed, format!("{app_count} 个软件 · {error}")),
            }
        }
    })
}
