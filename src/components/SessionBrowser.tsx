import { useState } from "react";

export interface SessionEntry {
  id: string;
  date: string; // ISO string
  firstMessage: string;
  tokenCount: number;
  modelId: string;
}

interface Props {
  sessions: SessionEntry[];
  activeSessionId?: string;
  onResume: (sessionId: string) => void;
  onDelete: (sessionId: string) => void;
}

function formatDate(iso: string): string {
  const d = new Date(iso);
  const now = new Date();
  const diffMs = now.getTime() - d.getTime();
  const diffHours = diffMs / (1000 * 60 * 60);
  if (diffHours < 24) {
    return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  } else if (diffHours < 24 * 7) {
    return d.toLocaleDateString([], { weekday: "short", hour: "2-digit", minute: "2-digit" });
  }
  return d.toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" });
}

function formatTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M tok`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}K tok`;
  return `${n} tok`;
}

export default function SessionBrowser({ sessions, activeSessionId, onResume, onDelete }: Props) {
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);

  const sorted = [...sessions].sort(
    (a, b) => new Date(b.date).getTime() - new Date(a.date).getTime()
  );

  if (sorted.length === 0) {
    return (
      <div style={{ padding: 24, color: "#6c7086", textAlign: "center", fontSize: 13 }}>
        No past sessions
      </div>
    );
  }

  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        gap: 2,
        padding: "4px 0",
        overflowY: "auto",
        maxHeight: "100%",
      }}
    >
      {sorted.map((session) => {
        const isActive = session.id === activeSessionId;
        const isConfirming = confirmDelete === session.id;

        return (
          <div
            key={session.id}
            style={{
              padding: "8px 12px",
              background: isActive ? "#313244" : "transparent",
              borderLeft: isActive ? "2px solid #89b4fa" : "2px solid transparent",
              cursor: "pointer",
              display: "flex",
              flexDirection: "column",
              gap: 4,
              position: "relative",
            }}
            onClick={() => !isConfirming && onResume(session.id)}
          >
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start" }}>
              <span
                style={{
                  color: "#cdd6f4",
                  fontSize: 12,
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                  maxWidth: "70%",
                }}
              >
                {session.firstMessage || "(empty session)"}
              </span>
              <div style={{ display: "flex", gap: 6, alignItems: "center", flexShrink: 0 }}>
                <span style={{ color: "#6c7086", fontSize: 10 }}>{formatDate(session.date)}</span>
                {isConfirming ? (
                  <>
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        onDelete(session.id);
                        setConfirmDelete(null);
                      }}
                      style={dangerBtnStyle}
                    >
                      Confirm
                    </button>
                    <button
                      onClick={(e) => {
                        e.stopPropagation();
                        setConfirmDelete(null);
                      }}
                      style={cancelBtnStyle}
                    >
                      Cancel
                    </button>
                  </>
                ) : (
                  <button
                    onClick={(e) => {
                      e.stopPropagation();
                      setConfirmDelete(session.id);
                    }}
                    style={deleteBtnStyle}
                    title="Delete session"
                  >
                    ✕
                  </button>
                )}
              </div>
            </div>
            <div style={{ display: "flex", gap: 8 }}>
              <span style={{ color: "#6c7086", fontSize: 10 }}>{formatTokens(session.tokenCount)}</span>
              <span style={{ color: "#6c7086", fontSize: 10 }}>{session.modelId}</span>
            </div>
          </div>
        );
      })}
    </div>
  );
}

const deleteBtnStyle: React.CSSProperties = {
  background: "transparent",
  border: "none",
  color: "#6c7086",
  cursor: "pointer",
  fontSize: 10,
  padding: "1px 4px",
  borderRadius: 3,
  lineHeight: 1,
};

const dangerBtnStyle: React.CSSProperties = {
  background: "#f38ba8",
  border: "none",
  color: "#1e1e2e",
  cursor: "pointer",
  fontSize: 10,
  padding: "2px 6px",
  borderRadius: 3,
  fontWeight: 600,
};

const cancelBtnStyle: React.CSSProperties = {
  background: "#45475a",
  border: "none",
  color: "#cdd6f4",
  cursor: "pointer",
  fontSize: 10,
  padding: "2px 6px",
  borderRadius: 3,
};
