//! Windows notification delivery, matching Schedule Manager's protocol-link path.
use anyhow::{Context, Result, ensure};
use std::{
    cell::Cell,
    hash::{Hash, Hasher},
    path::Path,
};
use windows::{
    Data::Xml::Dom::XmlDocument,
    UI::Notifications::{ToastNotification, ToastNotificationManager},
    Win32::{
        Storage::EnhancedStorage::PKEY_AppUserModel_ID,
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            IPersistFile,
        },
        UI::Shell::{
            IShellLinkW, PropertiesSystem::IPropertyStore, SetCurrentProcessExplicitAppUserModelID,
            ShellLink,
        },
    },
    core::{HSTRING, Interface, PCWSTR, PROPVARIANT},
};
const APP_ID: &str = "com.emssion.ultrabar.schedule-calendar";
thread_local! {static COM_READY:Cell<bool>=const{Cell::new(false)};static IDENTITY_READY:Cell<bool>=const{Cell::new(false)};}
fn prepare_identity(root: &Path) -> Result<()> {
    if IDENTITY_READY.with(Cell::get) {
        return Ok(());
    }
    unsafe {
        if !COM_READY.with(Cell::get) {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
            COM_READY.with(|ready| ready.set(true));
        }
        SetCurrentProcessExplicitAppUserModelID(PCWSTR(
            ultra_bar_native_ui::wide(APP_ID).as_ptr(),
        ))?;
        let executable = std::env::current_exe()?;
        let shortcut =
            std::path::PathBuf::from(std::env::var_os("APPDATA").context("APPDATA unavailable")?)
                .join("Microsoft/Windows/Start Menu/Programs/Ultra Bar Schedule Calendar.lnk");
        std::fs::create_dir_all(shortcut.parent().unwrap())?;
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(PCWSTR(
            ultra_bar_native_ui::wide(&executable.to_string_lossy()).as_ptr(),
        ))?;
        link.SetArguments(PCWSTR(
            ultra_bar_native_ui::wide(&format!("--plugin-root \"{}\" --show", root.display()))
                .as_ptr(),
        ))?;
        link.SetWorkingDirectory(PCWSTR(
            ultra_bar_native_ui::wide(&root.to_string_lossy()).as_ptr(),
        ))?;
        link.SetIconLocation(
            PCWSTR(ultra_bar_native_ui::wide(&executable.to_string_lossy()).as_ptr()),
            0,
        )?;
        let properties: IPropertyStore = link.cast()?;
        properties.SetValue(&PKEY_AppUserModel_ID, &PROPVARIANT::from(APP_ID))?;
        properties.Commit()?;
        let file: IPersistFile = link.cast()?;
        file.Save(
            PCWSTR(ultra_bar_native_ui::wide(&shortcut.to_string_lossy()).as_ptr()),
            true,
        )?;
    }
    IDENTITY_READY.with(|ready| ready.set(true));
    Ok(())
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn xml(title: &str, body: &str, link: &str) -> Result<String> {
    let activation = if link.is_empty() {
        String::new()
    } else {
        let url = url::Url::parse(link)?;
        ensure!(
            matches!(url.scheme(), "http" | "https") && url.host_str().is_some(),
            "通知链接必须是 http 或 https 网页地址"
        );
        format!(
            " activationType=\"protocol\" launch=\"{}\"",
            escape(url.as_str())
        )
    };
    let actions = if link.is_empty() {
        String::new()
    } else {
        format!(
            "<actions><action content=\"打开链接\" activationType=\"protocol\" arguments=\"{}\"/></actions>",
            escape(url::Url::parse(link)?.as_str())
        )
    };
    Ok(format!(
        "<toast{activation}><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual>{actions}<audio silent=\"true\"/></toast>",
        escape(title),
        escape(body)
    ))
}
pub fn sound() {
    unsafe {
        windows_sys::Win32::System::Diagnostics::Debug::MessageBeep(
            windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONEXCLAMATION,
        );
    }
}
pub fn show(pending: &crate::engine::Pending, root: &Path, with_sound: bool) -> Result<()> {
    prepare_identity(root).context("注册通知身份失败")?;
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(APP_ID))
        .context("创建 Windows 通知发送器失败")?;
    let body = format!(
        "{} 开始 · 提前 {} 分钟\n{}",
        pending
            .occurrence
            .with_timezone(&crate::region::timezone())
            .format("%m-%d %H:%M"),
        pending.offset,
        pending.event.location
    );
    let document = XmlDocument::new()?;
    document.LoadXml(&HSTRING::from(xml(
        &pending.event.title,
        &body,
        &pending.event.notification_url,
    )?))?;
    let toast = ToastNotification::CreateToastNotification(&document)?;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    pending.key.hash(&mut hash);
    toast.SetTag(&HSTRING::from(format!("{:016x}", hash.finish())))?;
    toast.SetGroup(&HSTRING::from("schedule"))?;
    notifier.Show(&toast).context("发送系统通知失败")?;
    if with_sound {
        sound();
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn toast_escapes_text_and_only_activates_web_links() {
        let text = xml("A < B", "A & B", "https://example.com/?a=1&b=2").unwrap();
        assert!(text.contains("A &lt; B") && text.contains("A &amp; B"));
        assert!(text.contains("activationType=\"protocol\"") && text.contains("a=1&amp;b=2"));
        assert!(xml("x", "y", "file:///C:/test").is_err());
        assert!(!xml("x", "y", "").unwrap().contains("<actions>"));
    }
}
