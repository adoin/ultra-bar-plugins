//! Installed desktop programs and packaged AppsFolder applications. Never run uninstall commands.
use std::ffi::c_void;
use std::{
    collections::HashSet,
    path::PathBuf,
    ptr::{null, null_mut},
};
use windows_sys::{
    Win32::{
        Foundation::*,
        System::{Com::*, Registry::*},
        UI::Shell::*,
    },
    core::GUID,
};
#[derive(Clone, serde::Serialize)]
pub struct App {
    pub name: String,
    pub publisher: String,
    pub launch: Option<String>,
}
struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
unsafe fn string(key: HKEY, name: &str) -> String {
    unsafe {
        let mut buffer = vec![0u16; 32768];
        let mut bytes = (buffer.len() * 2) as u32;
        let mut kind = 0;
        if RegQueryValueExW(
            key,
            wide(name).as_ptr(),
            null(),
            &mut kind,
            buffer.as_mut_ptr().cast(),
            &mut bytes,
        ) != 0
            || !matches!(kind, REG_SZ | REG_EXPAND_SZ)
        {
            return String::new();
        }
        let end = buffer.iter().position(|&x| x == 0).unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end])
    }
}
unsafe fn number(key: HKEY, name: &str) -> u32 {
    unsafe {
        let mut v = 0u32;
        let mut n = 4;
        let mut kind = 0;
        if RegQueryValueExW(
            key,
            wide(name).as_ptr(),
            null(),
            &mut kind,
            (&mut v as *mut u32).cast(),
            &mut n,
        ) == 0
            && kind == REG_DWORD
        {
            v
        } else {
            0
        }
    }
}
fn normalized(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
fn shortcuts() -> Vec<(String, PathBuf)> {
    let mut found = vec![];
    for variable in ["APPDATA", "PROGRAMDATA"] {
        let Some(root) = std::env::var_os(variable) else {
            continue;
        };
        let mut todo = vec![PathBuf::from(root).join("Microsoft/Windows/Start Menu/Programs")];
        while let Some(dir) = todo.pop() {
            for item in std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .take(4096)
            {
                let p = item.path();
                if item
                    .file_type()
                    .is_ok_and(|t| t.is_dir() && !t.is_symlink())
                    && todo.len() < 512
                {
                    todo.push(p);
                } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk")) {
                    let name = p.file_stem().unwrap_or_default().to_string_lossy();
                    if !name.to_ascii_lowercase().contains("uninstall") {
                        found.push((normalized(&name), p));
                    }
                }
                if found.len() >= 4096 {
                    return found;
                }
            }
        }
    }
    found
}
fn launch(name: &str, icon: &str, location: &str, links: &[(String, PathBuf)]) -> Option<String> {
    let target = normalized(name);
    let best = links
        .iter()
        .filter_map(|(n, p)| {
            let score = if *n == target {
                0
            } else if target.starts_with(n) && n.len() >= 3 {
                target.len() - n.len() + 1
            } else if n.starts_with(&target) && target.len() >= 3 {
                n.len() - target.len() + 5
            } else {
                return None;
            };
            Some((score, p))
        })
        .min_by_key(|(score, _)| *score);
    if let Some((_, p)) = best {
        return Some(p.to_string_lossy().into_owned());
    }
    let trimmed = icon.trim();
    let raw = if let Some(v) = trimmed.strip_prefix('"') {
        v.split('"').next().unwrap_or("")
    } else {
        trimmed
            .rsplit_once(',')
            .filter(|(_, s)| s.trim().parse::<i32>().is_ok())
            .map(|(p, _)| p)
            .unwrap_or(trimmed)
    };
    let valid = |p: &std::path::Path| {
        p.is_file()
            && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))
            && !p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase()
                .contains("unins")
    };
    let path = PathBuf::from(raw);
    if path.is_absolute() && valid(&path) {
        return Some(path.to_string_lossy().into_owned());
    }
    let root = PathBuf::from(location);
    if root.is_absolute() {
        let candidates: Vec<_> = std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .take(128)
            .map(|e| e.path())
            .filter(|p| valid(p))
            .collect();
        if let Some(p) = candidates
            .iter()
            .find(|p| normalized(&p.file_stem().unwrap_or_default().to_string_lossy()) == target)
            .or_else(|| (candidates.len() == 1).then(|| &candidates[0]))
        {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    None
}
struct Object(*mut c_void);
impl Object {
    unsafe fn method(&self, index: usize) -> *const c_void {
        unsafe { *(*(self.0 as *const *const *const c_void)).add(index) }
    }
}
impl Drop for Object {
    fn drop(&mut self) {
        unsafe {
            let release: unsafe extern "system" fn(*mut c_void) -> u32 =
                std::mem::transmute(self.method(2));
            release(self.0);
        }
    }
}
struct Apartment(bool);
impl Drop for Apartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe {
                CoUninitialize();
            }
        }
    }
}
unsafe fn take_text(pointer: *mut u16) -> String {
    unsafe {
        if pointer.is_null() {
            return String::new();
        }
        let mut length = 0;
        while length < 32768 && *pointer.add(length) != 0 {
            length += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length));
        CoTaskMemFree(pointer.cast());
        text
    }
}
#[repr(C)]
struct PropertyKey {
    format: GUID,
    id: u32,
}
fn packaged_apps() -> anyhow::Result<Vec<App>> {
    unsafe {
        let hr = CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
        anyhow::ensure!(
            hr >= 0 || hr == 0x80010106u32 as i32,
            "AppsFolder COM initialization failed"
        );
        let _apartment = Apartment(hr >= 0);
        let mut pointer = null_mut();
        anyhow::ensure!(
            SHCreateItemFromParsingName(
                wide("shell:AppsFolder").as_ptr(),
                null_mut(),
                &GUID::from_u128(0x43826d1e_e718_42ee_bc55_a1e261c37bfe),
                &mut pointer
            ) >= 0
                && !pointer.is_null(),
            "Cannot open AppsFolder"
        );
        let folder = Object(pointer);
        let bind: unsafe extern "system" fn(
            *mut c_void,
            *mut c_void,
            *const GUID,
            *const GUID,
            *mut *mut c_void,
        ) -> i32 = std::mem::transmute(folder.method(3));
        let mut pointer = null_mut();
        anyhow::ensure!(
            bind(
                folder.0,
                null_mut(),
                &GUID::from_u128(0x94f60519_2850_4924_aa5a_d15e84868039),
                &GUID::from_u128(0x70629033_e363_4a28_a567_0db78006e6d7),
                &mut pointer
            ) >= 0
                && !pointer.is_null(),
            "Cannot enumerate AppsFolder"
        );
        let items = Object(pointer);
        let next: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void, *mut u32) -> i32 =
            std::mem::transmute(items.method(3));
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        for _ in 0..4096 {
            let mut pointer = null_mut();
            let mut fetched = 0;
            let status = next(items.0, 1, &mut pointer, &mut fetched);
            if status < 0 {
                anyhow::bail!("AppsFolder enumeration failed: {status:#x}");
            }
            if fetched == 0 || pointer.is_null() {
                break;
            }
            let item = Object(pointer);
            let query: unsafe extern "system" fn(
                *mut c_void,
                *const GUID,
                *mut *mut c_void,
            ) -> i32 = std::mem::transmute(item.method(0));
            let mut pointer = null_mut();
            if query(
                item.0,
                &GUID::from_u128(0x7e9fb0d3_919f_4307_ab2e_9b1860310c93),
                &mut pointer,
            ) < 0
                || pointer.is_null()
            {
                continue;
            }
            let item2 = Object(pointer);
            let get: unsafe extern "system" fn(
                *mut c_void,
                *const PropertyKey,
                *mut *mut u16,
            ) -> i32 = std::mem::transmute(item2.method(17));
            let mut pointer = null_mut();
            let status = get(
                item2.0,
                &PropertyKey {
                    format: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
                    id: 5,
                },
                &mut pointer,
            );
            let id = take_text(pointer);
            if status < 0 || !id.contains('!') || !seen.insert(id.to_ascii_lowercase()) {
                continue;
            }
            let display: unsafe extern "system" fn(*mut c_void, i32, *mut *mut u16) -> i32 =
                std::mem::transmute(item.method(5));
            let mut pointer = null_mut();
            let status = display(item.0, SIGDN_NORMALDISPLAY, &mut pointer);
            let name = take_text(pointer);
            if status < 0 || name.is_empty() {
                continue;
            }
            result.push(App {
                name,
                publisher: "商店应用".into(),
                launch: Some(format!("shell:AppsFolder\\{id}")),
            });
        }
        Ok(result)
    }
}
fn merge_packaged(apps: &mut Vec<App>, incoming: Vec<App>) {
    for app in incoming {
        if apps.iter().any(|old| {
            old.launch
                .as_ref()
                .zip(app.launch.as_ref())
                .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
        }) {
            continue;
        }
        if let Some(old) = apps
            .iter_mut()
            .find(|old| old.name.eq_ignore_ascii_case(&app.name) && old.launch.is_none())
        {
            *old = app;
        } else {
            apps.push(app);
        }
    }
}
pub fn load() -> Vec<App> {
    let links = shortcuts();
    let mut apps = vec![];
    let mut seen = HashSet::new();
    unsafe {
        for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
            for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
                let mut key = null_mut();
                if RegOpenKeyExW(
                    hive,
                    wide("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall").as_ptr(),
                    0,
                    KEY_READ | view,
                    &mut key,
                ) != 0
                {
                    continue;
                }
                let key = Key(key);
                for i in 0..8192 {
                    let mut name = [0u16; 256];
                    let mut len = name.len() as u32;
                    let status = RegEnumKeyExW(
                        key.0,
                        i,
                        name.as_mut_ptr(),
                        &mut len,
                        null(),
                        null_mut(),
                        null_mut(),
                        null_mut(),
                    );
                    if status == ERROR_NO_MORE_ITEMS {
                        break;
                    }
                    if status != 0 {
                        continue;
                    }
                    let mut sub = null_mut();
                    if RegOpenKeyExW(key.0, name.as_ptr(), 0, KEY_READ | view, &mut sub) != 0 {
                        continue;
                    }
                    let sub = Key(sub);
                    let name = string(sub.0, "DisplayName");
                    if name.is_empty()
                        || number(sub.0, "SystemComponent") == 1
                        || !string(sub.0, "ParentKeyName").is_empty()
                        || !seen.insert(normalized(&name))
                    {
                        continue;
                    }
                    let publisher = string(sub.0, "Publisher");
                    let launch = launch(
                        &name,
                        &string(sub.0, "DisplayIcon"),
                        &string(sub.0, "InstallLocation"),
                        &links,
                    );
                    apps.push(App {
                        name,
                        publisher,
                        launch,
                    });
                }
            }
        }
    }
    match packaged_apps() {
        Ok(packaged) => {
            ultra_bar_native_ui::diagnostics::event(
                "search",
                "apps.loaded",
                serde_json::json!({"desktop":apps.len(),"packaged":packaged.len()}),
            );
            merge_packaged(&mut apps, packaged);
        }
        Err(error) => ultra_bar_native_ui::diagnostics::event(
            "search",
            "apps.packaged_failed",
            serde_json::json!({"error":format!("{error:#}")}),
        ),
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}
pub fn search(apps: &[App], query: &str) -> Vec<ultra_bar_plugin_ui::Row> {
    let query = query.trim().to_lowercase();
    let mut matched: Vec<_> = apps
        .iter()
        .filter(|a| {
            a.name.to_lowercase().contains(&query)
                || a.launch.as_ref().is_some_and(|target| {
                    target.starts_with("shell:AppsFolder\\")
                        && target.to_lowercase().contains(&query)
                })
        })
        .collect();
    matched.sort_by_key(|a| {
        (
            !a.name.to_lowercase().starts_with(&query),
            a.name.to_lowercase(),
        )
    });
    matched
        .into_iter()
        .take(20)
        .map(|a| ultra_bar_plugin_ui::Row {
            title: a.name.clone(),
            detail: format!(
                "已安装软件 · {}{}",
                a.publisher,
                if a.launch.is_none() {
                    " · 未找到启动入口"
                } else {
                    ""
                }
            ),
            path: a.launch.clone(),
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn packaged_apps_keep_distinct_identities_and_replace_unlaunchable_entries() {
        let packaged = |id: &str| App {
            name: "ChatGPT".into(),
            publisher: "商店应用".into(),
            launch: Some(format!("shell:AppsFolder\\{id}!App")),
        };
        let mut apps = vec![App {
            name: "ChatGPT".into(),
            publisher: String::new(),
            launch: None,
        }];
        merge_packaged(
            &mut apps,
            vec![
                packaged("OpenAI.Codex_family"),
                packaged("OpenAI.Classic_family"),
                packaged("OpenAI.Codex_family"),
            ],
        );
        let rows = search(&apps, "chat");
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|row| row.path.as_ref().unwrap().starts_with("shell:AppsFolder\\"))
        );
    }
    #[test]
    fn prefix_apps_rank_first_and_uninstall_is_never_launched() {
        let apps = vec![
            App {
                name: "My Editor".into(),
                publisher: String::new(),
                launch: None,
            },
            App {
                name: "Editor".into(),
                publisher: String::new(),
                launch: None,
            },
        ];
        let rows = search(&apps, "editor");
        assert_eq!(rows[0].title, "Editor");
        assert!(launch("x", "C:\\unins000.exe", "", &[]).is_none());
    }
}
