import { useState } from "react";
import type { TokenBudget } from "../types";

// ── Types ──────────────────────────────────────────────────────────────────────

export type ContextZone = "Green" | "Yellow" | "Orange" | "Red" | "Critical";

export interface ContextBreakdown {
  system_prompt_tokens: number;
  tool_schemas_tokens: number;
  project_context_tokens: number;
  conversation_tokens: number;
  tool_results_tokens: number;
  pinned_context_tokens: number;
  total_tokens: number;
  context_limit: number;
  zone: ContextZone;
}

export interface PinnedContextItem {
  label: string;
  content: string;
  tokens: number;
  pinned_at: string;
}

// ── Helpers ────────────────────────────────────────────────────────────────────

function zoneFromBudget(budget: TokenBudget): ContextZone {
  const total = budget.used_input + budget.used_output + budget.reserved_output;
  const pct = budget.context_limit > 0 ? (total / budget.context_limit) * 100 : 0;
  if (pct >= 95) return "Critical";
  if (pct >= 85) return "Red";
  if (pct >= 70) return "Orange";
  if (pct >= 50) return "Yellow";
  return "Green";
}

function zoneColor(zone: ContextZone): string {
  switch (zone) {
    case "Green":
      return "var(--color-success, #22c55e)";
    case "Yellow":
      return "var(--color-warning, #eab308)";
    case "Orange":
      return "#f97316";
    case "Red":
      return "var(--color-danger, #ef4444)";
    case "Critical":
      return "#dc2626";
  }
}

function zoneRecommendation(zone: ContextZone): string {
  switch (zone) {
    case "Green":
      return "Context usage is healthy.";
    case "Yellow":
      return "Context filling up. Consider compacting soon.";
    case "Orange":
      return "Noticeable context loss. Compact recommended.";
    case "Red":
      return "Critical — compact now to avoid degradation.";
    case "Critical":
      return "Auto-compact triggered. Context nearly full.";
  }
}

function pct(value: number, total: number): number {
  if (total <= 0) return 0;
  return (value / total) * 100;
}

type CompactionStrategy = "summarize" | "truncate" | "hybrid" | "smart";

// ── Component ──────────────────────────────────────────────────────────────────

interface ContextViewerProps {
  tokenBudget: TokenBudget;
  pinnedItems?: PinnedContextItem[];
  onCompact?: (strategy: CompactionStrategy) => void;
  onUnpin?: (label: string) => void;
}

interface Segment {
  key: string;
  label: string;
  value: number;
  color: string;
}

export default function ContextViewer({
  tokenBudget,
  pinnedItems = [],
  onCompact,
  onUnpin,
}: ContextViewerProps) {
  const [selectedStrategy, setSelectedStrategy] = useState<CompactionStrategy>("hybrid");

  const total = Math.max(tokenBudget.context_limit, 1);
  const used = tokenBudget.used_input + tokenBudget.used_output + tokenBudget.reserved_output;
  const zone = zoneFromBudget(tokenBudget);
  const fillPct = pct(used, total);

  const systemPrompt = Math.min(tokenBudget.reserved_output, total);
  const conversation = Math.min(tokenBudget.used_input, Math.max(total - systemPrompt, 0));
  const toolResults = Math.min(
    tokenBudget.used_output,
    Math.max(total - systemPrompt - conversation, 0),
  );
  const pinned = pinnedItems.reduce((sum, item) => sum + item.tokens, 0);
  const remaining = Math.max(total - systemPrompt - conversation - toolResults - pinned, 0);

  const segments: Segment[] = [
    { key: "system", label: "System prompt", value: systemPrompt, color: "var(--color-accent-strong, #6366f1)" },
    { key: "conversation", label: "Conversation", value: conversation, color: "var(--color-accent, #818cf8)" },
    { key: "tools", label: "Tool results", value: toolResults, color: "var(--color-warning, #eab308)" },
    { key: "pinned", label: "Pinned", value: pinned, color: "var(--color-info, #06b6d4)" },
    { key: "remaining", label: "Remaining", value: remaining, color: "var(--color-success, #22c55e)" },
  ];

  const showWarning = zone === "Orange" || zone === "Red" || zone === "Critical";

  return (
    <div className="context-viewer">
      {/* Zone indicator header */}
      <div className="context-viewer__header">
        <div className="context-viewer__title">
          <strong>Context</strong>
          <span
            className="context-viewer__zone-badge"
            style={{ background: zoneColor(zone), color: "#fff" }}
          >
            {zone}
          </span>
        </div>
        <span className="context-viewer__stats">
          {used.toLocaleString()} / {total.toLocaleString()} tokens ({fillPct.toFixed(1)}%)
        </span>
      </div>

      {/* Warning banner */}
      {showWarning && (
        <div
          className="context-viewer__warning"
          style={{ borderColor: zoneColor(zone), background: `${zoneColor(zone)}15` }}
        >
          ⚠️ {zoneRecommendation(zone)}
        </div>
      )}

      {/* Donut / bar chart */}
      <div className="context-viewer__bar" aria-label="Context usage breakdown">
        {segments.map((segment) => {
          const w = pct(segment.value, total);
          if (w < 0.5) return null;
          return (
            <div
              key={segment.key}
              className="context-viewer__segment"
              style={{ width: `${w}%`, background: segment.color }}
              title={`${segment.label}: ${segment.value.toLocaleString()} tokens (${w.toFixed(1)}%)`}
            />
          );
        })}
      </div>

      {/* Legend */}
      <div className="context-viewer__legend">
        {segments.map((segment) => (
          <div key={segment.key} className="context-viewer__legend-item">
            <span className="context-viewer__swatch" style={{ background: segment.color }} />
            <span>{segment.label}</span>
            <strong>{segment.value.toLocaleString()}</strong>
          </div>
        ))}
      </div>

      {/* Pinned context items */}
      {pinnedItems.length > 0 && (
        <div className="context-viewer__pinned">
          <h4>📌 Pinned Context</h4>
          <ul>
            {pinnedItems.map((item) => (
              <li key={item.label} className="context-viewer__pin-item">
                <span className="context-viewer__pin-label">{item.label}</span>
                <span className="context-viewer__pin-tokens">{item.tokens} tokens</span>
                {onUnpin && (
                  <button
                    className="context-viewer__unpin-btn"
                    onClick={() => onUnpin(item.label)}
                    title={`Unpin "${item.label}"`}
                  >
                    ✕
                  </button>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}

      {/* Compaction controls */}
      <div className="context-viewer__controls">
        <select
          className="context-viewer__strategy-select"
          value={selectedStrategy}
          onChange={(e) => setSelectedStrategy(e.target.value as CompactionStrategy)}
        >
          <option value="hybrid">Hybrid</option>
          <option value="summarize">Summarize</option>
          <option value="truncate">Truncate</option>
          <option value="smart">Smart</option>
        </select>
        <button
          className="context-viewer__compact-btn"
          onClick={() => onCompact?.(selectedStrategy)}
          disabled={!onCompact}
        >
          Compact Now
        </button>
      </div>
    </div>
  );
}
