use crate::ui::App;
use chrono_tz::Asia::Shanghai;
use std::{
    mem::{size_of, zeroed},
    ptr::{null, null_mut},
};
use ultra_bar_native_ui::text::draw_text as DrawTextW;
use ultra_bar_native_ui::{Surface, wide};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::LibraryLoader::*,
    UI::{Shell::*, WindowsAndMessaging::*},
};
pub struct Reminder {
    pub hwnd: HWND,
    app: *mut App,
    key: String,
    glass: bool,
}
pub unsafe fn show(app: &mut App) {
    unsafe {
        let Some(pending) = app.repo.data.pending.first() else {
            return;
        };
        let key = pending.key.clone();
        let force = pending.event.force_reminder;
        let class = wide("UltraBar.Calendar.Reminder");
        let module = GetModuleHandleW(null());
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: module,
            lpszClassName: class.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            ..zeroed()
        });
        let mut state = Box::new(Reminder {
            hwnd: null_mut(),
            app: app as *mut App,
            key,
            glass: false,
        });
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_LAYERED,
            class.as_ptr(),
            wide("日程提醒").as_ptr(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            app.hwnd,
            null_mut(),
            module,
            (&mut *state as *mut Reminder).cast(),
        );
        state.hwnd = hwnd;
        state.glass = ultra_bar_native_ui::enable_glass(hwnd);
        let mut info: MONITORINFO = zeroed();
        info.cbSize = size_of::<MONITORINFO>() as _;
        GetMonitorInfoW(
            MonitorFromWindow(app.owner, MONITOR_DEFAULTTOPRIMARY),
            &mut info,
        );
        let width = 380 * app.dpi / 96;
        let height = 190 * app.dpi / 96;
        SetWindowPos(
            hwnd,
            HWND_TOPMOST,
            info.rcWork.right - width - 8,
            info.rcWork.bottom - height - 8,
            width,
            height,
            SWP_NOACTIVATE,
        );
        app.reminder = Some(state);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        InvalidateRect(hwnd, null(), 0);
        if !force {
            SetTimer(hwnd, 1, 20000, None);
        }
    }
}
unsafe fn acknowledge(state: &mut Reminder) {
    unsafe {
        let app = &mut *state.app;
        let result = app.repo.acknowledge(&state.key);
        if result.is_ok() {
            DestroyWindow(state.hwnd);
        } else {
            crate::ui::error(state.hwnd, result);
        }
    }
}
unsafe extern "system" fn wndproc(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                (*(l as *const CREATESTRUCTW)).lpCreateParams as _,
            );
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Reminder;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, message, w, l);
        }
        let state = &mut *ptr;
        let app = &mut *state.app;
        match message {
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = zeroed();
                BeginPaint(hwnd, &mut ps);
                let mut area: RECT = zeroed();
                GetClientRect(hwnd, &mut area);
                let Some(surface) = Surface::new(area.right, area.bottom) else {
                    EndPaint(hwnd, &ps);
                    return 0;
                };
                let dc = surface.dc;
                let old = SelectObject(dc, app.font);
                let brush = CreateSolidBrush(ultra_bar_native_ui::BACKGROUND_BRUSH);
                FillRect(dc, &area, brush);
                DeleteObject(brush);
                SetBkMode(dc, TRANSPARENT as _);
                SetTextColor(dc, 0x00ffffff);
                if let Some(pending) = app
                    .repo
                    .data
                    .pending
                    .iter()
                    .find(|pending| pending.key == state.key)
                {
                    let unit = |n| n * app.dpi / 96;
                    let content = format!(
                        "{}\n{}\n{} · 提前 {} 分钟\n{}",
                        if pending.event.force_reminder {
                            "强制提醒"
                        } else {
                            "日程提醒"
                        },
                        pending.event.title,
                        pending
                            .occurrence
                            .with_timezone(&Shanghai)
                            .format("%m-%d %H:%M"),
                        pending.offset,
                        pending.event.location
                    );
                    let mut rect = RECT {
                        left: unit(16),
                        top: unit(12),
                        right: area.right - unit(16),
                        bottom: area.bottom - unit(52),
                    };
                    DrawTextW(
                        dc,
                        wide(&content).as_ptr(),
                        -1,
                        &mut rect,
                        DT_WORDBREAK | DT_NOPREFIX,
                    );
                    for (text, left, right) in [("已处理", 240, 364), ("打开链接", 16, 140)]
                    {
                        if text == "打开链接" && pending.event.notification_url.is_empty() {
                            continue;
                        }
                        let mut rect = RECT {
                            left: unit(left),
                            top: area.bottom - unit(42),
                            right: unit(right),
                            bottom: area.bottom - unit(10),
                        };
                        let brush = CreateSolidBrush(ultra_bar_native_ui::HOVER_BRUSH);
                        FillRect(dc, &rect, brush);
                        DeleteObject(brush);
                        DrawTextW(
                            dc,
                            wide(text).as_ptr(),
                            -1,
                            &mut rect,
                            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                        );
                    }
                }
                SelectObject(dc, old);
                surface.present(hwnd, state.glass);
                EndPaint(hwnd, &ps);
                0
            }
            WM_LBUTTONUP => {
                let x = l as u16 as i16 as i32;
                let y = (l >> 16) as u16 as i16 as i32;
                let mut rect: RECT = zeroed();
                GetClientRect(hwnd, &mut rect);
                if y >= rect.bottom - 42 * app.dpi / 96 {
                    if x >= 240 * app.dpi / 96 {
                        acknowledge(state);
                    } else if let Some(pending) = app
                        .repo
                        .data
                        .pending
                        .iter()
                        .find(|pending| pending.key == state.key)
                    {
                        let url = &pending.event.notification_url;
                        if url.starts_with("https://") || url.starts_with("http://") {
                            ShellExecuteW(
                                hwnd,
                                wide("open").as_ptr(),
                                wide(url).as_ptr(),
                                null(),
                                null(),
                                SW_SHOWNORMAL,
                            );
                        }
                    }
                }
                0
            }
            WM_TIMER => {
                acknowledge(state);
                0
            }
            WM_CLOSE => {
                if app
                    .repo
                    .data
                    .pending
                    .iter()
                    .find(|pending| pending.key == state.key)
                    .is_none_or(|pending| !pending.event.force_reminder)
                {
                    acknowledge(state);
                }
                0
            }
            WM_DESTROY => {
                KillTimer(hwnd, 1);
                0
            }
            WM_ERASEBKGND => 1,
            _ => DefWindowProcW(hwnd, message, w, l),
        }
    }
}
