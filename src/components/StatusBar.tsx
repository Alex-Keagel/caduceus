import type { SessionInfo, SessionPhase, TokenUsage, ProjectScanResult } from "../types";
import ContextVisualizer from "./ContextVisualizer";

interface StatusBarProps {
  session: SessionInfo | null;
  phase?: SessionPhase;
  tokenUsage?: TokenUsage | null;
  projectContext?: ProjectScanResult | null;
}

const PHASE_COLOR: Record<string, string> = {
  Idle: "var(--color-success)",
  Running: "var(--color-warning)",
  AwaitingPermission: "var(--color-accent-strong)",
  Cancelling: "var(--color-warning)",
  Completed: "var(--color-accent)",
  Error: "var(--color-danger)",
};

export default function StatusBar({ session, phase, tokenUsage, projectContext }: StatusBarProps) {
  const currentPhase = phase ?? session?.phase ?? "Idle";
  const phaseColor = PHASE_COLOR[currentPhase] ?? "var(--color-text)";

  return (
    <div className="status-bar">
      <div className="status-bar__left">
        <span>⚕ Caduceus</span>
        {session ? (
          <>
            <span>�� {session.project_root.split("/").pop()}</span>
            <span>🤖 {session.model_id}</span>
            <span style={{ color: phaseColor, fontWeight: 700 }}>● {currentPhase}</span>
            {tokenUsage ? (
              <span>
                {tokenUsage.input_tokens}↑ {tokenUsage.output_tokens}↓
              </span>
            ) : null}
            {projectContext?.languages.length ? <span>{projectContext.languages.slice(0, 3).join(", ")}</span> : null}
          </>
        ) : (
          <span>No project open · Cmd+K to open command palette</span>
        )}
      </div>
      {session ? (
        <div className="status-bar__viz">
          <ContextVisualizer tokenBudget={session.token_budget} />
        </div>
      ) : null}
      <span className="status-bar__version">v0.1.0</span>
    </div>
  );
}
