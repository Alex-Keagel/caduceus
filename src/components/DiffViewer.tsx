import { useState } from "react";

export interface FileDiff {
  path: string;
  patch: string; // unified diff patch for this file
  insertions?: number;
  deletions?: number;
}

interface Props {
  diffs: FileDiff[];
  className?: string;
}

type DiffLine =
  | { kind: "context"; text: string; oldNo: number; newNo: number }
  | { kind: "add"; text: string; newNo: number }
  | { kind: "del"; text: string; oldNo: number }
  | { kind: "hunk"; text: string };

function parsePatch(patch: string): DiffLine[] {
  const lines: DiffLine[] = [];
  let oldLine = 0;
  let newLine = 0;

  for (const raw of patch.split("\n")) {
    const hunkMatch = raw.match(/^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/);
    if (hunkMatch) {
      oldLine = parseInt(hunkMatch[1], 10);
      newLine = parseInt(hunkMatch[2], 10);
      lines.push({ kind: "hunk", text: raw });
      continue;
    }
    if (raw.startsWith("+") && !raw.startsWith("+++")) {
      lines.push({ kind: "add", text: raw.slice(1), newNo: newLine++ });
    } else if (raw.startsWith("-") && !raw.startsWith("---")) {
      lines.push({ kind: "del", text: raw.slice(1), oldNo: oldLine++ });
    } else if (raw.startsWith(" ")) {
      lines.push({ kind: "context", text: raw.slice(1), oldNo: oldLine++, newNo: newLine++ });
    }
  }

  return lines;
}

function fileExtension(path: string): string {
  return path.split(".").pop() ?? "";
}

function langColor(ext: string): string {
  const colors: Record<string, string> = {
    rs: "#f38ba8",
    ts: "#89b4fa",
    tsx: "#89dceb",
    js: "#f9e2af",
    jsx: "#fab387",
    py: "#a6e3a1",
    go: "#89dceb",
    md: "#cdd6f4",
    json: "#f9e2af",
    toml: "#fab387",
    css: "#89b4fa",
    html: "#f38ba8",
  };
  return colors[ext] ?? "#6c7086";
}

export default function DiffViewer({ diffs, className }: Props) {
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

  if (diffs.length === 0) {
    return (
      <div style={{ padding: 24, color: "#6c7086", textAlign: "center", fontSize: 13 }}>
        No changes
      </div>
    );
  }

  const toggleCollapse = (path: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(path)) next.delete(path);
      else next.add(path);
      return next;
    });
  };

  return (
    <div
      style={{ fontFamily: "monospace", fontSize: 12, background: "#181825", color: "#cdd6f4" }}
      className={className}
    >
      {diffs.map((diff) => {
        const isCollapsed = collapsed.has(diff.path);
        const ext = fileExtension(diff.path);
        const lines = parsePatch(diff.patch);
        const insertions = diff.insertions ?? lines.filter((l) => l.kind === "add").length;
        const deletions = diff.deletions ?? lines.filter((l) => l.kind === "del").length;

        return (
          <div
            key={diff.path}
            style={{ borderBottom: "1px solid #313244", marginBottom: 0 }}
          >
            {/* File header */}
            <div
              onClick={() => toggleCollapse(diff.path)}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 8,
                padding: "6px 12px",
                background: "#1e1e2e",
                cursor: "pointer",
                userSelect: "none",
                borderBottom: isCollapsed ? "none" : "1px solid #313244",
              }}
            >
              <span style={{ color: "#6c7086", fontSize: 10 }}>{isCollapsed ? "▶" : "▼"}</span>
              <span style={{ color: langColor(ext), fontWeight: 600 }}>{diff.path}</span>
              <div style={{ marginLeft: "auto", display: "flex", gap: 8 }}>
                {insertions > 0 && (
                  <span style={{ color: "#a6e3a1", fontSize: 11 }}>+{insertions}</span>
                )}
                {deletions > 0 && (
                  <span style={{ color: "#f38ba8", fontSize: 11 }}>-{deletions}</span>
                )}
              </div>
            </div>

            {/* Diff lines */}
            {!isCollapsed && (
              <div style={{ overflowX: "auto" }}>
                {lines.map((line, i) => {
                  if (line.kind === "hunk") {
                    return (
                      <div
                        key={i}
                        style={{
                          background: "#1e1e2e",
                          color: "#89b4fa",
                          padding: "2px 12px",
                          borderTop: "1px solid #313244",
                          borderBottom: "1px solid #313244",
                        }}
                      >
                        {line.text}
                      </div>
                    );
                  }
                  if (line.kind === "add") {
                    return (
                      <div key={i} style={{ display: "flex", background: "#1a2e1a" }}>
                        <span
                          style={{
                            minWidth: 40,
                            color: "#6c7086",
                            padding: "1px 8px",
                            textAlign: "right",
                            borderRight: "1px solid #313244",
                            userSelect: "none",
                          }}
                        >
                          {line.newNo}
                        </span>
                        <span style={{ color: "#a6e3a1", padding: "1px 2px 1px 0", marginLeft: 4 }}>+</span>
                        <span style={{ color: "#cdd6f4", padding: "1px 8px", whiteSpace: "pre" }}>
                          {line.text}
                        </span>
                      </div>
                    );
                  }
                  if (line.kind === "del") {
                    return (
                      <div key={i} style={{ display: "flex", background: "#2e1a1a" }}>
                        <span
                          style={{
                            minWidth: 40,
                            color: "#6c7086",
                            padding: "1px 8px",
                            textAlign: "right",
                            borderRight: "1px solid #313244",
                            userSelect: "none",
                          }}
                        >
                          {line.oldNo}
                        </span>
                        <span style={{ color: "#f38ba8", padding: "1px 2px 1px 0", marginLeft: 4 }}>-</span>
                        <span style={{ color: "#cdd6f4", padding: "1px 8px", whiteSpace: "pre" }}>
                          {line.text}
                        </span>
                      </div>
                    );
                  }
                  // context line
                  return (
                    <div key={i} style={{ display: "flex", background: "#181825" }}>
                      <span
                        style={{
                          minWidth: 40,
                          color: "#45475a",
                          padding: "1px 8px",
                          textAlign: "right",
                          borderRight: "1px solid #313244",
                          userSelect: "none",
                        }}
                      >
                        {line.oldNo}
                      </span>
                      <span style={{ color: "#6c7086", padding: "1px 2px 1px 0", marginLeft: 4 }}> </span>
                      <span style={{ color: "#cdd6f4", padding: "1px 8px", whiteSpace: "pre" }}>
                        {line.text}
                      </span>
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
