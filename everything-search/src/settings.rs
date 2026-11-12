use serde::{Deserialize, Serialize};
use std::path::Path;
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub ignore_dependencies: bool,
    pub ignored_directories: Vec<String>,
    pub exclude_regex: String,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            ignore_dependencies: true,
            ignored_directories: vec![],
            exclude_regex: String::new(),
        }
    }
}
const DEPENDENCIES: &[&str] = &[
    "node_modules",
    "__node_modules",
    "node_modules.asar.unpacked",
    ".pnpm-store",
    ".venv",
    "site-packages",
    ".git\\objects",
    ".cargo\\registry",
    ".gradle\\caches",
    ".nuget\\packages",
    ".yarn\\cache",
];
fn directory_pattern(directory: &str) -> String {
    let normalized = directory.replace('/', "\\");
    let absolute = Path::new(&normalized).is_absolute();
    let escaped = normalized
        .trim_end_matches('\\')
        .split('\\')
        .map(regex::escape)
        .collect::<Vec<_>>()
        .join(r"[\\/]");
    format!(
        r"{}{}(?:[\\/]|$)",
        if absolute { "^" } else { r"(?:^|[\\/])" },
        escaped
    )
}
fn validate_exclusion(value: &str) -> Result<(), String> {
    if value.starts_with('/')
        && value.rsplit_once('/').is_some_and(|(body, flags)| {
            body.len() > 1 && !flags.is_empty() && flags.chars().all(|c| "dgimsuvy".contains(c))
        })
    {
        return Err("不要使用 /…/g；忽略大小写请用 (?i)".into());
    }
    if value.len() > 2048 || value.contains(['\"', '\r', '\n']) {
        return Err("正则表达式过长或包含双引号、换行".into());
    }
    regex::RegexBuilder::new(value)
        .size_limit(1024 * 1024)
        .build()
        .map(|_| ())
        .map_err(|_| "正则表达式无效".into())
}
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
                "忽略依赖目录（node_modules、.venv 等，所有层级）",
                usize::from(self.ignore_dependencies),
                &["关闭", "开启"],
            ),
            Field::text(
                "额外忽略的目录（用分号分隔）",
                self.ignored_directories.join(";"),
            ),
            Field::text("排除路径（正则表达式，留空关闭）", &self.exclude_regex)
                .validated(validate_exclusion),
        ]
    }
    pub fn from_values(values: &[String]) -> anyhow::Result<Self> {
        anyhow::ensure!((2..=3).contains(&values.len()), "设置不完整");
        let exclude_regex = values.get(2).map_or("", String::as_str).trim().to_owned();
        validate_exclusion(&exclude_regex).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            exclude_regex.len() <= 2048 && !exclude_regex.contains(['\"', '\r', '\n']),
            "正则表达式过长或包含双引号、换行"
        );
        regex::RegexBuilder::new(&exclude_regex)
            .size_limit(1024 * 1024)
            .build()
            .map_err(|e| anyhow::anyhow!("正则表达式无效：{e}"))?;
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
            exclude_regex,
        })
    }
    pub fn query(&self, text: &str) -> String {
        let mut query = format!("<{text}>");
        let patterns: Vec<_> = DEPENDENCIES
            .iter()
            .copied()
            .filter(|_| self.ignore_dependencies)
            .chain(self.ignored_directories.iter().map(String::as_str))
            .map(directory_pattern)
            .collect();
        if !patterns.is_empty() {
            query.push_str(&format!(" !path:regex:\"(?i)(?:{})\"", patterns.join("|")));
        }
        if !self.exclude_regex.is_empty() {
            query.push_str(&format!(" !path:regex:\"{}\"", self.exclude_regex));
        }
        query
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dependencies_match_whole_directory_segments_at_every_depth_including_the_directory_itself() {
        let filter =
            regex::Regex::new(&format!("(?i){}", directory_pattern("node_modules"))).unwrap();
        for path in [
            r"C:\node_modules",
            r"D:\project\nested\node_modules",
            r"D:\project\NODE_MODULES\lib\index.js",
            "/work/a/node_modules/pkg/index.js",
        ] {
            assert!(filter.is_match(path), "{path}");
        }
        for path in [
            r"D:\my_node_modules\index.js",
            r"D:\node_modules_backup\index.js",
            r"D:\node_modules.txt",
        ] {
            assert!(!filter.is_match(path), "{path}");
        }
        let absolute =
            regex::Regex::new(&format!("(?i){}", directory_pattern(r"D:\cache(1)"))).unwrap();
        assert!(absolute.is_match(r"D:\cache(1)\file.txt"));
        assert!(!absolute.is_match(r"C:\cache(1)\file.txt"));
        assert!(!absolute.is_match(r"D:\cache1\file.txt"));
    }
    #[test]
    fn regex_exclusions_are_validated_and_sent_to_the_index_before_result_limits() {
        let pattern = r"(?i)\\(?:cache|vendor)\\|\.tmp$";
        let settings = Settings::from_values(&["1".into(), String::new(), pattern.into()]).unwrap();
        assert!(
            settings
                .query("notes")
                .ends_with(&format!(" !path:regex:\"{pattern}\""))
        );
        assert!(Settings::from_values(&["1".into(), String::new(), "[".into()]).is_err());
        assert!(Settings::from_values(&["1".into(), String::new(), "x\" | *".into()]).is_err());
        let legacy: Settings = serde_json::from_str(r#"{"ignore_dependencies":false}"#).unwrap();
        assert!(legacy.exclude_regex.is_empty());
    }
    #[test]
    fn excludes_whole_dependency_directories_without_generic_build_folders() {
        let q = Settings::default().query("foo | bar");
        assert!(q.starts_with("<foo | bar> !path:"));
        assert!(q.contains(&directory_pattern("node_modules")));
        assert!(!q.contains("\\src\\") && !q.contains("\\build\\"));
        assert_eq!(
            Settings {
                ignore_dependencies: false,
                ignored_directories: vec![],
                ..Default::default()
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
    #[test]
    fn builtins_are_enabled_without_a_custom_regex_and_js_delimiters_are_rejected() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert!(settings.ignore_dependencies);
        assert!(settings.exclude_regex.is_empty());
        let query = settings.query("demo");
        for name in ["node_modules", "__node_modules", ".venv", "site-packages"] {
            assert!(query.contains(&directory_pattern(name)));
        }
        assert!(
            Settings::from_values(&[
                "1".into(),
                String::new(),
                r"/(^|\\)node_modules(\\|$)/g".into()
            ])
            .is_err()
        );
        assert!(
            Settings::from_values(&[
                "1".into(),
                String::new(),
                r"(?i)(?:^|[\\/])node_modules(?:[\\/]|$)".into()
            ])
            .is_ok()
        );
    }
}
