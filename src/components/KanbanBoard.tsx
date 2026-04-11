import { useMemo, useState } from "react";
import type { KanbanBoard as KanbanBoardData, KanbanCard, CardStatus, KanbanTokenUsage } from "../types";

interface KanbanBoardProps {
  board: KanbanBoardData | null;
}

function statusLabel(status: CardStatus): string {
  if (typeof status === "string") {
    return status;
  }
  if ("Blocked" in status) {
    return `Blocked: ${status.Blocked}`;
  }
  return `Failed: ${status.Failed}`;
}

function tokenCount(usage: KanbanTokenUsage): number {
  return usage.input_tokens
    + usage.output_tokens
    + (usage.cache_read_tokens ?? 0)
    + (usage.cache_write_tokens ?? 0);
}

function statusColor(status: CardStatus): string {
  const label = statusLabel(status);
  if (label.startsWith("Blocked") || label.startsWith("Failed")) return "#f38ba8";
  if (label === "Done") return "#a6e3a1";
  if (label === "NeedsReview") return "#f9e2af";
  if (label === "Running") return "#89b4fa";
  return "#cba6f7";
}

export default function KanbanBoard({ board }: KanbanBoardProps) {
  const [expandedCardId, setExpandedCardId] = useState<string | null>(null);

  const cardsById = useMemo(() => {
    const entries = new Map<string, KanbanCard>();
    for (const card of board?.cards ?? []) {
      entries.set(card.id, card);
    }
    return entries;
  }, [board]);

  if (!board) {
    return (
      <div style={{ padding: 16, color: "#6c7086", fontSize: 13 }}>
        Open a session and run <code>/kanban</code> to load the board.
      </div>
    );
  }

  return (
    <div
      style={{
        display: "grid",
        gridTemplateColumns: "repeat(4, minmax(0, 1fr))",
        gap: 16,
        padding: 16,
        height: "100%",
        overflow: "auto",
        background: "#11111b",
      }}
    >
      {board.columns.map((column) => (
        <section
          key={column.id}
          style={{
            display: "flex",
            flexDirection: "column",
            gap: 12,
            background: "#181825",
            border: "1px solid #313244",
            borderRadius: 12,
            padding: 12,
            minHeight: 0,
          }}
        >
          <header style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
            <div>
              <div style={{ fontSize: 13, fontWeight: 700 }}>{column.name}</div>
              <div style={{ fontSize: 11, color: "#6c7086", marginTop: 2 }}>
                {column.card_ids.length} card{column.card_ids.length === 1 ? "" : "s"}
              </div>
            </div>
            {column.wip_limit ? (
              <span style={{ fontSize: 10, color: "#f9e2af" }}>WIP {column.wip_limit}</span>
            ) : null}
          </header>

          <div style={{ display: "flex", flexDirection: "column", gap: 10, minHeight: 0 }}>
            {column.card_ids.map((cardId) => {
              const card = cardsById.get(cardId);
              if (!card) return null;
              const expanded = expandedCardId === card.id;
              return (
                <button
                  key={card.id}
                  type="button"
                  onClick={() => setExpandedCardId(expanded ? null : card.id)}
                  style={{
                    textAlign: "left",
                    border: "1px solid #45475a",
                    borderRadius: 10,
                    background: "#1e1e2e",
                    color: "#cdd6f4",
                    padding: 12,
                    cursor: "pointer",
                  }}
                >
                  <div style={{ display: "flex", justifyContent: "space-between", gap: 8 }}>
                    <strong style={{ fontSize: 13 }}>{card.title}</strong>
                    <span style={{ fontSize: 11, color: "#6c7086" }}>{tokenCount(card.token_usage)} tok</span>
                  </div>
                  <div style={{ marginTop: 8, display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
                    <span
                      style={{
                        fontSize: 10,
                        borderRadius: 999,
                        padding: "3px 8px",
                        background: `${statusColor(card.status)}22`,
                        color: statusColor(card.status),
                        border: `1px solid ${statusColor(card.status)}55`,
                      }}
                    >
                      {statusLabel(card.status)}
                    </span>
                    {card.dependencies.length > 0 ? (
                      <span style={{ fontSize: 10, color: "#94e2d5" }}>
                        {card.dependencies.length} dependenc{card.dependencies.length === 1 ? "y" : "ies"}
                      </span>
                    ) : null}
                  </div>
                  {expanded ? (
                    <div style={{ marginTop: 10, fontSize: 12, color: "#bac2de", whiteSpace: "pre-wrap" }}>
                      {card.description || "No description yet."}
                      <div style={{ marginTop: 8, color: "#6c7086" }}>Card ID: {card.id}</div>
                    </div>
                  ) : null}
                </button>
              );
            })}
            {column.card_ids.length === 0 ? (
              <div style={{ fontSize: 12, color: "#6c7086", padding: "12px 4px" }}>No cards</div>
            ) : null}
          </div>
        </section>
      ))}
    </div>
  );
}
