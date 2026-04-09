import { useState, useEffect, useRef } from "react";
import { sessionList } from "../api/tauri";
import type { SessionInfo } from "../types";

interface CommandPaletteProps {
  onClose: () => void;
  onSessionSelect: (session: SessionInfo) => void;
}

interface CommandItem {
  id: string;
  label: string;
  description?: string;
  action: () => void;
}

export default function CommandPalette({ onClose, onSessionSelect }: CommandPaletteProps) {
  const [query, setQuery] = useState("");
  const [sessions, setSessions] = useState<SessionInfo[]>([]);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
    sessionList().then(setSessions).catch(console.error);
  }, []);

  const staticCommands: CommandItem[] = [
    {
      id: "new-session",
      label: "New Session",
      description: "Create a new agent session",
      action: onClose,
    },
    {
      id: "settings",
      label: "Open Settings",
      description: "Configure providers and models",
      action: onClose,
    },
  ];

  const sessionCommands: CommandItem[] = sessions.map((s) => ({
    id: s.id,
    label: s.project_root,
    description: `${s.model_id} · ${s.message_count} messages`,
    action: () => {
      onSessionSelect(s);
      onClose();
    },
  }));

  const allCommands = [...staticCommands, ...sessionCommands];

  const filtered = allCommands.filter(
    (c) =>
      !query ||
      c.label.toLowerCase().includes(query.toLowerCase()) ||
      c.description?.toLowerCase().includes(query.toLowerCase())
  );

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      onClose();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setSelectedIndex((i) => Math.min(i + 1, filtered.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelectedIndex((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter") {
      filtered[selectedIndex]?.action();
    }
  };

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        background: "#00000080",
        display: "flex",
        alignItems: "flex-start",
        justifyContent: "center",
        paddingTop: "15vh",
        zIndex: 1000,
      }}
      onClick={onClose}
    >
      <div
        style={{
          width: 560,
          background: "#1e1e2e",
          border: "1px solid #45475a",
          borderRadius: 8,
          boxShadow: "0 24px 64px #000000aa",
          overflow: "hidden",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <input
          ref={inputRef}
          value={query}
          onChange={(e) => {
            setQuery(e.target.value);
            setSelectedIndex(0);
          }}
          onKeyDown={handleKeyDown}
          placeholder="Search commands and sessions…"
          style={{
            width: "100%",
            background: "transparent",
            border: "none",
            borderBottom: "1px solid #45475a",
            padding: "14px 16px",
            color: "#cdd6f4",
            fontSize: 14,
            outline: "none",
            fontFamily: "inherit",
          }}
        />

        <div style={{ maxHeight: 360, overflowY: "auto" }}>
          {filtered.map((cmd, i) => (
            <div
              key={cmd.id}
              onClick={cmd.action}
              style={{
                padding: "10px 16px",
                background: i === selectedIndex ? "#313244" : "transparent",
                cursor: "pointer",
                display: "flex",
                flexDirection: "column",
                gap: 2,
              }}
              onMouseEnter={() => setSelectedIndex(i)}
            >
              <span style={{ fontSize: 13 }}>{cmd.label}</span>
              {cmd.description && (
                <span style={{ fontSize: 11, color: "#6c7086" }}>
                  {cmd.description}
                </span>
              )}
            </div>
          ))}
          {filtered.length === 0 && (
            <div style={{ padding: "16px", color: "#6c7086", fontSize: 12 }}>
              No results
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
