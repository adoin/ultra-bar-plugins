use serde::{Deserialize, Serialize};
use std::path::Path;
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub ignore_dependencies: bool,
    pub ignored_directories: Vec<String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            ignore_dependencies: true,
            ignored_directories: vec![],
        }
    }
}
const DEPENDENCIES: &[&str] = &[
    "node_modules",
    ".pnpm-store",
    ".venv",
    "site-packages",
    ".git\\objects",
    ".cargo\\registry",
    ".gradle\\caches",
    ".nuget\\packages",
    ".yarn\\cache",
];
impl Settings {
    pub fn load(dir: &Path) -> Self {
        std::fs::read(dir.join("settings.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir)?;
        let pending = dir.join("settings.json.new");
        std::fs::write(&pending, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(pending, dir.join("settings.json"))?;
        Ok(())
    }
    pub fn fields(&self) -> Vec<ultra_bar_native_ui::form::Field> {
        use ultra_bar_native_ui::form::Field;
        vec![
            Field::choice(
                "忽略大型依赖目录",
                usize::from(self.ignore_dependencies),
                &["关闭", "开启"],
            ),
            Field::text(
                "额外忽略的目录（用分号分隔）",
                self.ignored_directories.join(";"),
            ),
        ]
    }
    pub fn from_values(values: &[String]) -> anyhow::Result<Self> {
        anyhow::ensure!(values.len() == 2, "设置不完整");
        let directories: Vec<_> = values[1]
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        anyhow::ensure!(
            directories.len() <= 64
                && directories
                    .iter()
                    .all(|s| s.len() <= 260
                        && !s.contains(['\"', '\r', '\n', '*', '?', '<', '>', '|'])),
            "请填写目录名称或完整路径，不要包含通配符或搜索表达式"
        );
        Ok(Self {
            ignore_dependencies: values[0] == "1",
            ignored_directories: directories,
        })
    }
    pub fn query(&self, text: &str) -> String {
        let mut query = format!("<{text}>");
        for directory in DEPENDENCIES
            .iter()
            .copied()
            .filter(|_| self.ignore_dependencies)
            .chain(self.ignored_directories.iter().map(String::as_str))
        {
            let path = directory.replace('/', "\\");
            let pattern = if Path::new(&path).is_absolute() {
                format!("{}\\", path.trim_end_matches('\\'))
            } else {
                format!("\\{}\\", path.trim_matches('\\'))
            };
            // Literal paths are quoted. Never feed a user-supplied Everything expression into exclusions.
            query.push_str(&format!(" !path:\"{pattern}\""));
        }
        query
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn excludes_whole_dependency_directories_without_generic_build_folders() {
        let q = Settings::default().query("foo | bar");
        assert!(q.starts_with("<foo | bar> !path:"));
        assert!(q.contains("\\node_modules\\"));
        assert!(!q.contains("\\src\\") && !q.contains("\\build\\"));
        assert_eq!(
            Settings {
                ignore_dependencies: false,
                ignored_directories: vec![]
            }
            .query("abc"),
            "<abc>"
        );
    }
    #[test]
    fn custom_exclusions_are_literal() {
        assert!(Settings::from_values(&["1".into(), "foo\" | everything".into()]).is_err());
        assert!(Settings::from_values(&["1".into(), "D:\\cache;临时依赖".into()]).is_ok());
    }
}
