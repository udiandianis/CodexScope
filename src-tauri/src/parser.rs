// Parse Codex rollout JSONL, aggregate token_count events and tool calls into
// Hour / Day / Week / Month / Year reports + a daily heatmap.
use crate::account_usage::AccountUsage;
use crate::model::*;
use crate::pricing::Pricing;
use crate::store::{RateLimitEvent, RawEvent, Store};
use chrono::{DateTime, Datelike, Duration, Local, TimeZone, Timelike};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

// Serializes dashboard builds so the background refresh thread and the command
// handler never touch the incremental cache files concurrently.
static BUILD_LOCK: Mutex<()> = Mutex::new(());

// One Codex usage or tool-call event, with pricing applied.
struct Event {
    ts: DateTime<Local>,
    session: String,
    model: String,
    input: f64,         // raw tokens, uncached new input only
    cache: f64,         // raw tokens, cache creation + read
    output: f64,        // raw tokens
    cost: f64,          // USD (differentiated by token type), 0 if unknown model
    priced: bool,       // whether a price was found for this model
    effort: String,     // Codex reasoning effort from turn_context, when present
    tools: Vec<String>, // Codex tool-call names
}

// Top-5 models keep the Codex-blue depth scale; everything beyond is uniform gray.
const PALETTE: &[&str] = &["#116DC4", "#238BEF", "#339CFF", "#8DCAFF", "#C5E4FF"];
const OVERFLOW_GRAY: &str = "#6E7F90";

/// Strip a trailing "-YYYYMMDD" date suffix so dated releases merge into
/// their base model (e.g. "gpt-5-codex-20260201" -> "gpt-5-codex").
const MONTHS: [&str; 12] = [
    "1月", "2月", "3月", "4月", "5月", "6月", "7月", "8月", "9月", "10月", "11月", "12月",
];

fn normalize_model(name: &str) -> String {
    let name = name.trim();
    if name.eq_ignore_ascii_case("codex") {
        return "unknown".to_string();
    }
    if let Some(idx) = name.rfind('-') {
        let suffix = &name[idx + 1..];
        if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
            return name[..idx].to_string();
        }
    }
    name.to_string()
}

fn vendor_of(model: &str) -> &'static str {
    let m = model.to_lowercase();
    if m.contains("claude") {
        "Anthropic"
    } else if m.contains("gpt") || m.contains("o1") || m.contains("o3") || m.contains("codex") {
        "OpenAI"
    } else if m.contains("gemini") {
        "Google"
    } else if m.contains("llama") {
        "Local"
    } else if m.contains("glm") {
        "Zhipu"
    } else if m.contains("deepseek") {
        "DeepSeek"
    } else {
        "Other"
    }
}

// Load the incrementally maintained event cache. Callers must hold BUILD_LOCK.
fn load_store() -> Store {
    // 1. Ingest incrementally (full scan only on first run; afterwards just the
    //    appended lines), prune events older than the heatmap window, and persist
    //    only when something actually changed — so an idle tick doesn't rewrite
    //    the entire events.json every 30s.
    let mut store = Store::load();
    let mut dirty = store.ingest();
    // Year comparisons need the current and previous calendar year. Keep a
    // little over two years so January of the previous year remains available
    // even at the end of the current year.
    let cutoff = (Local::now() - Duration::days(760)).timestamp_millis();
    if store.prune_before(cutoff) {
        dirty = true;
    }
    if dirty {
        store.save();
    }
    store
}

pub fn build_dashboard() -> Dashboard {
    let _guard = BUILD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let store = load_store();

    // 2. Aggregate: apply current prices and slice by current time.
    // Memoized price table (cheap clone); loaded/refreshed off-thread elsewhere
    // so neither parsing nor the network runs while we hold BUILD_LOCK.
    let pricing = Pricing::shared();
    let events: Vec<Event> = store
        .events
        .iter()
        .map(|r| compute_event(r, &pricing))
        .collect();
    let account_usage = crate::account_usage::load_cached();
    let rate_limits = crate::account_usage::load_cached_rate_limits()
        .or_else(|| latest_rate_limits(&store.events));
    let mut quota_trend = crate::account_usage::load_rate_limit_history();

    let now = Local::now();
    if quota_trend.is_empty() {
        if let Some(primary) = rate_limits.as_ref().and_then(|limits| limits.primary.as_ref()) {
            quota_trend.push(RateLimitHistoryPoint {
                timestamp_ms: now.timestamp_millis(),
                remaining_percent: (100.0 - primary.used_percent).clamp(0.0, 100.0),
                secondary_remaining_percent: rate_limits
                    .as_ref()
                    .and_then(|limits| limits.secondary.as_ref())
                    .map(|secondary| (100.0 - secondary.used_percent).clamp(0.0, 100.0)),
                local_cumulative_tokens: None,
                local_cumulative_cost: None,
            });
        }
    }
    attach_local_cumulative_usage(&events, &mut quota_trend);
    let today = now.date_naive();

    let hour = report_hour(&events, now);
    let day = report_day(&events, now);
    let week = report_week(&events, now);
    let month = report_month(&events, now);
    let year = report_year(&events, now);
    let heatmap = build_heatmap(&events, today, account_usage.as_ref());
    let profile = build_profile(&events, today, account_usage.as_ref());

    // today's displayed tokens (M) for the tray
    let local_today_tokens: f64 = events
        .iter()
        .filter(|e| e.ts.date_naive() == today)
        .map(|e| (e.input + e.cache + e.output) / 1e6)
        .sum();
    let today_tokens = account_usage
        .as_ref()
        .and_then(|u| account_tokens_for_day(u, today))
        .unwrap_or(local_today_tokens);

    Dashboard {
        hour,
        day,
        week,
        month,
        year,
        heatmap,
        profile,
        today_tokens,
        generated_at: now.to_rfc3339(),
        rate_limits,
        quota_trend,
    }
}

/// Build one historical report using the same bucket and comparison semantics
/// as the live dashboard. Negative offsets move backward by one selected unit;
/// zero is the current period. The cache retains about 760 days, so offsets are
/// clamped to the available history horizon.
pub fn build_period_report(period: &str, offset: i32) -> PeriodReport {
    let _guard = BUILD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let store = load_store();
    let pricing = Pricing::shared();
    let events: Vec<Event> = store
        .events
        .iter()
        .map(|raw| compute_event(raw, &pricing))
        .collect();
    let now = Local::now();
    let offset = match period {
        "Hour" => offset.clamp(-18_240, 0),
        "Day" => offset.clamp(-760, 0),
        "Week" => offset.clamp(-108, 0),
        "Month" => offset.clamp(-25, 0),
        "Year" => offset.clamp(-2, 0),
        _ => 0,
    };
    let anchor = match period {
        "Hour" => now + Duration::hours(offset as i64),
        "Day" => now + Duration::days(offset as i64),
        "Week" => now + Duration::weeks(offset as i64),
        "Month" => {
            let month_index = now.year() * 12 + now.month0() as i32 + offset;
            let year = month_index.div_euclid(12);
            let month = month_index.rem_euclid(12) as u32 + 1;
            Local
                .with_ymd_and_hms(year, month, 1, 12, 0, 0)
                .single()
                .unwrap_or(now)
        }
        "Year" => Local
            .with_ymd_and_hms(now.year() + offset, 1, 1, 12, 0, 0)
            .single()
            .unwrap_or(now),
        _ => now,
    };
    match period {
        "Hour" => report_hour(&events, anchor),
        "Day" => report_day(&events, anchor),
        "Week" => report_week(&events, anchor),
        "Month" => report_month(&events, anchor),
        "Year" => report_year(&events, anchor),
        _ => report_week(&events, anchor),
    }
}

/// Build a report for an arbitrary local time range. The selected range is
/// split into twelve equal buckets and compared with the immediately preceding
/// range of the same duration, so the delta badges retain their normal meaning.
pub fn build_range_report(start_ms: i64, end_ms: i64) -> PeriodReport {
    const BUCKET_COUNT: usize = 12;

    let _guard = BUILD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let store = load_store();
    let pricing = Pricing::shared();
    let events: Vec<Event> = store
        .events
        .iter()
        .map(|raw| compute_event(raw, &pricing))
        .collect();

    let now_ms = Local::now().timestamp_millis();
    let retained_start_ms = now_ms - Duration::days(760).num_milliseconds();
    let end_ms = end_ms.min(now_ms).max(retained_start_ms + 60_000);
    let start_ms = start_ms.max(retained_start_ms).min(end_ms - 60_000);
    range_report(&events, start_ms, end_ms, BUCKET_COUNT)
}

fn range_report(events: &[Event], start_ms: i64, end_ms: i64, bucket_count: usize) -> PeriodReport {
    let duration_ms = (end_ms - start_ms).max(60_000);
    let previous_start_ms = start_ms.saturating_sub(duration_ms);
    let mut agg = Agg::default();
    let mut previous = Agg::default();
    let mut buckets = vec![(0.0f64, 0.0f64, 0.0f64); bucket_count];
    let mut request_buckets = vec![0.0f64; bucket_count];
    let mut cost_buckets = vec![0.0f64; bucket_count];
    let mut session_buckets = vec![HashSet::<String>::new(); bucket_count];

    for event in events {
        let timestamp_ms = event.ts.timestamp_millis();
        if timestamp_ms >= start_ms && timestamp_ms <= end_ms {
            agg.add(event);
            let elapsed_ms = timestamp_ms.saturating_sub(start_ms);
            let index = (((elapsed_ms as i128) * bucket_count as i128) / duration_ms as i128)
                .min((bucket_count - 1) as i128) as usize;
            buckets[index].0 += event.input / 1e6;
            buckets[index].1 += event.cache / 1e6;
            buckets[index].2 += event.output / 1e6;
            if !event.model.is_empty() {
                request_buckets[index] += 1.0;
            }
            if !event.session.is_empty() {
                session_buckets[index].insert(event.session.clone());
            }
            cost_buckets[index] += event.cost;
        } else if timestamp_ms >= previous_start_ms && timestamp_ms < start_ms {
            previous.add(event);
        }
    }

    let series = (0..bucket_count)
        .map(|index| {
            let bucket_start_ms = start_ms
                + ((duration_ms as i128 * index as i128) / bucket_count as i128) as i64;
            let bucket_end_ms = start_ms
                + ((duration_ms as i128 * (index + 1) as i128) / bucket_count as i128) as i64;
            let bucket_start = DateTime::from_timestamp_millis(bucket_start_ms)
                .unwrap_or_default()
                .with_timezone(&Local);
            let bucket_end = DateTime::from_timestamp_millis(bucket_end_ms)
                .unwrap_or_default()
                .with_timezone(&Local);
            let label = if index % 2 == 0 {
                if duration_ms <= Duration::days(2).num_milliseconds() {
                    bucket_start.format("%H:%M").to_string()
                } else if duration_ms <= Duration::days(62).num_milliseconds() {
                    bucket_start.format("%-m/%-d").to_string()
                } else {
                    bucket_start.format("%y/%-m/%-d").to_string()
                }
            } else {
                String::new()
            };
            let full = if duration_ms <= Duration::days(2).num_milliseconds() {
                format!(
                    "{}–{}",
                    bucket_start.format("%-m月%-d日 %H:%M"),
                    bucket_end.format("%-m月%-d日 %H:%M")
                )
            } else {
                format!(
                    "{}–{}",
                    bucket_start.format("%Y年%-m月%-d日 %H:%M"),
                    bucket_end.format("%Y年%-m月%-d日 %H:%M")
                )
            };
            SeriesPoint {
                label,
                full,
                input: buckets[index].0,
                cache: buckets[index].1,
                output: buckets[index].2,
            }
        })
        .collect();

    PeriodReport {
        metrics: agg.metrics(
            pct_delta(
                agg.input + agg.cache + agg.output,
                previous.input + previous.cache + previous.output,
            ),
            pct_delta(agg.cost, previous.cost),
        ),
        series,
        models: agg.models(),
        tools: Agg::named(&agg.tool_counts),
        req_trend: request_buckets,
        cost_trend: cost_buckets,
        session_trend: session_buckets
            .into_iter()
            .map(|sessions| sessions.len() as f64)
            .collect(),
    }
}

/// Attach a monotonic local-token counter to every official quota sample. The
/// frontend can then compare any two samples exactly, including all Token used
/// while the rounded official percentage stayed unchanged.
fn attach_local_cumulative_usage(events: &[Event], points: &mut [RateLimitHistoryPoint]) {
    let mut usage_events: Vec<(i64, f64, f64)> = events
        .iter()
        .map(|event| {
            (
                event.ts.timestamp_millis(),
                (event.input + event.cache + event.output) / 1e6,
                event.cost,
            )
        })
        .collect();
    usage_events.sort_by_key(|event| event.0);
    points.sort_by_key(|point| point.timestamp_ms);

    let mut event_index = 0usize;
    let mut cumulative_tokens = 0.0;
    let mut cumulative_cost = 0.0;
    for point in points {
        while event_index < usage_events.len()
            && usage_events[event_index].0 <= point.timestamp_ms
        {
            cumulative_tokens += usage_events[event_index].1;
            cumulative_cost += usage_events[event_index].2;
            event_index += 1;
        }
        point.local_cumulative_tokens = Some(cumulative_tokens);
        point.local_cumulative_cost = Some(cumulative_cost);
    }
}

fn compute_event(r: &RawEvent, pricing: &Pricing) -> Event {
    let ts = DateTime::from_timestamp_millis(r.ts_ms)
        .unwrap_or_default()
        .with_timezone(&Local);
    let model = normalize_model(&r.model);
    // price lookup uses the raw (possibly dated) id, then the normalized one
    let cost_opt = pricing
        .cost(&r.model, r.in_tok, r.out_tok, r.cc, r.cr)
        .or_else(|| pricing.cost(&model, r.in_tok, r.out_tok, r.cc, r.cr));
    Event {
        ts,
        session: r.session.clone(),
        model,
        input: r.in_tok,
        cache: r.cc + r.cr,
        output: r.out_tok,
        cost: cost_opt.unwrap_or(0.0),
        priced: cost_opt.is_some(),
        effort: r.effort.clone(),
        tools: r.tools.clone(),
    }
}

fn latest_rate_limits(events: &[RawEvent]) -> Option<RateLimitSnapshot> {
    let mut latest_codex: Option<(i64, &RateLimitEvent)> = None;
    let mut latest_any: Option<(i64, &RateLimitEvent)> = None;

    for e in events {
        let Some(r) = e.rate_limit.as_ref() else {
            continue;
        };
        let item = (e.ts_ms, r);
        if latest_any.map(|(ts, _)| e.ts_ms > ts).unwrap_or(true) {
            latest_any = Some(item);
        }
        if r.limit_id == "codex" && latest_codex.map(|(ts, _)| e.ts_ms > ts).unwrap_or(true) {
            latest_codex = Some(item);
        }
    }

    let (_, r) = latest_codex.or(latest_any)?;
    Some(rate_snapshot(r))
}

fn rate_snapshot(r: &RateLimitEvent) -> RateLimitSnapshot {
    fn rate_window_snapshot(w: &crate::store::RateLimitWindowEvent) -> RateLimitWindow {
        RateLimitWindow {
            used_percent: w.used_percent,
            window_minutes: w.window_minutes,
            resets_at: w.resets_at,
        }
    }
    RateLimitSnapshot {
        limit_id: r.limit_id.clone(),
        plan_type: r.plan_type.clone(),
        primary: r.primary.as_ref().map(rate_window_snapshot),
        secondary: r.secondary.as_ref().map(rate_window_snapshot),
        reset_credits_available: None,
        reset_credits_expires_at: None,
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn account_tokens_for_day(usage: &AccountUsage, day: chrono::NaiveDate) -> Option<f64> {
    let key = day.format("%Y-%m-%d").to_string();
    usage
        .daily_usage_buckets
        .iter()
        .find(|b| b.start_date == key)
        .map(|b| b.tokens as f64 / 1e6)
}

fn build_profile(
    events: &[Event],
    today: chrono::NaiveDate,
    account_usage: Option<&AccountUsage>,
) -> ProfileStats {
    let mut total_tokens = 0.0;
    let mut by_day: HashMap<chrono::NaiveDate, f64> = HashMap::new();
    let mut sessions: HashMap<String, (i64, i64)> = HashMap::new();
    let mut effort_counts: HashMap<String, u64> = HashMap::new();
    let mut effort_total = 0u64;
    let mut low_effort = 0u64;
    let mut tool_counts: HashMap<String, u64> = HashMap::new();

    for e in events {
        let tok = e.input + e.cache + e.output;
        if tok > 0.0 {
            total_tokens += tok;
            *by_day.entry(e.ts.date_naive()).or_default() += tok / 1e6;
            if !e.effort.is_empty() {
                effort_total += 1;
                *effort_counts.entry(e.effort.clone()).or_default() += 1;
                if matches!(e.effort.as_str(), "none" | "minimal" | "low") {
                    low_effort += 1;
                }
            }
        }
        if !e.session.is_empty() {
            let ts = e.ts.timestamp_millis();
            sessions
                .entry(e.session.clone())
                .and_modify(|(min_ts, max_ts)| {
                    *min_ts = (*min_ts).min(ts);
                    *max_ts = (*max_ts).max(ts);
                })
                .or_insert((ts, ts));
        }
        for tool in &e.tools {
            *tool_counts.entry(tool.clone()).or_default() += 1;
        }
    }

    let peak_day_tokens = by_day.values().copied().fold(0.0, f64::max);
    let active_days: HashSet<_> = by_day
        .iter()
        .filter_map(|(d, tok)| if *tok > 0.0 { Some(*d) } else { None })
        .collect();

    let mut current_streak = 0u64;
    let mut d = today;
    while active_days.contains(&d) {
        current_streak += 1;
        d -= Duration::days(1);
    }

    let mut days: Vec<_> = active_days.iter().copied().collect();
    days.sort_unstable();
    let mut longest_streak = 0u64;
    let mut run = 0u64;
    let mut prev = None;
    for d in days {
        run = if prev.map(|p| d == p + Duration::days(1)).unwrap_or(false) {
            run + 1
        } else {
            1
        };
        longest_streak = longest_streak.max(run);
        prev = Some(d);
    }

    let longest_task_minutes = sessions
        .values()
        .map(|(min_ts, max_ts)| ((*max_ts - *min_ts).max(0) / 60_000) as u64)
        .max()
        .unwrap_or(0);
    let top_effort = effort_counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(effort, _)| effort)
        .unwrap_or_default();
    let (
        source,
        cumulative_tokens,
        peak_day_tokens,
        longest_task_minutes,
        current_streak_days,
        longest_streak_days,
    ) = if let Some(usage) = account_usage {
        (
            "account".to_string(),
            round2(usage.summary.lifetime_tokens as f64 / 1e6),
            round2(usage.summary.peak_daily_tokens as f64 / 1e6),
            (usage.summary.longest_running_turn_sec / 60) as u64,
            usage.summary.current_streak_days,
            usage.summary.longest_streak_days,
        )
    } else {
        (
            "logs".to_string(),
            round2(total_tokens / 1e6),
            round2(peak_day_tokens),
            longest_task_minutes,
            current_streak,
            longest_streak,
        )
    };

    ProfileStats {
        source,
        cumulative_tokens,
        peak_day_tokens,
        longest_task_minutes,
        current_streak_days,
        longest_streak_days,
        low_effort_percent: if effort_total > 0 {
            round2((low_effort as f64 / effort_total as f64) * 100.0)
        } else {
            0.0
        },
        top_effort,
        explored_tools: tool_counts.len() as u64,
        total_tool_runs: tool_counts.values().sum(),
        total_sessions: sessions.len() as u64,
        top_tools: Agg::named(&tool_counts).into_iter().take(5).collect(),
    }
}

// ── aggregation helpers ────────────────────────────────────────────
#[derive(Default)]
struct Agg {
    input: f64,
    cache: f64,
    output: f64,
    cost: f64,
    requests: u64,
    sessions: HashSet<String>,
    tool_calls: u64,
    model_tok: HashMap<String, f64>,
    model_cost: HashMap<String, f64>,
    model_priced: HashMap<String, bool>,
    tool_counts: HashMap<String, u64>,
}

impl Agg {
    fn add(&mut self, e: &Event) {
        self.input += e.input;
        self.cache += e.cache;
        self.output += e.output;
        self.cost += e.cost;
        if !e.session.is_empty() {
            self.sessions.insert(e.session.clone());
        }
        // Tool-only events carry no model (empty) — they're not LLM requests,
        // so they must not inflate request counts or the model split.
        if !e.model.is_empty() {
            self.requests += 1;
            // model totals keep all token types so shares sum to Total tokens
            *self.model_tok.entry(e.model.clone()).or_default() += e.input + e.cache + e.output;
            *self.model_cost.entry(e.model.clone()).or_default() += e.cost;
            // a model is "priced" if any of its messages had a known price
            *self.model_priced.entry(e.model.clone()).or_default() |= e.priced;
        }
        for s in &e.tools {
            self.tool_calls += 1;
            *self.tool_counts.entry(s.clone()).or_default() += 1;
        }
    }

    fn models(&self) -> Vec<ModelStat> {
        let mut v: Vec<(String, f64, f64)> = self
            .model_tok
            .iter()
            .map(|(k, t)| (k.clone(), *t, *self.model_cost.get(k).unwrap_or(&0.0)))
            .collect();
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        v.into_iter()
            .enumerate()
            .map(|(i, (name, tok, cost))| {
                let priced = *self.model_priced.get(&name).unwrap_or(&false);
                ModelStat {
                    vendor: vendor_of(&name).to_string(),
                    tokens: (tok / 1e6 * 100.0).round() / 100.0,
                    cost: (cost * 100.0).round() / 100.0,
                    color: if i < PALETTE.len() {
                        PALETTE[i]
                    } else {
                        OVERFLOW_GRAY
                    }
                    .to_string(),
                    priced,
                    name,
                }
            })
            .collect()
    }

    fn named(counts: &HashMap<String, u64>) -> Vec<NamedCount> {
        let mut v: Vec<NamedCount> = counts
            .iter()
            .map(|(k, c)| NamedCount {
                name: k.clone(),
                count: *c,
            })
            .collect();
        v.sort_by(|a, b| b.count.cmp(&a.count));
        v
    }

    fn metrics(&self, delta_tokens: f64, delta_cost: f64) -> Metrics {
        Metrics {
            total_tokens: ((self.input + self.cache + self.output) / 1e6 * 100.0).round() / 100.0,
            input_tokens: (self.input / 1e6 * 100.0).round() / 100.0,
            cache_tokens: (self.cache / 1e6 * 100.0).round() / 100.0,
            output_tokens: (self.output / 1e6 * 100.0).round() / 100.0,
            cost: (self.cost * 100.0).round() / 100.0,
            tool_calls: self.tool_calls,
            requests: self.requests,
            sessions: self.sessions.len() as u64,
            delta_tokens,
            delta_cost,
            unique_tools: self.tool_counts.len() as u64,
        }
    }
}

/// Percentage change of `cur` vs `prev`, e.g. +20.0 for a 20% increase,
/// rounded to 2 decimals. Returns 0 when there's no baseline to compare.
fn pct_delta(cur: f64, prev: f64) -> f64 {
    if prev <= 0.0 {
        return 0.0;
    }
    ((cur - prev) / prev * 10000.0).round() / 100.0
}

// ── Hour report: rolling 60 minutes vs the preceding 60 minutes ─────
fn report_hour(events: &[Event], now: DateTime<Local>) -> PeriodReport {
    const BUCKET_MINUTES: i64 = 5;
    const BUCKET_COUNT: usize = 12;

    let start = now - Duration::minutes(60);
    let prev_start = start - Duration::minutes(60);
    let mut agg = Agg::default();
    let mut prev = Agg::default();
    let mut buckets = vec![(0.0f64, 0.0f64, 0.0f64); BUCKET_COUNT];
    let mut req_b = vec![0.0f64; BUCKET_COUNT];
    let mut cost_b = vec![0.0f64; BUCKET_COUNT];
    let mut session_b = vec![HashSet::<String>::new(); BUCKET_COUNT];

    for e in events {
        if e.ts >= start && e.ts <= now {
            agg.add(e);
            let elapsed = e.ts.signed_duration_since(start).num_minutes().max(0);
            let idx = ((elapsed / BUCKET_MINUTES) as usize).min(BUCKET_COUNT - 1);
            buckets[idx].0 += e.input / 1e6;
            buckets[idx].1 += e.cache / 1e6;
            buckets[idx].2 += e.output / 1e6;
            if !e.model.is_empty() {
                req_b[idx] += 1.0;
            }
            if !e.session.is_empty() {
                session_b[idx].insert(e.session.clone());
            }
            cost_b[idx] += e.cost;
        } else if e.ts >= prev_start && e.ts < start {
            prev.add(e);
        }
    }

    let series = (0..BUCKET_COUNT)
        .map(|i| {
            let bucket_start = start + Duration::minutes(i as i64 * BUCKET_MINUTES);
            let bucket_end = bucket_start + Duration::minutes(BUCKET_MINUTES);
            SeriesPoint {
                label: if i % 3 == 0 {
                    bucket_start.format("%H:%M").to_string()
                } else {
                    String::new()
                },
                full: format!(
                    "{}–{}",
                    bucket_start.format("%H:%M"),
                    bucket_end.format("%H:%M")
                ),
                input: buckets[i].0,
                cache: buckets[i].1,
                output: buckets[i].2,
            }
        })
        .collect();

    PeriodReport {
        metrics: agg.metrics(
            pct_delta(
                agg.input + agg.cache + agg.output,
                prev.input + prev.cache + prev.output,
            ),
            pct_delta(agg.cost, prev.cost),
        ),
        series,
        models: agg.models(),
        tools: Agg::named(&agg.tool_counts),
        req_trend: req_b,
        cost_trend: cost_b,
        session_trend: session_b.into_iter().map(|sessions| sessions.len() as f64).collect(),
    }
}

// ── Year report: current calendar year vs previous calendar year ────
fn report_year(events: &[Event], now: DateTime<Local>) -> PeriodReport {
    let current_year = now.year();
    let previous_year = current_year - 1;
    let mut agg = Agg::default();
    let mut prev = Agg::default();
    let mut buckets = vec![(0.0f64, 0.0f64, 0.0f64); 12];
    let mut req_b = vec![0.0f64; 12];
    let mut cost_b = vec![0.0f64; 12];
    let mut session_b = vec![HashSet::<String>::new(); 12];

    for e in events {
        let year = e.ts.year();
        if year == current_year {
            agg.add(e);
            let idx = e.ts.month0() as usize;
            buckets[idx].0 += e.input / 1e6;
            buckets[idx].1 += e.cache / 1e6;
            buckets[idx].2 += e.output / 1e6;
            if !e.model.is_empty() {
                req_b[idx] += 1.0;
            }
            if !e.session.is_empty() {
                session_b[idx].insert(e.session.clone());
            }
            cost_b[idx] += e.cost;
        } else if year == previous_year {
            prev.add(e);
        }
    }

    let series = (0..12usize)
        .map(|i| SeriesPoint {
            label: format!("{}月", i + 1),
            full: format!("{}年{}月", current_year, i + 1),
            input: buckets[i].0,
            cache: buckets[i].1,
            output: buckets[i].2,
        })
        .collect();

    PeriodReport {
        metrics: agg.metrics(
            pct_delta(
                agg.input + agg.cache + agg.output,
                prev.input + prev.cache + prev.output,
            ),
            pct_delta(agg.cost, prev.cost),
        ),
        series,
        models: agg.models(),
        tools: Agg::named(&agg.tool_counts),
        req_trend: req_b,
        cost_trend: cost_b,
        session_trend: session_b.into_iter().map(|sessions| sessions.len() as f64).collect(),
    }
}

// ── Day report: today, 24 hourly buckets ───────────────────────────
fn report_day(events: &[Event], now: DateTime<Local>) -> PeriodReport {
    let today = now.date_naive();
    let yesterday = today - Duration::days(1);
    let mut agg = Agg::default();
    let mut prev = Agg::default();
    let mut buckets = vec![(0.0f64, 0.0f64, 0.0f64); 24]; // (input, cache, output) M
    let mut req_b = vec![0.0f64; 24];
    let mut cost_b = vec![0.0f64; 24];
    let mut session_b = vec![HashSet::<String>::new(); 24];

    for e in events {
        let d = e.ts.date_naive();
        if d == today {
            agg.add(e);
            let h = e.ts.hour() as usize;
            buckets[h].0 += e.input / 1e6;
            buckets[h].1 += e.cache / 1e6;
            buckets[h].2 += e.output / 1e6;
            // Match Agg::add exactly: only the request COUNT excludes model-less
            // (slash-command) events; total cost accumulates unconditionally
            // (those events carry cost 0, so this is identical today).
            if !e.model.is_empty() {
                req_b[h] += 1.0;
            }
            if !e.session.is_empty() {
                session_b[h].insert(e.session.clone());
            }
            cost_b[h] += e.cost;
        } else if d == yesterday {
            prev.add(e);
        }
    }

    let series = (0..24)
        .map(|h| SeriesPoint {
            // axis ticks every 4h, skipping the 00/24 endpoints
            label: if h % 4 == 0 && h != 0 {
                format!("{:02}", h)
            } else {
                String::new()
            },
            full: format!("{:02}:00", h),
            input: buckets[h].0,
            cache: buckets[h].1,
            output: buckets[h].2,
        })
        .collect();

    PeriodReport {
        metrics: agg.metrics(
            pct_delta(
                agg.input + agg.cache + agg.output,
                prev.input + prev.cache + prev.output,
            ),
            pct_delta(agg.cost, prev.cost),
        ),
        series,
        models: agg.models(),
        tools: Agg::named(&agg.tool_counts),
        req_trend: req_b,
        cost_trend: cost_b,
        session_trend: session_b.into_iter().map(|sessions| sessions.len() as f64).collect(),
    }
}

// ── Week report: current calendar week (Mon-Sun) vs previous week ────
fn report_week(events: &[Event], now: DateTime<Local>) -> PeriodReport {
    let today = now.date_naive();
    // Monday of the current week (Mon=0 … Sun=6).
    let start = today - Duration::days(today.weekday().num_days_from_monday() as i64);
    let next_start = start + Duration::days(7);
    let prev_start = start - Duration::days(7);

    let mut agg = Agg::default();
    let mut prev = Agg::default();
    let mut buckets = vec![(0.0f64, 0.0f64, 0.0f64); 7];
    let mut req_b = vec![0.0f64; 7];
    let mut cost_b = vec![0.0f64; 7];
    let mut session_b = vec![HashSet::<String>::new(); 7];

    for e in events {
        let d = e.ts.date_naive();
        if d >= start && d < next_start {
            agg.add(e);
            let idx = (d - start).num_days() as usize;
            if idx < buckets.len() {
                buckets[idx].0 += e.input / 1e6;
                buckets[idx].1 += e.cache / 1e6;
                buckets[idx].2 += e.output / 1e6;
                // Match Agg::add: only the request COUNT excludes model-less
                // events; cost accumulates unconditionally (their cost is 0).
                if !e.model.is_empty() {
                    req_b[idx] += 1.0;
                }
                if !e.session.is_empty() {
                    session_b[idx].insert(e.session.clone());
                }
                cost_b[idx] += e.cost;
            }
        } else if d >= prev_start && d < start {
            prev.add(e);
        }
    }

    let weekday = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];
    let series = (0..7usize)
        .map(|i| {
            let date = start + Duration::days(i as i64);
            let wd = weekday[i];
            SeriesPoint {
                label: wd.to_string(),
                full: format!(
                    "{} {}{}日",
                    wd,
                    MONTHS[(date.month() - 1) as usize],
                    date.day()
                ),
                input: buckets[i].0,
                cache: buckets[i].1,
                output: buckets[i].2,
            }
        })
        .collect();

    PeriodReport {
        metrics: agg.metrics(
            pct_delta(
                agg.input + agg.cache + agg.output,
                prev.input + prev.cache + prev.output,
            ),
            pct_delta(agg.cost, prev.cost),
        ),
        series,
        models: agg.models(),
        tools: Agg::named(&agg.tool_counts),
        req_trend: req_b,
        cost_trend: cost_b,
        session_trend: session_b.into_iter().map(|sessions| sessions.len() as f64).collect(),
    }
}

// ── Month report: current calendar month vs previous calendar month ──
fn report_month(events: &[Event], now: DateTime<Local>) -> PeriodReport {
    use chrono::NaiveDate;
    let today = now.date_naive();
    let (y, m) = (today.year(), today.month());
    let cur_first = NaiveDate::from_ymd_opt(y, m, 1).unwrap();
    let next_first = if m == 12 {
        NaiveDate::from_ymd_opt(y + 1, 1, 1).unwrap()
    } else {
        NaiveDate::from_ymd_opt(y, m + 1, 1).unwrap()
    };
    let (py, pm) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
    let prev_first = NaiveDate::from_ymd_opt(py, pm, 1).unwrap();
    let days_in_month = (next_first - cur_first).num_days() as usize;

    let mut agg = Agg::default();
    let mut prev = Agg::default();
    let mut buckets = vec![(0.0f64, 0.0f64, 0.0f64); days_in_month];
    let mut req_b = vec![0.0f64; days_in_month];
    let mut cost_b = vec![0.0f64; days_in_month];
    let mut session_b = vec![HashSet::<String>::new(); days_in_month];

    for e in events {
        let d = e.ts.date_naive();
        if d >= cur_first && d < next_first {
            agg.add(e);
            let idx = (d - cur_first).num_days() as usize;
            if idx < buckets.len() {
                buckets[idx].0 += e.input / 1e6;
                buckets[idx].1 += e.cache / 1e6;
                buckets[idx].2 += e.output / 1e6;
                // Match Agg::add: only the request COUNT excludes model-less
                // events; cost accumulates unconditionally (their cost is 0).
                if !e.model.is_empty() {
                    req_b[idx] += 1.0;
                }
                if !e.session.is_empty() {
                    session_b[idx].insert(e.session.clone());
                }
                cost_b[idx] += e.cost;
            }
        } else if d >= prev_first && d < cur_first {
            prev.add(e);
        }
    }

    let series = (0..days_in_month)
        .map(|i| {
            let dn = (i + 1) as u32;
            let label = if i == 0 || dn % 5 == 0 {
                dn.to_string()
            } else {
                String::new()
            };
            SeriesPoint {
                label,
                full: format!("{}{}日", MONTHS[(m - 1) as usize], dn),
                input: buckets[i].0,
                cache: buckets[i].1,
                output: buckets[i].2,
            }
        })
        .collect();

    PeriodReport {
        metrics: agg.metrics(
            pct_delta(
                agg.input + agg.cache + agg.output,
                prev.input + prev.cache + prev.output,
            ),
            pct_delta(agg.cost, prev.cost),
        ),
        series,
        models: agg.models(),
        tools: Agg::named(&agg.tool_counts),
        req_trend: req_b,
        cost_trend: cost_b,
        session_trend: session_b.into_iter().map(|sessions| sessions.len() as f64).collect(),
    }
}

// ── Heatmap: last ~26 weeks daily totals ────────────────────────────
fn build_heatmap(
    events: &[Event],
    today: chrono::NaiveDate,
    account_usage: Option<&AccountUsage>,
) -> Vec<HeatDay> {
    let start = today - Duration::days(25 * 7 + today.weekday().num_days_from_sunday() as i64);
    let mut by_day: HashMap<chrono::NaiveDate, f64> = HashMap::new();
    if let Some(usage) = account_usage {
        for bucket in &usage.daily_usage_buckets {
            let Ok(d) = chrono::NaiveDate::parse_from_str(&bucket.start_date, "%Y-%m-%d") else {
                continue;
            };
            if d >= start && d <= today {
                *by_day.entry(d).or_default() += bucket.tokens as f64 / 1e6;
            }
        }
    } else {
        for e in events {
            let d = e.ts.date_naive();
            if d >= start && d <= today {
                *by_day.entry(d).or_default() += (e.input + e.cache + e.output) / 1e6;
            }
        }
    }
    let mut days = Vec::new();
    let mut d = start;
    let mut maxv = 0.0f64;
    while d <= today {
        let t = *by_day.get(&d).unwrap_or(&0.0);
        maxv = maxv.max(t);
        days.push((d, t));
        d += Duration::days(1);
    }
    days.into_iter()
        .map(|(date, tokens)| {
            let f = if maxv > 0.0 { tokens / maxv } else { 0.0 };
            let level = if tokens == 0.0 {
                0
            } else if f < 0.25 {
                1
            } else if f < 0.5 {
                2
            } else if f < 0.75 {
                3
            } else {
                4
            };
            HeatDay {
                date: date.format("%Y-%m-%d").to_string(),
                tokens: (tokens * 100.0).round() / 100.0,
                level,
            }
        })
        .collect()
}
