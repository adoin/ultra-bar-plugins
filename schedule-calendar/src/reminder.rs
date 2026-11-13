use crate::ui::App;
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
#[derive(Debug, PartialEq)]
enum FooterAction {
    Link,
    Acknowledge,
}
fn footer_action(x: i32, y: i32, height: i32, dpi: i32, has_link: bool) -> Option<FooterAction> {
    let u = |n| n * dpi / 96;
    if y < height - u(42) || y >= height - u(10) {
        return None;
    }
    if (u(240)..u(364)).contains(&x) {
        Some(FooterAction::Acknowledge)
    } else if has_link && (u(16)..u(140)).contains(&x) {
        Some(FooterAction::Link)
    } else {
        None
    }
}
pub unsafe fn show(app: &mut App) {
    unsafe {
        let Some(pending) = app
            .repo
            .data
            .pending
            .iter()
            .find(|p| p.event.force_reminder)
            .or_else(|| app.repo.data.pending.first())
        else {
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
            null_mut(),
            null_mut(),
            module,
            (&mut *state as *mut Reminder).cast(),
        );
        if hwnd.is_null() {
            ultra_bar_native_ui::diagnostics::event(
                "calendar",
                "reminder.window_failed",
                serde_json::json!({"key":state.key,"error":GetLastError()}),
            );
            return;
        }
        state.hwnd = hwnd;
        state.glass = ultra_bar_native_ui::enable_glass(hwnd);
        ultra_bar_native_ui::geometry::attach_rounded(hwnd, 20);
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
            if app.smoke {
                -32000
            } else {
                info.rcWork.right - width - 8
            },
            if app.smoke {
                -32000
            } else {
                info.rcWork.bottom - height - 8
            },
            width,
            height,
            SWP_NOACTIVATE,
        );
        app.reminder = Some(state);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        InvalidateRect(hwnd, null(), 0);
        ultra_bar_native_ui::diagnostics::event(
            "calendar",
            "reminder.window_shown",
            serde_json::json!({"key":app.reminder.as_ref().unwrap().key,"force":force,"visible":IsWindowVisible(hwnd)!=0,"hwnd":hwnd as usize}),
        );
        if !app.smoke && app.region.notification_sound_enabled {
            crate::notifications::sound();
        }
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
                ultra_bar_native_ui::fonts::delete_object(brush);
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
                            .with_timezone(&crate::region::timezone())
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
                        ultra_bar_native_ui::fonts::delete_object(brush);
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
                let url = app
                    .repo
                    .data
                    .pending
                    .iter()
                    .find(|pending| pending.key == state.key)
                    .map(|pending| pending.event.notification_url.clone())
                    .unwrap_or_default();
                match footer_action(x, y, rect.bottom, app.dpi, !url.is_empty()) {
                    Some(FooterAction::Acknowledge) => acknowledge(state),
                    Some(FooterAction::Link)
                        if url::Url::parse(&url).is_ok_and(|url| {
                            matches!(url.scheme(), "https" | "http") && url.host_str().is_some()
                        }) =>
                    {
                        let result = ShellExecuteW(
                            hwnd,
                            wide("open").as_ptr(),
                            wide(&url).as_ptr(),
                            null(),
                            null(),
                            SW_SHOWNORMAL,
                        );
                        let opened = result as usize > 32;
                        ultra_bar_native_ui::diagnostics::event(
                            "calendar",
                            "reminder.link_opened",
                            serde_json::json!({"key":state.key,"ok":opened,"shell_result":result as usize}),
                        );
                        if opened {
                            acknowledge(state);
                        }
                    }
                    _ => {}
                }
                0
            }
            WM_TIMER => {
                if w == 1
                    && app
                        .repo
                        .data
                        .pending
                        .iter()
                        .find(|pending| pending.key == state.key)
                        .is_some_and(|pending| !pending.event.force_reminder)
                {
                    acknowledge(state);
                }
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
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_real_footer_buttons_acknowledge_or_launch() {
        for dpi in [96, 144, 192] {
            let u = |n| n * dpi / 96;
            let h = u(190);
            assert_eq!(
                footer_action(u(20), h - u(20), h, dpi, true),
                Some(FooterAction::Link)
            );
            assert_eq!(
                footer_action(u(260), h - u(20), h, dpi, true),
                Some(FooterAction::Acknowledge)
            );
            for (x, y) in [(200, 170), (20, 100), (375, 170), (260, 185)] {
                assert_eq!(footer_action(u(x), u(y), h, dpi, true), None);
            }
            assert_eq!(footer_action(u(20), h - u(20), h, dpi, false), None);
        }
    }
}
