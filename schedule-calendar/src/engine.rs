use crate::region::timezone;
use crate::{
    calendar::{self, ScheduleKind},
    models::{CalendarEvent, Holiday},
};
use anyhow::{Context, Result, bail, ensure};
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Pending {
    pub key: String,
    pub event: CalendarEvent,
    pub occurrence: DateTime<Utc>,
    pub offset: i64,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Data {
    pub events: Vec<CalendarEvent>,
    pub holidays: Vec<Holiday>,
    pub fired: Vec<String>,
    pub pending: Vec<Pending>,
}
pub struct Repository {
    pub data: Data,
    pub path: PathBuf,
    pub holidays: HashMap<String, Holiday>,
}
impl Repository {
    pub fn open(directory: &Path) -> Result<Self> {
        let path = directory.join("calendar.json");
        let data: Data = if path.exists() {
            serde_json::from_slice(&std::fs::read(&path)?)?
        } else {
            Data::default()
        };
        ensure!(data.events.len() <= 10000, "日程数量过多");
        for event in &data.events {
            validate_event(event)?;
        }
        let mut repo = Self {
            data,
            path,
            holidays: HashMap::new(),
        };
        for source in [
            include_str!("../assets/2025.json"),
            include_str!("../assets/2026.json"),
            include_str!("../assets/2027.json"),
        ] {
            for day in holiday_json(source.trim_start_matches('\u{feff}'))? {
                repo.holidays.insert(day.date.clone(), day);
            }
        }
        repo.refresh_holidays();
        Ok(repo)
    }
    fn refresh_holidays(&mut self) {
        for day in &self.data.holidays {
            self.holidays.insert(day.date.clone(), day.clone());
        }
    }
    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(self.path.parent().context("invalid data directory")?)?;
        let pending = self.path.with_extension("json.new");
        std::fs::write(&pending, serde_json::to_vec_pretty(&self.data)?)?;
        std::fs::rename(pending, &self.path).context("无法保存日程")
    }
    pub fn upsert(&mut self, event: CalendarEvent) -> Result<()> {
        let old = self.data.clone();
        if let Some(previous) = self.data.events.iter_mut().find(|item| item.id == event.id) {
            *previous = event;
        } else {
            self.data.events.push(event);
        }
        if let Err(error) = self.save() {
            self.data = old;
            return Err(error);
        }
        Ok(())
    }
    pub fn remove(&mut self, id: &str) -> Result<()> {
        let mut event = self.event(id).context("日程不存在")?.clone();
        event.deleted = true;
        event.updated_at = Utc::now();
        self.upsert(event)
    }
    pub fn complete(&mut self, id: &str) -> Result<()> {
        let mut event = self.event(id).context("日程不存在")?.clone();
        event.completed = !event.completed;
        event.updated_at = Utc::now();
        self.upsert(event)
    }
    pub fn event(&self, id: &str) -> Option<&CalendarEvent> {
        self.data
            .events
            .iter()
            .find(|event| event.id == id && !event.deleted)
    }
    pub fn occurrences(&self, date: NaiveDate) -> Vec<&CalendarEvent> {
        let holiday = self.holidays.get(&date.to_string());
        let mut result: Vec<_> = self
            .data
            .events
            .iter()
            .filter(|event| calendar::event_occurs_on(event, date, holiday))
            .collect();
        result
            .sort_by_key(|event| calendar::occurrence_start(event, date).unwrap_or(event.start_at));
        result
    }
    pub fn scan_reminders(&mut self, from: DateTime<Utc>, now: DateTime<Utc>) -> Result<bool> {
        let old = self.data.clone();
        let mut added = false;
        for event in &self.data.events {
            if event.deleted || event.completed {
                continue;
            }
            for offset in &event.reminder_minutes {
                let occurrence_date = (now + Duration::minutes(*offset))
                    .with_timezone(&timezone())
                    .date_naive();
                if !calendar::event_occurs_on(
                    event,
                    occurrence_date,
                    self.holidays.get(&occurrence_date.to_string()),
                ) {
                    continue;
                }
                let occurrence = calendar::occurrence_start(event, occurrence_date)?;
                let due = occurrence - Duration::minutes(*offset);
                let key = format!("{}:{}:{}", event.id, occurrence.timestamp(), offset);
                if due > from && due <= now && !self.data.fired.contains(&key) {
                    self.data.fired.push(key.clone());
                    self.data.pending.push(Pending {
                        key,
                        event: event.clone(),
                        occurrence,
                        offset: *offset,
                    });
                    added = true;
                }
            }
        }
        if self.data.fired.len() > 5000 {
            self.data.fired.drain(..self.data.fired.len() - 5000);
        }
        if added && let Err(error) = self.save() {
            self.data = old;
            return Err(error);
        }
        Ok(added)
    }
    pub fn acknowledge(&mut self, key: &str) -> Result<()> {
        let old = self.data.clone();
        self.data.pending.retain(|pending| pending.key != key);
        if let Err(error) = self.save() {
            self.data = old;
            return Err(error);
        }
        Ok(())
    }
    pub fn import(&mut self, bytes: &[u8]) -> Result<usize> {
        let value: serde_json::Value = serde_json::from_slice(bytes)?;
        let incoming: Vec<CalendarEvent> = serde_json::from_value(if value.is_array() {
            value
        } else {
            value.get("events").context("文件没有 events 数组")?.clone()
        })?;
        ensure!(incoming.len() <= 10000, "日程数量过多");
        for event in &incoming {
            validate_event(event)?;
        }
        let old = self.data.clone();
        let mut count = 0;
        for event in incoming {
            if !self
                .data
                .events
                .iter()
                .any(|previous| previous.id == event.id)
            {
                self.data.events.push(event);
                count += 1;
            }
        }
        if let Err(error) = self.save() {
            self.data = old;
            return Err(error);
        }
        Ok(count)
    }
    pub fn import_holidays(&mut self, source: &str) -> Result<usize> {
        let days = holiday_json(source.trim_start_matches('\u{feff}'))?;
        let count = days.len();
        let old = self.data.clone();
        for day in days {
            if let Some(previous) = self
                .data
                .holidays
                .iter_mut()
                .find(|previous| previous.date == day.date)
            {
                *previous = day;
            } else {
                self.data.holidays.push(day);
            }
        }
        if let Err(error) = self.save() {
            self.data = old;
            return Err(error);
        }
        self.refresh_holidays();
        Ok(count)
    }
}

fn validate_event(event: &CalendarEvent) -> Result<()> {
    ensure!(
        !event.id.is_empty() && !event.title.trim().is_empty() && event.end_at > event.start_at,
        "日程内容无效"
    );
    ensure!(
        event
            .reminder_minutes
            .iter()
            .all(|minutes| (0..=525600).contains(minutes)),
        "提醒分钟数无效"
    );
    ensure!(
        event.notification_url.is_empty()
            || event.notification_url.starts_with("https://")
            || event.notification_url.starts_with("http://"),
        "通知链接需以 http:// 或 https:// 开头"
    );
    Ok(())
}

pub fn holiday_json(source: &str) -> Result<Vec<Holiday>> {
    #[derive(Deserialize)]
    struct Day {
        date: String,
        name: String,
        #[serde(rename = "isOffDay")]
        is_off_day: bool,
    }
    #[derive(Deserialize)]
    struct File {
        days: Vec<Day>,
    }
    let file: File = serde_json::from_str(source)?;
    file.days
        .into_iter()
        .map(|day| {
            NaiveDate::parse_from_str(&day.date, "%Y-%m-%d")?;
            Ok(Holiday {
                date: day.date,
                name: day.name,
                is_off_day: day.is_off_day,
                source_url: "https://github.com/NateScarlet/holiday-cn".into(),
            })
        })
        .collect()
}

pub struct Draft {
    pub title: String,
    pub kind: i32,
    pub date: NaiveDate,
    pub time: NaiveTime,
    pub duration: i64,
    pub lunar_month: u8,
    pub lunar_day: u8,
    pub lunar_leap: bool,
    pub all_day: bool,
    pub location: String,
    pub url: String,
    pub notes: String,
    pub reminders: Vec<i64>,
    pub force: bool,
    pub completed: bool,
}
impl Draft {
    pub fn build(self, previous: Option<&CalendarEvent>) -> Result<CalendarEvent> {
        ensure!(!self.title.trim().is_empty(), "标题不能为空");
        ensure!(
            (1..=525600).contains(&self.duration),
            "时长必须在 1 分钟到 1 年之间"
        );
        ensure!(
            (1..=12).contains(&self.lunar_month) && (1..=30).contains(&self.lunar_day),
            "农历日期无效"
        );
        ensure!(
            self.reminders
                .iter()
                .all(|minute| (0..=525600).contains(minute)),
            "提醒需在 0 分钟到 1 年之间"
        );
        let kind = ScheduleKind::from_index(self.kind);
        let all_day =
            self.all_day && matches!(kind, ScheduleKind::Specific | ScheduleKind::YearlySolar);
        let time = if all_day {
            NaiveTime::from_hms_opt(0, 0, 0).unwrap()
        } else {
            self.time
        };
        let start = timezone()
            .from_local_datetime(&self.date.and_time(time))
            .single()
            .context("日期时间无效")?
            .with_timezone(&Utc);
        let mut event = previous.cloned().unwrap_or_else(|| {
            CalendarEvent::draft(start, start + Duration::minutes(self.duration))
        });
        event.title = self.title.trim().into();
        event.notes = self.notes;
        event.location = self.location;
        event.notification_url = self.url.trim().into();
        if !event.notification_url.is_empty()
            && !event.notification_url.starts_with("https://")
            && !event.notification_url.starts_with("http://")
        {
            bail!("通知链接需以 http:// 或 https:// 开头");
        }
        event.start_at = start;
        event.end_at = start + Duration::minutes(self.duration);
        event.all_day = all_day;
        event.recurrence_rule = match kind {
            ScheduleKind::Specific => String::new(),
            ScheduleKind::LegalRestDay => "LEGAL_REST_DAY".into(),
            ScheduleKind::LegalWorkday => "LEGAL_WORKDAY".into(),
            ScheduleKind::YearlySolar => format!(
                "YEARLY_SOLAR:{:02}-{:02}",
                self.date.month(),
                self.date.day()
            ),
            ScheduleKind::YearlyLunar => format!(
                "YEARLY_LUNAR:{:02}-{:02}:{}",
                self.lunar_month,
                self.lunar_day,
                if self.lunar_leap { "leap" } else { "regular" }
            ),
            ScheduleKind::Daily => "DAILY".into(),
        };
        event.reminder_minutes = self.reminders;
        event.reminder_minutes.sort_unstable();
        event.reminder_minutes.dedup();
        event.force_reminder = self.force;
        event.completed = self.completed;
        event.updated_at = Utc::now();
        Ok(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::path::PathBuf::from("D:/tmp/ultra-bar-calendar-tests")
                .join(format!("{}-{nonce}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0.join("calendar.json"));
            let _ = std::fs::remove_file(self.0.join("calendar.json.new"));
            let _ = std::fs::remove_dir(&self.0);
        }
    }
    #[test]
    fn reminders_deduplicate_survive_restart_and_acknowledge_without_completing_event() {
        let fixture = Fixture::new();
        let mut repo = Repository::open(&fixture.0).unwrap();
        let now = timezone()
            .with_ymd_and_hms(2026, 11, 10, 9, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let mut event = CalendarEvent::draft(now, now + Duration::hours(1));
        event.title = "提醒".into();
        event.reminder_minutes = vec![0];
        event.force_reminder = true;
        repo.upsert(event.clone()).unwrap();
        assert!(
            repo.scan_reminders(now - Duration::seconds(1), now)
                .unwrap()
        );
        assert!(
            !repo
                .scan_reminders(now - Duration::seconds(1), now)
                .unwrap()
        );
        let mut restored = Repository::open(&fixture.0).unwrap();
        assert_eq!(restored.data.pending.len(), 1);
        let key = restored.data.pending[0].key.clone();
        restored.acknowledge(&key).unwrap();
        assert!(!restored.event(&event.id).unwrap().completed);
        restored.complete(&event.id).unwrap();
        assert!(
            Repository::open(&fixture.0)
                .unwrap()
                .event(&event.id)
                .unwrap()
                .completed
        );
        restored.remove(&event.id).unwrap();
        assert!(
            Repository::open(&fixture.0)
                .unwrap()
                .event(&event.id)
                .is_none()
        );
    }
    #[test]
    fn invalid_import_does_not_partially_mutate_existing_data() {
        let fixture = Fixture::new();
        let mut repo = Repository::open(&fixture.0).unwrap();
        let now = Utc::now();
        let mut valid = CalendarEvent::draft(now, now + Duration::hours(1));
        valid.title = "有效日程".into();
        let mut invalid = valid.clone();
        invalid.id = "other".into();
        invalid.reminder_minutes = vec![-10];
        assert!(
            repo.import(&serde_json::to_vec(&vec![valid.clone(), invalid]).unwrap())
                .is_err()
        );
        assert!(repo.data.events.is_empty());
        assert_eq!(
            repo.import(&serde_json::to_vec(&vec![valid.clone()]).unwrap())
                .unwrap(),
            1
        );
        assert_eq!(
            repo.import(&serde_json::to_vec(&vec![valid]).unwrap())
                .unwrap(),
            0
        );
    }
    #[test]
    fn built_in_holidays_include_real_makeup_days() {
        let days = holiday_json(include_str!("../assets/2026.json").trim_start_matches('\u{feff}'))
            .unwrap();
        assert!(days.iter().any(|day| !day.is_off_day));
        assert!(
            days.iter()
                .any(|day| day.date == "2026-02-17" && day.is_off_day)
        );
    }
    #[test]
    fn six_rule_drafts_round_trip_without_losing_reminder_dimensions() {
        for kind in 0..6 {
            let event = Draft {
                title: "日程".into(),
                kind,
                date: NaiveDate::from_ymd_opt(2026, 2, 17).unwrap(),
                time: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                duration: 60,
                lunar_month: 1,
                lunar_day: 1,
                lunar_leap: false,
                all_day: false,
                location: "会议室".into(),
                url: "https://example.com".into(),
                notes: "备注".into(),
                reminders: vec![1440, 10, 10, 0],
                force: true,
                completed: false,
            }
            .build(None)
            .unwrap();
            assert_eq!(calendar::schedule_kind(&event).index(), kind);
            assert_eq!(event.reminder_minutes, [0, 10, 1440]);
            assert!(event.force_reminder);
        }
    }
}
