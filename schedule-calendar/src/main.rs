#![cfg_attr(not(test), windows_subsystem = "windows")]
macro_rules! eprintln {($($arg:tt)*)=>{ultra_bar_native_ui::diagnostic_stderr!($($arg)*)};}
mod calendar;
mod editor;
mod engine;
mod http;
mod models;
mod region;
mod reminder;
mod ui;
mod weather;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::{path::PathBuf, sync::Arc};

#[derive(Deserialize)]
pub struct Configuration {
    pub title: String,
    pub views: Vec<String>,
    pub weekdays: Vec<String>,
}
fn configuration(root: &std::path::Path) -> Result<Configuration> {
    let context = rune::Context::with_default_modules()?;
    let mut sources = rune::Sources::new();
    sources.insert(rune::Source::memory(&std::fs::read_to_string(
        root.join("main.rn"),
    )?)?)?;
    let unit = rune::prepare(&mut sources).with_context(&context).build()?;
    let mut vm = rune::Vm::new(Arc::new(context.runtime()?), Arc::new(unit));
    let value = rune::runtime::budget::with(25000, || vm.call(["configuration"], ())).call()?;
    let json: String = rune::from_value(value)?;
    let configuration: Configuration = serde_json::from_str(&json)?;
    anyhow::ensure!(
        configuration.views.len() == 4 && configuration.weekdays.len() == 7,
        "invalid calendar configuration"
    );
    Ok(configuration)
}

fn main() {
    let _logs = match ultra_bar_native_ui::diagnostics::init("calendar", env!("CARGO_PKG_VERSION"))
    {
        Ok(l) => Some(l),
        Err(error) => {
            ::std::eprintln!("Cannot initialize diagnostic logs: {error}");
            None
        }
    };
    let ok = run_main();
    ultra_bar_native_ui::diagnostics::event(
        "lifecycle",
        "process.end",
        serde_json::json!({"ok":ok}),
    );
    drop(_logs);
    if !ok {
        std::process::exit(1);
    }
}
fn run_main() -> bool {
    ultra_bar_native_ui::theme::init_from_env();
    if let Err(error) = run() {
        ultra_bar_native_ui::diagnostics::event(
            "errors",
            "calendar.failed",
            serde_json::json!({"error":format!("{error:#}")}),
        );
        if std::env::args().any(|arg| {
            matches!(
                arg.as_str(),
                "--smoke-test" | "--region-smoke" | "--probe-weather"
            )
        }) {
            eprintln!("{error:#}");
            return false;
        }
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                std::ptr::null_mut(),
                ultra_bar_native_ui::wide(&format!("{error:#}")).as_ptr(),
                ultra_bar_native_ui::wide("Schedule Calendar").as_ptr(),
                0x10,
            );
        }
        return false;
    }
    true
}
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "--region-smoke") {
        let directory = std::path::Path::new(args.get(2).context("Missing output directory")?);
        std::fs::create_dir_all(directory)?;
        std::fs::write(
            directory.join("region-settings-schema.json"),
            serde_json::to_vec_pretty(&region::Config::default().fields())?,
        )?;
        return Ok(());
    }
    let probe: Vec<_> = std::env::args().collect();
    if probe.get(1).is_some_and(|s| s == "--probe-weather") {
        let config = region::Config {
            city: probe.get(2).context("Missing city")?.clone(),
            provider: 1,
            ..Default::default()
        };
        std::fs::write(
            probe.get(3).context("Missing output")?,
            serde_json::to_vec_pretty(&weather::fetch(&config)?)?,
        )?;
        return Ok(());
    }
    let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut owner = 0usize;
    let mut parent = 0u32;
    let mut show = false;
    let mut smoke = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--plugin-root" => root = args.next().context("missing plugin root")?.into(),
            "--owner" => owner = args.next().context("missing owner")?.parse()?,
            "--parent" => parent = args.next().context("missing parent")?.parse()?,
            "--show" => show = true,
            "--background" => {}
            "--smoke-test" => smoke = true,
            _ => anyhow::bail!("unknown argument: {arg}"),
        }
    }
    unsafe { ui::run(root, owner, parent, show, smoke) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn rune_configuration_has_the_original_calendar_dimensions() {
        let config =
            super::configuration(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert_eq!(config.views, ["今天", "月视图", "全部日程", "已完成"]);
        assert_eq!(config.weekdays.len(), 7);
    }
}
