//! Account-only official app-server RPC; never starts a conversation or a model turn.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};
pub struct Client {
    child: Child,
    input: std::process::ChildStdin,
    output: Receiver<Value>,
    next: u64,
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn executable() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("ULTRA_BAR_CODEX_EXE") {
        let p = PathBuf::from(path);
        ensure!(p.is_absolute() && p.is_file(), "Codex 路径无效");
        return Ok(p);
    }
    let root = PathBuf::from(std::env::var_os("LOCALAPPDATA").context("缺少 LOCALAPPDATA")?)
        .join("OpenAI/Codex/bin");
    let mut candidates: Vec<_> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .take(128)
        .map(|e| e.path().join("codex.exe"))
        .filter(|p| p.is_file())
        .collect();
    candidates.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    if let Some(p) = candidates.pop() {
        return Ok(p);
    }
    for p in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let exe = p.join("codex.exe");
        if exe.is_file() {
            return Ok(exe);
        }
    }
    anyhow::bail!("未找到 Codex CLI")
}
impl Client {
    pub fn start(home: Option<&std::path::Path>) -> Result<Self> {
        let mut command = Command::new(executable()?);
        command
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(0x08000000 | 0x00004000);
        if let Some(home) = home {
            command.env("CODEX_HOME", home);
        }
        let mut child = command.spawn().context("无法启动 Codex 账户连接")?;
        let input = child.stdin.take().unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, output) = mpsc::sync_channel(16);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(out);
            // Account replies can exceed the small service protocol; keep an independent 1 MiB cap.
            loop {
                let mut line = String::new();
                let mut limited = (&mut reader).take(1024 * 1024);
                use std::io::Read;
                match limited.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if !line.ends_with('\n') => break,
                    _ => {}
                }
                if let Ok(v) = serde_json::from_str(&line)
                    && tx.send(v).is_err()
                {
                    break;
                }
            }
        });
        let mut client = Self {
            child,
            input,
            output,
            next: 1,
        };
        client.request("initialize",Some(json!({"clientInfo":{"name":"ultra_bar_agent_usage","title":"Ultra Bar Agent Usage","version":env!("CARGO_PKG_VERSION")}})))?;
        client.write(&json!({"method":"initialized"}))?;
        Ok(client)
    }
    fn write(&mut self, v: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.input, v)?;
        self.input.write_all(b"\n")?;
        self.input.flush()?;
        Ok(())
    }
    fn request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next;
        self.next += 1;
        let mut v = json!({"id":id,"method":method});
        if let Some(p) = params {
            v["params"] = p;
        }
        self.write(&v)?;
        let end = Instant::now() + Duration::from_secs(8);
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            let reply = self
                .output
                .recv_timeout(left)
                .context("Codex 账户查询超时")?;
            if reply["id"].as_u64() == Some(id) {
                ensure!(
                    reply.get("error").is_none(),
                    "Codex 账户不可用，请检查 CLI 登录"
                );
                return Ok(reply["result"].clone());
            }
        }
        anyhow::bail!("Codex 账户查询超时")
    }
    pub fn read(&mut self) -> Result<Value> {
        self.request("account/rateLimits/read", None)
    }
}
