// Shared data structures returned to the frontend.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize)]
pub struct SeriesPoint {
    pub label: String, // sparse axis label (many empty)
    pub full: String,  // complete label for the hover tooltip (hour / date)
    pub input: f64,    // M tokens (uncached new input)
    pub cache: f64,    // M tokens (cache creation + read)
    pub output: f64,   // M tokens
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelStat {
    pub name: String,
    pub vendor: String,
    pub tokens: f64, // M tokens (input+output, weighted)
    pub cost: f64,   // USD estimate
    pub color: String,
    pub priced: bool, // false = no pricing data (cost is unknown, not $0)
}

#[derive(Debug, Clone, Serialize)]
pub struct NamedCount {
    pub name: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Metrics {
    #[serde(rename = "totalTokens")]
    pub total_tokens: f64,
    #[serde(rename = "inputTokens")]
    pub input_tokens: f64,
    #[serde(rename = "cacheTokens")]
    pub cache_tokens: f64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: f64,
    pub cost: f64,
    #[serde(rename = "toolCalls")]
    pub tool_calls: u64,
    pub requests: u64,
    pub sessions: u64,
    #[serde(rename = "deltaTokens")]
    pub delta_tokens: f64,
    #[serde(rename = "deltaCost")]
    pub delta_cost: f64,
    #[serde(rename = "uniqueTools")]
    pub unique_tools: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PeriodReport {
    pub metrics: Metrics,
    pub series: Vec<SeriesPoint>,
    pub models: Vec<ModelStat>,
    pub tools: Vec<NamedCount>,
    #[serde(rename = "reqTrend")]
    pub req_trend: Vec<f64>,
    #[serde(rename = "costTrend")]
    pub cost_trend: Vec<f64>,
    #[serde(rename = "sessionTrend")]
    pub session_trend: Vec<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HeatDay {
    pub date: String, // ISO yyyy-mm-dd
    pub tokens: f64,  // M tokens
    pub level: u8,    // 0..4
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitWindow {
    #[serde(rename = "usedPercent")]
    pub used_percent: f64,
    #[serde(rename = "windowMinutes")]
    pub window_minutes: u64,
    #[serde(rename = "resetsAt")]
    pub resets_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitSnapshot {
    #[serde(rename = "limitId")]
    pub limit_id: String,
    #[serde(rename = "planType")]
    pub plan_type: String,
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
    #[serde(rename = "resetCreditsAvailable")]
    pub reset_credits_available: Option<u64>,
    #[serde(rename = "resetCreditsExpiresAt")]
    pub reset_credits_expires_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitHistoryPoint {
    #[serde(rename = "timestampMs")]
    pub timestamp_ms: i64,
    #[serde(rename = "remainingPercent")]
    pub remaining_percent: f64,
    #[serde(rename = "secondaryRemainingPercent")]
    pub secondary_remaining_percent: Option<f64>,
    #[serde(default, rename = "localCumulativeTokens", skip_serializing_if = "Option::is_none")]
    pub local_cumulative_tokens: Option<f64>,
    #[serde(default, rename = "localCumulativeCost", skip_serializing_if = "Option::is_none")]
    pub local_cumulative_cost: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProfileStats {
    pub source: String,
    #[serde(rename = "cumulativeTokens")]
    pub cumulative_tokens: f64,
    #[serde(rename = "peakDayTokens")]
    pub peak_day_tokens: f64,
    #[serde(rename = "longestTaskMinutes")]
    pub longest_task_minutes: u64,
    #[serde(rename = "currentStreakDays")]
    pub current_streak_days: u64,
    #[serde(rename = "longestStreakDays")]
    pub longest_streak_days: u64,
    #[serde(rename = "lowEffortPercent")]
    pub low_effort_percent: f64,
    #[serde(rename = "topEffort")]
    pub top_effort: String,
    #[serde(rename = "exploredTools")]
    pub explored_tools: u64,
    #[serde(rename = "totalToolRuns")]
    pub total_tool_runs: u64,
    #[serde(rename = "totalSessions")]
    pub total_sessions: u64,
    #[serde(rename = "topTools")]
    pub top_tools: Vec<NamedCount>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Dashboard {
    pub hour: PeriodReport,
    pub day: PeriodReport,
    pub week: PeriodReport,
    pub month: PeriodReport,
    pub year: PeriodReport,
    pub heatmap: Vec<HeatDay>,
    pub profile: ProfileStats,
    #[serde(rename = "todayTokens")]
    pub today_tokens: f64, // M tokens, for the tray label
    #[serde(rename = "generatedAt")]
    pub generated_at: String,
    #[serde(rename = "rateLimits")]
    pub rate_limits: Option<RateLimitSnapshot>,
    #[serde(rename = "quotaTrend")]
    pub quota_trend: Vec<RateLimitHistoryPoint>,
}
