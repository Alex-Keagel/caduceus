import React, { useCallback, useMemo, useState } from "react";

type VimModeType = "normal" | "insert" | "visual" | "command";

interface VimState {
  mode: VimModeType;
  commandBuffer: string;
  register: string;
  lastCommand: string;
}

const containerStyle: React.CSSProperties = {
  display: "flex",
  flexDirection: "column",
  gap: 8,
  border: "1px solid var(--color-border, #313244)",
  borderRadius: 10,
  background: "var(--color-surface, #11111b)",
  color: "var(--color-text, #cdd6f4)",
  padding: 12,
  position: "relative",
};

const editorStyle: React.CSSProperties = {
  width: "100%",
  minHeight: 140,
  resize: "vertical",
  border: "none",
  outline: "none",
  background: "transparent",
  color: "inherit",
  fontFamily: '"JetBrains Mono", monospace',
  fontSize: 13,
  lineHeight: 1.5,
};

export default function VimModeInput({ onSubmit, enabled }: { onSubmit: (text: string) => void; enabled: boolean }) {
  const [text, setText] = useState("");
  const [visualRange, setVisualRange] = useState<{ start: number; end: number } | null>(null);
  const [vimState, setVimState] = useState<VimState>({
    mode: enabled ? "normal" : "insert",
    commandBuffer: "",
    register: "",
    lastCommand: "",
  });

  const mode = enabled ? vimState.mode : "insert";

  const clearCommandBuffer = useCallback(() => {
    setVimState((current) => ({ ...current, commandBuffer: "" }));
  }, []);

  const submitText = useCallback(() => {
    const trimmed = text.trim();
    if (trimmed) {
      onSubmit(trimmed);
      setText("");
      setVisualRange(null);
      setVimState((current) => ({ ...current, commandBuffer: "", lastCommand: ":w" }));
    }
  }, [onSubmit, text]);

  const clearText = useCallback(() => {
    setText("");
    setVisualRange(null);
    setVimState((current) => ({ ...current, commandBuffer: "", lastCommand: ":q" }));
  }, []);

  const deleteCurrentLine = useCallback(() => {
    const lines = text.split("\n");
    lines.pop();
    setText(lines.join("\n"));
    setVimState((current) => ({ ...current, lastCommand: "dd", commandBuffer: "", register: "" }));
  }, [text]);

  const yankCurrentLine = useCallback(() => {
    const line = text.split("\n").pop() ?? "";
    setVimState((current) => ({ ...current, register: line, lastCommand: "yy", commandBuffer: "" }));
  }, [text]);

  const deleteVisualSelection = useCallback(() => {
    if (!visualRange) return;
    const start = Math.max(0, Math.min(visualRange.start, visualRange.end));
    const end = Math.max(visualRange.start, visualRange.end);
    setText((current) => `${current.slice(0, start)}${current.slice(end)}`);
    setVisualRange(null);
    setVimState((current) => ({ ...current, mode: "normal", lastCommand: "vd", commandBuffer: "" }));
  }, [visualRange]);

  const yankVisualSelection = useCallback(() => {
    if (!visualRange) return;
    const start = Math.max(0, Math.min(visualRange.start, visualRange.end));
    const end = Math.max(visualRange.start, visualRange.end);
    setVimState((current) => ({
      ...current,
      register: text.slice(start, end),
      mode: "normal",
      lastCommand: "vy",
      commandBuffer: "",
    }));
    setVisualRange(null);
  }, [text, visualRange]);

  const modeLabel = useMemo(() => mode.toUpperCase(), [mode]);

  const handleKeyDown = useCallback((event: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (!enabled) {
      if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
        event.preventDefault();
        submitText();
      }
      return;
    }

    if (mode === "insert") {
      if (event.key === "Escape") {
        event.preventDefault();
        setVimState((current) => ({ ...current, mode: "normal", commandBuffer: "" }));
      } else if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
        event.preventDefault();
        submitText();
      }
      return;
    }

    if (mode === "command") {
      if (event.key === "Escape") {
        event.preventDefault();
        setVimState((current) => ({ ...current, mode: "normal", commandBuffer: "" }));
        return;
      }
      if (event.key === "Backspace") {
        event.preventDefault();
        setVimState((current) => ({
          ...current,
          commandBuffer: current.commandBuffer.slice(0, -1),
          mode: current.commandBuffer.length <= 1 ? "normal" : "command",
        }));
        return;
      }
      if (event.key === "Enter") {
        event.preventDefault();
        if (vimState.commandBuffer === ":w") submitText();
        if (vimState.commandBuffer === ":q") clearText();
        setVimState((current) => ({ ...current, mode: "normal", commandBuffer: "" }));
        return;
      }
      if (event.key.length === 1) {
        event.preventDefault();
        setVimState((current) => ({ ...current, commandBuffer: `${current.commandBuffer}${event.key}` }));
      }
      return;
    }

    event.preventDefault();

    if (mode === "visual") {
      if (event.key === "Escape") {
        setVisualRange(null);
        setVimState((current) => ({ ...current, mode: "normal", commandBuffer: "" }));
        return;
      }
      if (event.key === "y") {
        yankVisualSelection();
        return;
      }
      if (event.key === "d") {
        deleteVisualSelection();
        return;
      }
      if (event.key === "h") {
        setVisualRange((current) => current ? { ...current, end: Math.max(0, current.end - 1) } : current);
        return;
      }
      if (event.key === "l") {
        setVisualRange((current) => current ? { ...current, end: Math.min(text.length, current.end + 1) } : current);
      }
      return;
    }

    if (event.key === "i" || event.key === "a" || event.key === "o") {
      setVimState((current) => ({ ...current, mode: "insert", lastCommand: event.key, commandBuffer: "" }));
      if (event.key === "o" && text.length > 0) {
        setText((current) => `${current}\n`);
      }
      return;
    }

    if (event.key === "v") {
      setVisualRange({ start: text.length, end: text.length });
      setVimState((current) => ({ ...current, mode: "visual", lastCommand: "v", commandBuffer: "" }));
      return;
    }

    if (event.key === ":") {
      setVimState((current) => ({ ...current, mode: "command", commandBuffer: ":", lastCommand: ":" }));
      return;
    }

    if (event.key === "y" && vimState.commandBuffer === "y") {
      yankCurrentLine();
      return;
    }
    if (event.key === "d" && vimState.commandBuffer === "d") {
      deleteCurrentLine();
      return;
    }

    if (event.key === "y" || event.key === "d") {
      setVimState((current) => ({ ...current, commandBuffer: event.key }));
      return;
    }

    if (["h", "j", "k", "l"].includes(event.key)) {
      setVimState((current) => ({ ...current, lastCommand: event.key, commandBuffer: "" }));
      return;
    }

    clearCommandBuffer();
  }, [clearCommandBuffer, clearText, deleteCurrentLine, deleteVisualSelection, enabled, mode, submitText, text, vimState.commandBuffer, yankCurrentLine, yankVisualSelection]);

  return (
    <div style={containerStyle}>
      <div style={{ fontSize: 12, color: "var(--color-text-muted, #a6adc8)" }}>
        Vim mode input {enabled ? "enabled" : "disabled"} · Normal: h/j/k/l, i/a/o, dd, yy · Command: :w / :q
      </div>
      <textarea
        value={text}
        onChange={(event) => setText(event.target.value)}
        onKeyDown={handleKeyDown}
        style={editorStyle}
        spellCheck={false}
        placeholder={enabled ? "Press i to enter insert mode" : "Type here…"}
      />
      <div style={{ display: "flex", justifyContent: "space-between", fontSize: 12, color: "var(--color-text-muted, #a6adc8)" }}>
        <span>Register: {vimState.register || "∅"}</span>
        <span>Last command: {vimState.lastCommand || "—"}</span>
      </div>
      <div
        style={{
          position: "absolute",
          left: 12,
          bottom: 12,
          padding: "2px 8px",
          borderRadius: 999,
          background: "var(--color-accent, #89b4fa)",
          color: "#11111b",
          fontSize: 11,
          fontWeight: 700,
          letterSpacing: 0.4,
        }}
      >
        {modeLabel}
        {mode === "command" && vimState.commandBuffer ? ` ${vimState.commandBuffer}` : ""}
      </div>
    </div>
  );
}
