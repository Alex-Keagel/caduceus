import { useState, useRef, useEffect, useCallback } from "react";

export interface ModelOption {
  id: string;
  name: string;
  provider: string;
  contextWindow: number;
}

const MODELS: ModelOption[] = [
  { id: "claude-opus-4-5", name: "Claude Opus 4.5", provider: "Anthropic", contextWindow: 200000 },
  { id: "claude-sonnet-4-5", name: "Claude Sonnet 4.5", provider: "Anthropic", contextWindow: 200000 },
  { id: "claude-haiku-4-5", name: "Claude Haiku 4.5", provider: "Anthropic", contextWindow: 200000 },
  { id: "gpt-4o", name: "GPT-4o", provider: "OpenAI", contextWindow: 128000 },
  { id: "gpt-4o-mini", name: "GPT-4o Mini", provider: "OpenAI", contextWindow: 128000 },
  { id: "gemini-1.5-pro", name: "Gemini 1.5 Pro", provider: "Google", contextWindow: 1000000 },
  { id: "gemini-1.5-flash", name: "Gemini 1.5 Flash", provider: "Google", contextWindow: 1000000 },
];

interface Props {
  value: string;
  onChange: (modelId: string) => void;
  className?: string;
}

function formatContextWindow(tokens: number): string {
  if (tokens >= 1_000_000) return `${tokens / 1_000_000}M ctx`;
  if (tokens >= 1_000) return `${tokens / 1_000}K ctx`;
  return `${tokens} ctx`;
}

export default function ModelPicker({ value, onChange, className }: Props) {
  const [open, setOpen] = useState(false);
  const [search, setSearch] = useState("");
  const [highlighted, setHighlighted] = useState(0);
  const containerRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  const providers = Array.from(new Set(MODELS.map((m) => m.provider)));

  const filtered = search.trim()
    ? MODELS.filter(
        (m) =>
          m.name.toLowerCase().includes(search.toLowerCase()) ||
          m.provider.toLowerCase().includes(search.toLowerCase())
      )
    : MODELS;

  const currentModel = MODELS.find((m) => m.id === value);

  const handleOpen = () => {
    setOpen(true);
    setSearch("");
    setHighlighted(0);
    setTimeout(() => inputRef.current?.focus(), 0);
  };

  const handleSelect = useCallback(
    (id: string) => {
      onChange(id);
      setOpen(false);
      setSearch("");
    },
    [onChange]
  );

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setHighlighted((h) => Math.min(h + 1, filtered.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setHighlighted((h) => Math.max(h - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      if (filtered[highlighted]) {
        handleSelect(filtered[highlighted].id);
      }
    } else if (e.key === "Escape") {
      setOpen(false);
    }
  };

  useEffect(() => {
    const handleClickOutside = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    if (open) document.addEventListener("mousedown", handleClickOutside);
    return () => document.removeEventListener("mousedown", handleClickOutside);
  }, [open]);

  useEffect(() => {
    setHighlighted(0);
  }, [search]);

  return (
    <div ref={containerRef} style={{ position: "relative", display: "inline-block" }} className={className}>
      <button
        onClick={handleOpen}
        style={{
          background: "#1e1e2e",
          border: "1px solid #45475a",
          borderRadius: 6,
          color: "#cdd6f4",
          padding: "4px 10px",
          cursor: "pointer",
          display: "flex",
          alignItems: "center",
          gap: 6,
          fontSize: 13,
        }}
      >
        <span>{currentModel?.name ?? value}</span>
        <span style={{ color: "#6c7086", fontSize: 11 }}>
          {currentModel ? formatContextWindow(currentModel.contextWindow) : ""}
        </span>
        <span style={{ color: "#6c7086" }}>▾</span>
      </button>

      {open && (
        <div
          style={{
            position: "absolute",
            top: "calc(100% + 4px)",
            left: 0,
            minWidth: 280,
            background: "#1e1e2e",
            border: "1px solid #45475a",
            borderRadius: 8,
            boxShadow: "0 8px 24px rgba(0,0,0,0.5)",
            zIndex: 1000,
            overflow: "hidden",
          }}
        >
          <div style={{ padding: "8px 8px 4px" }}>
            <input
              ref={inputRef}
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder="Search models..."
              style={{
                width: "100%",
                background: "#181825",
                border: "1px solid #45475a",
                borderRadius: 4,
                color: "#cdd6f4",
                padding: "4px 8px",
                fontSize: 12,
                outline: "none",
                boxSizing: "border-box",
              }}
            />
          </div>

          <div style={{ maxHeight: 300, overflowY: "auto" }}>
            {search.trim()
              ? filtered.map((model, i) => (
                  <ModelRow
                    key={model.id}
                    model={model}
                    selected={model.id === value}
                    highlighted={i === highlighted}
                    onSelect={handleSelect}
                    onHover={() => setHighlighted(i)}
                  />
                ))
              : providers.map((provider) => {
                  const group = filtered.filter((m) => m.provider === provider);
                  const groupStart = filtered.indexOf(group[0]);
                  return (
                    <div key={provider}>
                      <div
                        style={{
                          padding: "6px 12px 2px",
                          fontSize: 10,
                          color: "#6c7086",
                          textTransform: "uppercase",
                          letterSpacing: "0.05em",
                        }}
                      >
                        {provider}
                      </div>
                      {group.map((model, i) => (
                        <ModelRow
                          key={model.id}
                          model={model}
                          selected={model.id === value}
                          highlighted={groupStart + i === highlighted}
                          onSelect={handleSelect}
                          onHover={() => setHighlighted(groupStart + i)}
                        />
                      ))}
                    </div>
                  );
                })}
            {filtered.length === 0 && (
              <div style={{ padding: "12px", color: "#6c7086", fontSize: 12, textAlign: "center" }}>
                No models found
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

interface ModelRowProps {
  model: ModelOption;
  selected: boolean;
  highlighted: boolean;
  onSelect: (id: string) => void;
  onHover: () => void;
}

function ModelRow({ model, selected, highlighted, onSelect, onHover }: ModelRowProps) {
  return (
    <div
      onClick={() => onSelect(model.id)}
      onMouseEnter={onHover}
      style={{
        padding: "6px 12px",
        display: "flex",
        justifyContent: "space-between",
        alignItems: "center",
        cursor: "pointer",
        background: highlighted ? "#313244" : "transparent",
        borderLeft: selected ? "2px solid #89b4fa" : "2px solid transparent",
      }}
    >
      <span style={{ color: selected ? "#89b4fa" : "#cdd6f4", fontSize: 13 }}>{model.name}</span>
      <span style={{ color: "#6c7086", fontSize: 11 }}>{formatContextWindow(model.contextWindow)}</span>
    </div>
  );
}
