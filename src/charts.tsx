import { useId, useLayoutEffect, useRef, useState } from "react";
import {
  Theme, ModelStat, NamedCount, SeriesPoint,
  fmtInt, fmtMoney, fmtTokens,
} from "./data";

export function TokenGlyph({ color = "#339CFF", size = 14 }: { color?: string; size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 14 14">
      <rect x="0.6" y="0.6" width="12.8" height="12.8" rx="3.2" fill="none" stroke={color} strokeWidth="1.3" />
      <rect x="3" y="7.5" width="1.7" height="3.2" rx="0.6" fill={color} />
      <rect x="6.15" y="5" width="1.7" height="5.7" rx="0.6" fill={color} />
      <rect x="9.3" y="3" width="1.7" height="7.7" rx="0.6" fill={color} />
    </svg>
  );
}

export function Segmented({ value, items = ["Hour", "Day", "Week", "Month", "Year"], theme, onSelect }:
  { value: string; items?: string[]; theme: Theme; onSelect?: (v: string) => void }) {
  const t = theme;
  return (
    <div style={{ display: "inline-flex", padding: 2, borderRadius: 7, background: t.segBg, border: `1px solid ${t.segBorder}`, gap: 2 }}>
      {items.map((it) => {
        const on = it === value;
        return (
          <div key={it} onClick={() => onSelect && onSelect(it)} style={{
            font: `600 11px ${t.ui}`, letterSpacing: ".02em", padding: "3px 8px", borderRadius: 5, cursor: "pointer", userSelect: "none",
            color: on ? t.segOnText : t.segOffText, background: on ? t.segOnBg : "transparent",
            boxShadow: on ? t.segOnShadow : "none", transition: "color .15s, background .15s",
          }}>{{ Hour: "时", Day: "日", Week: "周", Month: "月", Year: "年" }[it] || it}</div>
        );
      })}
    </div>
  );
}

export function BarChart({ data, theme, height = 96, accent, accentSoft, radius = 3 }:
  { data: SeriesPoint[]; theme: Theme; height?: number; accent?: string; accentSoft?: string; radius?: number }) {
  const t = theme;
  accent = accent || t.accent; accentSoft = accentSoft || t.accentSoft;
  const max = Math.max(...data.map((d) => d.input + d.cache + d.output), 1e-9);
  const n = data.length;
  const gapPct = Math.max(0.8, Math.min(6, 32 / n));
  const effRadius = n > 16 ? 1 : radius;
  const [hi, setHi] = useState<SeriesPoint | null>(null);
  const [tip, setTip] = useState({ x: 0, y: 0 });
  // position:fixed so the tooltip renders above the scrolling card (not clipped).
  // Anchor to the *visible bar top* (baseline − bar height), not the full-height
  // column top, so short bars don't push the tooltip up over the legend above.
  const onBar = (d: SeriesPoint, e: React.MouseEvent) => {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const barPx = ((d.input + d.cache + d.output) / max) * height;
    setHi(d); setTip({ x: r.left + r.width / 2, y: r.bottom - barPx });
  };
  return (
    <div>
      <div style={{ position: "relative", height, display: "flex", alignItems: "flex-end", gap: `${gapPct}%` }}>
        {[0.25, 0.5, 0.75, 1].map((g, i) => (
          <div key={i} style={{ position: "absolute", left: 0, right: 0, bottom: `${g * 100}%`, borderTop: `1px solid ${t.gridLine}` }} />
        ))}
        {data.map((d, i) => {
          // Stacked top-to-bottom: output, cached input, uncached input.
          const hO = (d.output / max) * height;
          const hC = (d.cache / max) * height;
          const hI = (d.input / max) * height;
          const empty = d.input + d.cache + d.output <= 0;
          const on = hi === d;
          return (
            <div key={i}
              onMouseEnter={empty ? undefined : (e) => onBar(d, e)}
              onMouseLeave={empty ? undefined : () => setHi(null)}
              style={{ flex: 1, alignSelf: "stretch", display: "flex", flexDirection: "column", justifyContent: "flex-end", position: "relative", zIndex: 1, cursor: "default", opacity: hi && !on && !empty ? 0.55 : 1, transition: "opacity .12s" }}>
              <div style={{ height: hO, background: accentSoft, borderRadius: `${effRadius}px ${effRadius}px 0 0` }} />
              <div style={{ height: hC, background: t.cacheCol }} />
              <div style={{ height: hI, background: accent }} />
            </div>
          );
        })}
      </div>
      <div style={{ display: "flex", gap: `${gapPct}%`, marginTop: 6 }}>
        {data.map((d, i) => (
          <div key={i} style={{ flex: 1, textAlign: "center", font: `500 9px ${t.mono}`, color: t.dim, letterSpacing: ".03em" }}>{d.label}</div>
        ))}
      </div>
      {hi && (
        <div style={{
          position: "fixed",
          left: Math.min(Math.max(tip.x, 96), (typeof window !== "undefined" ? window.innerWidth : 372) - 96),
          top: tip.y - 8, transform: "translate(-50%,-100%)",
          background: t.tip, color: "#fff", borderRadius: 6, padding: "5px 8px",
          font: `500 10px ${t.mono}`, whiteSpace: "nowrap", pointerEvents: "none", zIndex: 9999,
          boxShadow: "0 4px 14px rgba(0,0,0,0.35)" }}>
          <span style={{ color: accent, fontWeight: 600 }}>
            {(() => { const tot = hi.input + hi.cache + hi.output; return tot === 0 ? "无 Token" : fmtTokens(tot) + " Token"; })()}
          </span>
          <span style={{ opacity: 0.7 }}> · {hi.full}</span>
        </div>
      )}
    </div>
  );
}

type NullableTrendValue = number | null;

function sparseLineGeometry(values: NullableTrendValue[], w: number, h: number, pad: number) {
  const finite = values.filter((value): value is number => value != null && Number.isFinite(value));
  const max = finite.length ? Math.max(...finite) : 1;
  const min = finite.length ? Math.min(...finite) : 0;
  const range = max - min || 1;
  const px = (i: number) => values.length <= 1 ? w / 2 : pad + (i / (values.length - 1)) * (w - pad * 2);
  const py = (value: number) => pad + (1 - (value - min) / range) * (h - pad * 2);
  const pts = values.map((value, index) => value == null || !Number.isFinite(value) ? null : [px(index), py(value)] as [number, number]);
  const groups: Array<Array<[number, number]>> = [];
  let current: Array<[number, number]> | null = null;
  for (const point of pts) {
    if (point) {
      if (!current) {
        current = [];
        groups.push(current);
      }
      current.push(point);
    } else {
      current = null;
    }
  }
  const pathFor = (group: Array<[number, number]>) => {
    if (!group.length) return "";
    let d = `M ${group[0][0].toFixed(1)} ${group[0][1].toFixed(1)}`;
    for (let i = 0; i < group.length - 1; i++) {
      const p1 = group[i], p2 = group[i + 1];
      // Endpoint-constrained easing: both control-point Y values stay on the
      // adjacent endpoints, so the curve remains inside their value range and
      // cannot bulge above/below a falling or rising pair.
      const handle = (p2[0] - p1[0]) * 0.38;
      const c1x = p1[0] + handle, c1y = p1[1];
      const c2x = p2[0] - handle, c2y = p2[1];
      d += ` C ${c1x.toFixed(1)} ${c1y.toFixed(1)}, ${c2x.toFixed(1)} ${c2y.toFixed(1)}, ${p2[0].toFixed(1)} ${p2[1].toFixed(1)}`;
    }
    return d;
  };
  return { pts, groups, paths: groups.map(pathFor), px };
}

export function Sparkline({ values, theme, width = 80, height = 24, accent, strokeW = 1.6 }:
  { values: NullableTrendValue[]; theme: Theme; width?: number; height?: number; accent?: string; strokeW?: number }) {
  const t = theme; accent = accent || t.accent;
  if (values.length < 2) values = values.length ? [values[0], values[0]] : [0, 0];
  const gid = useId().replace(/:/g, "");
  const { pts, groups, paths } = sparseLineGeometry(values, width, height, strokeW + 1.8);
  return (
    <svg width={width} height={height} viewBox={`0 0 ${width} ${height}`} style={{ display: "block", overflow: "visible" }}>
      <defs>
        <linearGradient id={`sl${gid}`} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={accent} stopOpacity="0.32" />
          <stop offset="100%" stopColor={accent} stopOpacity="0" />
        </linearGradient>
      </defs>
      {groups.map((group, i) => group.length > 1 && <path key={`a${i}`} d={`${paths[i]} L ${group[group.length - 1][0].toFixed(1)} ${height} L ${group[0][0].toFixed(1)} ${height} Z`} fill={`url(#sl${gid})`} stroke="none" />)}
      {paths.map((d, i) => <path key={`p${i}`} d={d} fill="none" stroke={accent} strokeWidth={strokeW} strokeLinecap="round" strokeLinejoin="round" />)}
      {pts.map((point, i) => point && (
        <circle key={i} cx={point[0]} cy={point[1]} r={values.length > 12 ? 1.35 : 1.7} fill={accent} stroke={t.card} strokeWidth="0.7" />
      ))}
    </svg>
  );
}

export function TrendDetail({ title, values, labels, theme, accent, formatValue }:
  { title: string; values: NullableTrendValue[]; labels: string[]; theme: Theme; accent?: string; formatValue: (v: number, index?: number) => string }) {
  const t = theme; accent = accent || t.accent;
  const gid = useId().replace(/:/g, "");
  const [hi, setHi] = useState<number | null>(null);
  const safe = values.length >= 2 ? values : values.length === 1 ? [values[0], values[0]] : [null, null];
  const chartW = 800, chartH = 280;
  const { pts, groups, paths, px } = sparseLineGeometry(safe, chartW, chartH, 6);
  const lastValueIndex = safe.reduce<number>((last, value, index) => value == null ? last : index, 0);
  const active = hi === null ? lastValueIndex : hi;
  const sourceIndex = Math.min(active, Math.max(values.length - 1, 0));
  const activeLabel = labels[sourceIndex] || "";
  const activeValue = values[sourceIndex] ?? null;
  const hitW = chartW / safe.length;
  return (
    <div style={{ borderRadius: 12, padding: "17px 18px 13px", background: t.segBg, border: `1px solid ${t.segBorder}` }}>
      <div style={{ height: 35, marginBottom: 12, overflow: "hidden" }}>
        <div style={{ font: `600 14px/1 ${t.ui}`, color: t.text, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{title}</div>
        <div style={{ marginTop: 7, font: `500 11px/1 ${t.mono}`, color: t.dim, textAlign: "right",
          whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
          {activeLabel} · <span style={{ color: accent, fontWeight: 600 }}>{activeValue == null ? "无历史数据" : formatValue(activeValue, sourceIndex)}</span>
        </div>
      </div>
      <svg width="100%" viewBox={`0 0 ${chartW} ${chartH}`} style={{ display: "block", overflow: "visible" }} onMouseLeave={() => setHi(null)}>
        <defs>
          <linearGradient id={`td${gid}`} x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stopColor={accent} stopOpacity="0.32" />
            <stop offset="100%" stopColor={accent} stopOpacity="0" />
          </linearGradient>
        </defs>
        {[0.25, 0.5, 0.75].map((g) => <line key={g} x1="0" x2={chartW} y1={chartH * g} y2={chartH * g} stroke={t.gridLine} strokeWidth="1" />)}
        {groups.map((group, i) => group.length > 1 && <path key={`a${i}`} d={`${paths[i]} L ${group[group.length - 1][0].toFixed(1)} ${chartH} L ${group[0][0].toFixed(1)} ${chartH} Z`} fill={`url(#td${gid})`} stroke="none" />)}
        {paths.map((d, i) => <path key={`p${i}`} d={d} fill="none" stroke={accent} strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" />)}
        {pts.map((point, i) => point ? (
          <circle key={i} cx={point[0]} cy={point[1]} r="4.5" fill={t.card} stroke={accent} strokeWidth="2.2" />
        ) : (
          <circle key={i} cx={px(i)} cy={chartH - 3} r="3" fill={t.card} stroke={t.faint} strokeWidth="1.5" />
        ))}
        {hi !== null && pts[hi] && <>
          <line x1={pts[hi]![0]} x2={pts[hi]![0]} y1="0" y2={chartH} stroke={accent} strokeOpacity="0.3" strokeDasharray="3 3" />
          <circle cx={pts[hi]![0]} cy={pts[hi]![1]} r="7" fill={t.card} stroke={accent} strokeWidth="3" />
        </>}
        {safe.map((_, i) => <rect key={i} x={Math.max(0, px(i) - hitW / 2)} y="0" width={hitW} height={chartH} fill="transparent" onMouseEnter={() => setHi(i)} />)}
      </svg>
      <div style={{ display: "flex", justifyContent: "space-between", marginTop: 6, font: `500 10px ${t.mono}`, color: t.faint }}>
        <span>{labels[0] || ""}</span><span>{labels[labels.length - 1] || ""}</span>
      </div>
    </div>
  );
}

// Cost-rank palette: darkest/most-prominent Codex blue for the biggest cost share,
// fading down. Colors map to the *cost* ordering here (not the backend's
// token-rank), so the largest wedge always gets the leading color.
const DONUT_PALETTE = ["#116DC4", "#238BEF", "#339CFF", "#8DCAFF", "#C5E4FF"];
const DONUT_OVERFLOW = "#6E7F90";

export function CostDonut({ models, theme, size = 104, thickness = 16 }:
  { models: ModelStat[]; theme: Theme; size?: number; thickness?: number }) {
  const t = theme;
  const [hi, setHi] = useState(-1);
  // Rank by cost (desc) and recolor by that rank — usage from most to least.
  const ranked = [...models]
    .sort((a, b) => b.cost - a.cost)
    .map((m, i) => ({ ...m, color: i < DONUT_PALETTE.length ? DONUT_PALETTE[i] : DONUT_OVERFLOW }));
  models = ranked;
  const total = models.reduce((s, m) => s + m.cost, 0) || 1e-9;
  const cx = size / 2, cy = size / 2;
  const rOut = (size - 2) / 2, rIn = rOut - thickness;
  // No inter-segment gap: wedges butt together into one continuous solid ring.
  // (Segments stay distinguishable by colour and the hover dim.)
  const gap = 0;
  let a = -Math.PI / 2;
  const arc = (a0: number, a1: number, rO: number, rI: number) => {
    const large = a1 - a0 > Math.PI ? 1 : 0;
    const x0 = cx + rO * Math.cos(a0), y0 = cy + rO * Math.sin(a0);
    const x1 = cx + rO * Math.cos(a1), y1 = cy + rO * Math.sin(a1);
    const x2 = cx + rI * Math.cos(a1), y2 = cy + rI * Math.sin(a1);
    const x3 = cx + rI * Math.cos(a0), y3 = cy + rI * Math.sin(a0);
    return `M ${x0.toFixed(2)} ${y0.toFixed(2)} A ${rO} ${rO} 0 ${large} 1 ${x1.toFixed(2)} ${y1.toFixed(2)} L ${x2.toFixed(2)} ${y2.toFixed(2)} A ${rI} ${rI} 0 ${large} 0 ${x3.toFixed(2)} ${y3.toFixed(2)} Z`;
  };
  const wedges = models.map((m, i) => {
    const frac = m.cost / total;
    const a0 = a + gap / 2, a1 = a + frac * 2 * Math.PI - gap / 2;
    a += frac * 2 * Math.PI;
    return { m, i, d: arc(a0, a1, hi === i ? rOut + 2 : rOut, rIn) };
  });
  const cur = hi >= 0 ? models[hi] : null;
  const amount = cur ? cur.cost : total;
  const txt = fmtMoney(amount);
  const avail = (size - 2 - thickness * 2) * 0.98;
  const base = cur ? 15 : 17;
  const fit = Math.min(base, Math.max(10, avail / (txt.length * 0.62)));
  return (
    <div style={{ display: "flex", alignItems: "center", gap: 14 }}>
      <div style={{ position: "relative", width: size, height: size, flex: "0 0 auto" }}>
        <svg width={size} height={size} viewBox={`0 0 ${size} ${size}`} style={{ overflow: "visible" }}>
          {models.length === 1 ? (
            // Single model: a full closed ring (one 360° arc would be
            // degenerate). Trace both edges with a 1px card-coloured stroke for
            // a crisp inset look.
            <g onMouseEnter={() => setHi(0)} onMouseLeave={() => setHi(-1)} style={{ cursor: "default" }}>
              <circle cx={cx} cy={cy} r={(rOut + rIn) / 2}
                fill="none" stroke={models[0].color} strokeWidth={thickness} />
              <circle cx={cx} cy={cy} r={rOut} fill="none" stroke={t.card} strokeWidth={1} />
              <circle cx={cx} cy={cy} r={rIn} fill="none" stroke={t.card} strokeWidth={1} />
            </g>
          ) : (
            wedges.map((w) => (
              <path key={w.i} d={w.d} fill={w.m.color}
                opacity={hi === -1 || hi === w.i ? 1 : 0.32}
                onMouseEnter={() => setHi(w.i)} onMouseLeave={() => setHi(-1)}
                style={{ transition: "opacity .14s", cursor: "default" }} />
            ))
          )}
        </svg>
        <div style={{ position: "absolute", inset: 0, display: "flex", alignItems: "center", justifyContent: "center", pointerEvents: "none" }}>
          <span style={{ font: `600 ${fit.toFixed(1)}px/1 ${t.mono}`, color: cur ? cur.color : t.text, letterSpacing: "-.01em" }}>{txt}</span>
        </div>
      </div>
      <div style={{ flex: 1, minWidth: 0 }}>
        {models.map((m, i) => (
          <div key={i} onMouseEnter={() => setHi(i)} onMouseLeave={() => setHi(-1)}
            style={{ display: "flex", alignItems: "center", gap: 7, padding: "2.5px 0", opacity: hi === -1 || hi === i ? 1 : 0.45, transition: "opacity .14s", cursor: "default", userSelect: "none" }}>
            <span style={{ width: 7, height: 7, borderRadius: 2, background: m.color, flex: "0 0 auto" }} />
            <span style={{ font: `500 10.5px ${t.ui}`, color: t.text, whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis", flex: 1, fontWeight: hi === i ? 600 : 500 }}>{m.name}</span>
            <span style={{ font: `600 10.5px ${t.mono}`, color: hi === i ? m.color : t.dim, flex: "0 0 auto" }}>{fmtMoney(m.cost)}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

export function BarList({ items, theme, accent, limit = 5 }:
  { items: NamedCount[]; theme: Theme; accent?: string; limit?: number }) {
  const t = theme; accent = accent || t.accent;
  const [open, setOpen] = useState(false);
  const shown = items.slice(0, open ? items.length : limit);
  // Bar length = this item's count relative to the *most-called* item
  // (count / max), so top1 fills the track and the rest scale down — same logic
  // as ModelRow's token bars, and gives a descending comparison ladder even when
  // usage is spread across many tools (count / total leaves every bar tiny).
  const max = items.reduce((m, i) => Math.max(m, i.count), 0) || 1;
  const more = items.length - shown.length;
  return (
    <div>
      {shown.map((it, i) => (
        // name flush-left (width 134 keeps the bar start aligned with ModelRow's
        // bar at x=143); the bar then runs all the way to a far-right count, whose
        // right edge lines up with the model rows' trailing value.
        <div key={i} style={{ display: "flex", alignItems: "center", gap: 9, padding: "3px 0" }}>
          <span style={{ font: `500 10.5px ${t.mono}`, color: t.text, flex: "0 0 134px", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>{it.name}</span>
          <div style={{ flex: 1, height: 5, borderRadius: 3, background: t.gridLine, overflow: "hidden" }}>
            <div style={{ width: `${(it.count / max) * 100}%`, height: "100%", background: accent, borderRadius: 3 }} />
          </div>
          <span style={{ font: `600 10.5px ${t.mono}`, color: t.dim, flex: "0 0 auto", minWidth: 30, textAlign: "right" }}>{fmtInt(it.count)}</span>
        </div>
      ))}
      {more > 0 && (
        <div onClick={() => setOpen(true)} style={{ font: `500 9.5px ${t.ui}`, color: t.faint, paddingTop: 4, cursor: "pointer", userSelect: "none" }}
          onMouseEnter={(e) => (e.currentTarget.style.color = t.dim)} onMouseLeave={(e) => (e.currentTarget.style.color = t.faint)}>
          还有 {more} 项
        </div>
      )}
      {open && items.length > limit && (
        <div onClick={() => setOpen(false)} style={{ font: `500 9.5px ${t.ui}`, color: t.faint, paddingTop: 4, cursor: "pointer", userSelect: "none" }}
          onMouseEnter={(e) => (e.currentTarget.style.color = t.dim)} onMouseLeave={(e) => (e.currentTarget.style.color = t.faint)}>
          收起
        </div>
      )}
    </div>
  );
}

function ramp(accent: string, lvl: number, gridLine: string, card: string) {
  if (lvl === 0) return gridLine;
  const op = [0, 0.28, 0.5, 0.74, 1][lvl];
  return `color-mix(in srgb, ${accent} ${Math.round(op * 100)}%, ${card})`;
}

export function Heatmap({ data, columns, autoColumns = false, title, theme, accent, gap = 3 }:
  { data: SeriesPoint[]; columns: number; autoColumns?: boolean; title: string; theme: Theme; accent?: string; gap?: number }) {
  const t = theme; accent = accent || t.accent;
  const [hi, setHi] = useState<SeriesPoint | null>(null);
  const [tip, setTip] = useState({ x: 0, y: 0 });
  const gridAreaRef = useRef<HTMLDivElement>(null);
  const [cellSize, setCellSize] = useState(1);
  const [layoutColumns, setLayoutColumns] = useState(columns);
  const layoutRows = Math.max(1, Math.ceil(data.length / layoutColumns));
  useLayoutEffect(() => {
    const area = gridAreaRef.current;
    if (!area) return;
    const measure = () => {
      let bestColumns = columns;
      let bestRows = Math.max(1, Math.ceil(data.length / bestColumns));
      let bestSize = Math.min(
        (area.clientWidth - gap * (bestColumns - 1)) / bestColumns,
        (area.clientHeight - gap * (bestRows - 1)) / bestRows,
      );
      if (autoColumns) {
        for (let candidate = 1; candidate <= data.length; candidate += 1) {
          const candidateRows = Math.ceil(data.length / candidate);
          const candidateSize = Math.min(
            (area.clientWidth - gap * (candidate - 1)) / candidate,
            (area.clientHeight - gap * (candidateRows - 1)) / candidateRows,
          );
          if (candidateSize > bestSize) {
            bestColumns = candidate;
            bestRows = candidateRows;
            bestSize = candidateSize;
          }
        }
      }
      setLayoutColumns(bestColumns);
      setCellSize(Math.max(1, Math.floor(bestSize)));
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(area);
    return () => observer.disconnect();
  }, [columns, autoColumns, data.length, gap]);
  const totals = data.map((d) => d.input + d.cache + d.output);
  const max = Math.max(...totals, 0);
  const level = (tokens: number) => tokens <= 0 || max <= 0 ? 0 : Math.max(1, Math.ceil(tokens / max * 4));
  const onCell = (d: SeriesPoint, e: React.MouseEvent) => {
    // viewport coords → tooltip uses position:fixed so it isn't clipped by the
    // panel overflow (renders on top of the panel).
    const r = (e.target as HTMLElement).getBoundingClientRect();
    setHi(d); setTip({ x: r.left + r.width / 2, y: r.top });
  };
  return (
    <div style={{ position: "relative", height: "100%", minHeight: 0, display: "flex", flexDirection: "column" }}>
      <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 10, marginBottom: 7 }}>
        <span style={{ font: `600 10px ${t.ui}`, color: t.dim, letterSpacing: ".05em", textTransform: "uppercase", whiteSpace: "nowrap" }}>{title}</span>
        <div style={{ display: "flex", alignItems: "center", gap: 5, font: `500 8.5px ${t.mono}`, color: t.faint, whiteSpace: "nowrap" }}>
          <span>少</span>
          {[0, 1, 2, 3, 4].map((l) => (<span key={l} style={{ width: 9, height: 9, borderRadius: 2, background: ramp(accent!, l, t.gridLine, t.card) }} />))}
          <span>多</span>
        </div>
      </div>
      <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 5, font: `500 8.5px ${t.mono}`, color: t.faint }}>
        <span>{data[0]?.full || ""}</span>
        <span>{data[data.length - 1]?.full || ""}</span>
      </div>
      <div ref={gridAreaRef} style={{ flex: 1, minHeight: 0, width: "100%", overflow: "hidden", display: "flex", alignItems: "center", justifyContent: "center" }}>
        <div style={{ display: "grid", gridTemplateColumns: `repeat(${layoutColumns}, ${cellSize}px)`, gridTemplateRows: `repeat(${layoutRows}, ${cellSize}px)`, gap }}>
          {data.map((d, i) => {
            const tokens = totals[i];
            return <div key={i}
              onMouseEnter={(e) => onCell(d, e)}
              onMouseLeave={() => setHi(null)}
              style={{ width: cellSize, height: cellSize, borderRadius: 3,
                background: ramp(accent!, level(tokens), t.gridLine, t.card), cursor: "default" }} />;
          })}
        </div>
      </div>
      {hi && (
        <div style={{
          position: "fixed",
          left: Math.min(Math.max(tip.x, 96), (typeof window !== "undefined" ? window.innerWidth : 372) - 96),
          top: tip.y - 8, transform: "translate(-50%,-100%)",
          background: t.tip, color: "#fff", borderRadius: 6, padding: "5px 8px",
          font: `500 10px ${t.mono}`, whiteSpace: "nowrap", pointerEvents: "none", zIndex: 9999,
          boxShadow: "0 4px 14px rgba(0,0,0,0.35)" }}>
          <span style={{ color: accent, fontWeight: 600 }}>{hi.input + hi.cache + hi.output === 0 ? "无 Token" : fmtTokens(hi.input + hi.cache + hi.output) + " Token"}</span>
          <span style={{ opacity: 0.7 }}> · {hi.full}</span>
        </div>
      )}
    </div>
  );
}
