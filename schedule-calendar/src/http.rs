//! Windows HTTPS transport: system proxy/TLS and gzip, no extra async or TLS runtime.
use anyhow::{Result, ensure};
use std::{
    io::Read,
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
fn encoded(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char)
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
pub fn get(
    host: &str,
    path: &str,
    parameters: &[(&str, String)],
    key: Option<&str>,
) -> Result<serde_json::Value> {
    unsafe {
        ensure!(
            key.is_none_or(|s| s.len() < 4096 && !s.contains(['\r', '\n'])),
            "Invalid API key"
        );
        let session = Handle(WinHttpOpen(
            wide("UltraBar-ScheduleCalendar/0.2").as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            null(),
            null(),
            0,
        ));
        ensure!(!session.0.is_null(), "Cannot initialize weather connection");
        WinHttpSetTimeouts(session.0, 3000, 4000, 8000, 8000);
        let connect = Handle(WinHttpConnect(session.0, wide(host).as_ptr(), 443, 0));
        ensure!(!connect.0.is_null(), "Cannot connect weather service");
        let query = parameters
            .iter()
            .map(|(k, v)| format!("{}={}", encoded(k), encoded(v)))
            .collect::<Vec<_>>()
            .join("&");
        let path = if query.is_empty() {
            path.to_string()
        } else {
            format!("{path}?{query}")
        };
        let request = Handle(WinHttpOpenRequest(
            connect.0,
            wide("GET").as_ptr(),
            wide(&path).as_ptr(),
            null(),
            null(),
            null(),
            WINHTTP_FLAG_SECURE,
        ));
        ensure!(!request.0.is_null(), "Cannot create weather request");
        let policy = WINHTTP_OPTION_REDIRECT_POLICY_NEVER;
        ensure!(
            WinHttpSetOption(
                request.0,
                WINHTTP_OPTION_REDIRECT_POLICY,
                (&policy as *const u32).cast(),
                4
            ) != 0,
            "Cannot disable weather redirects"
        );
        let headers = wide(&format!(
            "Accept: application/json\r\nAccept-Encoding: gzip, deflate\r\n{}",
            key.map(|k| format!("X-QW-Api-Key: {k}\r\n"))
                .unwrap_or_default()
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
            "Weather request failed"
        );
        let mut status = 0u32;
        let mut len = 4;
        ensure!(
            WinHttpQueryHeaders(
                request.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                null(),
                (&mut status as *mut u32).cast(),
                &mut len,
                null_mut()
            ) != 0
                && status == 200,
            "Weather unavailable (HTTP {status})"
        );
        let mut bytes = Vec::new();
        loop {
            let mut available = 0;
            ensure!(
                WinHttpQueryDataAvailable(request.0, &mut available) != 0,
                "Weather response unavailable ({})",
                windows_sys::Win32::Foundation::GetLastError()
            );
            if available == 0 {
                break;
            }
            ensure!(
                available as usize <= 512 * 1024 - bytes.len(),
                "Weather response too large"
            );
            let mut buffer = [0u8; 8192];
            let mut count = 0;
            ensure!(
                WinHttpReadData(
                    request.0,
                    buffer.as_mut_ptr().cast(),
                    (buffer.len() as u32).min(available),
                    &mut count
                ) != 0,
                "Weather connection interrupted ({})",
                windows_sys::Win32::Foundation::GetLastError()
            );
            if count == 0 {
                break;
            }
            ensure!(
                bytes.len() + count as usize <= 512 * 1024,
                "Weather response too large"
            );
            bytes.extend_from_slice(&buffer[..count as usize]);
        }
        let mut encoding = [0u16; 64];
        let mut len = (encoding.len() * 2) as u32;
        let encoding = if WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_CONTENT_ENCODING,
            null(),
            encoding.as_mut_ptr().cast(),
            &mut len,
            null_mut(),
        ) != 0
        {
            String::from_utf16_lossy(
                &encoding[..encoding
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(encoding.len())],
            )
            .to_ascii_lowercase()
        } else {
            String::new()
        };
        let mut decoded = Vec::new();
        match encoding.as_str() {
            "gzip" => {
                flate2::read::GzDecoder::new(bytes.as_slice())
                    .take(512 * 1024 + 1)
                    .read_to_end(&mut decoded)?;
            }
            "deflate" => {
                flate2::read::ZlibDecoder::new(bytes.as_slice())
                    .take(512 * 1024 + 1)
                    .read_to_end(&mut decoded)?;
            }
            "" | "identity" => decoded = bytes,
            _ => anyhow::bail!("Unsupported weather encoding"),
        }
        ensure!(decoded.len() <= 512 * 1024, "Weather response too large");
        Ok(serde_json::from_slice(&decoded)?)
    }
}
