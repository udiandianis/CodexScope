import { useEffect, useLayoutEffect, useRef, useState, type CSSProperties } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";
import {
  Dashboard, PeriodReport, ModelStat, Theme, TH,
  fetchDashboard, fmtInt, fmtTokens,
} from "./data";
import {
  TokenGlyph, Segmented, BarChart, Sparkline, TrendDetail, CostDonut, Heatmap,
} from "./charts";

// Count up to `target`. Restarts from 0 whenever `resetKey` changes (popover
// open / period switch); on a live value change it eases from the current
// value to the new one instead of snapping back to 0.
function useCountUp(target: number, resetKey: string, active: boolean, duration = 850): number {
  const [val, setVal] = useState(0);
  const valRef = useRef(0);
  const keyRef = useRef<string | null>(null);
  const rafRef = useRef(0);
  // useLayoutEffect so the reset-to-0 is committed *before* the browser paints
  // (otherwise the old/final value flashes for a frame before counting up).
  useLayoutEffect(() => {
    cancelAnimationFrame(rafRef.current);
    const set = (v: number) => { valRef.current = v; setVal(v); };
    // while the popover is hidden, hold at 0 so the next open starts clean
    if (!active) { keyRef.current = null; set(0); return; }
    const reset = keyRef.current !== resetKey;
    keyRef.current = resetKey;
    // open / period switch → start from 0 (paint it now); live update → ease
    // from the current value to the new one.
    let from = valRef.current;
    if (reset) { from = 0; set(0); }
    const start = performance.now();
    const ease = (t: number) => 1 - Math.pow(1 - t, 3); // easeOutCubic
    const tick = (now: number) => {
      const p = Math.min(1, (now - start) / duration);
      set(from + (target - from) * ease(p));
      if (p < 1) rafRef.current = requestAnimationFrame(tick);
    };
    rafRef.current = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(rafRef.current);
  }, [resetKey, target, active, duration]);
  return val;
}

function Delta({ v, theme }: { v: number; theme: Theme }) {
  const up = v >= 0;
  // Usage/cost going up is "bad" → red; going down is "good" → theme accent.
  const col = up ? "#e0795f" : theme.accent;
  return (
    <span style={{ font: `600 10px ${theme.mono}`, color: col, display: "inline-flex", alignItems: "center", gap: 2,
      padding: "1.5px 5px", borderRadius: 5, background: up ? "rgba(224,121,95,0.16)" : "rgba(51,156,255,0.14)" }}>
      {up ? "▲" : "▼"}{Math.abs(Math.round(v))}%
    </span>
  );
}

// Round each value's share to 1 decimal (%) via largest-remainder apportionment,
// so the displayed percentages sum to exactly 100.0% (plain rounding wouldn't).
function sharePcts(values: number[]): number[] {
  const total = values.reduce((s, v) => s + v, 0);
  if (total <= 0) return values.map(() => 0);
  const UNITS = 1000; // work in 0.1% units; target is 100.0%
  const raw = values.map((v) => (v / total) * UNITS);
  const units = raw.map(Math.floor);
  const left = Math.round(UNITS - units.reduce((s, f) => s + f, 0));
  raw
    .map((r, i) => ({ i, frac: r - Math.floor(r) }))
    .sort((a, b) => b.frac - a.frac)
    .slice(0, left)
    .forEach(({ i }) => (units[i] += 1));
  return units.map((u) => u / 10);
}

function ModelRow({ m, max, theme, share }: { m: ModelStat; max: number; theme: Theme; share: number }) {
  // 1-decimal share; whole numbers drop the ".0" (100% not 100.0%).
  const pctStr = share % 1 === 0 ? share.toFixed(0) : share.toFixed(1);
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 9, padding: "5px 0" }}>
      <span style={{ width: 7, height: 7, borderRadius: 2, background: m.color, flex: "0 0 auto" }} />
      <div style={{ minWidth: 0, flex: "0 0 118px" }}>
        <div style={{ font: `500 11.5px ${theme.ui}`, color: theme.text, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{m.name}</div>
      </div>
      <div style={{ flex: 1, height: 5, borderRadius: 3, background: theme.gridLine, overflow: "hidden" }}>
        <div style={{ width: `${(m.tokens / max) * 100}%`, height: "100%", background: m.color, borderRadius: 3 }} />
      </div>
      <span style={{ font: `500 10.5px ${theme.mono}`, color: theme.dim, flex: "0 0 auto", width: 42, textAlign: "right" }}>{fmtTokens(m.tokens)}</span>
      <span style={{ font: `600 10.5px ${theme.mono}`, color: theme.text, flex: "0 0 auto", width: 40, textAlign: "right" }}>{pctStr}%</span>
    </div>
  );
}

function MiniStat({ label, labelExtra, value, sub, theme, accent, children, onClick }:
  { label: string; labelExtra?: React.ReactNode; value: React.ReactNode; sub?: string; theme: Theme; accent?: string; children?: React.ReactNode; onClick?: () => void }) {
  return (
    <div data-no-drag={onClick ? "" : undefined} role={onClick ? "button" : undefined} tabIndex={onClick ? 0 : undefined}
      onClick={onClick}
      onKeyDown={onClick ? (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); onClick(); } } : undefined}
      style={{ background: theme.gridLine, borderRadius: 9, padding: "9px 10px", minWidth: 0,
        cursor: onClick ? "pointer" : undefined, outline: "1px solid transparent",
        transition: "outline-color .16s, background .16s" }}>
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 5 }}>
        <span style={{ display: "inline-flex", alignItems: "center", gap: 5, minWidth: 0 }}>
          <span style={{ font: `500 9.5px ${theme.ui}`, color: theme.dim, letterSpacing: ".04em", textTransform: "uppercase" }}>{label}</span>
          {labelExtra}
        </span>
        {onClick && <span style={{ font: `600 10px ${theme.mono}`, color: theme.faint }}>↗</span>}
      </div>
      <div style={{ display: "flex", alignItems: "flex-end", justifyContent: "space-between", marginTop: 3, gap: 6 }}>
        <span style={{ font: `600 17px/1 ${theme.mono}`, color: accent || theme.text, whiteSpace: "nowrap" }}>{value}</span>
        {children}
      </div>
      {sub && <div style={{ font: `500 9px ${theme.mono}`, color: theme.faint, marginTop: 3 }}>{sub}</div>}
    </div>
  );
}

type TrendKind = "requests" | "cost" | "quota" | "sessions" | "peak";
type PeriodKind = "Hour" | "Day" | "Week" | "Month" | "Year";
type CustomRange = { startMs: number; endMs: number };

function toLocalDateTimeInput(timestampMs: number) {
  const date = new Date(timestampMs);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

function customRangeLabel(range: CustomRange) {
  const start = new Date(range.startMs);
  const end = new Date(range.endMs);
  const md = (date: Date) => `${date.getMonth() + 1}月${date.getDate()}日`;
  const hm = (date: Date) => `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
  return start.toDateString() === end.toDateString()
    ? `${md(start)} ${hm(start)}–${hm(end)}`
    : `${md(start)} ${hm(start)}–${md(end)} ${hm(end)}`;
}

function customRangePeriod(range: CustomRange): PeriodKind {
  const duration = range.endMs - range.startMs;
  if (duration <= 60 * 60 * 1000) return "Hour";
  if (duration <= 24 * 60 * 60 * 1000) return "Day";
  if (duration <= 7 * 24 * 60 * 60 * 1000) return "Week";
  if (duration <= 62 * 24 * 60 * 60 * 1000) return "Month";
  return "Year";
}

function activeBucketCount(period: PeriodKind, generatedAt: string) {
  const now = new Date(generatedAt);
  if (period === "Hour") return 12;
  if (period === "Day") return now.getHours() + 1;
  if (period === "Week") return ((now.getDay() + 6) % 7) + 1;
  if (period === "Month") return now.getDate();
  return now.getMonth() + 1;
}

function minimumPeriodOffset(period: PeriodKind) {
  return { Hour: -18_240, Day: -760, Week: -108, Month: -25, Year: -2 }[period];
}

function periodViewEnd(period: PeriodKind, offset: number, generatedAt: string) {
  const now = new Date(generatedAt);
  if (offset === 0) return now;
  if (period === "Hour") return new Date(now.getTime() + offset * 60 * 60 * 1000);
  if (period === "Day") {
    return new Date(new Date(now.getFullYear(), now.getMonth(), now.getDate() + offset + 1).getTime() - 1);
  }
  if (period === "Week") {
    const mondayOffset = (now.getDay() + 6) % 7;
    const monday = new Date(now.getFullYear(), now.getMonth(), now.getDate() - mondayOffset);
    return new Date(new Date(monday.getFullYear(), monday.getMonth(), monday.getDate() + offset * 7 + 7).getTime() - 1);
  }
  if (period === "Month") {
    return new Date(new Date(now.getFullYear(), now.getMonth() + offset + 1, 1).getTime() - 1);
  }
  return new Date(new Date(now.getFullYear() + offset + 1, 0, 1).getTime() - 1);
}

function periodDisplayLabel(period: PeriodKind, offset: number, generatedAt: string) {
  if (offset === 0) {
    return { Hour: "最近 60 分钟", Day: "今天 00:00 至今", Week: "本周", Month: "本月", Year: "本年" }[period];
  }
  const end = periodViewEnd(period, offset, generatedAt);
  const md = (date: Date) => `${date.getMonth() + 1}月${date.getDate()}日`;
  const hm = (date: Date) => `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
  if (period === "Hour") {
    const start = new Date(end.getTime() - 60 * 60 * 1000);
    return start.toDateString() === end.toDateString()
      ? `${md(end)} ${hm(start)}–${hm(end)}`
      : `${md(start)} ${hm(start)}–${md(end)} ${hm(end)}`;
  }
  if (period === "Day") return md(end);
  if (period === "Week") {
    const start = new Date(periodStartMs("Week", end));
    return start.getMonth() === end.getMonth()
      ? `${start.getMonth() + 1}月${start.getDate()}–${end.getDate()}日`
      : `${md(start)}–${md(end)}`;
  }
  if (period === "Month") return `${end.getFullYear()}年${end.getMonth() + 1}月`;
  return `${end.getFullYear()}年`;
}

function periodStartMs(period: PeriodKind, now: Date) {
  if (period === "Hour") return now.getTime() - 60 * 60 * 1000;
  if (period === "Day") return new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
  if (period === "Week") {
    return new Date(now.getFullYear(), now.getMonth(), now.getDate() - ((now.getDay() + 6) % 7)).getTime();
  }
  if (period === "Month") return new Date(now.getFullYear(), now.getMonth(), 1).getTime();
  return new Date(now.getFullYear(), 0, 1).getTime();
}

function quotaSampleLabel(timestampMs: number, period: PeriodKind) {
  const date = new Date(timestampMs);
  if (period === "Hour" || period === "Day") {
    return date.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit", hour12: false });
  }
  return date.toLocaleString("zh-CN", {
    month: "numeric", day: "numeric",
    ...(period === "Year" ? {} : { hour: "2-digit", minute: "2-digit", hour12: false }),
  });
}

function quotaEfficiency(points: Dashboard["quotaTrend"], period: PeriodKind, generatedAt: string, explicitStartMs?: number) {
  const now = new Date(generatedAt);
  const periodStart = explicitStartMs ?? periodStartMs(period, now);
  const sorted = [...points].filter((point) => point.timestampMs <= now.getTime()).sort((a, b) => a.timestampMs - b.timestampMs);
  const latestPoint = sorted[sorted.length - 1];
  const currentRemaining = latestPoint?.remainingPercent ?? 0;
  const empty = { averageTokens: null, averageCost: null, lastCompletedTokens: null, lastCompletedCost: null,
    currentTokens: 0, currentCost: 0, completedPercent: 0, segmentCosts: [] as number[], segmentLabels: [] as string[],
    currentRemaining };
  if (sorted.length < 2) return empty;

  let segmentStart = sorted[0];
  let completedTokens = 0;
  let completedCost = 0;
  let completedPercent = 0;
  let lastCompletedTokens: number | null = null;
  let lastCompletedCost: number | null = null;
  const segmentCosts: number[] = [];
  const segmentLabels: string[] = [];
  for (let i = 1; i < sorted.length; i++) {
    const previous = sorted[i - 1];
    const current = sorted[i];
    // A quota reset discards the unfinished old segment and starts a new one.
    if (current.remainingPercent > previous.remainingPercent) {
      segmentStart = current;
      continue;
    }
    const drop = previous.remainingPercent - current.remainingPercent;
    if (drop <= 0) continue;
    const startTokens = segmentStart.localCumulativeTokens;
    const endTokens = current.localCumulativeTokens;
    const startCost = segmentStart.localCumulativeCost;
    const endCost = current.localCumulativeCost;
    // Attribute a completed 1% segment to the period in which its official
    // boundary was crossed. The current unfinished segment is never included.
    const completedInPeriod = current.timestampMs >= periodStart;
    if (completedInPeriod && startTokens != null && endTokens != null) {
      const segmentTokens = Math.max(0, endTokens - startTokens);
      completedTokens += segmentTokens;
      lastCompletedTokens = segmentTokens / drop;
    }
    if (completedInPeriod && startCost != null && endCost != null) {
      const segmentCost = Math.max(0, endCost - startCost);
      completedCost += segmentCost;
      lastCompletedCost = segmentCost / drop;
      segmentCosts.push(lastCompletedCost);
      segmentLabels.push(`${Math.round(segmentStart.remainingPercent)}%→${Math.round(current.remainingPercent)}% · ${quotaSampleLabel(current.timestampMs, period)}`);
    }
    if (completedInPeriod) completedPercent += drop;
    segmentStart = current;
  }

  const latest = sorted[sorted.length - 1];
  const currentTokens = segmentStart.localCumulativeTokens != null && latest.localCumulativeTokens != null
    ? Math.max(0, latest.localCumulativeTokens - segmentStart.localCumulativeTokens) : 0;
  const currentCost = segmentStart.localCumulativeCost != null && latest.localCumulativeCost != null
    ? Math.max(0, latest.localCumulativeCost - segmentStart.localCumulativeCost) : 0;
  return {
    averageTokens: completedPercent > 0 ? completedTokens / completedPercent : null,
    averageCost: completedPercent > 0 ? completedCost / completedPercent : null,
    lastCompletedTokens,
    lastCompletedCost,
    currentTokens,
    currentCost,
    completedPercent,
    segmentCosts,
    segmentLabels,
    currentRemaining,
  };
}

function quotaPeriodTrend(points: Dashboard["quotaTrend"], period: PeriodKind, generatedAt: string, count: number, explicitStartMs?: number) {
  const now = new Date(generatedAt);
  const nowMs = now.getTime();
  const sorted = [...points].filter((point) => point.timestampMs <= nowMs).sort((a, b) => a.timestampMs - b.timestampMs);
  if (!sorted.length || count <= 0) return { values: [] as number[], labels: [] as string[], annotations: [] as string[] };
  const requestedStart = explicitStartMs ?? periodStartMs(period, now);
  const beforeStart = [...sorted].reverse().find((point) => point.timestampMs <= requestedStart);
  const firstInside = sorted.find((point) => point.timestampMs >= requestedStart);
  const baseline = beforeStart || firstInside || sorted[0];
  const startMs = beforeStart ? requestedStart : baseline.timestampMs;

  const boundaries: Array<{ timestampMs: number; drop: number; cost: number }> = [];
  let segmentStart = sorted[0];
  for (let i = 1; i < sorted.length; i++) {
    const previous = sorted[i - 1];
    const current = sorted[i];
    if (current.remainingPercent > previous.remainingPercent) {
      segmentStart = current;
      continue;
    }
    const drop = previous.remainingPercent - current.remainingPercent;
    if (drop <= 0) continue;
    const cost = segmentStart.localCumulativeCost != null && current.localCumulativeCost != null
      ? Math.max(0, current.localCumulativeCost - segmentStart.localCumulativeCost) : 0;
    boundaries.push({ timestampMs: current.timestampMs, drop, cost });
    segmentStart = current;
  }

  const values: number[] = [];
  const labels: string[] = [];
  const annotations: string[] = [];
  let previousTarget = startMs - 1;
  for (let i = 0; i < count; i++) {
    const target = count === 1 ? nowMs : startMs + ((nowMs - startMs) * i) / (count - 1);
    let latest = baseline;
    let activeStart = sorted[0];
    for (let j = 0; j < sorted.length; j++) {
      const point = sorted[j];
      if (point.timestampMs > target) break;
      latest = point;
      if (j > 0) {
        const previous = sorted[j - 1];
        if (point.remainingPercent !== previous.remainingPercent) activeStart = point;
      }
    }
    const completed = boundaries.filter((boundary) => boundary.timestampMs > previousTarget && boundary.timestampMs <= target);
    if (completed.length) {
      const completedPercent = completed.reduce((sum, boundary) => sum + boundary.drop, 0);
      const completedCost = completed.reduce((sum, boundary) => sum + boundary.cost, 0);
      annotations.push(completedPercent === 1
        ? `上个 1% $${fmtPeakCost(completedCost)}`
        : `本区间 ${completedPercent.toFixed(0)}% 均 $${fmtPeakCost(completedCost / completedPercent)}`);
    } else {
      const currentCost = activeStart.localCumulativeCost != null && latest.localCumulativeCost != null
        ? Math.max(0, latest.localCumulativeCost - activeStart.localCumulativeCost) : 0;
      annotations.push(`当前 1% 累计 $${fmtPeakCost(currentCost)}`);
    }
    values.push(latest.remainingPercent);
    labels.push(quotaSampleLabel(target, period));
    previousTarget = target;
  }
  return { values, labels, annotations };
}

function TrendOverlay({ kind, report, quotaTrendData, quotaEfficiencyData, peakUnit, bucketCount, periodLabel, theme, onClose }:
  { kind: TrendKind; report: PeriodReport; quotaTrendData: ReturnType<typeof quotaPeriodTrend>; quotaEfficiencyData: ReturnType<typeof quotaEfficiency>; peakUnit: "tokens" | "cost"; bucketCount: number; periodLabel: string; theme: Theme; onClose: () => void }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const peakValues = report.series.slice(0, bucketCount).map((point) => point.input + point.cache + point.output);
  const values = kind === "quota"
    ? quotaTrendData.values
    : kind === "requests"
      ? report.reqTrend.slice(0, bucketCount)
      : kind === "cost"
        ? report.costTrend.slice(0, bucketCount)
        : kind === "sessions"
          ? (report.sessionTrend || []).slice(0, bucketCount)
          : peakUnit === "tokens" ? peakValues : report.costTrend.slice(0, bucketCount);
  const labels = kind === "quota"
    ? quotaTrendData.labels
    : report.series.slice(0, bucketCount).map((s) => s.full);
  const chartValues = values;
  const chartLabels = labels;
  const total = values.reduce((s, v) => s + v, 0);
  const average = values.length ? total / values.length : 0;
  const peak = Math.max(...values, 0);
  const peakIndex = values.indexOf(peak);
  const active = values.filter((v) => v > 0).length;
  const startingRemaining = quotaTrendData.values[0] ?? quotaEfficiencyData.currentRemaining;
  const title = kind === "requests"
    ? "响应次数详细趋势"
    : kind === "cost"
      ? "API 等值费用详细趋势"
      : kind === "sessions"
        ? "会话数详细趋势"
        : kind === "peak"
          ? "阶段峰值详细趋势"
          : "剩余额度详细趋势";
  const formatValue = kind === "requests"
    ? (v: number, _index?: number) => `${fmtInt(Math.round(v))}次`
    : kind === "cost"
      ? (v: number, _index?: number) => v > 0 && v < 0.01 ? `$${v.toFixed(4)}` : `$${v.toFixed(2)}`
      : kind === "sessions"
        ? (v: number, _index?: number) => `${fmtInt(Math.round(v))}个`
        : kind === "peak"
          ? peakUnit === "tokens"
            ? (v: number, _index?: number) => fmtPeakTokens(v)
            : (v: number, _index?: number) => `$${fmtPeakCost(v)}`
          : (v: number, index = 0) => `${Math.round(v)}% · ${quotaTrendData.annotations[index] || ""}`;
  const summary = kind === "quota" ? [
    ["当前额度", `${Math.round(quotaEfficiencyData.currentRemaining)}%`],
    ["阶段起始", `${Math.round(startingRemaining)}%`],
    ["已完成 1% 均值", quotaEfficiencyData.averageCost != null ? `$${fmtPeakCost(quotaEfficiencyData.averageCost)}` : "待累计"],
    ["当前 1% 累计", quotaEfficiencyData.currentCost > 0 ? `$${fmtPeakCost(quotaEfficiencyData.currentCost)}` : "$0.00"],
  ] : kind === "sessions" ? [
    ["期间去重", `${fmtInt(report.metrics.sessions)}个`],
    ["桶均", `${average.toFixed(2)}个`],
    ["峰值", formatValue(peak, peakIndex)],
    ["活跃", `${active}/${values.length}`],
  ] : kind === "peak" ? [
    ["总计", formatValue(total)],
    ["桶均", formatValue(average)],
    ["峰值", formatValue(peak, peakIndex)],
    ["活跃", `${active}/${values.length}`],
  ] : [
    ["总计", formatValue(total)],
    ["平均", formatValue(average)],
    ["峰值", formatValue(peak)],
    ["活跃", `${active}/${values.length}`],
  ];
  const displayPeriod = periodLabel;
  const detailTitle = kind === "quota"
    ? labels[0] ? `阶段起点：${labels[0]} · ${Math.round(startingRemaining)}%` : displayPeriod
    : labels[peakIndex] ? `峰值区间：${labels[peakIndex]}` : displayPeriod;

  return <>
    <div data-no-drag="" onClick={onClose} style={{ position: "absolute", inset: 0, zIndex: 30, borderRadius: 12,
      background: "rgba(0,0,0,0.42)", backdropFilter: "blur(1px)" }} />
    <div data-no-drag="" role="dialog" aria-modal="true" aria-label={title} onClick={(e) => e.stopPropagation()}
      style={{ position: "absolute", zIndex: 31, left: 8, right: 8, top: "50%", transform: "translateY(-50%)",
        borderRadius: 13, padding: "12px 12px 11px", background: theme.card,
        border: `1px solid ${theme.segBorder}`, boxShadow: "0 18px 50px rgba(0,0,0,0.5)" }}>
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 8, marginBottom: 9 }}>
        <div>
          <div style={{ font: `700 13px ${theme.ui}`, color: theme.text }}>{title}</div>
          <div style={{ marginTop: 2, font: `500 8.5px ${theme.mono}`, color: theme.faint }}>
            {displayPeriod}{kind === "quota" ? ` · ${values.length}个额度节点` : ""} · 悬停折线查看每个区间
          </div>
        </div>
        <button onClick={onClose} aria-label="关闭详细趋势" style={{ width: 26, height: 26, border: 0, borderRadius: 7,
          background: theme.gridLine, color: theme.dim, cursor: "pointer", font: `600 15px ${theme.ui}` }}>×</button>
      </div>
      <div style={{ display: "grid", gridTemplateColumns: "repeat(4, minmax(0, 1fr))", gap: 5, marginBottom: 8 }}>
        {summary.map(([label, value]) => <div key={label} style={{ minWidth: 0, borderRadius: 8, padding: "7px 7px 6px", background: theme.segBg }}>
          <div style={{ font: `500 8px ${theme.ui}`, color: theme.faint }}>{label}</div>
          <div style={{ marginTop: 3, font: `700 11px ${theme.mono}`, color: theme.text, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{value}</div>
        </div>)}
      </div>
      <TrendDetail title={detailTitle}
        values={chartValues} labels={chartLabels} theme={theme} accent={theme.accent} formatValue={formatValue} />
    </div>
  </>;
}

function remainingPct(used?: number) {
  if (typeof used !== "number" || !Number.isFinite(used)) return 0;
  return Math.max(0, Math.min(100, 100 - used));
}

function fmtProfileTokens(m: number) {
  return fmtTokens(m);
}

function fmtPeakCost(value: number) {
  return Math.max(0, value).toFixed(2);
}

function fmtPeakTokens(millions: number) {
  if (millions >= 1000) return `${(millions / 1000).toFixed(2)}B`;
  if (millions >= 1) return `${millions.toFixed(2)}M`;
  return `${(millions * 1000).toFixed(2)}K`;
}

// Input/Cached/Output legend: full words by default, abbreviated only
// when the row would otherwise overflow the available width.
function SplitLegend({ t, inputM, cacheM, outputM }:
  { t: Theme; inputM: number; cacheM: number; outputM: number }) {
  const ref = useRef<HTMLDivElement>(null);
  const [compact, setCompact] = useState(false);
  const key = `${inputM}|${cacheM}|${outputM}`;
  const parts = [
    { label: "输入", compact: "入", color: t.accent, value: inputM },
    ...(cacheM > 0 ? [{ label: "缓存", compact: "缓", color: t.cacheCol, value: cacheM }] : []),
    { label: "输出", compact: "出", color: t.accentSoft, value: outputM },
  ];
  // reset to full labels whenever the numbers change, then re-measure
  useLayoutEffect(() => { setCompact(false); }, [key]);
  useLayoutEffect(() => {
    const el = ref.current;
    if (el && !compact && el.scrollWidth > el.clientWidth + 1) setCompact(true);
  });
  return (
    <div ref={ref} style={{
      display: "flex", alignItems: "center", gap: 10,
      font: `500 10px ${t.mono}`, color: t.dim, marginBottom: 14, whiteSpace: "nowrap", overflow: "hidden",
    }}>
      {parts.map((p) => (
        <span key={p.label}><span style={{ color: p.color }}>●</span> {compact ? p.compact : p.label} {p.value.toFixed(2)}M</span>
      ))}
    </div>
  );
}

const SectionRule = ({ t, m = "12px 0 10px" }: { t: Theme; m?: string }) => (
  <div style={{ height: 1, background: t.gridLine, margin: m }} />
);
const Label = ({ t, children }: { t: Theme; children: React.ReactNode }) => (
  <span style={{ font: `600 10px ${t.ui}`, color: t.dim, letterSpacing: ".05em", textTransform: "uppercase", whiteSpace: "nowrap" }}>{children}</span>
);

function ThemeToggle({ pref, theme, onCycle }: { pref: "dark" | "light" | "system"; theme: Theme; onCycle: () => void }) {
  const t = theme;
  // Single button cycling Dark → Light → System; the icon shows the current mode.
  const label = pref === "system" ? "跟随系统" : pref === "dark" ? "深色" : "浅色";
  return (
    <button onClick={onCycle} title={`主题：${label}（点击切换）`} aria-label={`主题：${label}`} style={{
      display: "inline-flex", alignItems: "center", justifyContent: "center",
      width: 26, height: 26, borderRadius: 7, cursor: "pointer", padding: 0,
      background: t.segBg, border: `1px solid ${t.segBorder}`, color: t.dim,
    }}>
      {pref === "light" ? (
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke={t.dim} strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <circle cx="12" cy="12" r="4.2" />
          <path d="M12 2.5v2.2M12 19.3v2.2M2.5 12h2.2M19.3 12h2.2M5.1 5.1l1.6 1.6M17.3 17.3l1.6 1.6M18.9 5.1l-1.6 1.6M6.7 17.3l-1.6 1.6" />
        </svg>
      ) : pref === "dark" ? (
        <svg width="14" height="14" viewBox="0 0 24 24" fill={t.dim} stroke="none">
          <path d="M21 12.9A9 9 0 1 1 11.1 3a7.2 7.2 0 0 0 9.9 9.9z" />
        </svg>
      ) : (
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke={t.dim} strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <rect x="3" y="4.5" width="18" height="12.5" rx="1.6" />
          <path d="M8.5 20.5h7M12 17v3.5" />
        </svg>
      )}
    </button>
  );
}

function CustomRangeOverlay({ initialRange, theme, onApply, onClose }: {
  initialRange: CustomRange;
  theme: Theme;
  onApply: (range: CustomRange) => void;
  onClose: () => void;
}) {
  const [start, setStart] = useState(() => toLocalDateTimeInput(initialRange.startMs));
  const [end, setEnd] = useState(() => toLocalDateTimeInput(initialRange.endMs));
  const [error, setError] = useState("");
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const choosePreset = (durationMs: number) => {
    const presetEnd = Date.now();
    setStart(toLocalDateTimeInput(presetEnd - durationMs));
    setEnd(toLocalDateTimeInput(presetEnd));
    setError("");
  };
  const apply = () => {
    const startMs = new Date(start).getTime();
    const endMs = new Date(end).getTime();
    if (!Number.isFinite(startMs) || !Number.isFinite(endMs)) {
      setError("请选择完整的开始和结束时间");
      return;
    }
    if (startMs >= endMs) {
      setError("结束时间必须晚于开始时间");
      return;
    }
    if (endMs - startMs < 60_000) {
      setError("时间段至少需要 1 分钟");
      return;
    }
    if (endMs > Date.now() + 60_000) {
      setError("结束时间不能晚于当前时间");
      return;
    }
    if (startMs < Date.now() - 760 * 24 * 60 * 60 * 1000) {
      setError("本地统计最多保留约 760 天");
      return;
    }
    onApply({ startMs, endMs: Math.min(endMs, Date.now()) });
  };
  const inputStyle: CSSProperties = {
    width: "100%", boxSizing: "border-box", borderRadius: 8,
    border: `1px solid ${theme.segBorder}`, background: theme.segBg, color: theme.text,
    padding: "8px 9px", font: `500 11px ${theme.mono}`, outline: "none",
    colorScheme: theme.card === "#181818" ? "dark" : "light",
  };

  return <>
    <div data-no-drag="" onClick={onClose} style={{ position: "absolute", inset: 0, zIndex: 40, borderRadius: 12,
      background: "rgba(0,0,0,0.42)", backdropFilter: "blur(1px)" }} />
    <div data-no-drag="" role="dialog" aria-modal="true" aria-label="时间段统计"
      style={{ position: "absolute", zIndex: 41, left: 12, right: 12, top: "50%", transform: "translateY(-50%)",
        borderRadius: 13, padding: "13px", background: theme.card, border: `1px solid ${theme.segBorder}`,
        boxShadow: "0 18px 50px rgba(0,0,0,0.5)" }}>
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: 10 }}>
        <div>
          <div style={{ font: `700 13px ${theme.ui}`, color: theme.text }}>时间段统计</div>
          <div style={{ marginTop: 2, font: `500 8.5px ${theme.mono}`, color: theme.faint }}>12 个等长分段 · 涨跌对比前一等长时间段</div>
        </div>
        <button onClick={onClose} aria-label="关闭" style={{ width: 26, height: 26, border: 0, borderRadius: 7,
          background: theme.gridLine, color: theme.dim, cursor: "pointer", font: `600 15px ${theme.ui}` }}>×</button>
      </div>
      <div style={{ display: "grid", gridTemplateColumns: "repeat(5, 1fr)", gap: 5, marginBottom: 10 }}>
        {[
          ["1小时", 60 * 60 * 1000], ["6小时", 6 * 60 * 60 * 1000], ["24小时", 24 * 60 * 60 * 1000],
          ["7天", 7 * 24 * 60 * 60 * 1000], ["30天", 30 * 24 * 60 * 60 * 1000],
        ].map(([label, duration]) => <button key={label as string} onClick={() => choosePreset(duration as number)} style={{
          border: `1px solid ${theme.segBorder}`, borderRadius: 7, padding: "6px 2px", cursor: "pointer",
          background: theme.segBg, color: theme.dim, font: `600 9px ${theme.ui}`,
        }}>{label}</button>)}
      </div>
      <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8 }}>
        <label style={{ font: `600 9px ${theme.ui}`, color: theme.dim }}>开始
          <input type="datetime-local" value={start} max={end} onChange={(event) => { setStart(event.target.value); setError(""); }} style={{ ...inputStyle, marginTop: 5 }} />
        </label>
        <label style={{ font: `600 9px ${theme.ui}`, color: theme.dim }}>结束
          <input type="datetime-local" value={end} min={start} max={toLocalDateTimeInput(Date.now())} onChange={(event) => { setEnd(event.target.value); setError(""); }} style={{ ...inputStyle, marginTop: 5 }} />
        </label>
      </div>
      <div style={{ minHeight: 18, paddingTop: 5, font: `500 9px ${theme.ui}`, color: error ? "#e0795f" : theme.faint }}>
        {error || "统计来自本机 Codex 日志；费用为 API 等值估算"}
      </div>
      <div style={{ display: "flex", justifyContent: "flex-end", gap: 7, marginTop: 4 }}>
        <button onClick={onClose} style={{ border: `1px solid ${theme.segBorder}`, borderRadius: 8, padding: "7px 13px",
          background: theme.segBg, color: theme.dim, cursor: "pointer", font: `600 10.5px ${theme.ui}` }}>取消</button>
        <button onClick={apply} style={{ border: 0, borderRadius: 8, padding: "7px 15px",
          background: theme.accent, color: "#fff", cursor: "pointer", font: `700 10.5px ${theme.ui}` }}>查看统计</button>
      </div>
    </div>
  </>;
}

function AppMenuButton({ theme, onRefresh, onCustomRange, showToast, pinned, onPinnedChange }: {
  theme: Theme;
  onRefresh: () => Promise<void>;
  onCustomRange: () => void;
  showToast: (msg: string, ok: boolean) => void;
  pinned: boolean;
  onPinnedChange: (pinned: boolean) => void;
}) {
  const t = theme;
  const ref = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [autostart, setAutostart] = useState<boolean | null>(null);
  const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  useEffect(() => {
    if (!inTauri) {
      setAutostart(false);
      return;
    }
    invoke<boolean>("get_autostart_enabled")
      .then(setAutostart)
      .catch(() => setAutostart(false));
    invoke<boolean>("get_pinned")
      .then(onPinnedChange)
      .catch(() => onPinnedChange(false));
  }, [inTauri]);

  const itemStyle: CSSProperties = {
    width: "100%",
    display: "flex",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 10,
    padding: "8px 9px",
    border: 0,
    borderRadius: 7,
    background: "transparent",
    color: t.text,
    font: `600 11.5px ${t.ui}`,
    cursor: busy ? "default" : "pointer",
    textAlign: "left",
  };
  const checkStyle = (checked: boolean): CSSProperties => ({
    width: 14,
    height: 14,
    borderRadius: 4,
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    border: `1px solid ${checked ? t.accent : t.segBorder}`,
    background: checked ? t.accent : t.segBg,
    color: "#fff",
    font: `700 10px ${t.ui}`,
    flex: "0 0 auto",
  });

  const doRefresh = async () => {
    if (busy) return;
    setBusy(true);
    try {
      await onRefresh();
      setOpen(false);
      showToast("已刷新", true);
    } catch {
      showToast("刷新失败", false);
    } finally {
      setBusy(false);
    }
  };

  const toggleAutostart = async () => {
    if (busy || !inTauri) return;
    const next = !(autostart ?? false);
    setBusy(true);
    try {
      const actual = await invoke<boolean>("set_autostart_enabled", { enabled: next });
      setAutostart(actual);
      showToast(actual ? "已开启开机自启" : "已关闭开机自启", true);
    } catch {
      showToast("开机自启设置失败", false);
    } finally {
      setBusy(false);
    }
  };

  const togglePinned = async () => {
    if (busy || !inTauri) return;
    const next = !pinned;
    setBusy(true);
    try {
      const actual = await invoke<boolean>("set_pinned", { enabled: next });
      onPinnedChange(actual);
      showToast(actual ? "窗口已置顶" : "已取消置顶", true);
    } catch {
      showToast("置顶设置失败", false);
    } finally {
      setBusy(false);
    }
  };

  const quit = () => {
    if (!inTauri) return;
    invoke("quit_app").catch(() => showToast("退出失败", false));
  };

  return (
    <div ref={ref} style={{ position: "relative" }}>
      <button onClick={() => setOpen((v) => !v)} title="菜单" aria-label="菜单" aria-haspopup="menu" aria-expanded={open} style={{
        display: "inline-flex", alignItems: "center", justifyContent: "center",
        width: 26, height: 26, borderRadius: 7, cursor: "pointer", padding: 0,
        background: open ? t.segOnBg : t.segBg, border: `1px solid ${t.segBorder}`, color: t.dim,
      }}>
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke={t.dim} strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round">
          <circle cx="12" cy="5" r="1.4" />
          <circle cx="12" cy="12" r="1.4" />
          <circle cx="12" cy="19" r="1.4" />
        </svg>
      </button>
      {open && (
        <div role="menu" style={{
          position: "absolute", right: 0, top: 32, zIndex: 30,
          width: 184, padding: 5, borderRadius: 10,
          background: darkenMenuBg(t), border: `1px solid ${t.segBorder}`,
          boxShadow: "0 14px 36px rgba(0,0,0,0.28)",
        }}>
          <button role="menuitem" disabled={busy} onClick={doRefresh} style={itemStyle}>
            <span>刷新</span>
            {busy && <span style={{ color: t.faint, font: `600 10px ${t.mono}` }}>...</span>}
          </button>
          <button role="menuitem" disabled={busy} onClick={() => { setOpen(false); onCustomRange(); }} style={itemStyle}>
            <span>时间段统计</span>
            <span style={{ color: t.faint, font: `700 12px ${t.mono}` }}>↗</span>
          </button>
          <button role="menuitemcheckbox" aria-checked={pinned} disabled={busy || !inTauri} onClick={togglePinned} style={itemStyle}>
            <span>置顶</span>
            <span style={checkStyle(pinned)}>{pinned ? "✓" : ""}</span>
          </button>
          <button role="menuitemcheckbox" aria-checked={!!autostart} disabled={busy || !inTauri} onClick={toggleAutostart} style={itemStyle}>
            <span>开机自动启动</span>
            <span style={checkStyle(!!autostart)}>{autostart ? "✓" : ""}</span>
          </button>
          <div style={{ height: 1, background: t.gridLine, margin: "4px 3px" }} />
          <button role="menuitem" onClick={quit} style={{ ...itemStyle, color: "#e0795f" }}>
            <span>退出</span>
          </button>
        </div>
      )}
    </div>
  );
}

function darkenMenuBg(t: Theme) {
  return t.card === "#181818" ? "rgba(28,28,28,0.98)" : "rgba(255,255,255,0.98)";
}

function Panel({ dash, dark, themePref, onToggleTheme, onRefresh, openGen, active }: {
  dash: Dashboard;
  dark: boolean;
  themePref: "dark" | "light" | "system";
  onToggleTheme: () => void;
  onRefresh: () => Promise<void>;
  openGen: number;
  active: boolean;
}) {
  const t = TH[dark ? "dark" : "light"];
  // Drag the popover by its body (Windows/Linux only — macOS uses the menu-bar
  // NSPanel and is gated out). A real OS window-drag begins only once the
  // pointer moves past a small threshold, so a plain click still clicks through
  // / dismisses and never arms the hide-suppression guard.
  const canDrag = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window && !navigator.userAgent.includes("Macintosh");
  const dragRef = useRef<{ x: number; y: number } | null>(null);
  const [period, setPeriod] = useState<PeriodKind>("Week");
  const [periodOffset, setPeriodOffset] = useState(0);
  const [historicalReport, setHistoricalReport] = useState<{ key: string; report: PeriodReport } | null>(null);
  const [customRange, setCustomRange] = useState<CustomRange | null>(null);
  const [customReport, setCustomReport] = useState<{ key: string; report: PeriodReport } | null>(null);
  const [rangePickerOpen, setRangePickerOpen] = useState(false);
  const [detailKind, setDetailKind] = useState<TrendKind | null>(null);
  const [peakUnit, setPeakUnit] = useState<"tokens" | "cost">("tokens");
  const [pinned, setPinned] = useState(false);
  const currentReport: PeriodReport = period === "Hour" ? dash.hour : period === "Day" ? dash.day : period === "Month" ? dash.month : period === "Year" ? dash.year : dash.week;
  const historicalKey = `${period}:${periodOffset}`;
  useEffect(() => {
    if (periodOffset === 0) {
      setHistoricalReport(null);
      return;
    }
    let cancelled = false;
    const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
    const request = inTauri
      ? invoke<PeriodReport>("get_period_report", { period, offset: periodOffset })
      : Promise.resolve(currentReport);
    request.then((report) => {
      if (!cancelled) setHistoricalReport({ key: historicalKey, report });
    }).catch((error) => console.error("failed to load historical period", error));
    return () => { cancelled = true; };
  }, [period, periodOffset, dash.generatedAt]);
  const customKey = customRange ? `${customRange.startMs}:${customRange.endMs}` : "";
  useEffect(() => {
    if (!customRange) {
      setCustomReport(null);
      return;
    }
    let cancelled = false;
    const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
    const request = inTauri
      ? invoke<PeriodReport>("get_range_report", { startMs: customRange.startMs, endMs: customRange.endMs })
      : Promise.resolve(currentReport);
    request.then((report) => {
      if (!cancelled) setCustomReport({ key: customKey, report });
    }).catch((error) => console.error("failed to load custom time range", error));
    return () => { cancelled = true; };
  }, [customKey, dash.generatedAt]);
  const fixedReport: PeriodReport = periodOffset === 0
    ? currentReport
    : historicalReport?.key === historicalKey ? historicalReport.report : currentReport;
  const P: PeriodReport = customRange
    ? customReport?.key === customKey ? customReport.report : fixedReport
    : fixedReport;
  const M = P.metrics;
  // animated Total tokens: counts up from 0 on each open / period switch;
  // held at 0 while the popover is hidden so it never flashes the final value.
  const animTotal = useCountUp(M.totalTokens, `${customKey || `${period}:${periodOffset}`}:${openGen}`, active || pinned);
  const models = P.models;
  // Hide noise: 0% token-share rows, and $0 entries in the cost donut.
  // Show models whose share is at least 0.1% when rounded to 1 decimal; below
  // that it'd render a meaningless "0.0%" (a negligible token share). Such a
  // model can still appear under Cost if it has a non-zero cost.
  const tokenModels = models.filter(
    (m) => Math.round((m.tokens / (M.totalTokens || 1)) * 1000) / 10 >= 0.1
  );
  const costModels = models.filter((m) => m.cost > 0);
  // models that were used but have no pricing entry (cost unknown, not $0)
  const unpricedModels = models.filter((m) => !m.priced && m.tokens > 0);
  const maxM = Math.max(...tokenModels.map((m) => m.tokens), 1e-9);
  const tokenShares = tokenModels.map(
    (m) => Math.round((m.tokens / (M.totalTokens || 1)) * 1000) / 10
  );
  const trendSub = customRange ? customRangeLabel(customRange) : periodDisplayLabel(period, periodOffset, dash.generatedAt);
  const viewGeneratedAt = customRange
    ? new Date(customRange.endMs).toISOString()
    : periodViewEnd(period, periodOffset, dash.generatedAt).toISOString();
  const fixedHeatmapMeta = {
    Hour: { title: `${trendSub}热力图`, columns: 6, autoColumns: false, peakSub: "单个5分钟最高 Token" },
    Day: { title: `${trendSub}小时热力图`, columns: 12, autoColumns: false, peakSub: "单小时最高 Token" },
    Week: { title: `${trendSub}每日热力图`, columns: 7, autoColumns: false, peakSub: "单日最高 Token" },
    Month: { title: `${trendSub}每日热力图`, columns: 7, autoColumns: true, peakSub: "单日最高 Token" },
    Year: { title: `${trendSub}每月热力图`, columns: 6, autoColumns: false, peakSub: "单月最高 Token" },
  }[period];
  const heatmapMeta = customRange
    ? { title: `${trendSub}分段热力图`, columns: 6, autoColumns: false, peakSub: "单分段最高 Token" }
    : fixedHeatmapMeta;
  const annualHeatmap = !customRange && period === "Year";
  const stagePeak = P.series.reduce((peak, point, index) => {
    const tokens = point.input + point.cache + point.output;
    return tokens > peak.tokens
      ? { tokens, cost: P.costTrend[index] ?? 0 }
      : peak;
  }, { tokens: 0, cost: 0 });
  const primaryLimit = dash.rateLimits?.primary || null;
  const primaryRemaining = primaryLimit ? remainingPct(primaryLimit.usedPercent) : 0;
  const visibleBucketCount = customRange ? P.series.length : periodOffset === 0 ? activeBucketCount(period, dash.generatedAt) : P.series.length;
  const requestTrendValues = P.reqTrend.slice(0, visibleBucketCount);
  const costTrendValues = P.costTrend.slice(0, visibleBucketCount);
  const sessionTrendValues = (P.sessionTrend || []).slice(0, visibleBucketCount);
  const peakTrendValues = P.series.slice(0, visibleBucketCount).map((point) => point.input + point.cache + point.output);
  const quotaPeriod = customRange ? customRangePeriod(customRange) : period;
  const quotaTrendData = quotaPeriodTrend(dash.quotaTrend || [], quotaPeriod, viewGeneratedAt, visibleBucketCount, customRange?.startMs);
  const quotaEfficiencyData = quotaEfficiency(dash.quotaTrend || [], quotaPeriod, viewGeneratedAt, customRange?.startMs);
  const historicalQuotaAvailable = quotaTrendData.values.length > 0;
  const isCurrentFixedPeriod = !customRange && periodOffset === 0;
  const displayedRemaining = isCurrentFixedPeriod
    ? primaryRemaining
    : historicalQuotaAvailable ? quotaEfficiencyData.currentRemaining : null;
  const quotaSparkValues = historicalQuotaAvailable ? quotaTrendData.values : isCurrentFixedPeriod ? [primaryRemaining] : [0, 0];
  const quotaEfficiencyLabel = quotaEfficiencyData.averageCost != null
    ? `1%均≈$${fmtPeakCost(quotaEfficiencyData.averageCost)}`
    : quotaEfficiencyData.currentCost > 0
      ? `当前1%·$${fmtPeakCost(quotaEfficiencyData.currentCost)}`
      : "1%待累计";
  const [toast, setToast] = useState<{ msg: string; ok: boolean } | null>(null);
  const toastTimer = useRef<number | null>(null);
  const showToast = (msg: string, ok: boolean) => {
    if (toastTimer.current) window.clearTimeout(toastTimer.current);
    setToast({ msg, ok });
    toastTimer.current = window.setTimeout(() => setToast(null), 1800);
  };
  const moveCustomRange = (direction: -1 | 1) => {
    setDetailKind(null);
    setCustomRange((range) => {
      if (!range) return range;
      const duration = range.endMs - range.startMs;
      if (direction < 0) return { startMs: range.startMs - duration, endMs: range.endMs - duration };
      const nextEnd = Math.min(Date.now(), range.endMs + duration);
      return { startMs: nextEnd - duration, endMs: nextEnd };
    });
  };
  const customForwardDisabled = !!customRange && customRange.endMs >= Date.now() - 60_000;
  return (
    <div style={{
      width: "100%", height: "100vh", overflow: "hidden", boxSizing: "border-box",
      position: "relative",
      background: "transparent", padding: 0,
      fontFamily: t.ui,
    }}>
      <div className="om-scroll"
        onMouseDown={canDrag ? (e) => {
          // Record the press; the real drag only starts once the pointer moves
          // past the threshold (onMouseMove). Skip interactive controls
          // (data-no-drag) and non-left buttons so clicks still register.
          if (e.button !== 0) return;
          if ((e.target as HTMLElement).closest("[data-no-drag]")) return;
          dragRef.current = { x: e.clientX, y: e.clientY };
        } : undefined}
        onMouseMove={canDrag ? (e) => {
          const s = dragRef.current;
          if (!s) return;
          const dx = e.clientX - s.x, dy = e.clientY - s.y;
          if (dx * dx + dy * dy >= 16) { // ~4px → a drag, not a click
            dragRef.current = null;
            invoke("begin_drag").catch(() => {});
          }
        } : undefined}
        onMouseUp={canDrag ? () => { dragRef.current = null; } : undefined}
        style={{
        width: "100%", height: "100%", overflow: "hidden", display: "flex", flexDirection: "column",
        borderRadius: 12, background: dark ? "#181818" : "#ffffff",
        border: `1px solid ${dark ? "rgba(255,255,255,0.10)" : "rgba(0,0,0,0.08)"}`,
        padding: 0, color: t.text, cursor: canDrag ? "grab" : undefined,
      }}>
        {/* fixed header */}
        <div style={{
          position: "relative", zIndex: 10, flex: "0 0 auto",
          display: "flex", alignItems: "center", justifyContent: "space-between",
          padding: "15px 15px 12px",
          background: dark ? "#181818" : "#ffffff",
          borderBottom: `1px solid ${t.gridLine}`,
        }}>
          <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
            <TokenGlyph color={t.accent} size={16} />
            <span style={{ font: `600 13px ${t.ui}`, color: t.text, letterSpacing: ".01em" }}>CodexScope</span>
          </div>
          <div data-no-drag="" style={{ display: "flex", alignItems: "center", gap: 7, cursor: "default" }}>
            <div style={{ display: "inline-flex", alignItems: "center", gap: 2 }}>
              <button onClick={() => customRange ? moveCustomRange(-1) : (setDetailKind(null), setPeriodOffset((value) => Math.max(minimumPeriodOffset(period), value - 1)))}
                title={customRange ? "查看前一等长时间段" : `查看更早的${{ Hour: "小时", Day: "一天", Week: "一周", Month: "一月", Year: "一年" }[period]}`} aria-label="查看上一阶段"
                style={{ width: 18, height: 25, padding: 0, border: 0, borderRadius: 5, background: t.segBg, color: t.segOffText, cursor: "pointer", font: `700 15px ${t.ui}` }}>‹</button>
              <Segmented value={customRange ? "" : period} theme={t} onSelect={(v) => { setCustomRange(null); setPeriod(v as PeriodKind); setPeriodOffset(0); setDetailKind(null); }} />
              <button disabled={customRange ? customForwardDisabled : periodOffset === 0}
                onClick={() => customRange ? moveCustomRange(1) : (setDetailKind(null), setPeriodOffset((value) => Math.min(0, value + 1)))}
                title={customRange ? customForwardDisabled ? "已经到当前时间" : "查看后一等长时间段" : periodOffset === 0 ? "已经是当前阶段" : "查看下一阶段"} aria-label="查看下一阶段"
                style={{ width: 18, height: 25, padding: 0, border: 0, borderRadius: 5, background: t.segBg,
                  color: (customRange ? customForwardDisabled : periodOffset === 0) ? t.faint : t.segOffText,
                  opacity: (customRange ? customForwardDisabled : periodOffset === 0) ? 0.45 : 1,
                  cursor: (customRange ? customForwardDisabled : periodOffset === 0) ? "default" : "pointer", font: `700 15px ${t.ui}` }}>›</button>
            </div>
            <ThemeToggle pref={themePref} theme={t} onCycle={onToggleTheme} />
            <AppMenuButton theme={t} onRefresh={onRefresh} onCustomRange={() => { setDetailKind(null); setRangePickerOpen(true); }} showToast={showToast} pinned={pinned} onPinnedChange={setPinned} />
          </div>
        </div>
        {/* single-page body; the heatmap consumes the remaining height */}
        <div style={{ padding: "12px 15px 14px", flex: "1 1 auto", minHeight: 0, overflow: "hidden", display: "flex", flexDirection: "column" }}>
        {/* hero */}
        <div style={{ display: "flex", alignItems: "flex-end", justifyContent: "space-between", marginBottom: 10 }}>
          <div>
            <div style={{ font: `500 10px ${t.ui}`, color: t.dim, letterSpacing: ".04em" }}>Token 总量 · {trendSub}</div>
            <div style={{ display: "flex", alignItems: "baseline", gap: 8, marginTop: 3 }}>
              <span style={{ font: `600 30px ${t.mono}`, color: t.text, letterSpacing: "-.01em" }}>{animTotal >= 1000 ? (animTotal / 1000).toFixed(2) : animTotal.toFixed(2)}<span style={{ font: `500 15px ${t.mono}`, color: t.dim, marginLeft: 2 }}>{animTotal >= 1000 ? "B" : "M"}</span></span>
              {Math.round(M.deltaTokens) !== 0 && <Delta v={M.deltaTokens} theme={t} />}
            </div>
          </div>
          <div style={{ textAlign: "right" }}>
            <div style={{ font: `500 10px ${t.ui}`, color: t.dim }}>预估 API 等值费用</div>
            <div style={{ font: `600 18px ${t.mono}`, color: t.accent, marginTop: 2 }}>${M.cost.toFixed(2)}</div>
          </div>
        </div>
        {/* input / cached input / output split.
            When there's no usage the bar is just the empty track (no slivers). */}
        <div style={{ display: "flex", gap: 0, height: 7, borderRadius: 4, overflow: "hidden", marginBottom: 5, background: t.gridLine }}>
          {M.totalTokens > 0 && <>
            <div style={{ flexGrow: Math.max(M.inputTokens, 1e-6), flexBasis: 0, minWidth: 4, background: t.accent }} />
            {M.cacheTokens > 0 && <div style={{ flexGrow: Math.max(M.cacheTokens, 1e-6), flexBasis: 0, minWidth: 4, background: t.cacheCol }} />}
            <div style={{ flexGrow: Math.max(M.outputTokens, 1e-6), flexBasis: 0, minWidth: 4, background: t.accentSoft }} />
          </>}
        </div>
        <SplitLegend t={t} inputM={M.inputTokens} cacheM={M.cacheTokens} outputM={M.outputTokens} />
        {/* bar chart */}
        <BarChart data={P.series} theme={t} height={annualHeatmap ? 76 : 84} />
        <SectionRule t={t} m={annualHeatmap ? "12px 0 8px" : "14px 0 10px"} />
        {/* models */}
        <div style={{ marginBottom: 4 }}><Label t={t}>各模型 Token</Label></div>
        {tokenModels.length === 0 && <div style={{ font: `500 10.5px ${t.mono}`, color: t.faint, padding: "4px 0" }}>此周期暂无用量</div>}
        {tokenModels.map((m, i) => <ModelRow key={i} m={m} max={maxM} theme={t} share={tokenShares[i]} />)}
        <SectionRule t={t} m="10px 0 10px" />
        {/* estimated API value donut */}
        <div style={{ marginBottom: 8 }}><Label t={t}>各模型 API 等值费用</Label></div>
        {costModels.length > 0
          ? <CostDonut models={costModels} theme={t} size={annualHeatmap ? 92 : 100} thickness={annualHeatmap ? 14 : 15} />
          : <div style={{ font: `500 10.5px ${t.mono}`, color: t.faint }}>—</div>}
        {unpricedModels.length > 0 && (
          <div style={{ marginTop: 9, font: `500 9.5px/1.5 ${t.mono}`, color: t.faint }}>
            {unpricedModels.length} 个模型缺少价格数据（未计入费用）：{" "}
            <span style={{ color: t.dim }}>{unpricedModels.map((m) => m.name).join(", ")}</span>
          </div>
        )}
        <SectionRule t={t} m={annualHeatmap ? "10px 0 10px" : "12px 0 12px"} />
        {/* footer stats */}
        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8 }}>
          <MiniStat label="响应次数" value={fmtInt(M.requests)} sub={`${M.sessions} 个会话`} theme={t}
            onClick={() => setDetailKind("requests")}>
            <Sparkline values={requestTrendValues.length ? requestTrendValues : [0, 0]} theme={t} width={52} height={20} accent={t.accent} />
          </MiniStat>
          <MiniStat label="API 等值费用" value={`$${M.cost.toFixed(2)}`} sub={trendSub} theme={t} accent={t.accent}
            onClick={() => setDetailKind("cost")}>
            <Sparkline values={costTrendValues.length ? costTrendValues : [0, 0]} theme={t} width={52} height={20} accent={t.accent} />
          </MiniStat>
          {primaryLimit && (
            <MiniStat label={`剩余额度 · ${!isCurrentFixedPeriod && !historicalQuotaAvailable ? "无历史额度" : quotaEfficiencyLabel}`} value={displayedRemaining == null ? "—" : `${Math.round(displayedRemaining)}%`} sub={trendSub} theme={t} accent={(displayedRemaining ?? 100) <= 20 ? "#e0795f" : t.accent}
              onClick={() => setDetailKind("quota")}>
              <Sparkline values={quotaSparkValues} theme={t} width={52} height={20} accent={(displayedRemaining ?? 100) <= 20 ? "#e0795f" : t.accent} />
            </MiniStat>
          )}
          <MiniStat label="会话数" value={fmtInt(M.sessions)} sub={trendSub} theme={t} accent={t.accent}
            onClick={() => setDetailKind("sessions")}>
            <Sparkline values={sessionTrendValues.length ? sessionTrendValues : [0, 0]} theme={t} width={52} height={20} accent={t.accent} />
          </MiniStat>
          {dash.profile && <>
            <MiniStat label="累计 Token" value={fmtProfileTokens(dash.profile.cumulativeTokens)} sub={dash.profile.source === "account" ? "账户累计" : "本地日志"} theme={t} />
            <MiniStat label="阶段峰值" labelExtra={<span data-no-drag="" style={{ display: "inline-flex", padding: 1, borderRadius: 4, background: t.segBg, border: `1px solid ${t.segBorder}` }}>
                {(["tokens", "cost"] as const).map((unit) => <button key={unit} onClick={(event) => { event.stopPropagation(); setPeakUnit(unit); }}
                  aria-label={unit === "tokens" ? "显示 Token 峰值" : "显示费用峰值"}
                  style={{ border: 0, borderRadius: 3, padding: "1px 4px", cursor: "pointer", background: peakUnit === unit ? t.segOnBg : "transparent",
                    color: peakUnit === unit ? t.segOnText : t.segOffText, font: `600 8px ${t.mono}` }}>{unit === "tokens" ? "M" : "$"}</button>)}
              </span>} value={peakUnit === "tokens" ? fmtPeakTokens(stagePeak.tokens) : `$${fmtPeakCost(stagePeak.cost)}`}
              sub={heatmapMeta.peakSub} theme={t} accent={t.accent}
              onClick={() => setDetailKind("peak")}>
              <Sparkline values={(peakUnit === "tokens" ? peakTrendValues : costTrendValues).length ? (peakUnit === "tokens" ? peakTrendValues : costTrendValues) : [0, 0]} theme={t} width={52} height={20} accent={t.accent} />
            </MiniStat>
          </>}
        </div>
        {/* heatmap */}
        <SectionRule t={t} m="10px 0 9px" />
        <div style={{ flex: annualHeatmap ? "0 0 80px" : "1 1 0", minHeight: 0 }}>
          <Heatmap data={P.series} columns={heatmapMeta.columns} autoColumns={heatmapMeta.autoColumns} title={heatmapMeta.title} theme={t} accent={t.accent} showRange={!annualHeatmap} />
        </div>
        </div>{/* /single-page body */}
      </div>
      {detailKind && <TrendOverlay kind={detailKind} report={P} quotaTrendData={quotaTrendData} quotaEfficiencyData={quotaEfficiencyData} peakUnit={peakUnit} bucketCount={visibleBucketCount} periodLabel={trendSub} theme={t} onClose={() => setDetailKind(null)} />}
      {rangePickerOpen && <CustomRangeOverlay
        initialRange={customRange || { startMs: Date.now() - 24 * 60 * 60 * 1000, endMs: Date.now() }}
        theme={t}
        onClose={() => setRangePickerOpen(false)}
        onApply={(range) => { setCustomRange(range); setRangePickerOpen(false); setDetailKind(null); }} />}
      {toast && (
        <div className="om-toast" style={{
          position: "absolute", top: 58, left: "50%", transform: "translateX(-50%)",
          zIndex: 20, whiteSpace: "nowrap", pointerEvents: "none",
          font: `600 12px ${t.mono}`, color: "#fff",
          background: toast.ok ? t.accent : "#e0795f",
          padding: "7px 13px", borderRadius: 9,
          boxShadow: "0 8px 22px rgba(0,0,0,0.34)",
        }}>
          {toast.msg}
        </div>
      )}
    </div>
  );
}

function MainApp() {
  const [dash, setDash] = useState<Dashboard | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [openGen, setOpenGen] = useState(0);
  const [focused, setFocused] = useState(true); // browser preview: always "focused"
  // Theme preference: explicit Dark / Light, or System (follows the OS
  // appearance live on both macOS and Windows via prefers-color-scheme). First
  // run defaults to System.
  const [themePref, setThemePref] = useState<"dark" | "light" | "system">(() => {
    const saved = typeof localStorage !== "undefined" ? localStorage.getItem("codexscope-theme") : null;
    if (saved === "dark" || saved === "light" || saved === "system") return saved;
    return "system";
  });
  const [systemDark, setSystemDark] = useState<boolean>(
    () => typeof window !== "undefined" && !!window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches
  );
  // Follow the OS appearance live while in System mode (and keep it current for
  // an instant switch back to System).
  useEffect(() => {
    if (typeof window === "undefined" || !window.matchMedia) return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = (e: MediaQueryListEvent) => setSystemDark(e.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);
  const dark = themePref === "system" ? systemDark : themePref === "dark";
  // Cycle Dark → Light → System on each click; persist the choice.
  const cycleTheme = () =>
    setThemePref((p) => {
      const n = p === "dark" ? "light" : p === "light" ? "system" : "dark";
      try { localStorage.setItem("codexscope-theme", n); } catch {}
      return n;
    });

  useEffect(() => {
    // Apply fresh data AND clear any stale error: a transient initial-load
    // failure must not pin the error page for the whole session — the next
    // successful fetch (focus refetch or the 30s background push) recovers it.
    const apply = (d: Dashboard) => {
      setDash(d);
      setErr(null);
    };
    // initial load (shows the Loading state only until the first data arrives)
    fetchDashboard().then(apply).catch((e) => setErr(String(e)));

    const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
    if (!inTauri) return;
    // Under StrictMode the effect mounts → cleans up → remounts; the async
    // listen()/onFocusChanged() promises can resolve after the first cleanup,
    // so unregister any late arrival immediately instead of leaking a duplicate.
    let dead = false;
    const unlisten: Array<() => void> = [];
    const track = (u: () => void) => {
      if (dead) u();
      else unlisten.push(u);
    };
    // live updates pushed from the background refresh thread — swaps the data in
    // place (no Loading), so values update without any flicker.
    listen<Dashboard>("dashboard-updated", (e) => apply(e.payload)).then(track);
    // System appearance pushed natively from Rust (macOS). The webview's
    // prefers-color-scheme is unreliable for our hidden, non-activating menu-bar
    // panel, so the native event is the source of truth for System mode there;
    // it fires once at startup (correcting any stale launch value) and on every
    // OS theme change. Harmlessly never fires on Windows, where matchMedia works.
    listen<boolean>("system-theme", (e) => setSystemDark(e.payload)).then(track);
    // refetch the instant the popover gains focus (i.e. is opened)
    getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => {
        setFocused(focused);
        if (focused) {
          setOpenGen((g) => g + 1); // re-run the count-up on each open
          fetchDashboard().then(apply).catch(() => {});
        }
      })
      .then(track);
    return () => {
      dead = true;
      unlisten.forEach((u) => u());
    };
  }, []);

  // window is transparent; the rounded card paints its own background
  useEffect(() => {
    document.body.style.background = "transparent";
  }, [dark]);

  // Suppress per-property CSS transitions across a theme flip so the panel
  // repaints in the new theme in one step instead of cross-fading each color
  // (see .ts-no-transition in main.tsx). A background light→dark switch lands
  // while the panel is hidden; rAF callbacks don't run while hidden, so the
  // class stays on until the popover is shown — the first painted frame is
  // already the new theme with no transition, then we strip it a couple of
  // frames later so live interactions (e.g. switching the period) animate as
  // before. Skipped on the very first render (no prior frame to cross-fade).
  const firstThemeRun = useRef(true);
  useEffect(() => {
    if (firstThemeRun.current) {
      firstThemeRun.current = false;
      return;
    }
    const el = document.documentElement;
    el.classList.add("ts-no-transition");
    const id = requestAnimationFrame(() =>
      requestAnimationFrame(() => el.classList.remove("ts-no-transition"))
    );
    return () => cancelAnimationFrame(id);
  }, [dark]);

  const t = TH[dark ? "dark" : "light"];
  if (err) {
    return <div style={{ padding: 20, font: `500 12px ${t.mono}`, color: "#e0795f" }}>加载失败：{err}</div>;
  }
  if (!dash) {
    return (
      <div style={{ height: "100vh", padding: 10, boxSizing: "border-box", background: "transparent" }}>
        <div style={{ height: "100%", borderRadius: 14, background: dark ? "#181818" : "#ffffff",
          display: "flex", alignItems: "center", justifyContent: "center",
          font: `500 12px ${t.mono}`, color: t.dim }}>正在加载…</div>
      </div>
    );
  }
  const refreshDashboard = async () => {
    const inTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
    const d = inTauri ? await invoke<Dashboard>("refresh_dashboard") : await fetchDashboard();
    setDash(d);
    setErr(null);
  };

  return <Panel dash={dash} dark={dark} themePref={themePref} onToggleTheme={cycleTheme} onRefresh={refreshDashboard} openGen={openGen} active={focused} />;
}

type TrendPeriod = "Hour" | "Day" | "Week" | "Month";

function periodReport(dash: Dashboard, period: TrendPeriod): PeriodReport {
  return period === "Hour" ? dash.hour : period === "Day" ? dash.day : period === "Month" ? dash.month : dash.week;
}

function TrendWindow({ kind, period }: { kind: "requests" | "cost"; period: TrendPeriod }) {
  const [dash, setDash] = useState<Dashboard | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [dark, setDark] = useState(() => {
    const saved = localStorage.getItem("codexscope-theme");
    if (saved === "dark") return true;
    if (saved === "light") return false;
    return !!window.matchMedia?.("(prefers-color-scheme: dark)").matches;
  });

  useEffect(() => {
    fetchDashboard().then(setDash).catch((e) => setErr(String(e)));
    let dead = false;
    let unlisten: (() => void) | undefined;
    listen<Dashboard>("dashboard-updated", (e) => setDash(e.payload)).then((u) => dead ? u() : (unlisten = u));
    const mq = window.matchMedia?.("(prefers-color-scheme: dark)");
    const onTheme = (e: MediaQueryListEvent) => {
      if (localStorage.getItem("codexscope-theme") === "system") setDark(e.matches);
    };
    mq?.addEventListener("change", onTheme);
    return () => { dead = true; unlisten?.(); mq?.removeEventListener("change", onTheme); };
  }, []);

  const t = TH[dark ? "dark" : "light"];
  useEffect(() => {
    document.body.style.background = t.card;
    document.documentElement.style.background = t.card;
  }, [t.card]);

  if (err) return <div style={{ padding: 24, font: `500 13px ${t.mono}`, color: "#e0795f" }}>加载失败：{err}</div>;
  if (!dash) return <div style={{ height: "100vh", display: "grid", placeItems: "center", background: t.card, font: `500 13px ${t.mono}`, color: t.dim }}>正在加载详细趋势…</div>;

  const P = periodReport(dash, period);
  const values = kind === "requests" ? P.reqTrend : P.costTrend;
  const labels = P.series.map((s) => s.full);
  const total = values.reduce((s, v) => s + v, 0);
  const average = values.length ? total / values.length : 0;
  const peak = Math.max(...values, 0);
  const peakIndex = values.indexOf(peak);
  const activeBuckets = values.filter((v) => v > 0).length;
  const periodName = { Hour: "最近60分钟", Day: "今天", Week: "本周", Month: "本月" }[period];
  const title = kind === "requests" ? "响应次数详细趋势" : "API 等值费用详细趋势";
  const unitName = kind === "requests" ? "次" : "美元";
  const formatValue = kind === "requests"
    ? (v: number) => `${fmtInt(Math.round(v))} 次`
    : (v: number) => v > 0 && v < 0.01 ? `$${v.toFixed(4)}` : `$${v.toFixed(2)}`;
  const max = Math.max(...values, 1e-9);

  const summary = [
    { label: "总计", value: formatValue(total), sub: periodName },
    { label: "平均每区间", value: formatValue(average), sub: `${values.length} 个统计区间` },
    { label: "峰值", value: formatValue(peak), sub: labels[peakIndex] || "—" },
    { label: "活跃区间", value: `${activeBuckets} / ${values.length}`, sub: `单位：${unitName}` },
  ];

  return (
    <div style={{ minHeight: "100vh", boxSizing: "border-box", padding: "24px 28px 30px", background: t.card, color: t.text, fontFamily: t.ui }}>
      <div style={{ display: "flex", alignItems: "baseline", justifyContent: "space-between", gap: 16, marginBottom: 18 }}>
        <div>
          <div style={{ font: `700 22px ${t.display}`, color: t.text }}>{title}</div>
          <div style={{ marginTop: 5, font: `500 11px ${t.mono}`, color: t.dim }}>{periodName} · 数据每30秒自动更新</div>
        </div>
        <div style={{ font: `600 12px ${t.mono}`, color: t.accent }}>CodexScope</div>
      </div>
      <div style={{ display: "grid", gridTemplateColumns: "repeat(4, minmax(0, 1fr))", gap: 10, marginBottom: 14 }}>
        {summary.map((s) => <div key={s.label} style={{ padding: "12px 14px", borderRadius: 11, background: t.segBg, border: `1px solid ${t.segBorder}` }}>
          <div style={{ font: `500 10px ${t.ui}`, color: t.dim }}>{s.label}</div>
          <div style={{ marginTop: 5, font: `700 18px ${t.mono}`, color: t.text }}>{s.value}</div>
          <div style={{ marginTop: 4, font: `500 9px ${t.mono}`, color: t.faint, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{s.sub}</div>
        </div>)}
      </div>
      <TrendDetail title={`${title} · ${periodName}`} values={values} labels={labels} theme={t} accent={t.accent} formatValue={formatValue} />
      <div style={{ marginTop: 16, font: `600 13px ${t.ui}`, color: t.text }}>分时明细</div>
      <div style={{ marginTop: 8, borderRadius: 11, overflow: "hidden", border: `1px solid ${t.segBorder}` }}>
        {P.series.map((point, i) => <div key={i} style={{ display: "grid", gridTemplateColumns: "180px 1fr 100px", alignItems: "center", gap: 14, padding: "9px 12px", background: i % 2 ? t.segBg : "transparent", borderTop: i ? `1px solid ${t.gridLine}` : undefined }}>
          <span style={{ font: `500 10px ${t.mono}`, color: t.dim }}>{point.full}</span>
          <span style={{ height: 7, borderRadius: 4, background: t.gridLine, overflow: "hidden" }}><span style={{ display: "block", width: `${values[i] > 0 ? Math.max(1, values[i] / max * 100) : 0}%`, height: "100%", borderRadius: 4, background: t.accent }} /></span>
          <span style={{ font: `600 10px ${t.mono}`, color: values[i] > 0 ? t.text : t.faint, textAlign: "right" }}>{formatValue(values[i] || 0)}</span>
        </div>)}
      </div>
    </div>
  );
}

export default function App() {
  return <MainApp />;
}
