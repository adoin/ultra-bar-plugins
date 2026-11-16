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
    id: u32,
    data: Option<Vec<u8>>,
}
impl Reply {
    fn accepts(&self, sender: usize, id: usize, size: u32) -> bool {
        sender == self.sender as usize
            && id == self.id as usize
            && (28..=512 * 1024).contains(&size)
    }
}
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: usize, l: isize) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE && l != 0 {
            SetWindowLongPtrW(
                hwnd,
                GWLP_USERDATA,
                (*(l as *const CREATESTRUCTW)).lpCreateParams as isize,
            );
        }
        let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Reply;
        if msg == WM_COPYDATA && l != 0 && !state.is_null() {
            let data = &*(l as *const COPYDATASTRUCT);
            if (*state).accepts(w, data.dwData, data.cbData) && !data.lpData.is_null() {
                (*state).data = Some(
                    std::slice::from_raw_parts(data.lpData.cast::<u8>(), data.cbData as usize)
                        .to_vec(),
                );
                return 1;
            }
        }
        if msg == WM_NCDESTROY {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        }
        DefWindowProcW(hwnd, msg, w, l)
    }
}
static REPLY_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x55425331);
fn next_id() -> u32 {
    REPLY_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// One receive window per active search burst. Everything uses this window's
/// identity to replace an unfinished query; separate windows cannot cancel it.
pub struct Client {
    // SendMessage can synchronously invoke proc; callback mutation must not
    // alias a normal Rust reference held across the native call.
    reply: Box<std::cell::UnsafeCell<Reply>>,
    hwnd: HWND,
    started: Option<Instant>,
}
impl Client {
    pub fn new() -> Self {
        Self {
            reply: Box::new(std::cell::UnsafeCell::new(Reply {
                sender: null_mut(),
                id: 0,
                data: None,
            })),
            hwnd: null_mut(),
            started: None,
        }
    }
    pub fn start(&mut self, search: &str) -> Result<()> {
        let result = self.begin(search);
        if result.is_err() {
            self.close();
        }
        result
    }
    fn begin(&mut self, search: &str) -> Result<()> {
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
            if self.hwnd.is_null() {
                let class = wide("UltraBar.Everything.Reply.v1");
                let module = GetModuleHandleW(null());
                RegisterClassW(&WNDCLASSW {
                    hInstance: module,
                    lpszClassName: class.as_ptr(),
                    lpfnWndProc: Some(proc),
                    ..zeroed()
                });
                self.hwnd = CreateWindowExW(
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
                    self.reply.get().cast(),
                );
                ensure!(!self.hwnd.is_null(), "无法建立 Everything 查询连接");
                ChangeWindowMessageFilterEx(self.hwnd, WM_COPYDATA, MSGFLT_ALLOW, null_mut());
            }
            {
                let state = &mut *self.reply.get();
                state.sender = server;
                state.id = next_id();
                state.data = None;
            }
            self.started = Some(Instant::now());
            self.send(search, 60, 1000)?;
        }
        Ok(())
    }
    unsafe fn send(&self, search: &str, max: u32, timeout: u32) -> Result<()> {
        unsafe {
            let (server, id) = {
                let state = &*self.reply.get();
                (state.sender, state.id)
            };
            let mut packet = Vec::new();
            for n in [self.hwnd as usize as u32, id, 0, 0, max] {
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
            SetLastError(0);
            let sent = SendMessageTimeoutW(
                server,
                WM_COPYDATA,
                self.hwnd as usize,
                (&data as *const COPYDATASTRUCT) as isize,
                SMTO_ABORTIFHUNG,
                timeout,
                &mut accepted,
            );
            let error = GetLastError();
            if sent == 0 || accepted == 0 {
                ultra_bar_native_ui::diagnostics::event(
                    "search",
                    "ipc.send_failed",
                    serde_json::json!({"sent":sent!=0,"accepted":accepted,"win32_error":error,"query_chars":search.chars().count()}),
                );
                if error == ERROR_ACCESS_DENIED {
                    anyhow::bail!("Everything 通信被权限限制");
                }
                if sent == 0 {
                    anyhow::bail!("Everything 正忙，请稍后重试");
                }
                anyhow::bail!("Everything 暂未接受查询");
            }
            Ok(())
        }
    }
    pub fn poll(&mut self) -> Option<Result<(u32, Vec<PathBuf>)>> {
        let started = self.started?;
        if !self.pump() {
            self.cancel();
            return Some(Err(anyhow::anyhow!("查询已取消")));
        }
        unsafe {
            let data = (*self.reply.get()).data.take();
            if let Some(bytes) = data {
                self.close();
                return Some(parse(&bytes));
            }
        }
        if started.elapsed() >= Duration::from_secs(8) {
            ultra_bar_native_ui::diagnostics::event(
                "search",
                "ipc.reply_timeout",
                serde_json::json!({"elapsed_ms":started.elapsed().as_millis()}),
            );
            self.cancel();
            return Some(Err(anyhow::anyhow!("等待 Everything 搜索结果超时")));
        }
        None
    }
    fn pump(&self) -> bool {
        unsafe {
            let mut message: MSG = zeroed();
            while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                if message.message == WM_QUIT {
                    return false;
                }
                DispatchMessageW(&message);
            }
        }
        true
    }
    pub fn cancel(&mut self) {
        if self.started.is_some() && !self.hwnd.is_null() {
            unsafe {
                (*self.reply.get()).id = next_id();
                (*self.reply.get()).data = None;
                // This also matches nothing on Everything 1.4. Keep the same
                // receive window so the server can cancel the previous query.
                let _ = self.send("nothing:", 0, 100);
            }
        }
        self.close();
    }
    fn close(&mut self) {
        unsafe {
            if !self.hwnd.is_null() {
                DestroyWindow(self.hwnd);
                self.hwnd = null_mut();
            }
            (*self.reply.get()).data = None;
        }
        self.started = None;
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.cancel();
    }
}
pub fn query(search: &str) -> Result<(u32, Vec<PathBuf>)> {
    let mut client = Client::new();
    client.start(search)?;
    loop {
        if let Some(result) = client.poll() {
            return result;
        }
        unsafe {
            MsgWaitForMultipleObjectsEx(0, null(), 20, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        }
    }
}
pub fn probe_latest(searches: &[String]) -> Result<serde_json::Value> {
    ensure!(!searches.is_empty(), "Missing probe queries");
    let started = Instant::now();
    let mut client = Client::new();
    let mut windows = std::collections::HashSet::new();
    let mut ids = std::collections::HashSet::new();
    for search in searches {
        client.start(search)?;
        windows.insert(client.hwnd as usize);
        unsafe {
            ids.insert((*client.reply.get()).id);
        }
        let next = Instant::now() + Duration::from_millis(80);
        while Instant::now() < next {
            ensure!(client.pump(), "Probe interrupted");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let (total, paths) = loop {
        if let Some(result) = client.poll() {
            break result?;
        }
        unsafe {
            MsgWaitForMultipleObjectsEx(0, null(), 20, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
        }
    };
    ensure!(
        windows.len() == 1 && ids.len() == searches.len(),
        "Search replacement did not preserve its receive window/correlation IDs"
    );
    Ok(
        serde_json::json!({"requests":searches.len(),"receive_windows":windows.len(),"unique_reply_ids":ids.len(),"total":total,"returned":paths.len(),"elapsed_ms":started.elapsed().as_millis()}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reply_ids_discard_late_results_from_replaced_queries() {
        let reply = Reply {
            sender: 0x1234usize as HWND,
            id: 42,
            data: None,
        };
        assert!(reply.accepts(0x1234, 42, 28));
        assert!(!reply.accepts(0x1234, 41, 28));
        assert!(!reply.accepts(0x5678, 42, 28));
        assert!(!reply.accepts(0x1234, 42, 27));
        assert!(!reply.accepts(0x1234, 42, 512 * 1024 + 1));
    }
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
