import { useEffect, useMemo, useState } from "react";

interface ImageRendererProps {
  src: string;
  alt: string;
  maxWidth?: number;
  maxHeight?: number;
  protocol?: "inline" | "sixel" | "kitty";
}

const BADGE_COLORS: Record<NonNullable<ImageRendererProps["protocol"]>, string> = {
  inline: "var(--color-accent, #4f46e5)",
  sixel: "var(--color-success, #16a34a)",
  kitty: "var(--color-warning, #f59e0b)",
};

export default function ImageRenderer({
  src,
  alt,
  maxWidth = 800,
  maxHeight = 600,
  protocol = "inline",
}: ImageRendererProps) {
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(false);

  useEffect(() => {
    setLoading(true);
    setError(false);
  }, [src]);

  const sourceLabel = useMemo(() => {
    if (src.startsWith("data:")) return "data URL";
    if (src.startsWith("blob:")) return "blob";
    try {
      const url = new URL(src, window.location.href);
      return url.protocol.replace(":", "");
    } catch {
      return "image";
    }
  }, [src]);

  return (
    <div
      style={{
        position: "relative",
        display: "inline-flex",
        alignItems: "center",
        justifyContent: "center",
        minWidth: 160,
        minHeight: 120,
        maxWidth,
        maxHeight,
        overflow: "hidden",
        borderRadius: 12,
        border: "1px solid rgba(148, 163, 184, 0.35)",
        background: "rgba(15, 23, 42, 0.92)",
        boxShadow: "0 10px 30px rgba(15, 23, 42, 0.2)",
      }}
    >
      <span
        style={{
          position: "absolute",
          top: 10,
          right: 10,
          zIndex: 3,
          padding: "4px 8px",
          borderRadius: 999,
          background: BADGE_COLORS[protocol],
          color: "#fff",
          fontSize: 11,
          fontWeight: 700,
          textTransform: "uppercase",
          letterSpacing: "0.08em",
        }}
      >
        {protocol}
      </span>

      {loading && !error ? (
        <div
          aria-hidden="true"
          style={{
            position: "absolute",
            inset: 0,
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            justifyContent: "center",
            gap: 8,
            background: "linear-gradient(135deg, rgba(30, 41, 59, 0.9), rgba(15, 23, 42, 0.9))",
            color: "rgba(226, 232, 240, 0.9)",
            zIndex: 2,
          }}
        >
          <div
            style={{
              width: 32,
              height: 32,
              borderRadius: "50%",
              border: "3px solid rgba(255, 255, 255, 0.15)",
              borderTopColor: "rgba(255, 255, 255, 0.9)",
            }}
          />
          <span style={{ fontSize: 13 }}>Rendering {sourceLabel}…</span>
        </div>
      ) : null}

      {error ? (
        <div
          role="img"
          aria-label={`${alt} failed to load`}
          style={{
            display: "flex",
            flexDirection: "column",
            alignItems: "center",
            justifyContent: "center",
            gap: 8,
            width: "100%",
            height: "100%",
            minHeight: 120,
            padding: 20,
            color: "rgba(248, 113, 113, 0.95)",
            textAlign: "center",
          }}
        >
          <span style={{ fontSize: 24 }}>⚠</span>
          <strong style={{ fontSize: 14 }}>Unable to render image</strong>
          <span style={{ fontSize: 12, color: "rgba(226, 232, 240, 0.8)" }}>{alt}</span>
        </div>
      ) : (
        <img
          src={src}
          alt={alt}
          onLoad={() => {
            setLoading(false);
            setError(false);
          }}
          onError={() => {
            setLoading(false);
            setError(true);
          }}
          style={{
            display: "block",
            width: "100%",
            height: "100%",
            maxWidth,
            maxHeight,
            objectFit: "contain",
            opacity: loading ? 0.01 : 1,
            transition: "opacity 180ms ease",
          }}
        />
      )}
    </div>
  );
}
