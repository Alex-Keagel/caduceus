import { useEffect, useRef, useState } from "react";

type BuddyMood = "idle" | "thinking" | "working" | "celebrating" | "error" | "sleeping";

interface BuddySpriteProps {
  mood: BuddyMood;
  size?: number;
  name?: string;
  onClick?: () => void;
}

const MOOD_FRAMES: Record<BuddyMood, string> = {
  idle: String.raw` /\_/\\
( •_• )
/ >💻 `,
  thinking: String.raw` /\_/\\
( •.• ) ?
/ >🧠 `,
  working: String.raw` /\_/\\
( •̀ᴗ•́)و
/ >⚙️ `,
  celebrating: String.raw` /\_/\\
( ^‿^ ) ✨
/ >🎉 `,
  error: String.raw` /\_/\\
( x_x ) !!
/ >⚠️ `,
  sleeping: String.raw` /\_/\\
( -.- ) zZ
/ >🌙 `,
};

const MOOD_ACCENT: Record<BuddyMood, string> = {
  idle: "var(--color-accent, #4f46e5)",
  thinking: "var(--color-warning, #f59e0b)",
  working: "var(--color-success, #16a34a)",
  celebrating: "#ec4899",
  error: "var(--color-danger, #dc2626)",
  sleeping: "#64748b",
};

export default function BuddySprite({ mood, size = 48, name = "Cady", onClick }: BuddySpriteProps) {
  const [showTooltip, setShowTooltip] = useState(false);
  const spriteRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const element = spriteRef.current;
    if (!element || typeof element.animate !== "function" || mood !== "working") return;

    const animation = element.animate(
      [
        { transform: "translateY(0px)" },
        { transform: "translateY(-5px)" },
        { transform: "translateY(0px)" },
      ],
      { duration: 650, easing: "ease-in-out", iterations: Number.POSITIVE_INFINITY }
    );

    return () => {
      animation.cancel();
    };
  }, [mood]);

  const interactive = Boolean(onClick);

  return (
    <div
      style={{
        position: "relative",
        display: "inline-flex",
        flexDirection: "column",
        alignItems: "center",
      }}
    >
      <div
        ref={spriteRef}
        title={name}
        role={interactive ? "button" : "img"}
        aria-label={`${name} is ${mood}`}
        tabIndex={0}
        onClick={() => onClick?.()}
        onKeyDown={(event) => {
          if (!interactive) return;
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            onClick?.();
          }
        }}
        onMouseEnter={() => setShowTooltip(true)}
        onMouseLeave={() => setShowTooltip(false)}
        onFocus={() => setShowTooltip(true)}
        onBlur={() => setShowTooltip(false)}
        style={{
          cursor: interactive ? "pointer" : "default",
          userSelect: "none",
          width: size * 2.25,
          minHeight: size * 2.2,
          padding: 10,
          borderRadius: 14,
          border: `1px solid ${MOOD_ACCENT[mood]}`,
          background: "rgba(15, 23, 42, 0.96)",
          color: "#f8fafc",
          boxShadow: `0 10px 20px color-mix(in srgb, ${MOOD_ACCENT[mood]} 30%, transparent)`,
          outline: "none",
        }}
      >
        <div
          style={{
            fontSize: 11,
            letterSpacing: "0.08em",
            textTransform: "uppercase",
            color: MOOD_ACCENT[mood],
            marginBottom: 6,
            fontWeight: 700,
          }}
        >
          {mood}
        </div>
        <pre
          style={{
            margin: 0,
            fontSize: Math.max(size / 5.5, 10),
            lineHeight: 1.15,
            fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace',
            whiteSpace: "pre",
          }}
        >
          {MOOD_FRAMES[mood]}
        </pre>
      </div>

      {showTooltip ? (
        <div
          role="tooltip"
          style={{
            position: "absolute",
            top: "100%",
            marginTop: 8,
            padding: "6px 10px",
            borderRadius: 999,
            background: "rgba(15, 23, 42, 0.95)",
            color: "#e2e8f0",
            fontSize: 12,
            whiteSpace: "nowrap",
            border: "1px solid rgba(148, 163, 184, 0.3)",
          }}
        >
          {name}
        </div>
      ) : null}
    </div>
  );
}
