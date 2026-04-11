import React from "react";

interface CardTerminalProps {
  cardId: string;
  agentStatus: string;
  lastMessage: string;
  tokenUsage: { used: number; limit: number };
  isExpanded: boolean;
  onToggle: () => void;
}

export default function CardTerminal({ cardId, agentStatus, lastMessage, tokenUsage, isExpanded, onToggle }: CardTerminalProps) {
  const usagePercent = tokenUsage.limit > 0 ? Math.min((tokenUsage.used / tokenUsage.limit) * 100, 100) : 0;

  return (
    <button
      type="button"
      onClick={onToggle}
      style={{
        width: "100%",
        border: "1px solid var(--color-border, #313244)",
        borderRadius: 12,
        background: "var(--color-surface, #11111b)",
        color: "var(--color-text, #cdd6f4)",
        padding: 14,
        display: "flex",
        flexDirection: "column",
        gap: 10,
        textAlign: "left",
        cursor: "pointer",
      }}
    >
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
        <div>
          <div style={{ fontSize: 12, color: "var(--color-text-muted, #a6adc8)" }}>Card {cardId}</div>
          <div style={{ fontFamily: '"JetBrains Mono", monospace', fontSize: 13 }}>{agentStatus}</div>
        </div>
        <span style={{ fontSize: 12, color: "var(--color-accent, #89b4fa)" }}>{isExpanded ? "Collapse" : "Expand"}</span>
      </div>

      <div
        style={{
          fontFamily: '"JetBrains Mono", monospace',
          fontSize: 12,
          lineHeight: 1.5,
          minHeight: isExpanded ? 96 : 44,
          overflow: "hidden",
          whiteSpace: isExpanded ? "pre-wrap" : "nowrap",
          textOverflow: "ellipsis",
          borderRadius: 8,
          background: "#0b1020",
          padding: 12,
          border: "1px solid rgba(137, 180, 250, 0.2)",
        }}
      >
        <div style={{ color: "#94e2d5", marginBottom: 6 }}>$ agent status --card {cardId}</div>
        <div>{lastMessage || "Waiting for agent output…"}</div>
      </div>

      <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
        <div style={{ display: "flex", justifyContent: "space-between", fontSize: 12, color: "var(--color-text-muted, #a6adc8)" }}>
          <span>Token usage</span>
          <span>{tokenUsage.used}/{tokenUsage.limit}</span>
        </div>
        <div style={{ height: 8, borderRadius: 999, background: "rgba(255, 255, 255, 0.08)", overflow: "hidden" }}>
          <div
            style={{
              width: `${usagePercent}%`,
              height: "100%",
              background: usagePercent > 85 ? "#f38ba8" : "#89b4fa",
              transition: "width 160ms ease",
            }}
          />
        </div>
      </div>
    </button>
  );
}
