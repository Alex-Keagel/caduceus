import { useEffect, useMemo, useRef, useState } from "react";

interface Annotation {
  id: string;
  type: "arrow" | "box" | "text" | "highlight";
  x: number;
  y: number;
  width?: number;
  height?: number;
  text?: string;
  color: string;
}

interface DesignModeProps {
  enabled: boolean;
  annotations: Annotation[];
  onAddAnnotation: (annotation: Annotation) => void;
  onRemoveAnnotation: (id: string) => void;
}

type AnnotationTool = Annotation["type"];

interface DragState {
  id: string;
  startClientX: number;
  startClientY: number;
  startX: number;
  startY: number;
}

const TOOL_LABELS: Record<AnnotationTool, string> = {
  arrow: "Arrow",
  box: "Box",
  text: "Text",
  highlight: "Highlight",
};

function createAnnotationId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `annotation-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function toRatio(value: number | undefined, size: number, fallback: number): number {
  if (typeof value !== "number") return fallback;
  if (value <= 1) return value;
  if (!size) return fallback;
  return value / size;
}

function positionToCss(value: number | undefined, size: number, fallback: number): string {
  if (typeof value !== "number") return `${fallback * 100}%`;
  if (value <= 1) return `${value * 100}%`;
  return `${value}px`;
}

function sizeToCss(value: number | undefined, fallback: number): string {
  if (typeof value !== "number") return `${fallback * 100}%`;
  if (value <= 1) return `${value * 100}%`;
  return `${value}px`;
}

export default function DesignMode({
  enabled,
  annotations,
  onAddAnnotation,
  onRemoveAnnotation,
}: DesignModeProps) {
  const [activeTool, setActiveTool] = useState<AnnotationTool>("arrow");
  const [color, setColor] = useState("#38bdf8");
  const [textValue, setTextValue] = useState("Review this");
  const [draftPositions, setDraftPositions] = useState<Record<string, { x: number; y: number }>>({});
  const [isDragging, setIsDragging] = useState(false);

  const overlayRef = useRef<HTMLDivElement>(null);
  const stageRef = useRef<HTMLDivElement>(null);
  const dragStateRef = useRef<DragState | null>(null);

  const visibleAnnotations = useMemo(
    () =>
      annotations.map((annotation) => {
        const draft = draftPositions[annotation.id];
        return {
          ...annotation,
          x: draft?.x ?? annotation.x,
          y: draft?.y ?? annotation.y,
        };
      }),
    [annotations, draftPositions]
  );

  useEffect(() => {
    if (!enabled) {
      dragStateRef.current = null;
      setIsDragging(false);
      return;
    }
  }, [enabled]);

  useEffect(() => {
    const annotationIds = new Set(annotations.map((annotation) => annotation.id));

    setDraftPositions((current) => {
      const nextEntries = Object.entries(current).filter(([id]) => annotationIds.has(id));
      if (nextEntries.length === Object.keys(current).length) return current;
      return Object.fromEntries(nextEntries);
    });

    if (dragStateRef.current && !annotationIds.has(dragStateRef.current.id)) {
      dragStateRef.current = null;
      setIsDragging(false);
    }
  }, [annotations]);

  useEffect(() => {
    if (!enabled || !isDragging) return;

    const handlePointerMove = (event: MouseEvent) => {
      const dragState = dragStateRef.current;
      const stage = stageRef.current;
      if (!dragState || !stage) return;

      const rect = stage.getBoundingClientRect();
      if (!rect.width || !rect.height) return;

      const dx = (event.clientX - dragState.startClientX) / rect.width;
      const dy = (event.clientY - dragState.startClientY) / rect.height;

      setDraftPositions((current) => ({
        ...current,
        [dragState.id]: {
          x: Math.max(0, Math.min(1, dragState.startX + dx)),
          y: Math.max(0, Math.min(1, dragState.startY + dy)),
        },
      }));
    };

    const handlePointerUp = () => {
      dragStateRef.current = null;
      setIsDragging(false);
    };

    window.addEventListener("mousemove", handlePointerMove);
    window.addEventListener("mouseup", handlePointerUp);

    return () => {
      window.removeEventListener("mousemove", handlePointerMove);
      window.removeEventListener("mouseup", handlePointerUp);
    };
  }, [enabled, isDragging]);

  if (!enabled) return null;

  return (
    <div
      ref={overlayRef}
      aria-label="Design mode overlay"
      style={{
        position: "absolute",
        inset: 0,
        display: "flex",
        flexDirection: "column",
        background: "rgba(15, 23, 42, 0.2)",
        backdropFilter: "blur(1px)",
        zIndex: 20,
      }}
    >
      <div
        style={{
          display: "flex",
          flexWrap: "wrap",
          alignItems: "center",
          gap: 8,
          padding: 12,
          borderBottom: "1px solid rgba(148, 163, 184, 0.25)",
          background: "rgba(15, 23, 42, 0.92)",
          color: "#e2e8f0",
        }}
      >
        {Object.entries(TOOL_LABELS).map(([tool, label]) => {
          const selected = activeTool === tool;
          return (
            <button
              key={tool}
              type="button"
              onClick={() => setActiveTool(tool as AnnotationTool)}
              style={{
                padding: "8px 12px",
                borderRadius: 10,
                border: selected ? "1px solid rgba(56, 189, 248, 0.95)" : "1px solid rgba(148, 163, 184, 0.25)",
                background: selected ? "rgba(56, 189, 248, 0.18)" : "rgba(30, 41, 59, 0.7)",
                color: "#f8fafc",
                cursor: "pointer",
              }}
            >
              {label}
            </button>
          );
        })}

        <label style={{ display: "inline-flex", alignItems: "center", gap: 8, marginLeft: 8 }}>
          <span style={{ fontSize: 12 }}>Color</span>
          <input type="color" value={color} onChange={(event) => setColor(event.target.value)} />
        </label>

        {activeTool === "text" ? (
          <input
            type="text"
            value={textValue}
            onChange={(event) => setTextValue(event.target.value)}
            placeholder="Annotation text"
            style={{
              minWidth: 160,
              padding: "8px 10px",
              borderRadius: 10,
              border: "1px solid rgba(148, 163, 184, 0.25)",
              background: "rgba(30, 41, 59, 0.75)",
              color: "#f8fafc",
            }}
          />
        ) : null}

        <span style={{ marginLeft: "auto", fontSize: 12, color: "rgba(226, 232, 240, 0.7)" }}>
          Click to place · drag to reposition
        </span>
      </div>

      <div
        ref={stageRef}
        onClick={(event) => {
          if (event.target !== event.currentTarget) return;

          const rect = stageRef.current?.getBoundingClientRect() ?? event.currentTarget.getBoundingClientRect();
          const x = Math.max(0, Math.min(1, (event.clientX - rect.left) / rect.width));
          const y = Math.max(0, Math.min(1, (event.clientY - rect.top) / rect.height));

          const annotation: Annotation = {
            id: createAnnotationId(),
            type: activeTool,
            x,
            y,
            width: activeTool === "text" || activeTool === "arrow" ? 0.18 : 0.22,
            height: activeTool === "highlight" ? 0.08 : activeTool === "text" ? 0.1 : 0.16,
            text: activeTool === "text" ? textValue.trim() || "Note" : activeTool === "arrow" ? "Follow-up" : undefined,
            color,
          };

          onAddAnnotation(annotation);
        }}
        style={{ position: "relative", flex: 1 }}
      >
        {visibleAnnotations.map((annotation) => {
          const stageWidth = stageRef.current?.clientWidth ?? 0;
          const stageHeight = stageRef.current?.clientHeight ?? 0;
          const left = positionToCss(annotation.x, stageWidth, 0.1);
          const top = positionToCss(annotation.y, stageHeight, 0.1);
          const width = sizeToCss(annotation.width, annotation.type === "highlight" ? 0.22 : 0.18);
          const height = sizeToCss(annotation.height, annotation.type === "highlight" ? 0.08 : 0.16);

          return (
            <div
              key={annotation.id}
              tabIndex={0}
              onMouseDown={(event) => {
                event.preventDefault();
                event.stopPropagation();
                const current = draftPositions[annotation.id] ?? {
                  x: toRatio(annotation.x, stageWidth, 0.1),
                  y: toRatio(annotation.y, stageHeight, 0.1),
                };
                dragStateRef.current = {
                  id: annotation.id,
                  startClientX: event.clientX,
                  startClientY: event.clientY,
                  startX: current.x,
                  startY: current.y,
                };
                setIsDragging(true);
              }}
              onKeyDown={(event) => {
                if (event.key === "Delete" || event.key === "Backspace") {
                  event.preventDefault();
                  onRemoveAnnotation(annotation.id);
                }
              }}
              style={{
                position: "absolute",
                left,
                top,
                width,
                minWidth: annotation.type === "text" ? 120 : undefined,
                height: annotation.type === "text" ? "auto" : height,
                transform: "translate(-50%, -50%)",
                cursor: "move",
                color: annotation.color,
                outline: "none",
              }}
            >
              <button
                type="button"
                aria-label="Delete annotation"
                onClick={(event) => {
                  event.stopPropagation();
                  onRemoveAnnotation(annotation.id);
                }}
                style={{
                  position: "absolute",
                  top: -12,
                  right: -12,
                  width: 24,
                  height: 24,
                  borderRadius: "50%",
                  border: "1px solid rgba(248, 250, 252, 0.5)",
                  background: "rgba(15, 23, 42, 0.95)",
                  color: "#fff",
                  cursor: "pointer",
                }}
              >
                ×
              </button>

              {annotation.type === "arrow" ? (
                <div
                  style={{
                    display: "flex",
                    alignItems: "center",
                    gap: 8,
                    padding: "6px 10px",
                    borderRadius: 999,
                    border: `1px solid ${annotation.color}`,
                    background: "rgba(15, 23, 42, 0.82)",
                    boxShadow: `0 0 0 2px ${annotation.color}22`,
                  }}
                >
                  <span style={{ fontSize: 18 }}>➜</span>
                  <span style={{ fontWeight: 700, fontSize: 12 }}>{annotation.text ?? "Point"}</span>
                </div>
              ) : null}

              {annotation.type === "box" ? (
                <div
                  style={{
                    width: "100%",
                    height: "100%",
                    borderRadius: 10,
                    border: `2px solid ${annotation.color}`,
                    background: `${annotation.color}12`,
                  }}
                />
              ) : null}

              {annotation.type === "highlight" ? (
                <div
                  style={{
                    width: "100%",
                    height: "100%",
                    borderRadius: 8,
                    background: annotation.color,
                    opacity: 0.25,
                    boxShadow: `0 0 0 1px ${annotation.color}`,
                  }}
                />
              ) : null}

              {annotation.type === "text" ? (
                <div
                  style={{
                    padding: "10px 12px",
                    borderRadius: 12,
                    background: "rgba(15, 23, 42, 0.95)",
                    border: `1px solid ${annotation.color}`,
                    boxShadow: `0 8px 20px ${annotation.color}22`,
                    color: "#f8fafc",
                    fontWeight: 600,
                  }}
                >
                  {annotation.text ?? "Note"}
                </div>
              ) : null}
            </div>
          );
        })}
      </div>
    </div>
  );
}
