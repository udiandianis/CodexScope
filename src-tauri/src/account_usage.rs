use crate::model::{RateLimitHistoryPoint, RateLimitSnapshot, RateLimitWindow};
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const REFRESH_RETRY_MS: i64 = 2 * 60 * 1000;
const RATE_LIMIT_REFRESH_RETRY_MS: i64 = 2 * 60 * 1000;
const RATE_LIMIT_HISTORY_RETENTION_MS: i64 = 400 * 24 * 60 * 60 * 1000;
const RESET_CREDITS_URL: &str = "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
static LAST_REFRESH_ATTEMPT_MS: AtomicI64 = AtomicI64::new(0);
static REFRESH_LOCK: Mutex<()> = Mutex::new(());
static LAST_RATE_LIMIT_REFRESH_ATTEMPT_MS: AtomicI64 = AtomicI64::new(0);
static RATE_LIMIT_REFRESH_LOCK: Mutex<()> = Mutex::new(());
static RATE_LIMIT_HISTORY_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUsageSummary {
    pub lifetime_tokens: u64,
    pub peak_daily_tokens: u64,
    pub longest_running_turn_sec: u64,
    pub current_streak_days: u64,
    pub longest_streak_days: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DailyUsageBucket {
    pub start_date: String,
    pub tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUsage {
    pub summary: AccountUsageSummary,
    pub daily_usage_buckets: Vec<DailyUsageBucket>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedAccountUsage {
    fetched_at_ms: i64,
    usage: AccountUsage,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedRateLimits {
    fetched_at_ms: i64,
    rate_limits: RateLimitSnapshot,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedRateLimitHistory {
    points: Vec<RateLimitHistoryPoint>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitsReadResult {
    rate_limits: AppRateLimits,
    rate_limit_reset_credits: Option<RateLimitResetCredits>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitResetCredits {
    available_count: Option<u64>,
}

#[derive(Deserialize)]
struct CodexAuth {
    tokens: Option<CodexAuthTokens>,
}

#[derive(Deserialize)]
struct CodexAuthTokens {
    access_token: Option<String>,
    account_id: Option<String>,
}

#[derive(Deserialize)]
struct WhamResetCredits {
    available_count: Option<u64>,
    credits: Option<Vec<WhamResetCredit>>,
}

#[derive(Deserialize)]
struct WhamResetCredit {
    status: Option<String>,
    expires_at: Option<String>,
}

struct ResetCreditDetails {
    available_count: Option<u64>,
    expires_at: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppRateLimits {
    limit_id: String,
    plan_type: String,
    primary: Option<AppRateLimitWindow>,
    secondary: Option<AppRateLimitWindow>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppRateLimitWindow {
    used_percent: f64,
    window_duration_mins: u64,
    resets_at: i64,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn cache_path() -> Option<PathBuf> {
    let d = dirs::cache_dir()?.join("codexscope");
    let _ = std::fs::create_dir_all(&d);
    Some(d.join("account_usage.json"))
}

fn rate_limits_cache_path() -> Option<PathBuf> {
    let d = dirs::cache_dir()?.join("codexscope");
    let _ = std::fs::create_dir_all(&d);
    Some(d.join("account_rate_limits.json"))
}

fn rate_limit_history_path() -> Option<PathBuf> {
    let d = dirs::cache_dir()?.join("codexscope");
    let _ = std::fs::create_dir_all(&d);
    Some(d.join("account_rate_limit_history.json"))
}

fn auth_path() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        return Some(PathBuf::from(home).join("auth.json"));
    }
    Some(dirs::home_dir()?.join(".codex").join("auth.json"))
}

pub fn load_cached() -> Option<AccountUsage> {
    let t = std::fs::read_to_string(cache_path()?).ok()?;
    serde_json::from_str::<CachedAccountUsage>(&t)
        .ok()
        .map(|c| c.usage)
}

pub fn load_cached_rate_limits() -> Option<RateLimitSnapshot> {
    let t = std::fs::read_to_string(rate_limits_cache_path()?).ok()?;
    serde_json::from_str::<CachedRateLimits>(&t)
        .ok()
        .map(|c| c.rate_limits)
}

pub fn load_rate_limit_history() -> Vec<RateLimitHistoryPoint> {
    let _guard = RATE_LIMIT_HISTORY_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let Some(path) = rate_limit_history_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let cutoff = now_ms() - RATE_LIMIT_HISTORY_RETENTION_MS;
    serde_json::from_str::<CachedRateLimitHistory>(&text)
        .map(|history| {
            history
                .points
                .into_iter()
                .filter(|point| point.timestamp_ms >= cutoff)
                .collect()
        })
        .unwrap_or_default()
}

pub fn rate_limits_cache_is_stale(max_age_ms: i64) -> bool {
    let Some(path) = rate_limits_cache_path() else {
        return true;
    };
    let Ok(t) = std::fs::read_to_string(path) else {
        return true;
    };
    serde_json::from_str::<CachedRateLimits>(&t)
        .map(|c| now_ms() - c.fetched_at_ms >= max_age_ms)
        .unwrap_or(true)
}

fn save_cached(usage: &AccountUsage) {
    let Some(path) = cache_path() else {
        return;
    };
    let cached = CachedAccountUsage {
        fetched_at_ms: now_ms(),
        usage: usage.clone(),
    };
    if let Ok(t) = serde_json::to_string(&cached) {
        let _ = std::fs::write(path, t);
    }
}

fn save_cached_rate_limits(rate_limits: &RateLimitSnapshot) {
    let Some(path) = rate_limits_cache_path() else {
        return;
    };
    let cached = CachedRateLimits {
        fetched_at_ms: now_ms(),
        rate_limits: rate_limits.clone(),
    };
    if let Ok(t) = serde_json::to_string(&cached) {
        let _ = std::fs::write(path, t);
    }
    append_rate_limit_history(rate_limits, cached.fetched_at_ms);
}

fn append_rate_limit_history(rate_limits: &RateLimitSnapshot, timestamp_ms: i64) {
    let Some(primary) = rate_limits.primary.as_ref() else {
        return;
    };
    let _guard = RATE_LIMIT_HISTORY_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let Some(path) = rate_limit_history_path() else {
        return;
    };
    let mut history = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<CachedRateLimitHistory>(&text).ok())
        .unwrap_or_default();
    let cutoff = timestamp_ms - RATE_LIMIT_HISTORY_RETENTION_MS;
    history.points.retain(|point| point.timestamp_ms >= cutoff);
    let point = RateLimitHistoryPoint {
        timestamp_ms,
        remaining_percent: (100.0 - primary.used_percent).clamp(0.0, 100.0),
        secondary_remaining_percent: rate_limits
            .secondary
            .as_ref()
            .map(|secondary| (100.0 - secondary.used_percent).clamp(0.0, 100.0)),
        local_cumulative_tokens: None,
        local_cumulative_cost: None,
    };
    // A manual refresh can land immediately after the background refresh.
    // Replace that sample instead of drawing duplicate points at the same time.
    if history
        .points
        .last()
        .map(|last| timestamp_ms - last.timestamp_ms < 30_000)
        .unwrap_or(false)
    {
        if let Some(last) = history.points.last_mut() {
            *last = point;
        }
    } else {
        history.points.push(point);
    }
    compact_rate_limit_history(&mut history.points, timestamp_ms);
    if let Ok(text) = serde_json::to_string(&history) {
        let _ = std::fs::write(path, text);
    }
}

/// Keep high-resolution samples for recent charts, then progressively compact
/// old history so month/year views remain available without growing the cache
/// by hundreds of thousands of two-minute samples.
fn compact_rate_limit_history(points: &mut Vec<RateLimitHistoryPoint>, now: i64) {
    const HOUR_MS: i64 = 60 * 60 * 1000;
    const DAY_MS: i64 = 24 * HOUR_MS;
    let raw_cutoff = now - 48 * HOUR_MS;
    let hourly_cutoff = now - 60 * DAY_MS;
    let mut compacted: Vec<RateLimitHistoryPoint> = Vec::with_capacity(points.len());
    let mut last_bucket: Option<(u8, i64)> = None;

    for point in points.drain(..) {
        let bucket = if point.timestamp_ms >= raw_cutoff {
            // Every recent sample is its own bucket.
            (2, point.timestamp_ms)
        } else if point.timestamp_ms >= hourly_cutoff {
            (1, point.timestamp_ms.div_euclid(HOUR_MS))
        } else {
            (0, point.timestamp_ms.div_euclid(DAY_MS))
        };
        if last_bucket == Some(bucket) {
            if let Some(last) = compacted.last_mut() {
                *last = point;
            }
        } else {
            compacted.push(point);
            last_bucket = Some(bucket);
        }
    }
    *points = compacted;
}

#[cfg(not(target_os = "windows"))]
fn codex_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("CODEX_BINARY") {
        return PathBuf::from(p);
    }
    let bundled = PathBuf::from("/Applications/Codex.app/Contents/Resources/codex");
    if bundled.exists() {
        return bundled;
    }
    PathBuf::from("codex")
}

#[cfg(target_os = "windows")]
fn windows_codex_cmd() -> PathBuf {
    if let Some(p) = std::env::var_os("CODEX_BINARY") {
        return PathBuf::from(p);
    }
    let mut where_command = Command::new("where.exe");
    where_command.creation_flags(CREATE_NO_WINDOW);
    if let Ok(output) = where_command.arg("codex.cmd").output() {
        if output.status.success() {
            if let Some(line) = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
            {
                return PathBuf::from(line);
            }
        }
    }
    PathBuf::from("codex.cmd")
}

fn app_server_command() -> Command {
    #[cfg(target_os = "windows")]
    {
        // npm installs Codex as codex.cmd on Windows. Launch its JavaScript
        // entrypoint with node.exe directly: std::process::Command cannot run a
        // batch script, cmd.exe quoting is brittle, and the Microsoft Store
        // package's private codex.exe is not executable by external apps.
        let script = windows_codex_cmd();
        if let Some(parent) = script.parent() {
            let entry = parent
                .join("node_modules")
                .join("@openai")
                .join("codex")
                .join("bin")
                .join("codex.js");
            if entry.exists() {
                let mut command = Command::new("node.exe");
                command.creation_flags(CREATE_NO_WINDOW);
                command.arg(entry).args(["app-server", "--stdio"]);
                return command;
            }
        }

        // Last-resort compatibility path for non-npm wrappers.
        let invocation = format!("\"{}\" app-server --stdio", script.display());
        let mut command = Command::new("cmd.exe");
        command.creation_flags(CREATE_NO_WINDOW);
        command.args(["/D", "/S", "/C", invocation.as_str()]);
        command
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut command = Command::new(codex_bin());
        command.args(["app-server", "--stdio"]);
        command
    }
}

fn wait_for_response(
    rx: &mpsc::Receiver<String>,
    id: u64,
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| format!("timed out waiting for app-server response {id}"))?;
        let line = rx
            .recv_timeout(remaining)
            .map_err(|_| format!("timed out waiting for app-server response {id}"))?;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if v.get("id").and_then(|x| x.as_u64()) != Some(id) {
            continue;
        }
        if let Some(err) = v.get("error") {
            return Err(format!("app-server response {id} failed: {err}"));
        }
        return v
            .get("result")
            .cloned()
            .ok_or_else(|| format!("app-server response {id} had no result"));
    }
}

fn query_app_server(method: &str, timeout: Duration) -> Result<serde_json::Value, String> {
    let mut child = app_server_command()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to start codex app-server: {e}"))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "failed to open app-server stdin".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to open app-server stdout".to_string())?;
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });

    let init = json!({
        "id": 1,
        "method": "initialize",
        "params": {
            "clientInfo": {
                "name": "codexscope",
                "title": "CodexScope",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    });
    writeln!(stdin, "{init}").map_err(|e| format!("failed to send initialize: {e}"))?;
    stdin
        .flush()
        .map_err(|e| format!("failed to flush initialize: {e}"))?;
    let _ = wait_for_response(&rx, 1, Duration::from_secs(15))?;

    let req = json!({ "id": 2, "method": method });
    writeln!(stdin, "{req}").map_err(|e| format!("failed to send {method} request: {e}"))?;
    stdin
        .flush()
        .map_err(|e| format!("failed to flush {method} request: {e}"))?;
    let result = wait_for_response(&rx, 2, timeout)?;

    let _ = child.kill();
    let _ = child.wait();
    Ok(result)
}

fn query_usage() -> Result<AccountUsage, String> {
    let result = query_app_server("account/usage/read", Duration::from_secs(45))?;
    serde_json::from_value::<AccountUsage>(result)
        .map_err(|e| format!("failed to decode usage response: {e}"))
}

fn app_window_to_model(w: AppRateLimitWindow) -> RateLimitWindow {
    RateLimitWindow {
        used_percent: w.used_percent,
        window_minutes: w.window_duration_mins,
        resets_at: w.resets_at,
    }
}

fn parse_rfc3339_unix_seconds(s: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp())
}

fn query_reset_credit_details() -> Result<ResetCreditDetails, String> {
    let auth_text = std::fs::read_to_string(
        auth_path().ok_or_else(|| "failed to resolve Codex auth path".to_string())?,
    )
    .map_err(|e| format!("failed to read Codex auth: {e}"))?;
    let auth = serde_json::from_str::<CodexAuth>(&auth_text)
        .map_err(|e| format!("failed to decode Codex auth: {e}"))?;
    let tokens = auth
        .tokens
        .ok_or_else(|| "Codex auth had no tokens".to_string())?;
    let access_token = tokens
        .access_token
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Codex auth had no access token".to_string())?;

    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(15))
        .build();
    let mut req = agent
        .get(RESET_CREDITS_URL)
        .set("Authorization", &format!("Bearer {access_token}"))
        .set("Accept", "application/json")
        .set("originator", "Codex Desktop")
        .set("User-Agent", "CodexScope");
    if let Some(account_id) = tokens.account_id.filter(|s| !s.is_empty()) {
        req = req.set("ChatGPT-Account-ID", &account_id);
    }
    let response = req
        .call()
        .map_err(|e| format!("reset credits request failed: {e}"))?;
    let decoded = response
        .into_json::<WhamResetCredits>()
        .map_err(|e| format!("failed to decode reset credits response: {e}"))?;
    let expires_at = decoded
        .credits
        .unwrap_or_default()
        .into_iter()
        .filter(|c| c.status.as_deref() == Some("available"))
        .filter_map(|c| c.expires_at.as_deref().and_then(parse_rfc3339_unix_seconds))
        .min();

    Ok(ResetCreditDetails {
        available_count: decoded.available_count,
        expires_at,
    })
}

fn query_rate_limits() -> Result<RateLimitSnapshot, String> {
    let result = query_app_server("account/rateLimits/read", Duration::from_secs(15))?;
    let decoded = serde_json::from_value::<RateLimitsReadResult>(result)
        .map_err(|e| format!("failed to decode rate limits response: {e}"))?;
    let mut snapshot = RateLimitSnapshot {
        limit_id: decoded.rate_limits.limit_id,
        plan_type: decoded.rate_limits.plan_type,
        primary: decoded.rate_limits.primary.map(app_window_to_model),
        secondary: decoded.rate_limits.secondary.map(app_window_to_model),
        reset_credits_available: decoded
            .rate_limit_reset_credits
            .and_then(|r| r.available_count),
        reset_credits_expires_at: None,
    };
    // The app-server response already contains the count. Only call the slower
    // HTTP details endpoint when there is actually a credit whose expiry is
    // useful to display.
    if snapshot.reset_credits_available.unwrap_or(0) > 0 {
        if let Ok(details) = query_reset_credit_details() {
            snapshot.reset_credits_available =
                details.available_count.or(snapshot.reset_credits_available);
            snapshot.reset_credits_expires_at = details.expires_at;
        }
    }
    Ok(snapshot)
}

pub fn refresh_cache() -> Option<AccountUsage> {
    let now = now_ms();
    if now - LAST_REFRESH_ATTEMPT_MS.load(Ordering::Relaxed) < REFRESH_RETRY_MS {
        return load_cached();
    }
    let Ok(_guard) = REFRESH_LOCK.try_lock() else {
        return load_cached();
    };
    LAST_REFRESH_ATTEMPT_MS.store(now, Ordering::Relaxed);
    match query_usage() {
        Ok(usage) => {
            save_cached(&usage);
            Some(usage)
        }
        Err(_) => load_cached(),
    }
}

fn refresh_rate_limits_cache_inner(force: bool) -> Option<RateLimitSnapshot> {
    let now = now_ms();
    if !force
        && now - LAST_RATE_LIMIT_REFRESH_ATTEMPT_MS.load(Ordering::Relaxed)
            < RATE_LIMIT_REFRESH_RETRY_MS
    {
        return load_cached_rate_limits();
    }
    let Ok(_guard) = RATE_LIMIT_REFRESH_LOCK.try_lock() else {
        return load_cached_rate_limits();
    };
    LAST_RATE_LIMIT_REFRESH_ATTEMPT_MS.store(now, Ordering::Relaxed);
    match query_rate_limits() {
        Ok(rate_limits) => {
            save_cached_rate_limits(&rate_limits);
            Some(rate_limits)
        }
        Err(_) => load_cached_rate_limits(),
    }
}

pub fn refresh_rate_limits_cache() -> Option<RateLimitSnapshot> {
    refresh_rate_limits_cache_inner(false)
}

pub fn force_refresh_rate_limits_cache() -> Option<RateLimitSnapshot> {
    refresh_rate_limits_cache_inner(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a signed-in local Codex installation"]
    fn live_rate_limits_can_be_read() {
        let limits = query_rate_limits().expect("Codex rate limits should be readable");
        assert_eq!(limits.limit_id, "codex");
        assert!(limits.primary.is_some());
    }
}
