import type { TokenBudget } from "../types";

interface ContextVisualizerProps {
  tokenBudget: TokenBudget;
}

interface Segment {
  key: string;
  label: string;
  value: number;
  color: string;
}

function pct(value: number, total: number): number {
  if (total <= 0) return 0;
  return (value / total) * 100;
}

export default function ContextVisualizer({ tokenBudget }: ContextVisualizerProps) {
  const total = Math.max(tokenBudget.context_limit, 1);
  const systemPrompt = Math.min(tokenBudget.reserved_output, total);
  const conversation = Math.min(tokenBudget.used_input, Math.max(total - systemPrompt, 0));
  const toolResults = Math.min(
    tokenBudget.used_output,
    Math.max(total - systemPrompt - conversation, 0)
  );
  const remaining = Math.max(total - systemPrompt - conversation - toolResults, 0);

  const segments: Segment[] = [
    { key: "system", label: "System prompt", value: systemPrompt, color: "var(--color-accent-strong)" },
    { key: "conversation", label: "Conversation", value: conversation, color: "var(--color-accent)" },
    { key: "tools", label: "Tool results", value: toolResults, color: "var(--color-warning)" },
    { key: "remaining", label: "Remaining", value: remaining, color: "var(--color-success)" },
  ];

  return (
    <div className="context-visualizer">
      <div className="context-visualizer__header">
        <strong>Context</strong>
        <span>{tokenBudget.context_limit.toLocaleString()} tokens</span>
      </div>
      <div className="context-visualizer__bar" aria-label="Context usage breakdown">
        {segments.map((segment) => (
          <div
            key={segment.key}
            className="context-visualizer__segment"
            style={{ width: `${pct(segment.value, total)}%`, background: segment.color }}
            title={`${segment.label}: ${pct(segment.value, total).toFixed(1)}%`}
          />
        ))}
      </div>
      <div className="context-visualizer__legend">
        {segments.map((segment) => (
          <div key={segment.key} className="context-visualizer__legend-item">
            <span className="context-visualizer__swatch" style={{ background: segment.color }} />
            <span>{segment.label}</span>
            <strong>{pct(segment.value, total).toFixed(1)}%</strong>
          </div>
        ))}
      </div>
    </div>
  );
}
