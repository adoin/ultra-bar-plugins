//! Programs and Features entries from all per-user/machine registry views. Never run uninstall commands.
use std::{
    collections::HashSet,
    path::PathBuf,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{Foundation::*, System::Registry::*};
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
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}
pub fn search(apps: &[App], query: &str) -> Vec<ultra_bar_plugin_ui::Row> {
    let query = query.trim().to_lowercase();
    let mut matched: Vec<_> = apps
        .iter()
        .filter(|a| a.name.to_lowercase().contains(&query))
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
