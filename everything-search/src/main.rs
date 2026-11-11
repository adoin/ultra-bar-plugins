#![cfg_attr(not(test), windows_subsystem = "windows")]
mod apps;
mod ipc;
mod settings;
use ultra_bar_plugin_ui::{Command, HostEvent, PluginEvent, Row, send};
fn main() -> anyhow::Result<()> {
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
        let (total, paths) = ipc::query(
            &settings::Settings::default()
                .query(args.get(2).map(String::as_str).unwrap_or("ultra-bar")),
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
                            settings = next;
                            panel.settings(settings.fields());
                        }
                        Err(error) => {
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
            let installed = apps::search(&apps, &query);
            let app_count = installed.len();
            match ipc::query(&settings.query(&query)) {
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
