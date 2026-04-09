import { useEffect, useState } from "react";
import { gitStatus } from "../api/tauri";
import type { GitStatusEntry } from "../types";

interface GitPanelProps {
  projectRoot: string | null;
}

const STATUS_ICON: Record<string, string> = {
  New: "✚",
  Modified: "●",
  Deleted: "✖",
  Renamed: "➜",
  Untracked: "?",
  Conflicted: "!",
};

const STATUS_COLOR: Record<string, string> = {
  New: "#a6e3a1",
  Modified: "#f9e2af",
  Deleted: "#f38ba8",
  Renamed: "#89dceb",
  Untracked: "#6c7086",
  Conflicted: "#f38ba8",
};

export default function GitPanel({ projectRoot }: GitPanelProps) {
  const [entries, setEntries] = useState<GitStatusEntry[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!projectRoot) {
      setEntries([]);
      return;
    }

    const load = async () => {
      try {
        const status = await gitStatus(projectRoot);
        setEntries(status);
        setError(null);
      } catch (e) {
        setError(String(e));
      }
    };

    load();
    const interval = setInterval(load, 5000);
    return () => clearInterval(interval);
  }, [projectRoot]);

  return (
    <div>
      <div
        style={{
          fontSize: 10,
          fontWeight: 700,
          color: "#6c7086",
          textTransform: "uppercase",
          letterSpacing: 1,
          marginBottom: 8,
          padding: "0 4px",
        }}
      >
        Git Changes
      </div>

      {error && (
        <div style={{ color: "#f38ba8", fontSize: 11, padding: "0 4px" }}>
          {error}
        </div>
      )}

      {!projectRoot && (
        <div style={{ color: "#6c7086", fontSize: 11, padding: "0 4px" }}>
          No project open
        </div>
      )}

      {entries.length === 0 && projectRoot && !error && (
        <div style={{ color: "#6c7086", fontSize: 11, padding: "0 4px" }}>
          Working tree clean
        </div>
      )}

      {entries.map((entry) => (
        <div
          key={entry.path}
          style={{
            display: "flex",
            alignItems: "center",
            gap: 6,
            padding: "3px 4px",
            borderRadius: 3,
            fontSize: 11,
          }}
        >
          <span style={{ color: STATUS_COLOR[entry.status] ?? "#cdd6f4" }}>
            {STATUS_ICON[entry.status] ?? "?"}
          </span>
          <span
            style={{
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
              color: "#cdd6f4",
            }}
            title={entry.path}
          >
            {entry.path.split("/").pop() ?? entry.path}
          </span>
        </div>
      ))}
    </div>
  );
}
