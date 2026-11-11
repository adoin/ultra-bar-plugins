//! Everything's documented WM_COPYDATA Unicode query protocol. No DLL, scans, service installation or index writes.
use anyhow::{Result, ensure};
use std::{
    mem::zeroed,
    path::PathBuf,
    ptr::{null, null_mut},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    System::{DataExchange::*, LibraryLoader::*},
    UI::WindowsAndMessaging::*,
};
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn word(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}
fn text(bytes: &[u8], at: usize) -> Option<String> {
    if !at.is_multiple_of(2) || at >= bytes.len() {
        return None;
    }
    let mut value = Vec::new();
    for b in bytes[at..].chunks_exact(2).take(32768) {
        let c = u16::from_le_bytes([b[0], b[1]]);
        if c == 0 {
            return String::from_utf16(&value).ok();
        }
        value.push(c);
    }
    None
}
pub fn parse(bytes: &[u8]) -> Result<(u32, Vec<PathBuf>)> {
    let count =
        word(bytes, 20).ok_or_else(|| anyhow::anyhow!("Incomplete Everything reply"))? as usize;
    ensure!(
        count <= 60 && bytes.len() >= 28 + count * 12,
        "Invalid result count"
    );
    let mut result = Vec::new();
    for i in 0..count {
        let at = 28 + i * 12;
        let name = text(bytes, word(bytes, at + 4).unwrap() as usize)
            .ok_or_else(|| anyhow::anyhow!("Invalid filename"))?;
        let parent = text(bytes, word(bytes, at + 8).unwrap() as usize)
            .ok_or_else(|| anyhow::anyhow!("Invalid directory"))?;
        ensure!(
            !name.contains(['\\', '/']) && name != "..",
            "Invalid filename"
        );
        let path = PathBuf::from(parent).join(name);
        ensure!(path.is_absolute(), "Invalid result path");
        result.push(path);
    }
    Ok((word(bytes, 8).unwrap(), result))
}
struct Reply {
    sender: HWND,
    data: Option<Vec<u8>>,
}
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: usize, l: isize) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                (*(l as *const CREATESTRUCTW)).lpCreateParams as isize,
            );
        }
        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Reply;
        if msg == WM_COPYDATA && !state.is_null() && w == (*state).sender as usize {
            let data = &*(l as *const COPYDATASTRUCT);
            if data.dwData == 0x55425331
                && data.cbData >= 28
                && data.cbData <= 512 * 1024
                && !data.lpData.is_null()
            {
                (*state).data = Some(
                    std::slice::from_raw_parts(data.lpData.cast::<u8>(), data.cbData as usize)
                        .to_vec(),
                );
                return 1;
            }
        }
        DefWindowProcW(hwnd, msg, w, l)
    }
}
pub fn query(search: &str) -> Result<(u32, Vec<PathBuf>)> {
    ensure!(search.len() <= 4096, "搜索内容过长");
    unsafe {
        let mut server = FindWindowW(wide("EVERYTHING_TASKBAR_NOTIFICATION").as_ptr(), null());
        if server.is_null() {
            server = FindWindowW(
                wide("EVERYTHING_TASKBAR_NOTIFICATION_(1.5a)").as_ptr(),
                null(),
            );
        }
        ensure!(!server.is_null(), "请先启动 Everything");
        let class = wide("UltraBar.Everything.Reply.v1");
        let module = GetModuleHandleW(null());
        RegisterClassW(&WNDCLASSW {
            hInstance: module,
            lpszClassName: class.as_ptr(),
            lpfnWndProc: Some(proc),
            ..zeroed()
        });
        let mut state = Reply {
            sender: server,
            data: None,
        };
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            wide("").as_ptr(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            null_mut(),
            null_mut(),
            module,
            (&mut state as *mut Reply).cast(),
        );
        ensure!(!hwnd.is_null(), "Cannot create Everything reply window");
        ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, null_mut());
        let mut packet = Vec::<u8>::new();
        for n in [hwnd as usize as u32, 0x55425331, 0, 0, 60] {
            packet.extend_from_slice(&n.to_le_bytes());
        }
        for c in wide(search) {
            packet.extend_from_slice(&c.to_le_bytes());
        }
        let data = COPYDATASTRUCT {
            dwData: 2,
            cbData: packet.len() as u32,
            lpData: packet.as_mut_ptr().cast(),
        };
        let mut accepted = 0;
        let sent = SendMessageTimeoutW(
            server,
            WM_COPYDATA,
            hwnd as usize,
            (&data as *const COPYDATASTRUCT) as isize,
            SMTO_ABORTIFHUNG,
            250,
            &mut accepted,
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while sent != 0 && accepted != 0 && state.data.is_none() && Instant::now() < deadline {
            let mut message: MSG = zeroed();
            while PeekMessageW(&mut message, hwnd, 0, 0, PM_REMOVE) != 0 {
                DispatchMessageW(&message);
            }
            if state.data.is_none() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        DestroyWindow(hwnd);
        let bytes = state
            .data
            .ok_or_else(|| anyhow::anyhow!("Everything 暂未响应，请确认它已运行且索引就绪"))?;
        parse(&bytes)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_bad_offsets_and_excessive_counts() {
        let mut b = vec![0u8; 40];
        b[20..24].copy_from_slice(&61u32.to_le_bytes());
        assert!(parse(&b).is_err());
        b[20..24].copy_from_slice(&1u32.to_le_bytes());
        b[32..36].copy_from_slice(&999u32.to_le_bytes());
        assert!(parse(&b).is_err());
    }
    #[test]
    fn unicode_offsets_preserve_the_complete_original_path() {
        let name = wide("日程笔记.txt");
        let parent = wide("D:\\资料");
        let mut bytes = vec![0u8; 40];
        bytes[8..12].copy_from_slice(&1u32.to_le_bytes());
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
        bytes[32..36].copy_from_slice(&40u32.to_le_bytes());
        bytes[36..40].copy_from_slice(&(40 + name.len() as u32 * 2).to_le_bytes());
        for c in name.into_iter().chain(parent) {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
        let (total, paths) = parse(&bytes).unwrap();
        assert_eq!(total, 1);
        assert_eq!(paths, vec![PathBuf::from("D:\\资料\\日程笔记.txt")]);
    }
}
