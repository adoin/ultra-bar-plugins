use crate::region::timezone;
use crate::{Configuration, calendar, engine::Repository, models::CalendarEvent};
use anyhow::{Context, Result};
use chrono::{Datelike, Duration, NaiveDate, TimeZone, Utc};
use std::{
    mem::{size_of, zeroed},
    path::PathBuf,
    ptr::{null, null_mut},
};
use ultra_bar_native_ui::text::draw_text as DrawTextW;
use ultra_bar_native_ui::{Surface, Tip, Tooltips, wide};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{DataExchange::COPYDATASTRUCT, LibraryLoader::*, Threading::*},
    UI::{Controls::*, HiDpi::*, Input::KeyboardAndMouse::IsWindowEnabled, WindowsAndMessaging::*},
};

#[derive(Clone)]
enum Action {
    Settings,
    Weather,
    RefreshWeather,
    Hide,
    Previous,
    Next,
    View(usize),
    Day(NaiveDate),
    Event(String),
    New,
    Edit,
    Complete,
    Delete,
    Data,
}
struct Hit {
    rect: RECT,
    action: Action,
    tip: String,
}
pub struct App {
    popover: ultra_bar_native_ui::popover::Popover,
    pub region: crate::region::Config,
    region_dir: PathBuf,
    settings_dir: PathBuf,
    pending_settings: Option<ultra_bar_native_ui::settings_bridge::SaveRequest>,
    pending_editor: std::collections::VecDeque<ultra_bar_native_ui::editor_bridge::EditorEvent>,
    pending_path: Option<(u64, i32)>,
    weather_worker: Option<crate::region::Worker>,
    weather: crate::region::Weather,
    weather_error: String,
    weather_view: bool,
    pub hwnd: HWND,
    pub owner: HWND,
    pub parent: HANDLE,
    pub font: HFONT,
    icon_font: HFONT,
    pub dpi: i32,
    pub repo: Repository,
    pub selected: NaiveDate,
    pub month: NaiveDate,
    pub view: usize,
    pub selected_event: Option<String>,
    pub editor: Option<Box<crate::editor::Editor>>,
    pub reminder: Option<Box<crate::reminder::Reminder>>,
    config: Configuration,
    root: PathBuf,
    watcher: Option<notify::RecommendedWatcher>,
    hits: Vec<Hit>,
    tips: Option<Tooltips>,
    glass: bool,
    scroll: usize,
    last_check: chrono::DateTime<Utc>,
    last_minute: i64,
    pub smoke: bool,
}
fn apply_region(state: &mut App, values: &[String]) -> Result<()> {
    let config = crate::region::Config::parse(values)?;
    crate::region::save_key(&values[7])?;
    crate::region::save(&state.region_dir, &config)?;
    crate::region::set_timezone(&config.timezone)?;
    state.region = config;
    if let Some(worker) = &state.weather_worker {
        let _ = worker
            .send
            .try_send(crate::region::Request::Configure(state.region.clone()));
        if !values[7].trim().is_empty() {
            let _ = worker.send.try_send(crate::region::Request::Refresh);
        }
    }
    Ok(())
}
fn publish_settings(state: &App, error: String, ack: Option<u64>) {
    let _ = ultra_bar_native_ui::settings_bridge::publish_ack(
        &state.settings_dir,
        state.owner as isize,
        state.region.fields(),
        error,
        ack,
    );
}
unsafe fn region_settings(state: &mut App) {
    unsafe {
        let result = ultra_bar_native_ui::editor_bridge::request(
            state.owner,
            state.hwnd,
            "com.emssion.schedule-calendar",
            ultra_bar_native_ui::editor_bridge::Operation::Settings,
        );
        error(state.hwnd, result.map_err(anyhow::Error::msg));
    }
}
pub fn today() -> NaiveDate {
    Utc::now().with_timezone(&timezone()).date_naive()
}

unsafe fn refresh_editor_frame(root: HWND) {
    unsafe {
        let editor = GetLastActivePopup(root);
        if editor.is_null() || editor == root {
            return;
        }
        let mut root_pid = 0;
        let mut editor_pid = 0;
        GetWindowThreadProcessId(root, &mut root_pid);
        GetWindowThreadProcessId(editor, &mut editor_pid);
        let mut class = [0u16; 128];
        let length = GetClassNameW(editor, class.as_mut_ptr(), class.len() as _);
        if root_pid != editor_pid
            || String::from_utf16_lossy(&class[..length.max(0) as usize])
                != "UltraBar.Calendar.Editor"
            || GetWindowLongPtrW(editor, GWL_STYLE) & WS_CAPTION as isize == WS_CAPTION as isize
        {
            return;
        }
        let style = GetWindowLongPtrW(editor, GWL_EXSTYLE);
        if style & WS_EX_DLGMODALFRAME as isize != 0 {
            SetWindowLongPtrW(
                editor,
                GWL_STYLE,
                GetWindowLongPtrW(editor, GWL_STYLE) & !((WS_BORDER | WS_DLGFRAME) as isize),
            );
            SetWindowLongPtrW(
                editor,
                GWL_EXSTYLE,
                (style & !(WS_EX_DLGMODALFRAME as isize)) | WS_EX_TOOLWINDOW as isize,
            );
            let border = 0xfffffffeu32;
            windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute(
                editor,
                35,
                (&border as *const u32).cast(),
                size_of::<u32>() as _,
            );
            SetWindowPos(
                editor,
                null_mut(),
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}
pub unsafe fn run(
    root: PathBuf,
    owner: usize,
    parent: u32,
    mut show: bool,
    smoke: bool,
) -> Result<()> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let class = wide(&format!(
            "UltraBar.ScheduleCalendar.{parent}{}",
            if smoke { ".Smoke" } else { "" }
        ));
        let mut previous = FindWindowW(class.as_ptr(), null());
        if !previous.is_null() && !smoke {
            refresh_editor_frame(previous);
            let mut pid = 0;
            let thread = GetWindowThreadProcessId(previous, &mut pid);
            let process = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                pid,
            );
            if !process.is_null() {
                let mut buffer = [0u16; 32768];
                let mut length = buffer.len() as u32;
                if QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length) != 0 {
                    let old_path =
                        PathBuf::from(String::from_utf16_lossy(&buffer[..length as usize]));
                    let new_path = std::env::current_exe()?;
                    if old_path.canonicalize().ok() != new_path.canonicalize().ok()
                        && old_path.file_name() == new_path.file_name()
                        && IsWindowEnabled(previous) != 0
                    {
                        show |= IsWindowVisible(previous) != 0;
                        if PostThreadMessageW(thread, WM_QUIT, 0, 0) != 0
                            && WaitForSingleObject(process, 2000) == WAIT_OBJECT_0
                        {
                            previous = null_mut();
                        }
                    }
                }
                CloseHandle(process);
            }
        }
        if !previous.is_null() {
            if show {
                if IsWindowEnabled(previous) == 0 {
                    let editor = GetLastActivePopup(previous);
                    if !editor.is_null() {
                        SetForegroundWindow(editor);
                    }
                } else {
                    PostMessageW(previous, WM_APP + 1, 0, 0);
                }
            }
            return Ok(());
        }
        let mutex = CreateMutexW(
            null(),
            0,
            wide(&format!(
                "Local\\UltraBar.ScheduleCalendar.{parent}{}",
                if smoke { ".Smoke" } else { "" }
            ))
            .as_ptr(),
        );
        anyhow::ensure!(!mutex.is_null(), "无法建立日历进程锁");
        struct MutexGuard(HANDLE);
        impl Drop for MutexGuard {
            fn drop(&mut self) {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
        let _guard = MutexGuard(mutex);
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if show {
                for _ in 0..20 {
                    let existing = FindWindowW(class.as_ptr(), null());
                    if !existing.is_null() {
                        PostMessageW(existing, WM_APP + 1, 0, 0);
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            return Ok(());
        }
        struct DataGuard(HANDLE);
        impl Drop for DataGuard {
            fn drop(&mut self) {
                unsafe {
                    ReleaseMutex(self.0);
                    CloseHandle(self.0);
                }
            }
        }
        let _data_guard = if !smoke {
            let mutex = CreateMutexW(
                null(),
                0,
                wide("Local\\UltraBar.ScheduleCalendar.Data").as_ptr(),
            );
            anyhow::ensure!(!mutex.is_null(), "无法锁定日历数据");
            let result = WaitForSingleObject(mutex, 5000);
            if result != WAIT_OBJECT_0 && result != WAIT_ABANDONED {
                CloseHandle(mutex);
                anyhow::bail!("日历数据正在被其他实例使用");
            }
            Some(DataGuard(mutex))
        } else {
            None
        };
        let module = GetModuleHandleW(null());
        RegisterClassW(&WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: module,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            ..zeroed()
        });
        InitCommonControlsEx(&INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as _,
            dwICC: ICC_DATE_CLASSES | ICC_STANDARD_CLASSES,
        });
        let directory = if smoke {
            std::env::var_os("ULTRA_BAR_CALENDAR_SMOKE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("D:/tmp/ultra-bar-calendar-smoke"))
                .join("data")
        } else {
            PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA unavailable")?)
                .join("UltraBar/plugins/schedule-calendar")
        };
        let config = crate::configuration(&root)?;
        let mut region = crate::region::Config::load(&directory);
        if crate::region::set_timezone(&region.timezone).is_err() {
            region.timezone = "Asia/Shanghai".into();
            crate::region::set_timezone(&region.timezone)?;
        }
        let date = today();
        let parent_handle = if parent == 0 {
            null_mut()
        } else {
            OpenProcess(PROCESS_SYNCHRONIZE, 0, parent)
        };
        anyhow::ensure!(parent == 0 || !parent_handle.is_null(), "任务栏进程已退出");
        let mut state = Box::new(App {
            popover: Default::default(),
            region,
            region_dir: directory.clone(),
            settings_dir: std::env::var_os("ULTRA_BAR_CLOCK_STATE")
                .map(PathBuf::from)
                .unwrap_or_else(|| directory.clone()),
            pending_settings: None,
            pending_editor: std::collections::VecDeque::new(),
            pending_path: None,
            weather_worker: None,
            weather: Default::default(),
            weather_error: String::new(),
            weather_view: false,
            hwnd: null_mut(),
            owner: owner as _,
            parent: parent_handle,
            font: null_mut(),
            icon_font: null_mut(),
            dpi: 96,
            repo: Repository::open(&directory)?,
            selected: date,
            month: date.with_day(1).unwrap(),
            view: 1,
            selected_event: None,
            editor: None,
            reminder: None,
            config,
            root: root.clone(),
            watcher: None,
            hits: vec![],
            tips: None,
            glass: false,
            scroll: 0,
            last_check: Utc::now() - Duration::seconds(60),
            last_minute: 0,
            smoke,
        });
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_LAYERED,
            class.as_ptr(),
            wide(&state.config.title).as_ptr(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            owner as _,
            null_mut(),
            module,
            (&mut *state as *mut App).cast(),
        );
        anyhow::ensure!(!hwnd.is_null(), "无法创建日历窗口");
        state.hwnd = hwnd;
        publish_settings(&state, String::new(), None);
        if !smoke {
            let public = std::env::var_os("ULTRA_BAR_CLOCK_STATE")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    directory
                        .parent()
                        .unwrap()
                        .join("com.emssion.schedule-calendar")
                });
            state.weather_worker = Some(crate::region::start(
                directory.clone(),
                public,
                state.region.clone(),
            ));
        }
        state.dpi = if owner != 0 {
            GetDpiForWindow(owner as _) as i32
        } else {
            use notify::Watcher;
            let window = hwnd as usize;
            if let Ok(mut watcher) =
                notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                    if event.is_ok_and(|event| !matches!(event.kind, notify::EventKind::Access(_)))
                    {
                        PostMessageW(window as _, WM_APP + 2, 0, 0);
                    }
                })
            {
                let _ = watcher.watch(&root.join("main.rn"), notify::RecursiveMode::NonRecursive);
                state.watcher = Some(watcher);
            }
            GetDpiForWindow(hwnd) as i32
        };
        if smoke && let Ok(dpi) = std::env::var("ULTRA_BAR_CALENDAR_SMOKE_DPI") {
            state.dpi = dpi.parse::<i32>()?.clamp(96, 192);
        }
        state.font = CreateFontW(
            -13 * state.dpi / 96,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET as _,
            OUT_DEFAULT_PRECIS as _,
            CLIP_DEFAULT_PRECIS as _,
            CLEARTYPE_NATURAL_QUALITY as _,
            DEFAULT_PITCH as _,
            wide("Segoe UI").as_ptr(),
        );
        state.glass = ultra_bar_native_ui::enable_glass(hwnd);
        state.icon_font = CreateFontW(
            -19 * state.dpi / 96,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET as _,
            OUT_DEFAULT_PRECIS as _,
            CLIP_DEFAULT_PRECIS as _,
            CLEARTYPE_NATURAL_QUALITY as _,
            DEFAULT_PITCH as _,
            wide("Segoe MDL2 Assets").as_ptr(),
        );
        state.tips = Some(Tooltips::new(hwnd));
        position(&state);
        if smoke {
            let start = timezone()
                .from_local_datetime(&state.selected.and_hms_opt(9, 0, 0).unwrap())
                .single()
                .unwrap()
                .with_timezone(&Utc);
            let mut sample = CalendarEvent::draft(start, start + Duration::hours(1));
            sample.title = "项目复盘".into();
            sample.location = "会议室".into();
            sample.notes = "确认本周进度".into();
            state.repo.data.events.push(sample);
            for view in 0..4 {
                state.view = view;
                paint(&mut state);
            }
            crate::editor::smoke(&mut state)?;
            eprintln!(
                "calendar smoke: 4 views rendered; 6 declarative schedule editor schemas validated; no keyboard/mouse input"
            );
            DestroyWindow(hwnd);
        } else {
            SetTimer(hwnd, 1, 1000, None);
            if show {
                show_panel(&mut state);
            }
            let mut message: MSG = zeroed();
            while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
                let dialog = state
                    .editor
                    .as_ref()
                    .filter(|editor| IsWindow(editor.hwnd) != 0)
                    .map(|editor| editor.hwnd);
                if dialog.is_none_or(|dialog| IsDialogMessageW(dialog, &message) == 0) {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
        if !state.parent.is_null() {
            CloseHandle(state.parent);
        }
        if !state.font.is_null() {
            DeleteObject(state.font);
        }
        if !state.icon_font.is_null() {
            DeleteObject(state.icon_font);
        }
        Ok(())
    }
}
unsafe fn position(state: &App) {
    unsafe {
        let mut info: MONITORINFO = zeroed();
        info.cbSize = size_of::<MONITORINFO>() as _;
        GetMonitorInfoW(
            MonitorFromWindow(
                if state.owner.is_null() {
                    state.hwnd
                } else {
                    state.owner
                },
                MONITOR_DEFAULTTOPRIMARY,
            ),
            &mut info,
        );
        let unit = |n: i32| n * state.dpi / 96;
        let width = unit(640).min(info.rcWork.right - info.rcWork.left - 16);
        let height = unit(620).min(info.rcWork.bottom - info.rcWork.top - 16);
        SetWindowPos(
            state.hwnd,
            HWND_TOPMOST,
            info.rcWork.right - width - 8,
            info.rcWork.bottom - height - 8,
            width,
            height,
            SWP_NOACTIVATE,
        );
        SetWindowRgn(
            state.hwnd,
            CreateRoundRectRgn(0, 0, width, height, unit(20), unit(20)),
            1,
        );
    }
}
unsafe fn show_panel(state: &mut App) {
    unsafe {
        position(state);
        ShowWindow(state.hwnd, SW_SHOW);
        SetForegroundWindow(state.hwnd);
        InvalidateRect(state.hwnd, null(), 0);
    }
}
pub fn label(event: &CalendarEvent) -> String {
    let time = if event.all_day {
        "全天".into()
    } else {
        event
            .start_at
            .with_timezone(&timezone())
            .format("%H:%M")
            .to_string()
    };
    format!(
        "{} {}{} · {}",
        time,
        if event.completed { "✓ " } else { "" },
        event.title,
        calendar::schedule_kind(event).label()
    )
}
pub unsafe fn error(owner: HWND, result: Result<()>) {
    if let Err(error) = result {
        unsafe {
            MessageBoxW(
                owner,
                wide(&format!("{error:#}")).as_ptr(),
                wide("Schedule Calendar").as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}
unsafe extern "system" fn wndproc(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if ultra_bar_native_ui::theme::handle_message(hwnd, message, w) {
            return 0;
        }
        if message == WM_NCCREATE {
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                (*(l as *const CREATESTRUCTW)).lpCreateParams as _,
            );
        }
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
        if pointer.is_null() {
            return DefWindowProcW(hwnd, message, w, l);
        }
        let state = &mut *pointer;
        match message {
            WM_PAINT => {
                paint(state);
                0
            }
            WM_ERASEBKGND => 1,
            WM_COPYDATA if l != 0 && w as HWND == state.owner => {
                let packet = &*(l as *const COPYDATASTRUCT);
                if packet.dwData == ultra_bar_native_ui::editor_bridge::EVENT
                    && packet.cbData <= 65536
                    && !packet.lpData.is_null()
                {
                    let bytes = std::slice::from_raw_parts(
                        packet.lpData as *const u8,
                        packet.cbData as usize,
                    );
                    if let Ok(event) = serde_json::from_slice(bytes) {
                        state.pending_editor.push_back(event);
                        PostMessageW(hwnd, WM_APP + 14, 0, 0);
                        return 1;
                    }
                }
                if packet.dwData == ultra_bar_native_ui::settings_bridge::COPYDATA_SETTINGS
                    && packet.cbData <= 65536
                    && !packet.lpData.is_null()
                {
                    let bytes = std::slice::from_raw_parts(
                        packet.lpData as *const u8,
                        packet.cbData as usize,
                    );
                    if let Ok(request) = serde_json::from_slice(bytes) {
                        state.pending_settings = Some(request);
                        PostMessageW(hwnd, WM_APP + 11, 0, 0);
                        return 1;
                    }
                }
                0
            }
            m if m == WM_APP + 14 => {
                while let Some(event) = state.pending_editor.pop_front() {
                    if event.action == "path" {
                        if let Some((_, choice)) = state
                            .pending_path
                            .filter(|(session, _)| *session == event.session)
                        {
                            state.pending_path = None;
                            if let Some(path) = event.values.get("path").filter(|s| !s.is_empty()) {
                                let result = (|| -> Result<()> {
                                    match choice {
                                        1 => {
                                            state.repo.import(&std::fs::read(path)?)?;
                                        }
                                        2 => std::fs::write(
                                            path,
                                            serde_json::to_vec_pretty(&state.repo.data)?,
                                        )?,
                                        3 => {
                                            state
                                                .repo
                                                .import_holidays(&std::fs::read_to_string(path)?)?;
                                        }
                                        _ => {}
                                    }
                                    Ok(())
                                })();
                                error(hwnd, result);
                                InvalidateRect(hwnd, null(), 0);
                            }
                        }
                    } else {
                        crate::editor::event(state, event);
                    }
                }
                0
            }
            m if m == WM_APP + 11 => {
                if let Some(request) = state.pending_settings.take() {
                    let result = (|| -> Result<()> {
                        anyhow::ensure!(
                            IsWindowEnabled(hwnd) != 0
                                && state
                                    .editor
                                    .as_ref()
                                    .is_none_or(|editor| IsWindow(editor.hwnd) == 0),
                            "请先保存或关闭日程编辑器"
                        );
                        anyhow::ensure!(
                            ultra_bar_native_ui::settings_bridge::load(&state.settings_dir)?
                                .revision
                                == request.revision,
                            "设置已在其他位置更改，请重新载入后再保存"
                        );
                        apply_region(state, &request.values)
                    })();
                    publish_settings(
                        state,
                        result.err().map(|e| e.to_string()).unwrap_or_default(),
                        Some(request.revision),
                    );
                    InvalidateRect(hwnd, null(), 0);
                }
                0
            }
            m if m == RegisterWindowMessageW(wide("UltraBar.Plugin.Settings.v1").as_ptr()) => {
                if IsWindowEnabled(hwnd) == 0 {
                    SetForegroundWindow(GetLastActivePopup(hwnd));
                } else if state
                    .editor
                    .as_ref()
                    .is_some_and(|editor| IsWindow(editor.hwnd) != 0)
                {
                    if let Some(editor) = &state.editor {
                        SetForegroundWindow(editor.hwnd);
                    }
                } else {
                    region_settings(state);
                }
                0
            }
            m if m == WM_APP + 1 => {
                if let Some(editor) = &state.editor
                    && IsWindow(editor.hwnd) != 0
                {
                    SetForegroundWindow(editor.hwnd);
                } else if state.popover.toggle(hwnd) {
                    show_panel(state);
                }
                0
            }
            m if m == WM_APP + 2 => {
                SetTimer(hwnd, 2, 350, None);
                0
            }
            WM_TIMER => {
                if w == 2 {
                    KillTimer(hwnd, 2);
                    if let Ok(config) = crate::configuration(&state.root) {
                        state.config = config;
                        InvalidateRect(hwnd, null(), 0);
                    }
                    return 0;
                }
                if !state.parent.is_null() && WaitForSingleObject(state.parent, 0) == WAIT_OBJECT_0
                {
                    DestroyWindow(hwnd);
                    return 0;
                }
                let now = Utc::now();
                let mut weather_changed = false;
                if let Some(worker) = &state.weather_worker {
                    while let Ok((weather, error)) = worker.receive.try_recv() {
                        state.weather = weather;
                        state.weather_error = error;
                        weather_changed = true;
                    }
                }
                if weather_changed && IsWindowVisible(hwnd) != 0 {
                    InvalidateRect(hwnd, null(), 0);
                }
                if now.timestamp() / 60 != state.last_minute {
                    state.last_minute = now.timestamp() / 60;
                    let result = state.repo.scan_reminders(state.last_check, now).map(|_| ());
                    error(hwnd, result);
                    state.last_check = now;
                    if IsWindowVisible(hwnd) != 0 {
                        InvalidateRect(hwnd, null(), 0);
                    }
                }
                if !state.repo.data.pending.is_empty()
                    && state
                        .reminder
                        .as_ref()
                        .is_none_or(|reminder| IsWindow(reminder.hwnd) == 0)
                {
                    crate::reminder::show(state);
                }
                0
            }
            WM_ACTIVATE if w & 0xffff == WA_INACTIVE as usize => {
                if IsWindowEnabled(hwnd) != 0
                    && state
                        .editor
                        .as_ref()
                        .is_none_or(|editor| IsWindow(editor.hwnd) == 0)
                {
                    state.popover.outside(hwnd);
                }
                0
            }
            WM_CLOSE => {
                ShowWindow(hwnd, SW_HIDE);
                0
            }
            WM_DESTROY => {
                KillTimer(hwnd, 1);
                KillTimer(hwnd, 2);
                PostQuitMessage(0);
                0
            }
            WM_KEYDOWN if w == 0x1b => {
                ShowWindow(hwnd, SW_HIDE);
                0
            }
            WM_MOUSEWHEEL => {
                if (w >> 16) as u16 as i16 > 0 {
                    state.scroll = state.scroll.saturating_sub(3);
                } else {
                    state.scroll += 3;
                }
                InvalidateRect(hwnd, null(), 0);
                0
            }
            WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                let x = l as u16 as i16 as i32;
                let y = (l >> 16) as u16 as i16 as i32;
                let action = state
                    .hits
                    .iter()
                    .find(|hit| {
                        x >= hit.rect.left
                            && x < hit.rect.right
                            && y >= hit.rect.top
                            && y < hit.rect.bottom
                    })
                    .map(|hit| hit.action.clone());
                if let Some(action) = action {
                    match action {
                        Action::Settings => region_settings(state),
                        Action::Weather => {
                            state.weather_view = !state.weather_view;
                            InvalidateRect(hwnd, null(), 0);
                        }
                        Action::RefreshWeather => {
                            if let Some(worker) = &state.weather_worker {
                                let _ = worker.send.try_send(crate::region::Request::Refresh);
                            }
                        }
                        Action::Previous | Action::Next => {
                            let delta = if matches!(action, Action::Previous) {
                                -1
                            } else {
                                1
                            };
                            let absolute =
                                state.month.year() * 12 + state.month.month0() as i32 + delta;
                            state.month = NaiveDate::from_ymd_opt(
                                absolute.div_euclid(12),
                                absolute.rem_euclid(12) as u32 + 1,
                                1,
                            )
                            .unwrap_or(state.month);
                            state.view = 1;
                        }
                        Action::View(view) => {
                            state.view = view;
                            state.scroll = 0;
                            if view == 0 {
                                state.selected = today();
                                state.month = state.selected.with_day(1).unwrap();
                            }
                        }
                        Action::Day(date) => {
                            state.selected = date;
                            state.selected_event = None;
                            state.scroll = 0;
                            if message == WM_LBUTTONDBLCLK {
                                crate::editor::open(state, None);
                            }
                        }
                        Action::Event(id) => {
                            state.selected_event = Some(id.clone());
                            if message == WM_LBUTTONDBLCLK {
                                crate::editor::open(state, Some(id));
                            }
                        }
                        Action::New => crate::editor::open(state, None),
                        Action::Hide => {
                            ShowWindow(hwnd, SW_HIDE);
                        }
                        Action::Edit => {
                            if let Some(id) = state.selected_event.clone() {
                                crate::editor::open(state, Some(id));
                            }
                        }
                        Action::Complete => {
                            if let Some(id) = &state.selected_event {
                                let result = state.repo.complete(id);
                                error(hwnd, result);
                            }
                        }
                        Action::Delete => {
                            if let Some(id) = &state.selected_event
                                && MessageBoxW(
                                    hwnd,
                                    wide("删除这条日程？").as_ptr(),
                                    wide("删除日程").as_ptr(),
                                    MB_YESNO | MB_DEFBUTTON2,
                                ) == IDYES
                            {
                                let result = state.repo.remove(id);
                                error(hwnd, result);
                                state.selected_event = None;
                            }
                        }
                        Action::Data => data_menu(state),
                    }
                    InvalidateRect(hwnd, null(), 0);
                }
                0
            }
            _ => DefWindowProcW(hwnd, message, w, l),
        }
    }
}
fn action_glyph(action: &Action) -> &'static str {
    match action {
        Action::Settings => "\u{e713}",
        Action::Weather => "\u{e753}",
        Action::RefreshWeather => "\u{e72c}",
        Action::Hide => "\u{e8bb}",
        Action::Previous => "\u{e76b}",
        Action::Next => "\u{e76c}",
        Action::View(0) => "\u{e8bf}",
        Action::View(1) => "\u{e787}",
        Action::View(2) => "\u{e8fd}",
        Action::View(3) => "\u{e73e}",
        Action::New => "\u{e710}",
        Action::Edit => "\u{e70f}",
        Action::Complete => "\u{e73e}",
        Action::Delete => "\u{e74d}",
        Action::Data => "\u{e8b7}",
        _ => "",
    }
}
unsafe fn round_fill(dc: HDC, rect: &RECT, color: u32, radius: i32) {
    unsafe {
        let brush = CreateSolidBrush(color);
        let old_brush = SelectObject(dc, brush);
        let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
        RoundRect(
            dc,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            radius,
            radius,
        );
        SelectObject(dc, old_pen);
        SelectObject(dc, old_brush);
        DeleteObject(brush);
    }
}
unsafe fn paint(state: &mut App) {
    unsafe {
        let mut ps: PAINTSTRUCT = zeroed();
        BeginPaint(state.hwnd, &mut ps);
        let mut area: RECT = zeroed();
        GetClientRect(state.hwnd, &mut area);
        let Some(surface) = Surface::new(area.right, area.bottom) else {
            EndPaint(state.hwnd, &ps);
            return;
        };
        let dc = surface.dc;
        let old = SelectObject(dc, state.font);
        let brush = CreateSolidBrush(ultra_bar_native_ui::theme::palette().background);
        FillRect(dc, &area, brush);
        DeleteObject(brush);
        SetBkMode(dc, TRANSPARENT as _);
        let unit = |n: i32| n * state.dpi / 96;
        let text = |value: &str, mut rect: RECT, color: u32| {
            SetTextColor(dc, color);
            DrawTextW(
                dc,
                wide(value).as_ptr(),
                -1,
                &mut rect,
                DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
        };
        state.hits.clear();
        let mut button = |label: &str, x: i32, y: i32, width: i32, action: Action, active: bool| {
            let rect = RECT {
                left: unit(x),
                top: unit(y),
                right: unit(x + width),
                bottom: unit(y + 30),
            };
            round_fill(
                dc,
                &rect,
                if active {
                    ultra_bar_native_ui::theme::palette().selection
                } else {
                    ultra_bar_native_ui::theme::palette().hover
                },
                unit(12),
            );
            let font = SelectObject(dc, state.icon_font);
            SetTextColor(dc, ultra_bar_native_ui::theme::palette().text);
            let mut icon_rect = rect;
            DrawTextW(
                dc,
                wide(action_glyph(&action)).as_ptr(),
                -1,
                &mut icon_rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            SelectObject(dc, font);
            state.hits.push(Hit {
                rect,
                action,
                tip: label.into(),
            });
        };
        button("上个月", 12, 12, 32, Action::Previous, false);
        button("下个月", 50, 12, 32, Action::Next, false);
        text(
            &state.month.format("%Y年 %m月").to_string(),
            RECT {
                left: unit(98),
                top: unit(12),
                right: unit(300),
                bottom: unit(42),
            },
            0x00ffffff,
        );
        button("关闭日历", 588, 12, 32, Action::Hide, false);
        button("地区与天气设置", 548, 12, 32, Action::Settings, false);
        if !(2025..=2027).contains(&state.month.year())
            && !state
                .repo
                .data
                .holidays
                .iter()
                .any(|day| day.date.starts_with(&state.month.year().to_string()))
        {
            text(
                "该年调休数据未加载",
                RECT {
                    left: unit(300),
                    top: unit(12),
                    right: unit(500),
                    bottom: unit(42),
                },
                ultra_bar_native_ui::theme::palette().active_border,
            );
        }
        for (index, label) in state.config.views.iter().enumerate() {
            button(
                label,
                12 + index as i32 * 52,
                52,
                44,
                Action::View(index),
                state.view == index,
            );
        }
        button("天气", 300, 52, 32, Action::Weather, state.weather_view);
        text(
            if state.weather.summary.is_empty() {
                "天气"
            } else {
                &state.weather.summary
            },
            RECT {
                left: unit(344),
                top: unit(52),
                right: unit(580),
                bottom: unit(82),
            },
            ultra_bar_native_ui::theme::palette().text,
        );
        if state.weather_view {
            button("刷新天气", 588, 52, 32, Action::RefreshWeather, false);
            let mut lines = vec![state.weather.place.clone(), state.weather.summary.clone()];
            lines.extend(state.weather.detail.split(" · ").map(str::to_owned));
            if state.region.city.is_empty() {
                lines.push("在地区与天气设置中选择天气地区".into());
            }
            if !state.weather_error.is_empty() {
                lines.push(state.weather_error.clone());
            }
            if state.weather.updated > 0 {
                lines.push(format!(
                    "更新于 {}",
                    chrono::DateTime::from_timestamp(state.weather.updated, 0)
                        .unwrap_or_default()
                        .with_timezone(&timezone())
                        .format("%m-%d %H:%M")
                ));
            }
            lines.push(state.weather.source.clone());
            for (i, line) in lines.iter().filter(|s| !s.is_empty()).enumerate() {
                text(
                    line,
                    RECT {
                        left: unit(24),
                        top: unit(116 + i as i32 * 34),
                        right: area.right - unit(24),
                        bottom: unit(146 + i as i32 * 34),
                    },
                    ultra_bar_native_ui::theme::palette().text,
                );
            }
            BitBlt(ps.hdc, 0, 0, area.right, area.bottom, dc, 0, 0, SRCCOPY);
            SelectObject(dc, old);
            EndPaint(state.hwnd, &ps);
            return;
        }
        let list_top;
        if state.view <= 1 {
            let cell_width = (area.right - unit(24)) / 7;
            let grid_top = unit(116);
            let cell_height = unit(48).min((area.bottom - unit(250)) / 6).max(unit(30));
            let start = state.month
                - Duration::days(if state.region.sunday_first {
                    state.month.weekday().num_days_from_sunday()
                } else {
                    state.month.weekday().num_days_from_monday()
                } as i64);
            for index in 0..7 {
                let label = &state.config.weekdays[if state.region.sunday_first {
                    (index + 6) % 7
                } else {
                    index
                }];
                text(
                    label,
                    RECT {
                        left: unit(20) + index as i32 * cell_width,
                        top: unit(88),
                        right: unit(12) + (index as i32 + 1) * cell_width,
                        bottom: unit(112),
                    },
                    ultra_bar_native_ui::theme::palette().muted,
                );
            }
            for index in 0..42 {
                let date = start + Duration::days(index as i64);
                let rect = RECT {
                    left: unit(12) + index % 7 * cell_width,
                    top: grid_top + index / 7 * cell_height,
                    right: unit(12) + (index % 7 + 1) * cell_width - unit(3),
                    bottom: grid_top + (index / 7 + 1) * cell_height - unit(3),
                };
                if date == state.selected || date == today() {
                    round_fill(
                        dc,
                        &rect,
                        if date == state.selected {
                            ultra_bar_native_ui::theme::palette().selection
                        } else {
                            ultra_bar_native_ui::theme::palette().hover
                        },
                        unit(12),
                    );
                }
                let holiday = state.repo.holidays.get(&date.to_string());
                let color = if date.month() != state.month.month() {
                    ultra_bar_native_ui::theme::palette().disabled
                } else if calendar::is_legal_rest_day(date, holiday) {
                    ultra_bar_native_ui::theme::palette().active_border
                } else {
                    ultra_bar_native_ui::theme::palette().text
                };
                let count = state.repo.occurrences(date).len();
                let title = format!("{}{}", date.day(), if count > 0 { " •" } else { "" });
                text(
                    &title,
                    RECT {
                        left: rect.left + unit(6),
                        bottom: rect.top + cell_height / 2,
                        ..rect
                    },
                    color,
                );
                let lunar = holiday
                    .map(|day| format!("{} {}", if day.is_off_day { "休" } else { "班" }, day.name))
                    .or_else(|| calendar::lunar_festival(date).map(str::to_owned))
                    .unwrap_or_else(|| calendar::lunar_label(date));
                text(
                    &lunar,
                    RECT {
                        left: rect.left + unit(6),
                        top: rect.top + cell_height / 2,
                        ..rect
                    },
                    ultra_bar_native_ui::theme::palette().muted,
                );
                state.hits.push(Hit {
                    rect,
                    action: Action::Day(date),
                    tip: format!(
                        "{} {} · {} 项日程",
                        date,
                        calendar::lunar_date_label(
                            calendar::lunar_parts(date).unwrap_or((1, 1, false)).0,
                            calendar::lunar_parts(date).unwrap_or((1, 1, false)).1,
                            calendar::lunar_parts(date).unwrap_or((1, 1, false)).2
                        ),
                        count
                    ),
                });
            }
            list_top = grid_top + cell_height * 6 + unit(8);
            text(
                &format!(
                    "{} · {}",
                    state.selected,
                    calendar::lunar_label(state.selected)
                ),
                RECT {
                    left: unit(12),
                    top: list_top,
                    right: area.right - unit(12),
                    bottom: list_top + unit(24),
                },
                0x00ffffff,
            );
        } else {
            list_top = unit(90);
        }
        let mut events: Vec<_> = if state.view <= 1 {
            state.repo.occurrences(state.selected)
        } else {
            state
                .repo
                .data
                .events
                .iter()
                .filter(|event| !event.deleted && (state.view != 3 || event.completed))
                .collect()
        };
        events.sort_by_key(|event| event.start_at);
        let start_y = list_top + unit(28);
        let row_height = unit(28);
        let capacity = ((area.bottom - unit(56) - start_y) / row_height).max(1) as usize;
        state.scroll = state.scroll.min(events.len().saturating_sub(capacity));
        if events.is_empty() {
            text(
                "暂无日程",
                RECT {
                    left: unit(12),
                    top: start_y,
                    right: area.right - unit(12),
                    bottom: start_y + row_height,
                },
                ultra_bar_native_ui::theme::palette().muted,
            );
        }
        for (position, event) in events.iter().skip(state.scroll).take(capacity).enumerate() {
            let rect = RECT {
                left: unit(12),
                top: start_y + position as i32 * row_height,
                right: area.right - unit(12),
                bottom: start_y + (position as i32 + 1) * row_height - unit(2),
            };
            if state.selected_event.as_deref() == Some(&event.id) {
                round_fill(
                    dc,
                    &rect,
                    ultra_bar_native_ui::theme::palette().hover,
                    unit(12),
                );
            }
            let value = if state.view >= 2 {
                format!(
                    "{} {}",
                    event.start_at.with_timezone(&timezone()).format("%m-%d"),
                    label(event)
                )
            } else {
                label(event)
            };
            text(
                &value,
                RECT {
                    left: rect.left + unit(6),
                    ..rect
                },
                ultra_bar_native_ui::theme::palette().text,
            );
            state.hits.push(Hit {
                rect,
                action: Action::Event(event.id.clone()),
                tip: format!("{}\n{}\n{}", value, event.location, event.notes),
            });
        }
        // Footer remains reachable regardless of event count; the list scrolls independently.
        let footer = area.bottom * 96 / state.dpi - 42;
        for (label, x, width, action) in [
            ("新建日程", 12, 44, Action::New),
            ("编辑", 68, 44, Action::Edit),
            ("标记完成／取消完成", 124, 44, Action::Complete),
            ("删除", 180, 44, Action::Delete),
            ("数据管理", 576, 44, Action::Data),
        ] {
            let rect = RECT {
                left: unit(x),
                top: unit(footer),
                right: unit(x + width),
                bottom: unit(footer + 30),
            };
            round_fill(
                dc,
                &rect,
                ultra_bar_native_ui::theme::palette().hover,
                unit(12),
            );
            let font = SelectObject(dc, state.icon_font);
            SetTextColor(dc, ultra_bar_native_ui::theme::palette().text);
            let mut icon_rect = rect;
            DrawTextW(
                dc,
                wide(action_glyph(&action)).as_ptr(),
                -1,
                &mut icon_rect,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
            SelectObject(dc, font);
            state.hits.push(Hit {
                rect,
                action,
                tip: label.into(),
            });
        }
        if state.smoke
            && let Some(directory) = std::env::var_os("ULTRA_BAR_CALENDAR_SMOKE_DIR")
        {
            let path = PathBuf::from(directory).join(format!("calendar-view-{}.bmp", state.view));
            let _ = surface.save_bmp(&path);
        }
        SelectObject(dc, old);
        surface.present(state.hwnd, state.glass);
        if let Some(tips) = &mut state.tips {
            tips.sync(
                state
                    .hits
                    .iter()
                    .map(|hit| Tip {
                        bounds: (hit.rect.left, hit.rect.top, hit.rect.right, hit.rect.bottom),
                        text: hit.tip.clone(),
                    })
                    .collect(),
            );
        }
        EndPaint(state.hwnd, &ps);
    }
}

unsafe fn data_menu(state: &mut App) {
    unsafe {
        let menu = CreatePopupMenu();
        for (id, label) in [
            (1, "导入日程 JSON（补充新日程）"),
            (2, "导出日程 JSON"),
            (3, "导入节假日 JSON"),
        ] {
            AppendMenuW(menu, MF_STRING, id, wide(label).as_ptr());
        }
        let mut point: POINT = zeroed();
        GetCursorPos(&mut point);
        let choice = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY,
            point.x,
            point.y,
            0,
            state.hwnd,
            null(),
        );
        DestroyMenu(menu);
        if choice == 0 {
            return;
        }
        if (1..=3).contains(&choice) {
            let session = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            state.pending_path = Some((session, choice));
            let result = ultra_bar_native_ui::editor_bridge::request(
                state.owner,
                state.hwnd,
                "com.emssion.schedule-calendar",
                ultra_bar_native_ui::editor_bridge::Operation::ChoosePath {
                    session,
                    save: choice == 2,
                },
            );
            error(state.hwnd, result.map_err(anyhow::Error::msg));
        }
    }
}
