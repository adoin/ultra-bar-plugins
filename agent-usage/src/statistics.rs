//! Incremental, metadata-only token accounting. Prompt/message bodies are never retained.
use chrono::{DateTime, Datelike, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    time::UNIX_EPOCH,
};
use ultra_bar_plugin_ui::{
    Panel,
    statistics::{Agent, Bucket, Dataset, Tokens},
};
#[derive(Default, Serialize, Deserialize)]
struct FileState {
    #[serde(default)]
    breakdown: bool,
    offset: u64,
    length: u64,
    stamp: u128,
    session: String,
    project: String,
    model: String,
    previous: Tokens,
    subagent_at: Option<i64>,
    rows: BTreeMap<String, Bucket>,
    messages: HashMap<String, (String, Tokens)>,
}
#[derive(Default, Serialize, Deserialize)]
struct Index {
    version: u32,
    files: HashMap<String, FileState>,
}
fn day() -> i32 {
    Local::now().date_naive().num_days_from_ce()
}
fn timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|s| s.with_timezone(&Utc))
}
fn read_tokens(value: &Value, claude: bool) -> Tokens {
    let n = |k| value[k].as_u64().unwrap_or(0);
    let cached = if claude {
        n("cache_read_input_tokens")
    } else {
        n("cached_input_tokens")
    };
    let input = n("input_tokens")
        + if claude {
            cached + n("cache_creation_input_tokens")
        } else {
            0
        };
    let output = n("output_tokens");
    Tokens {
        input,
        cached,
        output,
        reasoning: n("reasoning_output_tokens"),
        total: if claude {
            input + output
        } else {
            value["total_tokens"].as_u64().unwrap_or(input + output)
        },
    }
}
fn delta(now: &Tokens, old: &Tokens) -> Tokens {
    if now.total < old.total {
        return now.clone();
    }
    Tokens {
        total: now.total.saturating_sub(old.total),
        input: now.input.saturating_sub(old.input),
        cached: now.cached.saturating_sub(old.cached),
        output: now.output.saturating_sub(old.output),
        reasoning: now.reasoning.saturating_sub(old.reasoning),
    }
}
fn apply(state: &mut FileState, record: &Value, claude: bool, today: i32) {
    let payload = &record["payload"];
    let stamp = timestamp(&record["timestamp"]).or_else(|| timestamp(&payload["timestamp"]));
    if !claude && record["type"] == "session_meta" {
        if let Some(id) = payload["id"].as_str() {
            state.session = id.into();
        }
        if let Some(cwd) = payload["cwd"].as_str() {
            state.project = cwd.trim_start_matches(r"\\?\").into();
        }
        if let Some(model) = payload["model"].as_str() {
            state.model = model.into();
        }
        if payload.pointer("/source/subagent").is_some() || payload["thread_source"] == "subagent" {
            state.subagent_at = stamp.map(|s| s.timestamp());
        }
        return;
    }
    if !claude && record["type"] == "turn_context" {
        if let Some(model) = payload["model"].as_str() {
            state.model = model.into();
        }
        return;
    }
    let Some(stamp) = stamp else { return };
    let date = stamp.with_timezone(&Local).date_naive();
    let ordinal = date.num_days_from_ce();
    let mut calls = 1;
    let tokens;
    if claude {
        if record["type"] != "assistant" || !record["message"]["usage"].is_object() {
            return;
        }
        let message = &record["message"];
        if let Some(cwd) = record["cwd"].as_str() {
            state.project = cwd.into();
        }
        if let Some(model) = message["model"].as_str() {
            state.model = model.into();
        }
        if let Some(id) = record["sessionId"].as_str() {
            state.session = id.into();
        }
        if ordinal < today - 29 {
            return;
        }
        let current = read_tokens(&message["usage"], true);
        state.breakdown |=
            message["usage"]["input_tokens"].is_u64() && message["usage"]["output_tokens"].is_u64();
        let Some(id) = message["id"].as_str() else {
            return;
        };
        if let Some((_, old)) = state.messages.get(id) {
            tokens = delta(
                &Tokens {
                    total: current
                        .input
                        .max(old.input)
                        .saturating_add(current.output.max(old.output)),
                    input: current.input.max(old.input),
                    cached: current.cached.max(old.cached),
                    output: current.output.max(old.output),
                    reasoning: 0,
                },
                old,
            );
            calls = 0;
        } else {
            tokens = current.clone();
        }
        let previous = state
            .messages
            .get(id)
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let mut merged = previous;
        merged.add(&tokens);
        state.messages.insert(id.into(), (date.to_string(), merged));
    } else {
        if payload["type"] != "token_count" || !payload["info"]["total_token_usage"].is_object() {
            return;
        }
        let now = read_tokens(&payload["info"]["total_token_usage"], false);
        state.breakdown |= payload["info"]["total_token_usage"]["input_tokens"].is_u64()
            && payload["info"]["total_token_usage"]["output_tokens"].is_u64();
        tokens = delta(&now, &state.previous);
        state.previous = now;
        if state
            .subagent_at
            .is_some_and(|created| stamp.timestamp() <= created + 2)
        {
            return;
        }
        if ordinal < today - 29 {
            return;
        }
    }
    if tokens.total == 0 {
        return;
    }
    let key = format!("{}|{}", date, state.model);
    let row = state.rows.entry(key).or_insert_with(|| Bucket {
        day: ordinal,
        date: date.to_string(),
        project: state.project.clone(),
        model: state.model.clone(),
        session: state.session.clone(),
        ..Default::default()
    });
    row.tokens.add(&tokens);
    row.calls += calls;
}
fn collect_paths(root: &Path, paths: &mut Vec<PathBuf>) {
    let mut directories = vec![root.to_owned()];
    while let Some(dir) = directories.pop() {
        for entry in fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
        {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            if kind.is_dir() {
                directories.push(path);
            } else if path.extension().is_some_and(|s| s == "jsonl")
                && entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age.as_secs() < 32 * 86400)
            {
                paths.push(path);
            }
            if paths.len() > 4096 {
                return;
            }
        }
    }
}
fn parse_file(path: &Path, state: &mut FileState, claude: bool, today: i32) -> std::io::Result<()> {
    let mut file = File::open(path)?;
    let meta = file.metadata()?;
    let stamp = meta
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    if meta.len() == state.length && stamp == state.stamp {
        return Ok(());
    }
    if meta.len() < state.offset || meta.len() == state.length {
        *state = FileState::default();
    }
    if state.session.is_empty() {
        state.session = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
    }
    file.seek(SeekFrom::Start(state.offset))?;
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut line = Vec::new();
    loop {
        line.clear();
        let count = (&mut reader)
            .take(2 * 1024 * 1024)
            .read_until(b'\n', &mut line)?;
        if count == 0 {
            break;
        }
        if !line.ends_with(b"\n") {
            if count < 2 * 1024 * 1024 {
                break;
            }
            let mut skipped = count;
            loop {
                let buffer = reader.fill_buf()?;
                if buffer.is_empty() {
                    return Ok(());
                }
                let end = buffer.iter().position(|b| *b == b'\n');
                let n = end.map_or(buffer.len(), |i| i + 1);
                reader.consume(n);
                skipped += n;
                if end.is_some() {
                    break;
                }
            }
            state.offset += skipped as u64;
            continue;
        }
        state.offset += count as u64;
        let Ok(text) = std::str::from_utf8(&line) else {
            continue;
        };
        let selected = if claude {
            text.contains("\"usage\"")
        } else {
            text.contains("\"token_count\"")
                || text.contains("\"session_meta\"")
                || text.contains("\"turn_context\"")
        };
        if selected && let Ok(value) = serde_json::from_slice(&line) {
            apply(state, &value, claude, today);
        }
    }
    state.length = meta.len();
    state.stamp = stamp;
    state.rows.retain(|_, row| row.day >= today - 29);
    state.messages.retain(|_, (date, _)| {
        NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .is_ok_and(|d| d.num_days_from_ce() >= today - 29)
    });
    Ok(())
}
pub struct Service {
    pub data: Arc<Mutex<Dataset>>,
    request: mpsc::SyncSender<()>,
    panel: Panel,
    directory: PathBuf,
}
fn progress(dir: &Path, phase: &str, files: usize) {
    let _ = fs::write(
        dir.join("statistics-progress.json"),
        serde_json::to_vec(&serde_json::json!({"phase":phase,"files":files})).unwrap_or_default(),
    );
}
impl Service {
    pub fn start(home: PathBuf, data_dir: PathBuf, panel: Panel) -> Self {
        let (data, rx) = {
            let p = data_dir.join("statistics-index.json");
            let index = fs::metadata(&p)
                .ok()
                .filter(|m| m.len() < 32 * 1024 * 1024)
                .and_then(|_| fs::read(p).ok())
                .and_then(|b| serde_json::from_slice::<Index>(&b).ok())
                .filter(|i| i.version == 1)
                .unwrap_or_default();
            let mut agents = vec![Agent {
                id: "Codex".into(),
                name: "Codex".into(),
                ..Default::default()
            }];
            if crate::claude::home().join(".credentials.json").is_file() {
                agents.push(Agent {
                    id: "Claude".into(),
                    name: "Claude".into(),
                    ..Default::default()
                });
            }
            if crate::grok::installed() {
                agents.push(Agent {
                    id: "Grok Bot".into(),
                    name: "Grok Bot".into(),
                    quota: crate::grok::read()
                        .ok()
                        .and_then(|q| q.remaining)
                        .map(|r| format!("{r:.1}%"))
                        .unwrap_or_else(|| "—".into()),
                    source: "Grok Bot 本地额度快照".into(),
                    ..Default::default()
                });
            }
            (
                Arc::new(Mutex::new(Dataset {
                    today: day(),
                    agents,
                    loading: true,
                })),
                index,
            )
        };
        let (request, commands) = mpsc::sync_channel(1);
        panel.statistics(data.lock().unwrap_or_else(|e| e.into_inner()).clone());
        let shared = data.clone();
        let ui = panel.clone();
        let directory = data_dir.clone();
        std::thread::spawn(move || {
            let mut index = rx;
            while commands.recv().is_ok() {
                progress(&data_dir, "scanning", 0);
                let today = day();
                let mut agents = vec![];
                let mut discovered = HashSet::new();
                for (id, name, roots, claude) in [
                    (
                        "Codex",
                        "Codex",
                        vec![home.join("sessions"), home.join("archived_sessions")],
                        false,
                    ),
                    (
                        "Claude",
                        "Claude",
                        vec![crate::claude::home().join("projects")],
                        true,
                    ),
                ] {
                    let mut paths = vec![];
                    for root in roots {
                        collect_paths(&root, &mut paths);
                    }
                    if id == "Claude"
                        && paths.is_empty()
                        && !crate::claude::home().join(".credentials.json").exists()
                    {
                        continue;
                    }
                    let mut agent = Agent {
                        id: id.into(),
                        name: name.into(),
                        available: !paths.is_empty(),
                        breakdown: false,
                        source: "本地使用记录 · 最近 30 天".into(),
                        ..Default::default()
                    };
                    let mut sessions = HashMap::<String, (u128, u64, Vec<Bucket>)>::new();
                    for path in paths {
                        let key = path.to_string_lossy().into_owned();
                        discovered.insert(key.clone());
                        let state = index.files.entry(key).or_default();
                        let _ = parse_file(&path, state, claude, today);
                        agent.breakdown |= state.breakdown
                            || state.previous.input > 0
                            || state.previous.output > 0;
                        progress(&data_dir, "scanning", discovered.len());
                        let replace =
                            sessions
                                .get(&state.session)
                                .is_none_or(|(stamp, length, _)| {
                                    state.stamp > *stamp
                                        || (state.stamp == *stamp && state.length > *length)
                                });
                        if replace {
                            sessions.insert(
                                state.session.clone(),
                                (
                                    state.stamp,
                                    state.length,
                                    state.rows.values().cloned().collect(),
                                ),
                            );
                        }
                        let mut partial = agent.clone();
                        partial.buckets = sessions
                            .values()
                            .flat_map(|(_, _, rows)| rows.clone())
                            .collect();
                        partial.available = !partial.buckets.is_empty();
                        partial.source =
                            format!("正在整理本地记录 · 已处理 {} 份", discovered.len());
                        let snapshot = {
                            let mut data = shared.lock().unwrap_or_else(|e| e.into_inner());
                            if let Some(existing) =
                                data.agents.iter_mut().find(|a| a.id == partial.id)
                            {
                                partial.quota = existing.quota.clone();
                                partial.resets = existing.resets;
                                partial.reset_credits = existing.reset_credits.clone();
                                *existing = partial;
                            }
                            data.loading = true;
                            data.clone()
                        };
                        ui.statistics(snapshot);
                        index.version = 1;
                        let pending = data_dir.join("statistics-index.json.new");
                        if let Ok(bytes) = serde_json::to_vec(&index)
                            && fs::write(&pending, bytes).is_ok()
                        {
                            let _ = fs::rename(pending, data_dir.join("statistics-index.json"));
                        }
                    }
                    for (_, (_, _, rows)) in sessions {
                        agent.buckets.extend(rows);
                    }
                    agent.available = !agent.buckets.is_empty();
                    if id == "Claude"
                        && !agent.available
                        && !crate::claude::home().join(".credentials.json").is_file()
                    {
                        continue;
                    }
                    agents.push(agent);
                }
                if crate::grok::installed() {
                    let quota = crate::grok::read()
                        .ok()
                        .and_then(|q| q.remaining)
                        .map(|r| format!("{r:.1}%"))
                        .unwrap_or_else(|| "—".into());
                    agents.push(Agent {
                        id: "Grok Bot".into(),
                        name: "Grok Bot".into(),
                        quota,
                        source: "Grok Bot 本地额度快照 · 未提供 Token 明细".into(),
                        ..Default::default()
                    });
                }
                let mut dataset = Dataset {
                    today,
                    agents,
                    loading: false,
                };
                {
                    let mut old = shared.lock().unwrap_or_else(|e| e.into_inner());
                    for agent in &mut dataset.agents {
                        if let Some(previous) = old.agents.iter().find(|a| a.id == agent.id) {
                            agent.resets = previous.resets;
                            agent.reset_credits = previous.reset_credits.clone();
                        }
                        if agent.quota.is_empty()
                            && let Some(previous) = old.agents.iter().find(|a| a.id == agent.id)
                        {
                            agent.quota = previous.quota.clone();
                        }
                    }
                    *old = dataset.clone();
                }
                if let Ok(bytes) = serde_json::to_vec(&dataset) {
                    let _ = fs::write(data_dir.join("statistics-view.json"), bytes);
                }
                ui.statistics(dataset);
                progress(&data_dir, "ready", discovered.len());
                index.files.retain(|key, _| discovered.contains(key));
                index.version = 1;
                let pending = data_dir.join("statistics-index.json.new");
                if let Ok(bytes) = serde_json::to_vec(&index)
                    && fs::write(&pending, bytes).is_ok()
                {
                    let _ = fs::rename(pending, data_dir.join("statistics-index.json"));
                }
            }
        });
        Self {
            data,
            request,
            panel,
            directory,
        }
    }
    pub fn show(&self) {
        progress(&self.directory, "queued", 0);
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if data.agents.is_empty() {
            data = Dataset {
                today: day(),
                loading: true,
                agents: vec![Agent {
                    id: "Codex".into(),
                    name: "Codex".into(),
                    ..Default::default()
                }],
            };
        }
        self.panel.statistics(data);
        let _ = self.request.try_send(());
    }
    pub fn quota(&self, names: &[&str], quotas: &[super::Quota]) {
        let mut data = self.data.lock().unwrap_or_else(|e| e.into_inner());
        for agent in &mut data.agents {
            if let Some(i) = names.iter().position(|name| *name == agent.id) {
                agent.resets = quotas[i].resets;
                agent.reset_credits = quotas[i].reset_credits.clone();
                let Some(value) = quotas[i].remaining else {
                    continue;
                };
                agent.quota = if agent.id == "Grok Bot" {
                    format!("{value:.1}%")
                } else {
                    format!("{value:.0}%")
                };
            }
        }
        self.panel.statistics(data.clone());
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn codex_deltas_and_cached_tokens_are_not_double_counted() {
        let today = day();
        let at = Utc::now().to_rfc3339();
        let mut state = FileState::default();
        for total in [100, 100, 150] {
            apply(
                &mut state,
                &serde_json::json!({"timestamp":at,"payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":total,"input_tokens":total-10,"cached_input_tokens":50,"output_tokens":10,"reasoning_output_tokens":5}}}}),
                false,
                today,
            );
        }
        let total = state.rows.values().map(|r| r.tokens.total).sum::<u64>();
        assert_eq!(total, 150);
        assert_eq!(state.rows.values().map(|r| r.calls).sum::<u64>(), 2);
    }
    #[test]
    fn repeated_claude_message_ids_are_counted_once() {
        let today = day();
        let mut state = FileState::default();
        let value = serde_json::json!({"type":"assistant","timestamp":Utc::now().to_rfc3339(),"message":{"id":"one","usage":{"input_tokens":10,"cache_read_input_tokens":20,"output_tokens":5}}});
        apply(&mut state, &value, true, today);
        apply(&mut state, &value, true, today);
        let row = state.rows.values().next().unwrap();
        assert_eq!(row.tokens.total, 35);
        assert_eq!(row.calls, 1);
    }
    #[test]
    fn claude_streaming_output_updates_without_recounting_input() {
        let today = day();
        let mut state = FileState::default();
        for (input, cached, output) in [(10, 20, 5), (0, 0, 10)] {
            apply(
                &mut state,
                &serde_json::json!({"type":"assistant","timestamp":Utc::now().to_rfc3339(),"message":{"id":"one","usage":{"input_tokens":input,"cache_read_input_tokens":cached,"output_tokens":output}}}),
                true,
                today,
            );
        }
        let row = state.rows.values().next().unwrap();
        assert_eq!(row.tokens.total, 40);
        assert_eq!(row.tokens.input, 30);
        assert_eq!(row.calls, 1);
    }
    #[test]
    fn inherited_subagent_prefix_is_only_a_baseline_and_counter_reset_starts_a_new_segment() {
        let today = day();
        let start = Utc::now();
        let mut state = FileState::default();
        apply(
            &mut state,
            &serde_json::json!({"type":"session_meta","timestamp":start.to_rfc3339(),"payload":{"id":"child","source":{"subagent":{}},"cwd":"D:/project"}}),
            false,
            today,
        );
        for (seconds, total) in [(0, 100), (5, 120), (10, 10)] {
            apply(
                &mut state,
                &serde_json::json!({"timestamp":(start+chrono::Duration::seconds(seconds)).to_rfc3339(),"payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":total,"input_tokens":total,"output_tokens":0}}}}),
                false,
                today,
            );
        }
        assert_eq!(state.rows.values().map(|r| r.tokens.total).sum::<u64>(), 30);
        apply(
            &mut state,
            &serde_json::json!({"type":"response_item","timestamp":start.to_rfc3339(),"payload":{"text":"PRIVATE_PROMPT_SENTINEL"}}),
            false,
            today,
        );
        assert!(
            !serde_json::to_string(&state)
                .unwrap()
                .contains("PRIVATE_PROMPT_SENTINEL")
        );
    }
    #[test]
    fn incremental_file_reads_retry_an_incomplete_line_and_cache_roundtrips() {
        use std::io::Write;
        let root = PathBuf::from("D:/tmp/ultra-bar-agent-dashboard-20261111");
        fs::create_dir_all(&root).unwrap();
        let file = root.join(format!(
            "incremental-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let record = |total| {
            serde_json::json!({"timestamp":Utc::now().to_rfc3339(),"payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":total,"input_tokens":total,"output_tokens":0}}}}).to_string()
        };
        let first = record(100);
        let second = record(150);
        fs::write(&file, format!("{first}\n{}", &second[..second.len() - 4])).unwrap();
        let mut state = FileState::default();
        parse_file(&file, &mut state, false, day()).unwrap();
        assert_eq!(state.previous.total, 100);
        let mut writer = fs::OpenOptions::new().append(true).open(&file).unwrap();
        writeln!(writer, "{}", &second[second.len() - 4..]).unwrap();
        drop(writer);
        parse_file(&file, &mut state, false, day()).unwrap();
        parse_file(&file, &mut state, false, day()).unwrap();
        assert_eq!(
            state.rows.values().map(|r| r.tokens.total).sum::<u64>(),
            150
        );
        let saved = serde_json::to_vec(&state).unwrap();
        let copy: FileState = serde_json::from_slice(&saved).unwrap();
        assert_eq!(copy.offset, state.offset);
        fs::remove_file(file).unwrap();
    }
}
