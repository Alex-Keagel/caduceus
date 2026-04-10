import { useEffect, useState } from "react";
import { gitStatus, gitFileDiff } from "../api/tauri";
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
  const [selectedFile, setSelectedFile] = useState<string | null>(null);
  const [diffContent, setDiffContent] = useState<string | null>(null);
  const [diffLoading, setDiffLoading] = useState(false);

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

  const handleFileClick = async (filePath: string) => {
    if (selectedFile === filePath) {
      setSelectedFile(null);
      setDiffContent(null);
      return;
    }
    setSelectedFile(filePath);
    setDiffLoading(true);
    try {
      const diff = await gitFileDiff(projectRoot!, filePath);
      setDiffContent(diff);
    } catch (e) {
      setDiffContent(`Error: ${e}`);
    } finally {
      setDiffLoading(false);
    }
  };

  const renderDiffLine = (line: string, index: number) => {
    let color = "#cdd6f4";
    if (line.startsWith("+")) color = "#a6e3a1";
    else if (line.startsWith("-")) color = "#f38ba8";
    else if (line.startsWith("@@")) color = "#89b4fa";
    return (
      <div key={index} style={{ color, whiteSpace: "pre", minHeight: "1em" }}>
        {line}
      </div>
    );
  };

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
          onClick={() => handleFileClick(entry.path)}
          style={{
            display: "flex",
            alignItems: "center",
            gap: 6,
            padding: "3px 4px",
            borderRadius: 3,
            fontSize: 11,
            cursor: "pointer",
            background: selectedFile === entry.path ? "#313244" : "transparent",
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

      {selectedFile && (
        <div
          style={{
            marginTop: 8,
            borderTop: "1px solid #313244",
            paddingTop: 8,
          }}
        >
          <div
            style={{
              fontSize: 10,
              color: "#6c7086",
              marginBottom: 4,
              padding: "0 4px",
            }}
          >
            {selectedFile}
          </div>
          {diffLoading ? (
            <div style={{ color: "#6c7086", fontSize: 11, padding: "0 4px" }}>
              Loading diff…
            </div>
          ) : (
            <pre
              style={{
                background: "#11111b",
                border: "1px solid #313244",
                borderRadius: 4,
                padding: 6,
                fontSize: 10,
                fontFamily: '"JetBrains Mono", monospace',
                overflow: "auto",
                maxHeight: 300,
                margin: 0,
              }}
            >
              {diffContent?.split("\n").map(renderDiffLine)}
            </pre>
          )}
        </div>
      )}
    </div>
  );
}
