// Incremental Codex event store.
//
// Codex writes local rollout JSONL files under ~/.codex/sessions and
// ~/.codex/archived_sessions. We ingest only the appended bytes of those files,
// convert token_count events and tool-call response items into compact RawEvents,
// and persist that compact event stream in the app cache.
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct RateLimitWindowEvent {
    pub used_percent: f64,
    pub window_minutes: u64,
    pub resets_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct RateLimitEvent {
    pub limit_id: String,
    pub plan_type: String,
    pub primary: Option<RateLimitWindowEvent>,
    pub secondary: Option<RateLimitWindowEvent>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct RawEvent {
    pub ts_ms: i64,
    pub session: String,
    pub model: String,
    pub in_tok: f64, // uncached input tokens
    pub cc: f64,     // cache creation tokens (Codex logs do not expose this today)
    pub cr: f64,     // cached input tokens
    pub out_tok: f64,
    pub tools: Vec<String>,
    pub id: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub effort: String,
    #[serde(default)]
    pub rate_limit: Option<RateLimitEvent>,
}

#[derive(Serialize, Deserialize, Default)]
struct Manifest {
    // path -> (size, mtime_ms, byte offset already ingested)
    files: HashMap<String, (u64, i64, u64)>,
}

pub struct Store {
    pub events: Vec<RawEvent>,
    index: HashMap<String, usize>,
    manifest: Manifest,
}

// v104 publishes the Codex-specific event schema: tool calls are stored in
// a Codex-native `tools` bucket.
const STORE_VERSION: u32 = 105;

#[derive(Clone)]
struct FileState {
    session: String,
    model: String,
    effort: String,
}

fn model_from_log(model: &str) -> String {
    let trimmed = model.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("codex") {
        "unknown".to_string()
    } else {
        trimmed.to_string()
    }
}

fn file_state_from_prefix(path: &Path, offset: u64) -> FileState {
    let mut state = FileState {
        session: session_from_path(path),
        model: "unknown".to_string(),
        effort: String::new(),
    };
    if offset == 0 {
        return state;
    }
    let Ok(f) = fs::File::open(path) else {
        return state;
    };
    let mut buf = Vec::new();
    if f.take(offset).read_to_end(&mut buf).is_err() {
        return state;
    }
    for line in buf.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        let Ok(s) = std::str::from_utf8(line) else {
            continue;
        };
        update_state_from_line(s, &mut state);
    }
    state
}

fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)
}

fn codex_home() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CODEX_HOME") {
        return Some(PathBuf::from(p));
    }
    Some(dirs::home_dir()?.join(".codex"))
}

fn rollout_roots() -> Vec<PathBuf> {
    let Some(home) = codex_home() else {
        return Vec::new();
    };
    vec![home.join("sessions"), home.join("archived_sessions")]
}

fn cache_dir() -> Option<PathBuf> {
    let d = dirs::cache_dir()?.join("codexscope");
    let _ = fs::create_dir_all(&d);
    Some(d)
}

fn session_from_path(path: &Path) -> String {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return String::new();
    };
    if stem.len() >= 36 {
        stem[stem.len() - 36..].to_string()
    } else {
        stem.to_string()
    }
}

impl Store {
    pub fn load() -> Self {
        let mut events: Vec<RawEvent> = Vec::new();
        let mut manifest = Manifest::default();
        if let Some(dir) = cache_dir() {
            let version_ok = fs::read_to_string(dir.join("version"))
                .ok()
                .and_then(|s| s.trim().parse::<u32>().ok())
                == Some(STORE_VERSION);
            if version_ok {
                let loaded_events = fs::read_to_string(dir.join("events.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str::<Vec<RawEvent>>(&t).ok());
                let loaded_manifest = fs::read_to_string(dir.join("offsets.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str::<Manifest>(&t).ok());
                if let (Some(e), Some(m)) = (loaded_events, loaded_manifest) {
                    events = e;
                    manifest = m;
                }
            }
        }
        let index = events
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.id.is_empty())
            .map(|(i, e)| (e.id.clone(), i))
            .collect();
        Store {
            events,
            index,
            manifest,
        }
    }

    pub fn save(&self) {
        if let Some(dir) = cache_dir() {
            if let Ok(t) = serde_json::to_string(&self.events) {
                let _ = write_atomic(&dir.join("events.json"), t.as_bytes());
            }
            if let Ok(t) = serde_json::to_string(&self.manifest) {
                let _ = write_atomic(&dir.join("offsets.json"), t.as_bytes());
            }
            let _ = write_atomic(&dir.join("version"), STORE_VERSION.to_string().as_bytes());
        }
    }

    fn rebuild_index(&mut self) {
        self.index = self
            .events
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.id.is_empty())
            .map(|(i, e)| (e.id.clone(), i))
            .collect();
    }

    fn purge_source(&mut self, key: &str) {
        self.events.retain(|e| e.source != key);
        self.rebuild_index();
    }

    pub fn prune_before(&mut self, cutoff_ms: i64) -> bool {
        let before = self.events.len();
        self.events.retain(|e| e.ts_ms >= cutoff_ms);
        let removed = self.events.len() != before;
        if removed {
            self.rebuild_index();
        }
        removed
    }

    pub fn ingest(&mut self) -> bool {
        let mut dirty = false;
        for root in rollout_roots() {
            if !root.exists() {
                continue;
            }
            for entry in WalkDir::new(&root)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map(|x| x == "jsonl").unwrap_or(false))
            {
                let path = entry.path();
                let key = path.to_string_lossy().to_string();
                let Ok(meta) = fs::metadata(path) else {
                    continue;
                };
                let size = meta.len();
                let mtime_ms = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);

                let mut offset = match self.manifest.files.get(&key).copied() {
                    Some((psize, pmtime, poff)) => {
                        if psize == size && pmtime == mtime_ms {
                            continue;
                        }
                        if size < poff {
                            self.purge_source(&key);
                            0
                        } else {
                            poff
                        }
                    }
                    None => 0,
                };

                let Ok(mut f) = fs::File::open(path) else {
                    continue;
                };
                if f.seek(SeekFrom::Start(offset)).is_err() {
                    continue;
                }
                let mut buf = Vec::new();
                if f.read_to_end(&mut buf).is_err() {
                    continue;
                }
                let process_until = match buf.iter().rposition(|&b| b == b'\n') {
                    Some(i) => i + 1,
                    None => 0,
                };
                let mut state = file_state_from_prefix(path, offset);
                for line in buf[..process_until].split(|&b| b == b'\n') {
                    if line.is_empty() {
                        continue;
                    }
                    let Ok(s) = std::str::from_utf8(line) else {
                        continue;
                    };
                    if let Some(mut ev) = parse_line(s, &mut state) {
                        ev.source = key.clone();
                        if !ev.id.is_empty() {
                            if self.index.contains_key(&ev.id) {
                                continue;
                            }
                            self.index.insert(ev.id.clone(), self.events.len());
                        }
                        self.events.push(ev);
                    }
                }
                offset += process_until as u64;
                self.manifest.files.insert(key, (size, mtime_ms, offset));
                dirty = true;
            }
        }
        dirty
    }
}

fn parse_ts_ms(v: &serde_json::Value) -> Option<i64> {
    let ts = v.get("timestamp")?.as_str()?;
    Some(DateTime::parse_from_rfc3339(ts).ok()?.timestamp_millis())
}

fn num(v: Option<&serde_json::Value>) -> f64 {
    v.and_then(|x| x.as_f64()).unwrap_or(0.0)
}

fn parse_rate_window(v: Option<&serde_json::Value>) -> Option<RateLimitWindowEvent> {
    let v = v?;
    Some(RateLimitWindowEvent {
        used_percent: num(v.get("used_percent")),
        window_minutes: v
            .get("window_minutes")
            .and_then(|x| x.as_u64())
            .unwrap_or(0),
        resets_at: v.get("resets_at").and_then(|x| x.as_i64()).unwrap_or(0),
    })
}

fn parse_rate_limit(v: &serde_json::Value) -> Option<RateLimitEvent> {
    let r = v.get("rate_limits")?;
    Some(RateLimitEvent {
        limit_id: r
            .get("limit_id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        plan_type: r
            .get("plan_type")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        primary: parse_rate_window(r.get("primary")),
        secondary: parse_rate_window(r.get("secondary")),
    })
}

fn update_state_from_line(line: &str, state: &mut FileState) {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
        update_state_from_value(&v, state);
    }
}

fn update_state_from_value(v: &serde_json::Value, state: &mut FileState) {
    match v.get("type").and_then(|x| x.as_str()) {
        Some("session_meta") => {
            if let Some(id) = v
                .get("payload")
                .and_then(|p| p.get("session_id").or_else(|| p.get("id")))
                .and_then(|x| x.as_str())
            {
                state.session = id.to_string();
            }
        }
        Some("turn_context") => {
            let Some(payload) = v.get("payload") else {
                return;
            };
            if let Some(model) = payload.get("model").and_then(|x| x.as_str()) {
                state.model = model_from_log(model);
            }
            if let Some(effort) = payload
                .get("effort")
                .or_else(|| {
                    payload
                        .get("collaboration_mode")
                        .and_then(|c| c.get("settings"))
                        .and_then(|s| s.get("reasoning_effort"))
                })
                .and_then(|x| x.as_str())
            {
                state.effort = effort.to_string();
            }
        }
        _ => {}
    }
}

fn parse_line(line: &str, state: &mut FileState) -> Option<RawEvent> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    match v.get("type")?.as_str()? {
        "session_meta" => {
            update_state_from_value(&v, state);
            None
        }
        "turn_context" => {
            update_state_from_value(&v, state);
            None
        }
        "event_msg" => parse_event_msg(&v, state),
        "response_item" => parse_response_item(&v, state),
        _ => None,
    }
}

fn parse_event_msg(v: &serde_json::Value, state: &FileState) -> Option<RawEvent> {
    let payload = v.get("payload")?;
    if payload.get("type").and_then(|x| x.as_str()) != Some("token_count") {
        return None;
    }
    let ts_ms = parse_ts_ms(v)?;
    let Some(info) = payload.get("info").filter(|x| !x.is_null()) else {
        return parse_rate_limit(payload).map(|rate_limit| RawEvent {
            ts_ms,
            session: state.session.clone(),
            model: String::new(),
            in_tok: 0.0,
            cc: 0.0,
            cr: 0.0,
            out_tok: 0.0,
            tools: Vec::new(),
            id: format!("rate:{}:{ts_ms}", state.session),
            source: String::new(),
            effort: state.effort.clone(),
            rate_limit: Some(rate_limit),
        });
    };
    let usage = info.get("last_token_usage")?;
    let input_total = num(usage.get("input_tokens"));
    let cached = num(usage.get("cached_input_tokens"));
    let output = num(usage.get("output_tokens"));
    let uncached = (input_total - cached).max(0.0);
    let total = info
        .get("total_token_usage")
        .and_then(|u| u.get("total_tokens"))
        .and_then(|x| x.as_u64())
        .unwrap_or(ts_ms as u64);
    let session = state.session.clone();
    Some(RawEvent {
        ts_ms,
        session: session.clone(),
        model: state.model.clone(),
        in_tok: uncached,
        cc: 0.0,
        cr: cached,
        out_tok: output,
        tools: Vec::new(),
        id: format!("tok:{session}:{total}"),
        source: String::new(),
        effort: state.effort.clone(),
        rate_limit: parse_rate_limit(payload),
    })
}

fn parse_response_item(v: &serde_json::Value, state: &FileState) -> Option<RawEvent> {
    let payload = v.get("payload")?;
    let typ = payload.get("type").and_then(|x| x.as_str()).unwrap_or("");
    let tool = match typ {
        "function_call" => payload
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("function_call"),
        "web_search_call" => "web_search",
        "tool_search_call" => "tool_search",
        other if other.ends_with("_call") => other.trim_end_matches("_call"),
        _ => return None,
    };
    let ts_ms = parse_ts_ms(v)?;
    let id = payload
        .get("id")
        .or_else(|| payload.get("call_id"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("tool:{}:{}:{}", state.session, tool, ts_ms));
    Some(RawEvent {
        ts_ms,
        session: state.session.clone(),
        model: String::new(),
        in_tok: 0.0,
        cc: 0.0,
        cr: 0.0,
        out_tok: 0.0,
        tools: vec![tool.to_string()],
        id: format!("tool:{id}"),
        source: String::new(),
        effort: state.effort.clone(),
        rate_limit: None,
    })
}
