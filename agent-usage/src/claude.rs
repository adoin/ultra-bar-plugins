use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    ptr::{null, null_mut},
};
use windows_sys::Win32::Networking::WinHttp::*;
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
struct Handle(*mut std::ffi::c_void);
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            WinHttpCloseHandle(self.0);
        }
    }
}
pub fn home() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join(".claude")
        })
}
pub fn read(data: &Path) -> Result<Value> {
    // Official statusLine feed is preferred; stale snapshots are not presented as live quota.
    let feed = data.join("claude-rate-limits.json");
    if let Ok(meta) = std::fs::metadata(&feed)
        && meta.len() <= 64 * 1024
        && meta.modified()?.elapsed().unwrap_or_default().as_secs() < 300
    {
        let value: Value = serde_json::from_slice(&std::fs::read(feed)?)?;
        if value["rate_limits"].is_object() {
            return Ok(value["rate_limits"].clone());
        }
    }
    // Compatibility fallback: use existing CLI login without refreshing or modifying credentials.
    let path = home().join(".credentials.json");
    let meta = std::fs::metadata(&path).context("未连接 Claude 订阅账户")?;
    ensure!(meta.len() <= 64 * 1024, "Claude 登录文件过大");
    let value: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let auth = &value["claudeAiOauth"];
    let token = auth["accessToken"]
        .as_str()
        .context("未连接 Claude 订阅账户")?;
    ensure!(
        !token.contains(['\r', '\n']) && token.len() < 8192,
        "Claude 登录状态无效"
    );
    let expires = auth["expiresAt"].as_u64().unwrap_or(u64::MAX);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    ensure!(expires > now, "Claude 登录已过期，请在 CLI 中重新登录");
    unsafe {
        let session = Handle(WinHttpOpen(
            wide("UltraBar-AgentUsage/0.1").as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            null(),
            null(),
            0,
        ));
        ensure!(!session.0.is_null(), "无法初始化 Claude 连接");
        WinHttpSetTimeouts(session.0, 3000, 3000, 5000, 5000);
        let connect = Handle(WinHttpConnect(
            session.0,
            wide("api.anthropic.com").as_ptr(),
            443,
            0,
        ));
        ensure!(!connect.0.is_null(), "无法连接 Claude");
        let request = Handle(WinHttpOpenRequest(
            connect.0,
            wide("GET").as_ptr(),
            wide("/api/oauth/usage").as_ptr(),
            null(),
            null(),
            null(),
            WINHTTP_FLAG_SECURE,
        ));
        ensure!(!request.0.is_null(), "无法创建 Claude 查询");
        let policy = WINHTTP_OPTION_REDIRECT_POLICY_NEVER;
        ensure!(
            WinHttpSetOption(
                request.0,
                WINHTTP_OPTION_REDIRECT_POLICY,
                (&policy as *const u32).cast(),
                4
            ) != 0,
            "无法设置连接策略"
        );
        let headers = wide(&format!(
            "Authorization: Bearer {token}\r\nAccept: application/json\r\nanthropic-beta: oauth-2025-04-20\r\n"
        ));
        ensure!(
            WinHttpSendRequest(
                request.0,
                headers.as_ptr(),
                (headers.len() - 1) as _,
                null(),
                0,
                0,
                0
            ) != 0
                && WinHttpReceiveResponse(request.0, null_mut()) != 0,
            "Claude 额度查询失败"
        );
        let mut status = 0u32;
        let mut length = 4;
        ensure!(
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                null(),
                (&mut status as *mut u32).cast(),
                &mut length,
                null_mut()
            ) != 0
                && status == 200,
            "Claude 额度不可用（HTTP {status}）"
        );
        let mut bytes = Vec::new();
        loop {
            let mut b = [0u8; 4096];
            let mut n = 0;
            ensure!(
                WinHttpReadData(request.0, b.as_mut_ptr().cast(), b.len() as _, &mut n) != 0,
                "Claude 连接中断"
            );
            if n == 0 {
                break;
            }
            ensure!(bytes.len() + n as usize <= 64 * 1024, "Claude 响应过大");
            bytes.extend_from_slice(&b[..n as usize]);
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
}
