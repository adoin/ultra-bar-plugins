use anyhow::{Context, Result, ensure};
use chrono::{Offset, Utc};
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
    pub city: String,
    pub provider: usize,
    pub host: String,
    pub fahrenheit: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            timezone: "Asia/Shanghai".into(),
            sunday_first: false,
            lunar: false,
            display: 0,
            city: String::new(),
            provider: 0,
            host: String::new(),
            fahrenheit: false,
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
        std::fs::read(dir.join("region.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    pub fn fields(&self) -> Vec<Field> {
        vec![
            Field::text("时区（IANA）", &self.timezone),
            Field::choice(
                "每周第一天",
                usize::from(self.sunday_first),
                &["星期一", "星期日"],
            ),
            Field::choice("任务栏农历", usize::from(self.lunar), &["隐藏", "显示"]),
            Field::choice(
                "任务栏时间与天气",
                self.display,
                &["仅时间", "并排展示", "轮播展示"],
            ),
            Field::text("天气地区（城市名，或 纬度,经度）", &self.city),
            Field::choice(
                "天气服务",
                self.provider,
                &[
                    "自动：优先和风，失败时 Open-Meteo",
                    "Open-Meteo（免 API Key）",
                    "和风天气",
                ],
            ),
            Field::text("和风 API Host（专属域名）", &self.host),
            Field::secret("和风 API Key（留空保留现有密钥）"),
            Field::choice(
                "温度单位",
                usize::from(self.fahrenheit),
                &["摄氏度 °C", "华氏度 °F"],
            ),
            Field::link(
                "和风天气：创建项目 → 获取 API Host 与 API Key",
                "https://dev.qweather.com/docs/configuration/project-and-key/",
            ),
        ]
    }
    pub fn parse(values: &[String]) -> Result<Self> {
        ensure!(values.len() == 10, "设置不完整");
        let config = Self {
            timezone: values[0].trim().into(),
            sunday_first: values[1] == "1",
            lunar: values[2] == "1",
            display: values[3].parse::<usize>()?.min(2),
            city: values[4].trim().into(),
            provider: values[5].parse::<usize>()?.min(2),
            host: values[6]
                .trim()
                .trim_end_matches('/')
                .trim_start_matches("https://")
                .into(),
            fahrenheit: values[8] == "1",
        };
        ensure!(
            config.timezone.parse::<chrono_tz::Tz>().is_ok(),
            "时区无效，请使用 IANA 时区名称"
        );
        ensure!(config.city.len() <= 160, "地区名称过长");
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
fn publish(dir: &std::path::Path, config: &Config, weather: &Weather) {
    let now = Utc::now().with_timezone(&timezone());
    let lunar = crate::calendar::lunar_parts(now.date_naive())
        .map(|(m, d, leap)| crate::calendar::lunar_date_label(m, d, leap))
        .unwrap_or_default();
    let data = serde_json::json!({"version":1,"display":config.display,"lunar":if config.lunar{lunar}else{String::new()},"weather":if Utc::now().timestamp()-weather.updated<=7200{weather.summary.as_str()}else{""},"tooltip":format!("{}\n{}\n{}",weather.place,weather.detail,weather.source),"utc_offset_seconds":now.offset().fix().local_minus_utc(),"updated":Utc::now().timestamp()});
    let path = dir.join("clock.json");
    if let Ok(bytes) = serde_json::to_vec(&data) {
        let pending = dir.join("clock.json.new");
        if std::fs::write(&pending, bytes).is_ok() {
            let _ = std::fs::rename(pending, path);
        }
    }
}
pub fn start(dir: PathBuf, public: PathBuf, mut config: Config) -> Worker {
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
        let mut due = Instant::now() + Duration::from_secs(1800u64.saturating_sub(age));
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
    fn settings_validate_timezone_and_credential_destination() {
        let fields = Config::default().fields();
        let mut v: Vec<_> = fields.into_iter().map(|f| f.value).collect();
        assert!(Config::parse(&v).is_ok());
        v[6] = "https://evil.test".into();
        assert!(Config::parse(&v).is_err());
        v[6] = "abc.qweatherapi.com".into();
        assert!(Config::parse(&v).is_ok());
        v[0] = "wrong".into();
        assert!(Config::parse(&v).is_err());
    }
}
