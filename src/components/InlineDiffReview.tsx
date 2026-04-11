import React, { useMemo, useState } from "react";

interface DiffLine {
  type: "add" | "remove" | "context";
  content: string;
  lineNumber: number;
}

interface DiffComment {
  lineNumber: number;
  text: string;
  author: string;
  timestamp: number;
}

interface InlineDiffReviewProps {
  diff: DiffLine[];
  comments: DiffComment[];
  onAddComment: (lineNumber: number, text: string) => void;
}

const lineColors: Record<DiffLine["type"], { background: string; accent: string }> = {
  add: { background: "rgba(166, 227, 161, 0.12)", accent: "#a6e3a1" },
  remove: { background: "rgba(243, 139, 168, 0.12)", accent: "#f38ba8" },
  context: { background: "transparent", accent: "#6c7086" },
};

export default function InlineDiffReview({ diff, comments, onAddComment }: InlineDiffReviewProps) {
  const [drafts, setDrafts] = useState<Record<number, string>>({});
  const [activeLine, setActiveLine] = useState<number | null>(null);

  const commentsByLine = useMemo(() => {
    return comments.reduce<Record<number, DiffComment[]>>((acc, comment) => {
      acc[comment.lineNumber] = [...(acc[comment.lineNumber] ?? []), comment];
      return acc;
    }, {});
  }, [comments]);

  return (
    <div style={{ border: "1px solid var(--color-border, #313244)", borderRadius: 12, overflow: "hidden" }}>
      {diff.map((line) => {
        const palette = lineColors[line.type];
        const lineComments = commentsByLine[line.lineNumber] ?? [];
        const draft = drafts[line.lineNumber] ?? "";
        const isActive = activeLine === line.lineNumber;

        return (
          <div key={`${line.lineNumber}-${line.type}-${line.content}`}>
            <button
              type="button"
              onClick={() => setActiveLine(isActive ? null : line.lineNumber)}
              style={{
                width: "100%",
                display: "flex",
                gap: 12,
                alignItems: "flex-start",
                background: palette.background,
                color: "var(--color-text, #cdd6f4)",
                border: "none",
                borderBottom: "1px solid rgba(255, 255, 255, 0.04)",
                padding: "10px 12px",
                cursor: "pointer",
                fontFamily: '"JetBrains Mono", monospace',
                fontSize: 12,
                textAlign: "left",
              }}
            >
              <span style={{ width: 48, color: "#7f849c" }}>{line.lineNumber}</span>
              <span style={{ width: 12, color: palette.accent }}>{line.type === "add" ? "+" : line.type === "remove" ? "-" : " "}</span>
              <span style={{ whiteSpace: "pre-wrap", flex: 1 }}>{line.content}</span>
            </button>

            {lineComments.map((comment) => (
              <div
                key={`${comment.lineNumber}-${comment.timestamp}-${comment.author}`}
                style={{
                  margin: "8px 12px 8px 72px",
                  padding: 10,
                  borderRadius: 10,
                  background: "rgba(137, 180, 250, 0.12)",
                  color: "var(--color-text, #cdd6f4)",
                  fontSize: 12,
                }}
              >
                <div style={{ color: "var(--color-text-muted, #a6adc8)", marginBottom: 4 }}>
                  {comment.author} · {new Date(comment.timestamp).toLocaleString()}
                </div>
                <div>{comment.text}</div>
              </div>
            ))}

            {isActive ? (
              <div style={{ margin: "0 12px 12px 72px", display: "flex", flexDirection: "column", gap: 8 }}>
                <textarea
                  value={draft}
                  onChange={(event) => setDrafts((current) => ({ ...current, [line.lineNumber]: event.target.value }))}
                  placeholder={`Add a comment for line ${line.lineNumber}`}
                  style={{
                    minHeight: 72,
                    borderRadius: 10,
                    border: "1px solid var(--color-border, #313244)",
                    background: "var(--color-surface, #11111b)",
                    color: "var(--color-text, #cdd6f4)",
                    padding: 10,
                    resize: "vertical",
                  }}
                />
                <div style={{ display: "flex", justifyContent: "flex-end", gap: 8 }}>
                  <button type="button" onClick={() => setActiveLine(null)} style={{ padding: "6px 10px" }}>
                    Cancel
                  </button>
                  <button
                    type="button"
                    onClick={() => {
                      const value = draft.trim();
                      if (!value) return;
                      onAddComment(line.lineNumber, value);
                      setDrafts((current) => ({ ...current, [line.lineNumber]: "" }));
                      setActiveLine(null);
                    }}
                    style={{ padding: "6px 10px" }}
                  >
                    Comment
                  </button>
                </div>
              </div>
            ) : null}
          </div>
        );
      })}
    </div>
  );
}
