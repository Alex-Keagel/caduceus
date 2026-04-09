import type { SessionInfo } from "../types";

interface StatusBarProps {
  session: SessionInfo | null;
}

const PHASE_COLOR: Record<string, string> = {
  Idle: "#a6e3a1",
  Planning: "#89b4fa",
  Executing: "#f9e2af",
  AwaitingPermission: "#cba6f7",
  Summarizing: "#89dceb",
  Error: "#f38ba8",
};

export default function StatusBar({ session }: StatusBarProps) {
  const phase = session?.phase ?? "Idle";
  const phaseColor = PHASE_COLOR[phase] ?? "#cdd6f4";

  return (
    <div className="status-bar">
      <span style={{ marginRight: 16 }}>⚕ Caduceus</span>

      {session ? (
        <>
          <span style={{ marginRight: 12 }}>
            📁 {session.project_root.split("/").pop()}
          </span>
          <span style={{ marginRight: 12 }}>
            🤖 {session.model_id}
          </span>
          <span style={{ color: phaseColor, fontWeight: 700 }}>
            ● {phase}
          </span>
        </>
      ) : (
        <span style={{ opacity: 0.6 }}>No project open · Cmd+P to open command palette</span>
      )}

      <span style={{ marginLeft: "auto", opacity: 0.7 }}>
        v0.1.0
      </span>
    </div>
  );
}
