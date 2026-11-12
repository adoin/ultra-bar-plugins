//! Calendar business adapter. Every editing widget is provided by Ultra Bar.
use crate::{
    calendar,
    engine::Draft,
    models::CalendarEvent,
    region::timezone,
    ui::{self, App},
};
use anyhow::{Context, Result};
use chrono::{Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use std::ptr::null;
use ultra_bar_native_ui::editor_bridge::{
    self, Condition, EditorEvent, Field, Form, Kind, Operation, OptionItem, Values,
};
use windows_sys::Win32::{
    Foundation::HWND, Graphics::Gdi::InvalidateRect, UI::WindowsAndMessaging::*,
};
pub struct Editor {
    pub hwnd: HWND,
    pub pending: bool,
    session: u64,
    source: Option<CalendarEvent>,
}
fn schema(initial: &CalendarEvent, date: NaiveDate, session: u64, editing: bool) -> Form {
    let start = initial.start_at.with_timezone(&timezone());
    let rule = calendar::schedule_kind(initial).index();
    let condition = |field: &str, values: &[&str]| vec![Condition::one(field, values)];
    let mut fields = Vec::new();
    let mut title = Field::new("title", "标题", Kind::Text, &initial.title);
    title.constraints.required = true;
    fields.push(title);
    let mut kind = Field::new("kind", "发生规则", Kind::Select, rule);
    kind.options = [
        "具体日期与时间",
        "每个法定休息日",
        "每个法定工作日",
        "每年公历某一天",
        "每年农历某一天",
        "每天某个时刻",
    ]
    .iter()
    .enumerate()
    .map(|(i, label)| OptionItem {
        value: i.to_string(),
        label: label.to_string(),
    })
    .collect();
    fields.push(kind);
    let mut day = Field::new(
        "date",
        "日期",
        Kind::Date,
        if editing { start.date_naive() } else { date },
    );
    day.visible_when = condition("kind", &["0", "3"]);
    fields.push(day);
    let recurrence = initial.recurrence_rule.split(':').nth(1).unwrap_or("1-1");
    let mut lunar = recurrence.split('-');
    let month = lunar.next().unwrap_or("1").parse::<u8>().unwrap_or(1);
    let day = lunar.next().unwrap_or("1").parse::<u8>().unwrap_or(1);
    for (id, label, count, selected) in [
        ("lunar_month", "农历月", 12, month),
        ("lunar_day", "农历日", 30, day),
    ] {
        let mut f = Field::new(id, label, Kind::Select, selected);
        f.span = 1;
        f.visible_when = condition("kind", &["4"]);
        f.options = (1..=count)
            .map(|n| OptionItem {
                value: n.to_string(),
                label: if id == "lunar_month" {
                    calendar::MONTH_NAMES[n as usize - 1].into()
                } else {
                    calendar::DAY_NAMES[n as usize - 1].into()
                },
            })
            .collect();
        fields.push(f);
    }
    let mut leap = Field::new(
        "lunar_leap",
        "闰月",
        Kind::Toggle,
        initial.recurrence_rule.ends_with(":leap"),
    );
    leap.visible_when = condition("kind", &["4"]);
    fields.push(leap);
    let mut time = Field::new("time", "开始时间", Kind::Time, start.format("%H:%M"));
    time.span = 1;
    time.enabled_when = vec![Condition {
        field: String::new(),
        one_of: vec![],
        any: vec![
            Condition::one("all_day", &["false"]),
            Condition::one("kind", &["1", "2", "4", "5"]),
        ],
    }];
    fields.push(time);
    let mut duration = Field::new(
        "duration",
        "时长（分钟）",
        Kind::Number,
        (initial.end_at - initial.start_at).num_minutes().max(1),
    );
    duration.span = 1;
    duration.constraints.min = Some(1.);
    duration.constraints.max = Some(525600.);
    duration.constraints.step = Some(1.);
    fields.push(duration);
    let mut all = Field::new("all_day", "全天", Kind::Toggle, initial.all_day);
    all.visible_when = condition("kind", &["0", "3"]);
    fields.push(all);
    for (id, label, value) in [
        ("location", "地点", &initial.location),
        ("url", "通知链接", &initial.notification_url),
    ] {
        let mut f = Field::new(id, label, Kind::Text, value);
        f.span = 1;
        fields.push(f);
    }
    fields.push(Field::new("notes", "备注", Kind::Multiline, &initial.notes));
    let standard = [0i64, 10, 60, 1440];
    let selected: Vec<String> = initial
        .reminder_minutes
        .iter()
        .filter(|n| standard.contains(n))
        .map(ToString::to_string)
        .collect();
    let custom: Vec<String> = initial
        .reminder_minutes
        .iter()
        .filter(|n| !standard.contains(n))
        .map(ToString::to_string)
        .collect();
    let mut reminders = Field::new(
        "reminders",
        "相对提醒",
        Kind::MultiSelect,
        serde_json::to_string(&selected).unwrap(),
    );
    reminders.options = [
        ("0", "准时"),
        ("10", "10 分钟"),
        ("60", "1 小时"),
        ("1440", "前一天"),
    ]
    .iter()
    .map(|(v, l)| OptionItem {
        value: v.to_string(),
        label: l.to_string(),
    })
    .collect();
    fields.push(reminders);
    let mut enabled = Field::new(
        "custom_enabled",
        "自定义提前提醒",
        Kind::Toggle,
        !custom.is_empty(),
    );
    enabled.span = 1;
    fields.push(enabled);
    let mut extra = Field::new(
        "custom",
        "提前分钟（逗号分隔）",
        Kind::Tags,
        custom.join(","),
    );
    extra.span = 1;
    extra.enabled_when = condition("custom_enabled", &["true"]);
    fields.push(extra);
    let mut force = Field::new("force", "强制提醒", Kind::Toggle, initial.force_reminder);
    force.span = 1;
    fields.push(force);
    let mut done = Field::new("completed", "标记完成", Kind::Toggle, initial.completed);
    done.span = 1;
    fields.push(done);
    Form {
        session,
        title: if editing {
            "编辑日程"
        } else {
            "新建日程"
        }
        .into(),
        fields,
        can_delete: editing,
    }
}
fn draft(values: &Values, source: Option<&CalendarEvent>) -> Result<CalendarEvent> {
    let s = |id: &str| values.get(id).map(String::as_str).unwrap_or("");
    let b = |id: &str| s(id) == "true";
    let mut reminders: Vec<i64> = serde_json::from_str::<Vec<String>>(s("reminders"))
        .unwrap_or_default()
        .iter()
        .map(|s| s.parse::<i64>())
        .collect::<std::result::Result<_, _>>()?;
    if b("custom_enabled") {
        for part in s("custom")
            .split([',', ';', '，', ' '])
            .filter(|s| !s.is_empty())
        {
            reminders.push(part.parse().context("提前分钟需为数字")?);
        }
    }
    Draft {
        title: s("title").into(),
        kind: s("kind").parse()?,
        date: NaiveDate::parse_from_str(s("date"), "%Y-%m-%d")?,
        time: NaiveTime::parse_from_str(s("time"), "%H:%M")?,
        duration: s("duration").parse()?,
        lunar_month: s("lunar_month").parse()?,
        lunar_day: s("lunar_day").parse()?,
        lunar_leap: b("lunar_leap"),
        all_day: b("all_day"),
        location: s("location").into(),
        url: s("url").into(),
        notes: s("notes").into(),
        reminders,
        force: b("force"),
        completed: b("completed"),
    }
    .build(source)
}
pub unsafe fn open(app: &mut App, id: Option<String>) {
    unsafe {
        if let Some(editor) = &app.editor {
            if editor.pending {
                return;
            }
            if IsWindow(editor.hwnd) != 0 {
                SetForegroundWindow(editor.hwnd);
                return;
            }
        }
        let source = id.as_ref().and_then(|id| app.repo.event(id)).cloned();
        let start = timezone()
            .from_local_datetime(&app.selected.and_hms_opt(9, 0, 0).unwrap())
            .earliest()
            .unwrap()
            .with_timezone(&Utc);
        let initial = source
            .clone()
            .unwrap_or_else(|| CalendarEvent::draft(start, start + Duration::hours(1)));
        let session = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let form = schema(&initial, app.selected, session, source.is_some());
        match editor_bridge::request(
            app.owner,
            app.hwnd,
            "com.emssion.schedule-calendar",
            Operation::Open { form },
        ) {
            Ok(()) => {
                app.editor = Some(Box::new(Editor {
                    hwnd: std::ptr::null_mut(),
                    pending: true,
                    session,
                    source,
                }))
            }
            Err(error) => ui::error(app.hwnd, Err(anyhow::anyhow!(error))),
        }
    }
}
pub unsafe fn event(app: &mut App, event: EditorEvent) {
    unsafe {
        let Some(editor) = &app.editor else {
            return;
        };
        if editor.session != event.session {
            return;
        }
        if event.action == "opened" {
            if let Some(editor) = &mut app.editor {
                editor.hwnd = event.window as HWND;
                editor.pending = false;
            }
            return;
        }
        if event.action == "closed" {
            app.editor = None;
            InvalidateRect(app.hwnd, null(), 0);
            return;
        }
        let source = editor.source.clone();
        let result = if event.action == "submit" {
            draft(&event.values, source.as_ref()).and_then(|value| {
                app.selected_event = Some(value.id.clone());
                app.repo.upsert(value)
            })
        } else if event.action == "delete" {
            source
                .as_ref()
                .context("日程不存在")
                .and_then(|e| app.repo.remove(&e.id))
        } else {
            return;
        };
        let error = result
            .as_ref()
            .err()
            .map(|e| format!("{e:#}"))
            .unwrap_or_default();
        ultra_bar_native_ui::diagnostics::event(
            "calendar",
            "editor.submitted",
            serde_json::json!({"action":event.action,"ok":result.is_ok()}),
        );
        let _ = editor_bridge::request(
            app.owner,
            app.hwnd,
            "com.emssion.schedule-calendar",
            Operation::Complete {
                session: event.session,
                error,
                close: result.is_ok(),
            },
        );
    }
}
pub unsafe fn smoke(app: &mut App) -> Result<()> {
    let start = timezone()
        .from_local_datetime(&app.selected.and_hms_opt(9, 0, 0).unwrap())
        .earliest()
        .unwrap()
        .with_timezone(&Utc);
    let event = CalendarEvent::draft(start, start + Duration::hours(1));
    for kind in 0..6 {
        let mut form = schema(&event, app.selected, 1, false);
        form.fields
            .iter_mut()
            .find(|f| f.id == "title")
            .unwrap()
            .value = "验证日程".into();
        form.fields
            .iter_mut()
            .find(|f| f.id == "kind")
            .unwrap()
            .value = kind.to_string();
        form.validate_schema().map_err(anyhow::Error::msg)?;
        let values = form
            .fields
            .iter()
            .map(|f| (f.id.clone(), f.value.clone()))
            .collect();
        draft(&values, None)?;
        if kind == 0
            && let Some(dir) = std::env::var_os("ULTRA_BAR_CALENDAR_SMOKE_DIR")
        {
            std::fs::write(
                std::path::PathBuf::from(dir).join("calendar-editor-schema.json"),
                serde_json::to_vec_pretty(&form)?,
            )?;
        }
    }
    Ok(())
}
