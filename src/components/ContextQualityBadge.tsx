import { useId, useState } from "react";

interface ContextQualityBadgeProps {
  score: number;
  details?: {
    relevance: number;
    density: number;
    freshness: number;
    diversity: number;
  };
}

function clampScore(value: number): number {
  return Math.max(0, Math.min(1, value));
}

function scoreColor(score: number): string {
  if (score > 0.8) return "var(--color-success, #16a34a)";
  if (score >= 0.5) return "var(--color-warning, #f59e0b)";
  return "var(--color-danger, #dc2626)";
}

function formatPercent(value: number): string {
  return `${Math.round(clampScore(value) * 100)}%`;
}

export default function ContextQualityBadge({ score, details }: ContextQualityBadgeProps) {
  const [open, setOpen] = useState(false);
  const tooltipId = useId();
  const safeScore = clampScore(score);
  const color = scoreColor(safeScore);

  return (
    <div style={{ position: "relative", display: "inline-flex", alignItems: "center" }}>
      <button
        type="button"
        aria-describedby={details ? tooltipId : undefined}
        aria-label={`Context quality ${formatPercent(safeScore)}`}
        onMouseEnter={() => setOpen(true)}
        onMouseLeave={() => setOpen(false)}
        onFocus={() => setOpen(true)}
        onBlur={() => setOpen(false)}
        style={{
          display: "inline-flex",
          alignItems: "center",
          gap: 8,
          padding: "6px 10px",
          borderRadius: 999,
          border: `1px solid ${color}`,
          background: `${color}18`,
          color,
          fontSize: 12,
          fontWeight: 700,
          cursor: details ? "help" : "default",
        }}
      >
        <span
          aria-hidden="true"
          style={{
            width: 8,
            height: 8,
            borderRadius: "50%",
            background: color,
          }}
        />
        <span>Context</span>
        <strong>{formatPercent(safeScore)}</strong>
      </button>

      {details && open ? (
        <div
          id={tooltipId}
          role="tooltip"
          style={{
            position: "absolute",
            top: "calc(100% + 10px)",
            right: 0,
            minWidth: 210,
            padding: 12,
            borderRadius: 12,
            border: "1px solid rgba(148, 163, 184, 0.25)",
            background: "rgba(15, 23, 42, 0.97)",
            color: "#e2e8f0",
            boxShadow: "0 12px 30px rgba(15, 23, 42, 0.35)",
            zIndex: 10,
          }}
        >
          <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 8, fontWeight: 700 }}>
            <span>Quality breakdown</span>
            <span style={{ color }}>{formatPercent(safeScore)}</span>
          </div>

          {Object.entries(details).map(([label, value]) => (
            <div key={label} style={{ display: "grid", gridTemplateColumns: "1fr auto", gap: 8, fontSize: 12, marginTop: 6 }}>
              <span style={{ textTransform: "capitalize", color: "rgba(226, 232, 240, 0.75)" }}>{label}</span>
              <strong>{formatPercent(value)}</strong>
            </div>
          ))}
        </div>
      ) : null}
    </div>
  );
}
