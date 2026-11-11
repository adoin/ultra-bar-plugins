use crate::region::timezone;
use crate::{
    calendar::{self, ScheduleKind},
    engine::Draft,
    models::CalendarEvent,
    ui::{self, App},
};
use anyhow::{Context, Result};
use chrono::{Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};
use std::{
    collections::HashMap,
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
};
use ultra_bar_native_ui::text::draw_text as DrawTextW;
use ultra_bar_native_ui::wide;
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    Storage::Xps::PrintWindow,
    System::LibraryLoader::*,
    UI::{Controls::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};

pub struct Editor {
    pub hwnd: HWND,
    form: HWND,
    app: *mut App,
    fields: HashMap<i32, HWND>,
    source: Option<CalendarEvent>,
    scroll: i32,
    total: i32,
    icon_font: HFONT,
    tip_strings: Vec<Vec<u16>>,
}

fn background() -> HBRUSH {
    ultra_bar_native_ui::theme::background_brush()
}
fn field_brush() -> HBRUSH {
    ultra_bar_native_ui::theme::field_brush()
}
const PREPARE_POPUP: u32 = WM_APP + 0x52;
unsafe fn opaque_popup(hwnd: HWND) {
    unsafe {
        if hwnd.is_null() {
            return;
        }
        #[repr(C)]
        struct Accent {
            state: i32,
            flags: i32,
            tint: u32,
            animation: i32,
        }
        #[repr(C)]
        struct Attribute {
            attribute: i32,
            data: *mut std::ffi::c_void,
            size: usize,
        }
        let module = GetModuleHandleW(wide("user32.dll").as_ptr());
        if let Some(address) =
            GetProcAddress(module, c"SetWindowCompositionAttribute".as_ptr().cast())
        {
            let set: unsafe extern "system" fn(HWND, *mut Attribute) -> i32 =
                std::mem::transmute(address);
            let mut accent = Accent {
                state: 0,
                flags: 0,
                tint: 0,
                animation: 0,
            };
            let mut data = Attribute {
                attribute: 19,
                data: (&mut accent as *mut Accent).cast(),
                size: size_of::<Accent>(),
            };
            set(hwnd, &mut data);
        }
        use windows_sys::Win32::Graphics::Dwm::*;
        let blur = DWM_BLURBEHIND {
            dwFlags: DWM_BB_ENABLE,
            fEnable: 0,
            hRgnBlur: null_mut(),
            fTransitionOnMaximized: 0,
        };
        DwmEnableBlurBehindWindow(hwnd, &blur);
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let next = ex & !((WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_CLIENTEDGE) as isize);
        if ex != next {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, next);
        }
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
        InvalidateRect(hwnd, null(), 1);
    }
}
unsafe fn prepare_combo_popup(combo: HWND) {
    unsafe {
        let mut info: COMBOBOXINFO = zeroed();
        info.cbSize = size_of::<COMBOBOXINFO>() as _;
        if GetComboBoxInfo(combo, &mut info) != 0 && !info.hwndList.is_null() {
            SetWindowTheme(info.hwndList, wide("DarkMode_Explorer").as_ptr(), null());
            SetWindowSubclass(info.hwndList, Some(popup_proc), 9, combo as usize);
            PostMessageW(info.hwndList, PREPARE_POPUP, 0, 0);
        }
    }
}
unsafe fn paint_combo_popup(list: HWND, combo: HWND, dc: HDC) {
    unsafe {
        let mut area: RECT = zeroed();
        GetClientRect(list, &mut area);
        FillRect(dc, &area, field_brush());
        SetBkMode(dc, TRANSPARENT as _);
        let font = SendMessageW(combo, WM_GETFONT, 0, 0) as HFONT;
        let old = SelectObject(dc, font);
        let count = SendMessageW(list, LB_GETCOUNT, 0, 0).clamp(0, 1024) as usize;
        let top = SendMessageW(list, LB_GETTOPINDEX, 0, 0).max(0) as usize;
        let selected = SendMessageW(list, LB_GETCURSEL, 0, 0);
        for i in top..count {
            let mut rect: RECT = zeroed();
            if SendMessageW(list, LB_GETITEMRECT, i, (&mut rect as *mut RECT) as isize)
                == LB_ERR as isize
            {
                break;
            }
            if rect.top >= area.bottom {
                break;
            }
            if selected == i as isize {
                let brush = CreateSolidBrush(ultra_bar_native_ui::theme::palette().selection);
                FillRect(dc, &rect, brush);
                DeleteObject(brush);
            }
            let length = SendMessageW(list, LB_GETTEXTLEN, i, 0);
            if !(0..=4096).contains(&length) {
                continue;
            }
            let mut text = vec![0u16; length as usize + 1];
            if SendMessageW(list, LB_GETTEXT, i, text.as_mut_ptr() as isize) == LB_ERR as isize {
                continue;
            }
            let padding =
                8 * windows_sys::Win32::UI::HiDpi::GetDpiForWindow(combo).max(96) as i32 / 96;
            rect.left += padding;
            rect.right -= padding;
            SetTextColor(dc, ultra_bar_native_ui::theme::palette().text);
            DrawTextW(
                dc,
                text.as_ptr(),
                -1,
                &mut rect,
                DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
        }
        SelectObject(dc, old);
    }
}
unsafe extern "system" fn popup_proc(
    hwnd: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    unsafe {
        if message == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(popup_proc), id);
            return DefSubclassProc(hwnd, message, w, l);
        }
        if message == PREPARE_POPUP {
            opaque_popup(hwnd);
            return 0;
        }
        if message == WM_WINDOWPOSCHANGING && l != 0 {
            let position = &mut *(l as *mut WINDOWPOS);
            if position.flags & SWP_HIDEWINDOW == 0 {
                position.hwndInsertAfter = HWND_TOPMOST;
                position.flags &= !SWP_NOZORDER;
                position.flags |= SWP_NOACTIVATE;
            }
        }
        if message == WM_SHOWWINDOW && w != 0 {
            let result = DefSubclassProc(hwnd, message, w, l);
            PostMessageW(hwnd, PREPARE_POPUP, 0, 0);
            return result;
        }
        if message == WM_STYLECHANGED
            && w as i32 == GWL_EXSTYLE
            && l != 0
            && (*(l as *const STYLESTRUCT)).styleNew & (WS_EX_LAYERED | WS_EX_TRANSPARENT) != 0
        {
            PostMessageW(hwnd, PREPARE_POPUP, 0, 0);
        }
        if data != 0 {
            if message == WM_ERASEBKGND {
                let mut area = zeroed();
                GetClientRect(hwnd, &mut area);
                FillRect(w as _, &area, field_brush());
                return 1;
            }
            if message == WM_PAINT {
                let mut ps = zeroed();
                let dc = BeginPaint(hwnd, &mut ps);
                paint_combo_popup(hwnd, data as HWND, dc);
                EndPaint(hwnd, &ps);
                return 0;
            }
            if message == WM_PRINT || message == WM_PRINTCLIENT {
                paint_combo_popup(hwnd, data as HWND, w as _);
                return 0;
            }
        }
        let result = DefSubclassProc(hwnd, message, w, l);
        if data != 0
            && matches!(
                message,
                WM_MOUSEMOVE | WM_KEYDOWN | WM_VSCROLL | LB_SETCURSEL
            )
        {
            InvalidateRect(hwnd, null(), 0);
        }
        result
    }
}

unsafe fn control_color(message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        let dc = w as HDC;
        SetBkMode(dc, TRANSPARENT as _);
        let static_label = message == WM_CTLCOLORSTATIC;
        SetTextColor(
            dc,
            if static_label && GetDlgCtrlID(l as HWND) != 250 {
                ultra_bar_native_ui::theme::palette().muted
            } else {
                ultra_bar_native_ui::theme::palette().text
            },
        );
        SetBkColor(
            dc,
            if static_label {
                ultra_bar_native_ui::theme::palette().background
            } else {
                ultra_bar_native_ui::theme::palette().field
            },
        );
        if static_label {
            background() as _
        } else {
            field_brush() as _
        }
    }
}
unsafe fn create(
    parent: HWND,
    class: &str,
    label: &str,
    id: i32,
    style: u32,
    bounds: (i32, i32, i32, i32),
    typography: (HFONT, i32),
) -> HWND {
    unsafe {
        let (x, y, width, height) = bounds;
        let (font, dpi) = typography;
        let unit = |n: i32| n * dpi / 96;
        let child = CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(label).as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            unit(x),
            unit(y),
            unit(width),
            unit(height),
            parent,
            id as _,
            GetModuleHandleW(null()),
            null(),
        );
        SendMessageW(child, WM_SETFONT, font as _, 1);
        if class == "EDIT" || class == "SysDateTimePick32" {
            SetWindowLongPtrW(
                child,
                GWL_STYLE,
                GetWindowLongPtrW(child, GWL_STYLE) & !(WS_BORDER as isize),
            );
            SetWindowLongPtrW(
                child,
                GWL_EXSTYLE,
                GetWindowLongPtrW(child, GWL_EXSTYLE) & !(WS_EX_CLIENTEDGE as isize),
            );
            SetWindowPos(
                child,
                null_mut(),
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        if class == "EDIT" {
            let margin = unit(8) as u32;
            SendMessageW(
                child,
                EM_SETMARGINS,
                (EC_LEFTMARGIN | EC_RIGHTMARGIN) as _,
                (margin | (margin << 16)) as _,
            );
        }
        SetWindowTheme(child, wide("DarkMode_CFD").as_ptr(), null());
        if class == "COMBOBOX" {
            SetWindowSubclass(child, Some(combo_proc), 1, dpi as _);
        }
        if class == "SysDateTimePick32" {
            SetWindowSubclass(child, Some(date_proc), 1, dpi as _);
        }
        if class == "BUTTON" && (style & 0xf) == BS_AUTOCHECKBOX as u32 {
            SetWindowSubclass(child, Some(check_proc), 1, dpi as _);
        }
        if class == "EDIT" || class == "COMBOBOX" || class == "SysDateTimePick32" {
            let mut bounds: RECT = zeroed();
            GetWindowRect(child, &mut bounds);
            let region = CreateRoundRectRgn(
                0,
                0,
                bounds.right - bounds.left,
                bounds.bottom - bounds.top,
                unit(10),
                unit(10),
            );
            SetWindowRgn(child, region, 1);
        }
        child
    }
}
pub unsafe fn open(app: &mut App, id: Option<String>) {
    unsafe {
        if let Some(editor) = &app.editor
            && IsWindow(editor.hwnd) != 0
        {
            SetForegroundWindow(editor.hwnd);
            return;
        }
        let source = id.as_ref().and_then(|id| app.repo.event(id)).cloned();
        let initial = source.clone().unwrap_or_else(|| {
            let time = timezone()
                .from_local_datetime(&app.selected.and_hms_opt(9, 0, 0).unwrap())
                .single()
                .unwrap()
                .with_timezone(&Utc);
            CalendarEvent::draft(time, time + Duration::hours(1))
        });
        let module = GetModuleHandleW(null());
        for (class, proc) in [
            (
                "UltraBar.Calendar.Editor",
                Some(
                    editor_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
                ),
            ),
            (
                "UltraBar.Calendar.Form",
                Some(form_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT),
            ),
        ] {
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: proc,
                hInstance: module,
                lpszClassName: wide(class).as_ptr(),
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),

                ..zeroed()
            });
        }
        let dpi = app.dpi;
        let unit = |n: i32| n * dpi / 96;
        let mut info: MONITORINFO = zeroed();
        info.cbSize = size_of::<MONITORINFO>() as _;
        GetMonitorInfoW(
            MonitorFromWindow(app.hwnd, MONITOR_DEFAULTTONEAREST),
            &mut info,
        );
        let width = unit(520);
        let height = unit(740).min(info.rcWork.bottom - info.rcWork.top - 16);
        let mut editor = Box::new(Editor {
            hwnd: null_mut(),
            form: null_mut(),
            app: app as *mut App,
            fields: HashMap::new(),
            source,
            scroll: 0,
            total: unit(602),
            icon_font: CreateFontW(
                -19 * dpi / 96,
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
            ),
            tip_strings: vec![],
        });
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            wide("UltraBar.Calendar.Editor").as_ptr(),
            wide(if editor.source.is_some() {
                "编辑日程"
            } else {
                "新建日程"
            })
            .as_ptr(),
            WS_POPUP | WS_SYSMENU | WS_CLIPCHILDREN,
            (info.rcWork.left + info.rcWork.right - width) / 2,
            (info.rcWork.top + info.rcWork.bottom - height) / 2,
            width,
            height,
            app.hwnd,
            null_mut(),
            module,
            (&mut *editor as *mut Editor).cast(),
        );
        if hwnd.is_null() {
            return;
        }
        editor.hwnd = hwnd;
        let border = 0xfffffffeu32;
        windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            hwnd,
            35,
            (&border as *const u32).cast(),
            size_of::<u32>() as _,
        );
        ultra_bar_native_ui::enable_glass(hwnd);
        SetLayeredWindowAttributes(hwnd, 0, 232, LWA_ALPHA);
        let region = CreateRoundRectRgn(0, 0, width, height, unit(20), unit(20));
        SetWindowRgn(hwnd, region, 1);
        let mut rect: RECT = zeroed();
        GetClientRect(hwnd, &mut rect);
        create(
            hwnd,
            "STATIC",
            if editor.source.is_some() {
                "编辑日程"
            } else {
                "新建日程"
            },
            250,
            0,
            (16, 12, 430, 28),
            (app.font, app.dpi),
        );
        let close = create(
            hwnd,
            "BUTTON",
            "关闭编辑器",
            4,
            BS_OWNERDRAW as u32 | WS_TABSTOP,
            (468, 8, 32, 32),
            (app.font, app.dpi),
        );
        editor.fields.insert(4, close);
        let form = CreateWindowExW(
            0,
            wide("UltraBar.Calendar.Form").as_ptr(),
            null(),
            WS_CHILD | WS_VISIBLE | WS_VSCROLL | WS_CLIPCHILDREN,
            0,
            unit(48),
            rect.right,
            rect.bottom - unit(102),
            hwnd,
            null_mut(),
            module,
            (&mut *editor as *mut Editor).cast(),
        );
        editor.form = form;
        let mut label = |value: &str, x: i32, y: i32, id: i32| {
            let field = create(
                form,
                "STATIC",
                value,
                id,
                0,
                (x, y, 450 - x, 20),
                (app.font, app.dpi),
            );
            editor.fields.insert(id, field);
        };
        for (value, x, y, id) in [
            ("标题", 16, 8, 200),
            ("发生规则", 16, 68, 201),
            ("日期", 16, 128, 202),
            ("农历月 / 日", 16, 128, 205),
            ("开始时间", 16, 208, 203),
            ("时长（分钟）", 240, 208, 204),
            ("地点", 16, 290, 209),
            ("通知链接", 256, 290, 210),
            ("备注", 16, 350, 211),
            ("相对提醒", 16, 442, 212),
        ] {
            label(value, x, y, id);
        }
        let mut field =
            |class: &str, text: &str, id: i32, style: u32, x: i32, y: i32, w: i32, h: i32| {
                let field = create(
                    form,
                    class,
                    text,
                    id,
                    style | WS_TABSTOP,
                    (x, y, w, h),
                    (app.font, app.dpi),
                );
                editor.fields.insert(id, field);
                field
            };
        field(
            "EDIT",
            &initial.title,
            100,
            WS_BORDER | ES_AUTOHSCROLL as u32,
            16,
            30,
            468,
            28,
        );
        let kind = field(
            "COMBOBOX",
            "",
            101,
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
            16,
            90,
            468,
            220,
        );
        for index in 0..6 {
            SendMessageW(
                kind,
                CB_ADDSTRING,
                0,
                wide(ScheduleKind::from_index(index).label()).as_ptr() as _,
            );
        }
        SendMessageW(
            kind,
            CB_SETCURSEL,
            calendar::schedule_kind(&initial).index() as _,
            0,
        );
        let date = field(
            "SysDateTimePick32",
            "",
            102,
            DTS_SHORTDATECENTURYFORMAT,
            16,
            150,
            468,
            28,
        );
        SendMessageW(date, DTM_SETFORMATW, 0, wide("yyyy-MM-dd").as_ptr() as _);
        let time = field(
            "SysDateTimePick32",
            "",
            103,
            DTS_TIMEFORMAT | DTS_UPDOWN,
            16,
            230,
            200,
            28,
        );
        SendMessageW(time, DTM_SETFORMATW, 0, wide("HH:mm").as_ptr() as _);
        let local = initial.start_at.with_timezone(&timezone());
        let system = SYSTEMTIME {
            wYear: local.year() as _,
            wMonth: local.month() as _,
            wDay: local.day() as _,
            wHour: local.hour() as _,
            wMinute: local.minute() as _,
            ..zeroed()
        };
        for field in [date, time] {
            SendMessageW(
                field,
                DTM_SETSYSTEMTIME,
                GDT_VALID as _,
                (&system as *const SYSTEMTIME) as _,
            );
        }
        field(
            "EDIT",
            &(initial.end_at - initial.start_at)
                .num_minutes()
                .to_string(),
            104,
            WS_BORDER | ES_NUMBER as u32,
            240,
            230,
            244,
            28,
        );
        let month = field(
            "COMBOBOX",
            "",
            105,
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
            16,
            150,
            124,
            260,
        );
        for i in 1..=12 {
            SendMessageW(
                month,
                CB_ADDSTRING,
                0,
                wide(&calendar::lunar_date_label(i, 1, false).replace("初一", "")).as_ptr() as _,
            );
        }
        let day = field(
            "COMBOBOX",
            "",
            106,
            CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
            156,
            150,
            124,
            260,
        );
        for i in 1..=30 {
            SendMessageW(
                day,
                CB_ADDSTRING,
                0,
                wide(&calendar::lunar_date_label(1, i, false).replace("农历正月", "")).as_ptr()
                    as _,
            );
        }
        let lunar = calendar::yearly_lunar_parts(&initial);
        SendMessageW(month, CB_SETCURSEL, lunar.0.saturating_sub(1) as _, 0);
        SendMessageW(day, CB_SETCURSEL, lunar.1.saturating_sub(1) as _, 0);
        field(
            "BUTTON",
            "闰月",
            107,
            BS_AUTOCHECKBOX as u32,
            300,
            150,
            180,
            28,
        );
        field(
            "STATIC",
            "按法定休息／工作日触发，包含调休安排",
            220,
            0,
            16,
            150,
            468,
            42,
        );
        field(
            "BUTTON",
            "全天",
            108,
            BS_AUTOCHECKBOX as u32,
            16,
            260,
            180,
            24,
        );
        field(
            "EDIT",
            &initial.location,
            109,
            WS_BORDER | ES_AUTOHSCROLL as u32,
            16,
            312,
            224,
            28,
        );
        field(
            "EDIT",
            &initial.notification_url,
            110,
            WS_BORDER | ES_AUTOHSCROLL as u32,
            256,
            312,
            228,
            28,
        );
        field(
            "EDIT",
            &initial.notes,
            111,
            WS_BORDER | ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32 | WS_VSCROLL,
            16,
            372,
            468,
            60,
        );
        for (id, text, x) in [
            (112, "准时", 16),
            (113, "10 分钟", 126),
            (114, "1 小时", 252),
            (115, "前一天", 368),
        ] {
            field("BUTTON", text, id, BS_AUTOCHECKBOX as u32, x, 464, 112, 28);
        }
        field(
            "BUTTON",
            "自定义提前（分钟）",
            117,
            BS_AUTOCHECKBOX as u32,
            16,
            504,
            220,
            28,
        );
        let custom = initial
            .reminder_minutes
            .iter()
            .find(|minute| ![0, 10, 60, 1440].contains(minute))
            .copied();
        field(
            "EDIT",
            &custom.map(|v| v.to_string()).unwrap_or_default(),
            116,
            WS_BORDER | ES_NUMBER as u32,
            252,
            504,
            232,
            28,
        );
        field(
            "BUTTON",
            "强制提醒",
            118,
            BS_AUTOCHECKBOX as u32,
            16,
            552,
            220,
            28,
        );
        field(
            "BUTTON",
            "标记完成",
            119,
            BS_AUTOCHECKBOX as u32,
            252,
            552,
            232,
            28,
        );
        for (id, checked) in [
            (107, lunar.2),
            (108, initial.all_day),
            (112, initial.reminder_minutes.contains(&0)),
            (113, initial.reminder_minutes.contains(&10)),
            (114, initial.reminder_minutes.contains(&60)),
            (115, initial.reminder_minutes.contains(&1440)),
            (117, custom.is_some()),
            (118, initial.force_reminder),
            (119, initial.completed),
        ] {
            SendMessageW(
                editor.fields[&id],
                BM_SETCHECK,
                if checked { BST_CHECKED } else { BST_UNCHECKED } as _,
                0,
            );
        }
        for (id, text, x) in [(1, "保存", 388), (2, "取消", 444), (3, "删除", 16)] {
            let button = create(
                hwnd,
                "BUTTON",
                text,
                id,
                WS_TABSTOP | BS_OWNERDRAW as u32,
                (x, (rect.bottom - unit(42)) * 96 / app.dpi, 40, 30),
                (app.font, app.dpi),
            );
            editor.fields.insert(id, button);
            if id == 3 && editor.source.is_none() {
                ShowWindow(button, SW_HIDE);
            }
        }
        let info = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as _,
            fMask: SIF_RANGE | SIF_PAGE,
            nMin: 0,
            nMax: editor.total,
            nPage: (rect.bottom - unit(102)) as _,
            ..zeroed()
        };
        SetScrollInfo(form, SB_VERT, &info, 1);
        update_fields(&editor);
        let tips = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            TOOLTIPS_CLASSW,
            null(),
            WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX,
            0,
            0,
            0,
            0,
            hwnd,
            null_mut(),
            module,
            null(),
        );
        editor.tip_strings = ["保存", "取消", "删除", "关闭编辑器"]
            .into_iter()
            .map(wide)
            .collect();
        for (index, id) in [1, 2, 3, 4].into_iter().enumerate() {
            let info = TTTOOLINFOW {
                cbSize: size_of::<TTTOOLINFOW>() as _,
                uFlags: TTF_IDISHWND | TTF_SUBCLASS,
                hwnd,
                uId: editor.fields[&id] as usize,
                lpszText: editor.tip_strings[index].as_mut_ptr(),
                ..zeroed()
            };
            SendMessageW(tips, TTM_ADDTOOLW, 0, (&info as *const TTTOOLINFOW) as _);
        }
        app.editor = Some(editor);
        if !app.smoke {
            EnableWindow(app.hwnd, 0);
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
            SetFocus(app.editor.as_ref().unwrap().fields[&100]);
        }
    }
}

unsafe fn update_fields(editor: &Editor) {
    unsafe {
        let kind = SendMessageW(editor.fields[&101], CB_GETCURSEL, 0, 0) as i32;
        let lunar = kind == 4;
        let date = kind == 0 || kind == 3;
        for id in [102, 202, 108] {
            ShowWindow(editor.fields[&id], if date { SW_SHOW } else { SW_HIDE });
        }
        for id in [105, 106, 107, 205] {
            ShowWindow(editor.fields[&id], if lunar { SW_SHOW } else { SW_HIDE });
        }
        ShowWindow(
            editor.fields[&220],
            if kind == 1 || kind == 2 {
                SW_SHOW
            } else {
                SW_HIDE
            },
        );
        EnableWindow(
            editor.fields[&103],
            if date && SendMessageW(editor.fields[&108], BM_GETCHECK, 0, 0) == BST_CHECKED as isize
            {
                0
            } else {
                1
            },
        );
    }
}
unsafe fn text(editor: &Editor, id: i32) -> String {
    unsafe {
        let field = editor.fields[&id];
        let length = GetWindowTextLengthW(field).clamp(0, 65536) as usize;
        let mut value = vec![0u16; length + 1];
        let len = GetWindowTextW(field, value.as_mut_ptr(), value.len() as _);
        String::from_utf16_lossy(&value[..len.max(0) as usize])
    }
}
unsafe fn draft(editor: &Editor) -> Result<CalendarEvent> {
    unsafe {
        let checked =
            |id| SendMessageW(editor.fields[&id], BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
        let mut date: SYSTEMTIME = zeroed();
        let mut time: SYSTEMTIME = zeroed();
        anyhow::ensure!(
            SendMessageW(
                editor.fields[&102],
                DTM_GETSYSTEMTIME,
                0,
                (&mut date as *mut SYSTEMTIME) as _
            ) == GDT_VALID as isize,
            "请选择日期"
        );
        anyhow::ensure!(
            SendMessageW(
                editor.fields[&103],
                DTM_GETSYSTEMTIME,
                0,
                (&mut time as *mut SYSTEMTIME) as _
            ) == GDT_VALID as isize,
            "请选择时间"
        );
        let mut reminders = vec![];
        for (id, minutes) in [(112, 0), (113, 10), (114, 60), (115, 1440)] {
            if checked(id) {
                reminders.push(minutes);
            }
        }
        if checked(117) {
            reminders.push(
                text(editor, 116)
                    .parse()
                    .context("自定义提醒必须为分钟数")?,
            );
        }
        Draft {
            title: text(editor, 100),
            kind: SendMessageW(editor.fields[&101], CB_GETCURSEL, 0, 0) as _,
            date: NaiveDate::from_ymd_opt(date.wYear as _, date.wMonth as _, date.wDay as _)
                .context("日期无效")?,
            time: NaiveTime::from_hms_opt(time.wHour as _, time.wMinute as _, 0)
                .context("时间无效")?,
            duration: text(editor, 104).parse().context("时长必须为分钟数")?,
            lunar_month: (SendMessageW(editor.fields[&105], CB_GETCURSEL, 0, 0) + 1) as _,
            lunar_day: (SendMessageW(editor.fields[&106], CB_GETCURSEL, 0, 0) + 1) as _,
            lunar_leap: checked(107),
            all_day: checked(108),
            location: text(editor, 109),
            url: text(editor, 110),
            notes: text(editor, 111),
            reminders,
            force: checked(118),
            completed: checked(119),
        }
        .build(editor.source.as_ref())
    }
}
unsafe extern "system" fn editor_proc(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if ultra_bar_native_ui::theme::handle_message(hwnd, message, w) {
            return 0;
        }
        if message == WM_ERASEBKGND {
            let mut rect: RECT = zeroed();
            GetClientRect(hwnd, &mut rect);
            FillRect(w as HDC, &rect, background());
            return 1;
        }
        if message == WM_NCCREATE {
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                (*(l as *const CREATESTRUCTW)).lpCreateParams as _,
            );
        }
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Editor;
        if pointer.is_null() {
            return DefWindowProcW(hwnd, message, w, l);
        }
        let editor = &mut *pointer;
        match message {
            WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX | WM_CTLCOLORBTN => {
                control_color(message, w, l)
            }
            WM_DRAWITEM => {
                draw_button(&*(l as *const DRAWITEMSTRUCT), editor);
                1
            }
            WM_NOTIFY => {
                let header = &*(l as *const NMHDR);
                if header.code == DTN_DROPDOWN {
                    let calendar = SendMessageW(header.hwndFrom, DTM_GETMONTHCAL, 0, 0) as HWND;
                    SetWindowTheme(calendar, wide("").as_ptr(), wide("").as_ptr());
                    SetWindowSubclass(calendar, Some(popup_proc), 9, 0);
                    PostMessageW(calendar, PREPARE_POPUP, 0, 0);
                    for (index, color) in [
                        (
                            MCSC_BACKGROUND,
                            ultra_bar_native_ui::theme::palette().background,
                        ),
                        (MCSC_MONTHBK, ultra_bar_native_ui::theme::palette().field),
                        (MCSC_TEXT, ultra_bar_native_ui::theme::palette().text),
                        (MCSC_TITLEBK, ultra_bar_native_ui::theme::palette().field),
                        (MCSC_TITLETEXT, ultra_bar_native_ui::theme::palette().text),
                        (
                            MCSC_TRAILINGTEXT,
                            ultra_bar_native_ui::theme::palette().muted,
                        ),
                    ] {
                        SendMessageW(calendar, MCM_SETCOLOR, index as _, color as _);
                    }
                }
                0
            }
            WM_NCHITTEST => {
                let mut point = POINT {
                    x: l as u16 as i16 as i32,
                    y: (l >> 16) as u16 as i16 as i32,
                };
                ScreenToClient(hwnd, &mut point);
                let mut rect: RECT = zeroed();
                GetClientRect(hwnd, &mut rect);
                if point.y < 48 * (*editor.app).dpi / 96
                    && point.x < rect.right - 54 * (*editor.app).dpi / 96
                {
                    HTCAPTION as _
                } else {
                    DefWindowProcW(hwnd, message, w, l)
                }
            }
            DM_GETDEFID => ((DC_HASDEFID as usize) << 16 | 1) as _,
            WM_COMMAND => {
                let id = (w & 0xffff) as i32;
                let notification = (w >> 16) as u32;
                if notification == CBN_DROPDOWN {
                    prepare_combo_popup(l as HWND);
                    return 0;
                }
                if id == 1 {
                    let result = draft(editor).and_then(|event| {
                        let app = &mut *editor.app;
                        app.selected_event = Some(event.id.clone());
                        app.repo.upsert(event)
                    });
                    if result.is_ok() {
                        DestroyWindow(hwnd);
                    } else {
                        ui::error(hwnd, result);
                    }
                } else if id == 2 || id == 4 {
                    DestroyWindow(hwnd);
                } else if id == 3 {
                    if let Some(source) = &editor.source
                        && MessageBoxW(
                            hwnd,
                            wide("删除这条日程？").as_ptr(),
                            wide("删除日程").as_ptr(),
                            MB_YESNO | MB_DEFBUTTON2,
                        ) == IDYES
                    {
                        let result = (*editor.app).repo.remove(&source.id);
                        if result.is_ok() {
                            DestroyWindow(hwnd);
                        } else {
                            ui::error(hwnd, result);
                        }
                    }
                } else if (id == 101 && notification == CBN_SELCHANGE)
                    || (id == 108 && notification == BN_CLICKED)
                {
                    update_fields(editor);
                }
                0
            }
            WM_CLOSE => {
                DestroyWindow(hwnd);
                0
            }
            WM_MOUSEWHEEL => {
                SendMessageW(editor.form, message, w, l);
                0
            }
            WM_DESTROY => {
                if !editor.icon_font.is_null() {
                    DeleteObject(editor.icon_font);
                    editor.icon_font = null_mut();
                }
                let app = &mut *editor.app;
                EnableWindow(app.hwnd, 1);
                InvalidateRect(app.hwnd, null(), 0);
                if IsWindowVisible(app.hwnd) != 0 {
                    SetForegroundWindow(app.hwnd);
                }
                0
            }
            _ => DefWindowProcW(hwnd, message, w, l),
        }
    }
}
unsafe extern "system" fn form_proc(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if ultra_bar_native_ui::theme::handle_message(hwnd, message, w) {
            return 0;
        }
        if message == WM_ERASEBKGND {
            let mut rect: RECT = zeroed();
            GetClientRect(hwnd, &mut rect);
            FillRect(w as HDC, &rect, background());
            return 1;
        }
        if message == WM_NCCREATE {
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                (*(l as *const CREATESTRUCTW)).lpCreateParams as _,
            );
        }
        let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Editor;
        if pointer.is_null() {
            return DefWindowProcW(hwnd, message, w, l);
        }
        let editor = &mut *pointer;
        match message {
            WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX | WM_CTLCOLORBTN => {
                control_color(message, w, l)
            }
            WM_COMMAND | WM_NOTIFY => SendMessageW(GetParent(hwnd), message, w, l),
            WM_MOUSEWHEEL | WM_VSCROLL => {
                let mut info: SCROLLINFO = zeroed();
                info.cbSize = size_of::<SCROLLINFO>() as _;
                info.fMask = SIF_ALL;
                GetScrollInfo(hwnd, SB_VERT, &mut info);
                let line = 36 * (*editor.app).dpi / 96;
                let requested = if message == WM_MOUSEWHEEL {
                    editor.scroll
                        - if (w >> 16) as u16 as i16 > 0 {
                            line * 2
                        } else {
                            -line * 2
                        }
                } else {
                    match (w & 0xffff) as i32 {
                        SB_LINEUP => editor.scroll - line,
                        SB_LINEDOWN => editor.scroll + line,
                        SB_PAGEUP => editor.scroll - info.nPage as i32,
                        SB_PAGEDOWN => editor.scroll + info.nPage as i32,
                        SB_THUMBTRACK => info.nTrackPos,
                        SB_TOP => 0,
                        SB_BOTTOM => info.nMax,
                        _ => editor.scroll,
                    }
                };
                let next = requested.clamp(0, (info.nMax - info.nPage as i32 + 1).max(0));
                let delta = editor.scroll - next;
                editor.scroll = next;
                info.fMask = SIF_POS;
                info.nPos = next;
                SetScrollInfo(hwnd, SB_VERT, &info, 1);
                ScrollWindowEx(
                    hwnd,
                    0,
                    delta,
                    null(),
                    null(),
                    null_mut(),
                    null_mut(),
                    SW_SCROLLCHILDREN | SW_INVALIDATE | SW_ERASE,
                );
                0
            }
            _ => DefWindowProcW(hwnd, message, w, l),
        }
    }
}

unsafe fn draw_button(item: &DRAWITEMSTRUCT, editor: &Editor) {
    unsafe {
        let dc = item.hDC;
        let old = SelectObject(dc, editor.icon_font);
        let id = GetDlgCtrlID(item.hwndItem);
        let brush = CreateSolidBrush(if id == 1 {
            ultra_bar_native_ui::theme::palette().selection
        } else {
            ultra_bar_native_ui::theme::palette().field
        });
        let old_brush = SelectObject(dc, brush);
        let pen = CreatePen(
            PS_SOLID,
            1,
            if item.itemState & ODS_FOCUS != 0 {
                ultra_bar_native_ui::theme::palette().active_border
            } else {
                ultra_bar_native_ui::theme::palette().field
            },
        );
        let old_pen = SelectObject(dc, pen);
        let radius = 12 * (*editor.app).dpi / 96;
        RoundRect(
            dc,
            item.rcItem.left,
            item.rcItem.top,
            item.rcItem.right,
            item.rcItem.bottom,
            radius,
            radius,
        );
        SelectObject(dc, old_pen);
        SelectObject(dc, old_brush);
        DeleteObject(pen);
        DeleteObject(brush);
        SetBkMode(dc, TRANSPARENT as _);
        SetTextColor(
            dc,
            if id == 3 {
                0x00a4a9f3
            } else {
                ultra_bar_native_ui::theme::palette().text
            },
        );
        let glyph = match id {
            1 => "\u{e74e}",
            2 => "\u{e711}",
            3 => "\u{e74d}",
            _ => "\u{e8bb}",
        };
        let mut rect = item.rcItem;
        DrawTextW(
            dc,
            wide(glyph).as_ptr(),
            -1,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        SelectObject(dc, old);
    }
}

unsafe fn paint_control(hwnd: HWND, dc: HDC, dpi: i32, kind: u8) {
    unsafe {
        let mut rect: RECT = zeroed();
        GetClientRect(hwnd, &mut rect);
        let unit = |n: i32| n * dpi / 96;
        FillRect(dc, &rect, background());
        let old_font = SelectObject(dc, SendMessageW(hwnd, WM_GETFONT, 0, 0) as HGDIOBJ);
        SetBkMode(dc, TRANSPARENT as _);
        SetTextColor(dc, ultra_bar_native_ui::theme::palette().text);
        let mut caption = [0u16; 512];
        let length = GetWindowTextW(hwnd, caption.as_mut_ptr(), caption.len() as _);
        let mut text_rect = rect;
        if kind == 0 {
            let size = unit(16);
            let top = (rect.bottom - size) / 2;
            let box_rect = RECT {
                left: unit(1),
                top,
                right: unit(1) + size,
                bottom: top + size,
            };
            let brush = CreateSolidBrush(
                if SendMessageW(hwnd, BM_GETCHECK, 0, 0) == BST_CHECKED as isize {
                    ultra_bar_native_ui::theme::palette().active_fill
                } else {
                    ultra_bar_native_ui::theme::palette().field
                },
            );
            FillRect(dc, &box_rect, brush);
            DeleteObject(brush);
            if SendMessageW(hwnd, BM_GETCHECK, 0, 0) == BST_CHECKED as isize {
                let pen = CreatePen(
                    PS_SOLID,
                    unit(2).max(1),
                    ultra_bar_native_ui::theme::palette().text,
                );
                let old = SelectObject(dc, pen);
                MoveToEx(dc, unit(4), top + unit(8), null_mut());
                LineTo(dc, unit(7), top + unit(11));
                LineTo(dc, unit(13), top + unit(4));
                SelectObject(dc, old);
                DeleteObject(pen);
            }
            text_rect.left = unit(24);
            DrawTextW(
                dc,
                caption.as_ptr(),
                length,
                &mut text_rect,
                DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
            );
        } else {
            let old_brush = SelectObject(dc, field_brush());
            let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
            RoundRect(dc, 0, 0, rect.right, rect.bottom, unit(10), unit(10));
            SelectObject(dc, old_pen);
            SelectObject(dc, old_brush);
            let value = if kind == 2 {
                let mut time: SYSTEMTIME = zeroed();
                SendMessageW(
                    hwnd,
                    DTM_GETSYSTEMTIME,
                    0,
                    (&mut time as *mut SYSTEMTIME) as _,
                );
                if GetDlgCtrlID(hwnd) == 103 {
                    format!("{:02}:{:02}", time.wHour, time.wMinute)
                } else {
                    format!("{:04}-{:02}-{:02}", time.wYear, time.wMonth, time.wDay)
                }
            } else {
                String::from_utf16_lossy(&caption[..length.max(0) as usize])
            };
            text_rect.left = unit(8);
            text_rect.right -= unit(26);
            DrawTextW(
                dc,
                wide(&value).as_ptr(),
                -1,
                &mut text_rect,
                DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
            if GetDlgCtrlID(hwnd) != 103 {
                let points = [
                    POINT {
                        x: rect.right - unit(18),
                        y: rect.bottom / 2 - unit(2),
                    },
                    POINT {
                        x: rect.right - unit(10),
                        y: rect.bottom / 2 - unit(2),
                    },
                    POINT {
                        x: rect.right - unit(14),
                        y: rect.bottom / 2 + unit(3),
                    },
                ];
                let brush = CreateSolidBrush(ultra_bar_native_ui::theme::palette().muted);
                let old_brush = SelectObject(dc, brush);
                let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
                Polygon(dc, points.as_ptr(), 3);
                SelectObject(dc, old_pen);
                SelectObject(dc, old_brush);
                DeleteObject(brush);
            }
        }
        if GetFocus() == hwnd {
            let mut focus = rect;
            InflateRect(&mut focus, -unit(2), -unit(2));
            DrawFocusRect(dc, &focus);
        }
        SelectObject(dc, old_font);
    }
}

unsafe fn themed_control(
    hwnd: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    dpi: usize,
    kind: u8,
) -> LRESULT {
    unsafe {
        if message == WM_NCDESTROY {
            let callback: SUBCLASSPROC = match kind {
                0 => Some(check_proc),
                1 => Some(combo_proc),
                _ => Some(date_proc),
            };
            RemoveWindowSubclass(hwnd, callback, id);
            return DefSubclassProc(hwnd, message, w, l);
        }
        if kind == 2
            && GetFocus() == hwnd
            && (message == WM_PAINT || message == WM_PRINTCLIENT || message == WM_PRINT)
        {
            return DefSubclassProc(hwnd, message, w, l);
        }
        if message == WM_PAINT {
            let mut ps: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            paint_control(hwnd, dc, dpi as _, kind);
            EndPaint(hwnd, &ps);
            return 0;
        }
        if message == WM_PRINT || message == WM_PRINTCLIENT {
            paint_control(hwnd, w as _, dpi as _, kind);
            return 0;
        }
        let result = DefSubclassProc(hwnd, message, w, l);
        if matches!(
            message,
            BM_SETCHECK | CB_SETCURSEL | WM_SETFOCUS | WM_KILLFOCUS | WM_LBUTTONUP | WM_KEYUP
        ) {
            InvalidateRect(hwnd, null(), 0);
        }
        result
    }
}
unsafe extern "system" fn check_proc(
    hwnd: HWND,
    m: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    unsafe { themed_control(hwnd, m, w, l, id, data, 0) }
}
unsafe extern "system" fn combo_proc(
    hwnd: HWND,
    m: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    unsafe { themed_control(hwnd, m, w, l, id, data, 1) }
}
unsafe extern "system" fn date_proc(
    hwnd: HWND,
    m: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    unsafe { themed_control(hwnd, m, w, l, id, data, 2) }
}

pub unsafe fn smoke(app: &mut App) -> Result<()> {
    unsafe {
        open(app, None);
        let editor = app
            .editor
            .as_ref()
            .context("native editor was not created")?;
        anyhow::ensure!(
            GetWindowLongPtrW(editor.hwnd, GWL_EXSTYLE) & WS_EX_DLGMODALFRAME as isize == 0,
            "unexpected system dialog frame"
        );
        anyhow::ensure!(
            GetWindowLongPtrW(editor.hwnd, GWL_STYLE) & ((WS_BORDER | WS_DLGFRAME) as isize) == 0,
            "unexpected nonclient border"
        );
        let mut parent_bounds: RECT = zeroed();
        GetClientRect(editor.hwnd, &mut parent_bounds);
        for id in [1, 2] {
            let mut bounds: RECT = zeroed();
            GetWindowRect(editor.fields[&id], &mut bounds);
            MapWindowPoints(
                null_mut(),
                editor.hwnd,
                (&mut bounds as *mut RECT).cast(),
                2,
            );
            anyhow::ensure!(
                bounds.top >= 0 && bounds.bottom <= parent_bounds.bottom,
                "editor footer is clipped"
            );
        }
        anyhow::ensure!(
            editor.fields.values().all(|field| !field.is_null()),
            "native editor control creation failed"
        );
        SetWindowTextW(editor.fields[&100], wide("日历编辑器验证").as_ptr());
        for kind in 0..6 {
            SendMessageW(editor.fields[&101], CB_SETCURSEL, kind, 0);
            update_fields(editor);
            let event = draft(editor)?;
            anyhow::ensure!(
                calendar::schedule_kind(&event).index() == kind as i32,
                "editor recurrence rule mismatch"
            );
        }
        SendMessageW(editor.fields[&101], CB_SETCURSEL, 4, 0);
        update_fields(editor);
        if let Some(directory) = std::env::var_os("ULTRA_BAR_CALENDAR_SMOKE_DIR") {
            let style = GetWindowLongPtrW(editor.hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(
                editor.hwnd,
                GWL_EXSTYLE,
                style | WS_EX_TOOLWINDOW as isize | WS_EX_NOACTIVATE as isize,
            );
            SetWindowPos(
                editor.hwnd,
                null_mut(),
                -32000,
                -32000,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            ShowWindow(editor.hwnd, SW_SHOWNOACTIVATE);
            UpdateWindow(editor.hwnd);
            let mut rect: RECT = zeroed();
            GetClientRect(editor.hwnd, &mut rect);
            if let Some(surface) = ultra_bar_native_ui::Surface::new(rect.right, rect.bottom) {
                let printed = PrintWindow(editor.hwnd, surface.dc, 1);
                anyhow::ensure!(printed != 0, "native editor capture failed");
                surface
                    .save_bmp(&std::path::PathBuf::from(directory).join("calendar-editor.bmp"))?;
            }
        }
        if std::env::var_os("ULTRA_BAR_CALENDAR_DROPDOWN_SMOKE").is_some() {
            dropdown_smoke(editor)?;
        }
        let mut bounds: RECT = zeroed();
        GetClientRect(editor.form, &mut bounds);
        let short_height = 180 * app.dpi / 96;
        MoveWindow(editor.form, 0, 0, bounds.right, short_height, 0);
        let info = SCROLLINFO {
            cbSize: size_of::<SCROLLINFO>() as _,
            fMask: SIF_PAGE | SIF_RANGE,
            nMin: 0,
            nMax: editor.total,
            nPage: short_height as _,
            ..zeroed()
        };
        SetScrollInfo(editor.form, SB_VERT, &info, 1);
        SendMessageW(editor.form, WM_VSCROLL, SB_BOTTOM as _, 0);
        anyhow::ensure!(
            app.editor.as_ref().unwrap().scroll > 0,
            "editor form did not scroll"
        );
        DestroyWindow(editor.hwnd);
        Ok(())
    }
}
unsafe fn dropdown_smoke(editor: &Editor) -> Result<()> {
    unsafe {
        let directory = std::path::PathBuf::from(
            std::env::var_os("ULTRA_BAR_CALENDAR_SMOKE_DIR")
                .context("Missing dropdown output directory")?,
        );
        // The isolated smoke instance only. Never touches the user's editor or sends global input.
        SetWindowPos(
            editor.hwnd,
            HWND_TOPMOST,
            100,
            80,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        UpdateWindow(editor.hwnd);
        let pump = |milliseconds: u64| {
            let end = std::time::Instant::now() + std::time::Duration::from_millis(milliseconds);
            while std::time::Instant::now() < end {
                let mut message: MSG = zeroed();
                while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        };
        let mut report = vec![];
        for id in [101, 105, 106] {
            let combo = editor.fields[&id];
            let initial = SendMessageW(combo, CB_GETCURSEL, 0, 0);
            SendMessageW(combo, CB_SHOWDROPDOWN, 1, 0);
            pump(100);
            let mut info: COMBOBOXINFO = zeroed();
            info.cbSize = size_of::<COMBOBOXINFO>() as _;
            anyhow::ensure!(
                GetComboBoxInfo(combo, &mut info) != 0 && !info.hwndList.is_null(),
                "Cannot obtain dropdown list"
            );
            let list = info.hwndList;
            let capture = |stage: &str| -> Result<Vec<u32>> {
                windows_sys::Win32::Graphics::Dwm::DwmFlush();
                let mut bounds: RECT = zeroed();
                GetWindowRect(list, &mut bounds);
                let (width, height) = (bounds.right - bounds.left, bounds.bottom - bounds.top);
                let surface = ultra_bar_native_ui::Surface::new(width, height)
                    .context("Cannot capture dropdown")?;
                let screen = GetDC(null_mut());
                let result = BitBlt(
                    surface.dc,
                    0,
                    0,
                    width,
                    height,
                    screen,
                    bounds.left,
                    bounds.top,
                    SRCCOPY | CAPTUREBLT,
                );
                ReleaseDC(null_mut(), screen);
                anyhow::ensure!(result != 0, "Dropdown capture failed");
                surface.save_bmp(&directory.join(format!("dropdown-{id}-{stage}.bmp")))?;
                Ok(surface.copy_pixels())
            };
            let before = capture("initial")?;
            pump(650);
            let after = capture("settled")?;
            let ex = GetWindowLongPtrW(list, GWL_EXSTYLE);
            let mut bounds: RECT = zeroed();
            GetWindowRect(list, &mut bounds);
            let top = WindowFromPoint(POINT {
                x: (bounds.left + bounds.right) / 2,
                y: (bounds.top + bounds.bottom) / 2,
            });
            let opaque = ex & (WS_EX_LAYERED | WS_EX_TRANSPARENT) as isize == 0;
            let above = top == list || IsChild(list, top) != 0;
            let changed = before.iter().zip(&after).filter(|(a, b)| a != b).count();
            report.push(serde_json::json!({"control":id,"opaque":opaque,"above_editor":above,"changed_pixels_after_settle":changed}));
            anyhow::ensure!(
                opaque && above,
                "Dropdown was composited behind its glass editor"
            );
            anyhow::ensure!(
                changed < before.len() / 20 + 1,
                "Dropdown changed after opening instead of remaining stable"
            );
            // Exercise the real native selection path without moving the desktop keyboard or mouse.
            SendMessageW(combo, WM_KEYDOWN, VK_DOWN as usize, 0);
            SendMessageW(combo, WM_KEYUP, VK_DOWN as usize, 0);
            SendMessageW(combo, WM_KEYDOWN, VK_RETURN as usize, 0);
            SendMessageW(combo, WM_KEYUP, VK_RETURN as usize, 0);
            SendMessageW(combo, CB_SHOWDROPDOWN, 0, 0);
            anyhow::ensure!(
                SendMessageW(combo, CB_GETCURSEL, 0, 0) != initial,
                "Native dropdown keyboard selection failed"
            );
            SendMessageW(combo, CB_SETCURSEL, initial as usize, 0);
            update_fields(editor);
        }
        std::fs::write(
            directory.join("dropdown-checks.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;
        ShowWindow(editor.hwnd, SW_HIDE);
        Ok(())
    }
}
