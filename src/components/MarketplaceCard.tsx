interface MarketplaceCardProps {
  icon?: string;
  name: string;
  description: string;
  categories: string[];
  installed?: boolean;
  actionLabel?: string;
  disabled?: boolean;
  onInstall?: () => void | Promise<void>;
  extraContent?: React.ReactNode;
}

export default function MarketplaceCard({
  icon = "⬢",
  name,
  description,
  categories,
  installed = false,
  actionLabel = "Install",
  disabled = false,
  onInstall,
  extraContent,
}: MarketplaceCardProps) {
  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        gap: 12,
        padding: 16,
        borderRadius: 10,
        border: "1px solid #313244",
        background: "#181825",
        minHeight: 220,
      }}
    >
      <div style={{ display: "flex", alignItems: "flex-start", gap: 12 }}>
        <div
          style={{
            width: 40,
            height: 40,
            borderRadius: 10,
            display: "grid",
            placeItems: "center",
            background: "#313244",
            color: "#89b4fa",
            fontSize: 18,
            flexShrink: 0,
          }}
        >
          {icon}
        </div>

        <div style={{ flex: 1, minWidth: 0 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
            <strong style={{ fontSize: 14 }}>{name}</strong>
            {installed && (
              <span
                style={{
                  fontSize: 10,
                  fontWeight: 700,
                  color: "#1e1e2e",
                  background: "#a6e3a1",
                  borderRadius: 999,
                  padding: "2px 8px",
                }}
              >
                ✓ Installed
              </span>
            )}
          </div>
          <p style={{ color: "#bac2de", fontSize: 12, lineHeight: 1.5, marginTop: 6 }}>
            {description}
          </p>
        </div>
      </div>

      {extraContent}

      <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
        {categories.map((category) => (
          <span
            key={category}
            style={{
              fontSize: 10,
              color: "#cba6f7",
              background: "#313244",
              borderRadius: 999,
              padding: "4px 8px",
            }}
          >
            {category}
          </span>
        ))}
      </div>

      <div style={{ marginTop: "auto" }}>
        <button
          type="button"
          onClick={() => void onInstall?.()}
          disabled={installed || disabled}
          style={{
            width: "100%",
            border: "none",
            borderRadius: 8,
            padding: "10px 12px",
            fontWeight: 700,
            cursor: installed || disabled ? "default" : "pointer",
            background: installed ? "#a6e3a122" : "#89b4fa",
            color: installed ? "#a6e3a1" : "#1e1e2e",
            opacity: disabled ? 0.6 : 1,
          }}
        >
          {installed ? "✓ Installed" : actionLabel}
        </button>
      </div>
    </div>
  );
}
