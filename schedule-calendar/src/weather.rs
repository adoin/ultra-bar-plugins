//! Open-Meteo and QWeather adapters using Windows HTTPS.
use crate::{
    http,
    region::{Config, Weather},
};
use anyhow::{Context, Result, ensure};
fn coordinates(text: &str) -> Option<(f64, f64)> {
    let (a, b) = text.split_once(',')?;
    let (a, b) = (a.trim().parse::<f64>().ok()?, b.trim().parse::<f64>().ok()?);
    (a.is_finite() && b.is_finite() && (-90.0..=90.0).contains(&a) && (-180.0..=180.0).contains(&b))
        .then_some((a, b))
}
fn description(code: u64) -> &'static str {
    match code {
        0 => "晴",
        1 | 2 => "多云",
        3 => "阴",
        45 | 48 => "雾",
        51..=67 | 80..=82 => "雨",
        71..=77 | 85 | 86 => "雪",
        95..=99 => "雷雨",
        _ => "天气",
    }
}
fn temperature(value: f64, fahrenheit: bool) -> String {
    format!(
        "{:.0}°{}",
        if fahrenheit { value * 1.8 + 32. } else { value },
        if fahrenheit { "F" } else { "C" }
    )
}
fn open_meteo(c: &Config) -> Result<Weather> {
    let (lat, lon, place) = if let Some((lat, lon)) = coordinates(&c.city) {
        (lat, lon, c.city.clone())
    } else {
        let geo = http::get(
            "geocoding-api.open-meteo.com",
            "/v1/search",
            &[
                ("name", c.city.clone()),
                ("count", "1".into()),
                ("language", "zh".into()),
                ("format", "json".into()),
            ],
            None,
        )?;
        let city = geo["results"]
            .as_array()
            .and_then(|v| v.first())
            .context("未找到地区")?;
        (
            city["latitude"].as_f64().context("Missing latitude")?,
            city["longitude"].as_f64().context("Missing longitude")?,
            ["country", "admin1", "name"]
                .iter()
                .filter_map(|k| city[*k].as_str())
                .collect::<Vec<_>>()
                .join(" · "),
        )
    };
    let value=http::get("api.open-meteo.com","/v1/forecast",&[("latitude",lat.to_string()),("longitude",lon.to_string()),("current","temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,wind_speed_10m".into()),("daily","temperature_2m_max,temperature_2m_min".into()),("forecast_days","1".into()),("timezone",c.timezone.clone())],None)?;
    let current = &value["current"];
    let temp = current["temperature_2m"]
        .as_f64()
        .context("Missing temperature")?;
    ensure!(temp.is_finite(), "Invalid temperature");
    let desc = description(
        current["weather_code"]
            .as_u64()
            .context("Missing weather code")?,
    );
    let range = &value["daily"];
    Ok(Weather {
        place,
        summary: format!("{desc} {}", temperature(temp, c.fahrenheit)),
        detail: format!(
            "体感 {} · 湿度 {}% · 风速 {} km/h · 今日 {}–{}",
            temperature(
                current["apparent_temperature"].as_f64().unwrap_or(temp),
                c.fahrenheit
            ),
            current["relative_humidity_2m"],
            current["wind_speed_10m"],
            temperature(
                range["temperature_2m_min"][0].as_f64().unwrap_or(temp),
                c.fahrenheit
            ),
            temperature(
                range["temperature_2m_max"][0].as_f64().unwrap_or(temp),
                c.fahrenheit
            )
        ),
        source: "Open-Meteo · CC BY 4.0".into(),
        updated: chrono::Utc::now().timestamp(),
    })
}
fn qweather(c: &Config, key: &str) -> Result<Weather> {
    ensure!(
        c.host.ends_with(".qweatherapi.com")
            && c.host
                .chars()
                .all(|x| x.is_ascii_alphanumeric() || x == '.' || x == '-'),
        "请填写有效的和风 API Host"
    );
    let (lat, lon, place) = if let Some((lat, lon)) = coordinates(&c.city) {
        (lat, lon, c.city.clone())
    } else {
        let geo = http::get(
            &c.host,
            "/geo/v2/city/lookup",
            &[
                ("location", c.city.clone()),
                ("lang", "zh".into()),
                ("number", "1".into()),
            ],
            Some(key),
        )?;
        ensure!(geo["code"] == "200", "Location unavailable");
        let city = geo["location"]
            .as_array()
            .and_then(|v| v.first())
            .context("No city")?;
        (
            city["lat"]
                .as_str()
                .context("No latitude")?
                .parse::<f64>()?,
            city["lon"]
                .as_str()
                .context("No longitude")?
                .parse::<f64>()?,
            ["country", "adm1", "name"]
                .iter()
                .filter_map(|k| city[*k].as_str())
                .collect::<Vec<_>>()
                .join(" · "),
        )
    };
    ensure!(
        lat.is_finite()
            && lon.is_finite()
            && (-90.0..=90.0).contains(&lat)
            && (-180.0..=180.0).contains(&lon),
        "Invalid coordinates"
    );
    let value = http::get(
        &c.host,
        &format!("/weather/v1/current/{lat:.2}/{lon:.2}"),
        &[("lang", "zh".into())],
        Some(key),
    )?;
    let temp = value["temperature"]["value"]
        .as_f64()
        .context("No temperature")?;
    ensure!(temp.is_finite(), "Invalid temperature");
    let desc = value["condition"]["text"]
        .as_str()
        .context("No condition")?;
    Ok(Weather {
        place,
        summary: format!("{desc} {}", temperature(temp, c.fahrenheit)),
        detail: format!(
            "体感 {} · 湿度 {:.0}% · 风速 {} m/s",
            temperature(
                value["feelsLike"]["value"].as_f64().unwrap_or(temp),
                c.fahrenheit
            ),
            value["humidity"].as_f64().unwrap_or(0.) * 100.,
            value["wind"]["speed"]["value"]
        ),
        source: "QWeather 和风天气 · https://developer.qweather.com/attribution.html".into(),
        updated: chrono::Utc::now().timestamp(),
    })
}
pub fn fetch(config: &Config) -> Result<Weather> {
    if config.provider != 1 {
        let key = if config.provider == 2 {
            crate::region::key()?
        } else {
            crate::region::key().ok().flatten()
        };
        if let Some(key) = key.filter(|_| !config.host.is_empty()) {
            match qweather(config, &key) {
                Ok(weather) => return Ok(weather),
                Err(error) if config.provider == 2 => return Err(error),
                _ => {}
            }
        } else if config.provider == 2 {
            anyhow::bail!("请配置和风 API Host 和 API Key");
        }
    }
    open_meteo(config)
}
pub fn test_qweather(config: &Config, entered_key: Option<&str>) -> Result<()> {
    let saved;
    let key = if let Some(key) = entered_key.filter(|key| !key.trim().is_empty()) {
        key.trim()
    } else {
        saved = crate::region::key()?.context("请填写和风 API Key")?;
        saved.as_str()
    };
    let mut test = config.clone();
    test.provider = 2;
    if test.city.trim().is_empty() {
        test.city = "39.90,116.40".into();
    }
    qweather(&test, key).map(|_| ())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn qweather_test_never_falls_back_or_changes_provider() {
        let config = Config {
            host: "not-a-qweather-host.invalid".into(),
            provider: 0,
            ..Config::default()
        };
        let result = test_qweather(&config, Some("test-key-without-network"));
        assert!(result.is_err());
        assert_eq!(config.provider, 0);
        assert_eq!(config.host, "not-a-qweather-host.invalid");
    }
    #[test]
    fn validates_coordinates() {
        assert_eq!(coordinates("31.2,121.5"), Some((31.2, 121.5)));
        assert!(coordinates("NaN,10").is_none());
        assert!(coordinates("91,10").is_none());
        assert_eq!(temperature(0., true), "32°F");
    }
}
