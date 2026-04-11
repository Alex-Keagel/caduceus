import { useEffect, useMemo, useRef, useState } from "react";

interface SplitPaneProps {
  direction: "horizontal" | "vertical";
  children: React.ReactNode;
  storageKey?: string;
  defaultSplit?: number;
  minSize?: number;
}

export default function SplitPane({
  direction,
  children,
  storageKey,
  defaultSplit = 0.5,
  minSize = 0.2,
}: SplitPaneProps) {
  const panes = useMemo(() => Array.from((children as React.ReactNode[] | undefined) ?? [children]), [children]);
  const [split, setSplit] = useState(defaultSplit);
  const containerRef = useRef<HTMLDivElement>(null);
  const isDraggingRef = useRef(false);

  useEffect(() => {
    if (!storageKey) return;
    const raw = window.localStorage.getItem(storageKey);
    if (!raw) return;
    const parsed = Number.parseFloat(raw);
    if (!Number.isNaN(parsed)) {
      setSplit(Math.min(1 - minSize, Math.max(minSize, parsed)));
    }
  }, [minSize, storageKey]);

  useEffect(() => {
    if (storageKey) {
      window.localStorage.setItem(storageKey, split.toString());
    }
  }, [split, storageKey]);

  useEffect(() => {
    const handleMove = (event: MouseEvent) => {
      if (!isDraggingRef.current || !containerRef.current) return;
      const rect = containerRef.current.getBoundingClientRect();
      const ratio =
        direction === "horizontal"
          ? (event.clientX - rect.left) / rect.width
          : (event.clientY - rect.top) / rect.height;
      setSplit(Math.min(1 - minSize, Math.max(minSize, ratio)));
    };
    const stopDragging = () => {
      isDraggingRef.current = false;
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
    window.addEventListener("mousemove", handleMove);
    window.addEventListener("mouseup", stopDragging);
    return () => {
      window.removeEventListener("mousemove", handleMove);
      window.removeEventListener("mouseup", stopDragging);
    };
  }, [direction, minSize]);

  if (panes.length !== 2) {
    return <div className={`split-pane split-pane--${direction}`}>{children}</div>;
  }

  const firstBasis = `${split * 100}%`;
  const secondBasis = `${(1 - split) * 100}%`;

  return (
    <div ref={containerRef} className={`split-pane split-pane--${direction}`}>
      <div className="split-pane__section" style={{ flexBasis: firstBasis }}>
        {panes[0]}
      </div>
      <div
        className={`split-pane__handle split-pane__handle--${direction}`}
        onMouseDown={() => {
          isDraggingRef.current = true;
          document.body.style.cursor = direction === "horizontal" ? "col-resize" : "row-resize";
          document.body.style.userSelect = "none";
        }}
        role="separator"
        aria-orientation={direction === "horizontal" ? "vertical" : "horizontal"}
      />
      <div className="split-pane__section" style={{ flexBasis: secondBasis }}>
        {panes[1]}
      </div>
    </div>
  );
}
