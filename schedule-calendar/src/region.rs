use anyhow::{Context, Result, ensure};
use chrono::{Datelike, Offset, Utc};
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{OnceLock, RwLock, mpsc},
    time::{Duration, Instant},
};
use ultra_bar_native_ui::form::Field;
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub timezone: String,
    pub sunday_first: bool,
    pub lunar: bool,
    pub display: usize,
    #[serde(alias = "rotation_seconds")]
    pub clock_seconds: u32,
    pub weather_seconds: u32,
    pub city: String,
    pub provider: usize,
    pub host: String,
    pub fahrenheit: bool,
    pub clock_format: String,
    pub notifications_enabled: bool,
    pub notification_sound_enabled: bool,
}
pub const API_KEY_FIELD: usize = 13;
const DEFAULT_CLOCK_SECONDS: u32 = 12;
const DEFAULT_WEATHER_SECONDS: u32 = 4;
impl Default for Config {
    fn default() -> Self {
        Self {
            timezone: "Asia/Shanghai".into(),
            sunday_first: false,
            lunar: false,
            display: 0,
            clock_seconds: DEFAULT_CLOCK_SECONDS,
            weather_seconds: DEFAULT_WEATHER_SECONDS,
            city: String::new(),
            provider: 0,
            host: String::new(),
            fahrenheit: false,
            clock_format: "MM-DD HH:mm".into(),
            notifications_enabled: true,
            notification_sound_enabled: true,
        }
    }
}
static TIMEZONE: OnceLock<RwLock<chrono_tz::Tz>> = OnceLock::new();
pub fn timezone() -> chrono_tz::Tz {
    *TIMEZONE
        .get_or_init(|| RwLock::new(chrono_tz::Asia::Shanghai))
        .read()
        .unwrap()
}
pub fn set_timezone(value: &str) -> Result<()> {
    let timezone = value
        .parse::<chrono_tz::Tz>()
        .map_err(|_| anyhow::anyhow!("时区无效，例如 Asia/Shanghai、Asia/Tokyo、Europe/London"))?;
    *TIMEZONE
        .get_or_init(|| RwLock::new(chrono_tz::Asia::Shanghai))
        .write()
        .unwrap() = timezone;
    Ok(())
}
impl Config {
    pub fn load(dir: &std::path::Path) -> Self {
        let stored = std::fs::read(dir.join("region.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
        let mut config: Self = stored
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        if stored
            .as_ref()
            .is_none_or(|v| v.get("clock_format").is_none())
        {
            // Preserve the former host preference when moving the setting here.
            if let Some(root) = dir.parent().and_then(|p| p.parent())
                && let Ok(bytes) = std::fs::read(root.join("settings.json"))
                && let Ok(host) = serde_json::from_slice::<serde_json::Value>(&bytes)
                && let Some(pattern) = host.get("clock_format").and_then(|v| v.as_str())
                && !pattern.trim().is_empty()
                && pattern.chars().count() <= 128
                && !pattern.chars().any(char::is_control)
            {
                config.clock_format = pattern.into();
            }
        }
        if !(2..=3600).contains(&config.clock_seconds) {
            config.clock_seconds = DEFAULT_CLOCK_SECONDS;
        }
        if !(2..=3600).contains(&config.weather_seconds) {
            config.weather_seconds = DEFAULT_WEATHER_SECONDS;
        }
        config
    }
    pub fn fields(&self) -> Vec<Field> {
        vec![
            Field::text("任务栏时间格式", &self.clock_format),
            Field::choice(
                "任务栏时间与天气",
                self.display,
                &["仅时间", "并排展示", "轮播展示"],
            ),
            Field::text("时间停留（秒，2–3600）", self.clock_seconds.to_string()),
            Field::text("天气停留（秒，2–3600）", self.weather_seconds.to_string()),
            Field::choice("任务栏农历", usize::from(self.lunar), &["隐藏", "显示"]),
            Field::choice(
                "日程提醒",
                usize::from(self.notifications_enabled),
                &["关闭", "开启"],
            ),
            Field::choice(
                "提醒声音",
                usize::from(self.notification_sound_enabled),
                &["关闭", "开启"],
            ),
            Field::text("时区（IANA）", &self.timezone),
            Field::choice(
                "每周第一天",
                usize::from(self.sunday_first),
                &["星期一", "星期日"],
            ),
            Field::text("天气地区（城市名，或 纬度,经度）", &self.city),
            Field::choice(
                "温度单位",
                usize::from(self.fahrenheit),
                &["摄氏度 °C", "华氏度 °F"],
            ),
            Field::choice(
                "天气服务",
                self.provider,
                &[
                    "自动：优先和风，失败时 Open-Meteo",
                    "Open-Meteo（免 API Key）",
                    "和风天气",
                ],
            ),
            Field::text("和风 API Host（专属域名）", &self.host).with_action(
                "test_qweather",
                "测试当前和风 API 配置",
                "\u{e768}",
            ),
            Field::secret("和风 API Key（留空保留现有密钥）"),
            Field::link(
                "和风天气：创建项目 → 获取 API Host 与 API Key",
                "https://dev.qweather.com/docs/configuration/project-and-key/",
            ),
        ]
    }
    pub fn parse(values: &[String]) -> Result<Self> {
        ensure!(values.len() == 15, "设置不完整");
        let config = Self {
            timezone: values[7].trim().into(),
            sunday_first: values[8] == "1",
            lunar: values[4] == "1",
            display: values[1].parse::<usize>()?.min(2),
            clock_seconds: dwell_seconds(&values[2], "时间停留")?,
            weather_seconds: dwell_seconds(&values[3], "天气停留")?,
            city: values[9].trim().into(),
            provider: values[11].parse::<usize>()?.min(2),
            host: values[12]
                .trim()
                .trim_end_matches('/')
                .trim_start_matches("https://")
                .into(),
            fahrenheit: values[10] == "1",
            clock_format: values[0].trim().into(),
            notifications_enabled: values[5] == "1",
            notification_sound_enabled: values[6] == "1",
        };
        ensure!(
            config.timezone.parse::<chrono_tz::Tz>().is_ok(),
            "时区无效，请使用 IANA 时区名称"
        );
        ensure!(config.city.len() <= 160, "地区名称过长");
        ensure!(
            !config.clock_format.is_empty()
                && config.clock_format.chars().count() <= 128
                && !config.clock_format.chars().any(char::is_control),
            "时间格式不能为空或包含控制字符，最多 128 字符"
        );
        if !config.host.is_empty() {
            ensure!(
                config.host.ends_with(".qweatherapi.com")
                    && config
                        .host
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'),
                "请填写控制台中的专属 qweatherapi.com 域名"
            );
        }
        Ok(config)
    }
}
fn dwell_seconds(value: &str, label: &str) -> Result<u32> {
    value
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|seconds| (2..=3600).contains(seconds))
        .with_context(|| format!("{label}须为 2–3600 秒的整数"))
}
pub fn key() -> Result<Option<String>> {
    match keyring::Entry::new("Ultra Bar Schedule Calendar", "qweather-api-key")?.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}
pub fn save_key(value: &str) -> Result<()> {
    if !value.trim().is_empty() {
        keyring::Entry::new("Ultra Bar Schedule Calendar", "qweather-api-key")?
            .set_password(value.trim())?;
    }
    Ok(())
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Weather {
    pub place: String,
    pub summary: String,
    #[serde(default)]
    pub humidity_percent: Option<u8>,
    pub detail: String,
    pub source: String,
    pub updated: i64,
}
#[derive(Serialize, Deserialize)]
struct Cache {
    key: String,
    weather: Weather,
}
fn cache_key(config: &Config) -> String {
    format!(
        "{}|{}|{}|{}",
        config.city, config.provider, config.host, config.fahrenheit
    )
}
pub enum Request {
    Configure(Config),
    Refresh,
    Test {
        config: Config,
        key: Option<String>,
        request_id: u64,
    },
    Stop,
}
pub struct Worker {
    pub send: mpsc::SyncSender<Request>,
    pub receive: mpsc::Receiver<(Weather, String)>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.send.try_send(Request::Stop);
    }
}
fn clock_tooltip(date: chrono::NaiveDate, lunar: &str, weather: &Weather) -> String {
    let weekday =
        ["一", "二", "三", "四", "五", "六", "日"][date.weekday().num_days_from_monday() as usize];
    let mut lines = vec![
        format!("{} 星期{} {}", date.format("%Y-%m-%d"), weekday, lunar)
            .trim_end()
            .to_owned(),
    ];
    lines.extend(
        [weather.place.trim(), weather.detail.trim()]
            .into_iter()
            .filter(|line| !line.is_empty())
            .map(str::to_owned),
    );
    lines.join("\n")
}
fn publish(dir: &std::path::Path, config: &Config, weather: &Weather) {
    let now = Utc::now().with_timezone(&timezone());
    let lunar = crate::calendar::lunar_parts(now.date_naive())
        .map(|(m, d, leap)| crate::calendar::lunar_date_label(m, d, leap))
        .unwrap_or_default();
    let tooltip = clock_tooltip(now.date_naive(), &lunar, weather);
    let fresh = now.timestamp() - weather.updated <= 7200;
    let data = serde_json::json!({
        "version": 1,
        "clock_format": config.clock_format,
        "display": config.display,
        "rotation_seconds": config.clock_seconds,
        "clock_seconds": config.clock_seconds,
        "weather_seconds": config.weather_seconds,
        "lunar": if config.lunar { lunar } else { String::new() },
        "weather": if fresh { weather.summary.as_str() } else { "" },
        "humidity_percent": if fresh { weather.humidity_percent } else { None },
        "tooltip": tooltip,
        "utc_offset_seconds": now.offset().fix().local_minus_utc(),
        "updated": now.timestamp(),
    });
    let path = dir.join("clock.json");
    if let Ok(bytes) = serde_json::to_vec(&data) {
        let pending = dir.join("clock.json.new");
        if std::fs::write(&pending, bytes).is_ok() {
            let _ = std::fs::rename(pending, path);
        }
    }
}
pub fn start(dir: PathBuf, public: PathBuf, mut config: Config, owner: usize) -> Worker {
    let (send, commands) = mpsc::sync_channel(4);
    let (output, receive) = mpsc::sync_channel(4);
    std::thread::spawn(move || {
        let mut weather: Weather = std::fs::read(dir.join("weather-cache.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Cache>(&b).ok())
            .filter(|cache| !config.city.is_empty() && cache.key == cache_key(&config))
            .map(|cache| cache.weather)
            .unwrap_or_default();
        let age = Utc::now()
            .timestamp()
            .saturating_sub(weather.updated)
            .max(0) as u64;
        let wait = if !config.city.is_empty() && weather.humidity_percent.is_none() {
            0
        } else {
            1800u64.saturating_sub(age)
        };
        let mut due = Instant::now() + Duration::from_secs(wait);
        let _ = output.try_send((weather.clone(), String::new()));
        let mut publish_due = Instant::now();
        let _ = std::fs::create_dir_all(&public);
        loop {
            match commands.recv_timeout(
                due.min(publish_due)
                    .saturating_duration_since(Instant::now()),
            ) {
                Ok(Request::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(Request::Configure(next)) => {
                    let changed = config.city != next.city
                        || config.provider != next.provider
                        || config.host != next.host
                        || config.fahrenheit != next.fahrenheit
                        || config.timezone != next.timezone;
                    config = next;
                    if changed {
                        due = Instant::now();
                        weather = Weather::default();
                    }
                    publish_due = Instant::now();
                }
                Ok(Request::Refresh) => {
                    due = Instant::now();
                }
                Ok(Request::Test {
                    config,
                    key,
                    request_id,
                }) => {
                    let outcome = crate::weather::test_qweather(&config, key.as_deref());
                    let message = match &outcome {
                        Ok(_) => "和风 API 连接成功".into(),
                        Err(error) => format!("和风 API 测试失败：{error:#}"),
                    };
                    let _ = ultra_bar_native_ui::settings_bridge::publish_action_result(
                        &public,
                        owner as isize,
                        request_id,
                        outcome.is_ok(),
                        message,
                    );
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if Instant::now() >= due {
                let mut error = String::new();
                if !config.city.is_empty() {
                    match crate::weather::fetch(&config) {
                        Ok(next) => {
                            weather = next;
                            let _ = std::fs::write(
                                dir.join("weather-cache.json"),
                                serde_json::to_vec(&Cache {
                                    key: cache_key(&config),
                                    weather: weather.clone(),
                                })
                                .unwrap_or_default(),
                            );
                        }
                        Err(cause) => {
                            error = format!("天气更新失败：{cause}");
                        }
                    }
                }
                let retry = if error.is_empty() { 1800 } else { 300 };
                due = Instant::now() + Duration::from_secs(retry);
                let _ = output.try_send((weather.clone(), error));
                publish_due = Instant::now();
            }
            if Instant::now() >= publish_due {
                publish(&public, &config, &weather);
                publish_due = Instant::now() + Duration::from_secs(60);
            }
        }
    });
    Worker { send, receive }
}
pub fn save(dir: &std::path::Path, config: &Config) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let pending = dir.join("region.json.new");
    std::fs::write(&pending, serde_json::to_vec_pretty(config)?)?;
    std::fs::rename(pending, dir.join("region.json")).context("无法保存地区设置")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotation_setting_defaults_for_existing_profiles_and_checks_bounds() {
        let config: Config = serde_json::from_str(r#"{"display":2}"#).unwrap();
        assert_eq!((config.clock_seconds, config.weather_seconds), (12, 4));
        let migrated: Config = serde_json::from_str(r#"{"rotation_seconds":20}"#).unwrap();
        assert_eq!((migrated.clock_seconds, migrated.weather_seconds), (20, 4));
        let mut values: Vec<_> = config.fields().into_iter().map(|f| f.value).collect();
        for bad in ["", "0", "1", "3601", "6.5", "no"] {
            values[2] = bad.into();
            assert!(Config::parse(&values).is_err());
        }
        for good in [2, 15, 3600] {
            values[2] = good.to_string();
            let parsed = Config::parse(&values).unwrap();
            assert_eq!(parsed.clock_seconds, good);
            assert_eq!(parsed.weather_seconds, 4);
        }
        values[3] = "1".into();
        assert!(Config::parse(&values).is_err());
        values[3] = "7".into();
        assert_eq!(Config::parse(&values).unwrap().weather_seconds, 7);
        assert!(matches!(
            config.fields()[API_KEY_FIELD].kind,
            ultra_bar_native_ui::form::Kind::Secret
        ));
    }
    #[test]
    fn reordered_settings_preserve_values_and_keep_the_secret_separate() {
        let config = Config {
            clock_format: "MM-DD HH:mm ddd".into(),
            timezone: "Asia/Tokyo".into(),
            sunday_first: true,
            lunar: true,
            display: 2,
            clock_seconds: 18,
            weather_seconds: 5,
            city: "Tokyo".into(),
            fahrenheit: true,
            provider: 2,
            host: "test.qweatherapi.com".into(),
            notifications_enabled: true,
            notification_sound_enabled: false,
        };
        let fields = config.fields();
        assert_eq!(fields[0].label, "任务栏时间格式");
        assert!(matches!(
            fields[API_KEY_FIELD].kind,
            ultra_bar_native_ui::form::Kind::Secret
        ));
        let mut values: Vec<_> = fields.into_iter().map(|field| field.value).collect();
        values[API_KEY_FIELD] = "test-secret-placeholder".into();
        let parsed = Config::parse(&values).unwrap();
        assert_eq!(
            serde_json::to_value(parsed).unwrap(),
            serde_json::to_value(config).unwrap()
        );
    }
    #[test]
    fn settings_validate_timezone_and_credential_destination() {
        let fields = Config::default().fields();
        let mut v: Vec<_> = fields.into_iter().map(|f| f.value).collect();
        assert!(Config::parse(&v).is_ok());
        v[12] = "https://evil.test".into();
        assert!(Config::parse(&v).is_err());
        v[12] = "abc.qweatherapi.com".into();
        assert!(Config::parse(&v).is_ok());
        v[7] = "wrong".into();
        assert!(Config::parse(&v).is_err());
    }
}
